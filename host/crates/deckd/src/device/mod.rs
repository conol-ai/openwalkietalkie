//! The device link on the hid thread (design §6): desired state, outbox,
//! session state machine, framing, and the deck's status on channel 2.
pub mod framer;
pub mod outbox;
pub mod session;
pub mod status;
pub mod view;
pub mod writer;
