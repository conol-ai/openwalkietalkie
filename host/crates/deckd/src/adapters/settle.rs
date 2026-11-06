//! The settle window: a Claude `PermissionRequest` reaches the deck only
//! once nothing else has decided it.
//!
//! Claude runs a `PermissionRequest` hook *beside* whatever else can decide
//! the call — the terminal dialog, auto mode's classifier, the host app's
//! own approver — and "whichever decides first applies". Putting the card
//! up the moment the hook fires therefore put every approval on the deck,
//! including the ones an auto-approver settled a second later (on
//! 2026-11-06, 50 permission cards in a day and not one answered on the
//! deck). So the door parks the hook first and shows nothing: the card goes
//! up only when Claude says a human is being asked (`Notification`
//! `permission_prompt`), or when `[hooks.claude] settle_s` passes with no
//! sign of a decision. A sign of a decision — the call ran, failed or was
//! refused, or the turn moved on — answers the hook `{}` and the deck never
//! hears of it.
//!
//! The window keys a waiter by the ask's session, tool and input digest:
//! the payload has no `tool_use_id` (C5), and Claude runs tools in
//! parallel, so a `Read` finishing beside a `Bash` must not settle the
//! `Bash`.
use super::HookPayload;
use std::sync::Mutex;
use tokio::sync::oneshot;

/// How a waiting ask ends before its window does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// Decided elsewhere: answer `{}`, show nothing.
    Settled,
    /// A human is being asked: show the card now.
    Ask,
}

struct Waiter {
    id: u64,
    session: String,
    tool: String,
    digest: String,
    tx: oneshot::Sender<Outcome>,
}

/// The asks waiting out their window. A handful at most (one per session
/// with a dialog up), so a list is the right shape.
#[derive(Default)]
pub struct Settle {
    inner: Mutex<(u64, Vec<Waiter>)>,
}

impl std::fmt::Debug for Settle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Settle").finish_non_exhaustive()
    }
}

/// What a Claude hook event says about the asks waiting in its session.
enum Signal<'a> {
    /// This tool call was decided: it ran, failed, or auto mode refused it.
    Tool(&'a str, &'a serde_json::Value),
    /// The whole session moved on, or a human is being asked.
    Session(Outcome),
}

fn signal(payload: &HookPayload) -> Option<Signal<'_>> {
    match payload.event() {
        "PostToolUse" | "PostToolUseFailure" | "PermissionDenied" => Some(Signal::Tool(
            payload.str("tool_name").unwrap_or_default(),
            payload.get("tool_input").unwrap_or(&serde_json::Value::Null),
        )),
        "Stop" | "StopFailure" | "UserPromptSubmit" | "SessionStart" | "SessionEnd" => {
            Some(Signal::Session(Outcome::Settled))
        }
        "Notification" => match payload.str("notification_type") {
            Some("permission_prompt" | "worker_permission_prompt") => {
                Some(Signal::Session(Outcome::Ask))
            }
            Some("idle_prompt") => Some(Signal::Session(Outcome::Settled)),
            _ => None,
        },
        _ => None,
    }
}

impl Settle {
    /// Park an ask. Its receiver hears an [`Outcome`] if one comes before
    /// the caller's window ends; the caller must [`Settle::cancel`] it
    /// either way.
    pub fn wait(
        &self,
        session: &str,
        tool: &str,
        input: &serde_json::Value,
    ) -> (u64, oneshot::Receiver<Outcome>) {
        let (tx, rx) = oneshot::channel();
        let Ok(mut inner) = self.inner.lock() else {
            // A poisoned lock must never make a hook wait longer: the
            // sender drops here and the caller shows the card at once.
            return (0, rx);
        };
        inner.0 += 1;
        let id = inner.0;
        inner.1.push(Waiter {
            id,
            session: session.to_string(),
            tool: tool.to_string(),
            digest: digest(input),
            tx,
        });
        (id, rx)
    }

    /// The window is over: forget the ask.
    pub fn cancel(&self, id: u64) {
        if let Ok(mut inner) = self.inner.lock() {
            inner.1.retain(|w| w.id != id);
        }
    }

    /// A Claude hook event arrived: settle or raise the asks it is about.
    /// Returns how many waiters it ended.
    pub fn observe(&self, payload: &HookPayload) -> usize {
        let (Some(session), Some(signal)) = (payload.str("session_id"), signal(payload)) else {
            return 0;
        };
        let Ok(mut inner) = self.inner.lock() else {
            return 0;
        };
        let (done, kept): (Vec<Waiter>, Vec<Waiter>) =
            std::mem::take(&mut inner.1).into_iter().partition(|w| {
                w.session == session
                    && match &signal {
                        Signal::Tool(tool, input) => w.tool == *tool && w.digest == digest(input),
                        Signal::Session(_) => true,
                    }
            });
        inner.1 = kept;
        let outcome = match signal {
            Signal::Tool(..) => Outcome::Settled,
            Signal::Session(outcome) => outcome,
        };
        let ended = done.len();
        for waiter in done {
            let _ = waiter.tx.send(outcome);
        }
        ended
    }
}

fn digest(input: &serde_json::Value) -> String {
    blake3::hash(input.to_string().as_bytes()).to_hex().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn payload(v: serde_json::Value) -> HookPayload {
        HookPayload(v)
    }

    #[tokio::test]
    async fn a_matching_tool_event_settles_only_its_own_ask() {
        let settle = Settle::default();
        let bash = serde_json::json!({"command": "cargo build"});
        let (a, mut rx_bash) = settle.wait("s1", "Bash", &bash);
        let (b, mut rx_write) = settle.wait("s1", "Write", &serde_json::json!({"file_path": "x"}));
        // A parallel Read finishing settles nothing.
        let read = payload(serde_json::json!({"session_id": "s1", "hook_event_name": "PostToolUse",
            "tool_name": "Read", "tool_input": {"file_path": "y"}}));
        assert_eq!(settle.observe(&read), 0);
        // The same tool with another input is another call.
        let other = payload(serde_json::json!({"session_id": "s1", "hook_event_name": "PostToolUse",
            "tool_name": "Bash", "tool_input": {"command": "ls"}}));
        assert_eq!(settle.observe(&other), 0);
        let ran = payload(serde_json::json!({"session_id": "s1", "hook_event_name": "PostToolUse",
            "tool_name": "Bash", "tool_input": bash}));
        assert_eq!(settle.observe(&ran), 1);
        assert_eq!(rx_bash.try_recv(), Ok(Outcome::Settled));
        assert!(rx_write.try_recv().is_err(), "the Write ask still waits");
        settle.cancel(a);
        settle.cancel(b);
        assert_eq!(settle.inner.lock().unwrap().1.len(), 0);
    }

    #[tokio::test]
    async fn the_permission_prompt_raises_and_the_turn_moving_on_settles_the_session() {
        let settle = Settle::default();
        let (_, mut asked) = settle.wait("s1", "Bash", &serde_json::json!({}));
        let (_, mut elsewhere) = settle.wait("s2", "Bash", &serde_json::json!({}));
        let prompt = payload(serde_json::json!({"session_id": "s1", "hook_event_name": "Notification",
            "notification_type": "permission_prompt", "message": "Claude needs your permission to use Bash"}));
        assert_eq!(settle.observe(&prompt), 1);
        assert_eq!(asked.try_recv(), Ok(Outcome::Ask));
        for event in ["UserPromptSubmit", "Stop"] {
            let (_, mut rx) = settle.wait("s3", "Bash", &serde_json::json!({}));
            let moved = payload(serde_json::json!({"session_id": "s3", "hook_event_name": event}));
            assert_eq!(settle.observe(&moved), 1, "{event}");
            assert_eq!(rx.try_recv(), Ok(Outcome::Settled), "{event}");
        }
        let idle = payload(serde_json::json!({"session_id": "s2", "hook_event_name": "Notification",
            "notification_type": "idle_prompt"}));
        assert_eq!(settle.observe(&idle), 1);
        assert_eq!(elsewhere.try_recv(), Ok(Outcome::Settled));
    }
}
