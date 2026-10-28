//! `openwalkietalkie-app --shots DIR`: open the real window against whatever
//! daemon the socket leads to, walk it through every screen and the three
//! dialogs, save each as `DIR/<scene>-<dark|light>.png`, and quit.
//!
//! It is how the look is checked without anyone taking a screenshot: the
//! capture is this process's own window ([`crate::snapshot`]), which needs no
//! Screen Recording grant. Run it against `deckd run --demo --no-device` on
//! a throwaway `DECKD_DATA_DIR` for a populated roster, and once per
//! `OPENWALKIETALKIE_APPEARANCE` for both appearances.
//!
//! Nothing a scene does reaches past the daemon it is pointed at. The one
//! call it makes that is not a read is the plan scene's `integrations.install
//! {dry_run:true}`, which plans and returns without applying; the
//! confirmation and token scenes are staged in the window alone and are
//! cleared without being answered.
use crate::integrations::Shown;
use crate::present::Step;
use crate::shell::{Screen, Shell};
use deck_gui_core::admin::{Secret, Verb};
use deck_gui_core::ConfirmRequest;
use deck_model::{AdapterKind, Principal};
use gpui::{App, Context, Entity, Global, Window};
use std::path::PathBuf;
use std::time::Duration;

/// Where the PNGs go.
pub struct Shots {
    dir: PathBuf,
}

impl Global for Shots {}

impl Shots {
    /// Workflow-only captures use synthetic data, with no service connection.
    pub fn synthetic_only(&self) -> bool {
        std::env::var("OPENWALKIETALKIE_SHOTS_ONLY").ok().is_some_and(|list| {
            !list.is_empty() && list.split(',').all(|scene| matches!(scene.trim(),
                "images" | "image-expanded" | "usage-fable" | "usage-reset-expiring" | "workflow" | "workflow-runs" | "workflow-expanded" | "workflow-complete"))
        })
    }

    /// `--shots DIR` anywhere in the arguments.
    pub fn from_args(mut args: impl Iterator<Item = String>) -> Option<Shots> {
        while let Some(arg) = args.next() {
            if arg == "--shots" {
                return args.next().map(|dir| Shots {
                    dir: PathBuf::from(dir),
                });
            }
        }
        None
    }
}

/// One staged state of the window.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scene {
    Deck,
    Images,
    ImageExpanded,
    Workflow,
    WorkflowRuns,
    UsageFable,
    UsageResetExpiring,
    WorkflowExpanded,
    WorkflowComplete,
    Confirm,
    Integrations,
    Details,
    Plan,
    Token,
    Voice,
    /// The first-start question about a local recogniser, over the Voice
    /// screen.
    Recognition,
    Service,
    Welcome(Step),
    /// A screen scrolled to its foot.
    Lower(Screen),
}

/// Every scene, with the name its file gets.
pub const SCENES: [(Scene, &str); 23] = [
    (Scene::Deck, "deck"),
    (Scene::Images, "images"),
    (Scene::ImageExpanded, "image-expanded"),
    (Scene::Workflow, "workflow"),
    (Scene::WorkflowRuns, "workflow-runs"),
    (Scene::UsageFable, "usage-fable"),
    (Scene::UsageResetExpiring, "usage-reset-expiring"),
    (Scene::WorkflowExpanded, "workflow-expanded"),
    (Scene::WorkflowComplete, "workflow-complete"),
    (Scene::Confirm, "confirm"),
    (Scene::Integrations, "integrations"),
    (Scene::Details, "integrations-details"),
    (Scene::Plan, "plan"),
    (Scene::Token, "token"),
    (Scene::Voice, "voice"),
    (Scene::Recognition, "recognition"),
    (Scene::Service, "service"),
    (Scene::Welcome(Step::Service), "welcome-1"),
    (Scene::Welcome(Step::Deck), "welcome-2"),
    (Scene::Welcome(Step::Integrations), "welcome-3"),
    (Scene::Lower(Screen::Integrations), "integrations-lower"),
    (Scene::Lower(Screen::Voice), "voice-lower"),
    (Scene::Lower(Screen::Service), "service-lower"),
];

/// How long a scene is given to fetch what it draws and settle.
const SETTLE: Duration = Duration::from_millis(1600);

/// Walk the scenes, saving each, then quit.
pub fn start(shell: Entity<Shell>, window: &mut Window, cx: &mut App) {
    let dir = cx.global::<Shots>().dir.clone();
    let synthetic = cx.global::<Shots>().synthetic_only();
    if let Err(e) = std::fs::create_dir_all(&dir) {
        eprintln!(
            "openwalkietalkie-app: --shots cannot use {}: {e}",
            dir.display()
        );
        cx.quit();
        return;
    }
    // `OPENWALKIETALKIE_SHOTS_SIZE=880x560` draws every scene at that size
    // instead of the default, and says so in the file names.
    let size = std::env::var("OPENWALKIETALKIE_SHOTS_SIZE")
        .ok()
        .and_then(|s| {
            let (w, h) = s.split_once('x')?;
            Some((w.trim().parse::<f32>().ok()?, h.trim().parse::<f32>().ok()?))
        });
    if let Some((w, h)) = size {
        window.resize(gpui::size(gpui::px(w), gpui::px(h)));
    }
    let suffix = size.map_or(String::new(), |(w, h)| format!("-{w}x{h}"));
    window
        .spawn(cx, async move |cx| {
            // The socket, the first snapshot and the first frame: up to ten
            // seconds for a roster, so a slow start does not stage an empty one.
            for _ in 0..if synthetic { 0 } else { 20 } {
                cx.background_executor()
                    .timer(Duration::from_millis(500))
                    .await;
                let ready = cx
                    .update(|_, cx| {
                        let state = &shell.read(cx).state;
                        state.connected() && !state.mirror().agents.is_empty()
                    })
                    .unwrap_or(true);
                if ready {
                    break;
                }
            }
            cx.background_executor()
                .timer(Duration::from_millis(500))
                .await;
            // `OPENWALKIETALKIE_SHOTS_ONLY=deck,plan` narrows the walk.
            let only: Option<Vec<String>> = std::env::var("OPENWALKIETALKIE_SHOTS_ONLY")
                .ok()
                .map(|list| list.split(',').map(|s| s.trim().to_string()).collect());
            for (scene, name) in SCENES {
                if only
                    .as_ref()
                    .is_some_and(|only| !only.iter().any(|o| o == name))
                {
                    continue;
                }
                cx.update(|window, cx| {
                    shell.update(cx, |this, cx| this.stage(scene, window, cx));
                })
                .ok();
                cx.background_executor().timer(SETTLE).await;
                cx.update(|window, cx| {
                    let appearance = if crate::style::palette(cx).dark {
                        "dark"
                    } else {
                        "light"
                    };
                    let path = dir.join(format!("{name}-{appearance}{suffix}.png"));
                    match crate::snapshot::save_window_png(window, &path) {
                        Ok(()) => println!("{}", path.display()),
                        Err(e) => eprintln!("openwalkietalkie-app: {name}: {e}"),
                    }
                })
                .ok();
            }
            cx.update(|_, cx| cx.quit()).ok();
        })
        .detach();
}

impl Shell {
    /// Put the window in `scene`, clearing whatever the last scene left up
    /// without answering it.
    fn stage(&mut self, scene: Scene, window: &mut Window, cx: &mut Context<Self>) {
        self.confirm = None;
        self.recognition_prompt = false;
        self.integrations.stage_token(None);
        self.cancel_plan(cx);
        match scene {
            Scene::UsageFable | Scene::UsageResetExpiring => {
                use deck_gui_core::{ApiEvent, AppEvent};
                let snapshot = deck_model::Snapshot { usage: Some(deck_model::Usage { period: "5h / 7d".into(), at: Some(crate::present::now()),
                    items: [("Claude session", 40), ("Claude week", 52), ("Claude Fable week", 100), ("Codex session", 17), ("Codex week", 30)]
                        .into_iter().map(|(name, percent)| deck_model::UsageItem { reset_tip: if matches!(scene, Scene::UsageResetExpiring) {
                            match name { "Claude week" => Some("2 resets expire in 1d 6h".into()),
                                "Codex week" => Some("1 reset expires in 2h 10m".into()), _ => None }
                        } else { None }, name: name.into(), percent,
                            account: Some("work@example.com".into()), reset: "resets in 2d 4h".into(), tone: None }).collect() }), ..Default::default() };
                self.state.handle_event(AppEvent::Api(ApiEvent::Connected { snapshot }));
                self.go(Screen::Usage, window, cx);
            }
            Scene::Deck => {
                self.go(Screen::Deck, window, cx);
                self.select_for_shots();
            }
            Scene::Images | Scene::ImageExpanded | Scene::Workflow | Scene::WorkflowRuns | Scene::WorkflowExpanded | Scene::WorkflowComplete => {
                self.go(Screen::Deck, window, cx);
                self.select_for_shots();
                self.stage_workflow(scene);
            }
            Scene::Confirm => {
                self.go(Screen::Deck, window, cx);
                self.select_for_shots();
                let agent = self.deck.selected.clone();
                let card = self.card_of(agent.as_ref()).map(|c| c.id);
                self.raise_confirm(
                    ConfirmRequest {
                        agent,
                        card,
                        order: None,
                        principal: Some(Principal::Adapter(AdapterKind::Mcp)),
                    },
                    window,
                    cx,
                );
            }
            Scene::Integrations => {
                self.integrations.stage_expanded(&[]);
                self.go(Screen::Integrations, window, cx);
            }
            Scene::Details => {
                self.integrations.stage_expanded(&["claude"]);
                self.go(Screen::Integrations, window, cx);
            }
            Scene::Plan => {
                self.integrations.stage_expanded(&[]);
                self.go(Screen::Integrations, window, cx);
                self.plan_integration(Verb::Install, "claude", window, cx);
            }
            Scene::Token => {
                self.go(Screen::Integrations, window, cx);
                let secret = Secret::from_reply(&serde_json::json!({
                    "token": "owt_live_4c1f0e9a7b2d48e6a1c3f5b7d9e0a2c4"
                }));
                if let Some(secret) = secret {
                    self.integrations
                        .stage_token(Some(Shown::new("ci-runner", "Created", secret)));
                    self.dialog_opened(window);
                }
            }
            Scene::Voice => self.go(Screen::Voice, window, cx),
            Scene::Recognition => {
                self.go(Screen::Voice, window, cx);
                self.recognition_prompt = true;
                self.dialog_opened(window);
            }
            Scene::Service => self.go(Screen::Service, window, cx),
            Scene::Welcome(step) => {
                self.go(Screen::Welcome, window, cx);
                self.go_step(step, window, cx);
            }
            Scene::Lower(screen) => {
                self.go(screen, window, cx);
                self.page_scroll.scroll_to_bottom();
            }
        }
        cx.notify();
    }

    /// Synthetic provider events for visual checks; only the window's mirror
    /// changes. No request or content is sent to the connected daemon.
    fn stage_workflow(&mut self, scene: Scene) {
        use deck_gui_core::{ApiEvent, AppEvent};
        use deck_model::workflow::{ActivityStatus, Plan, PlanStep, StepStatus};
        use deck_model::{AgentState, ChatMessage, ChatRole, MessageMeta};
        // Keep previews deterministic even when the service is disconnected
        // or correctly refuses content access to an agent-launched process.
        let mut snapshot = deck_model::Snapshot::default();
        let id = deck_model::AgentId("ag_workflow_preview".into());
        self.deck.selected = Some(id.clone());
        let mut agent = deck_model::Agent {
            id: id.clone(), slot: Some(deck_model::Slot(1)), kind: deck_model::Kind::Codex,
            name: String::new(), state: AgentState::Working, title: None,
            title_source: deck_model::TitleSource::Prompt, since: crate::present::now(),
            source: deck_model::Source::Api,
            owner: deck_model::Owner { principal: Principal::Uid(0), session: deck_model::SessionId(1), lease_expires: None },
            session: deck_model::SessionInfo { key: None, cwd: Some("/demo/openwalkietalkie".into()), pid: None, pid_kind: None, terminal: None },
            caps: deck_model::Caps::default(), companion: None, card: None, reply: None, messages: Vec::new(), workflow_runs: Vec::new(), workflow: None,
            chat_rev: 1, reply_rev: 0, voice: None, pinned: false, meta: serde_json::Value::Null, redacted: false,
        };
        let complete = scene == Scene::WorkflowComplete;
        agent.name = "Workflow preview".into();
        agent.state = if complete { AgentState::Ready } else { AgentState::Working };
        agent.title = Some("Add a live workflow to the Deck".into());
        agent.card = None;
        agent.reply = None;
        agent.redacted = false;
        agent.workflow = Some(Plan { explanation: None, steps: [
            "Inspect provider events and the Deck layout",
            "Connect plans and tool activity",
            "Build the compact workflow view",
            "Verify live updates and small windows",
            "Review the result",
        ].into_iter().enumerate().map(|(i, step)| PlanStep { step: step.into(),
            status: if complete || i < 3 { StepStatus::Completed } else if i == 3 { StepStatus::InProgress } else { StepStatus::Pending }
        }).collect() });
        agent.messages = [("Read", "{\"file_path\":\"src/deck.rs\"}", "Deck layout inspected."),
            ("Edit", "{\"file_path\":\"src/workflow.rs\"}", "Workflow panel added."),
            ("Bash", "{\"command\":\"cargo test workflow\"}", "Running workflow checks…")]
            .into_iter().enumerate().map(|(i, (name, input, output))| {
                let running = !complete && i == 2;
                ChatMessage { message: MessageMeta { images: Vec::new(), id: format!("preview-tool-{i}"), role: ChatRole::Tool, streaming: running,
                    activity: Some(if running { ActivityStatus::Running } else { ActivityStatus::Completed }), runs: None, plan: None },
                    text: format!("**{name}**\n\n```json\n{input}\n```\n\n{output}"), at: crate::present::now() }
            }).collect();
        if scene == Scene::WorkflowRuns {
            agent.kind = deck_model::Kind::Claude;
            agent.name = "Parallel workflows".into();
            agent.title = Some("Background workflows are running".into());
            agent.state = AgentState::Idle; // parent may be idle while workers run
            agent.workflow = None;
            agent.messages.clear();
            agent.workflow_runs = [("fixup-constraints-and-truth", 9, 488_100), ("design-navigation-panel", 4, 1_100_000)]
                .into_iter().enumerate().map(|(i, (name, total, tokens))| deck_model::workflow::Run {
                    id: format!("wf_preview_{i}"), name: name.into(), status: ActivityStatus::Running,
                    completed: 0, total, elapsed_ms: 76_000, tokens,
                }).collect();
        }
        self.deck.workflow = Default::default();
        if scene == Scene::WorkflowExpanded {
            self.deck.workflow.toggle_tool(&agent.id, "preview-tool-2");
        }
        if matches!(scene, Scene::Images | Scene::ImageExpanded) {
            agent.name = "Image preview".into(); agent.title = Some("Review the device screen".into());
            agent.state = AgentState::Ready; agent.workflow = None; agent.workflow_runs.clear();
            agent.messages = vec![ChatMessage { message:MessageMeta { images:vec![deck_model::ChatImage { id:"preview-image".into(), label:"Device screen.png".into(), width:480, height:480 }], id:"preview-message".into(), role:ChatRole::Assistant, streaming:false, plan:None, runs:None, activity:None }, text:"Here is the device screen.".into(), at:crate::present::now() }];
            self.deck.images = crate::images::View::preview(id.clone(), scene == Scene::ImageExpanded);
        }
        snapshot.agents.insert(id, agent);
        self.state.handle_event(AppEvent::Api(ApiEvent::Connected { snapshot }));
    }

    /// The first agent with a card on it, or the first agent.
    fn select_for_shots(&mut self) {
        let agents = &self.state.mirror().agents;
        let pick = agents
            .values()
            .find(|a| a.card.is_some())
            .or_else(|| agents.values().next())
            .map(|a| a.id.clone());
        if pick.is_some() {
            self.deck.selected = pick;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shots_is_asked_for_by_a_flag_and_a_directory() {
        let args = |list: &[&str]| list.iter().map(|s| (*s).to_string()).collect::<Vec<_>>();
        assert!(Shots::from_args(args(&[]).into_iter()).is_none());
        assert!(Shots::from_args(args(&["--shots"]).into_iter()).is_none());
        let shots = Shots::from_args(args(&["--shots", "/tmp/x"]).into_iter()).unwrap();
        assert_eq!(shots.dir, PathBuf::from("/tmp/x"));
    }

    #[test]
    fn every_scene_is_saved_under_a_name_of_its_own() {
        for (i, (_, name)) in SCENES.iter().enumerate() {
            assert!(
                !SCENES[i + 1..].iter().any(|(_, other)| other == name),
                "{name} twice"
            );
        }
    }
}
