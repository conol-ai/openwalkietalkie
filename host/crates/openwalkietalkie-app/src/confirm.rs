//! The confirmation sheet's decision, as a pure function (§4.2, §8.2).
//!
//! `deckd` refuses a guarded operation from an untrusted principal with
//! `-32002 {need:"trusted", how:"gui_confirm"}` and emits
//! `op.confirm_requested{agent, card?, order?, principal}`; the reducer turns
//! that into `HostEffect::RaisePermissionSheet` with a [`ConfirmRequest`].
//! The sheet is the offer to **re-issue** that call from the GUI, which is
//! the trusted principal.
//!
//! Two consequences the rest of this module exists to keep straight.
//!
//! The first is that the event carries **no choice** (§4.12). The daemon knows
//! the untrusted caller asked for an allow; it does not say which index that
//! was. So the sheet finds the allow itself, in
//! [`present::allow_choice`](crate::present::allow_choice), from the card's
//! own labels and `deck_model::guard`'s vocabulary — the same words the
//! daemon guards on.
//!
//! The second is that **dismissing must deny**. Escape, the overlay and the
//! close button all route to [`Decision::Deny`], never to "nothing happened
//! and the request is still open": a sheet the operator waves away must not
//! be able to allow, and it must not leave an agent blocked on a prompt the
//! operator has already decided against. Where the card offers no deny at all
//! the honest answer is to send nothing — the guarded call was already
//! refused, so silence is still a refusal — and never to fall back to a
//! choice that might be an allow.
use crate::present;
use deck_gui_core::{ConfirmRequest, UiAction};
use deck_model::Card;

/// What the operator did with the sheet. A dismissal is a [`Decision::Deny`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Decision {
    Allow,
    Deny,
}

/// The call `decision` re-issues, or `None` when there is nothing to send.
///
/// `card` is the request's card as the **mirror** holds it now, not as it was
/// when the sheet went up: a card that has since been answered, cleared or
/// replaced has no choices to re-issue, and the sheet's click does nothing.
pub fn action(
    request: &ConfirmRequest,
    card: Option<&Card>,
    decision: Decision,
) -> Option<UiAction> {
    let agent = request.agent.clone()?;
    // An `op.order` that would have been injected. There is nothing to deny:
    // the daemon already refused it, and the text was never typed.
    if let Some(text) = &request.order {
        return match decision {
            Decision::Allow => Some(UiAction::Order {
                agent,
                text: text.clone(),
            }),
            Decision::Deny => None,
        };
    }
    let id = request.card?;
    let card = card?;
    if card.id != id {
        return None;
    }
    let choice = match decision {
        Decision::Allow => present::allow_choice(card)?,
        Decision::Deny => present::deny_choice(card)?,
    };
    Some(UiAction::Answer {
        agent,
        card: id,
        choice: Some(choice),
        text: None,
    })
}

/// Whether the sheet offers an **Allow** button at all. A permission card
/// with no allow among its choices, or a request whose card the mirror has
/// already moved past, can only be denied.
///
/// So can anything at all when `trusted` is false. The sheet's Allow works by
/// re-issuing the call **as this app**, and that only means something if this
/// app is §4.2's trusted principal: on an unverified build (`principal
/// .trusted:false`, the ad-hoc dev build) the re-issue is refused `-32002`
/// like the first attempt, and the daemon answers it with another
/// `op.confirm_requested` — which raises this sheet again and pulls the window
/// forward again, for every click, for ever. An Allow button there is a loop
/// and not an offer.
pub fn allow_offered(request: &ConfirmRequest, card: Option<&Card>, trusted: bool) -> bool {
    trusted && action(request, card, Decision::Allow).is_some()
}

/// The sheet's one-line summary: who asked, and for what.
pub fn summary(request: &ConfirmRequest, card: Option<&Card>) -> String {
    summary_localized(request, card, deck_gui_core::i18n::Language::English)
}

pub fn summary_localized(request: &ConfirmRequest, card: Option<&Card>, language: deck_gui_core::i18n::Language) -> String {
    let who = request
        .principal
        .as_ref()
        .map(ToString::to_string)
        .unwrap_or_else(|| language.text("an untrusted client").to_string());
    if let Some(text) = &request.order {
        return language.format("{0} asked to type “{1}” into the terminal.", &[&who, text]);
    }
    match card {
        Some(card) => language.format(
            "{0} asked to allow {1} · {2}.",
            &[&who, &present::card_ref(card), &present::origin_line_localized(&card.origin, language)]
        ),
        None => language.format("{0} asked to allow an operation on this agent.", &[&who]),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use deck_model::{
        AdapterKind, AgentId, CardId, CardKind, CardStatus, Choice, Deliver, Origin, OriginKind,
        Page, Principal, Ts,
    };

    fn card(labels: &[&str]) -> Card {
        Card {
            id: CardId(17),
            kind: CardKind::Q,
            status: CardStatus::Pending,
            deliver: Deliver::Hold,
            origin: Origin {
                kind: OriginKind::Permission,
                tool: Some("Bash".into()),
                request_id: None,
                group: None,
            },
            text: Some("Bash: psql".into()),
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
            created: Ts::from_millis(0),
            expires: None,
            redacted: false,
        }
    }

    fn request() -> ConfirmRequest {
        ConfirmRequest {
            agent: Some(AgentId("ag_1".into())),
            card: Some(CardId(17)),
            order: None,
            principal: Some(Principal::Adapter(AdapterKind::Mcp)),
        }
    }

    #[test]
    fn localized_permission_summary_keeps_the_requested_terminal_text_verbatim() {
        let mut request = request();
        let order = "echo '{0}' — Ready";
        request.order = Some(order.into());
        for language in deck_gui_core::i18n::Language::ALL {
            let line = summary_localized(&request, None, language);
            assert!(line.contains(order));
            if language != deck_gui_core::i18n::Language::English {
                assert!(!line.contains("asked to type"));
            }
        }
    }

    #[test]
    fn allow_re_issues_the_cards_own_allow_and_deny_re_issues_its_deny() {
        let card = card(&["Allow", "Allow always", "Deny"]);
        let request = request();
        assert_eq!(
            action(&request, Some(&card), Decision::Allow),
            Some(UiAction::Answer {
                agent: AgentId("ag_1".into()),
                card: CardId(17),
                choice: Some(0),
                text: None,
            })
        );
        assert_eq!(
            action(&request, Some(&card), Decision::Deny),
            Some(UiAction::Answer {
                agent: AgentId("ag_1".into()),
                card: CardId(17),
                choice: Some(2),
                text: None,
            })
        );
        assert!(allow_offered(&request, Some(&card), true));
        assert!(
            !allow_offered(&request, Some(&card), false),
            "an unverified build cannot allow, so it must not offer to"
        );
    }

    #[test]
    fn a_dismissal_can_never_allow_however_the_card_is_shaped() {
        let request = request();
        for labels in [
            vec!["Allow", "Deny"],
            vec!["Allow always", "No, stop"],
            vec!["Allow"],
            vec![],
        ] {
            let card = card(&labels);
            let denied = action(&request, Some(&card), Decision::Deny);
            let allowed = action(&request, Some(&card), Decision::Allow);
            // A denial is an answer or it is nothing at all; never an order,
            // and never a call this module would have made for an allow.
            match &denied {
                Some(UiAction::Answer { choice, .. }) => {
                    let chosen = choice.expect("a re-issued answer names its choice");
                    assert!(
                        !deck_model::guard::label_is_allow(
                            &card.choices[usize::from(chosen)].label
                        ),
                        "{labels:?} denied with an allow label"
                    );
                }
                // Nothing to send is still a refusal: the guarded call was
                // already `-32002`.
                None => {}
                other => panic!("{labels:?} denied with {other:?}"),
            }
            if allowed.is_some() {
                assert_ne!(denied, allowed, "{labels:?}");
            }
        }
        // Nothing but allows on offer: a dismissal sends nothing at all,
        // which still refuses (the guarded call was already `-32002`).
        let all_allow = card(&["Allow", "Allow always"]);
        assert_eq!(action(&request, Some(&all_allow), Decision::Deny), None);
    }

    #[test]
    fn a_guarded_order_is_allowed_by_re_issuing_it_and_denied_by_dropping_it() {
        let mut request = request();
        request.card = None;
        request.order = Some("psql -c 'drop table'".into());
        assert_eq!(
            action(&request, None, Decision::Allow),
            Some(UiAction::Order {
                agent: AgentId("ag_1".into()),
                text: "psql -c 'drop table'".into(),
            })
        );
        assert_eq!(
            action(&request, None, Decision::Deny),
            None,
            "the daemon already refused it; there is nothing to type"
        );
        assert_eq!(
            action(&request, None, Decision::Allow)
                .map(|a| a.request().0)
                .unwrap(),
            "op.order"
        );
    }

    #[test]
    fn a_request_the_mirror_has_moved_past_does_nothing_either_way() {
        let request = request();
        // No card in the mirror (answered, cleared, the agent went away).
        assert_eq!(action(&request, None, Decision::Allow), None);
        assert_eq!(action(&request, None, Decision::Deny), None);
        assert!(!allow_offered(&request, None, true));
        // A different card under the same agent is not this request's.
        let mut other = card(&["Allow", "Deny"]);
        other.id = CardId(18);
        assert_eq!(action(&request, Some(&other), Decision::Allow), None);
        // No agent at all: nothing addressable.
        let mut headless = request.clone();
        headless.agent = None;
        assert_eq!(
            action(&headless, Some(&card(&["Allow"])), Decision::Allow),
            None
        );
    }

    #[test]
    fn the_summary_names_who_asked_and_what_for() {
        let card = card(&["Allow", "Deny"]);
        let mut request = request();
        assert_eq!(
            summary(&request, Some(&card)),
            "adapter:mcp asked to allow #17 \u{b7} permission \u{b7} Bash."
        );
        request.principal = None;
        assert!(summary(&request, None).starts_with("an untrusted client asked to allow"));
        request.order = Some("rm -rf /".into());
        assert_eq!(
            summary(&request, None),
            "an untrusted client asked to type \u{201c}rm -rf /\u{201d} into the terminal."
        );
    }
}
