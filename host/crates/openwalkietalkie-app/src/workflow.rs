//! A live, compact view of explicit plans and observed tools. Expansion is
//! local to the window and keyed by agent/message, so streaming preserves it.
use crate::shell::Shell;
use crate::style::palette;
use crate::ui::{self, ic, Btn, Type as _};
use deck_model::workflow::{self, ActivityStatus, Plan, StepStatus};
use deck_model::{Agent, AgentId, AgentState};
use gpui::{div, prelude::*, px, relative, AnyElement, Context, Div, SharedString};
use gpui_component::{h_flex, v_flex};
use std::collections::BTreeSet;
use std::ops::Range;

#[derive(Default)]
pub struct ViewState {
    expanded: BTreeSet<(AgentId, String)>,
    full_plan: BTreeSet<AgentId>,
    full_activity: BTreeSet<AgentId>,
}

impl ViewState {
    pub fn toggle_tool(&mut self, agent: &AgentId, message: &str) {
        let key = (agent.clone(), message.to_owned());
        if !self.expanded.remove(&key) {
            // Old sessions must not grow window state without limit.
            if self.expanded.len() >= 256 {
                self.expanded.pop_first();
            }
            self.expanded.insert(key);
        }
    }

    pub fn tool_open(&self, agent: &AgentId, message: &str) -> bool {
        self.expanded.contains(&(agent.clone(), message.to_owned()))
    }
}

fn toggle(set: &mut BTreeSet<AgentId>, id: &AgentId) {
    if !set.remove(id) {
        if set.len() >= 128 {
            set.pop_first();
        }
        set.insert(id.clone());
    }
}

/// Keep the current step in view even when a long plan advances beyond the
/// first screen. One completed predecessor supplies context.
pub fn visible_steps(plan: &Plan, expanded: bool, limit: usize) -> Range<usize> {
    let len = plan.steps.len();
    let limit = limit.max(1);
    if expanded || len <= limit {
        return 0..len;
    }
    let current = plan
        .steps
        .iter()
        .position(|s| s.status == StepStatus::InProgress)
        .or_else(|| {
            plan.steps
                .iter()
                .position(|s| s.status == StepStatus::Pending)
        })
        .unwrap_or(len - 1);
    let start = current.saturating_sub(1).min(len - limit);
    start..start + limit
}

fn run_metrics(run: &workflow::Run, language: deck_gui_core::i18n::Language) -> String {
    let seconds = run.elapsed_ms / 1000;
    let elapsed = if seconds >= 3600 { format!("{}h{}m", seconds / 3600, seconds % 3600 / 60) }
        else if seconds >= 60 { format!("{}m{}s", seconds / 60, seconds % 60) }
        else { format!("{seconds}s") };
    let tokens = if run.tokens >= 1_000_000 { format!("{:.1}m", run.tokens as f64 / 1_000_000.) }
        else if run.tokens >= 1000 { format!("{:.1}k", run.tokens as f64 / 1000.) }
        else { run.tokens.to_string() };
    language.format("{0}/{1} · {2} · {3} tokens", &[&run.completed.to_string(), &run.total.to_string(), &elapsed, &tokens])
}

impl Shell {
    pub(crate) fn render_workflow(&self, agent: &Agent, dim: bool, cx: &Context<Self>) -> Div {
        let p = *palette(cx);
        let runs_live = agent
            .workflow_runs
            .iter()
            .any(|r| r.status == ActivityStatus::Running);
        let live = !dim && (matches!(agent.state, AgentState::Working) || runs_live);
        let status = if dim {
            self.tr("Disconnected")
        } else if runs_live && agent.state != AgentState::Question {
            self.tr("In progress")
        } else {
            match agent.state {
                AgentState::Working => self.tr("In progress"),
                AgentState::Question => self.tr("Waiting for you"),
                AgentState::Error => self.tr("Needs attention"),
                AgentState::Ready => self.tr("Ready"),
                _ => self.tr("Idle"),
            }
        };
        let mut body = ui::panel(&p).gap(px(12.));
        if agent.redacted {
            body = body.child(
                div()
                    .ts(12., 18.)
                    .text_color(p.muted)
                    .child(self.tr(crate::present::REDACTED)),
            );
        } else {
            for run in &agent.workflow_runs {
                let running = run.status == ActivityStatus::Running;
                let color = match run.status {
                    ActivityStatus::Running => p.accent,
                    ActivityStatus::Completed => p.success,
                    ActivityStatus::Failed => p.danger,
                    ActivityStatus::Stopped => p.muted,
                };
                let mark = if running && !dim {
                    ui::spinner(13., color).into_any_element()
                } else {
                    ui::icon(
                        match run.status {
                            ActivityStatus::Completed => ic::CHECK,
                            ActivityStatus::Failed => ic::CLOSE_CIRCLE,
                            ActivityStatus::Stopped => ic::STOP,
                            ActivityStatus::Running => ic::CLOCK,
                        },
                        13.,
                        color,
                    )
                    .into_any_element()
                };
                body = body.child(
                    v_flex()
                        .gap(px(6.))
                        .child(
                            h_flex()
                                .gap(px(8.))
                                .child(mark)
                                .child(
                                    div()
                                        .flex_1()
                                        .min_w_0()
                                        .truncate()
                                        .ts(12., 18.)
                                        .medium()
                                        .child(run.name.clone()),
                                )
                                .when(!running, |d| {
                                    d.child(
                                        div()
                                            .ts(10., 14.)
                                            .text_color(color)
                                            .child(self.tr(run.status.label())),
                                    )
                                }),
                        )
                        .child(
                            div().w_full().h(px(3.)).rounded_full().bg(p.border).child(
                                div()
                                    .h_full()
                                    .w(relative(if run.total == 0 {
                                        0.
                                    } else {
                                        run.completed as f32 / run.total as f32
                                    }))
                                    .rounded_full()
                                    .bg(color),
                            ),
                        )
                        .child(
                            div()
                                .pl(px(21.))
                                .ts(11., 16.)
                                .text_color(p.muted)
                                .child(run_metrics(run, self.language())),
                        ),
                );
            }
            if let Some(plan) = &agent.workflow {
                let completed = plan.completed();
                body = body.child(
                    h_flex()
                        .gap(px(8.))
                        .child(ui::icon(ic::LIST, 15., p.accent))
                        .child(div().flex_1().ts(13., 18.).medium().child(self.tr("Plan")))
                        .child(
                            div()
                                .ts(12., 16.)
                                .text_color(p.muted)
                                .child(self.fmt("{0}/{1} complete", &[&(completed).to_string(), &(plan.steps.len()).to_string()])),
                        ),
                );
                if !plan.steps.is_empty() {
                    body = body.child(
                        div().w_full().h(px(3.)).rounded_full().bg(p.border).child(
                            div()
                                .h_full()
                                .w(relative(completed as f32 / plan.steps.len() as f32))
                                .rounded_full()
                                .bg(p.accent),
                        ),
                    );
                }
                if let Some(explanation) = &plan.explanation {
                    body = body.child(
                        div()
                            .ts(12., 18.)
                            .text_color(p.muted)
                            .child(explanation.clone()),
                    );
                }
                let expanded = self.deck.workflow.full_plan.contains(&agent.id);
                let limit = if self.detail_room() < 440. { 3 } else { 5 };
                let range = visible_steps(plan, expanded, limit);
                let mut steps = v_flex();
                if range.start > 0 {
                    steps = steps.child(
                        div()
                            .pl(px(28.))
                            .pb(px(6.))
                            .ts(11., 16.)
                            .text_color(p.faint)
                            .child(self.fmt("{0} earlier steps", &[&(range.start).to_string()])),
                    );
                }
                for index in range.clone() {
                    let step = &plan.steps[index];
                    let running = step.status == StepStatus::InProgress;
                    let color = match step.status {
                        StepStatus::Completed => p.success,
                        StepStatus::InProgress => p.accent,
                        StepStatus::Pending => p.faint,
                    };
                    let mark: AnyElement = if running && live {
                        ui::spinner(14., color).into_any_element()
                    } else if step.status == StepStatus::Completed {
                        ui::icon(ic::CHECK, 14., color).into_any_element()
                    } else {
                        div()
                            .size(px(8.))
                            .rounded_full()
                            .border_1()
                            .border_color(color)
                            .when(running, |d| d.bg(color))
                            .into_any_element()
                    };
                    steps = steps.child(
                        h_flex()
                            .items_start()
                            .gap(px(10.))
                            .child(
                                v_flex()
                                    .w(px(18.))
                                    .flex_none()
                                    .items_center()
                                    .child(
                                        div()
                                            .h(px(24.))
                                            .flex()
                                            .items_center()
                                            .justify_center()
                                            .child(mark),
                                    )
                                    .when(index + 1 < range.end, |d| {
                                        d.child(div().w(px(1.)).flex_1().min_h(px(8.)).bg(p.border))
                                    }),
                            )
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .pt(px(2.))
                                    .pb(px(10.))
                                    .ts(13., 20.)
                                    .text_color(if step.status == StepStatus::Pending {
                                        p.muted
                                    } else {
                                        p.text
                                    })
                                    .child(step.step.clone()),
                            ),
                    );
                }
                body = body.child(steps);
                if plan.steps.len() > limit {
                    let id = agent.id.clone();
                    body = body.child(
                        Btn::new(
                            "workflow-plan-more",
                            if expanded {
                                self.tr("Show current steps").into()
                            } else {
                                self.fmt("Show all {0} steps", &[&(plan.steps.len()).to_string()])
                            },
                        )
                        .small()
                        .on_click(cx.listener(move |this, _, _, cx| {
                            toggle(&mut this.deck.workflow.full_plan, &id);
                            cx.notify();
                        })),
                    );
                }
            }
            let activity = workflow::activity(agent);
            if !activity.is_empty() {
                let expanded = self.deck.workflow.full_activity.contains(&agent.id);
                let recent = activity.len().saturating_sub(4);
                body = body.child(
                    div()
                        .when(agent.workflow.is_some(), |d| {
                            d.border_t_1().border_color(p.border).pt(px(12.))
                        })
                        .ts(12., 16.)
                        .text_color(p.muted)
                        .child(self.tr("Recent activity")),
                );
                for (index, item) in activity.iter().enumerate() {
                    let open = self
                        .deck
                        .workflow
                        .tool_open(&agent.id, &item.message.message.id);
                    if !expanded
                        && index < recent
                        && item.status != ActivityStatus::Running
                        && !open
                    {
                        continue;
                    }
                    let color = match item.status {
                        ActivityStatus::Failed => p.danger,
                        ActivityStatus::Running => p.accent,
                        ActivityStatus::Completed => p.success,
                        ActivityStatus::Stopped => p.muted,
                    };
                    let mark = if item.status == ActivityStatus::Running && live {
                        ui::spinner(13., color).into_any_element()
                    } else {
                        ui::icon(
                            match item.status {
                                ActivityStatus::Completed => ic::CHECK,
                                ActivityStatus::Failed => ic::CLOSE_CIRCLE,
                                ActivityStatus::Stopped => ic::STOP,
                                ActivityStatus::Running => ic::CLOCK,
                            },
                            13.,
                            color,
                        )
                        .into_any_element()
                    };
                    let id = agent.id.clone();
                    let message = item.message.message.id.clone();
                    let row = h_flex()
                        .id(SharedString::from(format!("workflow-tool-{message}")))
                        .min_h(px(30.))
                        .gap(px(8.))
                        .px(px(4.))
                        .rounded(px(6.))
                        .cursor_pointer()
                        .hover(|d| d.bg(p.hover))
                        .child(mark)
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                .ts(12., 18.)
                                .child(item.title.clone()),
                        )
                        .child(
                            div()
                                .ts(10., 14.)
                                .text_color(color)
                                .child(self.tr(item.status.label())),
                        )
                        .child(ui::icon(
                            if open {
                                ic::CHEVRON_DOWN
                            } else {
                                ic::CHEVRON_RIGHT
                            },
                            12.,
                            p.faint,
                        ))
                        .tooltip(ui::tip(self.fmt("{0} · {1} · Click to {2} details", &[&(item.title).to_string(), (self.tr(item.status.label())), (if open { self.tr("hide") } else { self.tr("show") })])))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.deck.workflow.toggle_tool(&id, &message);
                            cx.notify();
                        }));
                    body = body.child(row);
                    if open {
                        body = body.child(
                            ui::code_block_localized(
                                SharedString::from(format!(
                                    "workflow-output-{}",
                                    item.message.message.id
                                )),
                                item.message.text.clone(),
                                true,
                                &p, self.language())
                            .max_h(px(260.))
                            .overflow_y_scroll(),
                        );
                    }
                }
                if activity.len() > 4 {
                    let id = agent.id.clone();
                    body = body.child(
                        Btn::new(
                            "workflow-activity-more",
                            if expanded {
                                self.tr("Show recent activity").into()
                            } else {
                                self.fmt("Show all {0} tools", &[&(activity.len()).to_string()])
                            },
                        )
                        .small()
                        .on_click(cx.listener(move |this, _, _, cx| {
                            toggle(&mut this.deck.workflow.full_activity, &id);
                            cx.notify();
                        })),
                    );
                }
            } else if agent.workflow.is_none() && agent.workflow_runs.is_empty() {
                body = body.child(div().ts(12., 18.).text_color(p.muted).child(if live {
                    self.tr("Waiting for the agent’s next step…")
                } else {
                    self.tr("Plans and tool activity will appear here.")
                }));
            }
        }
        ui::section_with(
            self.tr("Workflow"),
            vec![div()
                .ts(11., 16.)
                .text_color(p.muted)
                .child(status)
                .into_any_element()],
            body,
            &p,
        )
    }
}
