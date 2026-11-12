//! Who is asking (§4.2): principals and scopes. Trust is not a scope.
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::fmt;

#[derive(
    Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "lowercase")]
pub enum AdapterKind {
    Claude,
    Codex,
    Gemini,
    Mcp,
    /// The API surface for virtual agents.
    Virtual,
}

/// `"device"`, `"uid:501"`, `"token:hooks"`, `"adapter:claude"`.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Principal {
    /// A line that arrived on the HID link. Trusted.
    Device,
    Uid(u32),
    Token(String),
    Adapter(AdapterKind),
}

impl Principal {
    /// The deck itself is the only principal that is trusted by identity;
    /// the verified GUI is trusted per session by the daemon, not here.
    pub const fn is_device(&self) -> bool {
        matches!(self, Principal::Device)
    }
}

impl fmt::Display for Principal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Principal::Device => f.write_str("device"),
            Principal::Uid(uid) => write!(f, "uid:{uid}"),
            Principal::Token(name) => write!(f, "token:{name}"),
            Principal::Adapter(kind) => write!(
                f,
                "adapter:{}",
                match kind {
                    AdapterKind::Claude => "claude",
                    AdapterKind::Codex => "codex",
                    AdapterKind::Gemini => "gemini",
                    AdapterKind::Mcp => "mcp",
                    AdapterKind::Virtual => "virtual",
                }
            ),
        }
    }
}

impl std::str::FromStr for Principal {
    type Err = ();

    fn from_str(s: &str) -> Result<Self, ()> {
        if s == "device" {
            return Ok(Principal::Device);
        }
        if let Some(uid) = s.strip_prefix("uid:") {
            return uid.parse().map(Principal::Uid).map_err(|_| ());
        }
        if let Some(name) = s.strip_prefix("token:") {
            return Ok(Principal::Token(name.to_string()));
        }
        if let Some(kind) = s.strip_prefix("adapter:") {
            return match kind {
                "claude" => Ok(Principal::Adapter(AdapterKind::Claude)),
                "codex" => Ok(Principal::Adapter(AdapterKind::Codex)),
                "gemini" => Ok(Principal::Adapter(AdapterKind::Gemini)),
                "mcp" => Ok(Principal::Adapter(AdapterKind::Mcp)),
                "virtual" => Ok(Principal::Adapter(AdapterKind::Virtual)),
                _ => Err(()),
            };
        }
        Err(())
    }
}

impl Serialize for Principal {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for Principal {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        s.parse()
            .map_err(|()| serde::de::Error::custom("bad principal"))
    }
}

impl JsonSchema for Principal {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "Principal".into()
    }

    fn json_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
        schemars::json_schema!({
            "type": "string",
            "pattern": "^(device|uid:[0-9]+|token:.+|adapter:(claude|codex|gemini|mcp|virtual))$"
        })
    }
}

#[derive(
    Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
pub enum Scope {
    #[serde(rename = "observe")]
    Observe,
    #[serde(rename = "observe:content")]
    ObserveContent,
    #[serde(rename = "observe:raw")]
    ObserveRaw,
    #[serde(rename = "hooks:ingest")]
    HooksIngest,
    #[serde(rename = "agent")]
    Agent,
    #[serde(rename = "operate")]
    Operate,
    #[serde(rename = "voice")]
    Voice,
    #[serde(rename = "device")]
    Device,
    #[serde(rename = "admin")]
    Admin,
}

impl Scope {
    pub const ALL: [Scope; 9] = [
        Scope::Observe,
        Scope::ObserveContent,
        Scope::ObserveRaw,
        Scope::HooksIngest,
        Scope::Agent,
        Scope::Operate,
        Scope::Voice,
        Scope::Device,
        Scope::Admin,
    ];

    /// The cap for a UDS peer whose ancestry contains an agent CLI.
    pub const AGENT_DESCENDED_CAP: [Scope; 4] = [
        Scope::Observe,
        Scope::Agent,
        Scope::Voice,
        Scope::HooksIngest,
    ];

    /// `operate` implies `observe:content`; `admin` implies everything but raw.
    pub fn grants_content(scopes: &[Scope]) -> bool {
        scopes.iter().any(|s| {
            matches!(
                s,
                Scope::ObserveContent | Scope::Operate | Scope::Admin | Scope::ObserveRaw
            )
        })
    }

    pub fn grants_raw(scopes: &[Scope]) -> bool {
        scopes.contains(&Scope::ObserveRaw)
    }

    /// `scopes` as redaction sees a holder that wants no content: every
    /// scope that grants it (`observe:content`, `operate`, `admin`,
    /// `observe:raw`) left out, the rest kept. For redacting only: what the
    /// holder may call is still its scopes' to say.
    pub fn without_content(scopes: &[Scope]) -> Vec<Scope> {
        scopes
            .iter()
            .copied()
            .filter(|scope| !Scope::grants_content(&[*scope]))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn principals_round_trip_as_strings() {
        for p in [
            Principal::Device,
            Principal::Uid(501),
            Principal::Token("hooks".into()),
            Principal::Adapter(AdapterKind::Claude),
            Principal::Adapter(AdapterKind::Virtual),
        ] {
            let s = p.to_string();
            assert_eq!(s.parse::<Principal>(), Ok(p.clone()));
            assert_eq!(serde_json::to_string(&p).unwrap(), format!("\"{s}\""));
            assert_eq!(
                serde_json::from_str::<Principal>(&format!("\"{s}\"")).unwrap(),
                p
            );
        }
        assert_eq!(Principal::Uid(501).to_string(), "uid:501");
        assert_eq!("uid:x".parse::<Principal>(), Err(()));
        assert_eq!("adapter:cursor".parse::<Principal>(), Err(()));
        assert!(Principal::Device.is_device());
    }

    #[test]
    fn scopes_serialize_with_colons() {
        assert_eq!(
            serde_json::to_string(&Scope::ObserveContent).unwrap(),
            "\"observe:content\""
        );
        assert_eq!(
            serde_json::to_string(&Scope::HooksIngest).unwrap(),
            "\"hooks:ingest\""
        );
        assert!(Scope::grants_content(&[Scope::Operate]));
        assert!(!Scope::grants_content(&[Scope::Observe, Scope::Agent]));
        assert!(Scope::grants_raw(&[Scope::ObserveRaw]));
        assert!(!Scope::grants_raw(&[Scope::Admin]));
    }

    /// A holder that wants no content is redacted as one that has none,
    /// whatever it holds, raw hook payloads included; everything else it
    /// holds is kept in its order.
    #[test]
    fn without_content_leaves_out_every_scope_that_grants_it() {
        let seen = Scope::without_content(&Scope::ALL);
        assert!(!Scope::grants_content(&seen));
        assert!(!Scope::grants_raw(&seen));
        assert_eq!(
            seen,
            [
                Scope::Observe,
                Scope::HooksIngest,
                Scope::Agent,
                Scope::Voice,
                Scope::Device
            ]
        );
        let app = [Scope::Voice, Scope::Device, Scope::Admin];
        assert_eq!(
            Scope::without_content(&app),
            [Scope::Voice, Scope::Device]
        );
        assert!(Scope::without_content(&[]).is_empty());
    }
}
