#!/usr/bin/env bash
# The packaging gate (docs/host-service.md §10.1, HOST-CONTRACT-W7 §3): assert
# that OpenWalkieTalkie.app is the bundle §10.1 describes and, above all, that its
# two Mach-Os share ONE code identity.
#
#   tools/assert-bundle-identity.sh dist/macos-aarch64/OpenWalkieTalkie.app
#
# Why this is a gate and not a nicety: TCC keys a grant to the code's
# identifier and team. `Contents/MacOS/deckd` signed without
# `-i ai.cohdl.openwalkietalkie` gets the identifier `deckd`, so the daemon's
# Microphone / Speech Recognition / Automation grant is not the GUI's and the
# GUI's is not the daemon's; worse, hardened-runtime capture from `deckd` fails
# even once a grant exists. A packaging run that cannot assert the two match
# has not produced anything usable, however cleanly it built.
#
# Exit status:
#   0  distributable: Developer ID, one identifier, one team, the audio-input entitlement
#   3  ad-hoc: every structural check holds, but the build is NOT distributable
#   1  a real failure -- the rows below say which
#   2  bad usage
set -uo pipefail

case "${1:-}" in
    -h | --help) sed -n '2,20s/^# \{0,1\}//p' "$0"; exit 0 ;;
    '') echo "usage: $0 APP_BUNDLE" >&2; exit 2 ;;
esac
app=$1
[ -d "$app" ] || { echo "assert-bundle-identity: not a bundle: $app" >&2; exit 2; }

identifier=${OWT_BUNDLE_IDENTIFIER:-ai.cohdl.openwalkietalkie}
label=ai.cohdl.openwalkietalkie.deckd
gui="$app/Contents/MacOS/OpenWalkieTalkie"
daemon="$app/Contents/MacOS/deckd"
agent="$app/Contents/Library/LaunchAgents/$label.plist"
info="$app/Contents/Info.plist"
entitlements=(com.apple.security.device.audio-input)

failures=0
adhoc=false
row() { # status, label, detail -- only FAIL counts against the gate
    printf '%-4s  %-32s %s\n' "$1" "$2" "$3"
    [ "$1" != FAIL ] || failures=$((failures + 1))
}

# --- layout (§10.1) -------------------------------------------------------
# `deckd` lives in Contents/MacOS, NOT Contents/Resources: a helper tool in
# Resources is not sealed as code and Apple DTS puts command-line helpers in
# MacOS. Do not "tidy" this.
for path in "$gui" "$daemon" "$agent" "$info"; do
    if [ -f "$path" ]; then
        row ok "present" "${path#"$app/"}"
    else
        row FAIL "missing" "${path#"$app/"}"
    fi
done
if [ -e "$app/Contents/Resources/deckd" ]; then
    row FAIL "deckd in Resources" "a helper in Resources is not sealed as code (Apple DTS)"
fi
[ $failures -eq 0 ] || { printf '\nassert-bundle-identity: %d failed\n' "$failures"; exit 1; }

# --- Info.plist (§10.1) ---------------------------------------------------
plutil -lint "$info" >/dev/null 2>&1 &&
    row ok "plutil -lint" "Contents/Info.plist" ||
    row FAIL "plutil -lint" "Contents/Info.plist is not a valid plist"
plist_value() { plutil -extract "$1" raw -o - -- "$2" 2>/dev/null; }
bundle_id=$(plist_value CFBundleIdentifier "$info")
if [ "$bundle_id" = "$identifier" ]; then
    row ok "CFBundleIdentifier" "$bundle_id"
else
    row FAIL "CFBundleIdentifier" "$bundle_id != $identifier"
fi
for key in NSMicrophoneUsageDescription NSSpeechRecognitionUsageDescription \
           SUFeedURL SUPublicEDKey; do
    value=$(plist_value "$key" "$info")
    if [ -z "$value" ]; then
        row FAIL "$key" "missing or empty"
    elif [ "$key" = SUPublicEDKey ] && ! [[ "$value" =~ ^[A-Za-z0-9+/]{43}=$ ]]; then
        # Not an Ed25519 key, so Sparkle can verify nothing and this build
        # cannot self-update. That is a gap, not a broken bundle.
        row note "$key" "not a key ($value) -- updates disabled"
    else
        row ok "$key" "$(printf '%.48s' "$value")"
    fi
done
minimum=$(plist_value LSMinimumSystemVersion "$info")
# SMAppService, which registers the bundled agent, is macOS 13 and later.
[ "$minimum" = "13.0" ] &&
    row ok "LSMinimumSystemVersion" "$minimum" ||
    row FAIL "LSMinimumSystemVersion" "$minimum != 13.0"
uielement=$(plist_value LSUIElement "$info")
[ "$uielement" = "false" ] &&
    row ok "LSUIElement" "false" ||
    row FAIL "LSUIElement" "$uielement != false"

# --- the bundled LaunchAgent (§10.1) --------------------------------------
plutil -lint "$agent" >/dev/null 2>&1 &&
    row ok "plutil -lint" "${agent#"$app/"}" ||
    row FAIL "plutil -lint" "${agent#"$app/"} is not a valid plist"
program=$(plist_value BundleProgram "$agent")
[ "$program" = "Contents/MacOS/deckd" ] &&
    row ok "BundleProgram" "$program" ||
    row FAIL "BundleProgram" "$program != Contents/MacOS/deckd"
# `KeepAlive: true` would restart the daemon on EVERY exit, which makes both of
# deckd's exit codes meaningless (week 6 fixed tools/install-host.sh to this
# and the bundle's copy must agree).
keepalive=$(plutil -extract KeepAlive json -o - -- "$agent" 2>/dev/null)
[ "$keepalive" = '{"SuccessfulExit":false}' ] &&
    row ok "KeepAlive" "$keepalive" ||
    row FAIL "KeepAlive" "$keepalive != {\"SuccessfulExit\":false}"
for key in StandardOutPath StandardErrorPath; do
    # launchd does not expand `~` and this plist is per-bundle, not per-user;
    # `deckd run` opens its own log (§9).
    if plutil -extract "$key" raw -o - -- "$agent" >/dev/null 2>&1; then
        row FAIL "$key" "must not be set in a per-bundle agent"
    else
        row ok "no $key" "deckd opens its own log (§9)"
    fi
done

# --- signature (§10.1, the point of the week) -----------------------------
codesign --verify --deep --strict "$app" >/dev/null 2>&1 &&
    row ok "codesign --verify --deep" "$(basename "$app")" ||
    row FAIL "codesign --verify --deep" "$(basename "$app")"

field() { sed -n "s/^$2=//p" <<<"$1" | head -n 1; }
seen_identifier=()
seen_team=()
for mach_o in "$gui" "$daemon"; do
    name=${mach_o#"$app/Contents/MacOS/"}
    details=$(codesign -dv "$mach_o" 2>&1)
    ents=$(codesign -d --entitlements :- "$mach_o" 2>/dev/null |
        plutil -convert json -o - -- - 2>/dev/null)
    [ -n "$ents" ] || ents='{}'
    # `flags=` is the only place the hardened runtime and an ad-hoc signature
    # are reported; `Runtime Version=` is printed whatever the flags say, so
    # grepping the whole block for "runtime" would always pass.
    flags=$(sed -n 's/.*flags=\([^ ]*\).*/\1/p' <<<"$details" | head -n 1)
    this_identifier=$(field "$details" Identifier)
    this_team=$(field "$details" TeamIdentifier)
    seen_identifier+=("$this_identifier")
    seen_team+=("$this_team")
    # Reported, not judged: the SAME rows at the end are what assert.
    [ -n "$this_identifier" ] &&
        row info "$name Identifier" "$this_identifier" ||
        row FAIL "$name Identifier" "unsigned or unreadable"
    [ -n "$this_team" ] &&
        row info "$name TeamIdentifier" "$this_team" ||
        row FAIL "$name TeamIdentifier" "unsigned or unreadable"
    case "$flags" in
        *adhoc*) adhoc=true ;;
    esac
    # Hardened runtime: without it the entitlements below are not consulted at
    # all, and notarisation refuses the build.
    case "$flags" in
        *runtime*) row ok "$name hardened runtime" "flags=$flags" ;;
        *) row FAIL "$name hardened runtime" "--options runtime was not applied (flags=${flags:-none})" ;;
    esac
    for wanted in "${entitlements[@]}"; do
        if [ "$(jq -r --arg k "$wanted" '.[$k] // false' <<<"$ents")" = true ]; then
            row ok "$name entitlement" "$wanted"
        else
            row FAIL "$name entitlement" "$wanted missing"
        fi
    done
done

# The two assertions the week exists for.
if [ "${seen_identifier[0]}" = "${seen_identifier[1]}" ] &&
   [ "${seen_identifier[0]}" = "$identifier" ]; then
    row ok "SAME Identifier" "${seen_identifier[0]} (one TCC subject)"
else
    row FAIL "SAME Identifier" "${seen_identifier[0]} vs ${seen_identifier[1]} (want $identifier)"
fi
if [ "${seen_team[0]}" = "${seen_team[1]}" ]; then
    row ok "SAME TeamIdentifier" "${seen_team[0]}"
else
    row FAIL "SAME TeamIdentifier" "${seen_team[0]} vs ${seen_team[1]}"
fi

printf '\n'
if [ $failures -ne 0 ]; then
    printf 'assert-bundle-identity: FAILED (%d checks)\n' "$failures"
    exit 1
fi
if [ "$adhoc" = true ] || [ "${seen_team[0]}" = "not set" ]; then
    printf 'assert-bundle-identity: ad-hoc, not a distributable build\n'
    printf '  Every structural check above holds and the two Mach-Os do share\n'
    printf '  one identifier, but an ad-hoc signature has no team and no\n'
    printf '  Developer ID, so TCC will not carry a grant across a rebuild and\n'
    printf '  Gatekeeper will refuse the DMG on any other Mac. Set\n'
    printf '  MACOS_SIGN_IDENTITY to a Developer ID Application identity to\n'
    printf '  produce a distributable build.\n'
    exit 3
fi
printf 'assert-bundle-identity: ok -- one identifier (%s), one team (%s),\n' \
    "${seen_identifier[0]}" "${seen_team[0]}"
printf '  hardened runtime and the audio-input entitlement on BOTH Mach-Os.\n'
