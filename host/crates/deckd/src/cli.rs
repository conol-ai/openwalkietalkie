//! The `deckd` command line (design §3, contract §3.8): `run`, `status`,
//! `ctl`, `events`, `token`, `device`, `doctor`, `ble` (the voice link's
//! bench tools, which talk to the deck directly and need no daemon). Every
//! command that talks to the running daemon (`ctl`, `events`, `status --live`, `device
//! flood|sync|metrics`, the `doctor` socket probe) goes through
//! `deck-client` over the UDS door; `token` edits `tokens.toml` directly.
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

/// One call on a fresh session (`deckd device …`, `status --live`).
fn uds_call(
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
    match failure {
        CtlFailure::Unreachable(why) => {
            eprintln!("deckd: {why}");
            EXIT_UNREACHABLE
        }
        CtlFailure::Rpc(error) => {
            eprintln!(
                "{}",
                serde_json::to_string(&error).unwrap_or_else(|_| error.to_string())
            );
            if error.code == ApiError::FORBIDDEN && error.data["need"] == "trusted" {
                eprintln!("deckd: {NEEDS_TRUST_HINT}");
                EXIT_NEEDS_TRUST
            } else {
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

#[derive(Parser, Debug)]
#[command(name = "deckd", version, about = "OpenWalkieTalkie host service")]
pub struct Cli {
    /// Data directory (default: ~/Library/Application Support/OpenWalkieTalkie, or $DECKD_DATA_DIR).
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
    /// Device debugging.
    Device {
        #[command(subcommand)]
        cmd: DeviceCmd,
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
    /// The cloud provider's API key in `[voice.tts]`: with one, the
    /// announcements are spoken by the provider's model; without, by
    /// Apple's synthesiser on this Mac.
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
    /// Store the key in config.toml (`[voice.tts] api_key`). It is read
    /// from the terminal without echo, or from stdin when piped; the
    /// running daemon applies it within a second, no restart.
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
}

pub fn main() -> i32 {
    let cli = Cli::parse();
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
        // The bench tools talk to the deck, not to the data directory.
        Cmd::Ble { cmd } => return Ok(ble_cmd(cmd)),
        other => other,
    };
    let paths = paths(data_dir)?;
    match cmd {
        Cmd::ClaudeChannel => crate::claude_channel::main(&paths.data),
        // Answered above, where a missing data directory is a missing
        // socket rather than an error exit; kept so the match is total.
        Cmd::Hook { cli, rest } => Ok(crate::hook::main(&cli, &rest, Some(paths.socket))),
        Cmd::Ble { cmd } => Ok(ble_cmd(cmd)),
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
            tts_cmd(&paths, cmd, &mut out)
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
        },
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
            if dry_run {
                println!("{}", uninstall_preview(&env, &paths, purge));
                return Ok(0);
            }
            if purge && !yes && !confirm_purge(&paths)? {
                println!("nothing was changed");
                return Ok(0);
            }
            let outcome = crate::uninstall::run(&env, &paths, crate::uninstall::Options { purge });
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

/// "What this removes", before anything is removed — the same sentence the
/// installer's `--dry-run` gives, for the command that cannot be undone.
pub fn uninstall_preview(env: &crate::integrations::Env, paths: &Paths, purge: bool) -> String {
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
        let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .map_err(|e| format!("cannot listen for SIGTERM: {e}"))?;
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
        let home = std::env::var_os("HOME").map(PathBuf::from)?;
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
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cache/huggingface/hub")))?;
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

/// `deckd tts key …` / `deckd tts install` / `deckd tts status`.
pub fn tts_cmd(paths: &Paths, cmd: TtsCmd, out: &mut dyn std::io::Write) -> Result<i32, String> {
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
            let mut patch = vec![("voice.tts.api_key", serde_json::Value::String(key))];
            if let Some(provider) = provider {
                patch.push(("voice.tts.provider", serde_json::Value::String(provider.name().into())));
            }
            let next = write_tts_keys(paths, &patch)?;
            let _ = writeln!(
                out,
                "tts: key {} stored in {} for {}",
                next.voice.tts.api_key_hint(),
                paths.config.display(),
                effective.name()
            );
            if next.voice.tts.engine != cloud::NAME {
                let _ = writeln!(
                    out,
                    "tts: [voice.tts] engine is {:?}: the key is not used until it is \"cloud\" (the default)",
                    next.voice.tts.engine
                );
            } else {
                let _ = writeln!(
                    out,
                    "tts: the daemon speaks with {} ({}) from the next announcement; `deckd tts status` to check",
                    effective.name(),
                    if next.voice.tts.model.is_empty() {
                        effective.default_model()
                    } else {
                        &next.voice.tts.model
                    }
                );
            }
            Ok(0)
        }
        TtsCmd::Key { cmd: KeyCmd::Clear } => {
            let next = write_tts_keys(paths, &[("voice.tts.api_key", serde_json::Value::Null)])?;
            let _ = writeln!(
                out,
                "tts: key removed from {}; {} speaks",
                paths.config.display(),
                next.voice.tts.effective_engine()
            );
            Ok(0)
        }
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
            let _ = writeln!(out, "tts: Apple's synthesiser speaks from the next utterance");
            Ok(0)
        }
        TtsCmd::Status => {
            let tts = &config.voice.tts;
            let kokoro_present = crate::kokoro::present(&paths.kokoro, &tts.model);
            let _ = writeln!(
                out,
                "engine:  [voice.tts] engine = {:?} → {}",
                tts.engine,
                match tts.effective_engine() {
                    "apple" => "Apple's synthesiser speaks, on this Mac".to_string(),
                    sidecar::NAME => "the local model in its sidecar speaks".to_string(),
                    crate::kokoro::ENGINE if kokoro_present => "the Kokoro voice speaks, on this Mac".to_string(),
                    crate::kokoro::ENGINE => "Apple's synthesiser, until the Kokoro voice is downloaded".to_string(),
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
            if tts.engine == cloud::NAME {
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
                let _ = writeln!(
                    out,
                    "key:     {}{}",
                    tts.api_key_hint(),
                    if tts.has_key() { "" } else { " — `deckd tts key set --provider openai|elevenlabs`" }
                );
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
                "\nstatus.json: {} s ago{}\nlink: {:?}, session {:?}, sid {}, fw {}, boot {}, face {}, {} faces, {} agents\npace {} ms{}, outbox depth {}, reports tx {} rx {}, hello tx {} rx {} misses {}\nlink drops {}, barriers {}, device err {} (size {} parse {} range {} unknown {}), decode err {}, encode err {}\nflood {}/{}, ch2 reports {}",
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
            ));
        }
        None => out.push_str("\nstatus.json: absent (the daemon has not run here)"),
    }
    out
}

#[derive(Debug, serde::Serialize)]
pub struct Doctor {
    pub checks: Vec<(String, bool, String)>,
}

/// A stray `~/Library/LaunchAgents/<label>.plist`: what `deckd install
/// --dev` writes, and what an SMAppService registration leaves behind only
/// as a file when the bundle is gone.
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
                                "not loaded, but {} is on disk and names {} — `launchctl \
                                 bootstrap gui/{} {}` would start it",
                                plist.display(),
                                program.display(),
                                crate::launchd::uid(),
                                plist.display()
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

fn doctor(paths: &Paths) -> Doctor {
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
    check(
        "tcp",
        match std::fs::read_to_string(&paths.port)
            .ok()
            .and_then(|s| s.trim().parse::<u16>().ok())
        {
            None if !running => Ok("no port file (the daemon is not running)".into()),
            None => Err(
                "port file empty while the service runs: the tcp door is degraded (port in use?)"
                    .into(),
            ),
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
        },
    );
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
                #[cfg(not(unix))]
                let mode = 0o600;
                let count = crate::tokens::Tokens::load(&paths.tokens)
                    .map(|t| t.list().len())
                    .map_err(|e| e.to_string());
                match (mode, count) {
                    (0o600, Ok(n)) => Ok(format!("mode 0600, {n} token(s)")),
                    (0o600, Err(e)) => Err(e),
                    (mode, _) => Err(format!(
                        "mode {mode:04o}, expected 0600 (chmod 600 {})",
                        paths.tokens.display()
                    )),
                }
            }
        },
    );
    check(
        "device",
        crate::hid::probe(|api| {
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
        }),
    );
    checks.extend(launchd_checks());
    Doctor { checks }
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
        assert!(d.checks.iter().any(|(n, _, _)| n == "launchd deckd"));
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
}
