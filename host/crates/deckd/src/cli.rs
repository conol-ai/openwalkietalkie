//! The `deckd` command line (design §3, contract §3.8): `run`, `status`,
//! `ctl`, `events`, `token`, `device`, `doctor`, `probe` (is a deck there,
//! for the first run, which has no daemon yet), `ble` (the voice link's
//! bench tools, which talk to the deck directly and need no daemon), and a
//! verb for every care function the companion app has (`care`, `firmware`,
//! `config`, `service`, `device get|set|faces|set-faces|set-macro`; they
//! live in `crate::verbs`). Every command that talks to the running daemon
//! (`ctl`, `events`, `status --live`, the device commands, the care verbs,
//! the `doctor` socket probe) goes through `deck-client` over the UDS door;
//! `token` edits `tokens.toml` directly.
use crate::config::{Config, Paths};
use crate::metrics::Status as LiveStatus;
use clap::{Parser, Subcommand};
use deck_client::{Client, ClientConfig, ClientError, ClientEvent, Transport};
use deck_model::{ApiError, Scope};
use deck_proto::{encode, HostLine, LineAssembler};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// `deckd ctl` waits this long for a reply unless `--timeout` says otherwise.
pub const CTL_TIMEOUT: Duration = Duration::from_secs(30);
/// The client name `ctl` announces unless `--client` says otherwise.
pub const CTL_CLIENT: &str = "deckd-ctl";
/// The client name `events` announces.
pub const EVENTS_CLIENT: &str = "deckd-events";
/// `doctor`'s probes give the daemon this long.
pub const DOCTOR_TIMEOUT: Duration = Duration::from_secs(3);
/// The synthesiser the system ships, as `deckd tts` names it.
const SYSTEM_SYNTH: &str = if cfg!(windows) { "Windows' own voice" } else { "Apple's synthesiser" };
/// The computer the service runs on, as `deckd tts` names it.
const THIS_COMPUTER: &str = if cfg!(windows) { "this PC" } else { "this Mac" };

/// Why a socket call did not return a result.
#[derive(Debug)]
pub enum CtlFailure {
    /// No daemon at the socket, or the connection broke.
    Unreachable(String),
    /// A JSON-RPC error object.
    Rpc(ApiError),
}

impl CtlFailure {
    /// A client error as `ctl` reports it: a refusal or an RPC error is
    /// the JSON-RPC error object, everything else is "unreachable".
    fn of(error: ClientError, socket: &Path) -> CtlFailure {
        match error {
            ClientError::Refused(e) => CtlFailure::Rpc(e),
            ClientError::Rpc {
                code,
                message,
                data,
            } => CtlFailure::Rpc(ApiError {
                code,
                message,
                data: data.unwrap_or(serde_json::Value::Null),
            }),
            ClientError::Io(e) => CtlFailure::Unreachable(format!(
                "cannot reach deckd at {}: {e} (is the service running?)",
                socket.display()
            )),
            ClientError::Disconnected => {
                CtlFailure::Unreachable("deckd closed the connection".into())
            }
            ClientError::Protocol(why) => CtlFailure::Unreachable(format!("bad reply: {why}")),
        }
    }
}

/// A current-thread tokio runtime for one CLI command.
fn runtime() -> Result<tokio::runtime::Runtime, CtlFailure> {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| CtlFailure::Unreachable(format!("no tokio runtime: {e}")))
}

/// `--scopes a,b` → scopes; unknown names are refused before connecting.
fn parse_scopes(names: &[String]) -> Result<Vec<Scope>, String> {
    crate::api::session::parse_scopes(names)
        .map_err(|e| e.data["why"].as_str().unwrap_or(&e.message).to_string())
}

/// A one-shot session's config (no reconnect, no subscriptions).
fn ctl_config(socket: &Path, client: &str, scopes: Vec<Scope>) -> ClientConfig {
    ClientConfig {
        transport: Transport::Uds(socket.to_path_buf()),
        client_name: client.into(),
        client_version: env!("CARGO_PKG_VERSION").into(),
        scopes,
        subscribe: Vec::new(),
        snapshot: false,
        reconnect: false,
        content: None,
    }
}

/// Connect, hello, one call, close — all within `timeout`.
fn one_call(
    socket: &Path,
    config: ClientConfig,
    timeout: Duration,
    method: &str,
    params: serde_json::Value,
) -> Result<serde_json::Value, CtlFailure> {
    let rt = runtime()?;
    // The timer is built inside the runtime: `tokio::time::timeout` needs
    // a reactor at construction, not only when polled.
    let outcome = rt.block_on(async {
        tokio::time::timeout(timeout, async {
            let client = Client::connect(config).await?;
            let result = client.call(method, params).await;
            client.close().await;
            result
        })
        .await
    });
    match outcome {
        Ok(result) => result.map_err(|e| CtlFailure::of(e, socket)),
        Err(_) => Err(CtlFailure::Unreachable(format!(
            "no reply from deckd within {:.1} s",
            timeout.as_secs_f64()
        ))),
    }
}

/// One call on a fresh session (`deckd device …`, `status --live`, the care
/// verbs).
pub(crate) fn uds_call(
    socket: &Path,
    scopes: &[&str],
    method: &str,
    params: serde_json::Value,
) -> Result<serde_json::Value, CtlFailure> {
    let names: Vec<String> = scopes.iter().map(|s| s.to_string()).collect();
    let scopes = parse_scopes(&names).map_err(CtlFailure::Unreachable)?;
    one_call(
        socket,
        ctl_config(socket, CTL_CLIENT, scopes),
        CTL_TIMEOUT,
        method,
        params,
    )
}

/// `GET /v1/info` on `127.0.0.1:<port>`, hand-written HTTP/1.1 over a
/// blocking `TcpStream` (no HTTP client crate: the same probe the TCP
/// door uses against a squatter, contract §3.3). The body is the JSON
/// object, or the reason it is not one.
pub fn info_probe(port: u16) -> Result<serde_json::Value, String> {
    use std::io::{Read, Write as _};
    let addr = std::net::SocketAddr::from(([127, 0, 0, 1], port));
    let mut stream = std::net::TcpStream::connect_timeout(&addr, DOCTOR_TIMEOUT)
        .map_err(|e| format!("cannot connect: {e}"))?;
    let _ = stream.set_read_timeout(Some(DOCTOR_TIMEOUT));
    let _ = stream.set_write_timeout(Some(DOCTOR_TIMEOUT));
    let request = format!(
        "GET /v1/info HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nAccept: application/json\r\nConnection: close\r\n\r\n"
    );
    stream
        .write_all(request.as_bytes())
        .map_err(|e| format!("cannot send the request: {e}"))?;
    // The reply is small (§4.1); anything larger is not ours anyway.
    let mut raw = Vec::new();
    stream
        .take(64 * 1024)
        .read_to_end(&mut raw)
        .map_err(|e| format!("no reply: {e}"))?;
    let text = String::from_utf8_lossy(&raw);
    let status = text.lines().next().unwrap_or_default().to_string();
    if !status.contains(" 200") {
        return Err(format!("answered {:?}", status.trim()));
    }
    let body = text
        .split_once("\r\n\r\n")
        .or_else(|| text.split_once("\n\n"))
        .map(|(_, body)| body)
        .ok_or_else(|| "no body".to_string())?;
    serde_json::from_str(body.trim()).map_err(|e| format!("body is not JSON: {e}"))
}

/// Is that `/v1/info` object this daemon's? `api:"1"`, our daemon version
/// and the same `uds` path (contract §3.3's squatter test).
pub fn info_is_ours(info: &serde_json::Value, socket: &Path) -> bool {
    info["api"] == "1"
        && info["daemon"] == crate::core::dispatch::VERSION
        && info["uds"].as_str() == socket.to_str()
}

/// Report a failure the way `ctl` does (contract §3.8) and give the exit code.
fn ctl_failure(failure: CtlFailure) -> i32 {
    report(failure, &mut std::io::stderr().lock())
}

/// [`ctl_failure`] onto `err`, which every care verb reports through too:
/// "deckd: …" for a service that did not answer (exit 2); the JSON-RPC
/// error as it came for one that refused, with a line saying why when the
/// refusal is one a person can act on — a guarded operation that wants a
/// trusted principal (exit 3), or a terminal that Claude Code or Codex
/// started, which the service gives no `admin` or `device` scope (exit 1).
pub(crate) fn report(failure: CtlFailure, err: &mut dyn std::io::Write) -> i32 {
    match failure {
        CtlFailure::Unreachable(why) => {
            let _ = writeln!(err, "deckd: {why}");
            EXIT_UNREACHABLE
        }
        CtlFailure::Rpc(error) => {
            let _ = writeln!(
                err,
                "{}",
                serde_json::to_string(&error).unwrap_or_else(|_| error.to_string())
            );
            if error.code == ApiError::FORBIDDEN && error.data["need"] == "trusted" {
                let _ = writeln!(err, "deckd: {NEEDS_TRUST_HINT}");
                EXIT_NEEDS_TRUST
            } else {
                if error.code == ApiError::FORBIDDEN && error.data["reason"] == "agent_ancestry" {
                    let _ = writeln!(err, "deckd: {ANCESTRY_HINT}");
                }
                EXIT_RPC_ERROR
            }
        }
    }
}

/// The LaunchAgent label `doctor` looks at (design D16). It lives in
/// `crate::launchd` with the rest of the launchd vocabulary and are named
/// here because `deckd doctor` and `deckd uninstall` are what use it.
pub use crate::launchd::DECKD_LABEL;
/// `status.json` older than this while the lock is held means a stuck daemon.
pub const STATUS_STALE_S: u64 = 120;
/// Exit codes of `ctl` (contract §3.8): a JSON-RPC error, the daemon
/// unreachable, a guarded operation refused for want of trust.
pub const EXIT_RPC_ERROR: i32 = 1;
pub const EXIT_UNREACHABLE: i32 = 2;
pub const EXIT_NEEDS_TRUST: i32 = 3;
/// What `ctl` says next to a `-32002 {need:"trusted"}`.
pub const NEEDS_TRUST_HINT: &str =
    "needs a trusted principal: answer from the deck or the OpenWalkieTalkie app";
/// What `ctl` and the care verbs say next to a `-32002
/// {reason:"agent_ancestry"}`: the macOS service gives a process that
/// Claude Code or Codex started only `observe`, `agent`, `voice` and
/// `hooks:ingest` (§4.2).
pub const ANCESTRY_HINT: &str =
    "refused because this terminal runs inside Claude Code or Codex; run it from a terminal of its \
     own";

#[derive(Parser, Debug)]
#[command(name = "deckd", version, about = "OpenWalkieTalkie host service")]
pub struct Cli {
    /// Data directory (default: $DECKD_DATA_DIR, else ~/Library/Application Support/OpenWalkieTalkie on a Mac, %LOCALAPPDATA%\OpenWalkieTalkie on Windows).
    #[arg(long, global = true)]
    pub data_dir: Option<PathBuf>,
    #[command(subcommand)]
    pub cmd: Cmd,
}

#[derive(Subcommand, Debug)]
pub enum Cmd {
    /// Stdio MCP bridge, launched by the Claude plugin.
    ClaudeChannel,
    /// Run the service (the LaunchAgent entry point).
    Run {
        /// Log to stderr instead of the log file.
        #[arg(long)]
        foreground: bool,
        /// Load the mockup roster so the deck can be exercised without hooks.
        #[arg(long)]
        demo: bool,
        /// Never open the deck (CI, the API gate).
        #[arg(long)]
        no_device: bool,
    },
    /// Show whether the service runs and what it persisted.
    Status {
        #[arg(long)]
        json: bool,
        /// Also ask the running daemon for `service.info` over the socket.
        #[arg(long)]
        live: bool,
    },
    /// Answer one hook event for a CLI (contract §4). Reads the payload on
    /// stdin (`codex-notify`: the last argument), resolves the CLI process
    /// and its terminal, calls `hooks.<cli>` over the socket, prints the
    /// JSON the CLI expects — and **always exits 0**, because for Claude
    /// and Codex a non-zero exit from a hook denies the tool call.
    Hook {
        /// `claude`, `codex`, `codex-notify`, `claude-statusline`, `gemini`.
        cli: String,
        /// `codex-notify`'s payload, or the `statusLine` command to chain
        /// after `--`.
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        rest: Vec<String>,
    },
    /// Call one API method over the socket; the result JSON goes to stdout.
    Ctl {
        /// Scopes to ask for (default: the daemon's UDS default, `observe`).
        #[arg(long, value_delimiter = ',')]
        scopes: Vec<String>,
        /// The client name in `session.hello` (default: deckd-ctl).
        #[arg(long)]
        client: Option<String>,
        /// Seconds to wait for the reply (default: 30).
        #[arg(long)]
        timeout: Option<f64>,
        /// `session.ping`, `agent.list`, `op.answer`, …
        method: String,
        /// The params as JSON (default: `{}`).
        params: Option<String>,
    },
    /// Print events as JSON lines until Ctrl-C.
    Events {
        /// Event kinds or globs (default: `*`).
        #[arg(long, value_delimiter = ',')]
        kinds: Vec<String>,
        /// Replay from this revision first.
        #[arg(long)]
        since: Option<u64>,
        /// Ask for `observe:content`.
        #[arg(long)]
        content: bool,
        /// Print the snapshot first.
        #[arg(long)]
        snapshot: bool,
    },
    /// Bearer tokens for the TCP door (`tokens.toml`; the daemon reloads it).
    Token {
        #[command(subcommand)]
        cmd: TokenCmd,
    },
    /// The synthesiser: the API key of the cloud model (`[voice.tts]`
    /// engine = "cloud", bring your own key), the local model's Python
    /// environment (`engine = "mlx"`), and what the daemon speaks with.
    Tts {
        #[command(subcommand)]
        cmd: TtsCmd,
    },
    /// The local recogniser (`[voice.stt] engine = "whisper"`): its model
    /// weights, downloaded only on this opt-in, and what the daemon
    /// recognises with.
    Stt {
        #[command(subcommand)]
        cmd: SttCmd,
    },
    /// The deck: its settings, its strip, and debugging.
    Device {
        #[command(subcommand)]
        cmd: DeviceCmd,
    },
    /// What the companion app does for the deck, from the command line: the
    /// setup checks, the footprint, the sources and their pause, the Usage
    /// face's sources, the tests, the permissions and the decks this Mac
    /// keeps a setup for. `--json` prints the service's reply as it came.
    Care {
        #[command(subcommand)]
        cmd: crate::verbs::care::CareCmd,
    },
    /// The deck's firmware: what the service knows, a check for a release,
    /// and an install by cable.
    Firmware {
        #[command(subcommand)]
        cmd: crate::verbs::firmware::FirmwareCmd,
    },
    /// The settings the running service manages (`config.get`, `config.set`,
    /// `config.reload`).
    Config {
        #[command(subcommand)]
        cmd: crate::verbs::config::ConfigCmd,
    },
    /// Restart or stop the running service.
    Service {
        #[command(subcommand)]
        cmd: crate::verbs::service::ServiceCmd,
    },
    /// The deck's BLE audio service, on the bench (`docs/deck-protocol.md`,
    /// "Voice"). Talks to the deck directly through CoreBluetooth; needs no
    /// daemon, and never pairs, unpairs or flashes anything.
    Ble {
        #[command(subcommand)]
        cmd: BleCmd,
    },
    /// Install, remove or inspect the CLI hook integrations (design §5.7).
    ///
    /// It writes the files itself rather than asking a running daemon to: the
    /// path a hook file embeds is `current_exe()`, and this binary is the one
    /// the bundle installs. No daemon is needed, and nothing outside the
    /// target files and `<data>/backups` is touched.
    Integrations {
        #[command(subcommand)]
        cmd: IntegrationsCmd,
    },
    /// Check the installation.
    Doctor {
        #[arg(long)]
        json: bool,
    },
    /// Whether a deck is plugged in or paired, and on which bus: the first
    /// run looks with it before any service runs. It enumerates HID and
    /// opens nothing. Exit 0 when a deck is there and 1 when none is; the
    /// JSON says the same either way.
    Probe {
        /// Print `{"present": bool, "instances": [{"bus": "usb" |
        /// "bluetooth" | "unknown", "id": label | null}]}`: `id` is the
        /// deck's label (the one `device.deck` names it by) when HID lists a
        /// serial number for that instance.
        #[arg(long)]
        json: bool,
    },
    /// Collect one file the operator can send (design §10.5).
    ///
    /// Config with the secret-shaped keys removed, `status.json`, `state.json`
    /// redacted per `[privacy]`, the three logs, `codesign -dv` for both
    /// Mach-Os, `launchctl print`, the integrations status and the versions —
    /// with what it redacted stated at the top. No token plaintext, no hook
    /// payload bodies, no transcripts.
    Diagnostics {
        /// Where to write it (default: ~/Desktop).
        #[arg(long)]
        out: Option<PathBuf>,
        /// Print the bundle on stdout instead of writing a file.
        #[arg(long)]
        stdout: bool,
    },
    /// Take deckd back out (design §10.5).
    ///
    /// Removes the LaunchAgent and the managed hook entries, restoring the
    /// CLIs' own files byte for byte from `<data>/backups`. Logs are always
    /// kept. Without `--purge` the data directory stays.
    Uninstall {
        /// Also delete the data directory, the caches and the GUI's prefs.
        #[arg(long)]
        purge: bool,
        /// Do not ask before purging (there is nothing to ask about without
        /// `--purge`, which changes nothing that cannot be re-installed).
        #[arg(long)]
        yes: bool,
        /// Print what it would do and change nothing.
        #[arg(long)]
        dry_run: bool,
    },
    /// Print `docs/deckd-api.md`, generated from this daemon's `/v1/schema`.
    ///
    /// The checked-in file is this output; `deckd`'s `api_doc` test compares
    /// them, so the document cannot drift from the schema.
    ApiDoc,
}

#[derive(Subcommand, Debug)]
pub enum TtsCmd {
    /// The cloud provider's API key, kept in the Keychain on a Mac and in
    /// `[voice.tts]` elsewhere: with one, the announcements are spoken by
    /// the provider's model; without, by Apple's synthesiser on this Mac.
    Key {
        #[command(subcommand)]
        cmd: KeyCmd,
    },
    /// Download the local voice (Kokoro, Chinese and English, 365 MB) and
    /// select it: the opt-in. With `--mlx`, instead build the sidecar's
    /// Python environment with `uv`, install mlx-audio and load that model
    /// once.
    Install {
        /// The Kokoro package (`kokoro-multi-lang-v1_1`, the default, or
        /// `kokoro-int8-multi-lang-v1_1`); with `--mlx`, the Hugging Face
        /// model id.
        #[arg(long)]
        model: Option<String>,
        /// `--mlx`: rebuild the environment even when it is there.
        #[arg(long)]
        force: bool,
        /// The Qwen3-TTS sidecar (`engine = "mlx"`) rather than the Kokoro
        /// voice.
        #[arg(long)]
        mlx: bool,
        /// Download only; leave `[voice.tts] engine` as it is.
        #[arg(long)]
        no_select: bool,
    },
    /// What is installed, whether the model is cached, and the engine the
    /// running daemon reports.
    Status,
    /// Delete the local voice's package; Apple's synthesiser speaks from
    /// the next utterance.
    Remove,
}

#[derive(Subcommand, Debug)]
pub enum SttCmd {
    /// Download the model's weights (1.6 GB for the default) into the
    /// caches directory and select the engine. Through the running daemon
    /// when there is one, else directly.
    Install {
        /// `large-v3-turbo` (default), `large-v3-turbo-q8_0`,
        /// `large-v3-turbo-q5_0`, `small`, `base.en`.
        #[arg(long)]
        model: Option<String>,
        /// Download only; leave `[voice.stt] engine` as it is.
        #[arg(long)]
        no_select: bool,
    },
    /// Which engine is selected, which model, whether it is on disk and
    /// loaded, and any download in flight.
    Status,
    /// Delete the selected model's weights; Apple's recogniser takes the
    /// next press.
    Remove,
}

#[derive(Subcommand, Debug)]
pub enum KeyCmd {
    /// Store the key where the service keeps it: the Keychain on a Mac
    /// (`ai.cohdl.openwalkietalkie`), config.toml elsewhere (`[voice.tts]
    /// api_key`), through the running service when there is one. It is read
    /// from the terminal without echo, or from stdin when piped; the running
    /// service applies it at once, no restart.
    Set {
        /// `openai` or `elevenlabs`; written to `[voice.tts] provider`.
        #[arg(long)]
        provider: Option<String>,
    },
    /// Remove the key: Apple's synthesiser speaks again.
    Clear,
}

#[derive(Subcommand, Debug)]
pub enum TokenCmd {
    /// Mint a token: the plaintext is printed once and never stored.
    Create {
        #[arg(long)]
        name: String,
        /// Comma-separated: observe, observe:content, agent, operate, voice, device, admin, …
        #[arg(long, value_delimiter = ',', required = true)]
        scopes: Vec<String>,
        /// `30d`, `12h`, `2026-12-31` or an RFC 3339 timestamp.
        #[arg(long)]
        expires: Option<String>,
        /// The client kind the token speaks for (`codex`, `claude`, …).
        #[arg(long)]
        client: Option<String>,
    },
    /// The tokens on file (names, scopes, dates; never hashes).
    List {
        #[arg(long)]
        json: bool,
    },
    /// Delete a token.
    Revoke { name: String },
    /// A new secret under the same name and scopes; the old one stops working.
    Rotate { name: String },
}

#[derive(Subcommand, Debug)]
pub enum IntegrationsCmd {
    /// What is installed where, per §4.11's vocabulary, with the limits of
    /// each answer spelled out.
    Status {
        #[arg(long)]
        json: bool,
        /// One of claude, codex, codex-desktop, gemini, claude-desktop.
        target: Option<String>,
    },
    /// Write deckd's hooks into a CLI's own config, backing up every file.
    Install {
        target: String,
        /// Codex only: record the hook hashes in `[hooks.state]` instead of
        /// printing the `/hooks` review step. This skips a review the user is
        /// meant to give, so it is never the default (C14).
        #[arg(long)]
        trust: bool,
        /// Print "What this writes" and change nothing.
        #[arg(long)]
        dry_run: bool,
    },
    /// Take deckd's entries back out, restoring what it displaced.
    Uninstall { target: String },
    /// Re-install with the current path, port and token.
    Repair {
        target: String,
        #[arg(long)]
        trust: bool,
        #[arg(long)]
        dry_run: bool,
    },
}

#[derive(Subcommand, Debug)]
pub enum BleCmd {
    /// Attach to the deck's audio service and exercise it. Without --tone:
    /// capture the microphone (MIC mode 1) and report rate, loss and level.
    /// With --tone: play a sine on the speaker. With --tone --loopback: play
    /// it while capturing (MIC mode 2) and check it comes back. Exit 0 pass,
    /// 1 fail, 2 no device within 20 s. Unlike the daemon's link, the probe
    /// scans and connects to a deck that is only advertising, so on an
    /// unpaired deck macOS shows its pairing request.
    Probe {
        /// Seconds to capture or play (1 to 60).
        #[arg(long, default_value_t = 5, value_parser = clap::value_parser!(u8).range(1..=60))]
        seconds: u8,
        /// Play a sine at this frequency, in Hz (100 to 7000).
        #[arg(long, value_parser = clap::value_parser!(u32).range(100..=7000))]
        tone: Option<u32>,
        /// With --tone: capture at the same time and look for the tone.
        #[arg(long, requires = "tone")]
        loopback: bool,
        /// With --tone: stream digital silence instead of the sine (the
        /// speaker path's own noise floor; with --loopback, as the deck's
        /// microphone hears it).
        #[arg(long, requires = "tone")]
        silence: bool,
        /// With --tone: the sine's peak level in dBFS (-60 to -6).
        #[arg(long, requires = "tone", allow_hyphen_values = true,
              default_value_t = crate::ble::probe::TONE_DBFS)]
        level: f64,
        /// Save the microphone capture as a 16 kHz mono WAV.
        #[arg(long)]
        wav: Option<std::path::PathBuf>,
        /// The deck's Bluetooth name, for a board that advertises another.
        /// It finds a deck the system has not seen offer the audio service;
        /// a connected deck that does offer it is used whatever its name.
        #[arg(long, default_value = crate::ble::DECK_NAME)]
        name: String,
    },
    /// Look without connecting: the radio, the connected peripherals with
    /// the audio or Battery service, and adverts named "OpenWalkieTalkie".
    /// Exit 0 when a deck is there, 2 when not.
    Scan {
        /// Seconds to listen for adverts (1 to 60).
        #[arg(long, default_value_t = 5, value_parser = clap::value_parser!(u8).range(1..=60))]
        seconds: u8,
        /// The deck's Bluetooth name, for a board that advertises another.
        #[arg(long, default_value = crate::ble::DECK_NAME)]
        name: String,
    },
}

/// `deckd ble …`. The link logs to stderr (`RUST_LOG`, default `warn`: the
/// probe says what matters on stdout itself).
fn ble_cmd(cmd: BleCmd) -> i32 {
    let level = std::env::var("RUST_LOG").unwrap_or_else(|_| "warn".into());
    // Foreground logging never touches the log directory.
    let _guard = crate::service::init_logging(&level, true, Path::new("."));
    match cmd {
        BleCmd::Probe {
            seconds,
            tone,
            loopback,
            silence,
            level,
            wav,
            name,
        } => crate::ble::probe::main(&crate::ble::probe::Options {
            seconds,
            tone,
            loopback,
            silence,
            level: level.clamp(-60.0, -6.0),
            wav,
            name,
            attach_timeout: crate::ble::probe::ATTACH_TIMEOUT,
        }),
        BleCmd::Scan { seconds, name } => crate::ble::probe::scan(
            Duration::from_secs(u64::from(seconds)),
            &name,
            &mut std::io::stdout().lock(),
        ),
    }
}

/// `deckd probe`: what HID lists of the deck, printed, and the exit code
/// that says the same: 0 when it lists one, 1 when it does not (O19; 2 is
/// clap's own usage error, which is what a `deckd` older than this verb
/// answers). A HID that cannot start lists nothing, with the reason on
/// stderr.
pub fn probe_cmd(
    found: Result<Vec<(hidapi::BusType, Option<String>)>, crate::hid::HidError>,
    json: bool,
    out: &mut impl std::io::Write,
) -> Result<i32, String> {
    let found = found.unwrap_or_else(|e| {
        eprintln!("deckd: {e}");
        Vec::new()
    });
    let written = if json {
        writeln!(out, "{}", probe_json(&found))
    } else if found.is_empty() {
        writeln!(out, "no OpenWalkieTalkie found")
    } else {
        let names: Vec<&str> = found.iter().map(|(bus, _)| bus_name(*bus)).collect();
        writeln!(out, "OpenWalkieTalkie present: {}", names.join(", "))
    };
    written.map_err(|e| e.to_string())?;
    Ok(i32::from(found.is_empty()))
}

/// `deckd probe --json`: `{"present": bool, "instances": [{"bus": …,
/// "id": …}]}`, one instance per entry HID lists of the deck's vendor
/// collection. `id` is the label of the deck its serial number names
/// (`device::identity`), never the number; null when HID lists none, which
/// is how a deck linked over Bluetooth only shows whether this Mac can tell
/// it from another one there.
pub fn probe_json(found: &[(hidapi::BusType, Option<String>)]) -> serde_json::Value {
    let instances: Vec<serde_json::Value> = found
        .iter()
        .map(|(bus, serial)| {
            let id = serial.as_deref().and_then(crate::device::identity::label_of);
            serde_json::json!({"bus": bus_name(*bus), "id": id})
        })
        .collect();
    serde_json::json!({"present": !found.is_empty(), "instances": instances})
}

/// A bus as `deckd probe` names it: `usb`, `bluetooth`, or `unknown` for
/// anything else hidapi may report.
pub fn bus_name(bus: hidapi::BusType) -> &'static str {
    match bus {
        hidapi::BusType::Usb => "usb",
        hidapi::BusType::Bluetooth => "bluetooth",
        _ => "unknown",
    }
}

#[derive(Subcommand, Debug)]
pub enum DeviceCmd {
    /// Queue N spinner title updates in the running service (one per pace tick).
    Flood { n: u64 },
    /// Make the running service write a full sync barrier now.
    Sync,
    /// Print the running service's link metrics.
    Metrics,
    /// Print the framed reports of the demo session, in deck-sim.py's layout.
    DryRun {
        /// Spinner title updates after the sync.
        #[arg(long, default_value_t = 0)]
        flood: usize,
        /// Extra host line(s), as JSON, after the sync.
        #[arg(long)]
        send: Vec<String>,
        /// Session id (default: random).
        #[arg(long)]
        sid: Option<u32>,
        /// Header clock HH:MM (default: now).
        #[arg(long)]
        hm: Option<String>,
    },
    // The deck's settings, strip and Macropad keys (`crate::verbs::device`):
    // `get`, `set`, `faces`, `set-faces`, `set-macro`.
    #[command(flatten)]
    Deck(crate::verbs::device::DeckCmd),
}

pub fn main() -> i32 {
    let cli = match Cli::try_parse() {
        Ok(cli) => cli,
        Err(e) => {
            // Help and the version are answers (exit 0, on stdout). Any
            // other command line clap could not take is refused, exit 1:
            // clap's own code for it, 2, is the one §10.7 gives a service
            // that did not answer, which a script would retry.
            let _ = e.print();
            return if e.exit_code() == 0 {
                0
            } else {
                EXIT_RPC_ERROR
            };
        }
    };
    match run(cli) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("deckd: {e}");
            1
        }
    }
}

fn paths(cli_dir: Option<PathBuf>) -> Result<Paths, String> {
    match cli_dir {
        Some(dir) => Ok(Paths::from_data_dir(
            dir.clone(),
            dir.join("logs"),
            dir.join("caches"),
        )),
        None => Paths::detect().map_err(|e| e.to_string()),
    }
}

fn run(cli: Cli) -> Result<i32, String> {
    let Cli { data_dir, cmd } = cli;
    // `hook` is answered before the data directory is resolved: a hook
    // that exits non-zero because `~/Library/Application Support` could
    // not be read would deny the user's tool call over a problem of ours
    // (contract §4.5). A missing directory is just a missing socket.
    if matches!(cmd, Cmd::Run { .. }) && data_dir.is_none() {
        crate::config::adopt_legacy_macos_dirs();
    }
    let cmd = match cmd {
        Cmd::Hook { cli, rest } => {
            let socket = paths(data_dir).ok().map(|p| p.socket);
            return Ok(crate::hook::main(&cli, &rest, socket));
        }
        // The bench tools talk to the deck, not to the data directory, and
        // the probe only looks for it.
        Cmd::Ble { cmd } => return Ok(ble_cmd(cmd)),
        Cmd::Probe { json } => {
            return probe_cmd(crate::hid::instance_buses(), json, &mut std::io::stdout().lock())
        }
        other => other,
    };
    let paths = paths(data_dir)?;
    match cmd {
        Cmd::ClaudeChannel => crate::claude_channel::main(&paths.data),
        // Answered above, where a missing data directory is a missing
        // socket rather than an error exit; kept so the match is total.
        Cmd::Hook { cli, rest } => Ok(crate::hook::main(&cli, &rest, Some(paths.socket))),
        Cmd::Ble { cmd } => Ok(ble_cmd(cmd)),
        Cmd::Probe { json } => probe_cmd(crate::hid::instance_buses(), json, &mut std::io::stdout().lock()),
        Cmd::Run {
            foreground,
            demo,
            no_device,
        } => {
            let config = Config::load(&paths.config).map_err(|e| e.to_string())?;
            let _guard = crate::service::init_logging(&config.log.level, foreground, &paths.logs);
            crate::core::run(crate::core::RunOptions {
                config,
                paths,
                demo,
                foreground,
                no_device,
            })
            .map_err(|e| e.to_string())
        }
        Cmd::Status { json, live } => {
            let status = status(&paths);
            let info = if live {
                Some(uds_call(
                    &paths.socket,
                    &[],
                    "service.info",
                    serde_json::json!({}),
                ))
            } else {
                None
            };
            if json {
                let mut value = serde_json::to_value(&status).map_err(|e| e.to_string())?;
                if let Some(Ok(info)) = &info {
                    value["info"] = info.clone();
                }
                println!(
                    "{}",
                    serde_json::to_string_pretty(&value).map_err(|e| e.to_string())?
                );
            } else {
                println!("{}", status_text(&status));
                if let Some(Ok(info)) = &info {
                    println!("{}", info_text(info));
                }
            }
            match info {
                Some(Err(failure)) => Ok(ctl_failure(failure)),
                _ => Ok(0),
            }
        }
        Cmd::Ctl {
            scopes,
            client,
            timeout,
            method,
            params,
        } => {
            let params: serde_json::Value = match params {
                Some(text) => {
                    serde_json::from_str(&text).map_err(|e| format!("params are not JSON: {e}"))?
                }
                None => serde_json::json!({}),
            };
            let timeout = timeout
                .filter(|s| *s > 0.0)
                .map(Duration::from_secs_f64)
                .unwrap_or(CTL_TIMEOUT);
            let scopes = parse_scopes(&scopes)?;
            let config = ctl_config(
                &paths.socket,
                client.as_deref().unwrap_or(CTL_CLIENT),
                scopes,
            );
            match one_call(&paths.socket, config, timeout, &method, params) {
                Ok(value) => {
                    println!("{value}");
                    Ok(0)
                }
                Err(failure) => Ok(ctl_failure(failure)),
            }
        }
        Cmd::Events {
            kinds,
            since,
            content,
            snapshot,
        } => {
            let mut out = std::io::stdout().lock();
            events_cmd(&paths, kinds, since, content, snapshot, &mut out)
        }
        Cmd::Token { cmd } => {
            let mut out = std::io::stdout().lock();
            token_cmd(&paths, cmd, &mut out, crate::core::wall_now())
        }
        Cmd::Tts { cmd } => {
            let mut out = std::io::stdout().lock();
            let keys = crate::secrets::home_for(&paths, false);
            tts_cmd(&paths, cmd, &keys, &mut out)
        }
        Cmd::Stt { cmd } => {
            let mut out = std::io::stdout().lock();
            stt_cmd(&paths, cmd, &mut out)
        }
        Cmd::Device { cmd } => match cmd {
            DeviceCmd::Flood { n } => {
                match uds_call(
                    &paths.socket,
                    &["admin"],
                    "device.flood",
                    serde_json::json!({"n": n}),
                ) {
                    Ok(r) => {
                        println!("flood queued: {} titles", r["queued"]);
                        Ok(0)
                    }
                    Err(failure) => Ok(ctl_failure(failure)),
                }
            }
            DeviceCmd::Sync => {
                match uds_call(
                    &paths.socket,
                    &["admin"],
                    "device.sync",
                    serde_json::json!({}),
                ) {
                    Ok(_) => {
                        println!("barrier requested");
                        Ok(0)
                    }
                    Err(failure) => Ok(ctl_failure(failure)),
                }
            }
            DeviceCmd::Metrics => {
                match uds_call(
                    &paths.socket,
                    &["admin"],
                    "metrics.get",
                    serde_json::json!({}),
                ) {
                    Ok(r) => {
                        let m: crate::metrics::Metric =
                            serde_json::from_value(r).map_err(|e| e.to_string())?;
                        println!("{}", m.summary());
                        Ok(0)
                    }
                    Err(failure) => Ok(ctl_failure(failure)),
                }
            }
            DeviceCmd::DryRun {
                flood,
                send,
                sid,
                hm,
            } => {
                let mut out = std::io::stdout().lock();
                dry_run(
                    &mut out,
                    sid.unwrap_or_else(crate::core::new_sid),
                    hm,
                    flood,
                    &send,
                )?;
                Ok(0)
            }
            DeviceCmd::Deck(cmd) => verb(&paths, cmd, crate::verbs::device::run),
        },
        Cmd::Care { cmd } => verb(&paths, cmd, crate::verbs::care::run),
        Cmd::Firmware { cmd } => verb(&paths, cmd, crate::verbs::firmware::run),
        Cmd::Config { cmd } => verb(&paths, cmd, crate::verbs::config::run),
        Cmd::Service { cmd } => verb(&paths, cmd, crate::verbs::service::run),
        Cmd::Doctor { json } => {
            let report = doctor(&paths);
            if json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&report).map_err(|e| e.to_string())?
                );
            } else {
                for (name, ok, detail) in &report.checks {
                    println!("{} {name}: {detail}", if *ok { "ok  " } else { "FAIL" });
                }
            }
            Ok(if report.checks.iter().all(|(_, ok, _)| *ok) {
                0
            } else {
                1
            })
        }
        Cmd::Diagnostics { out, stdout } => {
            let config = Config::load(&paths.config).unwrap_or_default();
            // The installer's environment is best effort: a machine where it
            // cannot even be constructed still deserves a bundle, and the
            // failure goes in the bundle rather than in place of it.
            let env = crate::integrations::Env::detect(&paths, &config).ok();
            let now = crate::core::wall_now();
            let bundle = crate::diagnostics::render(
                &paths,
                &config,
                env.as_ref(),
                &crate::diagnostics::Options::default(),
                now,
            );
            if stdout {
                println!("{}", bundle.text);
                return Ok(0);
            }
            let dir = match out {
                Some(dir) => dir,
                None => crate::diagnostics::default_dir()?,
            };
            let path = crate::diagnostics::write(&dir, &bundle, now)?;
            println!("wrote {}", path.display());
            println!("\nit redacted:");
            for line in &bundle.redacted {
                println!("  - {line}");
            }
            println!("\nread it before you send it: it is plain text.");
            Ok(0)
        }
        Cmd::Uninstall {
            purge,
            yes,
            dry_run,
        } => {
            let config = Config::load(&paths.config).unwrap_or_default();
            let env = crate::integrations::Env::detect(&paths, &config)?;
            let keys = crate::secrets::home_for(&paths, false);
            if dry_run {
                println!("{}", uninstall_preview(&env, &paths, purge, &keys));
                return Ok(0);
            }
            if purge && !yes && !confirm_purge(&paths)? {
                println!("nothing was changed");
                return Ok(0);
            }
            let outcome =
                crate::uninstall::run(&env, &paths, crate::uninstall::Options { purge }, &keys);
            print!("{}", outcome.text());
            Ok(i32::from(!outcome.problems.is_empty()))
        }
        Cmd::ApiDoc => {
            print!("{}", crate::apidoc::render(&crate::apidoc::schema()));
            Ok(0)
        }
        Cmd::Integrations { cmd } => {
            let mut out = std::io::stdout().lock();
            integrations_cmd(&paths, cmd, &mut out)
        }
    }
}

/// One of the care verbs (`crate::verbs`), against the service at this data
/// directory's socket, on this process's stdout and stderr.
fn verb<C>(paths: &Paths, cmd: C, run: crate::verbs::Run<C>) -> Result<i32, String> {
    let (mut out, mut err) = (std::io::stdout().lock(), std::io::stderr().lock());
    run(
        &crate::verbs::Socket::new(&paths.socket),
        cmd,
        &mut out,
        &mut err,
    )
}

/// "What this removes", before anything is removed — the same sentence the
/// installer's `--dry-run` gives, for the command that cannot be undone. The
/// Keychain is not asked whether it holds the cloud voice's key.
pub fn uninstall_preview(
    env: &crate::integrations::Env,
    paths: &Paths,
    purge: bool,
    keys: &crate::secrets::KeyHome,
) -> String {
    let mut out = String::from("deckd uninstall would:\n");
    for target in crate::uninstall::CLI_TARGETS {
        let reports = crate::integrations::status(env, Some(target));
        let status = reports
            .first()
            .map(|r| r.status.as_str())
            .unwrap_or("unknown");
        out.push_str(&format!(
            "  - {}: {status} → take our entries back out and restore what they displaced\n",
            target.display_name()
        ));
    }
    out.push_str(&format!("  - launchd: bootout {}\n", DECKD_LABEL));
    out.push_str(&format!("  - keep {}\n", paths.logs.display()));
    if purge {
        out.push_str(&format!(
            "  - PURGE {} and {}\n",
            paths.data.display(),
            paths.caches.display()
        ));
        if keys.store().is_some() {
            out.push_str(&format!(
                "  - PURGE the cloud voice key from {} ({}), if one is there\n",
                crate::secrets::STORE,
                crate::secrets::SERVICE
            ));
        }
    } else {
        out.push_str(&format!("  - keep {}\n", paths.data.display()));
    }
    out
}

/// `--purge` asks, because it is the one thing here that cannot be undone:
/// the tokens are hashes with no plaintext anywhere, and the deck's slot
/// memory is not recoverable from the CLIs' files. `--yes` is for scripts;
/// a non-interactive run without it refuses rather than guessing.
fn confirm_purge(paths: &Paths) -> Result<bool, String> {
    use std::io::Write as _;
    print!(
        "--purge deletes {} — settings, tokens and the deck's remembered slots. Type `purge` to \
         confirm: ",
        paths.data.display()
    );
    std::io::stdout().flush().map_err(|e| e.to_string())?;
    let mut line = String::new();
    match std::io::stdin().read_line(&mut line) {
        Ok(0) => Err("--purge needs a confirmation; pass --yes to skip the question".into()),
        Ok(_) => Ok(line.trim() == "purge"),
        Err(e) => Err(e.to_string()),
    }
}

/// `deckd integrations …`, written straight to the target files.
pub fn integrations_cmd(
    paths: &Paths,
    cmd: IntegrationsCmd,
    out: &mut dyn std::io::Write,
) -> Result<i32, String> {
    use crate::integrations::{self, Env, Target, Verb};
    let io = |e: std::io::Error| e.to_string();
    let config = Config::load(&paths.config).map_err(|e| e.to_string())?;
    let env = Env::detect(paths, &config)?;
    // On stderr, so `integrations status --json` stays machine-parseable.
    for warning in &env.warnings {
        eprintln!("warning: {warning}");
    }
    let (target, verb, dry_run) = match &cmd {
        IntegrationsCmd::Status { json, target } => {
            let target = target.as_deref().map(Target::parse).transpose()?;
            let reports = integrations::status(&env, target);
            if *json {
                writeln!(
                    out,
                    "{}",
                    serde_json::to_string_pretty(&reports).map_err(|e| e.to_string())?
                )
                .map_err(io)?;
            } else {
                for report in &reports {
                    writeln!(
                        out,
                        "{:<16} {:<18} {}",
                        report.target.name(),
                        report.status.as_str(),
                        report
                            .detected
                            .as_ref()
                            .map(|d| d.version.clone())
                            .unwrap_or_else(|| "-".into())
                    )
                    .map_err(io)?;
                    if let Some(detail) = &report.detail {
                        writeln!(out, "    {detail}").map_err(io)?;
                    }
                    for limit in &report.limits {
                        writeln!(out, "    note: {limit}").map_err(io)?;
                    }
                    if !report.backups.is_empty() {
                        writeln!(out, "    backups: {}", report.backups.len()).map_err(io)?;
                    }
                }
            }
            return Ok(0);
        }
        IntegrationsCmd::Install {
            target,
            trust,
            dry_run,
        } => (
            Target::parse(target)?,
            Verb::Install { trust: *trust },
            *dry_run,
        ),
        IntegrationsCmd::Uninstall { target } => (Target::parse(target)?, Verb::Uninstall, false),
        IntegrationsCmd::Repair {
            target,
            trust,
            dry_run,
        } => (
            Target::parse(target)?,
            Verb::Repair { trust: *trust },
            *dry_run,
        ),
    };
    let plan = integrations::plan(&env, target, verb)?;
    writeln!(out, "What this writes ({}):", target.display_name()).map_err(io)?;
    for change in plan.changes() {
        writeln!(
            out,
            "  {} {}",
            change["action"].as_str().unwrap_or("?"),
            change["path"].as_str().unwrap_or("?")
        )
        .map_err(io)?;
    }
    if plan.changes().is_empty() {
        writeln!(out, "  (nothing — already up to date)").map_err(io)?;
    }
    if !plan.commands.is_empty() {
        writeln!(out, "What this runs:").map_err(io)?;
        for command in &plan.commands {
            writeln!(out, "  $ {command}").map_err(io)?;
        }
    }
    for note in &plan.notes {
        writeln!(out, "  - {note}").map_err(io)?;
    }
    for conflict in &plan.conflicts {
        writeln!(out, "  ! {conflict}").map_err(io)?;
    }
    if dry_run {
        for step in &plan.next_steps {
            writeln!(out, "  next: {step}").map_err(io)?;
        }
        writeln!(out, "(dry run — nothing was written)").map_err(io)?;
        return Ok(0);
    }
    let receipt = integrations::apply(&env, target, verb, plan)?;
    for path in &receipt.changed {
        writeln!(out, "wrote {}", path.display()).map_err(io)?;
    }
    for path in &receipt.backups {
        writeln!(out, "backed up to {}", path.display()).map_err(io)?;
    }
    for step in &receipt.next_steps {
        writeln!(out, "next: {step}").map_err(io)?;
    }
    Ok(0)
}

/// `deckd events`: one JSON line per event on `out` until Ctrl-C (or
/// SIGTERM). `--kinds` are the subscription globs (default `*`), `--since`
/// replays the ring first, `--content` asks `observe:content`, `--snapshot`
/// prints `{"type":"snapshot",…}` first. Reconnects are automatic and show
/// as `{"type":"disconnected"}` / `{"type":"reconnected",…}` lines; a
/// `{"type":"resync","rev"}` line means "call `state.get`" (the ring had a
/// gap, or this reader lagged). Daemon unreachable at the start → exit 2.
pub fn events_cmd(
    paths: &Paths,
    kinds: Vec<String>,
    since: Option<u64>,
    content: bool,
    snapshot: bool,
    out: &mut dyn std::io::Write,
) -> Result<i32, String> {
    let config = ClientConfig {
        transport: Transport::Uds(paths.socket.clone()),
        client_name: EVENTS_CLIENT.into(),
        client_version: env!("CARGO_PKG_VERSION").into(),
        scopes: if content {
            vec![Scope::ObserveContent]
        } else {
            Vec::new()
        },
        subscribe: if kinds.is_empty() {
            vec!["*".into()]
        } else {
            kinds
        },
        snapshot,
        reconnect: true,
        content: None,
    };
    let rt = match runtime() {
        Ok(rt) => rt,
        Err(failure) => return Ok(ctl_failure(failure)),
    };
    let socket = paths.socket.clone();
    rt.block_on(async move {
        let (client, replay) =
            match tokio::time::timeout(CTL_TIMEOUT, Client::connect_since(config, since)).await {
                Ok(Ok(opened)) => opened,
                Ok(Err(e)) => return Ok(ctl_failure(CtlFailure::of(e, &socket))),
                Err(_) => {
                    return Ok(ctl_failure(CtlFailure::Unreachable(
                        "no hello reply from deckd".into(),
                    )))
                }
            };
        let mut rx = client.events();
        let hello = client.hello();
        let mut line = |value: serde_json::Value| -> bool { writeln!(out, "{value}").is_ok() };
        if snapshot
            && !line(serde_json::json!({
                "type": "snapshot",
                "rev": hello.rev,
                "session": hello.session,
                "snapshot": hello.snapshot,
            }))
        {
            return Ok(0);
        }
        if replay.gap && !line(serde_json::json!({"type": "resync", "rev": hello.rev})) {
            return Ok(0);
        }
        #[cfg(unix)]
        let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .map_err(|e| format!("cannot listen for SIGTERM: {e}"))?;
        // Windows has no SIGTERM: Ctrl-C (and Ctrl-Break, through the same
        // console handler) is the only stop.
        #[cfg(not(unix))]
        let mut term = Never;
        loop {
            let event = tokio::select! {
                _ = tokio::signal::ctrl_c() => break,
                _ = term.recv() => break,
                event = rx.recv() => event,
            };
            let written = match event {
                Ok(ClientEvent::Event(event)) => {
                    line(serde_json::to_value(&event).unwrap_or(serde_json::Value::Null))
                }
                Ok(ClientEvent::Resync { rev }) => {
                    line(serde_json::json!({"type": "resync", "rev": rev}))
                }
                Ok(ClientEvent::Reconnected {
                    hello,
                    replayed,
                    gap,
                }) => {
                    let ok = line(serde_json::json!({
                        "type": "reconnected",
                        "rev": hello.rev,
                        "session": hello.session,
                        "replayed": replayed,
                        "gap": gap,
                    }));
                    ok && (!gap || line(serde_json::json!({"type": "resync", "rev": hello.rev})))
                }
                Ok(ClientEvent::Disconnected) => line(serde_json::json!({"type": "disconnected"})),
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                    line(serde_json::json!({"type": "resync", "rev": client.last_rev()}))
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
            };
            if !written {
                break; // stdout closed (`| head`): done
            }
        }
        client.close().await;
        Ok(0)
    })
}

/// `deckd token …` against `tokens.toml` directly (no daemon needed; the
/// daemon reloads the file). Only a plaintext or the list goes to `out`.
/// `uv`, wherever the operator's shell would find it, and the two places
/// its installer puts it that a launchd-started daemon's PATH lacks.
fn find_uv() -> Option<PathBuf> {
    let on_path = std::env::var_os("PATH").and_then(|path| {
        std::env::split_paths(&path)
            .map(|dir| dir.join("uv"))
            .find(|p| p.is_file())
    });
    on_path.or_else(|| {
        let home = crate::integrations::home_dir().ok()?;
        [home.join(".local/bin/uv"), PathBuf::from("/opt/homebrew/bin/uv")]
            .into_iter()
            .find(|p| p.is_file())
    })
}

/// Run a tool with its output on the terminal, for the install's steps.
fn run_step(out: &mut dyn std::io::Write, what: &str, cmd: &mut std::process::Command) -> Result<(), String> {
    let _ = writeln!(out, "tts: {what}");
    let status = cmd
        .stdin(std::process::Stdio::null())
        .status()
        .map_err(|e| format!("{what}: {e}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("{what}: exit {}", status.code().unwrap_or(-1)))
    }
}

/// Where huggingface_hub keeps a model, for a status that does not load it.
fn hf_cache_dir(model: &str) -> Option<PathBuf> {
    let hub = std::env::var_os("HF_HUB_CACHE")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HF_HOME").map(|h| PathBuf::from(h).join("hub")))
        .or_else(|| crate::integrations::home_dir().ok().map(|h| h.join(".cache/huggingface/hub")))?;
    Some(hub.join(format!("models--{}", model.replace('/', "--"))))
}

/// The key, typed without echo on a terminal or read from a pipe.
fn read_key(prompt: &str) -> Result<String, String> {
    // SAFETY: `isatty` reads a descriptor's type; fd 0 is always valid here.
    let tty = unsafe { libc::isatty(0) } == 1;
    let mut line = String::new();
    if tty {
        eprint!("{prompt}");
        let _ = std::process::Command::new("stty").arg("-echo").status();
        let read = std::io::stdin().read_line(&mut line);
        let _ = std::process::Command::new("stty").arg("echo").status();
        eprintln!();
        read.map_err(|e| format!("stdin: {e}"))?;
    } else {
        std::io::stdin()
            .read_line(&mut line)
            .map_err(|e| format!("stdin: {e}"))?;
    }
    let key = line.trim().to_string();
    if key.is_empty() {
        return Err("no key given".into());
    }
    Ok(key)
}

/// Write `[voice.tts]` keys into config.toml as the operator (`config::edit`
/// keeps everything else byte for byte), validated whole before the write.
fn write_tts_keys(paths: &Paths, patch: &[(&str, serde_json::Value)]) -> Result<Config, String> {
    let text = match std::fs::read_to_string(&paths.config) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(format!("{}: {e}", paths.config.display())),
    };
    let mut edited = text;
    for (key, value) in patch {
        edited = crate::config::edit(&edited, key, value).map_err(|e| e.to_string())?;
    }
    let next = Config::parse(&edited).map_err(|e| format!("the result would not load: {e}"))?;
    if let Some(dir) = paths.config.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    crate::config::write(&paths.config, &edited).map_err(|e| e.to_string())?;
    Ok(next)
}

/// Why `deckd tts key` wrote nothing for a process an agent started: the
/// service refuses such a process its admin scope (the ancestry cap).
pub const KEY_ANCESTRY_HINT: &str =
    "a terminal that Claude Code or Codex started may not change the cloud voice key; nothing \
     was written. Run `deckd tts key` in a terminal of your own, or use the app's Voice page.";

/// The service refused a session the admin scope because an agent started
/// the process that asked.
fn ancestry_refused(failure: &CtlFailure) -> bool {
    matches!(failure, CtlFailure::Rpc(error)
        if error.code == ApiError::FORBIDDEN && error.data["reason"] == "agent_ancestry")
}

/// `config.get` from the running service, if it answers.
fn published_config(paths: &Paths) -> Option<serde_json::Value> {
    uds_call(&paths.socket, &["admin"], "config.get", serde_json::json!({})).ok()
}

/// Where `deckd tts key` says the running service keeps the key, from its
/// `config.get`.
fn key_place(paths: &Paths, published: &serde_json::Value) -> String {
    if published["voice"]["tts.key_store"] == "keychain" {
        crate::secrets::STORE.into()
    } else {
        paths.config.display().to_string()
    }
}

/// What `deckd tts key set` says after the key's line: whether the key is
/// used, and with what.
fn key_notes(
    out: &mut dyn std::io::Write,
    engine: &str,
    model: &str,
    provider: crate::audio::cloud::Provider,
) {
    if engine != crate::audio::cloud::NAME {
        let _ = writeln!(
            out,
            "tts: [voice.tts] engine is {engine:?}: the key is not used until it is \"cloud\" (the default)"
        );
    } else {
        let _ = writeln!(
            out,
            "tts: the daemon speaks with {} ({}) from the next announcement; `deckd tts status` to check",
            provider.name(),
            if model.is_empty() { provider.default_model() } else { model }
        );
    }
}

/// `deckd tts key set`: the key written where this install keeps it. The
/// running service writes it when there is one (`config.set`), so it is in
/// force at once and kept where the service keeps it. With no service it
/// goes straight into `keys`' store (the Keychain on a Mac's own install),
/// with the provider and no copy of a key left in config.toml, or into
/// config.toml. A process that Claude Code or Codex started is refused the
/// service's admin scope, and then nothing is written.
/// Whether something accepts a connection at `socket`: after a call there
/// failed short of an answer, whether a service is there at all. `deckd tts
/// key` writes the key itself only when none is: a service that took the
/// call and gave no answer (a Keychain dialog it waits on, a broken
/// connection, a timeout) holds a key of its own, and a key written around
/// it would leave the two apart until it restarts.
#[cfg(unix)]
fn service_listening(socket: &Path) -> bool {
    std::os::unix::net::UnixStream::connect(socket).is_ok()
}

/// [`service_listening`] on Windows: the pipe opens, or every instance of
/// it is busy, which is a service there just the same.
#[cfg(windows)]
fn service_listening(socket: &Path) -> bool {
    const ERROR_PIPE_BUSY: i32 = 231;
    match std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(socket)
    {
        Ok(_) => true,
        Err(e) => e.raw_os_error() == Some(ERROR_PIPE_BUSY),
    }
}

/// A service is at the socket and did not answer (`why`): nothing is
/// written, and the command exits as for a service that did not answer.
fn not_written(why: String) -> i32 {
    ctl_failure(CtlFailure::Unreachable(format!(
        "{why}; the service is running and did not answer, so nothing was written: run the \
         command again once it answers (`deckd status`)"
    )))
}

fn tts_key_set(
    paths: &Paths,
    provider: Option<crate::audio::cloud::Provider>,
    effective: crate::audio::cloud::Provider,
    key: &str,
    keys: &crate::secrets::KeyHome,
    out: &mut dyn std::io::Write,
) -> Result<i32, String> {
    let mut patch = serde_json::Map::new();
    patch.insert("voice.tts.api_key".into(), key.into());
    if let Some(provider) = provider {
        patch.insert("voice.tts.provider".into(), provider.name().into());
    }
    let asked = uds_call(
        &paths.socket,
        &["admin"],
        "config.set",
        serde_json::json!({ "patch": patch }),
    );
    match asked {
        Ok(_) => {
            let published = published_config(paths).unwrap_or_default();
            let voice = &published["voice"];
            let _ = writeln!(
                out,
                "tts: key {} stored in {} for {}",
                voice["tts.api_key"].as_str().unwrap_or_default(),
                key_place(paths, &published),
                effective.name()
            );
            key_notes(
                out,
                voice["tts.engine"].as_str().unwrap_or(crate::audio::cloud::NAME),
                voice["tts.model"].as_str().unwrap_or_default(),
                effective,
            );
            Ok(0)
        }
        Err(CtlFailure::Unreachable(why)) if service_listening(&paths.socket) => {
            Ok(not_written(why))
        }
        Err(CtlFailure::Unreachable(_)) => {
            let mut file = Vec::new();
            if let Some(provider) = provider {
                file.push((
                    "voice.tts.provider",
                    serde_json::Value::String(provider.name().into()),
                ));
            }
            let (next, place) = match keys.store() {
                Some(store) => {
                    store
                        .set(key)
                        .map_err(|why| format!("{} did not take the key: {why}", crate::secrets::STORE))?;
                    let config = Config::load(&paths.config).unwrap_or_default();
                    if config.voice.tts.has_key() {
                        file.push(("voice.tts.api_key", serde_json::Value::Null));
                    }
                    let next = if file.is_empty() { config } else { write_tts_keys(paths, &file)? };
                    (next, crate::secrets::STORE.to_string())
                }
                None => {
                    file.push(("voice.tts.api_key", serde_json::Value::String(key.into())));
                    (write_tts_keys(paths, &file)?, paths.config.display().to_string())
                }
            };
            let _ = writeln!(
                out,
                "tts: key {} stored in {place} for {}{}",
                crate::secrets::hint(key),
                effective.name(),
                if keys.store().is_some() { "; the service reads it when it starts" } else { "" }
            );
            key_notes(out, &next.voice.tts.engine, &next.voice.tts.model, effective);
            Ok(0)
        }
        Err(failure) if ancestry_refused(&failure) => Err(KEY_ANCESTRY_HINT.into()),
        Err(failure) => Ok(ctl_failure(failure)),
    }
}

/// `deckd tts key clear`: the key removed from where this install keeps it,
/// the three ways [`tts_key_set`] writes one.
fn tts_key_clear(
    paths: &Paths,
    keys: &crate::secrets::KeyHome,
    out: &mut dyn std::io::Write,
) -> Result<i32, String> {
    let asked = uds_call(
        &paths.socket,
        &["admin"],
        "config.set",
        serde_json::json!({ "patch": { "voice.tts.api_key": null } }),
    );
    match asked {
        Ok(_) => {
            let published = published_config(paths).unwrap_or_default();
            let voice = &published["voice"];
            let tts = crate::config::Tts {
                engine: voice["tts.engine"].as_str().unwrap_or_default().into(),
                provider: voice["tts.provider"].as_str().unwrap_or_default().into(),
                ..Default::default()
            };
            let _ = writeln!(
                out,
                "tts: key removed from {}; {} speaks",
                key_place(paths, &published),
                tts.effective_engine()
            );
            Ok(0)
        }
        Err(CtlFailure::Unreachable(why)) if service_listening(&paths.socket) => {
            Ok(not_written(why))
        }
        Err(CtlFailure::Unreachable(_)) => {
            let clear = [("voice.tts.api_key", serde_json::Value::Null)];
            let (next, place) = match keys.store() {
                Some(store) => {
                    store
                        .delete()
                        .map_err(|why| format!("{} did not remove the key: {why}", crate::secrets::STORE))?;
                    let config = Config::load(&paths.config).unwrap_or_default();
                    let next =
                        if config.voice.tts.has_key() { write_tts_keys(paths, &clear)? } else { config };
                    (next, crate::secrets::STORE.to_string())
                }
                None => (write_tts_keys(paths, &clear)?, paths.config.display().to_string()),
            };
            let _ = writeln!(
                out,
                "tts: key removed from {place}; {} speaks",
                next.voice.tts.effective_engine()
            );
            Ok(0)
        }
        Err(failure) if ancestry_refused(&failure) => Err(KEY_ANCESTRY_HINT.into()),
        Err(failure) => Ok(ctl_failure(failure)),
    }
}

/// The key as `deckd tts status` reports it: whether there is one, and its
/// line.
struct KeyLine {
    present: bool,
    line: String,
}

/// The key as `deckd tts status` finds it: the running service's
/// `config.get` when it answers (the hint of the key in force, and where it
/// is kept); else a key in config.toml; else whether the store holds one,
/// read from its attributes, never the secret.
fn key_line(paths: &Paths, config: &Config, keys: &crate::secrets::KeyHome) -> KeyLine {
    let none = || KeyLine {
        present: false,
        line: "(none) — `deckd tts key set --provider openai|elevenlabs`".into(),
    };
    if let Some(published) = published_config(paths) {
        let voice = &published["voice"];
        let hint = voice["tts.api_key"].as_str().unwrap_or_default();
        if hint.is_empty() {
            return none();
        }
        let line = if voice["tts.key_store"] == "keychain" {
            format!("{hint}, in {}", crate::secrets::STORE)
        } else {
            hint.to_string()
        };
        return KeyLine {
            present: true,
            line,
        };
    }
    let tts = &config.voice.tts;
    if tts.has_key() {
        return KeyLine {
            present: true,
            line: tts.api_key_hint(),
        };
    }
    match keys.store().map(|store| store.exists()) {
        Some(Ok(true)) => KeyLine {
            present: true,
            line: format!("in {} (start the service to see its last four characters)", crate::secrets::STORE),
        },
        Some(Err(why)) => KeyLine {
            present: false,
            line: format!("{} did not answer: {why}", crate::secrets::STORE),
        },
        Some(Ok(false)) | None => none(),
    }
}

/// `deckd tts key …` / `deckd tts install` / `deckd tts status`. `keys` is
/// where this install keeps the cloud voice's key
/// (`crate::secrets::home_for`).
pub fn tts_cmd(
    paths: &Paths,
    cmd: TtsCmd,
    keys: &crate::secrets::KeyHome,
    out: &mut dyn std::io::Write,
) -> Result<i32, String> {
    use crate::audio::{cloud, sidecar};
    let config = Config::load(&paths.config).unwrap_or_default();
    let cfg = crate::audio::sidecar_config(&config, paths);
    match cmd {
        TtsCmd::Key { cmd: KeyCmd::Set { provider } } => {
            let provider = match provider {
                Some(name) => Some(
                    cloud::Provider::parse(&name)
                        .ok_or_else(|| format!("{name:?} is not a provider this build knows: openai, elevenlabs"))?,
                ),
                None => None,
            };
            let effective = provider
                .or_else(|| cloud::Provider::parse(&config.voice.tts.provider))
                .ok_or_else(|| {
                    format!(
                        "[voice.tts] provider = {:?} is not one this build knows; say --provider openai or --provider elevenlabs",
                        config.voice.tts.provider
                    )
                })?;
            let key = read_key(&format!("{} API key: ", effective.name()))?;
            tts_key_set(paths, provider, effective, &key, keys, out)
        }
        TtsCmd::Key { cmd: KeyCmd::Clear } => tts_key_clear(paths, keys, out),
        TtsCmd::Install {
            model,
            force,
            mlx,
            no_select,
        } => {
            if !mlx && !model.as_deref().is_some_and(|m| m.contains('/')) {
                return tts_install_kokoro(paths, &config, model.as_deref(), no_select, out);
            }
            let model = model.unwrap_or_else(|| cfg.model.clone());
            let venv = paths.tts.join("venv");
            let managed = cfg.python.starts_with(&venv);
            if managed {
                let uv = find_uv().ok_or_else(|| {
                    "`uv` is not installed; it is one command: curl -LsSf https://astral.sh/uv/install.sh | sh".to_string()
                })?;
                if force && venv.exists() {
                    std::fs::remove_dir_all(&venv).map_err(|e| format!("{}: {e}", venv.display()))?;
                }
                std::fs::create_dir_all(&paths.tts).map_err(|e| format!("{}: {e}", paths.tts.display()))?;
                if !cfg.python.is_file() {
                    run_step(
                        out,
                        &format!("creating {} (Python 3.12)", venv.display()),
                        std::process::Command::new(&uv)
                            .arg("venv")
                            .arg(&venv)
                            .arg("--python")
                            .arg("3.12"),
                    )?;
                }
                run_step(
                    out,
                    "installing mlx-audio into it",
                    std::process::Command::new(&uv)
                        .arg("pip")
                        .arg("install")
                        .arg("--python")
                        .arg(&cfg.python)
                        .arg("mlx-audio>=0.5"),
                )?;
            } else if !cfg.python.is_file() {
                return Err(format!(
                    "[voice.tts] python = {} does not exist",
                    cfg.python.display()
                ));
            } else {
                let _ = writeln!(out, "tts: using {} from [voice.tts] python", cfg.python.display());
            }
            sidecar::write_script(&cfg.script)?;
            let _ = writeln!(out, "tts: loading {model} once (downloads it on first use)");
            let output = std::process::Command::new(&cfg.python)
                .arg(&cfg.script)
                .arg("--warm")
                .arg("--model")
                .arg(&model)
                .env("PYTHONUNBUFFERED", "1")
                .stdin(std::process::Stdio::null())
                .stderr(std::process::Stdio::inherit())
                .output()
                .map_err(|e| format!("{}: {e}", cfg.python.display()))?;
            let text = String::from_utf8_lossy(&output.stdout);
            let info: serde_json::Value = text
                .lines()
                .rev()
                .find_map(|l| serde_json::from_str(l.trim()).ok())
                .unwrap_or_default();
            if !output.status.success() || info.get("error").is_some() {
                return Err(format!(
                    "the model did not load: {}",
                    info["error"].as_str().unwrap_or("see the lines above")
                ));
            }
            let _ = writeln!(
                out,
                "tts: ready: {} at {} Hz, voices {}, loaded in {} ms",
                info["model"].as_str().unwrap_or(&model),
                info["rate"],
                info["voices"],
                info["load_ms"]
            );
            if config.voice.tts.engine != sidecar::NAME {
                let _ = writeln!(
                    out,
                    "tts: [voice.tts] engine is {:?}; set it to \"mlx\" to use this model",
                    config.voice.tts.engine
                );
            }
            if model != cfg.model {
                let _ = writeln!(out, "tts: set [voice.tts] model = {model:?} to make the daemon load this one");
            }
            let _ = writeln!(out, "tts: restart deckd to speak with it (the engine is built at start)");
            Ok(0)
        }
        TtsCmd::Remove => {
            match uds_call(&paths.socket, &["admin"], "tts.remove", serde_json::json!({})) {
                Ok(_) => {
                    let _ = writeln!(out, "tts: removed through the daemon");
                }
                Err(_) => {
                    let settings = crate::kokoro::Settings::from_config(&config);
                    crate::kokoro::job(&paths.kokoro, &settings.model).remove()?;
                    let _ = writeln!(out, "tts: removed {}", crate::kokoro::dir(&paths.kokoro, &settings.model).display());
                }
            }
            let _ = writeln!(out, "tts: {SYSTEM_SYNTH} speaks from the next utterance");
            Ok(0)
        }
        TtsCmd::Status => {
            let tts = &config.voice.tts;
            let kokoro_present = crate::kokoro::present(&paths.kokoro, &tts.model);
            // Only the cloud voice uses the key, and on a Mac it is not in
            // the file: where it is decides what speaks.
            let key = (tts.engine == cloud::NAME).then(|| key_line(paths, &config, keys));
            let _ = writeln!(
                out,
                "engine:  [voice.tts] engine = {:?} → {}",
                tts.engine,
                match tts.engine_with_key(key.as_ref().is_some_and(|key| key.present)) {
                    "apple" => format!("{SYSTEM_SYNTH} speaks, on {THIS_COMPUTER}"),
                    sidecar::NAME => "the local model in its sidecar speaks".to_string(),
                    crate::kokoro::ENGINE if kokoro_present => format!("the Kokoro voice speaks, on {THIS_COMPUTER}"),
                    crate::kokoro::ENGINE => format!("{SYSTEM_SYNTH}, until the Kokoro voice is downloaded"),
                    provider => format!("{provider}'s model speaks (your key)"),
                }
            );
            if tts.engine == crate::kokoro::ENGINE {
                let settings = crate::kokoro::Settings::from_config(&config);
                let job = crate::kokoro::job(&paths.kokoro, &settings.model);
                let _ = writeln!(
                    out,
                    "voice:   {} ({}), voices {}",
                    settings.model,
                    if kokoro_present { "downloaded" } else { "not downloaded: `deckd tts install`" },
                    settings.pool().join(", ")
                );
                let _ = writeln!(out, "package: {} ({:.0} MB)", job.path().display(), job.total as f64 / 1_048_576.0);
                match uds_call(&paths.socket, &["voice"], "tts.status", serde_json::json!({})) {
                    Ok(v) => {
                        let _ = writeln!(
                            out,
                            "daemon:  loaded {}, loading {}{}{}",
                            v["loaded"].as_bool().unwrap_or(false),
                            v["loading"].as_bool().unwrap_or(false),
                            v["load_error"].as_str().map_or(String::new(), |e| format!(", load error: {e}")),
                            v["download"]
                                .as_object()
                                .filter(|d| d["running"].as_bool().unwrap_or(false))
                                .map_or(String::new(), |d| format!(
                                    ", downloading {:.0} of {:.0} MB",
                                    d["bytes"].as_u64().unwrap_or(0) as f64 / 1_048_576.0,
                                    d["total"].as_u64().unwrap_or(0) as f64 / 1_048_576.0
                                ))
                        );
                    }
                    Err(CtlFailure::Unreachable(why)) => {
                        let _ = writeln!(out, "daemon:  {why}");
                    }
                    Err(CtlFailure::Rpc(error)) => {
                        let _ = writeln!(out, "daemon:  {} ({})", error.message, error.code);
                    }
                }
                return Ok(0);
            }
            if let Some(key) = key {
                let provider = cloud::Provider::parse(&tts.provider);
                let _ = writeln!(
                    out,
                    "provider: {}{}",
                    tts.provider,
                    provider.map_or_else(
                        || " (not one this build knows: openai, elevenlabs)".to_string(),
                        |p| format!(
                            ", model {}, voices {}",
                            if tts.model.is_empty() { p.default_model() } else { tts.model.as_str() },
                            cloud::Settings::from_config(&config).pool(p).join(", ")
                        )
                    )
                );
                let _ = writeln!(out, "key:     {}", key.line);
            }
            if tts.engine != sidecar::NAME {
                return status_daemon(paths, out);
            }
            let _ = writeln!(out, "model:   {}", cfg.model);
            let python = if cfg.python.is_file() {
                let version = std::process::Command::new(&cfg.python)
                    .arg("-c")
                    .arg("import sys, importlib.metadata as m; print(sys.version.split()[0], m.version('mlx-audio'))")
                    .output()
                    .ok()
                    .filter(|o| o.status.success())
                    .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string());
                match version {
                    Some(v) => format!("{} (Python {})", cfg.python.display(), v.replace(' ', ", mlx-audio ")),
                    None => format!("{} (mlx-audio is not installed there)", cfg.python.display()),
                }
            } else {
                format!("{} is missing: run `deckd tts install`", cfg.python.display())
            };
            let _ = writeln!(out, "python:  {python}");
            let cached = hf_cache_dir(&cfg.model).is_some_and(|d| d.join("snapshots").is_dir());
            let _ = writeln!(
                out,
                "cache:   {}",
                if cached { "the model is downloaded" } else { "the model is not downloaded yet (`deckd tts install`)" }
            );
            let _ = writeln!(out, "script:  {}", cfg.script.display());
            status_daemon(paths, out)
        }
    }
}

/// `deckd stt install|status|remove`.
pub fn stt_cmd(paths: &Paths, cmd: SttCmd, out: &mut dyn std::io::Write) -> Result<i32, String> {
    use crate::stt;
    let config = Config::load(&paths.config).unwrap_or_default();
    let settings = stt::Settings::from_config(&config);
    let mb = |b: u64| format!("{:.0} MB", b as f64 / 1_048_576.0);
    match cmd {
        SttCmd::Install { model, no_select } => {
            let name = stt::model_name(model.as_deref().unwrap_or(&settings.model)).to_string();
            let Some(entry) = stt::model(&name) else {
                return Err(format!(
                    "{name} is not a model this build can fetch: {}",
                    stt::MODELS.iter().map(|m| m.name).collect::<Vec<_>>().join(", ")
                ));
            };
            let _ = writeln!(out, "stt: {} — {} ({})", entry.name, entry.about, mb(entry.bytes));
            // Select first, so a daemon that is running loads the model the
            // moment it lands; the keys are hot.
            if !no_select {
                let mut patch = vec![("voice.stt.engine", serde_json::Value::String(stt::ENGINE.into()))];
                if name != stt::model_name(&config.voice.stt.model) {
                    patch.push(("voice.stt.model", serde_json::Value::String(name.clone())));
                }
                patch.push(("voice.stt.prompted", serde_json::Value::Bool(true)));
                write_tts_keys(paths, &patch)?;
                let _ = writeln!(out, "stt: [voice.stt] engine = \"whisper\", model = {name:?}");
            }
            if stt::present(&paths.stt, &name) {
                let _ = writeln!(out, "stt: {} is already downloaded", stt::model_path(&paths.stt, &name).display());
                return Ok(0);
            }
            // Through the daemon when it runs, so its own `stt.status`
            // shows the same download the app would; else here.
            let via_daemon = uds_call(
                &paths.socket,
                &["admin"],
                "stt.install",
                serde_json::json!({"model": name}),
            )
            .is_ok();
            let mut last = 0u64;
            let report = |out: &mut dyn std::io::Write, bytes: u64, total: u64, last: &mut u64| {
                progress_line(out, "stt", bytes, total, last);
            };
            if via_daemon {
                let _ = writeln!(out, "stt: downloading through the daemon");
                loop {
                    std::thread::sleep(std::time::Duration::from_millis(500));
                    let status = uds_call(&paths.socket, &["voice"], "stt.status", serde_json::json!({}))
                        .map_err(|e| match e {
                            CtlFailure::Unreachable(why) => why,
                            CtlFailure::Rpc(err) => err.message,
                        })?;
                    let d = &status["download"];
                    if d.is_null() {
                        return Err("the daemon reports no download".into());
                    }
                    let bytes = d["bytes"].as_u64().unwrap_or(0);
                    let total = d["total"].as_u64().unwrap_or(0);
                    report(out, bytes, total, &mut last);
                    if !d["running"].as_bool().unwrap_or(false) {
                        if let Some(why) = d["error"].as_str() {
                            return Err(format!("download failed: {why}"));
                        }
                        break;
                    }
                }
            } else {
                let _ = writeln!(out, "stt: the daemon is not running; downloading here");
                crate::download::fetch(&stt::job(&paths.stt, &name), |bytes, total| report(out, bytes, total, &mut last))?;
            }
            let _ = writeln!(out, "stt: {} is ready", stt::model_path(&paths.stt, &name).display());
            if !no_select {
                let _ = writeln!(out, "stt: the daemon recognises with it from the next press");
            }
            Ok(0)
        }
        SttCmd::Status => {
            let present = stt::present(&paths.stt, &settings.model);
            let _ = writeln!(
                out,
                "engine:  [voice.stt] engine = {:?} → {}",
                config.voice.stt.engine,
                if settings.enabled && present {
                    "the local model recognises"
                } else if settings.enabled {
                    "Apple's recogniser, until the model is downloaded"
                } else {
                    "Apple's on-device recogniser"
                }
            );
            let total = stt::model(&settings.model).map_or(0, |m| m.bytes);
            let _ = writeln!(
                out,
                "model:   {} ({}), language {}",
                settings.model,
                if present { "downloaded" } else { "not downloaded: `deckd stt install`" },
                if settings.language.is_empty() { "from the locale" } else { settings.language.as_str() }
            );
            let _ = writeln!(out, "file:    {} ({})", stt::model_path(&paths.stt, &settings.model).display(), mb(total));
            match uds_call(&paths.socket, &["voice"], "stt.status", serde_json::json!({})) {
                Ok(v) => {
                    let _ = writeln!(
                        out,
                        "daemon:  loaded {}, loading {}{}{}",
                        v["loaded"].as_bool().unwrap_or(false),
                        v["loading"].as_bool().unwrap_or(false),
                        v["load_error"].as_str().map_or(String::new(), |e| format!(", load error: {e}")),
                        v["download"]
                            .as_object()
                            .filter(|d| d["running"].as_bool().unwrap_or(false))
                            .map_or(String::new(), |d| format!(
                                ", downloading {} of {}",
                                mb(d["bytes"].as_u64().unwrap_or(0)),
                                mb(d["total"].as_u64().unwrap_or(0))
                            ))
                    );
                }
                Err(CtlFailure::Unreachable(why)) => {
                    let _ = writeln!(out, "daemon:  {why}");
                }
                Err(CtlFailure::Rpc(error)) => {
                    let _ = writeln!(out, "daemon:  {} ({})", error.message, error.code);
                }
            }
            Ok(0)
        }
        SttCmd::Remove => {
            match uds_call(&paths.socket, &["admin"], "stt.remove", serde_json::json!({})) {
                Ok(_) => {
                    let _ = writeln!(out, "stt: removed through the daemon");
                }
                Err(_) => {
                    stt::job(&paths.stt, &settings.model).remove()?;
                    let _ = writeln!(out, "stt: removed {}", stt::model_path(&paths.stt, &settings.model).display());
                }
            }
            let _ = writeln!(out, "stt: Apple's recogniser takes the next press");
            Ok(0)
        }
    }
}

/// One progress line per 16 MB, and at the end.
fn progress_line(out: &mut dyn std::io::Write, what: &str, bytes: u64, total: u64, last: &mut u64) {
    let mb = |b: u64| format!("{:.0} MB", b as f64 / 1_048_576.0);
    if bytes != *last && (bytes / (16 * 1_048_576) != *last / (16 * 1_048_576) || bytes == total) {
        *last = bytes;
        if total > 0 {
            let _ = writeln!(out, "{what}: {} of {} ({}%)", mb(bytes), mb(total), bytes * 100 / total);
        } else {
            let _ = writeln!(out, "{what}: {}", mb(bytes));
        }
    }
}

/// `deckd tts install` without `--mlx`: the Kokoro package, through the
/// daemon when it runs (so its `tts.status` shows the same download the
/// app would), else here.
fn tts_install_kokoro(
    paths: &Paths,
    config: &Config,
    model: Option<&str>,
    no_select: bool,
    out: &mut dyn std::io::Write,
) -> Result<i32, String> {
    use crate::kokoro;
    let settings = kokoro::Settings::from_config(config);
    let name = kokoro::model_name(model.unwrap_or(&settings.model)).to_string();
    let Some(entry) = kokoro::package(&name) else {
        return Err(format!(
            "{name} is not a voice package this build can fetch: {}",
            kokoro::PACKAGES.iter().map(|p| p.name).collect::<Vec<_>>().join(", ")
        ));
    };
    let _ = writeln!(out, "tts: {} — {} ({:.0} MB)", entry.name, entry.about, entry.bytes as f64 / 1_048_576.0);
    if !no_select {
        let mut patch = vec![("voice.tts.engine", serde_json::Value::String(kokoro::ENGINE.into()))];
        if name != kokoro::model_name(&config.voice.tts.model) || !config.voice.tts.model.starts_with("kokoro") {
            patch.push(("voice.tts.model", serde_json::Value::String(name.clone())));
        }
        patch.push(("voice.stt.prompted", serde_json::Value::Bool(true)));
        write_tts_keys(paths, &patch)?;
        let _ = writeln!(out, "tts: [voice.tts] engine = \"kokoro\", model = {name:?}");
    }
    let job = kokoro::job(&paths.kokoro, &name);
    if job.present() {
        let _ = writeln!(out, "tts: {} is already downloaded", job.path().display());
        return Ok(0);
    }
    let via_daemon = uds_call(
        &paths.socket,
        &["admin"],
        "tts.install",
        serde_json::json!({"model": name}),
    )
    .is_ok();
    let mut last = 0u64;
    if via_daemon {
        let _ = writeln!(out, "tts: downloading through the daemon");
        loop {
            std::thread::sleep(std::time::Duration::from_millis(500));
            let status = uds_call(&paths.socket, &["voice"], "tts.status", serde_json::json!({}))
                .map_err(|e| match e {
                    CtlFailure::Unreachable(why) => why,
                    CtlFailure::Rpc(err) => err.message,
                })?;
            let d = &status["download"];
            if d.is_null() {
                return Err("the daemon reports no download".into());
            }
            progress_line(out, "tts", d["bytes"].as_u64().unwrap_or(0), d["total"].as_u64().unwrap_or(0), &mut last);
            if !d["running"].as_bool().unwrap_or(false) {
                if let Some(why) = d["error"].as_str() {
                    return Err(format!("download failed: {why}"));
                }
                break;
            }
        }
    } else {
        let _ = writeln!(out, "tts: the daemon is not running; downloading here");
        crate::download::fetch(&job, |bytes, total| progress_line(out, "tts", bytes, total, &mut last))?;
    }
    let _ = writeln!(out, "tts: {} is ready", job.path().display());
    if !no_select {
        let _ = writeln!(out, "tts: the daemon speaks with it from the next announcement");
    }
    Ok(0)
}

/// The last line of `deckd tts status`: what the running daemon says.
fn status_daemon(paths: &Paths, out: &mut dyn std::io::Write) -> Result<i32, String> {
    match uds_call(&paths.socket, &["voice"], "voice.engines", serde_json::json!({})) {
        Ok(v) => {
            let engine = &v["tts"][0];
            let _ = writeln!(
                out,
                "daemon:  speaking with {} (on device: {}), selected {:?}",
                engine["id"].as_str().unwrap_or("?"),
                engine["on_device"].as_bool().unwrap_or(false),
                v["selected"]["tts"].as_str().unwrap_or("?")
            );
        }
        Err(CtlFailure::Unreachable(why)) => {
            let _ = writeln!(out, "daemon:  {why}");
        }
        Err(CtlFailure::Rpc(error)) => {
            let _ = writeln!(out, "daemon:  {} ({})", error.message, error.code);
        }
    }
    Ok(0)
}

pub fn token_cmd(
    paths: &Paths,
    cmd: TokenCmd,
    out: &mut dyn std::io::Write,
    now: deck_model::Ts,
) -> Result<i32, String> {
    use crate::tokens::{parse_expiry, parse_scopes, Tokens};
    let io = |e: std::io::Error| e.to_string();
    let mut tokens = Tokens::load(&paths.tokens).map_err(|e| e.to_string())?;
    match cmd {
        TokenCmd::Create {
            name,
            scopes,
            expires,
            client,
        } => {
            let scopes = parse_scopes(&scopes.join(",")).map_err(|e| e.to_string())?;
            let expires = expires
                .map(|s| parse_expiry(&s, now))
                .transpose()
                .map_err(|e| e.to_string())?;
            let plaintext = tokens
                .create(&name, scopes, expires, client, now)
                .map_err(|e| e.to_string())?;
            tokens.save(&paths.tokens).map_err(|e| e.to_string())?;
            writeln!(out, "{plaintext}").map_err(io)?;
            Ok(0)
        }
        TokenCmd::List { json } => {
            let rows: Vec<serde_json::Value> = tokens.list().iter().map(|t| t.public()).collect();
            if json {
                writeln!(
                    out,
                    "{}",
                    serde_json::to_string_pretty(&rows).map_err(|e| e.to_string())?
                )
                .map_err(io)?;
            } else {
                writeln!(
                    out,
                    "{:<16} {:<40} {:<24} {:<24} {:<8} EMBEDDED",
                    "NAME", "SCOPES", "CREATED", "EXPIRES", "CLIENT"
                )
                .map_err(io)?;
                for t in tokens.list() {
                    let scopes: Vec<String> = t
                        .scopes
                        .iter()
                        .filter_map(|s| serde_json::to_value(s).ok())
                        .filter_map(|v| v.as_str().map(String::from))
                        .collect();
                    writeln!(
                        out,
                        "{:<16} {:<40} {:<24} {:<24} {:<8} {}",
                        t.name,
                        scopes.join(","),
                        t.created,
                        t.expires.map_or("-".to_string(), |e| e.to_string()),
                        t.client.as_deref().unwrap_or("-"),
                        if t.embedded_in.is_empty() {
                            "-".to_string()
                        } else {
                            t.embedded_in.join(",")
                        },
                    )
                    .map_err(io)?;
                }
            }
            Ok(0)
        }
        TokenCmd::Revoke { name } => {
            tokens.revoke(&name).map_err(|e| e.to_string())?;
            tokens.save(&paths.tokens).map_err(|e| e.to_string())?;
            eprintln!("deckd: token {name:?} revoked");
            Ok(0)
        }
        TokenCmd::Rotate { name } => {
            let plaintext = tokens.rotate(&name, now).map_err(|e| e.to_string())?;
            tokens.save(&paths.tokens).map_err(|e| e.to_string())?;
            writeln!(out, "{plaintext}").map_err(io)?;
            Ok(0)
        }
    }
}

#[derive(Debug, serde::Serialize)]
pub struct Status {
    pub running: bool,
    pub data_dir: PathBuf,
    pub state_present: bool,
    pub faces: usize,
    pub card_ids: usize,
    pub port: Option<String>,
    /// `status.json` as the daemon last wrote it, and its age.
    pub live: Option<LiveStatus>,
    pub live_age_s: Option<u64>,
}

fn status(paths: &Paths) -> Status {
    let running = matches!(
        crate::service::lock(&paths.lock),
        Err(crate::service::ServiceError::AlreadyRunning)
    );
    let (state, loaded) = crate::state::State::load(&paths.state);
    let live = LiveStatus::read(&paths.status);
    let live_age_s = live.as_ref().map(|l| l.age_s(crate::core::wall_now()));
    Status {
        running,
        data_dir: paths.data.clone(),
        state_present: loaded == crate::state::Loaded::Read,
        faces: state.faces.len(),
        card_ids: state.card_ids.len(),
        port: std::fs::read_to_string(&paths.port)
            .ok()
            .map(|s| s.trim().to_string()),
        live,
        live_age_s,
    }
}

/// The `service.info` reply of `status --live`, one line per fact.
fn info_text(info: &serde_json::Value) -> String {
    format!(
        "live: api {} daemon {} on {}, uds {}, port {}, degraded {}, {} session(s), {} agent(s), rev {}",
        info["api"].as_str().unwrap_or("?"),
        info["daemon"].as_str().unwrap_or("?"),
        info["platform"].as_str().unwrap_or("?"),
        info["uds"].as_str().unwrap_or("-"),
        info["port"]
            .as_u64()
            .map_or("-".to_string(), |p| p.to_string()),
        info["degraded"].as_str().unwrap_or("none"),
        info["sessions"].as_u64().unwrap_or(0),
        info["agents"].as_u64().unwrap_or(0),
        info["rev"].as_u64().unwrap_or(0),
    )
}

fn status_text(s: &Status) -> String {
    let mut out = format!(
        "deckd: {}\ndata: {}\nstate.json: {}{}\nport: {}",
        match (&s.live, s.running) {
            (Some(l), true) => format!(
                "running (pid {}, up {} s{})",
                l.pid,
                l.uptime_s,
                if l.demo { ", demo" } else { "" }
            ),
            (_, true) => "running".into(),
            _ => "not running".into(),
        },
        s.data_dir.display(),
        if s.state_present { "present" } else { "absent" },
        if s.state_present {
            format!(" ({} faces, {} card counters)", s.faces, s.card_ids)
        } else {
            String::new()
        },
        s.port.as_deref().unwrap_or("-"),
    );
    match &s.live {
        Some(l) => {
            let m = &l.metric;
            out.push_str(&format!(
                "\nstatus.json: {} s ago{}\nlink: {:?}, session {:?}, sid {}, fw {}, boot {}, face {}, {} faces, {} agents\n{}\npace {} ms{}, outbox depth {}, reports tx {} rx {}, hello tx {} rx {} misses {}\nlink drops {}, barriers {}, device err {} (size {} parse {} range {} unknown {}), decode err {}, encode err {}\nflood {}/{}, ch2 reports {}, status polls {} replies {} timeouts {} errors {}",
                s.live_age_s.unwrap_or(0),
                if s.running { "" } else { " (daemon not running: last known)" },
                l.link,
                l.session,
                l.sid,
                l.fw.as_deref().unwrap_or("-"),
                l.boot.map_or("-".to_string(), |b| u8::from(b).to_string()),
                l.face.map_or("-".to_string(), |f| f.to_string()),
                l.faces,
                l.agents,
                deck_text(l),
                m.pace_ms,
                if m.backoff { " (backoff)" } else { "" },
                m.outbox_depth,
                m.reports_sent,
                m.reports_received,
                m.hello_sent,
                m.hello_replies,
                m.hello_misses_total,
                m.link_drops,
                m.barriers,
                m.err_total(),
                m.err_size,
                m.err_parse,
                m.err_range,
                m.err_unknown,
                m.decode_errors,
                m.encode_errors,
                m.flood_sent,
                m.flood_sent + m.flood_remaining,
                m.ch2_reports,
                m.status_polls,
                m.status_replies,
                m.status_timeouts,
                m.status_errors,
            ));
            if let Some(brief) = &l.deliveries {
                out.push('\n');
                out.push_str(&deliveries_text(brief));
            }
            if let Some(line) = collectors_text(l) {
                out.push('\n');
                out.push_str(&line);
            }
        }
        None => out.push_str("\nstatus.json: absent (the daemon has not run here)"),
    }
    out
}

/// The `cards since …` line of `deckd status`: the delivery record in brief
/// (`crate::deliveries`), counts only. `deckd ctl --scopes admin
/// metrics.get` has the whole record.
fn deliveries_text(b: &crate::deliveries::Brief) -> String {
    let r = &b.reader;
    format!(
        "cards since {}: {} from the sources, answered {} (on the deck {}), failure cards {}; \
         reader: channels {} (by PTT {}), history {}, plans {}, tool rows {}, images {}, \
         pickers {}",
        b.since.map_or("-".to_string(), |since| since.to_string()),
        b.cards,
        b.answered,
        b.answers_on_deck,
        b.failure_cards,
        r.channels,
        r.channels_by_ptt,
        r.history,
        r.plans,
        r.tool_rows,
        r.images,
        r.pickers,
    )
}

/// The `collectors:` line of `deckd status` (`crate::dormancy`): whether the
/// daemon collects from agent sessions, and since when it stopped, on this
/// Mac's clock. `None` for a daemon with no dormancy (`--no-device`).
fn collectors_text(l: &LiveStatus) -> Option<String> {
    Some(match (l.collecting?, l.dormant_since) {
        (true, _) => "collectors: on".to_string(),
        (false, Some(since)) => {
            let at = std::time::UNIX_EPOCH + std::time::Duration::from_millis(since.millis());
            let hm = crate::device::session::local_hm(at).unwrap_or_else(|| since.to_string());
            format!("collectors: off since {hm} (no deck linked for ten minutes)")
        }
        (false, None) => "collectors: off (no deck has been linked)".to_string(),
    })
}

/// The `deck:` line of `deckd status`: what the linked deck says about
/// itself on channel 2 and the bus it is on, `-` for what is not known.
fn deck_text(l: &LiveStatus) -> String {
    let transport = match l.transport {
        Some(deck_model::Transport::Usb) => "usb",
        Some(deck_model::Transport::Bluetooth) => "bluetooth",
        None => "-",
    };
    let charging = match l.charging {
        Some(true) => "yes",
        Some(false) => "no",
        None => "-",
    };
    format!(
        "deck: transport {transport}, battery {}, charging {charging}, board {}",
        l.battery
            .map_or("-".to_string(), |percent| format!("{percent}%")),
        l.board.as_deref().unwrap_or("-"),
    )
}

#[derive(Debug, serde::Serialize)]
pub struct Doctor {
    pub checks: Vec<(String, bool, String)>,
}

/// A stray `~/Library/LaunchAgents/<label>.plist`: what `deckd install
/// --dev` writes, and what an SMAppService registration leaves behind only
/// as a file when the bundle is gone.
#[cfg(not(windows))]
fn user_launch_agent(label: &str) -> Option<PathBuf> {
    let path = std::env::var_os("HOME")
        .map(PathBuf::from)?
        .join("Library/LaunchAgents")
        .join(format!("{label}.plist"));
    path.exists().then_some(path)
}

/// The launchd checks, from the one place the facts exist (§10.5): our
/// agent's state, **what its last exit meant** (75 / 0 / anything else, read
/// against the `KeepAlive` semaphore), whether the program it points at is
/// still there.
///
/// The exit and dangling rows are `crate::launchd`'s: they are text and
/// filesystem, so they are unit-tested there, and this only decides which
/// rows to print and whether each is a failure.
/// Windows' row in place of launchd's: whether the service starts at
/// sign-in, which the app sets as a Run value (`login_item`). Reading only.
#[cfg(windows)]
pub fn launchd_checks() -> Vec<(String, bool, String)> {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let reg = std::env::var_os("SystemRoot")
        .map(PathBuf::from)
        .unwrap_or_else(|| "C:\\Windows".into())
        .join(r"System32\reg.exe");
    let query = std::process::Command::new(reg)
        .args([
            "query",
            r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run",
            "/v",
            "OpenWalkieTalkie Service",
        ])
        .creation_flags(CREATE_NO_WINDOW)
        .output();
    let registered = query.ok().filter(|out| out.status.success()).and_then(|out| {
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .find_map(|line| line.split_once("REG_SZ").map(|(_, command)| command.trim().to_string()))
    });
    vec![(
        "startup".into(),
        true,
        match registered {
            Some(command) => format!("the service starts at sign-in: {command}"),
            None => "the service does not start at sign-in (the app's Service page sets it)".into(),
        },
    )]
}

#[cfg(not(windows))]
pub fn launchd_checks() -> Vec<(String, bool, String)> {
    let mut checks = Vec::new();
    match crate::launchd::Agent::load(DECKD_LABEL) {
        None => {
            // Not in the domain — but a plist may still be sitting in
            // `~/Library/LaunchAgents` pointing at a binary that is gone,
            // which is the half of the dangling state `launchctl print`
            // cannot see. This is the check that has to work from **any**
            // copy of the binary (§10.5): a fresh download diagnosing the
            // registration the trashed copy left.
            let stray = user_launch_agent(DECKD_LABEL);
            match stray.as_deref().map(crate::launchd::program_of_plist) {
                None => checks.push((
                    "launchd deckd".into(),
                    true,
                    format!("{DECKD_LABEL} not installed (tools/install-host.sh installs it)"),
                )),
                Some(Ok(program)) => {
                    let plist = stray.unwrap_or_default();
                    match crate::launchd::program_dangling(&program) {
                        None => checks.push((
                            "launchd deckd".into(),
                            true,
                            format!(
                                "not loaded, but {} is on disk and names {} — `{}` would \
                                 start it",
                                plist.display(),
                                program.display(),
                                crate::integrations::command_line([
                                    "launchctl",
                                    "bootstrap",
                                    &format!("gui/{}", crate::launchd::uid()),
                                    &plist.display().to_string(),
                                ])
                            ),
                        )),
                        Some(why) => checks.push((
                            "launchd deckd".into(),
                            false,
                            format!(
                                "{} points at {}: {why}. Remove the plist or re-install.",
                                plist.display(),
                                program.display()
                            ),
                        )),
                    }
                }
                Some(Err(why)) => checks.push(("launchd deckd".into(), false, why)),
            }
        }
        Some(agent) => {
            // A deliberate stop is the one way "not running" is not a
            // failure: `service.stop` exits 0, `KeepAlive = {SuccessfulExit:
            // false}` leaves it stopped, and that is the tray's own "Stop
            // service" working. Saying FAIL there would train the operator
            // to ignore this row.
            let stopped_on_purpose = agent.last_exit == crate::launchd::LastExit::Code(0)
                && agent.keepalive_on_failure_only;
            let ok = agent.state == "running" || agent.pid.is_some() || stopped_on_purpose;
            checks.push((
                "launchd deckd".into(),
                ok,
                format!(
                    "{}{}{}{}",
                    agent.state,
                    agent.pid.map(|p| format!(" (pid {p})")).unwrap_or_default(),
                    agent
                        .runs
                        .map(|r| format!(", {r} run(s)"))
                        .unwrap_or_default(),
                    if stopped_on_purpose && agent.pid.is_none() {
                        " — stopped on purpose, see the next row"
                    } else {
                        ""
                    }
                ),
            ));
            // A restart request and a deliberate stop are both healthy; a
            // crash is not, and neither is a plist whose KeepAlive makes the
            // codes meaningless.
            let healthy = matches!(
                agent.last_exit,
                crate::launchd::LastExit::Never | crate::launchd::LastExit::Code(75)
            ) || stopped_on_purpose;
            checks.push((
                "launchd deckd last exit".into(),
                healthy,
                agent.exit_meaning(),
            ));
            checks.push(match agent.dangling() {
                None => (
                    "launchd deckd program".into(),
                    true,
                    agent
                        .program
                        .as_ref()
                        .map(|p| p.display().to_string())
                        .unwrap_or_else(|| "launchctl named no program".into()),
                ),
                Some(why) => ("launchd deckd program".into(), false, why),
            });
            // Several copies of the app on one Mac (an old download, a test
            // build) can each register the daemon, and launchd runs whichever
            // did last — an old deckd behind a new app. Only a bundled copy
            // can say so: a dev binary is expected to differ.
            if let (Some(program), Ok(this)) = (&agent.program, std::env::current_exe()) {
                let same = |a: &std::path::Path, b: &std::path::Path| {
                    a.canonicalize().ok().zip(b.canonicalize().ok()).is_some_and(|(a, b)| a == b)
                };
                let bundled = crate::launchd::app_bundle_of(&this).is_some();
                if bundled && !same(program, &this) {
                    checks.push((
                        "launchd deckd copy".into(),
                        false,
                        format!(
                            "launchd runs {}, not this copy ({}). Quit every OpenWalkieTalkie, \
                             then open the one you mean to use: it registers its own daemon.",
                            program.display(),
                            this.display()
                        ),
                    ));
                }
            }
        }
    }
    checks
}

/// The doctor's `tcp` row: the port file, and whether what answers on that
/// port is this daemon. An empty port file while the service runs is a
/// degraded door, unless the door is off — by default, or because `[api]
/// tcp = false` says so — when it is the point. `door` says how the door is
/// decided (`config::Api::tcp_door`; `None` for a `config.toml` that does
/// not load), and is asked only when the port file names no port.
fn tcp_check(
    paths: &Paths,
    running: bool,
    door: impl FnOnce() -> Option<crate::config::TcpDoor>,
) -> Result<String, String> {
    let port = std::fs::read_to_string(&paths.port)
        .ok()
        .and_then(|s| s.trim().parse::<u16>().ok());
    match port {
        None => match door().filter(|door| !door.on) {
            Some(off) => Ok(format!(
                "off ({}): the socket is the only door",
                off.reason()
            )),
            None if !running => Ok("no port file (the daemon is not running)".into()),
            None => Err(
                "port file empty while the service runs: the tcp door is degraded (port in use?)"
                    .into(),
            ),
        },
        Some(port) => match info_probe(port) {
            Ok(info) if info_is_ours(&info, &paths.socket) => {
                Ok(format!("127.0.0.1:{port}/v1/info answers and is ours"))
            }
            Ok(_) => Err(format!(
                "127.0.0.1:{port}/v1/info answers but is not this daemon's (hooks_hijack_risk)"
            )),
            Err(why) if !running => Ok(format!("stale port {port} ({why})")),
            Err(why) => Err(format!("127.0.0.1:{port}: {why}")),
        },
    }
}

fn doctor(paths: &Paths) -> Doctor {
    let mut checks = setup_checks(paths, || file_door(paths));
    checks.push(device_check());
    checks.extend(launchd_checks());
    Doctor { checks }
}

/// The TCP door as `deckd run` would decide it from this data dir's
/// `config.toml` and this process's environment (`config::Api::tcp_door`):
/// what the doctor, a process of its own, has to go on. `None` for a file
/// that does not load.
fn file_door(paths: &Paths) -> Option<crate::config::TcpDoor> {
    let config = Config::load(&paths.config).ok()?;
    Some(
        config
            .api
            .tcp_door(|| crate::integrations::legacy_claude_hooks(paths, &config)),
    )
}

/// The doctor's rows about the data directory and the two doors: `data
/// dir`, `config.toml`, `state.json`, `service`, `status.json`, `socket`,
/// `tcp` and `tokens.toml`. Nothing here looks for the deck or asks
/// launchd; `care.checks` answers with these, then the deck's row by its
/// own rule, then [`launchd_checks`]. `door` is how the TCP door is decided
/// (the doctor works it out, the service knows it), asked only by the `tcp`
/// row and only when the port file names no port.
pub fn setup_checks(
    paths: &Paths,
    door: impl FnOnce() -> Option<crate::config::TcpDoor>,
) -> Vec<(String, bool, String)> {
    let mut checks = Vec::new();
    let mut check = |name: &str, result: Result<String, String>| match result {
        Ok(detail) => checks.push((name.to_string(), true, detail)),
        Err(detail) => checks.push((name.to_string(), false, detail)),
    };
    check(
        "data dir",
        if paths.data.is_dir() {
            Ok(paths.data.display().to_string())
        } else {
            Err(format!(
                "{} missing (created on first run)",
                paths.data.display()
            ))
        },
    );
    check(
        "config.toml",
        Config::load(&paths.config)
            .map(|_| "valid".into())
            .map_err(|e| e.to_string()),
    );
    let (_, loaded) = crate::state::State::load(&paths.state);
    check(
        "state.json",
        match loaded {
            crate::state::Loaded::Read => Ok("valid".into()),
            crate::state::Loaded::Absent => Ok("absent".into()),
            crate::state::Loaded::Corrupt => Err("was corrupt, moved aside".into()),
        },
    );
    let running = matches!(
        crate::service::lock(&paths.lock),
        Err(crate::service::ServiceError::AlreadyRunning)
    );
    check(
        "service",
        match crate::service::lock(&paths.lock) {
            Err(crate::service::ServiceError::AlreadyRunning) => Ok("running".into()),
            Ok(_) => Ok("not running".into()),
            Err(e) => Err(e.to_string()),
        },
    );
    check(
        "status.json",
        match LiveStatus::read(&paths.status) {
            None => Ok("absent (the daemon has not run here)".into()),
            Some(live) => {
                let age = live.age_s(crate::core::wall_now());
                let summary = format!(
                    "{age} s old: link {:?}, session {:?}, sid {}, link drops {}, device err {}",
                    live.link,
                    live.session,
                    live.sid,
                    live.metric.link_drops,
                    live.metric.err_total()
                );
                if running && age > STATUS_STALE_S {
                    Err(format!("stale while the service runs — {summary}"))
                } else {
                    Ok(summary)
                }
            }
        },
    );
    check(
        "socket",
        if paths.socket.as_os_str().len() > crate::api::uds::SUN_PATH_MAX {
            Err(format!(
                "{} is longer than sun_path allows ({} > {})",
                paths.socket.display(),
                paths.socket.as_os_str().len(),
                crate::api::uds::SUN_PATH_MAX
            ))
        } else if !paths.socket.exists() {
            Ok("absent".into())
        } else {
            match one_call(
                &paths.socket,
                ctl_config(&paths.socket, "deckd-doctor", Vec::new()),
                DOCTOR_TIMEOUT,
                "session.ping",
                serde_json::json!({}),
            ) {
                Ok(_) => Ok(format!("{} answers session.ping", paths.socket.display())),
                Err(CtlFailure::Unreachable(why)) if !running => Ok(format!("stale ({why})")),
                Err(CtlFailure::Unreachable(why)) => Err(why),
                Err(CtlFailure::Rpc(e)) => Err(e.to_string()),
            }
        },
    );
    check("tcp", tcp_check(paths, running, door));
    check(
        "tokens.toml",
        match std::fs::metadata(&paths.tokens) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                Ok("absent (no bearer tokens yet: deckd token create)".into())
            }
            Err(e) => Err(e.to_string()),
            Ok(meta) => {
                #[cfg(unix)]
                let mode = {
                    use std::os::unix::fs::PermissionsExt;
                    meta.permissions().mode() & 0o777
                };
                let count = crate::tokens::Tokens::load(&paths.tokens)
                    .map(|t| t.list().len())
                    .map_err(|e| e.to_string());
                // Windows has no mode bits: the file is private because the
                // data directory (%LOCALAPPDATA%) is the user's own, which
                // is what is said, rather than a mode nothing checked.
                #[cfg(not(unix))]
                let row = {
                    let _ = meta;
                    count.map(|n| format!("{n} token(s), in this user's own data folder"))
                };
                #[cfg(unix)]
                let row = match (mode, count) {
                    (0o600, Ok(n)) => Ok(format!("mode 0600, {n} token(s)")),
                    (0o600, Err(e)) => Err(e),
                    (mode, _) => Err(format!(
                        "mode {mode:04o}, expected 0600 ({})",
                        crate::integrations::command_line([
                            "chmod",
                            "600",
                            &paths.tokens.display().to_string(),
                        ])
                    )),
                };
                row
            }
        },
    );
    checks
}

/// The doctor's `device` row: whether HID lists the deck, and on which
/// buses. It enumerates and opens nothing, but a service started with
/// `--no-device` may not enumerate either, so `care.checks` asks for this
/// row only of a service that may (O16).
pub fn device_check() -> (String, bool, String) {
    let found = crate::hid::probe(|api| {
        api.device_list()
            .filter(|d| d.vendor_id() == crate::hid::VID && d.product_id() == crate::hid::PID)
            .map(|d| format!("{:?}", d.bus_type()).to_lowercase())
            .collect::<Vec<_>>()
    })
    .map_err(|_| "cannot initialize HID".to_string())
    .and_then(|found| {
        if found.is_empty() {
            Err("OpenWalkieTalkie not connected".into())
        } else {
            Ok(format!(
                "OpenWalkieTalkie present ({} interfaces: {})",
                found.len(),
                found.join(", ")
            ))
        }
    });
    match found {
        Ok(detail) => ("device".into(), true, detail),
        Err(detail) => ("device".into(), false, detail),
    }
}

/// Python's `repr(bytes)`, for the `  > ` lines of the dry run.
pub fn py_bytes_repr(bytes: &[u8]) -> String {
    let quote = if bytes.contains(&b'\'') && !bytes.contains(&b'"') {
        b'"'
    } else {
        b'\''
    };
    let mut out = String::with_capacity(bytes.len() + 3);
    out.push('b');
    out.push(quote as char);
    for &b in bytes {
        match b {
            b'\\' => out.push_str("\\\\"),
            b'\t' => out.push_str("\\t"),
            b'\n' => out.push_str("\\n"),
            b'\r' => out.push_str("\\r"),
            _ if b == quote => {
                out.push('\\');
                out.push(b as char);
            }
            0x20..=0x7e => out.push(b as char),
            _ => out.push_str(&format!("\\x{b:02x}")),
        }
    }
    out.push(quote as char);
    out
}

/// The demo session as `deck-sim.py --dry-run` prints it: every report, the
/// line run back through the assembler, and the line itself.
pub fn dry_run(
    out: &mut dyn std::io::Write,
    sid: u32,
    hm: Option<String>,
    flood: usize,
    send: &[String],
) -> Result<(), String> {
    let hm = match hm {
        Some(hm) => hm,
        None => crate::device::session::local_hm(std::time::SystemTime::now())
            .unwrap_or_else(|| "00:00".into()),
    };
    let mut lines = vec![HostLine::Hello { sid, hm: Some(hm) }];
    let sync_at = lines.len();
    lines.extend(crate::demo::roster());
    lines.push(HostLine::Faces {
        list: crate::demo::default_faces(),
    });
    lines.push(HostLine::Synced);
    let flood_at = lines.len();
    lines.extend((0..flood).map(crate::demo::flood_title));
    for json in send {
        lines.push(
            deck_proto::parse_host_line(json.as_bytes()).map_err(|e| format!("--send: {e}"))?,
        );
    }
    let mut assembler = LineAssembler::new();
    let mut chunker = deck_proto::Chunker::new();
    let mut sent = 0usize;
    let io = |e: std::io::Error| e.to_string();
    for (i, line) in lines.iter().enumerate() {
        if i == sync_at {
            writeln!(out, "== full sync").map_err(io)?;
        }
        if i == flood_at && flood > 0 {
            writeln!(out, "== title flood: {flood} updates").map_err(io)?;
        }
        let bytes = encode(line).map_err(|e| e.to_string())?;
        chunker.push_line(&bytes);
        while let Some(report) = chunker.next_report() {
            let len = usize::from(report[2]);
            writeln!(
                out,
                "  > {} {}",
                hex(&report[..3]),
                py_bytes_repr(&report[3..3 + len])
            )
            .map_err(io)?;
            let mut echoed = Vec::new();
            assembler.push(&report, &mut |got| {
                echoed.push(match got {
                    Ok(bytes) => String::from_utf8_lossy(bytes).into_owned(),
                    Err(_) => "<oversized>".into(),
                });
            });
            for line in echoed {
                writeln!(out, "  = {line}").map_err(io)?;
            }
            sent += 1;
        }
        writeln!(
            out,
            "-> {}",
            String::from_utf8_lossy(&bytes[..bytes.len() - 1])
        )
        .map_err(io)?;
    }
    writeln!(out, "dry run: {sent} reports").map_err(io)?;
    Ok(())
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// The SIGTERM stream where there is no SIGTERM (Windows): never fires.
#[cfg(not(unix))]
struct Never;

#[cfg(not(unix))]
impl Never {
    async fn recv(&mut self) -> Option<()> {
        std::future::pending().await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A blocking `deck_client::Client` for the tests: one runtime, one
    /// session, calls by method name. The CLI itself never keeps a session
    /// open (every command is `one_call`), so this lives in the tests.
    struct Session {
        rt: tokio::runtime::Runtime,
        client: Option<Client>,
        socket: PathBuf,
    }

    impl Session {
        fn open(
            socket: &Path,
            client: &str,
            scopes: &[String],
            timeout: Duration,
        ) -> Result<Session, CtlFailure> {
            let rt = runtime()?;
            let scopes = parse_scopes(scopes).map_err(CtlFailure::Unreachable)?;
            let config = ctl_config(socket, client, scopes);
            let opened = rt
                .block_on(async { tokio::time::timeout(timeout, Client::connect(config)).await })
                .map_err(|_| CtlFailure::Unreachable("no hello reply".into()))?
                .map_err(|e| CtlFailure::of(e, socket))?;
            Ok(Session {
                rt,
                client: Some(opened),
                socket: socket.to_path_buf(),
            })
        }

        fn call(
            &mut self,
            method: &str,
            params: serde_json::Value,
        ) -> Result<serde_json::Value, CtlFailure> {
            let client = self.client.as_ref().expect("the session is open");
            self.rt
                .block_on(client.call(method, params))
                .map_err(|e| CtlFailure::of(e, &self.socket))
        }
    }

    impl Drop for Session {
        fn drop(&mut self) {
            if let Some(client) = self.client.take() {
                self.rt.block_on(client.close());
            }
        }
    }

    #[test]
    fn bytes_repr_matches_python() {
        assert_eq!(py_bytes_repr(b"{\"m\":\"up\"}\n"), "b'{\"m\":\"up\"}\\n'");
        assert_eq!(
            py_bytes_repr("é⠋".as_bytes()),
            "b'\\xc3\\xa9\\xe2\\xa0\\x8b'"
        );
        assert_eq!(py_bytes_repr(b"it's"), "b\"it's\"");
        assert_eq!(py_bytes_repr(b"it's \"x\""), "b'it\\'s \"x\"'");
        assert_eq!(
            py_bytes_repr(b"a\\b\t\r\x00\x7f"),
            "b'a\\\\b\\t\\r\\x00\\x7f'"
        );
        assert_eq!(py_bytes_repr(b""), "b''");
    }

    #[test]
    fn the_dry_run_has_deck_sims_layout() {
        let mut out = Vec::new();
        dry_run(
            &mut out,
            314159,
            Some("19:42".into()),
            2,
            &["{\"m\":\"set\",\"k\":\"brightness\",\"v\":2}".into()],
        )
        .unwrap();
        let text = String::from_utf8(out).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(
            lines[0],
            "  > 06052e b'{\"m\":\"hello\",\"v\":1,\"sid\":314159,\"hm\":\"19:42\"}\\n'"
        );
        assert_eq!(
            lines[1],
            "  = {\"m\":\"hello\",\"v\":1,\"sid\":314159,\"hm\":\"19:42\"}"
        );
        assert_eq!(
            lines[2],
            "-> {\"m\":\"hello\",\"v\":1,\"sid\":314159,\"hm\":\"19:42\"}"
        );
        assert_eq!(lines[3], "== full sync");
        assert!(lines[4].starts_with("  > 06053d b'{\"m\":\"agent\",\"id\":1,\"kind\":\"codex\""));
        assert!(text.contains("== title flood: 2 updates\n"));
        assert!(text.contains(" b'{\"m\":\"agent\",\"id\":3,\"title\":\"Compiling openmicro \\xe2\\xa0\\x8b 0\"}\\n'\n"));
        assert!(
            text.contains("  = {\"m\":\"agent\",\"id\":3,\"title\":\"Compiling openmicro ⠋ 0\"}\n")
        );
        assert!(text.contains("-> {\"m\":\"set\",\"k\":\"brightness\",\"v\":2}\n"));
        let last = lines.last().unwrap();
        assert!(last.starts_with("dry run: ") && last.ends_with(" reports"));
        let reports = text.lines().filter(|l| l.starts_with("  > ")).count();
        assert_eq!(*last, format!("dry run: {reports} reports"));
        assert!(dry_run(&mut Vec::new(), 1, None, 0, &["{\"m\":\"nope\"}".into()]).is_err());
        assert!(dry_run(&mut Vec::new(), 1, None, 0, &[]).is_ok());
    }

    /// The `hook` subcommand is the one the CLIs' own config files name,
    /// so its argument shapes are a compatibility surface: Codex appends
    /// the `notify` JSON as the last argument (facts §2.2), and Claude's
    /// `statusLine` runs in a shell, so the installer passes the user's
    /// previous command after `--` where a leading `-` would otherwise be
    /// read as one of ours (C23).
    #[test]
    fn the_hook_subcommand_takes_a_payload_and_a_passthrough() {
        use clap::Parser;
        let bare = Cli::try_parse_from(["deckd", "hook", "codex"]).unwrap();
        match bare.cmd {
            Cmd::Hook { cli, rest } => {
                assert_eq!(cli, "codex");
                assert!(rest.is_empty());
            }
            other => panic!("{other:?}"),
        }
        // `notify = ["…/deckd", "hook", "codex-notify"]` plus the appended
        // payload; a fixed subcommand may sit between, as the user's own
        // `notify` has. The payload is always the last entry.
        let notify = Cli::try_parse_from([
            "deckd",
            "hook",
            "codex-notify",
            "turn-ended",
            r#"{"type":"agent-turn-complete","thread-id":"t"}"#,
        ])
        .unwrap();
        match notify.cmd {
            Cmd::Hook { cli, rest } => {
                assert_eq!(cli, "codex-notify");
                assert_eq!(rest.len(), 2);
                assert!(rest.last().unwrap().starts_with('{'));
            }
            other => panic!("{other:?}"),
        }
        // The chained statusLine: everything after `--` is the user's, and
        // its own flags are never ours.
        let chained = Cli::try_parse_from([
            "deckd",
            "hook",
            "claude-statusline",
            "--",
            "/usr/bin/env",
            "python3",
            "--quiet",
            "line.py",
        ])
        .unwrap();
        match chained.cmd {
            Cmd::Hook { cli, rest } => {
                assert_eq!(cli, "claude-statusline");
                assert_eq!(rest, ["/usr/bin/env", "python3", "--quiet", "line.py"]);
            }
            other => panic!("{other:?}"),
        }
        // A data dir still comes first, as the tests and the gate pass it.
        let scoped =
            Cli::try_parse_from(["deckd", "--data-dir", "/tmp/x", "hook", "claude"]).unwrap();
        assert_eq!(
            scoped.data_dir.as_deref(),
            Some(std::path::Path::new("/tmp/x"))
        );
    }

    #[test]
    fn the_cli_parses_every_subcommand() {
        use clap::Parser;
        let cli = Cli::try_parse_from(["deckd", "run", "--foreground", "--demo"]).unwrap();
        assert!(matches!(
            cli.cmd,
            Cmd::Run {
                foreground: true,
                demo: true,
                no_device: false,
            }
        ));
        assert!(matches!(
            Cli::try_parse_from(["deckd", "run", "--no-device"])
                .unwrap()
                .cmd,
            Cmd::Run {
                foreground: false,
                demo: false,
                no_device: true,
            }
        ));
        let cli =
            Cli::try_parse_from(["deckd", "--data-dir", "/tmp/x", "status", "--json"]).unwrap();
        assert_eq!(
            cli.data_dir.as_deref(),
            Some(std::path::Path::new("/tmp/x"))
        );
        assert!(matches!(
            cli.cmd,
            Cmd::Status {
                json: true,
                live: false
            }
        ));
        assert!(matches!(
            Cli::try_parse_from(["deckd", "status", "--live"])
                .unwrap()
                .cmd,
            Cmd::Status {
                json: false,
                live: true
            }
        ));
        // ctl: options before the method, params optional.
        let cli = Cli::try_parse_from([
            "deckd",
            "ctl",
            "--scopes",
            "operate,observe:content",
            "--client",
            "gate",
            "--timeout",
            "2.5",
            "op.answer",
            r#"{"agent":"ag_1","card":1,"choice":0}"#,
        ])
        .unwrap();
        match cli.cmd {
            Cmd::Ctl {
                scopes,
                client,
                timeout,
                method,
                params,
            } => {
                assert_eq!(scopes, vec!["operate", "observe:content"]);
                assert_eq!(client.as_deref(), Some("gate"));
                assert_eq!(timeout, Some(2.5));
                assert_eq!(method, "op.answer");
                assert_eq!(
                    params.as_deref(),
                    Some(r#"{"agent":"ag_1","card":1,"choice":0}"#)
                );
            }
            other => panic!("{other:?}"),
        }
        match Cli::try_parse_from(["deckd", "ctl", "session.ping"])
            .unwrap()
            .cmd
        {
            Cmd::Ctl {
                scopes,
                client,
                timeout,
                method,
                params,
            } => {
                assert!(scopes.is_empty() && client.is_none() && timeout.is_none());
                assert_eq!(method, "session.ping");
                assert_eq!(params, None);
            }
            other => panic!("{other:?}"),
        }
        assert!(
            Cli::try_parse_from(["deckd", "ctl"]).is_err(),
            "method required"
        );
        // events: every flag, and none.
        match Cli::try_parse_from([
            "deckd",
            "events",
            "--kinds",
            "card.*,op.confirm_requested",
            "--since",
            "12",
            "--content",
            "--snapshot",
        ])
        .unwrap()
        .cmd
        {
            Cmd::Events {
                kinds,
                since,
                content,
                snapshot,
            } => {
                assert_eq!(kinds, vec!["card.*", "op.confirm_requested"]);
                assert_eq!(since, Some(12));
                assert!(content && snapshot);
            }
            other => panic!("{other:?}"),
        }
        assert!(matches!(
            Cli::try_parse_from(["deckd", "events"]).unwrap().cmd,
            Cmd::Events {
                since: None,
                content: false,
                snapshot: false,
                ..
            }
        ));
        // token: create with every option, list, revoke, rotate.
        match Cli::try_parse_from([
            "deckd",
            "token",
            "create",
            "--name",
            "build-bot",
            "--scopes",
            "observe,voice",
            "--expires",
            "30d",
            "--client",
            "codex",
        ])
        .unwrap()
        .cmd
        {
            Cmd::Token {
                cmd:
                    TokenCmd::Create {
                        name,
                        scopes,
                        expires,
                        client,
                    },
            } => {
                assert_eq!(name, "build-bot");
                assert_eq!(scopes, vec!["observe", "voice"]);
                assert_eq!(expires.as_deref(), Some("30d"));
                assert_eq!(client.as_deref(), Some("codex"));
            }
            other => panic!("{other:?}"),
        }
        assert!(
            Cli::try_parse_from(["deckd", "token", "create", "--name", "x"]).is_err(),
            "scopes required"
        );
        assert!(matches!(
            Cli::try_parse_from(["deckd", "token", "list", "--json"])
                .unwrap()
                .cmd,
            Cmd::Token {
                cmd: TokenCmd::List { json: true }
            }
        ));
        assert!(matches!(
            Cli::try_parse_from(["deckd", "token", "revoke", "gate"])
                .unwrap()
                .cmd,
            Cmd::Token {
                cmd: TokenCmd::Revoke { name }
            } if name == "gate"
        ));
        assert!(matches!(
            Cli::try_parse_from(["deckd", "token", "rotate", "hooks"])
                .unwrap()
                .cmd,
            Cmd::Token {
                cmd: TokenCmd::Rotate { name }
            } if name == "hooks"
        ));
        assert!(Cli::try_parse_from(["deckd", "token"]).is_err());
        let cli = Cli::try_parse_from(["deckd", "device", "flood", "200"]).unwrap();
        assert!(matches!(
            cli.cmd,
            Cmd::Device {
                cmd: DeviceCmd::Flood { n: 200 }
            }
        ));
        assert!(matches!(
            Cli::try_parse_from(["deckd", "device", "sync"])
                .unwrap()
                .cmd,
            Cmd::Device {
                cmd: DeviceCmd::Sync
            }
        ));
        assert!(matches!(
            Cli::try_parse_from(["deckd", "device", "metrics"])
                .unwrap()
                .cmd,
            Cmd::Device {
                cmd: DeviceCmd::Metrics
            }
        ));
        let cli = Cli::try_parse_from([
            "deckd", "device", "dry-run", "--flood", "3", "--send", "{}", "--send", "{}", "--sid",
            "5", "--hm", "01:02",
        ])
        .unwrap();
        match cli.cmd {
            Cmd::Device {
                cmd:
                    DeviceCmd::DryRun {
                        flood,
                        send,
                        sid,
                        hm,
                    },
            } => {
                assert_eq!(
                    (flood, send.len(), sid, hm.as_deref()),
                    (3, 2, Some(5), Some("01:02"))
                );
            }
            other => panic!("{other:?}"),
        }
        assert!(matches!(
            Cli::try_parse_from(["deckd", "doctor"]).unwrap().cmd,
            Cmd::Doctor { json: false }
        ));
        assert!(Cli::try_parse_from(["deckd", "bogus"]).is_err());
    }

    #[test]
    fn the_ble_bench_commands_parse_and_refuse_what_the_contract_forbids() {
        use clap::Parser;
        let probe = |args: &[&str]| match Cli::try_parse_from(args).map(|c| c.cmd) {
            Ok(Cmd::Ble {
                cmd:
                    BleCmd::Probe {
                        seconds,
                        tone,
                        loopback,
                        name,
                        ..
                    },
            }) => {
                assert_eq!(name, "OpenWalkieTalkie");
                Ok((seconds, tone, loopback))
            }
            Ok(other) => panic!("{other:?}"),
            Err(e) => Err(e.kind()),
        };
        assert_eq!(probe(&["deckd", "ble", "probe"]), Ok((5, None, false)));
        assert_eq!(
            probe(&[
                "deckd",
                "ble",
                "probe",
                "--seconds",
                "3",
                "--tone",
                "1000",
                "--loopback"
            ]),
            Ok((3, Some(1000), true))
        );
        assert_eq!(
            probe(&["deckd", "ble", "probe", "--tone", "440"]),
            Ok((5, Some(440), false))
        );
        // Loopback is a tone coming back: no tone, nothing to listen for.
        assert!(probe(&["deckd", "ble", "probe", "--loopback"]).is_err());
        // MIC carries 1 to 60 seconds; the tone stays inside 16 kHz audio.
        assert!(probe(&["deckd", "ble", "probe", "--seconds", "0"]).is_err());
        assert!(probe(&["deckd", "ble", "probe", "--seconds", "61"]).is_err());
        assert!(probe(&["deckd", "ble", "probe", "--tone", "20"]).is_err());
        assert!(probe(&["deckd", "ble", "probe", "--tone", "8000"]).is_err());
        assert!(matches!(
            Cli::try_parse_from(["deckd", "ble", "scan"]).unwrap().cmd,
            Cmd::Ble {
                cmd: BleCmd::Scan { seconds: 5, name }
            } if name == "OpenWalkieTalkie"
        ));
        assert!(matches!(
            Cli::try_parse_from(["deckd", "ble", "scan", "--seconds", "2", "--name", "r6 bench"])
                .unwrap()
                .cmd,
            Cmd::Ble {
                cmd: BleCmd::Scan { seconds: 2, name }
            } if name == "r6 bench"
        ));
        assert!(matches!(
            Cli::try_parse_from(["deckd", "ble", "probe", "--name", "r6 bench"])
                .unwrap()
                .cmd,
            Cmd::Ble {
                cmd: BleCmd::Probe { name, .. }
            } if name == "r6 bench"
        ));
        assert!(Cli::try_parse_from(["deckd", "ble"]).is_err());
    }

    /// `deckd probe` (O19): the JSON names one bus per instance HID lists —
    /// none, one or two — and the exit code says the same as `present`: 0
    /// when a deck is there, 1 when not, never clap's own 2. A HID that
    /// cannot start is nothing found. Nothing here enumerates: the lists
    /// are the probe's input.
    #[test]
    fn the_probe_prints_each_instance_and_exits_by_whether_there_is_one() {
        use hidapi::BusType;
        use serde_json::{json, Value};
        let usb = (BusType::Usb, None);
        let bluetooth = (BusType::Bluetooth, None);
        assert_eq!(probe_json(&[]), json!({"present": false, "instances": []}));
        assert_eq!(
            probe_json(std::slice::from_ref(&usb)),
            json!({"present": true, "instances": [{"bus": "usb", "id": null}]})
        );
        assert_eq!(
            probe_json(&[bluetooth.clone(), usb.clone()]),
            json!({"present": true, "instances": [
                {"bus": "bluetooth", "id": null}, {"bus": "usb", "id": null}]})
        );
        assert_eq!(probe_json(&[(BusType::Spi, None)])["instances"][0]["bus"], "unknown");
        let run = |found, json| {
            let mut out = Vec::new();
            let code = probe_cmd(found, json, &mut out).unwrap();
            (code, String::from_utf8(out).unwrap())
        };
        let read = |out: &str| serde_json::from_str::<Value>(out).unwrap();
        let (code, out) = run(Ok(vec![usb.clone(), bluetooth.clone()]), true);
        assert_eq!(code, 0);
        assert_eq!(read(&out), probe_json(&[usb, bluetooth.clone()]));
        let (code, out) = run(Ok(Vec::new()), true);
        assert_eq!((code, read(&out)), (1, probe_json(&[])));
        let (code, out) = run(Err(crate::hid::HidError::Init), true);
        assert_eq!((code, read(&out)), (1, probe_json(&[])));
        assert_eq!(
            run(Ok(vec![bluetooth]), false),
            (0, "OpenWalkieTalkie present: bluetooth\n".to_string())
        );
        assert_eq!(
            run(Ok(Vec::new()), false),
            (1, "no OpenWalkieTalkie found\n".to_string())
        );
        assert!(matches!(
            Cli::try_parse_from(["deckd", "probe", "--json"]).unwrap().cmd,
            Cmd::Probe { json: true }
        ));
        assert!(matches!(
            Cli::try_parse_from(["deckd", "probe"]).unwrap().cmd,
            Cmd::Probe { json: false }
        ));
    }

    /// The probe's `id` is how the owner sees whether this Mac can tell a
    /// deck linked over Bluetooth only from another one: the same label as
    /// over the cable when HID lists its address there, in either spelling,
    /// and null when it lists none. The serial number itself is never
    /// printed.
    #[test]
    fn the_probe_names_each_instance_by_its_decks_label_and_never_its_serial() {
        use hidapi::BusType;
        let label = crate::device::identity::label("A1B2C3D4E5F6");
        let found = [
            (BusType::Usb, Some("A1B2C3D4E5F6".to_string())),
            (BusType::Bluetooth, Some("f6:e5:d4:c3:b2:a1".to_string())),
            (BusType::Bluetooth, Some(String::new())),
            (BusType::Bluetooth, Some("000000000000".to_string())),
            (BusType::Bluetooth, None),
        ];
        let json = probe_json(&found);
        let ids: Vec<&serde_json::Value> = json["instances"]
            .as_array()
            .unwrap()
            .iter()
            .map(|instance| &instance["id"])
            .collect();
        assert_eq!(ids[0], &serde_json::json!(label));
        assert_eq!(ids[1], &serde_json::json!(label), "the same deck over Bluetooth");
        assert!(ids[2..].iter().all(|id| id.is_null()), "{ids:?}");
        let text = json.to_string().to_ascii_lowercase();
        assert!(!text.contains("a1b2c3d4e5f6") && !text.contains("f6:e5"), "{text}");
    }

    #[test]
    fn status_and_doctor_work_on_an_empty_data_dir() {
        let dir = std::env::temp_dir().join(format!("deckd-cli-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let p = Paths::from_data_dir(dir.clone(), dir.join("logs"), dir.join("caches"));
        let s = status(&p);
        assert!(!s.running && !s.state_present && s.live.is_none());
        let text = status_text(&s);
        assert!(text.contains("not running"));
        assert!(text.contains("status.json: absent"));
        let d = doctor(&p);
        assert!(d.checks.iter().any(|(n, ok, _)| n == "config.toml" && *ok));
        assert!(d
            .checks
            .iter()
            .any(|(n, ok, d)| n == "state.json" && *ok && d == "absent"));
        assert!(d
            .checks
            .iter()
            .any(|(n, ok, d)| n == "status.json" && *ok && d.starts_with("absent")));
        // How the service starts: launchd's agent, or Windows' Run value.
        let start_row = if cfg!(windows) { "startup" } else { "launchd deckd" };
        assert!(d.checks.iter().any(|(n, _, _)| n == start_row));
        // A status file makes the text report the link; stale only counts while running.
        let live = LiveStatus {
            pid: 1,
            sid: 5,
            updated: crate::core::wall_now(),
            demo: true,
            ..LiveStatus::default()
        };
        live.write(&p.status).unwrap();
        let s = status(&p);
        assert_eq!(s.live_age_s, Some(0));
        let text = status_text(&s);
        assert!(text.contains("sid 5"));
        assert!(text.contains("last known"));
        let d = doctor(&p);
        assert!(d
            .checks
            .iter()
            .any(|(n, ok, d)| n == "status.json" && *ok && d.contains("sid 5")));
        // Without a service every socket command is "unreachable" (exit 2);
        // `events` is honest about not existing yet (exit 2 too).
        for cmd in [
            Cmd::Device {
                cmd: DeviceCmd::Flood { n: 3 },
            },
            Cmd::Device {
                cmd: DeviceCmd::Sync,
            },
            Cmd::Device {
                cmd: DeviceCmd::Metrics,
            },
            Cmd::Ctl {
                scopes: vec![],
                client: None,
                timeout: None,
                method: "session.ping".into(),
                params: None,
            },
            Cmd::Events {
                kinds: vec![],
                since: None,
                content: false,
                snapshot: false,
            },
            Cmd::Status {
                json: true,
                live: true,
            },
        ] {
            let code = run(Cli {
                data_dir: Some(dir.clone()),
                cmd,
            })
            .unwrap();
            assert_eq!(code, EXIT_UNREACHABLE);
        }
        let failure = uds_call(&p.socket, &[], "session.ping", serde_json::json!({})).unwrap_err();
        assert!(
            matches!(failure, CtlFailure::Unreachable(ref why) if why.contains("is the service running"))
        );
        assert!(run(Cli {
            data_dir: Some(dir.clone()),
            cmd: Cmd::Ctl {
                scopes: vec![],
                client: None,
                timeout: None,
                method: "session.ping".into(),
                params: Some("not json".into()),
            },
        })
        .unwrap_err()
        .contains("params are not JSON"));
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// D3, and the owner's decision 12 that made off the default: an empty
    /// port file is the door off — by default, or by `[api] tcp = false`,
    /// even with the old Claude hooks about — not a degraded one, so the
    /// doctor (and every check built on its rows) raises nothing and says
    /// which; with the door on, the file's or the old hooks', it is still
    /// the degrade it always was. How the door is decided is asked only
    /// when the port file names no port: the doctor's own answer reads
    /// Claude's settings.
    #[test]
    fn the_doctor_reads_no_port_as_the_door_switched_off_when_it_is() {
        use crate::config::TcpDoor;
        let dir = std::env::temp_dir().join(format!("deckd-cli-tcp-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let p = Paths::from_data_dir(dir.clone(), dir.join("logs"), dir.join("caches"));
        std::fs::write(&p.port, "").unwrap();
        let decided = |file: &str, legacy_hooks: bool| -> Option<TcpDoor> {
            Some(Config::parse(file).unwrap().api.tcp_door(|| legacy_hooks))
        };
        for running in [true, false] {
            let off = tcp_check(&p, running, || decided("", false)).unwrap();
            assert!(off.starts_with("off ([api] tcp is left out"), "{off}");
            assert!(off.ends_with("the socket is the only door"), "{off}");
            let off = tcp_check(&p, running, || decided("[api]\ntcp = false\n", true)).unwrap();
            assert!(off.starts_with("off ([api] tcp = false)"), "{off}");
        }
        for (file, legacy_hooks) in [("[api]\ntcp = true\n", false), ("", true)] {
            let on = || decided(file, legacy_hooks);
            let degraded = tcp_check(&p, true, on).unwrap_err();
            assert!(degraded.contains("degraded"), "{degraded}");
            assert!(tcp_check(&p, false, on).unwrap().contains("not running"));
        }
        // A config.toml that does not load is no reason to call it off.
        let degraded = tcp_check(&p, true, || None).unwrap_err();
        assert!(degraded.contains("degraded"), "{degraded}");
        // A port named: probed, and the door's decision is never asked. A
        // port just given back answers nothing, which a stopped daemon
        // leaves behind.
        let closed = std::net::TcpListener::bind(("127.0.0.1", 0))
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        std::fs::write(&p.port, closed.to_string()).unwrap();
        let stale = tcp_check(&p, false, || panic!("not asked with a port named")).unwrap();
        assert!(
            stale.starts_with(&format!("stale port {closed}")),
            "{stale}"
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// `deckd status` gives the linked deck's own status a line of its own
    /// (its bus, charge and board, a dash for what is not known), and says
    /// how the channel-2 requests behind it fare.
    #[test]
    fn the_status_text_names_the_decks_transport_charge_and_board() {
        let status = |live: LiveStatus| Status {
            running: true,
            data_dir: PathBuf::from("/nonexistent/deckd"),
            state_present: false,
            faces: 0,
            card_ids: 0,
            port: None,
            live: Some(live),
            live_age_s: Some(0),
        };
        let linked = LiveStatus {
            transport: Some(deck_model::Transport::Bluetooth),
            battery: Some(85),
            charging: Some(false),
            board: Some("Rev B".into()),
            metric: crate::metrics::Metric {
                status_polls: 4,
                status_replies: 3,
                status_timeouts: 1,
                ..Default::default()
            },
            ..LiveStatus::default()
        };
        let text = status_text(&status(linked));
        let deck = "\ndeck: transport bluetooth, battery 85%, charging no, board Rev B\n";
        assert!(text.contains(deck), "{text}");
        assert!(
            text.contains("status polls 4 replies 3 timeouts 1 errors 0"),
            "{text}"
        );
        let text = status_text(&status(LiveStatus::default()));
        let deck = "\ndeck: transport -, battery -, charging -, board -\n";
        assert!(text.contains(deck), "{text}");
    }

    /// `deckd status` sums the delivery record up in one line from
    /// `status.json`, and a file from a daemon that kept none has no line.
    #[test]
    fn the_status_text_sums_up_the_delivery_record() {
        let status = |live: LiveStatus| Status {
            running: true,
            data_dir: PathBuf::from("/nonexistent/deckd"),
            state_present: false,
            faces: 0,
            card_ids: 0,
            port: None,
            live: Some(live),
            live_age_s: Some(0),
        };
        let brief = crate::deliveries::Brief {
            since: Some(deck_model::Ts(1_789_760_523_000)),
            cards: 14,
            answered: 9,
            answers_on_deck: 6,
            failure_cards: 1,
            reader: crate::deliveries::Reader {
                channels: 5,
                channels_by_ptt: 2,
                history: 3,
                plans: 1,
                tool_rows: 4,
                images: 2,
                pickers: 1,
                ..Default::default()
            },
        };
        let text = status_text(&status(LiveStatus {
            deliveries: Some(brief.clone()),
            ..LiveStatus::default()
        }));
        let line = "\ncards since 2026-09-18T19:42:03.000Z: 14 from the sources, answered 9 (on \
                    the deck 6), failure cards 1; reader: channels 5 (by PTT 2), history 3, \
                    plans 1, tool rows 4, images 2, pickers 1";
        assert!(text.ends_with(line), "{text}");
        // With the collectors' line (`crate::dormancy`) as well, the record
        // comes first and the collectors' line ends the text.
        let text = status_text(&status(LiveStatus {
            deliveries: Some(brief),
            collecting: Some(true),
            ..LiveStatus::default()
        }));
        assert!(text.ends_with(&format!("{line}\ncollectors: on")), "{text}");
        let text = status_text(&status(LiveStatus::default()));
        assert!(!text.contains("cards since"), "{text}");
    }

    /// `deckd status` says whether the daemon collects from agent sessions
    /// (`crate::dormancy`), and since when it stopped on this Mac's clock;
    /// a daemon with no dormancy (`--no-device`) gets no line at all.
    #[test]
    fn the_status_text_says_whether_the_collectors_run() {
        let status = |live: LiveStatus| Status {
            running: true,
            data_dir: PathBuf::from("/nonexistent/deckd"),
            state_present: false,
            faces: 0,
            card_ids: 0,
            port: None,
            live: Some(live),
            live_age_s: Some(0),
        };
        let text = status_text(&status(LiveStatus::default()));
        assert!(!text.contains("collectors"), "{text}");
        let on = LiveStatus {
            collecting: Some(true),
            ..LiveStatus::default()
        };
        assert!(status_text(&status(on)).ends_with("\ncollectors: on"));
        let since = deck_model::Ts(1_789_760_523_000);
        let at = std::time::UNIX_EPOCH + std::time::Duration::from_millis(since.millis());
        let hm = crate::device::session::local_hm(at).unwrap();
        let off = LiveStatus {
            collecting: Some(false),
            dormant_since: Some(since),
            ..LiveStatus::default()
        };
        assert!(status_text(&status(off)).ends_with(&format!(
            "\ncollectors: off since {hm} (no deck linked for ten minutes)"
        )));
        let never = LiveStatus {
            collecting: Some(false),
            ..LiveStatus::default()
        };
        assert!(
            status_text(&status(never)).ends_with("\ncollectors: off (no deck has been linked)")
        );
    }

    #[test]
    fn ctl_and_the_device_commands_speak_json_rpc_over_the_socket() {
        use crate::api::testkit::test_api;
        use crate::trust::FakeVerifier;
        let daemon = test_api("cli-ctl", FakeVerifier::default());
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap();
        let served = rt.block_on(crate::api::serve(daemon.api.clone()));
        let socket = daemon.api.paths.socket.clone();
        // The metric feeder: the detached hid side answers each request.
        let debug = daemon.core.debug.clone();
        let lines = daemon.core.lines.clone();
        let feeder = std::thread::spawn(move || {
            while let Ok(cmd) = debug.recv() {
                if cmd == crate::hid::HidDebug::Metric {
                    let _ = lines.send(crate::hid::FromHid::Metric(crate::metrics::Metric {
                        lines_sent: 12,
                        ..Default::default()
                    }));
                }
            }
        });
        // A session: hello, then two calls on the same connection.
        let mut session =
            Session::open(&socket, "test", &["admin".into()], Duration::from_secs(5)).unwrap();
        let ping = session.call("session.ping", serde_json::json!({})).unwrap();
        assert!(ping["ts"].is_string());
        let flood = session
            .call("device.flood", serde_json::json!({"n": 4}))
            .unwrap();
        assert_eq!(flood["queued"], 4);
        let err = session.call("nope", serde_json::json!({})).unwrap_err();
        assert!(matches!(err, CtlFailure::Rpc(ref e) if e.code == ApiError::METHOD_NOT_FOUND));
        // Missing scope: forbidden (exit 1); a trust refusal would be exit 3.
        let observe = uds_call(&socket, &[], "device.sync", serde_json::json!({})).unwrap_err();
        assert!(matches!(observe, CtlFailure::Rpc(ref e) if e.code == ApiError::FORBIDDEN));
        assert_eq!(ctl_failure(observe), EXIT_RPC_ERROR);
        assert_eq!(
            ctl_failure(CtlFailure::Rpc(ApiError::forbidden_trusted())),
            EXIT_NEEDS_TRUST
        );
        assert_eq!(
            ctl_failure(CtlFailure::Unreachable("gone".into())),
            EXIT_UNREACHABLE
        );
        // Asking for too much at the hello is a refusal, not a silent cut.
        let too_much = Session::open(
            &socket,
            "test",
            &["observe:raw".into()],
            Duration::from_secs(5),
        )
        .err()
        .expect("refused");
        assert!(matches!(too_much, CtlFailure::Rpc(ref e) if e.code == ApiError::FORBIDDEN));
        // The subcommands end to end against the door.
        let data_dir = daemon.api.paths.data.clone();
        for (cmd, expect) in [
            (
                Cmd::Device {
                    cmd: DeviceCmd::Flood { n: 2 },
                },
                0,
            ),
            (
                Cmd::Device {
                    cmd: DeviceCmd::Sync,
                },
                0,
            ),
            (
                Cmd::Device {
                    cmd: DeviceCmd::Metrics,
                },
                0,
            ),
            (
                Cmd::Ctl {
                    scopes: vec!["observe".into()],
                    client: Some("gate".into()),
                    timeout: Some(5.0),
                    method: "state.get".into(),
                    params: Some("{}".into()),
                },
                0,
            ),
            (
                Cmd::Ctl {
                    scopes: vec![],
                    client: None,
                    timeout: None,
                    method: "device.flood".into(),
                    params: Some(r#"{"n":1}"#.into()),
                },
                EXIT_RPC_ERROR,
            ),
            (
                Cmd::Status {
                    json: false,
                    live: true,
                },
                0,
            ),
            (
                Cmd::Status {
                    json: true,
                    live: true,
                },
                0,
            ),
        ] {
            let code = run(Cli {
                data_dir: Some(data_dir.clone()),
                cmd,
            })
            .unwrap();
            assert_eq!(code, expect);
        }
        let d = doctor(&daemon.api.paths);
        assert!(d.checks.iter().any(|(n, ok, detail)| n == "socket"
            && *ok
            && detail.contains("answers session.ping")));
        rt.block_on(served.shutdown());
        drop(rt);
        drop(daemon);
        feeder.join().unwrap();
    }

    /// What `deckd ctl` and every care verb say on stderr when a call fails:
    /// a line for a service that did not answer (exit 2); the JSON-RPC
    /// error as it came, with a line a person can act on for a trust
    /// refusal (exit 3) and for a terminal an agent started (exit 1, the
    /// code it always had); and the error alone for any other refusal.
    #[test]
    fn a_failed_call_says_why_when_a_person_can_act_on_it() {
        let said = |failure| {
            let mut err = Vec::new();
            let code = report(failure, &mut err);
            (code, String::from_utf8(err).unwrap())
        };
        assert_eq!(
            said(CtlFailure::Unreachable("gone".into())),
            (EXIT_UNREACHABLE, "deckd: gone\n".to_string())
        );
        let (code, text) = said(CtlFailure::Rpc(ApiError::forbidden_trusted()));
        assert_eq!(code, EXIT_NEEDS_TRUST);
        assert!(text.ends_with(&format!("\ndeckd: {NEEDS_TRUST_HINT}\n")), "{text}");
        let (code, text) = said(CtlFailure::Rpc(ApiError::forbidden_ancestry("admin")));
        assert_eq!(code, EXIT_RPC_ERROR);
        let lines: Vec<&str> = text.lines().collect();
        let error: serde_json::Value = serde_json::from_str(lines[0]).unwrap();
        assert_eq!(error["data"]["reason"], "agent_ancestry");
        assert_eq!(lines[1..], [format!("deckd: {ANCESTRY_HINT}")]);
        let (code, text) = said(CtlFailure::Rpc(ApiError::forbidden("admin")));
        assert_eq!((code, text.lines().count()), (EXIT_RPC_ERROR, 1), "{text}");
    }

    #[test]
    fn deckd_token_works_end_to_end_against_tokens_toml() {
        let dir = std::env::temp_dir().join(format!("deckd-cli-token-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let p = Paths::from_data_dir(dir.clone(), dir.join("logs"), dir.join("caches"));
        let now = deck_model::Ts(1_789_760_523_000);
        let mut out = Vec::new();
        // create: the plaintext alone on stdout, the hash alone on disk.
        let code = token_cmd(
            &p,
            TokenCmd::Create {
                name: "gate".into(),
                scopes: vec!["observe".into(), "agent,operate".into()],
                expires: Some("30d".into()),
                client: Some("codex".into()),
            },
            &mut out,
            now,
        )
        .unwrap();
        assert_eq!(code, 0);
        let plain = String::from_utf8(out.clone()).unwrap();
        assert_eq!(plain.lines().count(), 1);
        let plain = plain.trim().to_string();
        assert!(plain.starts_with("dk_") && plain.len() == 46, "{plain}");
        let file = std::fs::read_to_string(&p.tokens).unwrap();
        assert!(!file.contains(&plain));
        assert!(file.contains(&crate::tokens::hash_hex(&plain)));
        assert!(file.contains("expires = \"2026-10-18T19:42:03.000Z\""));
        let tokens = crate::tokens::Tokens::load(&p.tokens).unwrap();
        let t = tokens.verify(&plain, now).unwrap();
        assert_eq!(
            t.scopes,
            vec![
                deck_model::Scope::Observe,
                deck_model::Scope::Agent,
                deck_model::Scope::Operate
            ]
        );
        // Refusals: duplicate, bad scope, bad expiry, unknown name. Nothing printed.
        let bad = |cmd: TokenCmd| {
            let mut out = Vec::new();
            let err = token_cmd(&p, cmd, &mut out, now).unwrap_err();
            assert!(out.is_empty());
            err
        };
        assert!(bad(TokenCmd::Create {
            name: "gate".into(),
            scopes: vec!["observe".into()],
            expires: None,
            client: None,
        })
        .contains("already exists"));
        assert!(bad(TokenCmd::Create {
            name: "x".into(),
            scopes: vec!["root".into()],
            expires: None,
            client: None,
        })
        .contains("unknown scope"));
        assert!(bad(TokenCmd::Create {
            name: "x".into(),
            scopes: vec!["observe".into()],
            expires: Some("soon".into()),
            client: None,
        })
        .contains("invalid expiry"));
        assert!(bad(TokenCmd::Revoke {
            name: "nope".into()
        })
        .contains("no token named"));
        assert!(bad(TokenCmd::Rotate {
            name: "nope".into()
        })
        .contains("no token named"));
        // list: JSON without hashes, and the table.
        let mut out = Vec::new();
        token_cmd(&p, TokenCmd::List { json: true }, &mut out, now).unwrap();
        let rows: serde_json::Value = serde_json::from_slice(&out).unwrap();
        assert_eq!(rows[0]["name"], "gate");
        assert_eq!(
            rows[0]["scopes"],
            serde_json::json!(["observe", "agent", "operate"])
        );
        assert_eq!(rows[0]["client"], "codex");
        assert_eq!(rows[0]["embedded_in"], serde_json::json!([]));
        assert!(rows[0].get("hash").is_none());
        assert!(!String::from_utf8_lossy(&out).contains(&plain));
        let mut out = Vec::new();
        token_cmd(&p, TokenCmd::List { json: false }, &mut out, now).unwrap();
        let table = String::from_utf8(out).unwrap();
        assert!(table.starts_with("NAME"));
        assert!(table.contains("gate"));
        assert!(table.contains("observe,agent,operate"));
        assert!(table.contains("codex"));
        assert!(!table.contains(&plain));
        // rotate: a new plaintext, the old one dead, scopes kept.
        let mut out = Vec::new();
        token_cmd(
            &p,
            TokenCmd::Rotate {
                name: "gate".into(),
            },
            &mut out,
            now,
        )
        .unwrap();
        let rotated = String::from_utf8(out).unwrap().trim().to_string();
        assert_ne!(rotated, plain);
        let tokens = crate::tokens::Tokens::load(&p.tokens).unwrap();
        assert!(tokens.verify(&plain, now).is_none());
        assert_eq!(tokens.verify(&rotated, now).unwrap().name, "gate");
        // revoke: gone from the file, nothing on stdout.
        let mut out = Vec::new();
        token_cmd(
            &p,
            TokenCmd::Revoke {
                name: "gate".into(),
            },
            &mut out,
            now,
        )
        .unwrap();
        assert!(out.is_empty());
        assert!(crate::tokens::Tokens::load(&p.tokens)
            .unwrap()
            .list()
            .is_empty());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&p.tokens).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// The rows `doctor` prints for launchd, on whatever this machine has.
    /// The parsing is `crate::launchd`'s and is tested there against the
    /// measured shapes; what this holds is that the daemon's agent always
    /// contributes its own row, and that a **loaded** daemon agent brings the two week-7 rows with it
    /// — the exit meaning and the dangling-program verdict.
    #[test]
    #[cfg(not(windows))] // launchd's rows; Windows has a startup row
    fn the_launchd_rows_always_cover_our_label_and_grow_when_it_is_loaded() {
        let checks = launchd_checks();
        let names: Vec<&str> = checks.iter().map(|(name, _, _)| name.as_str()).collect();
        assert!(names.contains(&"launchd deckd"), "{names:?}");
        if crate::launchd::Agent::load(DECKD_LABEL).is_some() {
            assert!(names.contains(&"launchd deckd last exit"), "{names:?}");
            assert!(names.contains(&"launchd deckd program"), "{names:?}");
        }
        // Every row says something; a blank detail is a row the operator
        // cannot act on.
        for (name, _, detail) in &checks {
            assert!(!detail.is_empty(), "{name} has no detail");
        }
    }

    /// D7: the fix a doctor row names is pasted into a terminal as it is
    /// printed, so the `chmod` for a `tokens.toml` under `Application
    /// Support`, where the data directory lives, quotes the path.
    #[cfg(unix)]
    #[test]
    fn the_tokens_row_names_a_chmod_that_pastes_as_it_is() {
        use std::os::unix::fs::PermissionsExt;
        let dir = crate::integrations::testdir::TestDir::new("doctor-chmod");
        let data = dir.sub("Library/Application Support/OpenWalkieTalkie");
        let paths = Paths::from_data_dir(data.clone(), data.join("logs"), data.join("caches"));
        std::fs::write(&paths.tokens, "").unwrap();
        std::fs::set_permissions(&paths.tokens, std::fs::Permissions::from_mode(0o644)).unwrap();
        let checks = setup_checks(&paths, || None);
        let (_, ok, detail) = checks
            .iter()
            .find(|(name, _, _)| name == "tokens.toml")
            .expect("a tokens.toml row");
        assert!(!ok, "{detail}");
        assert_eq!(
            *detail,
            format!(
                "mode 0644, expected 0600 (chmod 600 '{}')",
                paths.tokens.display()
            )
        );
    }

    // ---- `deckd tts key` and the Keychain (`crate::secrets`) ----------------
    //
    // Every store here is a `MemoryKeyStore`: no test touches the Keychain.

    use crate::audio::cloud::Provider;
    use crate::secrets::{KeyHome, KeyStore, MemoryKeyStore};
    use std::sync::Arc;

    const KEY: &str = "sk-test-0123456789abcd";

    /// A data directory with no service behind its socket.
    fn no_service(dir: &crate::integrations::testdir::TestDir) -> Paths {
        let data = dir.sub("data");
        Paths::from_data_dir(data.clone(), data.join("logs"), data.join("caches"))
    }

    fn text(out: Vec<u8>) -> String {
        mac_words(String::from_utf8(out).unwrap())
    }

    /// `words` with the store named as on a Mac, so one expected line holds
    /// for Credential Manager on Windows too (`crate::secrets::STORE`).
    fn mac_words(words: String) -> String {
        words.replace(crate::secrets::STORE, "the Keychain")
    }

    /// The in-process daemon's doors, served on a runtime of their own; the
    /// CLI under test calls them from this thread.
    fn serve(
        daemon: &crate::api::testkit::TestDaemon,
    ) -> (tokio::runtime::Runtime, crate::api::Served) {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap();
        let served = rt.block_on(crate::api::serve(daemon.api.clone()));
        (rt, served)
    }

    /// With no service to ask, a Mac's own install puts the key straight
    /// into the store: config.toml gets the provider and loses a key it
    /// still held, every other byte kept, and the line says the service
    /// reads the key when it starts. Clearing empties the store. A store
    /// that refuses writes nothing.
    #[test]
    fn with_no_service_the_key_goes_straight_into_the_store() {
        let dir = crate::integrations::testdir::TestDir::new("cli-key-store");
        let paths = no_service(&dir);
        std::fs::write(&paths.config, "# mine\n[voice.tts]\napi_key = \"sk-old\"\n").unwrap();
        let store = Arc::new(MemoryKeyStore::default());
        let home = KeyHome::Store(store.clone());
        let mut out = Vec::new();
        let code = tts_key_set(
            &paths,
            Some(Provider::ElevenLabs),
            Provider::ElevenLabs,
            KEY,
            &home,
            &mut out,
        )
        .unwrap();
        assert_eq!(code, 0);
        assert_eq!(store.current().as_deref(), Some(KEY));
        assert_eq!(
            std::fs::read_to_string(&paths.config).unwrap(),
            "# mine\n[voice.tts]\nprovider = \"elevenlabs\"\n"
        );
        assert_eq!(
            text(out),
            "tts: key \u{2026}abcd (22 chars) stored in the Keychain for elevenlabs; the service \
             reads it when it starts\ntts: the daemon speaks with elevenlabs (eleven_flash_v2_5) \
             from the next announcement; `deckd tts status` to check\n"
        );
        let mut out = Vec::new();
        assert_eq!(tts_key_clear(&paths, &home, &mut out).unwrap(), 0);
        assert_eq!(store.current(), None);
        assert_eq!(text(out), "tts: key removed from the Keychain; apple speaks\n");
        // A store that refuses: an error, and nothing written anywhere.
        store.refuse(Some("locked"));
        let before = std::fs::read_to_string(&paths.config).unwrap();
        let err = tts_key_set(&paths, Some(Provider::OpenAi), Provider::OpenAi, KEY, &home, &mut Vec::new())
            .unwrap_err();
        assert_eq!(mac_words(err), "the Keychain did not take the key: locked");
        assert_eq!(std::fs::read_to_string(&paths.config).unwrap(), before);
    }

    /// A service that took the call and gave no answer (it waits on a
    /// Keychain dialog, it timed out, the connection broke) still holds a
    /// key of its own: the command writes nothing around it, and exits as a
    /// service that did not answer. Only a socket nothing accepts on means
    /// no service.
    #[test]
    #[cfg(unix)] // a Unix socket stands in for the service
    fn a_service_that_does_not_answer_is_never_written_around() {
        use std::sync::atomic::{AtomicBool, Ordering};
        // `sun_path` holds 103 bytes: a short directory of its own.
        let dir = std::env::temp_dir().join(format!("dkk{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let paths = Paths::from_data_dir(dir.clone(), dir.join("logs"), dir.join("caches"));
        let listener = std::os::unix::net::UnixListener::bind(&paths.socket).unwrap();
        listener.set_nonblocking(true).unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let mute = {
            let stop = stop.clone();
            std::thread::spawn(move || {
                // Every connection is taken and dropped unanswered.
                while !stop.load(Ordering::Relaxed) {
                    match listener.accept() {
                        Ok((stream, _)) => drop(stream),
                        Err(_) => std::thread::sleep(std::time::Duration::from_millis(5)),
                    }
                }
            })
        };
        let kept = Arc::new(MemoryKeyStore::with("sk-kept"));
        let home = KeyHome::Store(kept.clone());
        let mut out = Vec::new();
        let cleared = tts_key_clear(&paths, &home, &mut out);
        let set = tts_key_set(&paths, None, Provider::OpenAi, KEY, &home, &mut out);
        stop.store(true, Ordering::Relaxed);
        mute.join().unwrap();
        let config_written = paths.config.exists();
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(cleared, Ok(EXIT_UNREACHABLE));
        assert_eq!(set, Ok(EXIT_UNREACHABLE));
        assert_eq!(
            kept.current().as_deref(),
            Some("sk-kept"),
            "nothing deleted"
        );
        assert_eq!(kept.writes(), 0, "nothing written");
        assert!(!config_written, "config.toml was not written");
        assert!(out.is_empty(), "{}", text(out));
    }

    /// Off a Mac's own install (Linux, a data directory of its own) the key
    /// is written to config.toml as it always was.
    #[test]
    fn with_no_service_and_no_store_the_key_goes_to_config_toml() {
        let dir = crate::integrations::testdir::TestDir::new("cli-key-file");
        let paths = no_service(&dir);
        let mut out = Vec::new();
        tts_key_set(&paths, None, Provider::OpenAi, KEY, &KeyHome::File, &mut out).unwrap();
        let file = Config::load(&paths.config).unwrap();
        assert_eq!(file.voice.tts.api_key, KEY);
        assert!(
            text(out).starts_with(&format!(
                "tts: key \u{2026}abcd (22 chars) stored in {} for openai\n",
                paths.config.display()
            )),
        );
        let mut out = Vec::new();
        tts_key_clear(&paths, &KeyHome::File, &mut out).unwrap();
        assert!(!Config::load(&paths.config).unwrap().voice.tts.has_key());
        assert_eq!(
            text(out),
            format!("tts: key removed from {}; apple speaks\n", paths.config.display())
        );
    }

    /// With a service to ask, the key goes through `config.set`: it lands in
    /// the service's store (never in config.toml, and never in the store
    /// this process would have used), and the line says where it is.
    #[test]
    fn with_a_service_the_key_goes_through_config_set_into_its_store() {
        use crate::trust::FakeVerifier;
        let kept = Arc::new(MemoryKeyStore::default());
        let daemon =
            crate::api::testkit::test_api_keyed("cli-key", FakeVerifier::default(), kept.clone());
        let (rt, served) = serve(&daemon);
        let paths = daemon.api.paths.clone();
        let mine = Arc::new(MemoryKeyStore::default());
        let mut out = Vec::new();
        let code =
            tts_key_set(&paths, None, Provider::OpenAi, KEY, &KeyHome::Store(mine.clone()), &mut out)
                .unwrap();
        assert_eq!(code, 0);
        assert_eq!(kept.current().as_deref(), Some(KEY));
        assert_eq!(mine.writes(), 0, "the service wrote it, not this process");
        assert!(!Config::load(&paths.config).unwrap().voice.tts.has_key());
        assert_eq!(
            text(out),
            "tts: key \u{2026}abcd (22 chars) stored in the Keychain for openai\ntts: the daemon \
             speaks with openai (gpt-4o-mini-tts) from the next announcement; `deckd tts status` \
             to check\n"
        );
        // `deckd tts status` reads it back from the service.
        let mut out = Vec::new();
        tts_cmd(&paths, TtsCmd::Status, &KeyHome::Store(mine.clone()), &mut out).unwrap();
        let status = text(out);
        assert!(
            status.contains("engine:  [voice.tts] engine = \"cloud\" → openai's model speaks (your key)"),
            "{status}"
        );
        assert!(status.contains("key:     \u{2026}abcd (22 chars), in the Keychain\n"), "{status}");
        assert_eq!(mine.reads(), 0, "the service answered; this process asked no store");
        let mut out = Vec::new();
        tts_key_clear(&paths, &KeyHome::Store(mine.clone()), &mut out).unwrap();
        assert_eq!(kept.current(), None);
        assert_eq!(text(out), "tts: key removed from the Keychain; apple speaks\n");
        rt.block_on(served.shutdown());
        drop(rt);
        drop(daemon);
    }

    /// A process that Claude Code or Codex started gets no admin session, so
    /// the key is not changed through the service, and it is not written
    /// around it either.
    #[test]
    fn an_agents_terminal_is_refused_and_nothing_is_written() {
        use crate::trust::FakeVerifier;
        let kept = Arc::new(MemoryKeyStore::with("sk-kept"));
        let capped = FakeVerifier {
            capped: true,
            ..FakeVerifier::default()
        };
        let daemon = crate::api::testkit::test_api_keyed("cli-key-capped", capped, kept.clone());
        let (rt, served) = serve(&daemon);
        let paths = daemon.api.paths.clone();
        let mine = Arc::new(MemoryKeyStore::default());
        let home = KeyHome::Store(mine.clone());
        let err = tts_key_set(&paths, None, Provider::OpenAi, KEY, &home, &mut Vec::new())
            .unwrap_err();
        assert_eq!(err, KEY_ANCESTRY_HINT);
        let err = tts_key_clear(&paths, &home, &mut Vec::new()).unwrap_err();
        assert_eq!(err, KEY_ANCESTRY_HINT);
        assert_eq!(kept.current().as_deref(), Some("sk-kept"));
        assert_eq!(mine.writes(), 0);
        assert!(!paths.config.exists(), "config.toml was not written");
        rt.block_on(served.shutdown());
        drop(rt);
        drop(daemon);
    }

    /// With no service to ask, `deckd tts status` says whether the store
    /// holds a key without reading it, and what speaks accordingly.
    #[test]
    fn with_no_service_the_status_reads_the_store_without_its_secret() {
        let dir = crate::integrations::testdir::TestDir::new("cli-key-status");
        let paths = no_service(&dir);
        let status = |home: &KeyHome| {
            let mut out = Vec::new();
            tts_cmd(&paths, TtsCmd::Status, home, &mut out).unwrap();
            text(out)
        };
        let store = Arc::new(MemoryKeyStore::with(KEY));
        let held = status(&KeyHome::Store(store.clone()));
        assert!(held.contains("→ openai's model speaks (your key)"), "{held}");
        assert!(
            held.contains(
                "key:     in the Keychain (start the service to see its last four characters)\n"
            ),
            "{held}"
        );
        assert!(!held.contains("abcd"), "{held}");
        store.set("").unwrap();
        let none = status(&KeyHome::Store(store.clone()));
        assert!(none.contains(&format!("→ {SYSTEM_SYNTH} speaks, on {THIS_COMPUTER}")), "{none}");
        assert!(
            none.contains("key:     (none) — `deckd tts key set --provider openai|elevenlabs`\n"),
            "{none}"
        );
        // A key still in config.toml is the one shown, store or not.
        std::fs::write(&paths.config, format!("[voice.tts]\napi_key = \"{KEY}\"\n")).unwrap();
        let file = status(&KeyHome::File);
        assert!(file.contains("key:     \u{2026}abcd (22 chars)\n"), "{file}");
    }

    /// `deckd uninstall --dry-run` names the Keychain item only where a
    /// purge would delete it: a store home, with `--purge`.
    #[test]
    fn the_uninstall_preview_names_the_keychain_item_only_for_a_purge_of_a_store() {
        let dir = crate::integrations::testdir::TestDir::new("cli-uninstall-preview");
        let env = crate::integrations::test_env(&dir);
        let paths = Paths::from_data_dir(env.data.clone(), dir.sub("logs"), dir.sub("caches"));
        let line =
            "  - PURGE the cloud voice key from the Keychain (ai.cohdl.openwalkietalkie), if one is there\n";
        let store = KeyHome::Store(Arc::new(MemoryKeyStore::with(KEY)));
        assert!(mac_words(uninstall_preview(&env, &paths, true, &store)).contains(line));
        let store_named = crate::secrets::STORE;
        assert!(!uninstall_preview(&env, &paths, false, &store).contains(store_named));
        assert!(!uninstall_preview(&env, &paths, true, &KeyHome::File).contains(store_named));
    }
}
