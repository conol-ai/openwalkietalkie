//! Identifiers. Agents are addressed by `AgentId` everywhere in the API; the
//! device slot is a projection (D3).
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::fmt;

/// `"ag_…"`: stable for the life of the agent.
#[derive(
    Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[schemars(transparent)]
pub struct AgentId(pub String);

impl AgentId {
    /// A new id from a 64-bit seed (the core hashes the registration key and
    /// a counter; tests pass anything).
    pub fn from_seed(seed: u64) -> Self {
        AgentId(format!("ag_{}", base32(seed)))
    }
}

impl fmt::Display for AgentId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Device slot 1..=255 (one byte on the wire); a deck holds
/// `DeviceState::capacity` of them, 8 on a deck that does not say.
#[derive(
    Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[schemars(transparent)]
pub struct Slot(pub u8);

impl Slot {
    pub const MAX: u8 = u8::MAX;
    /// What a deck that does not report its table holds.
    pub const DEFAULT: u8 = 8;

    /// 1..=MAX; with MAX = 255, only slot 0 is out of range.
    pub const fn is_valid(self) -> bool {
        self.0 >= 1
    }
}

impl fmt::Display for Slot {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Per-slot card id, `last + 1` with wrap-around skipping 0 (§6.5); the
/// default 0 is "no card yet" (never on the wire).
#[derive(
    Clone,
    Copy,
    Debug,
    Default,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    Hash,
    Serialize,
    Deserialize,
    JsonSchema,
)]
#[schemars(transparent)]
pub struct CardId(pub u16);

impl CardId {
    /// The id after `self`; never 0.
    pub const fn next(self) -> CardId {
        match self.0.wrapping_add(1) {
            0 => CardId(1),
            n => CardId(n),
        }
    }
}

impl fmt::Display for CardId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// One connection (or logical HTTP session); assigned at `session.hello`.
/// On the wire it is the string `"sess_<base32>"` (its `Display`); a bare
/// integer is still accepted on input (week-1 compatibility).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SessionId(pub u64);

impl SessionId {
    pub const PREFIX: &'static str = "sess_";
}

impl fmt::Display for SessionId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}{}", Self::PREFIX, base32(self.0))
    }
}

/// `"sess_<base32>"` that is not one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BadSessionId;

impl fmt::Display for BadSessionId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("expected \"sess_<base32>\"")
    }
}

impl std::error::Error for BadSessionId {}

impl std::str::FromStr for SessionId {
    type Err = BadSessionId;

    fn from_str(s: &str) -> Result<Self, BadSessionId> {
        s.strip_prefix(Self::PREFIX)
            .and_then(unbase32)
            .map(SessionId)
            .ok_or(BadSessionId)
    }
}

impl Serialize for SessionId {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for SessionId {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct Visitor;

        impl serde::de::Visitor<'_> for Visitor {
            type Value = SessionId;

            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("\"sess_<base32>\" or an integer")
            }

            fn visit_u64<E: serde::de::Error>(self, v: u64) -> Result<SessionId, E> {
                Ok(SessionId(v))
            }

            fn visit_i64<E: serde::de::Error>(self, v: i64) -> Result<SessionId, E> {
                u64::try_from(v)
                    .map(SessionId)
                    .map_err(|_| E::custom("a session id is not negative"))
            }

            fn visit_str<E: serde::de::Error>(self, v: &str) -> Result<SessionId, E> {
                v.parse().map_err(E::custom)
            }
        }

        d.deserialize_any(Visitor)
    }
}

impl JsonSchema for SessionId {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "SessionId".into()
    }

    fn json_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
        schemars::json_schema!({ "type": "string", "pattern": "^sess_[0-9a-z]+$" })
    }
}

/// A parked hook response the core resolves later.
#[derive(
    Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[schemars(transparent)]
pub struct HoldId(pub u64);

/// Crockford-style base32 without i, l, o, u (`0123456789abcdefghjkmnpqrstvwxyz`).
const ALPHABET: &[u8; 32] = b"0123456789abcdefghjkmnpqrstvwxyz";

fn base32(mut n: u64) -> String {
    let mut out = Vec::new();
    loop {
        out.push(ALPHABET[(n & 31) as usize]);
        n >>= 5;
        if n == 0 {
            break;
        }
    }
    out.reverse();
    String::from_utf8(out).unwrap_or_default()
}

/// The inverse of `base32`: `None` on an empty string, a foreign character
/// or a value past `u64`.
fn unbase32(s: &str) -> Option<u64> {
    if s.is_empty() {
        return None;
    }
    s.bytes().try_fold(0u64, |acc, c| {
        let digit = ALPHABET.iter().position(|a| *a == c)? as u64;
        acc.checked_mul(32)?.checked_add(digit)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn card_ids_skip_zero_on_wrap() {
        assert_eq!(CardId(1).next(), CardId(2));
        assert_eq!(CardId(u16::MAX).next(), CardId(1));
        assert_eq!(CardId(0).next(), CardId(1));
    }

    #[test]
    fn ids_render_with_their_prefixes() {
        assert_eq!(AgentId::from_seed(0).to_string(), "ag_0");
        assert!(AgentId::from_seed(u64::MAX).to_string().starts_with("ag_"));
        assert_eq!(SessionId(31).to_string(), "sess_z");
        assert_ne!(AgentId::from_seed(1), AgentId::from_seed(2));
        assert!(Slot(1).is_valid() && Slot(8).is_valid());
        assert!(!Slot(0).is_valid() && Slot(255).is_valid());
        assert_eq!(serde_json::to_string(&Slot(3)).unwrap(), "3");
        assert_eq!(
            serde_json::to_string(&AgentId("ag_x".into())).unwrap(),
            "\"ag_x\""
        );
    }

    #[test]
    fn session_ids_round_trip_as_sess_strings_and_accept_integers() {
        for n in [0, 1, 3, 31, 32, 1023, 1_000_000, u64::MAX] {
            let id = SessionId(n);
            let json = serde_json::to_string(&id).unwrap();
            assert_eq!(json, format!("\"{id}\""));
            assert!(json.starts_with("\"sess_"), "{json}");
            assert_eq!(serde_json::from_str::<SessionId>(&json).unwrap(), id);
            assert_eq!(id.to_string().parse::<SessionId>(), Ok(id));
            assert_eq!(
                serde_json::from_str::<SessionId>(&n.to_string()).unwrap(),
                id
            );
        }
        assert_eq!(serde_json::to_string(&SessionId(3)).unwrap(), "\"sess_3\"");
        assert_eq!(SessionId(u64::MAX).to_string(), "sess_fzzzzzzzzzzzz");
        assert_eq!(
            serde_json::to_value(SessionId(1023)).unwrap(),
            serde_json::json!("sess_zz")
        );
        // Inside a struct field, too (the `Owner` shape of the conflict error).
        let owner: serde_json::Value = serde_json::json!({"session": SessionId(7)});
        assert_eq!(owner["session"], "sess_7");
        for bad in [
            "\"sess_\"",
            "\"sess_i\"",
            "\"7\"",
            "\"ag_7\"",
            "\"sess_zzzzzzzzzzzzz\"",
            "-1",
            "null",
            "1.5",
        ] {
            assert!(
                serde_json::from_str::<SessionId>(bad).is_err(),
                "{bad} must not parse"
            );
        }
        assert_eq!("sess_i".parse::<SessionId>(), Err(BadSessionId));
        assert_eq!(unbase32(""), None);
        assert_eq!(unbase32("fzzzzzzzzzzzz"), Some(u64::MAX));
        assert_eq!(unbase32("g0000000000000"), None, "past u64");
        let schema = schemars::schema_for!(SessionId);
        let schema = serde_json::to_value(schema).unwrap();
        assert_eq!(schema["type"], "string");
        assert_eq!(schema["pattern"], "^sess_[0-9a-z]+$");
    }
}
