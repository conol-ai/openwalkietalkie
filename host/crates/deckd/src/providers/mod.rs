//! The provider registry: where models are reached, set up once and shared
//! by every feature that uses a model.
//!
//! **Two layers.** `[providers.<id>]` says how to reach one provider — its
//! [`Kind`], a label, and for `custom` a base URL — and its key lives in the
//! OS key store ([`keys`]), never in `config.toml`. A feature names a
//! provider and a model (`[context.edit] provider = "openrouter"`, `model =
//! "…"`) and never a URL or a key: [`Config::resolve`] turns the pair into
//! a [`Target`] a call can be made with.
//!
//! **The protocols differ** and the adapters ([`wire`]) hide that: OpenAI's
//! chat completions with a Bearer token for `openai`, `openrouter`,
//! `ollama` and `custom`; Anthropic's Messages API (`x-api-key`,
//! `anthropic-version`, a top-level system prompt) for `anthropic`. A
//! feature asks for a completion and does not know which shape is on the
//! wire.
//!
//! **Models**: a curated list per kind, and the provider's own list fetched
//! with its key and cached ([`catalogue`]).
//!
//! **Moving in**: Milestone 1's per-role endpoints and keys, and the cloud
//! voice's key that sat in `config.toml` on Linux, are moved into the
//! registry ([`migrate`]).
pub mod api;
pub mod catalogue;
pub mod keys;
pub mod migrate;
pub mod omarchy;
pub mod wire;

use crate::config::{Config, ProviderEntry};
use std::time::Duration;

/// How long a call to a model on this machine or the local network may
/// take, unless its entry says otherwise: a local model on a laptop can
/// take several seconds to answer where a cloud one takes a few hundred
/// milliseconds. A cold start (minutes) is not waited for: a model known
/// not to be running is skipped (`context::rewrite`).
pub const LOCAL_TIMEOUT: Duration = Duration::from_secs(15);
/// The longest a provider may ask for, whatever its entry says: a press
/// must end.
pub const TIMEOUT_MAX: Duration = Duration::from_secs(60);

/// Whether `base_url` is on this computer or a private network: loopback,
/// `localhost`, `*.local`, the private IPv4 ranges, CGNAT (Tailscale), and
/// IPv6 loopback, link-local and unique-local.
#[must_use]
pub fn private_address(base_url: &str) -> bool {
    let host = crate::grokbot::webhook::host(base_url).to_ascii_lowercase();
    let host = match host.strip_prefix('[') {
        Some(v6) => v6.split(']').next().unwrap_or("").to_string(),
        None => host.rsplit_once(':').map_or(host.clone(), |(name, _)| name.to_string()),
    };
    if host == "localhost" || host.ends_with(".local") || host.ends_with(".localhost") {
        return true;
    }
    if let Ok(ip) = host.parse::<std::net::Ipv4Addr>() {
        let [a, b, ..] = ip.octets();
        return ip.is_loopback() || ip.is_private() || ip.is_link_local() || (a == 100 && (64..128).contains(&b));
    }
    if let Ok(ip) = host.parse::<std::net::Ipv6Addr>() {
        let first = ip.segments()[0];
        return ip.is_loopback() || (first & 0xfe00) == 0xfc00 || (first & 0xffc0) == 0xfe80;
    }
    false
}

/// How a provider is reached: its protocol, its URL unless `custom`, and
/// what it is good for.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Kind {
    OpenAi,
    OpenRouter,
    Anthropic,
    /// A local Ollama, on this computer unless a base URL says otherwise.
    /// Needs no key.
    Ollama,
    /// ElevenLabs: speech only (the cloud voice).
    ElevenLabs,
    /// This machine's own Omarchy local models: discovered
    /// ([`omarchy`]), never configured; one endpoint per model.
    Omarchy,
    /// Any OpenAI-compatible server, at the base URL given.
    Custom,
}

/// The two wire shapes ([`wire`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Wire {
    /// `POST {base}/chat/completions`, `Authorization: Bearer`.
    OpenAi,
    /// `POST {base}/messages`, `x-api-key`, `anthropic-version`.
    Anthropic,
}

impl Kind {
    pub const ALL: [Kind; 7] = [
        Kind::OpenAi,
        Kind::OpenRouter,
        Kind::Anthropic,
        Kind::Ollama,
        Kind::ElevenLabs,
        Kind::Omarchy,
        Kind::Custom,
    ];

    /// The word in `config.toml`.
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            Kind::OpenAi => "openai",
            Kind::OpenRouter => "openrouter",
            Kind::Anthropic => "anthropic",
            Kind::Ollama => "ollama",
            Kind::ElevenLabs => "elevenlabs",
            Kind::Omarchy => "omarchy",
            Kind::Custom => "custom",
        }
    }

    #[must_use]
    pub fn parse(word: &str) -> Option<Kind> {
        Kind::ALL
            .into_iter()
            .find(|kind| kind.word().eq_ignore_ascii_case(word.trim()))
    }

    /// The name a person reads.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Kind::OpenAi => "OpenAI",
            Kind::OpenRouter => "OpenRouter",
            Kind::Anthropic => "Anthropic",
            Kind::Ollama => "Ollama",
            Kind::ElevenLabs => "ElevenLabs",
            Kind::Omarchy => "Omarchy local models",
            Kind::Custom => "Custom",
        }
    }

    /// The base URL a built-in kind is reached at; `None` for `custom`,
    /// which must be given one.
    #[must_use]
    pub const fn base_url(self) -> Option<&'static str> {
        match self {
            Kind::OpenAi => Some("https://api.openai.com/v1"),
            Kind::OpenRouter => Some("https://openrouter.ai/api/v1"),
            Kind::Anthropic => Some("https://api.anthropic.com/v1"),
            Kind::Ollama => Some("http://127.0.0.1:11434/v1"),
            Kind::ElevenLabs => Some("https://api.elevenlabs.io/v1"),
            // The first model's; each model's own comes from discovery.
            Kind::Omarchy => Some("http://127.0.0.1:11435/v1"),
            Kind::Custom => None,
        }
    }

    #[must_use]
    pub const fn wire(self) -> Wire {
        match self {
            Kind::Anthropic => Wire::Anthropic,
            _ => Wire::OpenAi,
        }
    }

    /// Whether it answers chat completions (context-aware dictation).
    #[must_use]
    pub const fn chat(self) -> bool {
        !matches!(self, Kind::ElevenLabs)
    }

    /// Whether it speaks (the cloud voice).
    #[must_use]
    pub const fn speech(self) -> bool {
        matches!(self, Kind::OpenAi | Kind::ElevenLabs)
    }

    /// Whether a call needs a key. A local Ollama does not; a custom server
    /// may or may not, so a missing key is not refused for it.
    #[must_use]
    pub const fn needs_key(self) -> bool {
        !matches!(self, Kind::Ollama | Kind::Custom)
    }
}

/// One `[providers.<id>]` entry, checked: its kind known, and a base URL
/// where one is needed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Provider {
    pub id: String,
    pub kind: Kind,
    /// The label given, else the kind's name.
    pub label: String,
    pub base_url: String,
    /// On this computer or a private network ([`private_address`]):
    /// slower, and marked so.
    pub local: bool,
    /// How long one call may take when it needs longer than the dictation
    /// budget: [`LOCAL_TIMEOUT`] for a local one, the entry's `timeout_ms`
    /// when it says, `None` for a cloud one (the budget alone).
    pub timeout: Option<Duration>,
}

/// Why an entry is no provider.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Problem {
    UnknownKind(String),
    NoBaseUrl,
    BadBaseUrl,
}

impl std::fmt::Display for Problem {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Problem::UnknownKind(kind) => write!(f, "{kind:?} is no provider kind (openai, openrouter, anthropic, ollama, elevenlabs, omarchy, custom)"),
            Problem::NoBaseUrl => f.write_str("a custom provider needs base_url"),
            Problem::BadBaseUrl => f.write_str("base_url is not an http:// or https:// URL"),
        }
    }
}

impl Provider {
    /// `entry` as provider `id`.
    pub fn from_entry(id: &str, entry: &ProviderEntry) -> Result<Provider, Problem> {
        let kind = Kind::parse(&entry.kind).ok_or_else(|| Problem::UnknownKind(entry.kind.clone()))?;
        let base_url = match (entry.base_url.trim(), kind.base_url()) {
            ("", Some(builtin)) => builtin.to_string(),
            ("", None) => return Err(Problem::NoBaseUrl),
            (given, _) => given.trim_end_matches('/').to_string(),
        };
        if crate::context::http::url_problem(&base_url).is_some() {
            return Err(Problem::BadBaseUrl);
        }
        let local = matches!(kind, Kind::Omarchy | Kind::Ollama) || private_address(&base_url);
        let timeout = entry
            .timeout_ms
            .map(|ms| Duration::from_millis(u64::from(ms.clamp(1000, 60_000))))
            .or(local.then_some(LOCAL_TIMEOUT));
        Ok(Provider {
            id: id.to_string(),
            kind,
            label: entry
                .label
                .clone()
                .filter(|label| !label.trim().is_empty())
                .unwrap_or_else(|| kind.name().to_string()),
            base_url,
            local,
            timeout,
        })
    }
}

/// A provider id as `config.toml` may have one: 1 to 32 of lowercase ASCII
/// letters, digits, `-` and `_`, starting with a letter.
#[must_use]
pub fn id_ok(id: &str) -> bool {
    (1..=32).contains(&id.len())
        && id.starts_with(|c: char| c.is_ascii_lowercase())
        && id
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_')
}

/// What a feature's (provider, model) pair resolves to: everything a call
/// needs but the key, which is fetched where the call is made
/// ([`keys::ProviderKeys`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Target {
    /// Whose key: a provider id, or `legacy:<role>` for a Milestone 1 role
    /// still on its own endpoint (its key is the role's own).
    pub key_of: String,
    pub kind: Kind,
    pub base_url: String,
    pub model: String,
    /// On this computer or a private network.
    pub local: bool,
    /// The provider's own call timeout, when it declares one.
    pub timeout: Option<Duration>,
}

/// Why a pair does not resolve.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Unresolved {
    /// The provider or the model is empty: "not set up".
    NotSetUp,
    /// The provider named is not in `[providers]`.
    NoSuchProvider(String),
    /// The entry is broken.
    Broken(Problem),
    /// The provider does not do this (ElevenLabs for chat).
    WrongKind(Kind),
}

impl Config {
    /// The providers, checked; broken entries left out.
    #[must_use]
    pub fn provider_list(&self) -> Vec<Provider> {
        self.providers
            .iter()
            .filter_map(|(id, entry)| Provider::from_entry(id, entry).ok())
            .collect()
    }

    /// Provider `id`, checked. A built-in kind's own word names that kind
    /// even with no entry for it (`voice.tts.provider = "openai"` from
    /// before the registry keeps meaning OpenAI).
    pub fn provider(&self, id: &str) -> Result<Provider, Unresolved> {
        match self.providers.get(id) {
            Some(entry) => Provider::from_entry(id, entry).map_err(Unresolved::Broken),
            None => match Kind::parse(id).filter(|kind| *kind != Kind::Custom) {
                Some(kind) => Provider::from_entry(
                    id,
                    &ProviderEntry {
                        kind: kind.word().into(),
                        ..ProviderEntry::default()
                    },
                )
                .map_err(Unresolved::Broken),
                None => Err(Unresolved::NoSuchProvider(id.to_string())),
            },
        }
    }

    /// Context-aware dictation's `role` (`scene` or `edit`) as a chat
    /// [`Target`]. A role with no provider but Milestone 1's `endpoint`
    /// resolves to that endpoint with the role's own key.
    pub fn context_target(&self, role: crate::context::keys::Role) -> Result<Target, Unresolved> {
        let model = match role {
            crate::context::keys::Role::Scene => &self.context.scene,
            crate::context::keys::Role::Edit => &self.context.edit,
        };
        if model.model.trim().is_empty() {
            return Err(Unresolved::NotSetUp);
        }
        if model.provider.trim().is_empty() {
            let endpoint = model.endpoint.trim();
            if endpoint.is_empty() {
                return Err(Unresolved::NotSetUp);
            }
            let base_url = migrate::base_of(endpoint);
            let local = private_address(&base_url);
            return Ok(Target {
                key_of: format!("legacy:{}", role.name()),
                kind: migrate::kind_of(&base_url),
                base_url,
                model: model.model.trim().to_string(),
                local,
                timeout: local.then_some(LOCAL_TIMEOUT),
            });
        }
        let provider = self.provider(model.provider.trim())?;
        if !provider.kind.chat() {
            return Err(Unresolved::WrongKind(provider.kind));
        }
        Ok(Target {
            key_of: provider.id,
            kind: provider.kind,
            base_url: provider.base_url,
            model: model.model.trim().to_string(),
            local: provider.local,
            timeout: provider.timeout,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::context::keys::Role;

    #[test]
    fn the_registry_round_trips_and_a_config_without_it_loads() {
        let text = r#"
[providers.openrouter]
kind = "openrouter"
label = "OpenRouter"

[providers.local]
kind = "custom"
base_url = "http://192.168.1.10:11434/v1"

[context.edit]
provider = "openrouter"
model = "deepseek/deepseek-v4.1-flash"
"#;
        let config = Config::parse(text).unwrap();
        assert_eq!(config.providers.len(), 2);
        let written = toml::to_string(&config).unwrap();
        assert_eq!(Config::parse(&written).unwrap(), config);
        assert!(!written.contains("endpoint"), "no legacy field is written back: {written}");
        // No [providers] at all.
        assert!(Config::parse("[api]\nport = 47201\n").unwrap().providers.is_empty());
        let local = config.provider("local").unwrap();
        assert_eq!((local.kind, local.base_url.as_str(), local.label.as_str()), (Kind::Custom, "http://192.168.1.10:11434/v1", "Custom"));
        let or = config.provider("openrouter").unwrap();
        assert_eq!(or.base_url, "https://openrouter.ai/api/v1", "a built-in kind needs no URL typed");
    }

    #[test]
    fn a_role_resolves_to_a_target_or_says_why_not() {
        let mut config = Config::parse(
            "[providers.or]\nkind = \"openrouter\"\n[providers.voice]\nkind = \"elevenlabs\"\n[providers.bad]\nkind = \"custom\"\n",
        )
        .unwrap();
        assert_eq!(config.context_target(Role::Edit), Err(Unresolved::NotSetUp));
        config.context.edit.provider = "or".into();
        assert_eq!(config.context_target(Role::Edit), Err(Unresolved::NotSetUp), "no model: not set up");
        config.context.edit.model = "m".into();
        let target = config.context_target(Role::Edit).unwrap();
        assert_eq!((target.key_of.as_str(), target.kind, target.model.as_str()), ("or", Kind::OpenRouter, "m"));
        config.context.edit.provider = "voice".into();
        assert_eq!(config.context_target(Role::Edit), Err(Unresolved::WrongKind(Kind::ElevenLabs)));
        config.context.edit.provider = "bad".into();
        assert_eq!(config.context_target(Role::Edit), Err(Unresolved::Broken(Problem::NoBaseUrl)));
        config.context.edit.provider = "nope".into();
        assert_eq!(config.context_target(Role::Edit), Err(Unresolved::NoSuchProvider("nope".into())));
        // A kind's own word works with no entry.
        config.context.edit.provider = "anthropic".into();
        assert_eq!(config.context_target(Role::Edit).unwrap().kind, Kind::Anthropic);
        // Milestone 1's own endpoint still resolves, with the role's key.
        config.context.scene.endpoint = "https://openrouter.ai/api/v1/chat/completions".into();
        config.context.scene.model = "small".into();
        let legacy = config.context_target(Role::Scene).unwrap();
        assert_eq!((legacy.key_of.as_str(), legacy.kind, legacy.base_url.as_str()), ("legacy:scene", Kind::OpenRouter, "https://openrouter.ai/api/v1"));
    }

    #[test]
    fn an_id_is_short_lowercase_and_starts_with_a_letter() {
        for ok in ["openrouter", "local", "lab-2", "my_box"] {
            assert!(id_ok(ok), "{ok}");
        }
        for bad in ["", "2nd", "Local", "a b", "a.b", &"x".repeat(33)] {
            assert!(!id_ok(bad), "{bad}");
        }
    }

    #[test]
    fn a_local_provider_is_marked_and_gets_longer_and_a_cloud_one_does_not() {
        for local in ["http://127.0.0.1:11434/v1", "http://localhost:8080/v1", "http://192.168.1.10:11434/v1", "http://10.0.0.2/v1",
                      "http://172.20.1.1/v1", "http://100.101.102.103/v1", "http://box.local:1234/v1", "http://[::1]:8080/v1", "http://[fd00::1]/v1"] {
            assert!(private_address(local), "{local}");
        }
        for cloud in ["https://openrouter.ai/api/v1", "https://api.openai.com/v1", "http://172.32.0.1/v1", "https://8.8.8.8/v1"] {
            assert!(!private_address(cloud), "{cloud}");
        }
        let config = Config::parse("[providers.box]\nkind = \"custom\"\nbase_url = \"http://192.168.1.10:11434/v1\"\n[providers.slowbox]\nkind = \"custom\"\nbase_url = \"http://10.0.0.9/v1\"\ntimeout_ms = 40000\n[providers.or]\nkind = \"openrouter\"\n").unwrap();
        let p = |id: &str| config.provider(id).unwrap();
        assert_eq!((p("box").local, p("box").timeout), (true, Some(LOCAL_TIMEOUT)));
        assert_eq!(p("slowbox").timeout, Some(Duration::from_secs(40)));
        assert_eq!((p("or").local, p("or").timeout), (false, None));
        assert!(config.provider("ollama").unwrap().local && config.provider("omarchy").unwrap().local);
    }
}
