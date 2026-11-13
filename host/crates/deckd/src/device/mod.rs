//! The device link on the hid thread (design §6): desired state, outbox,
//! session state machine, framing, the deck's status on channel 2, and
//! which deck it is.
pub mod framer;
pub mod identity;
pub mod outbox;
pub mod session;
pub mod status;
pub mod view;
pub mod writer;
