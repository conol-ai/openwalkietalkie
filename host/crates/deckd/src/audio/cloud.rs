//! `[voice.tts] engine = "cloud"`: a hosted text-to-speech model behind the
//! operator's **own API key** (bring your own key), with Apple's synthesiser
//! speaking whenever there is no key or the provider will not — behind
//! §7.6's `TtsEngine` seam like the other two.
//!
//! Two providers, chosen by `[voice.tts] provider`: OpenAI's
//! `/v1/audio/speech` (`gpt-4o-mini-tts` by default) and ElevenLabs'
//! `/v1/text-to-speech/{voice}/stream` (`eleven_flash_v2_5`). Both stream
//! raw 24 kHz 16-bit mono PCM, which goes to `deckd-tts` chunk by chunk as it
//! arrives, so playback starts after the first chunk and not after the
//! whole sentence. Barge-in kills the transfer.
//!
//! **The request goes through `/usr/bin/curl`**, the way the usage probe's
//! does (`usage::claude`): the daemon has no TLS stack of its own and curl
//! is on every Mac. The key travels in a config file on curl's stdin
//! (`-K -`), never on its command line, so `ps` does not show it; the
//! response headers are asked for on stdout (`--include`) ahead of the
//! body, so the status is known — and a refusal is handed to the fallback —
//! before a single byte of audio is read.
//!
//! **Settings are read at every utterance** from a cell the core refreshes
//! on each config reload ([`Settings`]), so a key pasted into `config.toml`
//! or written by `deckd tts key set` takes effect at the next announcement
//! and no `[voice.tts]` key of the cloud engine needs a restart.
//!
//! **A refusal cools the engine down**: after a 4xx/5xx, a network failure
//! or a stalled start, the fallback speaks for [`COOLDOWN`] rather than
//! every announcement paying the round trip to be refused again. A changed
//! key ends the cooldown at once.
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use arc_swap::ArcSwap;
use crossbeam_channel::{Receiver, Sender};

use super::sidecar::SidecarTts;
use super::traits::{
    AudioError, AudioFormat, EngineInfo, TtsChunk, TtsEngine, TtsJob, TtsRequest, TtsStop,
    VoiceInfo,
};

/// The engine's name in `[voice.tts] engine`.
pub const NAME: &str = "cloud";
/// What both providers are asked for: raw 16-bit mono at this rate.
pub const RATE: u32 = 24_000;
/// After a refused or failed request the fallback speaks for this long.
pub const COOLDOWN: Duration = Duration::from_secs(60);
/// How long `synthesize` waits for the response status before giving the
/// utterance to the fallback (the provider's time to first byte is well
/// under a second; this is a dead network).
const STATUS_WAIT: Duration = Duration::from_secs(6);
/// How much of a refusal's body is kept for the log.
const ERROR_BODY_MAX: usize = 16 * 1024;
/// What `voice.engines` reports both providers speak; each covers more.
const LOCALES: [&str; 12] = [
    "en-US", "en-GB", "zh-CN", "ja-JP", "ko-KR", "de-DE", "fr-FR", "es-ES", "it-IT", "pt-BR",
    "ru-RU", "nl-NL",
];

/// OpenAI's voices for `gpt-4o-mini-tts`, so a `[voice.voices]
/// default_pool` name that *is* one of them is used as such.
pub const OPENAI_VOICES: [&str; 11] = [
    "alloy", "ash", "ballad", "coral", "echo", "fable", "nova", "onyx", "sage", "shimmer", "verse",
];

/// Whose model and key.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Provider {
    OpenAi,
    ElevenLabs,
}

impl Provider {
    /// `[voice.tts] provider`, forgiving about case and the odd spelling.
    #[must_use]
    pub fn parse(name: &str) -> Option<Provider> {
        match name.trim().to_ascii_lowercase().as_str() {
            "openai" | "open-ai" => Some(Provider::OpenAi),
            "elevenlabs" | "eleven-labs" | "eleven" | "11labs" => Some(Provider::ElevenLabs),
            _ => None,
        }
    }

    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Provider::OpenAi => "openai",
            Provider::ElevenLabs => "elevenlabs",
        }
    }

    /// The model used when `[voice.tts] model` is empty.
    #[must_use]
    pub const fn default_model(self) -> &'static str {
        match self {
            Provider::OpenAi => "gpt-4o-mini-tts",
            Provider::ElevenLabs => "eleven_flash_v2_5",
        }
    }

    /// The voices agents are assigned from when `[voice.tts] voices` names
    /// none: four distinct ones. ElevenLabs' are the names of its premade
    /// library, resolved to ids through the account's voice list.
    #[must_use]
    pub const fn default_voices(self) -> &'static [&'static str] {
        match self {
            Provider::OpenAi => &["coral", "ash", "sage", "echo"],
            Provider::ElevenLabs => &["Sarah", "George", "Laura", "Brian"],
        }
    }

    /// Where the key goes: the one header the provider authenticates by.
    fn auth_header(self, key: &str) -> String {
        match self {
            Provider::OpenAi => format!("Authorization: Bearer {key}"),
            Provider::ElevenLabs => format!("xi-api-key: {key}"),
        }
    }
}

/// What the engine reads at every utterance: `[voice.tts]` as the core
/// last saw it. Built by [`Settings::from_config`], swapped into the cell
/// on every reload.
#[derive(Clone, PartialEq)]
pub struct Settings {
    /// `[voice.tts] engine == "cloud"`. False makes the engine a pass-through
    /// to the fallback (that is how `engine = "apple"` is honoured without
    /// a rebuild).
    pub enabled: bool,
    /// `None` for a provider name this build does not know.
    pub provider: Option<Provider>,
    /// The name as configured, for the log when it is unknown.
    pub provider_name: String,
    /// Trimmed; empty means no key.
    pub api_key: String,
    /// Empty is the provider's default.
    pub model: String,
    /// Empty is the provider's default pool.
    pub voices: Vec<String>,
    /// `"auto"` sends none.
    pub lang: String,
    pub instruct: Option<String>,
}

impl Default for Settings {
    fn default() -> Self {
        Settings::from_tts(&crate::config::Tts::default())
    }
}

impl std::fmt::Debug for Settings {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Settings")
            .field("enabled", &self.enabled)
            .field("provider", &self.provider_name)
            .field("api_key", &if self.api_key.is_empty() { "(none)" } else { "(set)" })
            .field("model", &self.model)
            .field("voices", &self.voices)
            .field("lang", &self.lang)
            .field("instruct", &self.instruct)
            .finish()
    }
}

impl Settings {
    #[must_use]
    pub fn from_config(config: &crate::config::Config) -> Settings {
        Settings::from_tts(&config.voice.tts)
    }

    #[must_use]
    pub fn from_tts(tts: &crate::config::Tts) -> Settings {
        Settings {
            enabled: tts.engine == NAME,
            provider: Provider::parse(&tts.provider),
            provider_name: tts.provider.clone(),
            api_key: tts.api_key.trim().to_string(),
            model: tts.model.trim().to_string(),
            voices: tts
                .voices
                .iter()
                .map(|v| v.trim().to_string())
                .filter(|v| !v.is_empty())
                .collect(),
            lang: tts.lang.trim().to_string(),
            instruct: tts.instruct.clone().filter(|i| !i.trim().is_empty()),
        }
    }

    /// Whether the cloud speaks at all: enabled, a provider this build
    /// knows, and a key.
    #[must_use]
    pub fn active(&self) -> bool {
        self.enabled && self.provider.is_some() && !self.api_key.is_empty()
    }

    /// The model to send.
    #[must_use]
    pub fn model(&self, provider: Provider) -> &str {
        if self.model.is_empty() {
            provider.default_model()
        } else {
            &self.model
        }
    }

    /// The pool in the provider's names: `[voice.tts] voices` or the
    /// provider's default four.
    #[must_use]
    pub fn pool(&self, provider: Provider) -> Vec<String> {
        if self.voices.is_empty() {
            provider
                .default_voices()
                .iter()
                .map(|v| (*v).to_string())
                .collect()
        } else {
            self.voices.clone()
        }
    }

    /// The key's fingerprint: enough to tell two keys apart in the log and
    /// in the voice cache, never the key.
    #[must_use]
    pub fn key_id(&self) -> String {
        if self.api_key.is_empty() {
            return String::new();
        }
        blake3::hash(self.api_key.as_bytes()).to_hex()[..8].to_string()
    }

    /// What `voice.engines` reports when the cloud speaks: the provider by
    /// name, and not on this device. `None` when it does not.
    #[must_use]
    pub fn engine_info(&self) -> Option<EngineInfo> {
        if !self.active() {
            return None;
        }
        Some(EngineInfo {
            name: self.provider?.name().into(),
            on_device: false,
            locales: LOCALES.iter().map(|l| (*l).to_string()).collect(),
        })
    }
}

// ---------------------------------------------------------------------------
// The request, as curl is told it
// ---------------------------------------------------------------------------

/// One HTTP request, before it becomes curl's config.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Request {
    pub url: String,
    /// `Name: value` lines; the auth header is one of them.
    pub headers: Vec<String>,
    /// `None` is a GET.
    pub body: Option<String>,
}

/// A value inside the double quotes of a curl config line: curl's own
/// escapes are `\\` and `\"` (and `\t \n \r \v`), and a backslash before
/// anything else is dropped — so the two that matter are escaped and the
/// rest passes through, UTF-8 included.
fn quoted(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 2);
    out.push('"');
    for ch in value.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' | '\r' => out.push(' '),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// The config file on curl's stdin (`-K -`): everything about the request,
/// and how the answer is wanted — the headers first on stdout
/// (`include`), the body unbuffered behind them, errors on stderr, no
/// `Expect: 100-continue` round trip, and timeouts that end a dead
/// transfer rather than a slow one.
#[must_use]
pub fn curl_config(req: &Request) -> String {
    let mut cfg = String::new();
    cfg.push_str(&format!("url = {}\n", quoted(&req.url)));
    for header in &req.headers {
        cfg.push_str(&format!("header = {}\n", quoted(header)));
    }
    cfg.push_str("header = \"Expect:\"\n");
    if let Some(body) = &req.body {
        cfg.push_str("header = \"Content-Type: application/json\"\n");
        cfg.push_str(&format!("data-binary = {}\n", quoted(body)));
    }
    cfg.push_str(&format!(
        "user-agent = \"openwalkietalkie-deckd/{}\"\n",
        env!("CARGO_PKG_VERSION")
    ));
    cfg.push_str(
        "include\n\
         no-buffer\n\
         silent\n\
         show-error\n\
         connect-timeout = 5\n\
         max-time = 120\n\
         speed-time = 15\n\
         speed-limit = 1\n",
    );
    cfg
}

/// The synthesis request for `text` in `voice` (a name or id the provider
/// knows). `rate` is AVSpeech's, 0.5 being normal.
#[must_use]
pub fn speech_request(settings: &Settings, provider: Provider, text: &str, voice: &str, rate: f32) -> Request {
    let model = settings.model(provider);
    let speed = (rate / 0.5).clamp(0.25, 4.0);
    let speed_asked = (rate - 0.5).abs() > 0.05;
    match provider {
        Provider::OpenAi => {
            let mut body = serde_json::json!({
                "model": model,
                "input": text,
                "voice": voice,
                "response_format": "pcm",
            });
            // Only the instructable models take `instructions`; `tts-1` and
            // `tts-1-hd` refuse the field.
            if let Some(instruct) = settings.instruct.as_deref().filter(|_| !model.starts_with("tts-1")) {
                body["instructions"] = serde_json::Value::String(instruct.into());
            }
            if speed_asked {
                body["speed"] = serde_json::json!(speed);
            }
            Request {
                url: "https://api.openai.com/v1/audio/speech".into(),
                headers: vec![provider.auth_header(&settings.api_key)],
                body: Some(body.to_string()),
            }
        }
        Provider::ElevenLabs => {
            let mut body = serde_json::json!({
                "text": text,
                "model_id": model,
            });
            // `language_code` is for the models that take one (`v2_5`,
            // `v3`); `eleven_multilingual_v2` refuses it. An `en-US` locale
            // is sent as ISO 639-1 `en`.
            let takes_lang = !model.contains("multilingual_v2") && !model.contains("monolingual");
            if takes_lang && !settings.lang.is_empty() && !settings.lang.eq_ignore_ascii_case("auto") {
                let code = settings.lang.split(['-', '_']).next().unwrap_or("").to_ascii_lowercase();
                if !code.is_empty() {
                    body["language_code"] = serde_json::Value::String(code);
                }
            }
            if speed_asked {
                body["voice_settings"] = serde_json::json!({ "speed": speed.clamp(0.7, 1.2) });
            }
            Request {
                url: format!(
                    "https://api.elevenlabs.io/v1/text-to-speech/{voice}/stream?output_format=pcm_{RATE}"
                ),
                headers: vec![provider.auth_header(&settings.api_key)],
                body: Some(body.to_string()),
            }
        }
    }
}

/// ElevenLabs' voice list, for resolving names to ids.
#[must_use]
pub fn voices_request(settings: &Settings) -> Request {
    Request {
        url: "https://api.elevenlabs.io/v2/voices?page_size=100".into(),
        headers: vec![Provider::ElevenLabs.auth_header(&settings.api_key)],
        body: None,
    }
}

// ---------------------------------------------------------------------------
// Running curl
// ---------------------------------------------------------------------------

/// `HTTP/x <status>` lines and a blank line, consumed from the front of
/// `buf` as they complete. `Ok(Some(status))` for a final (non-1xx) status
/// whose header block is complete, `Ok(None)` for more bytes needed, `Err`
/// for something that is not an HTTP answer.
fn take_status(buf: &mut Vec<u8>) -> Result<Option<u16>, String> {
    loop {
        let Some(end) = find(buf, b"\r\n\r\n").map(|i| (i, 4)).or_else(|| find(buf, b"\n\n").map(|i| (i, 2))) else {
            // Not an HTTP answer at all, and we can already tell.
            if buf.len() >= 5 && !buf.starts_with(b"HTTP/") {
                return Err(format!("not an HTTP answer: {}", String::from_utf8_lossy(&buf[..buf.len().min(80)])));
            }
            return Ok(None);
        };
        let block = String::from_utf8_lossy(&buf[..end.0]).into_owned();
        buf.drain(..end.0 + end.1);
        let status = block
            .lines()
            .next()
            .and_then(|line| line.split_whitespace().nth(1))
            .and_then(|code| code.parse::<u16>().ok())
            .ok_or_else(|| format!("no status line in {block:?}"))?;
        if (100..200).contains(&status) {
            continue;
        }
        return Ok(Some(status));
    }
}

fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

/// A refusal's body as one line for the log: the provider's message when
/// it is JSON, else the text, trimmed.
fn error_text(status: u16, body: &[u8]) -> String {
    let text = String::from_utf8_lossy(body);
    let message = serde_json::from_str::<serde_json::Value>(text.trim())
        .ok()
        .and_then(|v| {
            // OpenAI: {"error":{"message"}}; ElevenLabs: {"detail":{"message"}}
            // or {"detail":"…"} or {"detail":[{"msg"}]}.
            let candidates = [
                v.pointer("/error/message"),
                v.pointer("/detail/message"),
                v.pointer("/detail"),
                v.pointer("/detail/0/msg"),
                v.pointer("/message"),
            ];
            candidates
                .into_iter()
                .flatten()
                .find_map(|m| m.as_str().map(str::to_string))
        })
        .unwrap_or_else(|| text.trim().chars().take(200).collect());
    format!("HTTP {status}: {message}")
}

/// Raw s16le → the seam's f32, carrying an odd trailing byte into the next
/// call.
fn pcm_f32(bytes: &[u8], carry: &mut Vec<u8>) -> Vec<f32> {
    carry.extend_from_slice(bytes);
    let whole = carry.len() / 2 * 2;
    let out: Vec<f32> = carry[..whole]
        .as_chunks::<2>()
        .0
        .iter()
        .map(|p| f32::from(i16::from_le_bytes(*p)) / 32768.0)
        .collect();
    carry.drain(..whole);
    out
}

/// The barge-in handle: the transfer's process.
struct Transfer {
    child: Mutex<Option<Child>>,
}

impl Transfer {
    fn kill(&self) {
        if let Some(mut child) = self.child.lock().unwrap().take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }

    /// The transfer ended on its own: reap it.
    fn reap(&self) {
        if let Some(mut child) = self.child.lock().unwrap().take() {
            let _ = child.wait();
        }
    }
}

impl TtsStop for Transfer {
    fn stop(&self) {
        self.kill();
    }
}

/// Start `curl` on `req`. The status comes back on `status` once the
/// header block is in (or the failure, when it never is); after a 2xx the
/// body streams to `chunks` as PCM, ending with [`TtsChunk::End`]. A
/// refusal's body is in the `Err`.
fn start(curl: &Path, req: &Request, status: Sender<Result<u16, String>>, chunks: Sender<TtsChunk>) -> Result<Arc<Transfer>, String> {
    let mut child = Command::new(curl)
        .arg("-K")
        .arg("-")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("{}: {e}", curl.display()))?;
    let mut stdin = child.stdin.take().ok_or("curl: no stdin")?;
    let stdout = child.stdout.take().ok_or("curl: no stdout")?;
    let stderr = child.stderr.take().ok_or("curl: no stderr")?;
    let config = curl_config(req);
    // The config is small; a curl that has already died makes this an
    // error, which the reader reports with curl's stderr.
    let _ = stdin.write_all(config.as_bytes());
    drop(stdin);
    let transfer = Arc::new(Transfer {
        child: Mutex::new(Some(child)),
    });
    let errors = std::thread::Builder::new()
        .name("deckd-tts-curl-err".into())
        .spawn(move || {
            let mut text = String::new();
            let _ = std::io::BufReader::new(stderr).read_to_string(&mut text);
            text
        })
        .map_err(|e| format!("curl stderr thread: {e}"))?;
    let reader = transfer.clone();
    std::thread::Builder::new()
        .name("deckd-tts-curl".into())
        .spawn(move || {
            read_answer(stdout, &status, &chunks, &reader);
            let text = errors.join().unwrap_or_default();
            let text = text.trim();
            if !text.is_empty() {
                tracing::debug!(curl = text, "cloud tts: curl said");
            }
            reader.reap();
        })
        .map_err(|e| format!("curl reader thread: {e}"))?;
    Ok(transfer)
}

/// The reader thread: the header block, then the body as PCM.
fn read_answer(mut stdout: std::process::ChildStdout, status: &Sender<Result<u16, String>>, chunks: &Sender<TtsChunk>, transfer: &Arc<Transfer>) {
    let mut buf = vec![0u8; 16 * 1024];
    let mut head: Vec<u8> = Vec::new();
    let mut code: Option<u16> = None;
    let mut carry: Vec<u8> = Vec::new();
    let mut refused: Vec<u8> = Vec::new();
    loop {
        let n = match stdout.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => n,
            Err(e) => {
                if code.is_none() {
                    let _ = status.send(Err(format!("curl: {e}")));
                }
                break;
            }
        };
        let mut bytes = &buf[..n];
        if code.is_none() {
            head.extend_from_slice(bytes);
            match take_status(&mut head) {
                Ok(None) => continue,
                Ok(Some(c)) => {
                    code = Some(c);
                    bytes = &[];
                    if (200..300).contains(&c) {
                        let _ = status.send(Ok(c));
                        let pcm = pcm_f32(&std::mem::take(&mut head), &mut carry);
                        if !pcm.is_empty() && chunks.send(TtsChunk::Pcm(pcm)).is_err() {
                            transfer.kill();
                            return;
                        }
                    } else {
                        refused.append(&mut head);
                    }
                }
                Err(why) => {
                    let _ = status.send(Err(why));
                    transfer.kill();
                    return;
                }
            }
        }
        match code {
            Some(c) if (200..300).contains(&c) => {
                let pcm = pcm_f32(bytes, &mut carry);
                if !pcm.is_empty() && chunks.send(TtsChunk::Pcm(pcm)).is_err() {
                    // Nobody is listening any more (barge-in, or the job
                    // was dropped): end the transfer rather than drain it.
                    transfer.kill();
                    return;
                }
            }
            Some(_) if refused.len() < ERROR_BODY_MAX => {
                refused.extend_from_slice(bytes);
            }
            Some(_) | None => {}
        }
    }
    match code {
        Some(c) if (200..300).contains(&c) => {
            let _ = chunks.send(TtsChunk::End);
        }
        Some(c) => {
            let _ = status.send(Err(error_text(c, &refused)));
        }
        None => {
            let _ = status.send(Err("curl: no answer".into()));
        }
    }
}

/// Run a request to completion (the voice list): the status and body.
/// curl's own `max-time` bounds it.
fn call(curl: &Path, req: &Request) -> Result<(u16, Vec<u8>), String> {
    let mut child = Command::new(curl)
        .arg("-K")
        .arg("-")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("{}: {e}", curl.display()))?;
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(curl_config(req).as_bytes());
    }
    let out = child.wait_with_output().map_err(|e| format!("curl: {e}"))?;
    let mut body = out.stdout;
    match take_status(&mut body) {
        Ok(Some(status)) => Ok((status, body)),
        Ok(None) => Err(format!(
            "curl: no answer: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        )),
        Err(why) => Err(why),
    }
}

// ---------------------------------------------------------------------------
// The engine
// ---------------------------------------------------------------------------

/// One ElevenLabs voice as the list reports it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ElevenVoice {
    pub id: String,
    pub name: String,
    pub premade: bool,
}

/// `{voices:[{voice_id, name, category}]}` → the list. A trailing odd byte
/// (from the PCM round trip) is harmless to the JSON parser's trim.
fn parse_voices(body: &[u8]) -> Result<Vec<ElevenVoice>, String> {
    let text = String::from_utf8_lossy(body);
    let v: serde_json::Value = serde_json::from_str(text.trim_end_matches(char::from(0)).trim())
        .map_err(|e| format!("voice list: {e}"))?;
    let rows = v["voices"].as_array().ok_or("voice list: no `voices`")?;
    Ok(rows
        .iter()
        .filter_map(|row| {
            Some(ElevenVoice {
                id: row["voice_id"].as_str()?.to_string(),
                name: row["name"].as_str().unwrap_or("").to_string(),
                premade: row["category"].as_str() == Some("premade"),
            })
        })
        .collect())
}

/// Resolve `wanted` (a name or an id) against the list: the id.
fn eleven_id(list: &[ElevenVoice], wanted: &str) -> Option<String> {
    list.iter()
        .find(|v| v.id == wanted || v.name.eq_ignore_ascii_case(wanted))
        .map(|v| v.id.clone())
}

/// Something that is shaped like a voice id rather than a name.
fn looks_like_id(s: &str) -> bool {
    s.len() >= 16 && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

struct State {
    /// The fallback speaks until then, and why; keyed by the key it was
    /// refused for, so a new key is tried at once.
    refused: Option<(Instant, String, String)>,
    /// ElevenLabs' voice list, by the key it was fetched with.
    eleven: Option<(String, Vec<ElevenVoice>)>,
    /// Whether the "no key, Apple speaks" line has been logged for the
    /// current key state, so it is one line and not one per utterance.
    said_idle: bool,
}

/// The engine.
pub struct CloudTts {
    settings: Arc<ArcSwap<Settings>>,
    fallback: Arc<dyn TtsEngine>,
    curl: PathBuf,
    state: Mutex<State>,
}

impl std::fmt::Debug for CloudTts {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CloudTts")
            .field("settings", &**self.settings.load())
            .finish_non_exhaustive()
    }
}

impl CloudTts {
    /// `settings` is the cell the core refreshes; `fallback` (Apple's)
    /// speaks whenever the cloud does not.
    #[must_use]
    pub fn new(settings: Arc<ArcSwap<Settings>>, fallback: Arc<dyn TtsEngine>) -> Self {
        CloudTts {
            settings,
            fallback,
            curl: PathBuf::from("curl"),
            state: Mutex::new(State {
                refused: None,
                eleven: None,
                said_idle: false,
            }),
        }
    }

    /// The curl to run (tests point this at a fake).
    #[must_use]
    pub fn with_curl(mut self, curl: impl Into<PathBuf>) -> Self {
        self.curl = curl.into();
        self
    }

    /// Why the fallback speaks right now, if it does.
    fn idle_reason(&self, settings: &Settings) -> Option<String> {
        if !settings.enabled {
            return Some("[voice.tts] engine is not \"cloud\"".into());
        }
        if settings.provider.is_none() {
            return Some(format!(
                "[voice.tts] provider {:?} is not one this build knows (openai, elevenlabs)",
                settings.provider_name
            ));
        }
        if settings.api_key.is_empty() {
            return Some("no [voice.tts] api_key".into());
        }
        let mut st = self.state.lock().unwrap();
        if let Some((until, key_id, why)) = &st.refused {
            if *key_id == settings.key_id() && Instant::now() < *until {
                return Some(format!("cooling down after: {why}"));
            }
            st.refused = None;
        }
        None
    }

    fn refuse(&self, settings: &Settings, why: &str) {
        tracing::warn!(
            provider = settings.provider.map_or("?", Provider::name),
            why,
            cooldown_s = COOLDOWN.as_secs(),
            "cloud tts: the fallback engine speaks until the provider answers again"
        );
        self.state.lock().unwrap().refused = Some((Instant::now() + COOLDOWN, settings.key_id(), why.to_string()));
    }

    /// ElevenLabs' list for this key, fetched once.
    fn eleven_voices(&self, settings: &Settings) -> Result<Vec<ElevenVoice>, String> {
        let key_id = settings.key_id();
        if let Some((for_key, list)) = &self.state.lock().unwrap().eleven {
            if *for_key == key_id {
                return Ok(list.clone());
            }
        }
        let (status, body) = call(&self.curl, &voices_request(settings))?;
        if !(200..300).contains(&status) {
            return Err(error_text(status, &body));
        }
        let list = parse_voices(&body)?;
        tracing::info!(voices = list.len(), "cloud tts: ElevenLabs voice list fetched");
        self.state.lock().unwrap().eleven = Some((key_id, list.clone()));
        Ok(list)
    }

    /// The voice to send for the request's: the agent's own when the
    /// provider has it, else one of the pool chosen for that name, stably
    /// (`SidecarTts::map_voice`'s rule).
    fn resolve_voice(&self, settings: &Settings, provider: Provider, requested: Option<&str>) -> Result<String, String> {
        let pool = settings.pool(provider);
        match provider {
            Provider::OpenAi => {
                if let Some(name) = requested.map(str::trim).filter(|n| !n.is_empty()) {
                    if let Some(known) = OPENAI_VOICES.iter().find(|v| v.eq_ignore_ascii_case(name)) {
                        return Ok((*known).to_string());
                    }
                }
                SidecarTts::map_voice(&pool, requested).ok_or_else(|| "no voice".to_string())
            }
            Provider::ElevenLabs => {
                let list = self.eleven_voices(settings)?;
                if let Some(name) = requested.map(str::trim).filter(|n| !n.is_empty()) {
                    if let Some(id) = eleven_id(&list, name) {
                        return Ok(id);
                    }
                }
                // The pool, as ids: what resolves, else the library's own.
                let mut ids: Vec<String> = pool
                    .iter()
                    .filter_map(|v| eleven_id(&list, v).or_else(|| looks_like_id(v).then(|| v.clone())))
                    .collect();
                if ids.is_empty() {
                    ids = list.iter().filter(|v| v.premade).take(4).map(|v| v.id.clone()).collect();
                }
                if ids.is_empty() {
                    ids = list.iter().take(4).map(|v| v.id.clone()).collect();
                }
                if ids.is_empty() {
                    return Err("the account has no voices".into());
                }
                // Map by the requested *name* so the choice is stable per
                // agent, then take the id at that position.
                let names: Vec<String> = ids.iter().map(|id| list.iter().find(|v| v.id == *id).map_or_else(|| id.clone(), |v| v.name.clone())).collect();
                let chosen = SidecarTts::map_voice(&names, requested).unwrap_or_default();
                let index = names.iter().position(|n| *n == chosen).unwrap_or(0);
                Ok(ids[index].clone())
            }
        }
    }

    fn speak_cloud(&self, settings: &Settings, provider: Provider, req: &TtsRequest) -> Result<Box<dyn TtsJob>, String> {
        let voice = self.resolve_voice(settings, provider, req.voice.as_deref())?;
        let request = speech_request(settings, provider, &req.text, &voice, req.rate);
        let (status_tx, status_rx) = crossbeam_channel::bounded(1);
        let (chunks_tx, chunks_rx) = crossbeam_channel::unbounded();
        let started = Instant::now();
        let transfer = start(&self.curl, &request, status_tx, chunks_tx)?;
        match status_rx.recv_timeout(STATUS_WAIT) {
            Ok(Ok(status)) => {
                tracing::debug!(
                    provider = provider.name(),
                    model = settings.model(provider),
                    voice,
                    status,
                    first_ms = started.elapsed().as_millis() as u64,
                    "cloud tts: streaming"
                );
                Ok(Box::new(CloudJob {
                    format: AudioFormat::new(RATE, 1),
                    chunks: chunks_rx,
                    stopper: transfer,
                }))
            }
            Ok(Err(why)) => Err(why),
            Err(_) => {
                transfer.kill();
                Err(format!("no answer in {} s", STATUS_WAIT.as_secs()))
            }
        }
    }
}

struct CloudJob {
    format: AudioFormat,
    chunks: Receiver<TtsChunk>,
    stopper: Arc<Transfer>,
}

impl TtsJob for CloudJob {
    fn format(&self) -> AudioFormat {
        self.format
    }

    fn chunks(&self) -> Receiver<TtsChunk> {
        self.chunks.clone()
    }

    fn stopper(&self) -> Arc<dyn TtsStop> {
        self.stopper.clone()
    }
}

impl TtsEngine for CloudTts {
    /// The provider when the cloud speaks, else the fallback's: this is
    /// what `voice.engines` would say if it asked (it computes the same
    /// from the config, without the hop).
    fn info(&self) -> EngineInfo {
        self.settings
            .load()
            .engine_info()
            .unwrap_or_else(|| self.fallback.info())
    }

    fn voices(&self) -> Vec<VoiceInfo> {
        let settings = self.settings.load();
        let Some(provider) = settings.provider.filter(|_| settings.active()) else {
            return self.fallback.voices();
        };
        settings
            .pool(provider)
            .into_iter()
            .map(|name| VoiceInfo {
                id: name.clone(),
                name,
                locale: String::new(),
                enhanced: true,
            })
            .collect()
    }

    fn synthesize(&self, req: &TtsRequest) -> Result<Box<dyn TtsJob>, AudioError> {
        let settings = self.settings.load_full();
        if let Some(why) = self.idle_reason(&settings) {
            let mut st = self.state.lock().unwrap();
            if !st.said_idle {
                st.said_idle = true;
                tracing::info!(why, "cloud tts: the fallback engine speaks");
            }
            drop(st);
            return self.fallback.synthesize(req);
        }
        self.state.lock().unwrap().said_idle = false;
        let provider = settings.provider.expect("active settings have a provider");
        match self.speak_cloud(&settings, provider, req) {
            Ok(job) => Ok(job),
            Err(why) => {
                self.refuse(&settings, &why);
                self.fallback.synthesize(req)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    // Most of these run curl through a shell-script fake: Unix-only, and
    // with them the helpers and imports only they use.
    #![cfg_attr(not(unix), allow(unused_imports, dead_code))]
    use super::*;
    use crate::audio::fakes::FakeTts;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// A curl that answers from the config it was given on stdin: a 401
    /// for the key `bad`, the voice list for `/v2/voices`, else a header
    /// block and a few PCM chunks. Every config it sees is appended to the
    /// file `FAKE_CURL_LOG` names, so a test can read what was sent.
    #[cfg(unix)]
    const FAKE: &str = r#"
import sys, time
cfg = sys.stdin.read()
with open(sys.argv[1], "a") as f:
    f.write(cfg + "\n=====\n")
out = sys.stdout.buffer
def write(b):
    out.write(b); out.flush()
if "Bearer bad" in cfg or "xi-api-key: bad" in cfg:
    write(b"HTTP/2 401 \r\ncontent-type: application/json\r\n\r\n")
    write(b'{"error":{"message":"Incorrect API key provided"}}')
    sys.exit(0)
if "/v2/voices" in cfg:
    write(b"HTTP/2 200 \r\ncontent-type: application/json\r\n\r\n")
    write(b'{"voices":[{"voice_id":"EXAVITQu4vr4xnSDxMaL","name":"Sarah","category":"premade"},'
          b'{"voice_id":"JBFqnCBsd6RMkjVDRZzb","name":"George","category":"premade"},'
          b'{"voice_id":"abcdefabcdefabcdefab","name":"Mine","category":"cloned"}],"has_more":false}')
    sys.exit(0)
slow = "slowly" in cfg
write(b"HTTP/1.1 100 Continue\r\n\r\n")
write(b"HTTP/1.1 200 OK\r\ncontent-type: audio/pcm\r\n\r\n")
# Three chunks: 1000 samples each, value = chunk index * 1000 (odd split on purpose).
import struct
for i in range(3):
    pcm = struct.pack("<1000h", *([i * 1000] * 1000))
    if i == 1:
        write(pcm[:1]); time.sleep(0.02); write(pcm[1:])
    else:
        write(pcm)
    if slow:
        time.sleep(0.5)
"#;

    #[cfg(unix)]
    struct Rig {
        dir: std::path::PathBuf,
        log: std::path::PathBuf,
    }

    #[cfg(unix)]
    static RIGS: AtomicUsize = AtomicUsize::new(0);

    #[cfg(unix)]
    impl Rig {
        fn new() -> Rig {
            let n = RIGS.fetch_add(1, Ordering::SeqCst);
            let dir = std::env::temp_dir().join(format!("deckd-cloud-tts-{}-{n}", std::process::id()));
            std::fs::create_dir_all(&dir).unwrap();
            let script = dir.join("fake_curl.py");
            std::fs::write(&script, FAKE).unwrap();
            let log = dir.join("calls.log");
            // A `curl` that is the fake, told where this rig's log is.
            let curl = dir.join("curl");
            std::fs::write(
                &curl,
                format!(
                    "#!/bin/sh\nexec /usr/bin/python3 -I {} {} \"$@\"\n",
                    script.display(),
                    log.display()
                ),
            )
            .unwrap();
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&curl, std::fs::Permissions::from_mode(0o755)).unwrap();
            Rig { dir, log }
        }

        fn engine(&self, settings: Settings) -> (CloudTts, crate::audio::fakes::FakeTtsHandle, Arc<ArcSwap<Settings>>) {
            let cell = Arc::new(ArcSwap::from_pointee(settings));
            let (fallback, handle) = FakeTts::new();
            let engine = CloudTts::new(cell.clone(), Arc::new(fallback)).with_curl(self.dir.join("curl"));
            (engine, handle, cell)
        }

        fn calls(&self) -> Vec<String> {
            std::fs::read_to_string(&self.log)
                .unwrap_or_default()
                .split("=====\n")
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string)
                .collect()
        }
    }

    #[cfg(unix)]
    impl Drop for Rig {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    fn settings(provider: &str, key: &str) -> Settings {
        Settings::from_tts(&crate::config::Tts {
            provider: provider.into(),
            api_key: key.into(),
            ..Default::default()
        })
    }

    fn say(text: &str, voice: Option<&str>) -> TtsRequest {
        TtsRequest {
            text: text.into(),
            voice: voice.map(str::to_string),
            rate: 0.5,
        }
    }

    fn drain(job: &dyn TtsJob) -> (Vec<f32>, bool) {
        let mut pcm = Vec::new();
        let mut ended = false;
        for chunk in job.chunks().iter() {
            match chunk {
                TtsChunk::Pcm(p) => pcm.extend(p),
                TtsChunk::End => {
                    ended = true;
                    break;
                }
                TtsChunk::Error(e) => panic!("error chunk: {e}"),
            }
        }
        (pcm, ended)
    }

    #[test]
    fn the_key_rides_in_curls_config_and_the_body_is_quoted_for_it() {
        let mut s = settings("openai", "sk-live-\"quoted\"\\slash");
        s.instruct = Some("calm".into());
        let req = speech_request(&s, Provider::OpenAi, "Say \"hi\" \\ now", "coral", 0.5);
        assert_eq!(req.url, "https://api.openai.com/v1/audio/speech");
        let body: serde_json::Value = serde_json::from_str(req.body.as_deref().unwrap()).unwrap();
        assert_eq!(body["model"], "gpt-4o-mini-tts");
        assert_eq!(body["voice"], "coral");
        assert_eq!(body["response_format"], "pcm");
        assert_eq!(body["instructions"], "calm");
        assert!(body.get("speed").is_none(), "normal rate sends no speed");
        let cfg = curl_config(&req);
        // The key is there, with its own quotes and backslash escaped.
        assert!(cfg.contains("header = \"Authorization: Bearer sk-live-\\\"quoted\\\"\\\\slash\""), "{cfg}");
        // The JSON body's quotes are escaped once for curl.
        assert!(cfg.contains("data-binary = \"{\\\""), "{cfg}");
        assert!(cfg.contains("\\\"model\\\":\\\"gpt-4o-mini-tts\\\""), "{cfg}");
        assert!(cfg.contains("Say \\\\\\\"hi\\\\\\\" \\\\\\\\ now"), "the text's own quotes are JSON-escaped, then curl-escaped: {cfg}");
        assert!(cfg.contains("include\n"));
        assert!(cfg.contains("no-buffer\n"));
        assert!(cfg.contains("header = \"Expect:\""));
        // `tts-1` takes no instructions; a faster rate is a `speed`.
        s.model = "tts-1-hd".into();
        let req = speech_request(&s, Provider::OpenAi, "x", "coral", 0.8);
        let body: serde_json::Value = serde_json::from_str(req.body.as_deref().unwrap()).unwrap();
        assert!(body.get("instructions").is_none());
        assert!((body["speed"].as_f64().unwrap() - 1.6).abs() < 1e-6);
        // ElevenLabs: the voice is in the path, the format in the query,
        // the key in its own header, the locale as ISO 639-1.
        let mut e = settings("elevenlabs", "xi-key");
        e.lang = "en-US".into();
        let req = speech_request(&e, Provider::ElevenLabs, "hello", "EXAVITQu4vr4xnSDxMaL", 0.5);
        assert_eq!(
            req.url,
            "https://api.elevenlabs.io/v1/text-to-speech/EXAVITQu4vr4xnSDxMaL/stream?output_format=pcm_24000"
        );
        assert_eq!(req.headers, vec!["xi-api-key: xi-key".to_string()]);
        let body: serde_json::Value = serde_json::from_str(req.body.as_deref().unwrap()).unwrap();
        assert_eq!(body["model_id"], "eleven_flash_v2_5");
        assert_eq!(body["language_code"], "en");
        e.model = "eleven_multilingual_v2".into();
        let req = speech_request(&e, Provider::ElevenLabs, "hello", "v", 0.5);
        let body: serde_json::Value = serde_json::from_str(req.body.as_deref().unwrap()).unwrap();
        assert!(body.get("language_code").is_none(), "multilingual_v2 refuses it");
    }

    #[test]
    fn a_status_block_is_taken_from_the_front_and_interim_ones_skipped() {
        let mut buf = b"HTTP/1.1 100 Continue\r\n\r\nHTTP/2 200 \r\ncontent-type: audio/pcm\r\n\r\n\x01\x02".to_vec();
        assert_eq!(take_status(&mut buf), Ok(Some(200)));
        assert_eq!(buf, vec![1, 2]);
        let mut partial = b"HTTP/2 401 \r\ncontent-".to_vec();
        assert_eq!(take_status(&mut partial), Ok(None));
        let mut not_http = b"curl: (6) Could not resolve".to_vec();
        assert!(take_status(&mut not_http).is_err());
        assert_eq!(error_text(401, br#"{"error":{"message":"Incorrect API key"}}"#), "HTTP 401: Incorrect API key");
        assert_eq!(error_text(422, br#"{"detail":{"status":"x","message":"bad voice"}}"#), "HTTP 422: bad voice");
        assert_eq!(error_text(500, b"<html>boom</html>"), "HTTP 500: <html>boom</html>");
        // PCM decoding carries an odd byte across calls.
        let mut carry = Vec::new();
        let a = pcm_f32(&[0x00, 0x40, 0x00], &mut carry);
        assert_eq!(a, vec![0.5]);
        let b = pcm_f32(&[0xC0], &mut carry);
        assert_eq!(b, vec![-0.5]);
        assert!(carry.is_empty());
    }

    #[test]
    #[cfg(unix)] // the fake curl is a shell script
    fn without_a_key_or_with_engine_apple_the_fallback_speaks_and_curl_never_runs() {
        let rig = Rig::new();
        let (engine, fallback, cell) = rig.engine(settings("openai", ""));
        let job = engine.synthesize(&say("hello", None)).unwrap();
        drain(job.as_ref());
        assert_eq!(fallback.said().len(), 1);
        assert!(rig.calls().is_empty(), "no key: nothing was sent");
        assert_eq!(engine.info().name, "fake", "voice.engines would name the fallback");
        // A key with the engine set to apple: still the fallback.
        let mut s = settings("openai", "sk-1");
        s.enabled = false;
        cell.store(Arc::new(s));
        drain(engine.synthesize(&say("again", None)).unwrap().as_ref());
        assert_eq!(fallback.said().len(), 2);
        assert!(rig.calls().is_empty());
    }

    #[test]
    #[cfg(unix)] // the fake curl is a shell script
    fn openai_streams_pcm_through_curl_in_the_jobs_format() {
        let rig = Rig::new();
        let (engine, fallback, _cell) = rig.engine(settings("openai", "sk-good"));
        let job = engine.synthesize(&say("hello there", Some("Samantha"))).unwrap();
        assert_eq!(job.format(), AudioFormat::new(24_000, 1));
        let (pcm, ended) = drain(job.as_ref());
        assert!(ended);
        assert_eq!(pcm.len(), 3000);
        assert!((pcm[0]).abs() < 1e-6);
        assert!((pcm[1500] - 1000.0 / 32768.0).abs() < 1e-6, "the odd split did not shift the samples");
        assert!((pcm[2999] - 2000.0 / 32768.0).abs() < 1e-6);
        assert!(fallback.said().is_empty(), "the cloud spoke, not the fallback");
        let calls = rig.calls();
        assert_eq!(calls.len(), 1);
        assert!(calls[0].contains("Bearer sk-good"));
        // `Samantha` is not an OpenAI voice: mapped onto the pool, stably.
        let body = calls[0].lines().find(|l| l.starts_with("data-binary")).unwrap();
        assert!(Provider::OpenAi.default_voices().iter().any(|v| body.contains(&format!("voice\\\":\\\"{v}"))), "{body}");
        // An OpenAI voice name is used as is, pool or not.
        drain(engine.synthesize(&say("x", Some("nova"))).unwrap().as_ref());
        assert!(rig.calls()[1].contains("voice\\\":\\\"nova\\\""));
    }

    #[test]
    #[cfg(unix)] // the fake curl is a shell script
    fn a_refusal_goes_to_the_fallback_and_cools_the_engine_down_until_the_key_changes() {
        let rig = Rig::new();
        let (engine, fallback, cell) = rig.engine(settings("openai", "bad"));
        drain(engine.synthesize(&say("one", None)).unwrap().as_ref());
        assert_eq!(fallback.said().len(), 1, "the 401 went to the fallback");
        assert_eq!(rig.calls().len(), 1);
        let why = engine.state.lock().unwrap().refused.clone().map(|r| r.2).unwrap();
        assert_eq!(why, "HTTP 401: Incorrect API key provided");
        // Cooling down: the next utterance does not even try.
        drain(engine.synthesize(&say("two", None)).unwrap().as_ref());
        assert_eq!(fallback.said().len(), 2);
        assert_eq!(rig.calls().len(), 1, "no request during the cooldown");
        // A new key is tried at once.
        cell.store(Arc::new(settings("openai", "sk-fixed")));
        let job = engine.synthesize(&say("three", None)).unwrap();
        let (pcm, _) = drain(job.as_ref());
        assert_eq!(pcm.len(), 3000);
        assert_eq!(fallback.said().len(), 2);
        assert_eq!(rig.calls().len(), 2);
    }

    #[test]
    #[cfg(unix)] // the fake curl is a shell script
    fn elevenlabs_resolves_names_through_the_voice_list_once() {
        let rig = Rig::new();
        let (engine, _fallback, _cell) = rig.engine(settings("elevenlabs", "xi-good"));
        let job = engine.synthesize(&say("hello", Some("George"))).unwrap();
        let (pcm, _) = drain(job.as_ref());
        assert_eq!(pcm.len(), 3000);
        let calls = rig.calls();
        assert_eq!(calls.len(), 2, "the list, then the speech");
        assert!(calls[0].contains("/v2/voices"));
        assert!(calls[0].contains("xi-api-key: xi-good"));
        assert!(calls[1].contains("/v1/text-to-speech/JBFqnCBsd6RMkjVDRZzb/stream?output_format=pcm_24000"), "{}", calls[1]);
        // A name the list does not have lands on the pool, stably, and
        // the list is not fetched again; an id is used as is.
        drain(engine.synthesize(&say("x", Some("Daniel"))).unwrap().as_ref());
        drain(engine.synthesize(&say("x", Some("Daniel"))).unwrap().as_ref());
        drain(engine.synthesize(&say("x", Some("abcdefabcdefabcdefab"))).unwrap().as_ref());
        let calls = rig.calls();
        assert_eq!(calls.len(), 5);
        let path = |c: &str| c.lines().find(|l| l.starts_with("url")).unwrap().to_string();
        assert_eq!(path(&calls[2]), path(&calls[3]));
        assert!(path(&calls[2]).contains("EXAVITQu4vr4xnSDxMaL") || path(&calls[2]).contains("JBFqnCBsd6RMkjVDRZzb"));
        assert!(path(&calls[4]).contains("/abcdefabcdefabcdefab/"));
    }

    #[test]
    #[cfg(unix)] // the fake curl is a shell script
    fn stop_ends_the_transfer() {
        let rig = Rig::new();
        let (engine, _fallback, _cell) = rig.engine(settings("openai", "sk-good"));
        let job = engine.synthesize(&say("speak slowly please", None)).unwrap();
        let chunks = job.chunks();
        let first = chunks.recv_timeout(Duration::from_secs(5)).unwrap();
        assert!(matches!(first, TtsChunk::Pcm(_)));
        let started = Instant::now();
        job.stopper().stop();
        // The receiver drains and disconnects well before the fake's
        // remaining 1 s of sleeps.
        while chunks.recv_timeout(Duration::from_millis(300)).is_ok() {}
        assert!(started.elapsed() < Duration::from_millis(900), "stop took {:?}", started.elapsed());
    }
}
