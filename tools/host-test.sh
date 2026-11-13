#!/usr/bin/env bash
# The host-workspace gate (docs/host-service.md §14, HOST-CONTRACT): fmt,
# clippy -D warnings, every workspace test with a per-crate count and the
# OWT_MIN_HOST_WS_TESTS ratchet, the deck-sim.py --dry-run parity diff, then
# the `api` step: a real daemon on a private data dir driven through curl,
# `deckd ctl`, the Python virtual agent and websocat (HOST-CONTRACT-W2 §5).
#
#   tools/host-test.sh              the CI gate
#   tools/host-test.sh --hardware   adds the device checks against the real deck:
#                                   `deckd run --demo` for 60 s (hello answered, barrier,
#                                   synced, heartbeat, no link drop, no device err), then a
#                                   restart (new sid → resync), then the week-2 §7 rows
#                                   (demo roster, guarded allow, poll delivery with and
#                                   without a waiter, the writer metrics, SIGTERM); stops
#                                   the user's installed deckd LaunchAgent first and
#                                   restores it after.
#                                   Not run by CI. OWT_HW_SECONDS overrides the 60 s.
#
# Builds use the host triple and their own target dir (target/host) because
# the repo root's .cargo/config.toml targets the firmware's thumb triple.
set -uo pipefail

hardware=false
gui=false
for arg in "$@"; do
    case "$arg" in
        --hardware) hardware=true ;;
        --gui) gui=true ;;
        -h | --help) sed -n '2,17s/^# \{0,1\}//p' "$0"; exit 0 ;;
        *) echo "host-test: unknown argument '$arg'" >&2; exit 2 ;;
    esac
done

root=$(cd "$(dirname "$0")/.." && pwd)
cd "$root" || exit 2
host=$(rustc -vV | sed -n 's/^host: //p')
manifest="$root/host/Cargo.toml"
target_dir="$root/target/host"
logs="$target_dir/gate"
mkdir -p "$logs"
common=(--manifest-path "$manifest" --target "$host" --target-dir "$target_dir" --locked)
crates=(deck-proto deck-wrap deck-model deck-client deckd deck-gui-core)
# The gpui bin is excluded from the default clippy/test sweep: it drags ~500
# crates in and carries no logic of its own (§8.1). `--gui` covers it.
gui_crate=openwalkietalkie-app
# Test-count ratchet: a merge may add tests, never lose them. It only bites
# when it equals the real count, so a run that finds more says so and
# `deckd`'s gate_ratchet row keeps this number and the one docs/host-service.md
# publishes in step.
min_tests=${OWT_MIN_HOST_WS_TESTS:-974}

failures=0
step() {
    local name=$1; shift
    local log="$logs/$(echo "$name" | tr ' /' '__').log"
    if "$@" >"$log" 2>&1; then
        printf 'ok    %s\n' "$name"
    else
        printf 'FAIL  %s (see %s)\n' "$name" "$log"
        tail -n 40 "$log"
        failures=$((failures + 1))
    fi
}

step "cargo fmt --check" cargo fmt --manifest-path "$manifest" --all -- --check
step "cargo clippy -D warnings" cargo clippy "${common[@]}" --workspace --exclude "$gui_crate" --all-targets -- -D warnings

total=0
for crate in "${crates[@]}"; do
    log="$logs/test-$crate.log"
    if cargo test "${common[@]}" -p "$crate" >"$log" 2>&1; then
        passed=$(grep -E '^test result: ok\.' "$log" | sed -E 's/.* ([0-9]+) passed.*/\1/' | paste -sd+ - | bc)
        passed=${passed:-0}
        printf 'ok    tests %-12s %4d passed\n' "$crate" "$passed"
        total=$((total + passed))
    else
        printf 'FAIL  tests %s (see %s)\n' "$crate" "$log"
        grep -E '^(test .* FAILED|failures:|---- )' "$log" | head -n 20
        tail -n 20 "$log"
        failures=$((failures + 1))
    fi
done
if [ "$total" -gt "$min_tests" ]; then
    printf 'ok    test ratchet: %d > OWT_MIN_HOST_WS_TESTS=%d -- raise min_tests (and the count in docs/host-service.md) to %d\n' \
        "$total" "$min_tests" "$total"
elif [ "$total" -eq "$min_tests" ]; then
    printf 'ok    test ratchet: %d == OWT_MIN_HOST_WS_TESTS=%d\n' "$total" "$min_tests"
else
    printf 'FAIL  test ratchet: %d < OWT_MIN_HOST_WS_TESTS=%d\n' "$total" "$min_tests"
    failures=$((failures + 1))
fi

# Framing parity: deckd's framed reports must be byte-identical to the
# reference host's for the same lines (sid and clock pinned on both sides).
parity() {
    local deckd="$target_dir/$host/debug/deckd"
    cargo build "${common[@]}" -p deckd >"$logs/build-deckd.log" 2>&1 || { tail -n 20 "$logs/build-deckd.log"; return 1; }
    local args=(--sid 314159 --hm 19:42 --flood 25 --send '{"m":"set","k":"brightness","v":2}' \
        --send '{"m":"card","agent":1,"id":1,"clear":1}' --send '{"m":"agent","id":5,"gone":1}')
    "$deckd" device dry-run "${args[@]}" >"$logs/parity-deckd.txt" 2>"$logs/parity-deckd.err" || { cat "$logs/parity-deckd.err"; return 1; }
    python3 "$root/tools/deck-sim.py" --dry-run "${args[@]}" >"$logs/parity-deck-sim.txt" 2>&1 || { tail -n 20 "$logs/parity-deck-sim.txt"; return 1; }
    diff -u "$logs/parity-deck-sim.txt" "$logs/parity-deckd.txt" && [ -s "$logs/parity-deckd.txt" ]
}
if parity >"$logs/parity.log" 2>&1; then
    printf 'ok    deck-sim parity: %s\n' "$(tail -n 1 "$logs/parity-deckd.txt")"
else
    printf 'FAIL  deck-sim parity (see %s)\n' "$logs/parity.log"
    head -n 40 "$logs/parity.log"
    failures=$((failures + 1))
fi

# ---------------------------------------------------------------------------
# The `gui` step (§8.1). `deck-gui-core` is an ordinary counted crate above —
# it has no gpui dependency precisely so the reducer suite runs everywhere.
# The gpui bin is only *built*: it pins `gpui` with `default-features = false,
# features = ["runtime_shaders","macos-blade"]` because the default features
# need Xcode's `metal` compiler, and the Metal Toolchain is a separately
# downloadable Xcode component that is not installed on every machine.
# `runtime_shaders` moves shader compilation past build time, so this builds
# and renders without that download.
# ---------------------------------------------------------------------------
# Its own tests are not in the ratchet above (the gpui tree is too heavy for
# every run), so they get a floor of their own here: a count, not just an exit
# code, because a crate whose tests quietly stopped being compiled would
# otherwise pass.
GUI_MIN=${OWT_MIN_GUI_TESTS:-108}
if [ "$gui" = true ]; then
    gui_log="$logs/gui.log"
    if cargo clippy "${common[@]}" -p "$gui_crate" --all-targets -- -D warnings >"$gui_log" 2>&1 \
       && cargo build "${common[@]}" -p "$gui_crate" >>"$gui_log" 2>&1 \
       && cargo test "${common[@]}" -p "$gui_crate" >>"$gui_log" 2>&1; then
        gt=$(grep -E '^test result: ok\.' "$gui_log" | sed -E 's/.* ([0-9]+) passed.*/\1/' | paste -sd+ - | bc)
        gt=${gt:-0}
        if [ "$gt" -ge "$GUI_MIN" ]; then
            printf 'ok    gui                 %s builds and runs %s tests (gpui pinned with runtime_shaders)\n' "$gui_crate" "$gt"
        else
            printf 'FAIL  gui                 %s tests < OWT_MIN_GUI_TESTS=%s (see %s)\n' "$gt" "$GUI_MIN" "$gui_log"
            failures=$((failures + 1))
        fi
    else
        printf 'FAIL  gui (see %s)\n' "$gui_log"
        if grep -q "cannot execute tool 'metal'" "$gui_log"; then
            printf '      the gpui feature pin was lost: default features need the Metal Toolchain (§8.1)\n'
        fi
        tail -n 20 "$gui_log"
        failures=$((failures + 1))
    fi
else
    printf 'skip  gui                 (no --gui; %s is a view layer over deck-gui-core)\n' "$gui_crate"
fi

# ---------------------------------------------------------------------------
# The `voice` step (HOST-CONTRACT-W4 §5): the deterministic rows that libtest
# cannot count, and — only with --hardware — the rows that need the deck.
#
# `voice_mainloop` is `harness = false` because B10 can only be true on the
# process's **main** thread: a `dispatch_async` to the main queue runs
# nowhere else (V13), and libtest runs every `#[test]` on a worker. It
# therefore contributes 0 to the ratchet above and has to be run and read
# here. Everything else voice-related is an ordinary `#[test]` and was
# already counted.
#
# The hardware rows print `skip` with a reason and pass when the deck, the
# grant or the opt-in is absent: §7's rule is that a missing grant is never
# a failure, and a gate that went red on a Mac with no headset paired would
# be switched off within a week. `OWT_HW_MIC=1` is a further opt-in for the
# rows that open the microphone, because opening it can raise a TCC prompt
# for whichever app is responsible for this process (V9) and a test suite
# may not raise a system dialog unasked.
voice_log="$logs/voice-mainloop.log"
if cargo test "${common[@]}" -p deckd --test voice_mainloop >"$voice_log" 2>&1; then
    rows=$(grep -c '^ok    mainloop ' "$voice_log")
    printf 'ok    voice mainloop      %s rows (B10, on the real main thread)\n' "$rows"
    sed -n 's/^note  /note  voice /p' "$voice_log"
else
    printf 'FAIL  voice mainloop (see %s)\n' "$voice_log"
    grep -E '^(FAIL|skip) ' "$voice_log" | head -n 10
    failures=$((failures + 1))
fi

if [ "$hardware" = true ]; then
    hw_log="$logs/voice-hardware.log"
    if cargo test "${common[@]}" -p deckd --test voice_hardware \
        -- --ignored --nocapture --test-threads=1 >"$hw_log" 2>&1; then
        printf 'ok    voice hardware\n'
        sed -n 's/^/      /p' "$hw_log" | grep -E "deck uid|cpal |nominal|first |A2DP|rendered|note |skip " | head -n 20
    else
        printf 'FAIL  voice hardware (see %s)\n' "$hw_log"
        tail -n 20 "$hw_log"
        failures=$((failures + 1))
    fi
else
    printf 'skip  voice hardware      (no --hardware; needs the deck as an audio device)\n'
fi

# ---------------------------------------------------------------------------
# The `packaging` step (§10.1). Building and signing a whole bundle takes
# minutes and needs a Developer ID, so the gate does not do it on every run.
# What it does check on every run is the part that is a *contract* rather than
# an artefact: that the entitlements file still carries both keys, that the
# LaunchAgent plist still says `KeepAlive {SuccessfulExit false}` and still has
# no StandardOutPath/StandardErrorPath, and that every plist parses. Those are
# the rows that, if they drift, produce a bundle which looks fine and cannot
# work — `KeepAlive: true` makes both of deckd's exit codes meaningless, and a
# missing entitlement fails capture under the hardened runtime even with a
# grant. When a built bundle happens to be present, its identity is asserted
# too (`tools/assert-bundle-identity.sh`, which is what CI runs).
# ---------------------------------------------------------------------------
pkg_fail=0
ent="$root/packaging/macos/deck.entitlements"
agent_plist="$root/packaging/macos/ai.cohdl.openwalkietalkie.deckd.plist"
for f in "$ent" "$agent_plist"; do
    plutil -lint "$f" >/dev/null 2>&1 || { printf 'FAIL  packaging plist       %s does not parse\n' "${f##*/}"; pkg_fail=1; }
done
# `Info.plist.in` is a template: `@APP_VERSION@` and friends are substituted at
# package time, so it cannot parse as-is and linting it directly is a category
# error. Render the placeholders to harmless values and lint *that*, which
# still catches malformed XML in the template itself.
info_in="$root/packaging/macos/Info.plist.in"
rendered="$logs/Info.plist.rendered"
# A placeholder standing alone on its line is an *element* (it expands to
# `<true/>`/`<false/>`); one inside a tag is text. Render each in its own kind
# or the result is not XML and the lint says so for the wrong reason.
sed -e 's/^[[:space:]]*@[A-Z_]*@[[:space:]]*$/<true\/>/' -e 's/@[A-Z_]*@/x/g' \
    "$info_in" >"$rendered" 2>/dev/null
if plutil -lint "$rendered" >/dev/null 2>&1; then
    for k in NSMicrophoneUsageDescription NSSpeechRecognitionUsageDescription; do
        grep -q "$k" "$info_in" || { printf 'FAIL  packaging Info.plist  %s missing -- a TCC prompt with no usage string is a crash, not a dialog\n' "$k"; pkg_fail=1; }
    done
else
    printf 'FAIL  packaging plist       Info.plist.in does not parse once its placeholders are filled\n'
    pkg_fail=1
fi
for key in com.apple.security.device.audio-input; do
    grep -q "$key" "$ent" 2>/dev/null || { printf 'FAIL  packaging entitlement %s missing from deck.entitlements\n' "$key"; pkg_fail=1; }
done
if plutil -extract KeepAlive json -o - "$agent_plist" 2>/dev/null | grep -q '"SuccessfulExit":false'; then
    :
else
    printf 'FAIL  packaging KeepAlive   the agent plist must say {SuccessfulExit:false} -- `true` restarts on every exit and makes exit 75 and exit 0 mean nothing\n'
    pkg_fail=1
fi
for k in StandardOutPath StandardErrorPath; do
    if plutil -extract "$k" raw -o - "$agent_plist" >/dev/null 2>&1; then
        printf 'FAIL  packaging %s  present; deckd opens its own log (§9) and launchd does not expand ~\n' "$k"
        pkg_fail=1
    fi
done
if [ "$pkg_fail" = 0 ]; then
    printf 'ok    packaging contract  entitlements, KeepAlive and the plists are what §10.1 requires\n'
else
    failures=$((failures + 1))
fi
bundle=$(ls -d "$root"/dist/*/OpenWalkieTalkie.app 2>/dev/null | head -n 1)
if [ -n "$bundle" ]; then
    if "$root/tools/assert-bundle-identity.sh" "$bundle" >"$logs/packaging.log" 2>&1; then
        printf 'ok    packaging identity  one identifier, one team, the audio-input entitlement on both Mach-Os\n'
    else
        rc=$?
        if [ "$rc" = 3 ]; then
            printf 'skip  packaging identity  the built bundle is ad-hoc, not a distributable build\n'
        else
            printf 'FAIL  packaging identity (see %s)\n' "$logs/packaging.log"
            failures=$((failures + 1))
        fi
    fi
else
    printf 'skip  packaging identity  (no built bundle; tools/package-macos.sh makes one)\n'
fi

# ---------------------------------------------------------------------------
# The `api` step (HOST-CONTRACT-W2 §5): one real `deckd run --foreground
# --no-device` on a private, short data dir with `[api] port = 0` (never
# 47201, never the user's files) and `tcp = true` (the TCP door is off by
# default until it is deleted), then every door exercised the way a user
# would — curl over TCP, `deckd ctl` over the UDS, the Python virtual agent,
# websocat when it is installed. Every row prints ok or FAIL. The only
# `skip`s left are websocat, which §5 allows by name (the Rust WS test covers
# that door), and a row §4.2's ancestry cap refuses when the gate itself runs
# inside Claude Code or Codex (`ancestry_refused`, which says so); the
# pre-merge fallbacks the other rows carried — a `state.get`
# stand-in for `agent.list`, a skip when the tokens.toml watcher answered
# anything but 200, a skip for a missing `virtual_agent.py` — are gone: each
# of them would now hide a regression instead of reporting it. The daemon
# and the agent are killed and the data dir removed whatever happens,
# including on Ctrl-C.
api_failures=0
api_pid=""
api_agent_pid=""
api_dir=""
api_deckd=""
api_cleanup() {
    [ -n "$api_agent_pid" ] && kill -TERM "$api_agent_pid" 2>/dev/null
    [ -n "$api_pid" ] && kill -TERM "$api_pid" 2>/dev/null
    [ -n "$api_dir" ] && [ -d "$api_dir" ] && rm -rf "$api_dir"
    api_agent_pid=""; api_pid=""; api_dir=""
}
trap 'api_cleanup; exit 130' INT TERM
trap api_cleanup EXIT

api_row() {  # name, status (0 = ok), detail
    if [ "$2" -eq 0 ]; then
        printf 'ok    api %-18s %s\n' "$1" "$3"
    else
        printf 'FAIL  api %-18s %s\n' "$1" "$3"
        api_failures=$((api_failures + 1))
    fi
}
api_skip() { printf 'skip  api %-18s %s\n' "$1" "$2"; }

# §4.2's ancestry cap: a gate run inside Claude Code or Codex (from a terminal
# one of them started) is refused `operate`, `admin` and `device` by the very
# service it tests. That is the cap working, not a regression, so a row so
# refused prints `skip` and says why — only for that refusal, read from what
# `deckd ctl` printed (`-32002 {reason:"agent_ancestry"}`), never for any
# other failure.
ANCESTRY_SKIP='the §4.2 ancestry cap refused it: this shell runs under Claude Code or Codex; run the gate from a terminal of its own'
ancestry_refused() {  # the output of a `deckd ctl` call that failed
    case $1 in *'"reason":"agent_ancestry"'*) return 0 ;; esac
    return 1
}

# A value out of the JSON object on stdin: `api_field result ts`. Exit 1
# when the path is missing or the input is not JSON. The script is an
# argument, not a heredoc: stdin belongs to the JSON.
api_field() {
    python3 -c '
import json, sys
try:
    value = json.load(sys.stdin)
except Exception:
    sys.exit(1)
for key in sys.argv[1:]:
    try:
        value = value[int(key)] if isinstance(value, list) else value[key]
    except Exception:
        sys.exit(1)
print(value if isinstance(value, str) else json.dumps(value))
' "$@"
}

# Wait for `$1` to be a non-empty file, at most `$2` seconds.
api_wait_file() {
    local waited=0
    while [ ! -s "$1" ]; do
        [ "$waited" -ge "${2}0" ] && return 1
        sleep 0.1
        waited=$((waited + 1))
    done
    return 0
}

api_curl() { curl -sS -m 10 "$@"; }
api_ctl() { DECKD_DATA_DIR="$api_dir" "$api_deckd" ctl "$@"; }

# `wait` on a child of ours with an upper bound: a watchdog kills it after
# `$2` seconds so a wedged daemon or agent cannot hang the gate. Returns
# the child's exit status (or the signal's).
api_wait_pid() {
    local pid=$1 seconds=$2 status killer
    # The watchdog resets our traps: it must never run the cleanup itself.
    ( trap - INT TERM EXIT
      sleep "$seconds"; kill -TERM "$pid" 2>/dev/null
      sleep 2; kill -KILL "$pid" 2>/dev/null ) &
    killer=$!
    wait "$pid" 2>/dev/null
    status=$?
    # The shell announces a `wait`ed job killed by a signal on its own
    # stderr, which `wait 2>/dev/null` cannot suppress: the whole group is
    # redirected so the watchdog's death never shows up between two rows.
    { kill -TERM "$killer" 2>/dev/null; wait "$killer"; } 2>/dev/null
    return $status
}

# `<agent id> <card id>` of the virtual agent named `$1`, from `agent.list`.
# No fallback: `agent.list` is a built method, and reading the roster out of
# `state.get` instead would turn its loss into a silent pass.
api_find_card() {
    api_ctl --scopes observe agent.list 2>&1 | python3 -c '
import json, sys
try:
    doc = json.load(sys.stdin)
except Exception:
    sys.exit(1)
for agent in doc.get("agents") or []:
    if agent.get("name") == sys.argv[1] and agent.get("card"):
        print(agent["id"], agent["card"]["id"])
        sys.exit(0)
sys.exit(1)
' "$1"
}

api_step() {
    api_deckd="$target_dir/$host/debug/deckd"
    if ! cargo build "${common[@]}" -p deckd >"$logs/build-deckd.log" 2>&1; then
        api_row "build" 1 "cargo build -p deckd failed (see $logs/build-deckd.log)"
        return
    fi
    local tmp=${TMPDIR:-/tmp}
    api_dir="${tmp%/}/deckd-gate-$$"
    # sun_path is 104 bytes: the socket must fit or the UDS door never binds.
    if [ $(( ${#api_dir} + 11 )) -gt 103 ]; then
        api_row "data dir" 1 "$api_dir/deckd.sock is longer than sun_path allows (set TMPDIR to something short)"
        return
    fi
    rm -rf "$api_dir"
    mkdir -p "$api_dir" || { api_row "data dir" 1 "cannot create $api_dir"; return; }
    # Self-contained: no roster seeding from the operator's own Claude
    # sessions and no attaching to their Codex app-server daemon. The TCP
    # door is off unless the file switches it on, and this step is what
    # exercises it until it is deleted, so `tcp = true` (named, the daemon
    # never looks in Claude's settings to decide it).
    printf '[api]\nport = 0\ntcp = true\nallowed_origins = ["http://localhost:5173"]\n\n[liveness]\nseed = false\n\n[codex]\ndaemon = false\n' >"$api_dir/config.toml"
    api_row "data dir" 0 "$api_dir ([api] port = 0 and tcp = true, never 47201, never the real data dir)"

    # The token is minted before the daemon starts: `deckd token` edits
    # tokens.toml directly and the daemon reads it at start. The reload of
    # a token minted while it runs is a row of its own below.
    local token
    token=$(DECKD_DATA_DIR="$api_dir" "$api_deckd" token create --name gate --scopes observe,agent,operate 2>"$logs/api-token.err")
    case "$token" in
        dk_*) api_row "token create" 0 "plaintext dk_… (${#token} chars), only the hash on disk" ;;
        *) api_row "token create" 1 "no plaintext (see $logs/api-token.err)"; return ;;
    esac

    local log="$logs/api-deckd.log"
    DECKD_DATA_DIR="$api_dir" "$api_deckd" run --foreground --no-device >"$log" 2>&1 &
    api_pid=$!
    if ! api_wait_file "$api_dir/port" 15; then
        api_row "daemon" 1 "no port file after 15 s (see $log)"
        tail -n 20 "$log"
        api_cleanup
        return
    fi
    local port
    port=$(tr -d '[:space:]' <"$api_dir/port")
    api_row "daemon" 0 "pid $api_pid, tcp 127.0.0.1:$port, uds $api_dir/deckd.sock"

    # /v1/info is the only unauthenticated route.
    local body code
    body=$(api_curl -w '\n%{http_code}' "http://127.0.0.1:$port/v1/info")
    code=${body##*$'\n'}
    if [ "$code" = 200 ] && [ "$(printf '%s' "${body%$'\n'*}" | api_field api)" = 1 ]; then
        api_row "/v1/info" 0 "200 without a token, api=1, port=$(printf '%s' "${body%$'\n'*}" | api_field port)"
    else
        api_row "/v1/info" 1 "http $code: ${body%$'\n'*}"
    fi

    # No bearer: 401 with a JSON-RPC -32001 body.
    body=$(api_curl -w '\n%{http_code}' -X POST "http://127.0.0.1:$port/v1/rpc" \
        -H 'content-type: application/json' \
        -d '{"jsonrpc":"2.0","id":1,"method":"session.ping","params":{}}')
    code=${body##*$'\n'}
    if [ "$code" = 401 ] && [ "$(printf '%s' "${body%$'\n'*}" | api_field error code)" = -32001 ]; then
        api_row "/v1/rpc no token" 0 "401 with a JSON-RPC -32001 body"
    else
        api_row "/v1/rpc no token" 1 "http $code: ${body%$'\n'*}"
    fi

    # The bearer: session.ping must answer with a timestamp.
    body=$(api_curl -w '\n%{http_code}' -X POST "http://127.0.0.1:$port/v1/rpc" \
        -H "authorization: Bearer $token" -H 'content-type: application/json' \
        -d '{"jsonrpc":"2.0","id":1,"method":"session.ping","params":{"client":{"name":"gate"}}}')
    code=${body##*$'\n'}
    local ts
    ts=$(printf '%s' "${body%$'\n'*}" | api_field result ts)
    if [ "$code" = 200 ] && [ -n "$ts" ]; then
        api_row "/v1/rpc token" 0 "200, result.ts $ts"
    else
        api_row "/v1/rpc token" 1 "http $code: ${body%$'\n'*}"
    fi

    # A foreign Origin is refused on every route; an allowed one passes; a
    # foreign Host is a 400.
    local evil allowed bad_host
    evil=$(api_curl -o /dev/null -w '%{http_code}' -X POST "http://127.0.0.1:$port/v1/rpc" \
        -H 'Origin: http://evil.example' -H "authorization: Bearer $token" \
        -H 'content-type: application/json' -d '{"jsonrpc":"2.0","id":1,"method":"session.ping"}')
    allowed=$(api_curl -o /dev/null -w '%{http_code}' -H 'Origin: http://localhost:5173' "http://127.0.0.1:$port/v1/info")
    bad_host=$(api_curl -o /dev/null -w '%{http_code}' -H 'Host: evil.example' "http://127.0.0.1:$port/v1/info")
    if [ "$evil" = 403 ] && [ "$allowed" = 200 ] && [ "$bad_host" = 400 ]; then
        api_row "origin + host" 0 "foreign Origin 403, allowed Origin 200, foreign Host 400"
    else
        api_row "origin + host" 1 "evil=$evil allowed=$allowed bad_host=$bad_host (want 403/200/400)"
    fi

    # The UDS door through `deckd ctl` (which is `deck-client`).
    local ctl_out ctl_code
    ctl_out=$(api_ctl session.ping 2>&1)
    ctl_code=$?
    if [ "$ctl_code" -eq 0 ] && [ -n "$(printf '%s' "$ctl_out" | api_field ts)" ]; then
        api_row "deckd ctl uds" 0 "session.ping over the socket: $ctl_out"
    else
        api_row "deckd ctl uds" 1 "exit $ctl_code: $ctl_out"
    fi

    # A token minted while the daemon runs needs the tokens.toml watcher.
    local live_token live_code
    live_token=$(DECKD_DATA_DIR="$api_dir" "$api_deckd" token create --name gate-live --scopes observe 2>/dev/null)
    sleep 1
    live_code=$(api_curl -o /dev/null -w '%{http_code}' -X POST "http://127.0.0.1:$port/v1/rpc" \
        -H "authorization: Bearer $live_token" -H 'content-type: application/json' \
        -d '{"jsonrpc":"2.0","id":1,"method":"session.ping","params":{}}')
    if [ "$live_code" = 200 ]; then
        api_row "tokens.toml reload" 0 "a token minted while the daemon runs is accepted"
    else
        api_row "tokens.toml reload" 1 "http $live_code (the tokens.toml watcher, §3.7)"
    fi

    # The §14 virtual agent: it registers, sets a card, parks in agent.wait;
    # the gate answers it with `deckd ctl op.answer` and it must exit 0.
    local script="$root/host/examples/virtual_agent.py"
    local agent_log="$logs/api-virtual-agent.log"
    if [ ! -f "$script" ]; then
        api_row "virtual agent" 1 "host/examples/virtual_agent.py is missing"
    elif ! command -v python3 >/dev/null 2>&1; then
        # Every row above already parses its JSON with python3: without it
        # this is not a skippable row, it is a broken gate.
        api_row "virtual agent" 1 "python3 is not installed (the gate needs it)"
    else
        python3 "$script" --port "$port" --token "$token" --name gatebot >"$agent_log" 2>&1 &
        api_agent_pid=$!
        local found="" waited=0
        while [ -z "$found" ] && [ "$waited" -lt 100 ]; do
            found=$(api_find_card gatebot)
            [ -n "$found" ] && break
            sleep 0.1
            waited=$((waited + 1))
        done
        if [ -z "$found" ]; then
            api_row "virtual agent" 1 "no card from gatebot within 10 s (see $agent_log)"
            kill -TERM "$api_agent_pid" 2>/dev/null
        else
            local agent_id=${found%% *} card_id=${found##* } answer answer_code agent_exit
            answer=$(api_ctl --scopes operate op.answer \
                "{\"agent\":\"$agent_id\",\"card\":$card_id,\"choice\":0}" 2>&1)
            answer_code=$?
            if [ "$answer_code" -ne 0 ] && ancestry_refused "$answer"; then
                api_skip "virtual agent" "op.answer: $ANCESTRY_SKIP"
                kill -TERM "$api_agent_pid" 2>/dev/null
            else
                api_wait_pid "$api_agent_pid" 15
                agent_exit=$?
                # Design §4.9: the answer is written after the delivery, so the
                # parked `agent.wait` makes `delivered` true, not a promise.
                local delivered=1
                case $answer in *'"delivered":true'*) delivered=0 ;; esac
                if [ "$answer_code" -eq 0 ] && [ "$agent_exit" -eq 0 ] && [ "$delivered" -eq 0 ]; then
                    api_row "virtual agent" 0 "$agent_id card $card_id answered (delivered:true), the script exited 0"
                else
                    api_row "virtual agent" 1 "op.answer exit $answer_code ($answer), script exit $agent_exit (see $agent_log)"
                    kill -TERM "$api_agent_pid" 2>/dev/null
                fi
            fi
        fi
        api_agent_pid=""
    fi

    # websocat is optional: the Rust WS test (`api_ws::ws_round_trip_with_events`)
    # is the one that must pass.
    if ! command -v websocat >/dev/null 2>&1; then
        api_skip "websocat" "not installed (the Rust WS test covers the door)"
    else
        local ws_out="$logs/api-websocat.txt"
        : >"$ws_out"
        printf '%s\n' '{"jsonrpc":"2.0","id":1,"method":"session.hello","params":{"client":{"name":"websocat","version":"1"}}}' \
            | websocat -t --no-close -H="Authorization: Bearer $token" \
              "ws://127.0.0.1:$port/v1/ws" >"$ws_out" 2>"$logs/api-websocat.err" &
        local ws_pid=$!
        api_wait_file "$ws_out" 8
        kill -TERM "$ws_pid" 2>/dev/null
        wait "$ws_pid" 2>/dev/null
        if [ -n "$(head -n 1 "$ws_out" | api_field result session)" ]; then
            api_row "websocat" 0 "hello over /v1/ws: $(head -n 1 "$ws_out" | api_field result session)"
        else
            api_row "websocat" 1 "no hello reply (see $ws_out, $logs/api-websocat.err)"
        fi
    fi

    # SIGTERM: an orderly stop, exit 0, and nothing panicked on the way.
    local daemon_exit
    kill -TERM "$api_pid" 2>/dev/null
    api_wait_pid "$api_pid" 15
    daemon_exit=$?
    api_pid=""
    if [ "$daemon_exit" -eq 0 ]; then
        api_row "sigterm" 0 "the daemon exited 0"
    else
        api_row "sigterm" 1 "the daemon exited $daemon_exit (see $log)"
    fi
    if grep -qi panic "$log"; then
        api_row "no panic" 1 "the log has a panic (see $log)"
        grep -i -m 3 panic "$log"
    else
        api_row "no panic" 0 "$(wc -l <"$log" | tr -d ' ') log lines, no panic"
    fi
    api_cleanup
}

api_step
api_cleanup          # the early `return`s above leave the data dir behind
trap - INT TERM EXIT # the adapters step below owns the signals from here
if [ "$api_failures" -ne 0 ]; then
    failures=$((failures + api_failures))
fi

# ---------------------------------------------------------------------------
# The `adapters` step (HOST-CONTRACT-W3 §6, last paragraph): `deckd
# integrations install` into a throwaway `CLAUDE_CONFIG_DIR` / `CODEX_HOME`,
# the files it wrote put in front of the real validators, then `uninstall`
# and a byte-for-byte diff against the originals.
#
# Nothing here goes near the operator's own `~/.claude`, `~/.codex` or
# `~/Library/LaunchAgents`: every path is under `$TMPDIR` (a temp `HOME`, the
# two config dirs and the daemon's `--data-dir`) and the whole tree is removed
# on exit, Ctrl-C included. The two CLI rows `skip` with a printed reason when
# the CLI is not installed — they are the only skips this step allows.
#
# Why each row is what it is:
#  * `claude doctor` **exits 0 even on a rejected settings file** (C24b,
#    20/20 runs in S2 and again here on 2.1.278: a copy with a `hooks` key
#    planted inside `_openwalkietalkie` printed `Invalid settings` and still
#    exited 0). The row therefore parses stdout for the literal `Invalid
#    settings` and never the exit code. It runs against a *copy* of the
#    written config dir, so the CLI's own scratch writes can never show up in
#    the restore diff.
#  * `settings shape` re-asserts C24a on the bytes deckd wrote, with no CLI in
#    the loop: a key named `hooks`, `PreToolUse` or `PermissionRequest` at any
#    depth inside the `_openwalkietalkie` block is a *fatal* diagnostic that
#    silently voids the user's entire settings.json — the model falls back to
#    its default and `Hooks: Found 0 total hooks in registry`, with nothing on
#    screen. It is the one landmine row that still runs when `claude` is
#    absent, and the row that would catch the regression first.
#  * Codex's `command` is a **string**. The array form makes Codex discard the
#    whole file with nothing but a `warnings` string and `errors: []` (C12,
#    reproduced here: `hooks: []`, `warnings: ["failed to parse hooks config
#    …: invalid type: sequence, expected a string"]`), so a row that only
#    asserted "it is valid JSON" would pass while every Codex hook on the
#    machine is dead. `hooks/list` over the app-server is the only invocation
#    that sees it: there is no `codex hooks list` subcommand in 0.153.4, and
#    `codex doctor --json` never mentions hooks.json at all (measured against
#    the broken file: no `failed to parse` anywhere in its 16 kB of checks,
#    and its own `overallStatus` is `fail` for unrelated reasons). So the row
#    drives the JSON-RPC (initialize → initialized → hooks/list) the way S4
#    did and demands a NON-EMPTY `hooks` array with empty `warnings`/`errors`.
#  * `codex trust` proves it from the installer's side: `--trust` can only
#    record a `currentHash` that `codex` handed back through `hooks/list`
#    (C14a iii), and `codex` only hands one back for a file it parsed.
ad_failures=0
ad_dir=""
ad_cleanup() {
    [ -n "$ad_dir" ] && [ -d "$ad_dir" ] && rm -rf "$ad_dir"
    ad_dir=""
}
trap 'ad_cleanup; exit 130' INT TERM
trap ad_cleanup EXIT

ad_row() {  # name, status (0 = ok), detail
    if [ "$2" -eq 0 ]; then
        printf 'ok    adapters %-16s %s\n' "$1" "$3"
    else
        printf 'FAIL  adapters %-16s %s\n' "$1" "$3"
        ad_failures=$((ad_failures + 1))
    fi
}
ad_skip() { printf 'skip  adapters %-16s %s\n' "$1" "$2"; }

# The three installer-owned files, relative to `$ad_dir`. Only these are
# deckd's to write, so only these are copied and diffed: an app-server log or
# a session file the CLI itself dropped into `CODEX_HOME` is not ours to
# restore.
ad_owned=(claude/settings.json codex/config.toml codex/hooks.json)

# The copies the restore is diffed against, taken before the first install.
ad_take_copies() {
    local name
    mkdir -p "$ad_dir/before" || return 1
    for name in "${ad_owned[@]}"; do
        cp "$ad_dir/$name" "$ad_dir/before/${name//\//_}" || return 1
    done
}

ad_step() {
    local deckd="$target_dir/$host/debug/deckd"
    if [ ! -x "$deckd" ]; then
        ad_row "build" 1 "no deckd at $deckd"
        return
    fi
    if ! command -v python3 >/dev/null 2>&1; then
        ad_row "build" 1 "python3 is not installed (the shape rows need it)"
        return
    fi
    local tmp=${TMPDIR:-/tmp}
    ad_dir="${tmp%/}/deckd-adapters-$$"
    rm -rf "$ad_dir"
    if ! mkdir -p "$ad_dir/claude" "$ad_dir/codex" "$ad_dir/data" "$ad_dir/home"; then
        ad_row "temp dirs" 1 "cannot create $ad_dir"
        return
    fi
    # A port that is never bound: this step writes files, it runs no daemon.
    # No `tcp` key either: nothing the installer writes reaches the daemon
    # through the TCP door, which is off by default.
    printf '[api]\nport = 47250\n\n[hooks.claude]\nhold_s = 120\n' >"$ad_dir/data/config.toml"
    # The originals: a settings file carrying the keys a fatal diagnostic
    # would silently take away, a foreign `SessionStart` hook on one of our
    # own events (exit test 12), and a Codex config the installer has to merge
    # into rather than replace.
    cat >"$ad_dir/claude/settings.json" <<'JSON'
{
  "model": "claude-haiku-4-5",
  "permissions": {"ask": ["Bash(date:*)"]},
  "statusLine": {"type": "command", "command": "npx ccstatusline@latest"},
  "hooks": {
    "SessionStart": [
      {"matcher": "startup", "hooks": [{"type": "command", "command": "/bin/echo not-ours"}]}
    ]
  }
}
JSON
    printf '# the gate\nmodel = "gpt-6"\n' >"$ad_dir/codex/config.toml"
    printf '{\n  "hooks": {}\n}\n' >"$ad_dir/codex/hooks.json"
    if ! ad_take_copies; then
        ad_row "temp dirs" 1 "cannot copy the originals aside"
        return
    fi
    ad_row "temp dirs" 0 "$ad_dir (claude, codex, data, home; the user's own config is untouched)"

    # `env -i` is deliberately NOT used: `deckd` probes for the two CLIs and
    # needs a PATH. What matters is that `HOME` and both config dirs point
    # inside `$ad_dir` and that the data dir is named on the command line.
    local ad_env=(env "HOME=$ad_dir/home"
        "CLAUDE_CONFIG_DIR=$ad_dir/claude" "CODEX_HOME=$ad_dir/codex")
    local ad_deckd=("${ad_env[@]}" "$deckd" --data-dir "$ad_dir/data")
    local out rc target

    # Install, per target, and again to prove it is idempotent (exit test 11).
    for target in claude codex; do
        out=$("${ad_deckd[@]}" integrations install "$target" 2>&1)
        rc=$?
        if [ "$rc" -ne 0 ]; then
            ad_row "install $target" 1 "exit $rc: $(printf '%s' "$out" | tail -n 3 | tr '\n' ' ')"
            continue
        fi
        local wrote again
        wrote=$(printf '%s\n' "$out" | grep -c '^wrote ')
        again=$("${ad_deckd[@]}" integrations install "$target" 2>&1 | grep -c '^wrote ')
        if [ "$wrote" -ge 1 ] && [ "$again" -eq 0 ]; then
            ad_row "install $target" 0 "$wrote file(s) written, second run idempotent"
        else
            ad_row "install $target" 1 "wrote=$wrote, second run wrote=$again"
        fi
    done

    # C24b: the settings file deckd wrote, in front of Claude's own validator,
    # on a copy so `claude` cannot touch what the restore diffs.
    #
    # The probe runs on a deliberately minimal PATH. `claude doctor` shells
    # out to `npm -g config get prefix` to name its install method, and under
    # a temp `HOME` a user's npm shim can block forever (measured on this
    # machine: `/Users/…/.vite-plus/bin/npm` never returns without its own
    # `$HOME`, and `claude doctor` sat there until the watchdog killed it,
    # with no output at all). Nothing this row asserts depends on npm, so the
    # probe gets `/usr/bin:/bin:/usr/sbin:/sbin` and the CLI by absolute path;
    # the cost is a cosmetic `config install method is 'unknown'` warning in
    # the log. If it still produces nothing inside the watchdog, that is a
    # FAIL, not a skip: an unanswerable validator is not a passed check.
    local claude_bin
    claude_bin=$(command -v claude 2>/dev/null)
    if [ -z "$claude_bin" ]; then
        ad_skip "claude doctor" "claude is not on PATH"
    elif ! cp -R "$ad_dir/claude" "$ad_dir/claude-probe"; then
        ad_row "claude doctor" 1 "cannot copy the written config dir"
    else
        local doctor="$logs/adapters-claude-doctor.log"
        ( HOME="$ad_dir/home" CLAUDE_CONFIG_DIR="$ad_dir/claude-probe" \
            PATH=/usr/bin:/bin:/usr/sbin:/sbin \
            "$claude_bin" doctor </dev/null >"$doctor" 2>&1 ) &
        local doctor_pid=$!
        api_wait_pid "$doctor_pid" 90
        if [ ! -s "$doctor" ]; then
            ad_row "claude doctor" 1 "no output in 90 s (see $doctor)"
        elif grep -q 'Invalid settings' "$doctor"; then
            ad_row "claude doctor" 1 "the whole settings file would be ignored (see $doctor)"
            grep -A 3 -m 1 'Invalid settings' "$doctor"
        else
            ad_row "claude doctor" 0 \
                "no \`Invalid settings\` line ($(grep -m 1 '^Running:' "$doctor" | tr -d '\r'))"
        fi
    fi

    # The plugin, on the bytes, with no CLI in the loop: the three files under
    # the daemon's own data dir, every hook a `command` hook running `deckd
    # hook claude` (no http hook, no bearer, no port), and the user's
    # settings.json carrying nothing of ours — the foreign `SessionStart`
    # group is still the only hook in it. Nothing of the files' content is
    # printed but counts (§11).
    local shape
    shape=$(python3 - "$ad_dir/data/claude-marketplace" "$ad_dir/claude/settings.json" <<'PY'
import json, os, sys
root, settings_path = sys.argv[1], sys.argv[2]
try:
    marketplace = json.load(open(os.path.join(root, ".claude-plugin", "marketplace.json")))
    plugin = json.load(open(os.path.join(root, "plugins", "openwalkietalkie", ".claude-plugin", "plugin.json")))
    hooks = json.load(open(os.path.join(root, "plugins", "openwalkietalkie", "hooks", "hooks.json")))
    settings = json.load(open(settings_path))
except Exception as e:
    print(f"a plugin file is missing or does not parse: {e}")
    raise SystemExit(1)
if marketplace.get("name") != "openwalkietalkie" or plugin.get("name") != "openwalkietalkie":
    print("the marketplace or the plugin is not named openwalkietalkie")
    raise SystemExit(1)
bad, seen = [], 0
for event, groups in (hooks.get("hooks") or {}).items():
    for group in groups:
        for handler in group.get("hooks") or []:
            seen += 1
            if handler.get("type") != "command" or handler.get("args") != ["hook", "claude"]:
                bad.append(f"{event}: not an exec-form `deckd hook claude` command hook")
            if "url" in handler or "headers" in handler:
                bad.append(f"{event}: an http hook in a plugin that must carry none")
if bad:
    print("; ".join(bad))
    raise SystemExit(1)
if seen < 8:
    print(f"only {seen} handler(s) in hooks.json")
    raise SystemExit(1)
text = json.dumps(settings)
if "_openwalkietalkie" in settings or "allowedHttpHookUrls" in settings or "/v1/hooks/" in text:
    print("settings.json carries pre-plugin deckd entries")
    raise SystemExit(1)
user_hooks = settings.get("hooks") or {}
if list(user_hooks) != ["SessionStart"] or len(user_hooks["SessionStart"]) != 1:
    print(f"settings.json hooks changed: {sorted(user_hooks)}")
    raise SystemExit(1)
print(f"{seen} command hooks in the plugin; settings.json untouched but for Claude's own plugin records")
PY
    )
    ad_row "plugin shape" $? "$shape"

    # And as Claude itself sees it: the plugin listed, enabled, at user scope,
    # read in place from the marketplace directory. `integrations install`
    # ran the `claude plugin` commands into the throwaway config dir.
    if [ -z "$claude_bin" ]; then
        ad_skip "claude plugin list" "claude is not on PATH"
    else
        local plugins="$logs/adapters-claude-plugins.log"
        ( HOME="$ad_dir/home" CLAUDE_CONFIG_DIR="$ad_dir/claude" \
            PATH=/usr/bin:/bin:/usr/sbin:/sbin \
            "$claude_bin" plugin list --json </dev/null >"$plugins" 2>&1 ) &
        local plugins_pid=$!
        api_wait_pid "$plugins_pid" 90
        local listed
        listed=$(python3 - "$plugins" "$ad_dir/data/claude-marketplace/plugins/openwalkietalkie" <<'PY'
import json, os, sys
text = open(sys.argv[1]).read()
start = text.find("[")
if start < 0:
    print("claude plugin list printed no JSON array")
    raise SystemExit(1)
try:
    rows = json.loads(text[start:])
except Exception as e:
    print(f"claude plugin list is not parseable: {e}")
    raise SystemExit(1)
ours = [r for r in rows if r.get("id") == "openwalkietalkie@openwalkietalkie"]
if not ours:
    print("the plugin is not listed")
    raise SystemExit(1)
row = ours[0]
if row.get("scope") != "user" or row.get("enabled") is not True:
    print(f"listed but scope={row.get('scope')!r} enabled={row.get('enabled')!r}")
    raise SystemExit(1)
folder = row.get("readFromFolder") or row.get("installPath") or ""
if os.path.realpath(folder) != os.path.realpath(sys.argv[2]):
    print(f"read from {folder!r}, not the marketplace directory")
    raise SystemExit(1)
print(f"listed, enabled at user scope, version {row.get('version')}, read in place")
PY
        )
        ad_row "claude plugin list" $? "$listed"
    fi

    # C12, on the bytes: every Codex `command` is a string.
    local shape
    shape=$(python3 - "$ad_dir/codex/hooks.json" <<'PY'
import json, sys
try:
    root = json.load(open(sys.argv[1]))
except Exception as e:
    print(f"hooks.json does not parse: {e}")
    raise SystemExit(1)
bad, seen = [], 0
for event, groups in (root.get("hooks") or {}).items():
    for group in groups:
        for handler in group.get("hooks") or []:
            seen += 1
            if not isinstance(handler.get("command"), str):
                bad.append(f"{event}: command is {type(handler.get('command')).__name__}, not str")
if bad:
    print("; ".join(bad))
    raise SystemExit(1)
if not seen:
    print("no handlers in hooks.json at all")
    raise SystemExit(1)
print(f"{seen} handler(s), every `command` a shell-quoted string")
PY
    )
    ad_row "codex command" $? "$shape"

    # C12, the way Codex itself sees it — the row that separates "valid JSON"
    # from "Codex loaded it". See the header for why `hooks/list` and not
    # `codex doctor --json`.
    if ! command -v codex >/dev/null 2>&1; then
        ad_skip "codex hooks/list" "codex is not on PATH"
    else
        cat >"$ad_dir/hooks_list.py" <<'PY'
"""`codex app-server` → initialize, initialized, hooks/list. Prints one
`summary: …` line and exits 0 only when Codex parsed the whole file.

The app-server is started in its own session and the whole group is killed on
the way out, SIGTERM included, so the gate's watchdog can never orphan it."""
import json, os, shutil, signal, subprocess, sys, threading, time

cwd = sys.argv[1]
deadline = time.monotonic() + 60
signal.signal(signal.SIGTERM, lambda *_: sys.exit(1))  # so `finally` still runs
proc = subprocess.Popen(
    [shutil.which("codex"), "app-server", "--listen", "stdio://"],
    stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
    text=True, bufsize=1, cwd=cwd, start_new_session=True,
)
lines: list[str] = []
threading.Thread(
    target=lambda: [lines.append(line) for line in proc.stdout], daemon=True
).start()


def send(obj):
    proc.stdin.write(json.dumps(obj) + "\n")
    proc.stdin.flush()


def answer(want):  # the reply to request id `want`, or None on the deadline
    while time.monotonic() < deadline:
        for line in list(lines):
            try:
                obj = json.loads(line)
            except Exception:
                continue
            if obj.get("id") == want:
                return obj
        time.sleep(0.1)
    return None


rc = 1
try:
    send({"method": "initialize", "id": 0, "params": {
        "clientInfo": {"name": "owt-gate", "title": "OpenWalkieTalkie gate",
                       "version": "0"},
        "capabilities": {"experimentalApi": True}}})
    if answer(0) is None:
        print("codex app-server never answered initialize")
    else:
        send({"method": "initialized", "params": {}})
        send({"method": "hooks/list", "id": 1, "params": {"cwds": [cwd]}})
        reply = answer(1)
        data = ((reply or {}).get("result") or {}).get("data") or []
        if reply is None:
            print("codex app-server never answered hooks/list")
        elif reply.get("error"):
            print(f"hooks/list failed: {reply['error']}")
        elif not data:
            print("hooks/list returned no data rows")
        else:
            row = data[0]
            hooks = row.get("hooks") or []
            warnings = row.get("warnings") or []
            errors = row.get("errors") or []
            mine = [h for h in hooks if "hook codex" in (h.get("command") or "")]
            if warnings or errors:
                print(f"Codex DISCARDED the file: warnings={warnings} errors={errors}")
            elif not hooks:
                print("hooks/list returned an EMPTY hooks array — the whole file is void")
            elif not mine:
                print(f"{len(hooks)} hook(s) parsed but none is deckd's")
            else:
                trust = sorted({h.get("trustStatus") for h in mine})
                enabled = sorted({h.get("enabled") for h in mine})
                print(f"summary: {len(hooks)} hook(s) parsed, {len(mine)} deckd's, "
                      f"warnings=[] errors=[], enabled={enabled}, trustStatus={trust}")
                rc = 0
finally:
    try:
        os.killpg(proc.pid, signal.SIGTERM)
    except Exception:
        proc.terminate()
    try:
        proc.wait(timeout=5)
    except Exception:
        proc.kill()
sys.exit(rc)
PY
        local listed="$logs/adapters-codex-hooks-list.log"
        ( "${ad_env[@]}" python3 "$ad_dir/hooks_list.py" "$ad_dir" >"$listed" 2>&1 ) &
        local list_pid=$!
        api_wait_pid "$list_pid" 90
        local list_rc=$?
        if [ "$list_rc" -eq 0 ]; then
            ad_row "codex hooks/list" 0 "$(sed -n 's/^summary: //p' "$listed" | head -n 1)"
        else
            ad_row "codex hooks/list" 1 \
                "exit $list_rc: $(head -n 2 "$listed" | tr '\n' ' ') (see $listed)"
        fi
    fi

    # C14/C14a: `--trust` can only record a hash `codex` gave it, and `codex`
    # can only give one for a hooks.json it actually parsed — so this row is
    # the parse check from the installer's own side. The key is the realpath
    # of hooks.json (C14a i), which is why it is asserted on the written file
    # and not on the path we passed in.
    if ! command -v codex >/dev/null 2>&1; then
        ad_skip "codex trust" "codex is not on PATH"
    else
        out=$("${ad_deckd[@]}" integrations install codex --trust 2>&1)
        rc=$?
        local states
        states=$(grep -c '^\[hooks\.state\."' "$ad_dir/codex/config.toml")
        if [ "$rc" -eq 0 ] && [ "$states" -ge 1 ] && ! printf '%s' "$out" | grep -q '^WARNING'; then
            ad_row "codex trust" 0 "codex hashed $states handler(s); the file parses"
        else
            ad_row "codex trust" 1 \
                "exit $rc, [hooks.state] entries=$states: $(printf '%s' "$out" | tail -n 2 | tr '\n' ' ')"
        fi
    fi

    # The status vocabulary (§4.11), machine-readable, with nothing written.
    # `claude` reports `installed` once Claude has registered the plugin and
    # `outdated` when the CLI was not there to do it; the foreign
    # `SessionStart` group in the fixture is simply not deckd's business any
    # more. What the row refuses is `absent` (the install did not take) and
    # `conflict` (a file deckd must read could not be read).
    local reported
    reported=$("${ad_deckd[@]}" integrations status --json 2>/dev/null | python3 -c '
import json, sys
try:
    rows = {r["target"]: r["status"] for r in json.load(sys.stdin)}
except Exception as e:
    print(f"integrations status --json is not parseable: {e}")
    raise SystemExit(1)
for want in ("claude", "codex"):
    if rows.get(want) in (None, "absent", "conflict"):
        print(f"{want} reports {rows.get(want)!r} right after an install")
        raise SystemExit(1)
print(" ".join(f"{k}={v}" for k, v in sorted(rows.items())))
')
    ad_row "status" $? "$reported"

    # Uninstall both, then the byte-for-byte diff (exit test 15). The `codex`
    # uninstall has to take the `[hooks.state]` hashes `--trust` wrote back
    # out again, so this also closes the loop on the row above.
    for target in claude codex; do
        out=$("${ad_deckd[@]}" integrations uninstall "$target" 2>&1)
        rc=$?
        if [ "$rc" -eq 0 ]; then
            ad_row "uninstall $target" 0 \
                "$(printf '%s\n' "$out" | grep -c '^wrote ') file(s) rewritten"
        else
            ad_row "uninstall $target" 1 \
                "exit $rc: $(printf '%s' "$out" | tail -n 2 | tr '\n' ' ')"
        fi
    done
    # The plugin's own files go with the uninstall (they are deckd's, under
    # the data dir, so the restore diff below does not cover them).
    if [ -e "$ad_dir/data/claude-marketplace/plugins/openwalkietalkie/hooks/hooks.json" ]; then
        ad_row "plugin removed" 1 "the plugin's hooks.json is still under the data dir"
    else
        ad_row "plugin removed" 0 "no plugin files left under the data dir"
    fi

    # Exit test 15, on the bytes: every owned file against the copy taken
    # before the install, `cmp` and not a hash, so a failure can say what
    # changed instead of only that something did. A file the install deleted
    # and never put back fails here too: `cmp` cannot read it.
    # `claude/settings.json` is the exception since the plugin: deckd never
    # writes it, but `claude plugin marketplace add`/`install`/`uninstall`
    # do — Claude re-serialises the whole file around its own
    # `extraKnownMarketplaces`/`enabledPlugins` keys and leaves them behind
    # empty. So that file is compared as JSON with those two keys set aside
    # (and must carry nothing of deckd's), the other two byte for byte.
    local changed=() name
    : >"$logs/adapters-restore.diff"
    for name in "${ad_owned[@]}"; do
        if [ "$name" = claude/settings.json ]; then
            if ! python3 - "$ad_dir/before/${name//\//_}" "$ad_dir/$name" <<'PY'
import json, sys
before, after = (json.load(open(p)) for p in sys.argv[1:3])
for key in ("extraKnownMarketplaces", "enabledPlugins"):
    if after.get(key) in ({}, None):
        after.pop(key, None)
if "_openwalkietalkie" in after or "allowedHttpHookUrls" in after or "/v1/hooks/" in json.dumps(after):
    print("settings.json still carries something of deckd's")
    raise SystemExit(1)
raise SystemExit(0 if before == after else 1)
PY
            then
                changed+=("$name")
                diff -u "$ad_dir/before/${name//\//_}" "$ad_dir/$name" \
                    >>"$logs/adapters-restore.diff" 2>&1
            fi
            continue
        fi
        if ! cmp -s "$ad_dir/before/${name//\//_}" "$ad_dir/$name"; then
            changed+=("$name")
            diff -u "$ad_dir/before/${name//\//_}" "$ad_dir/$name" \
                >>"$logs/adapters-restore.diff" 2>&1
        fi
    done
    if [ "${#changed[@]}" -eq 0 ]; then
        ad_row "restored" 0 "${#ad_owned[@]} file(s): hooks.json and config.toml byte for byte, settings.json as Claude re-serialised it"
    else
        ad_row "restored" 1 \
            "${changed[*]} came back changed (see $logs/adapters-restore.diff)"
        head -n 20 "$logs/adapters-restore.diff"
    fi
    # The backups the installer took live in the daemon's data dir, never
    # beside the user's own config (C22).
    local strays kept
    # Only deckd's own `*.bak` names count: `claude plugin …` keeps its own
    # `backups/.claude.json.backup.<ts>` under the config dir, which is
    # Claude's and not a stray of ours.
    strays=$(find "$ad_dir/claude" "$ad_dir/codex" -name '*.bak' 2>/dev/null | wc -l | tr -d ' ')
    kept=$(find "$ad_dir/data/backups" -name '*.bak' 2>/dev/null | wc -l | tr -d ' ')
    if [ "$strays" -eq 0 ] && [ "$kept" -ge 1 ]; then
        ad_row "backups" 0 "$kept in <data>/backups, none beside the user's config"
    else
        ad_row "backups" 1 "strays=$strays, <data>/backups=$kept"
    fi
    ad_cleanup
}

ad_step
ad_cleanup
trap - INT TERM EXIT
if [ "$ad_failures" -ne 0 ]; then
    failures=$((failures + ad_failures))
fi

# The device checks (design §14 week 1, HOST-CONTRACT hardware rules). Every
# number comes from the status.json the daemon writes at shutdown.
hw_summary() {
    python3 - "$1" <<'PY'
import json, sys
s = json.load(open(sys.argv[1]))
m = s["metric"]
err = m["err_size"] + m["err_parse"] + m["err_range"] + m["err_unknown"]
# The link's session at the closing metric: the store's own `session` is
# already `absent` in the status written after an orderly shutdown.
print(f'sid={s["sid"]} session={m["session"]} fw={s.get("fw")} boot={s.get("boot")} '
      f'hello_tx={m["hello_sent"]} hello_rx={m["hello_replies"]} misses={m["hello_misses_total"]} '
      f'barriers={m["barriers"]} link_drops={m["link_drops"]} device_err={err} '
      f'decode_err={m["decode_errors"]} encode_err={m["encode_errors"]} '
      f'reports_tx={m["reports_sent"]} reports_rx={m["reports_received"]} lines_tx={m["lines_sent"]} '
      f'ch2={m["ch2_reports"]} pace={m["pace_ms"]} '
      f'write_avg_ms={m["write_avg_us"] / 1000:.0f} write_max_ms={m["write_max_us"] / 1000:.0f}')
PY
}
hw_field() { sed -n "s/.*[[:space:]]$2=\([^ ]*\).*/\1/p" <<<" $1"; }
hw_run() {
    # $1 name, $2 seconds: `deckd run --foreground --demo` on a private data dir.
    local name=$1 seconds=$2 log="$logs/hw-$1.log"
    DECKD_DATA_DIR="$hw_data" "$deckd" run --foreground --demo >"$log" 2>&1 &
    local pid=$!
    sleep "$seconds"
    kill -TERM "$pid" 2>/dev/null
    wait "$pid" 2>/dev/null
    if [ ! -s "$hw_data/status.json" ]; then
        printf 'FAIL  hardware %s: no status.json (see %s)\n' "$name" "$log"
        tail -n 20 "$log"
        return 1
    fi
    local summary
    summary=$(hw_summary "$hw_data/status.json")
    local synced
    synced=$(grep -c ' synced' "$log")
    printf 'ok    hardware %s (%s s): %s synced=%s\n' "$name" "$seconds" "$summary" "$synced"
    local fail=0
    [ "$(hw_field "$summary" hello_rx)" -ge 1 ] || { printf 'FAIL  hardware %s: no hello reply\n' "$name"; fail=1; }
    [ "$synced" -ge 1 ] || { printf 'FAIL  hardware %s: no synced\n' "$name"; fail=1; }
    [ "$(hw_field "$summary" link_drops)" -eq 0 ] || { printf 'FAIL  hardware %s: link drops\n' "$name"; fail=1; }
    [ "$(hw_field "$summary" device_err)" -eq 0 ] || { printf 'FAIL  hardware %s: device err\n' "$name"; fail=1; }
    [ "$(hw_field "$summary" session)" = "live" ] || { printf 'FAIL  hardware %s: session not live at the end\n' "$name"; fail=1; }
    hw_last_sid=$(hw_field "$summary" sid)
    hw_last_hello_rx=$(hw_field "$summary" hello_rx)
    return $fail
}
# ---------------------------------------------------------------------------
# The week-2 hardware rows (HOST-CONTRACT-W2 §7, recorded in
# docs/deck-device-checklist.md §11 "Week 2 rows"): one `deckd run
# --foreground --demo` against the connected deck, driven through the same
# commands the operator ran by hand. Every row prints ok or FAIL; nothing
# here is skipped silently. The one `skip` is a row §4.2's ancestry cap
# refuses when the gate runs inside Claude Code or Codex
# (`ancestry_refused`), and it says so.
w2_failures=0
w2_pid=""
w2_events_pid=""
w2_agent_pid=""
w2_row() {  # name, status (0 = ok), detail
    if [ "$2" -eq 0 ]; then
        printf 'ok    hardware w2 %-16s %s\n' "$1" "$3"
    else
        printf 'FAIL  hardware w2 %-16s %s\n' "$1" "$3"
        w2_failures=$((w2_failures + 1))
    fi
}
w2_skip() { printf 'skip  hardware w2 %-16s %s\n' "$1" "$2"; }
w2_cleanup() {
    [ -n "$w2_agent_pid" ] && kill -TERM "$w2_agent_pid" 2>/dev/null
    [ -n "$w2_events_pid" ] && kill -TERM "$w2_events_pid" 2>/dev/null
    [ -n "$w2_pid" ] && kill -TERM "$w2_pid" 2>/dev/null
    w2_agent_pid=""; w2_events_pid=""; w2_pid=""
}

# `deckd ctl` against the week-2 daemon; the RPC error JSON is on stderr,
# so every caller wants it folded in.
w2_ctl() { DECKD_DATA_DIR="$w2_dir" "$deckd" ctl "$@" 2>&1; }

# `<agent id> <card id>` of the agent matching a python predicate over the
# `agent.list` entry, e.g. `w2_find 'a["name"] == "cohdl-api"'`.
w2_find() {
    w2_ctl --scopes observe agent.list | python3 -c '
import json, sys
try:
    doc = json.load(sys.stdin)
except Exception:
    sys.exit(1)
for a in doc.get("agents") or []:
    if not a.get("card"):
        continue
    if eval(sys.argv[1], {}, {"a": a}):  # noqa: S307 - the gate own predicate
        print(a["id"], a["card"]["id"])
        sys.exit(0)
sys.exit(1)
' "$1"
}

# Wait up to `$2` seconds for `w2_find "$1"` to answer; echoes what it found.
w2_wait_for() {
    local found="" waited=0
    while [ "$waited" -lt "${2}0" ]; do
        found=$(w2_find "$1")
        [ -n "$found" ] && { printf '%s' "$found"; return 0; }
        sleep 0.1
        waited=$((waited + 1))
    done
    return 1
}

hw_week2() {  # $1: the data dir for this run
    w2_dir=$1
    local log="$logs/hw-w2-deckd.log"
    # sun_path is 104 bytes, and the TCP door must never take 47201. The
    # door is off unless the file switches it on, and these rows wait for
    # its port file.
    if [ $(( ${#w2_dir} + 11 )) -gt 103 ]; then
        w2_row "data dir" 1 "$w2_dir/deckd.sock is longer than sun_path allows"
        return
    fi
    printf '[api]\nport = 0\ntcp = true\n\n[liveness]\nseed = false\n\n[codex]\ndaemon = false\n' >"$w2_dir/config.toml"
    local token
    token=$(DECKD_DATA_DIR="$w2_dir" "$deckd" token create --name gate-hw \
        --scopes observe,agent,operate 2>"$logs/hw-w2-token.err")
    case "$token" in
        dk_*) ;;
        *) w2_row "token create" 1 "no plaintext (see $logs/hw-w2-token.err)"; return ;;
    esac

    # `--no-device` is for checking this block's plumbing off hardware; the
    # `--hardware` gate never sets it.
    local device_args=--demo
    if [ -n "${OWT_W2_NO_DEVICE:-}" ]; then
        device_args="--demo --no-device"
        printf 'info  hardware w2: OWT_W2_NO_DEVICE is set — these rows are NOT touching the deck\n'
    fi
    # shellcheck disable=SC2086
    DECKD_DATA_DIR="$w2_dir" "$deckd" run --foreground $device_args >"$log" 2>&1 &
    w2_pid=$!
    if ! api_wait_file "$w2_dir/port" 20; then
        w2_row "daemon" 1 "no port file after 20 s (see $log)"
        tail -n 20 "$log"
        w2_cleanup
        return
    fi
    local port
    port=$(tr -d '[:space:]' <"$w2_dir/port")
    w2_row "daemon" 0 "pid $w2_pid, demo roster, tcp 127.0.0.1:$port"

    # §7.1 — the demo roster, and the permission card on the deck.
    local roster cohdl
    roster=$(w2_ctl --scopes observe agent.list | api_field agents | python3 -c 'import json,sys; print(len(json.load(sys.stdin)))')
    cohdl=$(w2_wait_for 'a["name"] == "cohdl-api" and a["card"]["origin"]["kind"] == "permission"' 20)
    if [ "${roster:-0}" -ge 6 ] && [ -n "$cohdl" ]; then
        w2_row "roster" 0 "$roster agents, cohdl-api holds the permission card (${cohdl})"
    else
        w2_row "roster" 1 "agents=${roster:-none} permission card=${cohdl:-none}"
        w2_cleanup
        return
    fi

    # §7.2 — the guarded allow: an untrusted UDS peer may not say yes.
    local ev_log="$logs/hw-w2-events.log"
    : >"$ev_log"
    DECKD_DATA_DIR="$w2_dir" "$deckd" events --kinds 'op.*' >"$ev_log" 2>&1 &
    w2_events_pid=$!
    sleep 1
    local agent_id=${cohdl%% *} card_id=${cohdl##* } allow allow_code deny deny_code
    allow=$(w2_ctl --scopes operate op.answer "{\"agent\":\"$agent_id\",\"card\":$card_id,\"choice\":0}")
    allow_code=$?
    if [ "$allow_code" -ne 0 ] && ancestry_refused "$allow"; then
        w2_skip "guarded allow" "op.answer: $ANCESTRY_SKIP"
    else
        # The confirm event is published as the refusal is written; give the
        # `deckd events` reader a moment to print its line.
        local waited=0
        while [ "$waited" -lt 50 ] && ! grep -q op.confirm_requested "$ev_log"; do
            sleep 0.1
            waited=$((waited + 1))
        done
        local confirmed=1
        grep -q op.confirm_requested "$ev_log" && confirmed=0
        case $allow in *'"code":-32002'*) ;; *) allow_code=99 ;; esac
        if [ "$allow_code" -eq 3 ] && [ "$confirmed" -eq 0 ]; then
            w2_row "guarded allow" 0 "choice 0 → -32002, exit 3, op.confirm_requested on the event stream"
        else
            w2_row "guarded allow" 1 "exit $allow_code ($allow), op.confirm_requested seen=$([ $confirmed -eq 0 ] && echo yes || echo no; ) (see $ev_log)"
        fi
    fi
    # Deny is not guarded: it goes through the demo's hold and clears the card.
    deny=$(w2_ctl --scopes operate op.answer "{\"agent\":\"$agent_id\",\"card\":$card_id,\"choice\":2}")
    deny_code=$?
    if [ "$deny_code" -ne 0 ] && ancestry_refused "$deny"; then
        w2_skip "guarded deny" "op.answer: $ANCESTRY_SKIP"
        w2_skip "card re-set" "the deny it follows was refused by the §4.2 ancestry cap"
    else
        case $deny in
            *'"delivered":true'*'"via":"hold"'*) w2_row "guarded deny" 0 "choice 2 → $deny, the deck clears the card" ;;
            *) w2_row "guarded deny" 1 "$deny" ;;
        esac
        # The demo re-sets a fresh permission card about 3 s later.
        local reset
        reset=$(w2_wait_for "a[\"name\"] == \"cohdl-api\" and a[\"card\"][\"origin\"][\"kind\"] == \"permission\" and a[\"card\"][\"id\"] != $card_id" 10)
        if [ -n "$reset" ]; then
            w2_row "card re-set" 0 "a fresh permission card ($reset) within 10 s"
        else
            w2_row "card re-set" 1 "no new permission card after the deny"
        fi
    fi
    kill -TERM "$w2_events_pid" 2>/dev/null
    wait "$w2_events_pid" 2>/dev/null
    w2_events_pid=""

    # §7.3 — a poll card answered while the agent's `agent.wait` is parked.
    local script="$root/host/examples/virtual_agent.py"
    if [ ! -f "$script" ]; then
        w2_row "poll waiter" 1 "host/examples/virtual_agent.py is missing"
    else
        local agent_log="$logs/hw-w2-agent.log" found
        python3 "$script" --port "$port" --token "$token" --name hwbot >"$agent_log" 2>&1 &
        w2_agent_pid=$!
        found=$(w2_wait_for 'a["name"] == "hwbot"' 15)
        if [ -z "$found" ]; then
            w2_row "poll waiter" 1 "no card from hwbot within 15 s (see $agent_log)"
            kill -TERM "$w2_agent_pid" 2>/dev/null
        else
            local bot_id=${found%% *} bot_card=${found##* } answer answer_code agent_exit
            answer=$(w2_ctl --scopes operate op.answer \
                "{\"agent\":\"$bot_id\",\"card\":$bot_card,\"choice\":0}")
            answer_code=$?
            if [ "$answer_code" -ne 0 ] && ancestry_refused "$answer"; then
                w2_skip "poll waiter" "op.answer: $ANCESTRY_SKIP"
                kill -TERM "$w2_agent_pid" 2>/dev/null
            else
                api_wait_pid "$w2_agent_pid" 20
                agent_exit=$?
                case "$answer:$agent_exit" in
                    *'"delivered":true'*'"via":"poll"'*:0)
                        w2_row "poll waiter" 0 "$bot_id card $bot_card → $answer, the script exited 0" ;;
                    *) w2_row "poll waiter" 1 "answer $answer, script exit $agent_exit (see $agent_log)" ;;
                esac
            fi
        fi
        w2_agent_pid=""
    fi

    # The host and wire halves of the answer→clear budget (§4.8). Both are
    # measured from a *device* `answer` input report, so they stay 0 for a
    # run whose answers all came over the API — the ENTER row is what makes
    # them non-zero. The contract's bound is asserted either way.
    local metrics metrics_code host_us wire_us
    metrics=$(w2_ctl --scopes admin metrics.get)
    metrics_code=$?
    host_us=$(printf '%s' "$metrics" | api_field answer_to_clear_host_us)
    wire_us=$(printf '%s' "$metrics" | api_field answer_to_clear_wire_us)
    if [ "$metrics_code" -ne 0 ] && ancestry_refused "$metrics"; then
        w2_skip "answer-clear" "metrics.get: $ANCESTRY_SKIP"
    elif [ -n "$host_us" ] && [ "$host_us" -lt 100000 ]; then
        w2_row "answer-clear" 0 "host_us=$host_us wire_us=${wire_us:-?} (0 = no device answer in this run; §7.5 needs a hand on the deck)"
    else
        w2_row "answer-clear" 1 "answer_to_clear_host_us=${host_us:-none} (want < 100000)"
    fi

    # §7.4 — the same poll card with nobody waiting: the store's 4 s budget
    # fails it, and a later `agent.wait` still picks the answer up.
    local late_log="$logs/hw-w2-late.log"
    w2_late_agent "$port" "$token" >"$late_log" 2>&1 &
    w2_agent_pid=$!
    local late
    late=$(w2_wait_for 'a["name"] == "hwlate"' 15)
    if [ -z "$late" ]; then
        w2_row "poll no waiter" 1 "no card from hwlate within 15 s (see $late_log)"
        kill -TERM "$w2_agent_pid" 2>/dev/null
        w2_agent_pid=""
    else
        local late_id=${late%% *} late_card=${late##* } failed failed_code status late_exit
        local started=$(date +%s)
        failed=$(w2_ctl --scopes operate op.answer \
            "{\"agent\":\"$late_id\",\"card\":$late_card,\"choice\":1}")
        failed_code=$?
        local took=$(( $(date +%s) - started ))
        if [ "$failed_code" -ne 0 ] && ancestry_refused "$failed"; then
            w2_skip "poll no waiter" "op.answer: $ANCESTRY_SKIP"
            w2_skip "late pickup" "the answer it picks up was refused by the §4.2 ancestry cap"
            kill -TERM "$w2_agent_pid" 2>/dev/null
            w2_agent_pid=""
        else
            # `agent.get` answers the agent object itself, card and all.
            status=$(w2_ctl --scopes observe agent.get "{\"agent\":\"$late_id\"}" | api_field card status)
            local timed_out=1
            case $failed in *'"delivered":false'*'"reason":"timeout"'*) timed_out=0 ;; esac
            if [ "$timed_out" -eq 0 ] && [ "$status" = failed ]; then
                w2_row "poll no waiter" 0 "$failed after ${took}s, agent.get card status=failed"
            else
                w2_row "poll no waiter" 1 "$failed after ${took}s, card status='${status:-none}' (want delivered:false/timeout and failed)"
            fi
            # The late pickup: the script's `agent.wait` returns the answer it
            # never received, and the deck gets its `card clear`.
            api_wait_pid "$w2_agent_pid" 40
            late_exit=$?
            w2_agent_pid=""
            if [ "$late_exit" -eq 0 ] && grep -q '"kind": *"answer"' "$late_log"; then
                w2_row "late pickup" 0 "$(grep -m1 '"kind"' "$late_log")"
            else
                w2_row "late pickup" 1 "exit $late_exit (see $late_log)"
            fi
        fi
    fi

    # §7.6 — the week-1 numbers with the writer thread in the loop.
    metrics=$(w2_ctl --scopes admin metrics.get)
    metrics_code=$?
    local drops errs stalls in_flight write_avg
    if [ "$metrics_code" -ne 0 ] && ancestry_refused "$metrics"; then
        w2_skip "writer metrics" "metrics.get: $ANCESTRY_SKIP"
    else
        drops=$(printf '%s' "$metrics" | api_field link_drops)
        stalls=$(printf '%s' "$metrics" | api_field write_stalls)
        in_flight=$(printf '%s' "$metrics" | api_field writes_in_flight_max)
        write_avg=$(printf '%s' "$metrics" | api_field write_avg_us)
        errs=$(printf '%s' "$metrics" | python3 -c '
import json, sys
m = json.load(sys.stdin)
print(m["err_size"] + m["err_parse"] + m["err_range"] + m["err_unknown"])
')
        local detail="write_avg_ms=$((${write_avg:-0} / 1000)) link_drops=$drops device_err=$errs write_stalls=$stalls writes_in_flight_max=$in_flight"
        if [ "${drops:-1}" -eq 0 ] && [ "${errs:-1}" -eq 0 ] && [ "${stalls:-1}" -eq 0 ] && [ "${in_flight:-9}" -le 1 ]; then
            w2_row "writer metrics" 0 "$detail"
        else
            w2_row "writer metrics" 1 "$detail (want link_drops=0 device_err=0 write_stalls=0 writes_in_flight_max<=1)"
        fi
    fi

    # SIGTERM with the writer thread in the loop: the daemon must exit 0
    # within seconds even if a BLE write is in flight (writer::CLOSE_BUDGET).
    local stop_started=$(date +%s) daemon_exit
    kill -TERM "$w2_pid" 2>/dev/null
    api_wait_pid "$w2_pid" 20
    daemon_exit=$?
    local stop_took=$(( $(date +%s) - stop_started ))
    w2_pid=""
    if [ "$daemon_exit" -eq 0 ] && [ "$stop_took" -le 10 ]; then
        w2_row "sigterm" 0 "exited 0 in ${stop_took}s"
    else
        w2_row "sigterm" 1 "exit $daemon_exit after ${stop_took}s (see $log)"
    fi
    if grep -qi panic "$log"; then
        w2_row "no panic" 1 "the log has a panic (see $log)"
        grep -i -m 3 panic "$log"
    else
        w2_row "no panic" 0 "$(wc -l <"$log" | tr -d ' ') log lines, no panic"
    fi
    w2_cleanup
}

# The §7.4 driver: register a poll agent and set a card, then stay *out* of
# `agent.wait` long enough for the store's 4 s delivery budget to fail it,
# and only then pick the answer up. `virtual_agent.py` parks immediately, so
# it cannot play this row.
w2_late_agent() {  # $1 port, $2 token
    python3 - "$1" "$2" <<'PY'
import json, sys, time, urllib.request

port, token = sys.argv[1], sys.argv[2]
URL = f"http://127.0.0.1:{port}/v1/rpc"
HEAD = {"Authorization": f"Bearer {token}", "Content-Type": "application/json"}
NAME = "hwlate"
_id = [0]


def call(method, params):
    _id[0] += 1
    params = dict(params, client={"name": NAME, "version": "1"})
    body = json.dumps({"jsonrpc": "2.0", "id": _id[0], "method": method, "params": params})
    req = urllib.request.Request(URL, body.encode(), HEAD, method="POST")
    with urllib.request.urlopen(req, timeout=90) as r:
        answer = json.load(r)
    if "error" in answer:
        raise SystemExit(f"{method}: {json.dumps(answer['error'])}")
    return answer["result"]


call("session.hello", {})
agent = call("agent.register", {"kind": "other", "name": NAME, "key": f"virtual:{NAME}",
             "lease_ttl_s": 120, "caps": {"answer": "poll", "order": "poll", "focus": False}})["agent"]
call("agent.card.set", {"agent": agent, "type": "q", "text": "Ship it?", "free_text": False,
     "deliver": "poll", "choices": [{"label": "Yes"}, {"label": "No"}],
     "origin": {"kind": "custom"}})
print(json.dumps({"agent": agent, "waiting": False}), flush=True)
# Nobody is waiting: the answer the gate sends now must fail at the budget.
time.sleep(12)
got = call("agent.wait", {"agent": agent, "timeout_s": 15})
print(json.dumps(got), flush=True)
call("agent.release", {"agent": agent, "reason": "done"})
sys.exit(0 if got.get("kind") in ("answer", "order") else 1)
PY
}

if $hardware; then
    deckd="$target_dir/$host/debug/deckd"
    hw_data="$logs/hw-data"
    seconds=${OWT_HW_SECONDS:-60}
    # Only the presence line matters: doctor exits 1 on any other finding,
    # which pipefail would turn into "not connected".
    doctor_out=$("$deckd" doctor --json 2>/dev/null || true)
    if ! grep -q '"OpenWalkieTalkie present' <<<"$doctor_out"; then
        printf 'FAIL  --hardware: the deck is not connected\n'
        failures=$((failures + 1))
    else
        rm -rf "$hw_data"
        mkdir -p "$hw_data"
        uid=$(id -u)
        old_label=ai.cohdl.openwalkietalkie.deckd
        old_plist="$HOME/Library/LaunchAgents/$old_label.plist"
        restore=false
        if launchctl print "gui/$uid/$old_label" >/dev/null 2>&1; then
            launchctl bootout "gui/$uid/$old_label" && restore=true
            printf 'info  stopped %s for the hardware checks\n' "$old_label"
            sleep 2
        fi
        if hw_run first "$seconds"; then
            first_sid=$hw_last_sid
            first_hello_rx=$hw_last_hello_rx
            expected=$((seconds / 5))
            if [ "$first_hello_rx" -lt $((expected - 2)) ]; then
                printf 'FAIL  hardware heartbeat: %s replies in %s s (expected about %s)\n' "$first_hello_rx" "$seconds" "$expected"
                failures=$((failures + 1))
            else
                printf 'ok    hardware heartbeat: %s hello replies in %s s\n' "$first_hello_rx" "$seconds"
            fi
            sleep 2
            if hw_run restart 25; then
                if [ "$hw_last_sid" != "$first_sid" ]; then
                    printf 'ok    hardware restart: new sid %s (was %s), resynced\n' "$hw_last_sid" "$first_sid"
                else
                    printf 'FAIL  hardware restart: sid did not change\n'
                    failures=$((failures + 1))
                fi
            else
                failures=$((failures + 1))
            fi
        else
            failures=$((failures + 1))
        fi
        # The week-2 §7 rows, on a short data dir of their own so the
        # week-1 status.json above stays readable.
        w2_data="${TMPDIR:-/tmp}"
        w2_data="${w2_data%/}/deckd-hw-w2-$$"
        rm -rf "$w2_data"
        if mkdir -p "$w2_data"; then
            trap 'w2_cleanup; rm -rf "$w2_data"; exit 130' INT TERM
            hw_week2 "$w2_data"
            trap - INT TERM
            failures=$((failures + w2_failures))
            rm -rf "$w2_data"
        else
            printf 'FAIL  hardware w2: cannot create %s\n' "$w2_data"
            failures=$((failures + 1))
        fi
        if $restore; then
            if launchctl bootstrap "gui/$uid" "$old_plist"; then
                printf 'info  restored %s\n' "$old_label"
            else
                printf 'FAIL  could not restore %s (launchctl bootstrap gui/%s %s)\n' "$old_label" "$uid" "$old_plist"
                failures=$((failures + 1))
            fi
        fi
    fi
fi

if [ "$failures" -ne 0 ]; then
    printf '%d step(s) failed\n' "$failures"
    exit 1
fi
printf 'all green (%d workspace tests)\n' "$total"
