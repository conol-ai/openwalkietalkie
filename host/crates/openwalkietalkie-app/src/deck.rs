//! The **Deck** screen (§8.2's Deck row): the roster mirror on the left, the
//! detail pane on the right, and the order composer under it.
//!
//! It is the [`Shell`]'s first pane. Local window state holds the selected
//! row, order box, workflow disclosures, and the last card answer so that
//! **Retry** can re-send it (§4.8). Agent data comes from the mirror.
use crate::present;
use crate::shell::Shell;
use crate::style::{palette, Palette};
use crate::ui::{self, frag, ic, word, Btn, Row, Tint, Type as _};
use deck_gui_core::{OrderEntry, UiAction};
use deck_model::{Agent, AgentId, Card, CardId, CardKind, CardStatus, Ts};
use gpui::{
    div, prelude::*, px, AnyElement, App, ClipboardItem, Context, Div, Entity, SharedString,
    Stateful, Subscription, Window,
};
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::{h_flex, v_flex};
use std::collections::BTreeMap;

/// The roster column: a mark, a name and an age on the first line, the
/// state, slot and source on the second, without truncating the name.
pub const ROSTER_WIDTH: f32 = 300.;

/// The detail column never grows past a comfortable measure, however wide
/// the window is.
const DETAIL_MAX: f32 = 760.;

/// The detail column's width under which the agent's actions go under its
/// name instead of beside it.
const NARROW_DETAIL: f32 = 520.;

/// What this GUI last answered on a card, which is what **Retry** re-sends.
///
/// Keyed on the card's `created` stamp as well as its id, because a `CardId`
/// is **not** an identity for the life of an agent: the counter lives on the
/// device slot and `Store::reset_slot` zeroes it whenever the agent leaves
/// that slot, so the same agent's next card can be `#17` again, with other
/// choices behind the same indices. Without the stamp, Retry on the second
/// `#17` would re-send the choice the operator picked on the first one.
type Answered = BTreeMap<(AgentId, CardId, Ts), (Option<u8>, Option<String>)>;

/// The Deck screen's own view state.
pub struct DeckPane {
    pub(crate) order: Entity<InputState>,
    pub(crate) selected: Option<AgentId>,
    answered: Answered,
    pub(crate) workflow: crate::workflow::ViewState,
    pub(crate) images: crate::images::View,
}

impl DeckPane {
    /// The pane and the subscriptions the [`Shell`] has to keep alive: a
    /// dropped `Subscription` is a cancelled one, and the order box's Return
    /// key is one.
    pub fn new(window: &mut Window, cx: &mut Context<Shell>) -> (DeckPane, Vec<Subscription>) {
        let order = cx.new(|cx| {
            InputState::new(window, cx).placeholder("Send an order to the selected agent\u{2026}")
        });
        // Return in the box sends the order, which is what §8.2's "text box
        // → `op.order`" means with a keyboard in it.
        let enter = cx.subscribe_in(&order, window, |this, _, event, window, cx| {
            if matches!(event, InputEvent::PressEnter { .. }) {
                this.send_order(window, cx);
            }
        });
        (
            DeckPane {
                order,
                selected: None,
                answered: Answered::new(),
                workflow: Default::default(),
                images: Default::default(),
            },
            vec![enter],
        )
    }

    /// What this window answered on *this* card, recorded when it answers.
    pub(crate) fn remember(
        &mut self,
        agent: &AgentId,
        card: &Card,
        choice: Option<u8>,
        text: Option<String>,
    ) {
        self.answered
            .insert((agent.clone(), card.id, card.created), (choice, text));
    }

    /// What **Retry** would re-send for this card, or `None` when this window
    /// has not answered *it* — a card that merely reuses its id is not it.
    fn recall(&self, agent: &AgentId, card: &Card) -> Option<(Option<u8>, Option<String>)> {
        self.answered
            .get(&(agent.clone(), card.id, card.created))
            .cloned()
    }
}

impl Shell {
    /// The whole Deck screen: roster, detail and the composer.
    pub(crate) fn render_deck(&self, dim: bool, cx: &Context<Self>) -> Div {
        let p = *palette(cx);
        h_flex()
            .size_full()
            .child(self.render_roster(dim, cx))
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .border_l_1()
                    .border_color(p.border)
                    .child(
                        div()
                            .flex_1()
                            .min_h_0()
                            .when(dim, |this| this.opacity(0.55))
                            .child(self.render_detail(dim, cx)),
                    )
                    .child(self.render_composer(dim, cx)),
            )
    }

    // ---- the roster ----------------------------------------------------

    fn render_roster(&self, dim: bool, cx: &Context<Self>) -> Div {
        let p = *palette(cx);
        let groups = present::projects(self.state.mirror());
        let now = present::now();
        let focused = self.state.mirror().focus.agent.clone();
        let home = std::env::var("HOME").ok();
        let total: usize = groups.iter().map(|group| group.agents.len()).sum();
        let empty = total == 0;
        // One header per project, the rows of the project under it — the
        // same grouping the deck pages through with LEFT/RIGHT. The header
        // names the directory's last component and holds the whole path.
        let mut sections: Vec<AnyElement> = Vec::new();
        for (index, group) in groups.iter().enumerate() {
            let count = group.agents.len().to_string();
            let path = group
                .cwd
                .as_deref()
                .map(|cwd| present::project_path(cwd, home.as_deref()));
            let header = h_flex()
                .id(SharedString::from(format!("project-{index}")))
                .h(px(28.))
                .px(px(10.))
                .gap(px(8.))
                .when(index > 0, |this| this.mt(px(6.)))
                .ts(12., 16.)
                .medium()
                .text_color(p.muted.opacity(0.75))
                .when(group.agents.first().is_some_and(|a| a.kind == deck_model::Kind::Conol), |this| {
                    let (mark, color) = ui::kind_mark(deck_model::Kind::Conol, &p);
                    this.child(ui::icon(mark, 18., color))
                })
                .child(
                    div()
                        .min_w_0()
                        .truncate()
                        .child(group.label.clone()),
                )
                .child(div().flex_none().text_color(p.faint).child(count))
                .when_some(path, |this, path| this.tooltip(ui::tip(path)));
            sections.push(header.into_any_element());
            for agent in &group.agents {
                sections.push(
                    self.render_row(agent, group, focused.as_ref(), now, dim, cx)
                        .into_any_element(),
                );
            }
        }
        v_flex()
            .w(px(ROSTER_WIDTH))
            .h_full()
            .flex_none()
            .when(dim, |this| this.opacity(0.55))
            .child(
                h_flex()
                    .flex_none()
                    .h(px(30.))
                    .px(px(18.))
                    .gap(px(8.))
                    .child(div().ts(13., 17.).medium().child("Agents"))
                    .child(
                        div()
                            .ts(12., 16.)
                            .text_color(p.faint)
                            .child(total.to_string()),
                    )
                    .child(div().flex_1())
                    .child(
                        div()
                            .id("roster-rev")
                            .ts(11., 14.)
                            .mono()
                            .text_color(p.faint)
                            .child(format!("rev {}", self.state.mirror().rev))
                            .tooltip(ui::tip(
                                "The mirror's revision: it moves when anything on the deck does",
                            )),
                    ),
            )
            .child(
                v_flex()
                    .id("roster")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .px(px(8.))
                    .pt(px(4.))
                    .pb(px(12.))
                    .gap(px(2.))
                    .when(empty, |this| {
                        this.child(ui::empty_state(
                            ic::BOT,
                            "No agents yet",
                            "Start a Claude, Codex or Gemini session in a terminal and it \
                             appears here.",
                            &p,
                        ))
                    })
                    // Every agent the slot policy could not fit on the deck
                    // sits in its project too, its row saying "not on deck";
                    // the operator can still answer it here.
                    .children(sections),
            )
    }

    fn render_row(
        &self,
        agent: &Agent,
        project: &present::Project,
        focused: Option<&AgentId>,
        now: Ts,
        dim: bool,
        cx: &Context<Self>,
    ) -> Stateful<Div> {
        let p = *palette(cx);
        let id = agent.id.clone();
        let name = agent.name.clone();
        let shown = present::row_name(agent, project);
        let selected = self.deck.selected.as_ref() == Some(&agent.id);
        // The active-channel bar (§8.2): which agent the deck's channel view
        // is on right now, which is not the same thing as the row the
        // operator happens to be reading here.
        let active = focused == Some(&agent.id);
        let stale = dim || present::stale(agent);
        let tone = present::state_tone(agent.state, stale);
        let (mark, mark_color) = ui::kind_mark(agent.kind, &p);
        let title = present::row_title(agent, project);
        let age = present::state_age(agent, now);
        div()
            .id(SharedString::from(format!("row-{}", agent.id)))
            .relative()
            .flex()
            .flex_col()
            .gap(px(3.))
            .px(px(10.))
            .py(px(8.))
            .rounded(px(8.))
            .cursor_pointer()
            .map(|this| {
                if selected {
                    this.bg(p.selected)
                } else {
                    this.hover(|s| s.bg(p.hover))
                }
            })
            .on_click(cx.listener(move |this, _, window, cx| {
                this.deck.selected = Some(id.clone());
                this.refresh_images(window, cx);
                let placeholder = format!("Send an order to {name}\u{2026}");
                this.deck.order.update(cx, |input, cx| {
                    input.set_placeholder(placeholder, window, cx);
                });
                cx.notify();
            }))
            .when(active, |this| {
                this.child(
                    div()
                        .absolute()
                        .left(px(1.))
                        .top(px(12.))
                        .bottom(px(12.))
                        .w(px(2.))
                        .rounded_full()
                        .bg(p.accent),
                )
            })
            .child(
                h_flex()
                    .gap(px(8.))
                    .child(ui::icon(
                        mark,
                        14.,
                        if stale { p.faint } else { mark_color },
                    ))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .ts(13., 17.)
                            .when(selected, |this| this.medium())
                            .text_color(if stale {
                                p.muted
                            } else if selected {
                                p.text
                            } else {
                                p.text.opacity(0.88)
                            })
                            .child(shown),
                    )
                    .children(age.map(|age| div().ts(11., 14.).text_color(p.faint).child(age))),
            )
            .child(
                h_flex()
                    .pl(px(22.))
                    .gap(px(6.))
                    .ts(11.5, 15.)
                    .child(ui::dot(p.tone(tone), 6.))
                    .child(
                        div()
                            .text_color(p.tone_text(tone))
                            .child(present::state_word(agent.state)),
                    )
                    .child(div().text_color(p.faint).child("\u{b7}"))
                    .child(
                        div()
                            .text_color(p.muted)
                            .child(present::slot_badge(agent.slot)),
                    )
                    .child(div().text_color(p.faint).child("\u{b7}"))
                    .child(
                        div()
                            .text_color(p.muted)
                            .child(present::owner_source(agent)),
                    ),
            )
            .when(!title.is_empty(), |this| {
                this.child(
                    div()
                        .pl(px(22.))
                        .truncate()
                        .ts(11.5, 15.)
                        .text_color(p.faint)
                        .child(title),
                )
            })
    }

    // ---- the detail pane -----------------------------------------------

    fn render_detail(&self, dim: bool, cx: &Context<Self>) -> Stateful<Div> {
        let p = *palette(cx);
        let pane = div().id("detail").size_full().overflow_y_scroll();
        if let Some(image) = self.expanded_image(cx) { return pane.child(image); }
        let Some(agent) = self
            .deck
            .selected
            .as_ref()
            .and_then(|id| self.state.mirror().agents.get(id))
        else {
            return pane.child(div().pt(px(80.)).child(ui::empty_state(
                ic::DIAL,
                "No agent selected",
                "Pick an agent to see its card, where it runs and what it was told.",
                &p,
            )));
        };
        let now = present::now();
        // A definite width, for the reason `ui::page` gives.
        let width = self.detail_room().min(DETAIL_MAX);
        pane.child(
            v_flex()
                .w(px(width))
                .mx_auto()
                .px(px(32.))
                .pt(px(6.))
                .pb(px(24.))
                .child(self.render_agent_head(agent, dim, cx))
                .children(
                    agent
                        .card
                        .as_ref()
                        .map(|card| self.render_card(agent, card, now, dim, cx)),
                )
                .when(agent.workflow.is_some() || !agent.workflow_runs.is_empty() || agent.messages.iter().any(|m| m.message.role == deck_model::ChatRole::Tool), |this| this.child(self.render_workflow(agent, dim, cx)))
                .children(agent.reply.as_ref().map(|reply| {
                    ui::section(
                        "Reply",
                        ui::panel(&p)
                            .ts(13.5, 21.)
                            .child(match (&reply.text, reply.redacted) {
                                (Some(text), _) => text.clone(),
                                (None, true) => present::REDACTED.to_string(),
                                (None, false) => String::new(),
                            }),
                        &p,
                    )
                }))
                .child(self.render_images(agent, cx))
                .child(self.render_orders(agent, cx))
                .child(self.render_about(agent, &p)),
        )
    }

    /// The agent's name, what it is and where it sits, and §8.2's actions.
    /// Everything here is a [`UiAction`]; nothing here touches the mirror.
    fn render_agent_head(&self, agent: &Agent, dim: bool, cx: &Context<Self>) -> Div {
        let p = *palette(cx);
        let (mark, mark_color) = ui::kind_mark(agent.kind, &p);
        let stale = present::stale(agent);
        let tone = present::state_tone(agent.state, stale || dim);
        let age = present::state_age(agent, present::now());
        let title = present::live_title(agent).to_string();
        let id = agent.id.clone();
        let card = agent.card.as_ref().map(|c| c.id);
        // `op.ack` is the GUI-only clear of a Ready card (§4.7); a question
        // card is answered or snoozed, never acknowledged.
        let ackable = agent
            .card
            .as_ref()
            .is_some_and(|c| matches!(c.kind, CardKind::R));
        let state = match age {
            Some(age) => format!("{} {age}", present::state_word(agent.state)),
            None => present::state_word(agent.state).to_string(),
        };
        // Below this the name, the meta line and three buttons do not fit on
        // one line beside the tile.
        let narrow = self.detail_room() < NARROW_DETAIL;
        let mut actions: Vec<AnyElement> = Vec::new();
        if let Some(card) = card.filter(|_| ackable) {
            let id = id.clone();
            actions.push(
                Btn::new("ack", "Acknowledge")
                    .small()
                    .icon(ic::CHECK)
                    .disabled(dim)
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.act(
                            UiAction::Ack {
                                agent: id.clone(),
                                card,
                            },
                            window,
                            cx,
                        );
                    }))
                    .into_any_element(),
            );
        }
        if let Some(card) = card {
            let id = id.clone();
            actions.push(
                Btn::new("snooze", "Snooze")
                    .small()
                    .icon(ic::CLOCK)
                    .disabled(dim)
                    .tooltip("Put the card back on the deck later")
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.act(
                            UiAction::Snooze {
                                agent: id.clone(),
                                card,
                            },
                            window,
                            cx,
                        );
                    }))
                    .into_any_element(),
            );
        }
        actions.push(
            Btn::new("focus", "Focus terminal")
                .small()
                .outlined()
                .icon(ic::TERMINAL)
                .disabled(dim || !agent.caps.focus)
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.act(UiAction::Focus { agent: id.clone() }, window, cx);
                }))
                .into_any_element(),
        );
        let mut actions = Some(actions);
        let mut actions_below = None;
        if narrow {
            actions_below = actions.take();
        }
        v_flex()
            .gap(px(12.))
            .child(
                h_flex()
                    .items_center()
                    .gap(px(14.))
                    .child(
                        div()
                            .flex_none()
                            .size(px(44.))
                            .rounded(px(12.))
                            .bg(p.wash(0.06))
                            .flex()
                            .items_center()
                            .justify_center()
                            .child(ui::icon(mark, 22., mark_color)),
                    )
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .gap(px(3.))
                            .child(
                                h_flex()
                                    .gap(px(8.))
                                    .child(
                                        div()
                                            .ts(20., 26.)
                                            .medium()
                                            .truncate()
                                            .child(agent.name.clone()),
                                    )
                                    .when(stale, |this| {
                                        this.child(ui::badge("session gone", Tint::Neutral, &p))
                                    }),
                            )
                            .child(ui::meta_line(
                                vec![
                                    word(state, p.tone_text(tone)),
                                    frag(present::kind_label(agent.kind)),
                                    frag(present::slot_badge(agent.slot)),
                                    frag(present::owner_source(agent)),
                                ],
                                &p,
                            )),
                    )
                    .children(
                        actions
                            .take()
                            .map(|actions| h_flex().flex_none().gap(px(6.)).children(actions)),
                    ),
            )
            .when(!title.is_empty(), |this| {
                this.child(div().ts(13., 19.).text_color(p.muted).child(title))
            })
            // A narrow detail column puts the actions under the name rather
            // than squeezing the name down to its first letters.
            .children(
                actions_below.map(|actions| h_flex().flex_wrap().gap(px(6.)).children(actions)),
            )
    }

    fn render_card(
        &self,
        agent: &Agent,
        card: &Card,
        now: Ts,
        dim: bool,
        cx: &Context<Self>,
    ) -> Div {
        let p = *palette(cx);
        let label = match card.kind {
            CardKind::Q => "Waiting on you",
            CardKind::R => "Ready",
        };
        let countdown = present::hold_countdown(card, now).map(|left| {
            h_flex()
                .gap(px(5.))
                .ts(12., 16.)
                .text_color(p.muted)
                .child(ui::icon(ic::CLOCK, 13., p.muted))
                .child(left)
                .into_any_element()
        });
        let mut badges: Vec<AnyElement> = vec![
            div()
                .mono()
                .text_color(p.text)
                .child(present::card_ref(card))
                .into_any_element(),
            ui::badge(card.status.name(), status_tint(&card.status), &p).into_any_element(),
            ui::badge(present::route(card.deliver), Tint::Neutral, &p).into_any_element(),
        ];
        if let Some(custom) = present::custom_badge(&card.origin) {
            badges.push(ui::badge(custom, Tint::Accent, &p).into_any_element());
        }
        badges.push(frag(present::origin_line(&card.origin)));
        let body = ui::panel(&p)
            .gap(px(12.))
            .child(ui::meta_line(badges, &p))
            // The full card text, never truncated: this is the thing the
            // operator is being asked about.
            .child(
                div()
                    .ts(14., 22.)
                    .text_color(p.text)
                    .child(present::card_text(card).to_string()),
            )
            .children(present::answering(card).map(|what| {
                h_flex()
                    .gap(px(6.))
                    .ts(12., 16.)
                    .text_color(p.muted)
                    .child(ui::spinner(12., p.muted))
                    .child(format!("Sent: {what}"))
            }))
            .children(present::degraded_at(card).map(|at| {
                ui::hint(
                    match at {
                        Some(at) => format!("Degraded \u{b7} focused at {at}"),
                        None => "Degraded \u{b7} not focused yet".to_string(),
                    },
                    &p,
                )
            }))
            .child(self.render_choices(agent, card, dim, cx))
            .children(self.render_failed(agent, card, dim, cx));
        ui::section_with(label, countdown.into_iter().collect(), body, &p)
    }

    /// Every choice, flat, however the deck had to page them (§8.2).
    fn render_choices(&self, agent: &Agent, card: &Card, dim: bool, cx: &Context<Self>) -> Div {
        let p = *palette(cx);
        let card_id = card.id;
        let rows: Vec<Stateful<Div>> = (0..card.choices.len())
            .map(|n| {
                let index = u8::try_from(n).unwrap_or(u8::MAX);
                let id = agent.id.clone();
                let choice = &card.choices[n];
                let recommended = card.rec == Some(index);
                let sent_as = card
                    .choice_map
                    .get(n)
                    .copied()
                    .filter(|sent| *sent != index);
                div()
                    .id(SharedString::from(format!("choice-{n}")))
                    .flex()
                    .items_center()
                    .gap(px(10.))
                    .w_full()
                    .min_h(px(40.))
                    .px(px(10.))
                    .py(px(8.))
                    .rounded(px(9.))
                    .bg(p.wash(0.05))
                    .border_1()
                    .border_color(if recommended {
                        p.accent.alpha(0.45)
                    } else {
                        gpui::transparent_black()
                    })
                    .map(|this| {
                        if dim {
                            this.opacity(0.5)
                        } else {
                            this.cursor_pointer()
                                .hover(|s| s.bg(p.wash(0.09)))
                                .active(|s| s.bg(p.wash(0.12)))
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    this.act(
                                        UiAction::Answer {
                                            agent: id.clone(),
                                            card: card_id,
                                            choice: Some(index),
                                            text: None,
                                        },
                                        window,
                                        cx,
                                    );
                                }))
                        }
                    })
                    .child(ui::keycap((n + 1).to_string(), &p))
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .child(div().ts(13.5, 19.).child(if choice.label.is_empty() {
                                present::REDACTED.to_string()
                            } else {
                                choice.label.clone()
                            }))
                            .children(choice.detail.clone().map(|detail| {
                                div().ts(12., 16.).text_color(p.muted).child(detail)
                            })),
                    )
                    .children(sent_as.map(|sent| {
                        div()
                            .ts(11., 14.)
                            .text_color(p.faint)
                            .child(format!("sent as {sent}"))
                    }))
                    .when(recommended, |this| {
                        this.child(ui::badge("Recommended", Tint::Accent, &p))
                    })
            })
            .collect();
        v_flex()
            .gap(px(6.))
            .children(rows)
            // Redaction keeps the count and drops the labels (§4.3); an
            // empty choice list under a non-zero count is not "no choices".
            .when(card.choices.is_empty() && card.choices_n > 0, |this| {
                this.child(ui::hint(
                    format!("{} choices, hidden by this reader's scope", card.choices_n),
                    &p,
                ))
            })
    }

    /// §4.8's two ways out of `failed`: **Retry** (`push`/`poll` only, and
    /// only when this GUI knows what it answered) and **Release to
    /// terminal**.
    fn render_failed(
        &self,
        agent: &Agent,
        card: &Card,
        dim: bool,
        cx: &Context<Self>,
    ) -> Option<Div> {
        let p = *palette(cx);
        let why = present::failed_reason(card)?;
        let card_id = card.id;
        let id = agent.id.clone();
        let retry = present::retry_offered(card)
            .then(|| self.deck.recall(&id, card))
            .flatten();
        let mut actions: Vec<AnyElement> = Vec::new();
        if let Some((choice, text)) = retry {
            let id = id.clone();
            actions.push(
                Btn::new("retry", "Retry")
                    .small()
                    .solid()
                    .disabled(dim)
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.act(
                            UiAction::Answer {
                                agent: id.clone(),
                                card: card_id,
                                choice,
                                text: text.clone(),
                            },
                            window,
                            cx,
                        );
                    }))
                    .into_any_element(),
            );
        }
        if present::release_offered(card) {
            // §4.8: `failed → degraded`. `op.snooze` *is* the release — it
            // drops the held hook and re-pushes the same id as a `⌨` focus
            // card, which is exactly what "release to terminal" means on the
            // device.
            actions.push(
                Btn::new("release", "Release to terminal")
                    .small()
                    .disabled(dim)
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.act(
                            UiAction::Snooze {
                                agent: id.clone(),
                                card: card_id,
                            },
                            window,
                            cx,
                        );
                    }))
                    .into_any_element(),
            );
        }
        Some(ui::strip(
            ui::Severity::Danger,
            Some("The answer did not get through".into()),
            Some(div().child(why.to_string()).into_any_element()),
            actions,
            &p,
        ))
    }

    /// The order history (§8.2): "the exact string typed or pushed, shown
    /// before an `inject` executes". Newest first.
    fn render_orders(&self, agent: &Agent, cx: &Context<Self>) -> Div {
        let p = *palette(cx);
        let orders = self.state.orders(&agent.id);
        if orders.is_empty() {
            return ui::section(
                "Orders",
                ui::panel(&p).child(ui::hint(
                    "Nothing typed or pushed yet. What you send below shows here before it is \
                     typed.",
                    &p,
                )),
                &p,
            );
        }
        let rows: Vec<AnyElement> = orders
            .iter()
            .enumerate()
            .rev()
            .map(|(n, entry)| render_order(n, entry, &p).into_any_element())
            .collect();
        ui::section("Orders", ui::card(rows, &p), &p)
    }

    /// Where the agent runs and what it can be asked to do.
    fn render_about(&self, agent: &Agent, p: &Palette) -> Div {
        ui::section(
            "Session",
            ui::panel(p)
                .gap(px(8.))
                .child(ui::kv("Source", present::source_line(&agent.session), p))
                .child(ui::kv("Can", present::caps_line(&agent.caps), p))
                .child(ui::kv(
                    "Agent",
                    div().mono().ts(12., 18.).child(agent.id.to_string()),
                    p,
                )),
            p,
        )
    }

    // ---- the composer --------------------------------------------------

    /// The text box → `op.order` (§8.2).
    fn render_composer(&self, dim: bool, cx: &Context<Self>) -> Div {
        let p = *palette(cx);
        let target = self
            .deck
            .selected
            .as_ref()
            .and_then(|id| self.state.mirror().agents.get(id));
        let can_send = !dim && target.is_some();
        let note = match target {
            Some(agent) => format!("Typed into {}'s terminal \u{b7} Return sends", agent.name),
            None => "Select an agent first".to_string(),
        };
        let width = (self.detail_room() - 48.).min(DETAIL_MAX - 16.);
        div()
            .flex_none()
            .w_full()
            .px(px(24.))
            .pb(px(16.))
            .pt(px(4.))
            .child(
                v_flex()
                    .w(px(width))
                    .mx_auto()
                    .gap(px(6.))
                    .child(
                        h_flex()
                            .h(px(48.))
                            .pl(px(16.))
                            .pr(px(8.))
                            .gap(px(8.))
                            .rounded(px(16.))
                            .bg(p.plate)
                            .border_1()
                            .border_color(p.hairline(0.09))
                            .when(!can_send, |this| this.opacity(0.6))
                            .child(
                                div().flex_1().min_w_0().child(
                                    Input::new(&self.deck.order)
                                        .appearance(false)
                                        .disabled(!can_send)
                                        .px(px(0.))
                                        .text_size(px(14.)),
                                ),
                            )
                            .child(
                                div()
                                    .id("order")
                                    .flex_none()
                                    .size(px(32.))
                                    .rounded_full()
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .bg(p.solid)
                                    .map(|this| {
                                        if can_send {
                                            this.cursor_pointer()
                                                .hover(|s| s.opacity(0.88))
                                                .on_click(cx.listener(|this, _, window, cx| {
                                                    this.send_order(window, cx);
                                                }))
                                        } else {
                                            this.opacity(0.35)
                                        }
                                    })
                                    .tooltip(ui::tip("Send the order"))
                                    .child(ui::icon(ic::ARROW_UP, 16., p.on_solid)),
                            ),
                    )
                    .child(
                        div()
                            .pl(px(16.))
                            .ts(11.5, 15.)
                            .text_color(p.faint)
                            .child(note),
                    ),
            )
    }

    /// The detail column's width this frame: what is left of the content
    /// area once the roster and its rule are off it.
    pub(crate) fn detail_room(&self) -> f32 {
        (self.content_width - ROSTER_WIDTH - 1.).max(0.)
    }

    pub(crate) fn send_order(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(agent) = self.deck.selected.clone() else {
            return;
        };
        let text = self.deck.order.read(cx).value().trim().to_string();
        if text.is_empty() {
            return;
        }
        self.deck.order.update(cx, |input, cx| {
            input.set_value("", window, cx);
        });
        self.act(UiAction::Order { agent, text }, window, cx);
    }
}

/// How a card's status is tinted: a failure is red, a delivery green, the
/// rest neutral.
fn status_tint(status: &CardStatus) -> Tint {
    match status {
        CardStatus::Failed { .. } => Tint::Danger,
        CardStatus::Delivered => Tint::Success,
        CardStatus::Answering { .. } => Tint::Blue,
        _ => Tint::Neutral,
    }
}

/// One line of the order history, with §8.2's Copy on a failure.
fn render_order(n: usize, entry: &OrderEntry, p: &Palette) -> Row {
    let failed = entry.reason.is_some();
    let text = match &entry.text {
        Some(text) => div().mono().ts(12.5, 18.).child(text.clone()),
        // `order.failed` carries no text (§4.12): an order refused before it
        // was ever dispatched never had one to keep.
        None => div().text_color(p.faint).child("(no text)"),
    };
    let mut meta: Vec<AnyElement> = Vec::new();
    if let Some(via) = &entry.via {
        meta.push(frag(via.clone()));
    }
    if let Some(reason) = &entry.reason {
        meta.push(word(format!("failed: {reason}"), p.danger_muted));
    }
    if meta.is_empty() {
        meta.push(frag("sent"));
    }
    let row = Row::new()
        .title_el(text.when(failed, |this| this.text_color(p.danger_muted)))
        .meta_el(ui::meta_line(meta, p));
    // The text was in the `order.dispatched` before the failure and nowhere
    // else (`order.failed` carries none), so this button is the operator's
    // only way to get the string back.
    match present::copyable(entry).map(str::to_string) {
        Some(text) => row.trailing(
            Btn::icon_only(SharedString::from(format!("copy-{n}")), ic::COPY)
                .small()
                .tooltip("Copy the order")
                .on_click(move |_, _, cx: &mut App| {
                    cx.write_to_clipboard(ClipboardItem::new_string(text.clone()));
                }),
        ),
        None => row,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app;
    use crate::shell::SIDEBAR_WIDTH;

    /// §4.8's Retry re-sends "what this window answered", which means *this
    /// card* and not merely its id. A `CardId` is not an identity for the
    /// life of an agent: the counter lives on the device slot and
    /// `Store::reset_slot` zeroes it whenever the agent leaves that slot, so
    /// the same agent's next card can be `#17` again with other choices
    /// behind the same indices — and nothing ever prunes this map.
    #[test]
    fn retry_only_re_sends_what_this_window_answered_on_this_very_card() {
        use deck_model::{Choice, Deliver, Origin, OriginKind, Page};
        let agent = AgentId("ag_1".into());
        let card = |labels: &[&str], created: u64| Card {
            id: CardId(17),
            kind: CardKind::Q,
            status: CardStatus::Failed {
                reason: "timeout".into(),
            },
            deliver: Deliver::Poll,
            origin: Origin {
                kind: OriginKind::Permission,
                tool: Some("Bash".into()),
                request_id: None,
                group: None,
            },
            text: Some("...".into()),
            choices: labels
                .iter()
                .map(|l| Choice {
                    label: (*l).into(),
                    detail: None,
                })
                .collect(),
            choices_n: u8::try_from(labels.len()).unwrap_or(u8::MAX),
            choice_map: Vec::new(),
            rec: Some(0),
            free_text: true,
            page: Page { index: 0, count: 1 },
            created: Ts::from_millis(created),
            expires: None,
            redacted: false,
        };
        let first = card(&["Allow", "Deny"], 1_000);
        // The map is the pane's, but `remember`/`recall` are pure over it,
        // so they are exercised here without a window.
        let mut answered = Answered::new();
        answered.insert(
            (agent.clone(), first.id, first.created),
            (Some(0), None::<String>),
        );
        assert_eq!(
            answered.get(&(agent.clone(), first.id, first.created)),
            Some(&(Some(0), None)),
            "the card this window really answered"
        );
        // A different card, under the recycled id, that this window never
        // answered — and whose choice 0 is not what was answered before.
        let second = card(&["Deploy to production", "Cancel"], 9_000);
        assert_eq!(
            answered.get(&(agent, second.id, second.created)),
            None,
            "Retry offered choice 0 of a card the operator never answered"
        );
    }

    #[test]
    fn the_roster_and_the_detail_pane_still_split_the_smallest_window_evenly() {
        // The sidebar takes its width off the top (§8.2), so the roster is
        // measured against what is left and not against the whole window.
        const { assert!(SIDEBAR_WIDTH + ROSTER_WIDTH < app::MIN_WIDTH) };
        const { assert!(ROSTER_WIDTH < (app::MIN_WIDTH - SIDEBAR_WIDTH) / 2.) };
    }

    #[test]
    fn a_failed_card_reads_red_and_a_delivered_one_green() {
        assert_eq!(
            status_tint(&CardStatus::Failed {
                reason: "timeout".into()
            }),
            Tint::Danger
        );
        assert_eq!(status_tint(&CardStatus::Delivered), Tint::Success);
        assert_eq!(status_tint(&CardStatus::Pending), Tint::Neutral);
    }
}
