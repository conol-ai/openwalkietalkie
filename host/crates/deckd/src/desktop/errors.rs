//! Why an order did not reach its session, as the two routes that post one
//! report it (`crate::inbox`, `crate::claude_channel`).
//!
//! Both run off the core thread and answer through [`super::FromDesktop`],
//! so `order.failed{reason}` has one way into the reducer. The words are
//! the reasons §4.12 publishes; the Ghostty injector's own words (its
//! AppleScript numbers, guard 3) went with it.

/// Why a route did not deliver an order.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum DesktopError {
    /// Guard 2: a permission, question or elicitation dialog is open in the
    /// session, and a message posted into it would cancel the dialog.
    #[error("a dialog is open for this agent")]
    DialogOpen,
    /// The inbox or Claude Channels could not take it: the session's socket
    /// could not be connected to or written, or the channel had nobody to
    /// give it to. The word is the `order.failed{reason}`.
    #[error("not posted: {0}")]
    Inbox(&'static str),
}

impl DesktopError {
    /// The `order.failed{reason}` word (§4.12).
    #[must_use]
    pub fn reason(&self) -> &'static str {
        match self {
            DesktopError::DialogOpen => "dialog_open",
            DesktopError::Inbox(reason) => reason,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The `order.failed{reason}` vocabulary, which the device and the
    /// connected apps both branch on.
    #[test]
    fn every_route_refusal_carries_the_reason_the_wire_publishes() {
        assert_eq!(DesktopError::DialogOpen.reason(), "dialog_open");
        assert_eq!(
            DesktopError::Inbox("inbox_unreachable").reason(),
            "inbox_unreachable"
        );
        assert_eq!(
            DesktopError::Inbox("channel_delivery_uncertain").reason(),
            "channel_delivery_uncertain"
        );
    }
}
