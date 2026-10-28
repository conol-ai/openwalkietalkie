//! Public native-companion presentation, independent of agent/provider kind.
use crate::{AgentId, ApiError, ApiResult, EventKind, SessionId, Store};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum CompanionRole { Session, Launcher }

/// A client-owned group. Identifiers partition slot promotion; labels are public.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CompanionProfile {
    /// Stable application identifier, 1–64 ASCII letters, digits, dots, dashes or underscores.
    pub app_id: String,
    /// Stable account/workspace identifier within this app, 1–128 bytes. Do not put secrets here.
    pub group_id: String,
    /// Roster heading, 1–24 UTF-8 bytes; use distinct headings for distinct device pages.
    pub group_label: String,
    pub role: CompanionRole,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CompanionSet {
    pub agent: AgentId,
    /// Null removes native presentation. Existing registrations remain valid.
    pub profile: Option<CompanionProfile>,
}

impl CompanionProfile {
    pub fn validate(&self) -> Result<(), ApiError> {
        if self.app_id.is_empty() || self.app_id.len() > 64
            || !self.app_id.bytes().all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b)) {
            return Err(ApiError::invalid("profile.app_id", "1..64 ASCII identifier"));
        }
        for (key, value, max) in [("profile.group_id", &self.group_id, 128), ("profile.group_label", &self.group_label, 24)] {
            if value.trim().is_empty() || value.len() > max || value.chars().any(char::is_control) {
                return Err(ApiError::invalid(key, "nonempty bounded text without control characters"));
            }
        }
        Ok(())
    }
}

impl Store {
    pub(crate) fn set_companion(&mut self, session: SessionId, request: CompanionSet) -> ApiResult {
        self.owned(&request.agent, session)?;
        if let Some(profile) = &request.profile { profile.validate()?; }
        let agent = self.snapshot.agents.get_mut(&request.agent).unwrap();
        if agent.companion == request.profile { return Ok(self.rev()); }
        agent.companion = request.profile.clone();
        self.emit(EventKind::AgentChanged, self.principal_of(&request.agent),
            serde_json::json!({"agent": request.agent, "fields": {"companion": request.profile}}));
        self.put_agent_state(&request.agent);
        Ok(self.rev())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{testkit::{Sim, UID, register}, Request, Kind, Effect, Command, DeviceLine, HostLine};

    #[test]
    fn any_provider_can_group_native_sessions_without_terminal_focus() {
        for kind in [Kind::Other, Kind::Codex, Kind::Claude, Kind::Gemini] {
            let mut sim = Sim::new();
            let mut registration = register("Session", Some("example:session"));
            registration.kind = kind;
            let value = sim.api(UID, SessionId(1), Request::AgentRegister(registration)).unwrap();
            let agent = AgentId(value["agent"].as_str().unwrap().into());
            let profile = CompanionProfile { app_id: "example.app".into(), group_id: "account-a".into(),
                group_label: "Example".into(), role: CompanionRole::Session };
            let configure = CompanionSet { agent: agent.clone(), profile: Some(profile) };
            assert!(sim.api(UID, SessionId(2), Request::CompanionSet(configure.clone())).is_err());
            sim.api(UID, SessionId(1), Request::CompanionSet(configure)).unwrap();
            let a = &sim.store.snapshot().agents[&agent];
            assert_eq!(a.kind, kind, "the API retains the provider identity");
            assert_eq!(a.project().as_deref(), Some("Example"));
            assert_eq!(a.device_kind(), Kind::Other);
            assert!(sim.effects.iter().any(|e| matches!(e, Effect::Device(crate::DeviceOp::Put(_, HostLine::Agent { project: Some(p), .. })) if p == "Example")));
            sim.api(UID, SessionId(1), Request::AgentFocus { agent: agent.clone() }).unwrap();
            assert!(!sim.effects.iter().any(|e| matches!(e, Effect::Desktop(_))));
            sim.apply(Command::Device(DeviceLine::Channel { slot: 1, seq: 1 }));
            assert!(!sim.effects.iter().any(|e| matches!(e, Effect::Desktop(_))));
            sim.api(UID, SessionId(1), Request::CompanionSet(CompanionSet { agent: agent.clone(), profile: None })).unwrap();
            assert!(sim.store.snapshot().agents[&agent].companion.is_none());
        }
    }

    #[test]
    fn profiles_reject_oversized_or_ambiguous_labels() {
        let profile = CompanionProfile { app_id: "example".into(), group_id: "a".into(), group_label: "Example".into(), role: CompanionRole::Launcher };
        assert!(profile.validate().is_ok());
        for bad in [CompanionProfile { app_id: "../x".into(), ..profile.clone() },
            CompanionProfile { group_label: "x".repeat(25), ..profile.clone() },
            CompanionProfile { group_label: "a\nb".into(), ..profile.clone() },
            CompanionProfile { group_id: String::new(), ..profile }] {
            assert!(bad.validate().is_err());
        }
    }
}
