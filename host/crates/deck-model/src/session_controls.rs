//! Public per-session model/effort display and selection. The owner confirms
//! settings requests; built-in adapters use the same model. Device choices
//! and API confirmations are pinned to a displayed page revision.
use crate::{
    AdapterKind, AgentId, Control, DeviceOp, EventKind, Kind, Principal, SessionId, Slot, Store,
};
use deck_proto::SessionMenu;
use std::time::Duration;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ModelOption {
    pub id: String,
    pub label: String,
    #[serde(default)]
    pub efforts: Vec<String>,
    #[serde(default)]
    pub default_effort: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        testkit::{self, Sim},
        Command, DeviceLine, Effect, Request,
    };

    fn fixture() -> (Sim, AgentId, u8, SessionControls) {
        let mut sim = Sim::new();
        let mut reg = testkit::register("controls", Some("codex:controls"));
        reg.kind = Kind::Codex;
        let value = sim
            .api(
                Principal::Adapter(AdapterKind::Codex),
                SessionId(1),
                Request::AgentRegister(reg),
            )
            .unwrap();
        let agent = AgentId(value["agent"].as_str().unwrap().into());
        let slot = sim.store.snapshot().agents[&agent].slot.unwrap().0;
        let controls = SessionControls {
            model: "m0".into(),
            effort: "high".into(),
            editable: true,
            note: String::new(),
            models: (0..6)
                .map(|i| ModelOption {
                    id: format!("m{i}"),
                    label: format!("Model {i}"),
                    efforts: if i == 0 {
                        vec!["low".into(), "high".into()]
                    } else {
                        vec!["low".into()]
                    },
                    default_effort: "low".into(),
                })
                .collect(),
        };
        sim.apply(Command::SessionControls {
            settled: true,
            agent: agent.clone(),
            session: SessionId(1),
            controls: controls.clone(),
        });
        (sim, agent, slot, controls)
    }

    fn request(sim: &mut Sim, slot: u8, field: u8, page: u8, rev: u16, choice: u8) {
        sim.apply(Command::Device(DeviceLine::Session {
            slot,
            field,
            page,
            rev,
            choice,
        }));
    }
    fn menu(sim: &Sim) -> SessionMenu {
        sim.effects
            .iter()
            .find_map(|e| match e {
                Effect::Device(DeviceOp::Control(Control::SessionMenu(m))) => Some(m.clone()),
                _ => None,
            })
            .expect("picker response")
    }

    #[test]
    fn selection_pages_the_catalog_and_waits_for_adapter_confirmation() {
        let (mut sim, agent, slot, mut controls) = fixture();
        request(&mut sim, slot, 1, 1, 0, 255);
        let page = menu(&sim);
        assert_eq!(page.choices, ["Model 4", "Model 5"]);
        request(&mut sim, slot, 1, 1, page.rev, 1);
        assert_eq!(menu(&sim).status, 2);
        assert_eq!(
            menu(&sim).model,
            "Model 0",
            "never optimistically claim the new model"
        );
        let event = sim
            .events
            .iter()
            .find(|e| e.kind == EventKind::SessionSettingsRequested)
            .unwrap();
        assert_eq!(event.data["model"], "m5");
        assert_eq!(
            event.data["effort"], "low",
            "unsupported effort changes to the new model's default"
        );
        sim.apply(Command::SessionControls {
            settled: false,
            agent: agent.clone(),
            session: SessionId(1),
            controls: controls.clone(),
        });
        assert!(
            sim.effects.is_empty(),
            "ordinary metadata cannot settle a pending change"
        );
        request(&mut sim, slot, 1, 1, 0, 255);
        assert_eq!(menu(&sim).status, 2);
        let pending = menu(&sim);
        request(&mut sim, slot, 1, 1, pending.rev, 0);
        assert!(
            sim.events.is_empty(),
            "a pending operation cannot be duplicated"
        );
        controls.model = "m5".into();
        controls.effort = "low".into();
        sim.apply(Command::SessionControls {
            settled: true,
            agent,
            session: SessionId(1),
            controls,
        });
        assert_eq!((menu(&sim).status, menu(&sim).selected), (1, Some(5)));
    }

    #[test]
    fn stale_revision_wrong_owner_and_reassigned_slot_cannot_change_settings() {
        let (mut sim, agent, slot, mut controls) = fixture();
        request(&mut sim, slot, 1, 0, 0, 255);
        let old = menu(&sim);
        controls.models.reverse();
        sim.apply(Command::SessionControls {
            settled: true,
            agent: agent.clone(),
            session: SessionId(99),
            controls: controls.clone(),
        });
        assert!(
            sim.effects.is_empty(),
            "another adapter session cannot report controls"
        );
        sim.apply(Command::SessionControls {
            settled: true,
            agent: agent.clone(),
            session: SessionId(1),
            controls,
        });
        request(&mut sim, slot, 1, 0, old.rev, 0);
        assert!(sim.events.is_empty());
        sim.api(
            Principal::Adapter(AdapterKind::Codex),
            SessionId(1),
            Request::AgentRelease {
                agent,
                reason: None,
            },
        )
        .unwrap();
        let _ = sim.register(
            Principal::Adapter(AdapterKind::Codex),
            SessionId(1),
            "new",
            Some("codex:new"),
        );
        request(&mut sim, slot, 1, 0, old.rev, 0);
        assert!(sim.events.is_empty());
    }

    #[test]
    fn supported_efforts_and_timeouts_are_honest_and_read_only_sessions_stay_inert() {
        let (mut sim, agent, slot, _) = fixture();
        request(&mut sim, slot, 2, 0, 0, 255);
        let m = menu(&sim);
        assert_eq!(m.choices, ["low", "high"]);
        request(&mut sim, slot, 2, 0, m.rev, 0);
        sim.advance(Duration::from_secs(46));
        assert_eq!(menu(&sim).status, 3);
        assert_eq!(menu(&sim).effort, "high");
        sim.apply(Command::SessionControls {
            settled: true,
            agent,
            session: SessionId(1),
            controls: SessionControls::default(),
        });
        let m = menu(&sim);
        request(&mut sim, slot, 2, 0, m.rev, 0);
        assert!(sim.events.is_empty());
    }

    #[test]
    fn public_settings_work_for_every_agent_kind_and_only_the_owner_can_report_them() {
        for kind in [Kind::Other, Kind::Claude, Kind::Codex, Kind::Gemini, Kind::Conol] {
            let mut sim = Sim::new();
            let mut reg = testkit::register("Native chat", Some("example:session"));
            reg.kind = kind;
            let result = sim.api(Principal::Uid(501), SessionId(1), Request::AgentRegister(reg)).unwrap();
            let agent = AgentId(result["agent"].as_str().unwrap().into());
            let slot = sim.store.snapshot().agents[&agent].slot.unwrap().0;
            let report = SessionSet { agent: agent.clone(), request_rev: None, controls: SessionControls {
                model: "Example Model".into(), effort: "High".into(), note: "Change settings in the app.".into(),
                ..SessionControls::default()
            }};
            assert!(sim.api(Principal::Uid(501), SessionId(2), Request::SessionSet(report.clone())).is_err());
            sim.api(Principal::Uid(501), SessionId(1), Request::SessionSet(report)).unwrap();
            request(&mut sim, slot, 0, 0, 0, 255);
            let m = menu(&sim);
            assert_eq!((&*m.model, &*m.effort, m.status), ("Example Model", "High", 0));
            assert_eq!(m.note, "Change settings in the app.");
            request(&mut sim, slot, 1, 0, m.rev, 0);
            assert!(sim.events.is_empty(), "read-only reports cannot enable choices");
        }
    }

    #[test]
    fn public_settings_confirmation_is_bound_to_the_requested_revision() {
        let (mut sim, agent, slot, mut controls) = fixture();
        request(&mut sim, slot, 1, 0, 0, 255);
        let rev = menu(&sim).rev;
        request(&mut sim, slot, 1, 0, rev, 1);
        let event = sim.events.iter().find(|e| e.kind == EventKind::SessionSettingsRequested).unwrap();
        assert_eq!(event.data["request_rev"], rev);
        controls.model = "m1".into(); controls.effort = "low".into();
        let report = SessionSet { agent, controls, request_rev: Some(rev.wrapping_add(1)) };
        assert!(sim.api(Principal::Uid(501), SessionId(1), Request::SessionSet(report.clone())).is_err());
        let refresh = SessionSet { request_rev: None, ..report.clone() };
        sim.api(Principal::Uid(501), SessionId(1), Request::SessionSet(refresh)).unwrap();
        request(&mut sim, slot, 1, 0, 0, 255);
        assert_eq!(menu(&sim).status, 2, "an ordinary report cannot confirm a request");
        let confirm = SessionSet { request_rev: Some(rev), ..report };
        sim.api(Principal::Uid(501), SessionId(1), Request::SessionSet(confirm.clone())).unwrap();
        assert_eq!((menu(&sim).status, menu(&sim).selected), (1, Some(1)));
        assert!(sim.api(Principal::Uid(501), SessionId(1), Request::SessionSet(confirm)).is_err());
    }

    #[test]
    fn public_catalog_validation_rejects_ambiguous_or_oversized_options() {
        let (mut sim, agent, _, controls) = fixture();
        for mutate in [0, 1, 2, 3] {
            let mut bad = controls.clone();
            match mutate {
                0 => bad.models.push(bad.models[0].clone()),
                1 => bad.model = "missing".into(),
                2 => bad.models[0].efforts.push("low".into()),
                _ => bad.note = "x".repeat(257),
            }
            assert!(sim.api(Principal::Uid(501), SessionId(1), Request::SessionSet(SessionSet {
                agent: agent.clone(), controls: bad, request_rev: None,
            })).is_err());
        }
    }

    #[test]
    fn models_without_effort_support_explain_why_the_effort_picker_is_empty() {
        let (mut sim, agent, slot, mut controls) = fixture();
        controls.models[0].efforts.clear();
        controls.models[0].default_effort.clear();
        sim.apply(Command::SessionControls {
            settled: false,
            agent,
            session: SessionId(1),
            controls,
        });
        request(&mut sim, slot, 2, 0, 0, 255);
        let m = menu(&sim);
        assert_eq!((m.status, m.total), (0, 0));
        assert_eq!(m.effort, "Not supported");
        assert!(m.note.contains("does not offer"));
        request(&mut sim, slot, 2, 0, m.rev, 0);
        assert!(sim.events.is_empty());
        request(&mut sim, slot, 1, 0, 0, 255);
        assert_eq!(menu(&sim).status, 1, "model selection remains available");
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SessionControls {
    pub model: String,
    pub effort: String,
    #[serde(default)]
    pub models: Vec<ModelOption>,
    #[serde(default)]
    pub editable: bool,
    #[serde(default)]
    pub note: String,
}

/// Owner-authenticated settings report. Omit `request_rev` for ordinary refreshes.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SessionSet {
    pub agent: AgentId,
    pub controls: SessionControls,
    /// Confirm the exact `session.settings_requested` revision after applying it.
    /// Report the actual settings and a nonempty note if the change failed.
    #[serde(default)]
    pub request_rev: Option<u16>,
}

impl SessionControls {
    fn validate(&self) -> Result<(), crate::ApiError> {
        let bounded = |value: &str, max| value.len() <= max && !value.chars().any(char::is_control);
        if !bounded(&self.model, 128) || !bounded(&self.effort, 32) || !bounded(&self.note, 256)
            || self.models.len() > 64 {
            return Err(crate::ApiError::invalid("controls", "model/effort/note/catalog exceeds bounds"));
        }
        let mut ids = std::collections::HashSet::new();
        for model in &self.models {
            if model.id.is_empty() || !bounded(&model.id, 128) || model.label.trim().is_empty()
                || !bounded(&model.label, 128) || !ids.insert(&model.id) || model.efforts.len() > 16
                || model.efforts.iter().any(|e| e.is_empty() || !bounded(e, 32))
                || model.efforts.iter().collect::<std::collections::HashSet<_>>().len() != model.efforts.len()
                || (!model.default_effort.is_empty() && !model.efforts.contains(&model.default_effort)) {
                return Err(crate::ApiError::invalid("controls.models", "invalid or duplicate model/effort option"));
            }
        }
        if self.editable && !self.models.iter().any(|m| m.id == self.model
            && (self.effort.is_empty() || m.efforts.contains(&self.effort))) {
            return Err(crate::ApiError::invalid("controls.model", "current selection must belong to the catalog"));
        }
        Ok(())
    }
}

#[derive(Debug)]
pub(crate) struct Panel {
    agent: AgentId,
    slot: u8,
    field: u8,
    page: u8,
    last: Option<SessionMenu>,
}

impl Store {
    pub(crate) fn set_session_controls(&mut self, session: SessionId, request: SessionSet) -> crate::ApiResult {
        self.owned(&request.agent, session)?;
        request.controls.validate()?;
        let aux = self.aux.entry(request.agent).or_default();
        if let Some(rev) = request.request_rev {
            if aux.settings_pending.is_none() || aux.settings_request_rev != Some(rev) {
                return Err(crate::ApiError::invalid("request_rev", "stale settings confirmation"));
            }
            aux.settings_pending = None;
            aux.settings_request_rev = None;
        }
        aux.controls = Some(request.controls);
        self.render_session_panel(false);
        Ok(self.rev())
    }

    pub(crate) fn on_session_controls(
        &mut self,
        agent: AgentId,
        session: SessionId,
        settled: bool,
        mut controls: SessionControls,
    ) {
        let Some(owner) = self.snapshot.agents.get(&agent) else {
            return;
        };
        if owner.owner.session != session
            || !matches!(
                owner.owner.principal,
                Principal::Adapter(AdapterKind::Codex | AdapterKind::Claude)
            )
        {
            return;
        }
        controls.models.truncate(64);
        for model in &mut controls.models {
            model.id = crate::sanitize::cut(&model.id, 128);
            model.label = crate::sanitize::text(&model.label);
            model.efforts.truncate(16);
        }
        let aux = self.aux.entry(agent).or_default();
        aux.controls = Some(controls);
        if settled {
            aux.settings_pending = None;
            aux.settings_request_rev = None;
        }
        self.render_session_panel(false);
    }

    pub(crate) fn on_device_session(
        &mut self,
        slot: u8,
        field: u8,
        page: u8,
        rev: u16,
        choice: u8,
    ) {
        if field > 2 || page > 63 {
            return;
        }
        let Some(agent) = self
            .snapshot
            .agents
            .values()
            .find(|a| a.slot == Some(Slot(slot)))
            .map(|a| a.id.clone())
        else {
            return;
        };
        if choice != 255 {
            // A slot may have been reassigned or the catalog reordered during a
            // touch. Never interpret that old touch against the new session.
            let valid = self.session_panel.as_ref().is_some_and(|p| {
                p.agent == agent
                    && p.slot == slot
                    && p.field == field
                    && p.page == page
                    && p.last.as_ref().is_some_and(|m| {
                        m.rev == rev && m.status != 2 && (choice as usize) < m.choices.len()
                    })
            });
            if !valid {
                self.render_session_panel(true);
                return;
            }
            let Some(aux) = self.aux.get_mut(&agent) else {
                return;
            };
            let Some(controls) = aux.controls.as_mut().filter(|c| c.editable) else {
                return;
            };
            if aux.settings_pending.is_some() {
                return;
            }
            let index = usize::from(page) * 4 + usize::from(choice);
            let (model, effort) = match field {
                1 => {
                    let Some(model) = controls.models.get(index) else {
                        return;
                    };
                    let effort = if model.efforts.contains(&controls.effort) {
                        controls.effort.clone()
                    } else {
                        model.default_effort.clone()
                    };
                    (model.id.clone(), effort)
                }
                2 => {
                    let Some(model) = controls.models.iter().find(|m| m.id == controls.model)
                    else {
                        return;
                    };
                    let Some(effort) = model.efforts.get(index) else {
                        return;
                    };
                    (model.id.clone(), effort.clone())
                }
                _ => return,
            };
            aux.settings_pending = Some(self.now);
            aux.settings_request_rev = Some(rev);
            controls.note.clear();
            self.emit(
                EventKind::SessionSettingsRequested,
                Principal::Device,
                serde_json::json!({"agent": agent, "model": model, "effort": effort, "request_rev": rev}),
            );
        } else {
            self.session_panel = Some(Panel {
                agent,
                slot,
                field,
                page,
                last: None,
            });
        }
        self.render_session_panel(true);
    }

    pub(crate) fn tick_session_controls(&mut self) {
        for aux in self.aux.values_mut() {
            if aux
                .settings_pending
                .is_some_and(|at| self.now.saturating_duration_since(at) > Duration::from_secs(45))
            {
                aux.settings_pending = None;
                aux.settings_request_rev = None;
                if let Some(c) = &mut aux.controls {
                    c.note = "Change not confirmed. Refresh and try again.".into();
                }
            }
        }
        self.render_session_panel(false);
    }

    fn render_session_panel(&mut self, force: bool) {
        let Some(panel) = &self.session_panel else {
            return;
        };
        let owner = self
            .snapshot
            .agents
            .get(&panel.agent)
            .filter(|a| a.slot == Some(Slot(panel.slot)));
        let aux = owner.and_then(|a| self.aux.get(&a.id));
        let controls = aux.and_then(|a| a.controls.as_ref());
        let pending = aux.is_some_and(|a| a.settings_pending.is_some());
        let current = controls.and_then(|c| c.models.iter().find(|m| m.id == c.model));
        let editable = controls.is_some_and(|c| c.editable);
        let mut menu = SessionMenu {
            slot: panel.slot,
            field: panel.field,
            page: panel.page,
            model: controls
                .map(|c| current.map_or_else(|| c.model.clone(), |m| m.label.clone()))
                .unwrap_or_default(),
            effort: if current.is_some_and(|m| m.efforts.is_empty()) {
                "Not supported".into()
            } else {
                controls.map(|c| c.effort.clone()).unwrap_or_default()
            },
            status: if pending {
                2
            } else if !editable {
                0
            } else if controls.is_some_and(|c| !c.note.is_empty()) {
                3
            } else {
                1
            },
            note: if pending {
                "Applying for the next turn...".into()
            } else if let Some(c) = controls.filter(|c| !c.note.is_empty()) {
                c.note.clone()
            } else if editable {
                "Changes apply to the next turn.".into()
            } else if owner.is_none() {
                "This session has ended.".into()
            } else if owner.is_some_and(|a| a.native_companion() || a.kind == Kind::Other) {
                "Settings are managed by this agent's app.".into()
            } else if owner.is_some_and(|a| a.kind == Kind::Claude) {
                "Use /model or /effort in Claude Code. Channels does not enable these controls."
                    .into()
            } else {
                "Start Codex with its shared service. Update Codex if selection stays unavailable."
                    .into()
            },
            ..SessionMenu::default()
        };
        let (labels, selected): (Vec<String>, Option<usize>) = match (menu.field, controls) {
            (1, Some(c)) if editable => (
                c.models.iter().map(|m| m.label.clone()).collect(),
                c.models.iter().position(|m| m.id == c.model),
            ),
            (2, Some(c)) if editable => current
                .map(|m| {
                    (
                        m.efforts.clone(),
                        m.efforts.iter().position(|e| e == &c.effort),
                    )
                })
                .unwrap_or_default(),
            _ => (Vec::new(), None),
        };
        menu.total = labels.len().min(255) as u8;
        if menu.field == 2 && menu.total == 0 && editable && !pending {
            menu.status = 0;
            menu.note = "This model does not offer thinking effort selection.".into();
        }
        menu.page = menu.page.min(menu.total.saturating_sub(1) / 4);
        menu.choices = labels
            .into_iter()
            .skip(usize::from(menu.page) * 4)
            .take(4)
            .collect();
        menu.selected = selected.map(|i| i as u8);
        if !force
            && panel.last.as_ref().is_some_and(|old| {
                let mut old = old.clone();
                old.rev = 0;
                old == menu
            })
        {
            return;
        }
        self.session_menu_rev = self.session_menu_rev.wrapping_add(1).max(1);
        menu.rev = self.session_menu_rev;
        let panel = self.session_panel.as_mut().unwrap();
        panel.page = menu.page;
        panel.last = Some(menu.clone());
        self.device_op(DeviceOp::Control(Control::SessionMenu(menu)));
    }
}
