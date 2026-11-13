//! What the core tells the hid thread (design §6.5). A `DeviceOp` carries
//! typed `HostLine` values, never wire bytes: the core says *what* the device
//! should show, the hid thread decides *how and when*.
use crate::{DocKind, HostLine, SetKey, Slot};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(
    Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
pub enum Lane {
    /// Control: hello, focus, card clear, transcript, name, set, faces.
    P0,
    /// Attention: cards, agent state, agent gone.
    P1,
    /// Content: replies, titles (4/s/slot).
    P2,
    /// Faces: ws, usage, system, macro.
    P3,
}

/// A Macropad face id 1..=8.
#[derive(
    Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[schemars(transparent)]
pub struct FaceId(pub u8);

/// One entry per key in the `DeviceView`. The declaration order is the
/// barrier order (§6.5), so a `BTreeMap<Key, _>` dumps in sync order:
/// agents → cards → replies → focus → macros → usage → system → ws → faces → set;
/// the trailing control keys are never part of a barrier.
#[derive(
    Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
pub enum Key {
    /// kind/name/state/since — the full record on the wire when the shadow is empty.
    AgentState(Slot),
    /// title only, token bucket 4/s/slot.
    AgentTitle(Slot),
    Card(Slot, u16),
    Reply(Slot),
    /// A window of a document the host laid out: the card's question body, the
    /// agent's reply or the live transcript. Keep-latest per document, so a
    /// fast scroll only ever costs the newest window.
    ///
    /// **Declared after `Card` and `Reply` on purpose.** The enum's order is
    /// the barrier's order, and a window names the body it belongs to by id:
    /// arriving before the `card`/`reply` line that carries that id, the deck
    /// would drop it as a window of a body it is not showing.
    DocText(Slot, DocKind, u16),
    Focus,
    Macro(FaceId),
    Usage,
    System,
    Ws,
    Faces,
    Set(SetKey),
    Hello,
    CardClear(Slot, u16),
    Transcript,
    Name,
    AgentGone(Slot),
}

impl Key {
    pub const fn lane(self) -> Lane {
        match self {
            Key::Hello
            | Key::Focus
            | Key::CardClear(..)
            | Key::Transcript
            | Key::Name
            | Key::Set(_)
            | Key::Faces => Lane::P0,
            Key::Card(..) | Key::AgentState(_) | Key::AgentGone(_) => Lane::P1,
            // A window rides behind the line whose body it is a window of.
            Key::Reply(_) | Key::AgentTitle(_) | Key::DocText(..) => Lane::P2,
            Key::Ws | Key::Usage | Key::System | Key::Macro(_) => Lane::P3,
        }
    }

    /// Keys a barrier dumps from the view (everything but the controls).
    pub const fn in_barrier(self) -> bool {
        !matches!(
            self,
            Key::Hello | Key::CardClear(..) | Key::Transcript | Key::Name | Key::AgentGone(_)
        )
    }

    pub const fn slot(self) -> Option<Slot> {
        match self {
            Key::AgentState(s)
            | Key::AgentTitle(s)
            | Key::Card(s, _)
            | Key::DocText(s, ..)
            | Key::Reply(s)
            | Key::CardClear(s, _)
            | Key::AgentGone(s) => Some(s),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum SyncReason {
    /// Daemon start: seed the view.
    Start,
    /// The device answered with a foreign or zero sid.
    NewSid,
    /// The core→hid channel overflowed.
    Overflow,
    /// `deckd ctl` / debugging.
    Manual,
}

/// One-shot lines that are not desired state: sent, not remembered.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum Control {
    Image(HostLine),
    SessionMenu(deck_proto::SessionMenu),
    Hello,
    CardClear(Slot, u16),
    Focus {
        slot: Slot,
        ack: Option<u16>,
    },
    Transcript {
        slot: Slot,
        text: String,
        last: bool,
        press: Option<u16>,
        failed: bool,
        /// Show the Claude Channels setup hint during this device PTT session.
        #[serde(default)]
        channel_tip: bool,
        /// A companion transcribes this press: the deck shows a recording
        /// waveform instead of live words, which would be the local
        /// recogniser's guess and not what the companion will hear.
        #[serde(default)]
        rec: bool,
    },
    Name(String),
    Synced,
    /// Not a line: the core has put the identified deck's own strip,
    /// settings and keys in the view (`Command::DeckProfile`), so the link's
    /// first hello, which the hid thread holds until this arrives, may go.
    ProfileReady,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum DeviceOp {
    /// Desired state for `Key` (keep-latest).
    Put(Key, HostLine),
    Remove(Key),
    /// Write a barrier from the view (`boot` adds faces + the eight `set`s).
    Sync {
        boot: bool,
        reason: SyncReason,
    },
    Control(Control),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_sort_in_barrier_order() {
        let mut keys = vec![
            Key::Set(SetKey::Speaker),
            Key::Ws,
            Key::Card(Slot(1), 1),
            Key::Reply(Slot(2)),
            Key::AgentState(Slot(2)),
            Key::Faces,
            Key::Usage,
            Key::Macro(FaceId(3)),
            Key::Focus,
            Key::AgentState(Slot(1)),
            Key::AgentTitle(Slot(1)),
            Key::Card(Slot(1), 0),
        ];
        keys.sort();
        assert_eq!(
            keys,
            vec![
                Key::AgentState(Slot(1)),
                Key::AgentState(Slot(2)),
                Key::AgentTitle(Slot(1)),
                Key::Card(Slot(1), 0),
                Key::Card(Slot(1), 1),
                Key::Reply(Slot(2)),
                Key::Focus,
                Key::Macro(FaceId(3)),
                Key::Usage,
                Key::Ws,
                Key::Faces,
                Key::Set(SetKey::Speaker),
            ]
        );
        assert!(Key::Hello > Key::Set(SetKey::Demo));
        assert!(!Key::Hello.in_barrier());
        assert!(!Key::AgentGone(Slot(1)).in_barrier());
        assert!(Key::Card(Slot(1), 1).in_barrier());
    }

    #[test]
    fn lanes_follow_the_table() {
        assert_eq!(Key::Hello.lane(), Lane::P0);
        assert_eq!(Key::Faces.lane(), Lane::P0);
        assert_eq!(Key::Card(Slot(1), 1).lane(), Lane::P1);
        assert_eq!(Key::AgentGone(Slot(1)).lane(), Lane::P1);
        assert_eq!(Key::AgentTitle(Slot(1)).lane(), Lane::P2);
        assert_eq!(Key::Macro(FaceId(1)).lane(), Lane::P3);
        assert!(Lane::P0 < Lane::P3);
        assert_eq!(Key::CardClear(Slot(4), 2).slot(), Some(Slot(4)));
        assert_eq!(Key::Ws.slot(), None);
    }
}
