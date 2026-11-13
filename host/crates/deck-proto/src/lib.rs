//! The host side of the deck's HID channel 5 (`docs/deck-protocol.md`): the
//! typed vocabulary in both directions, the line encoder with its byte caps,
//! the report chunker and the line assembler / device-line parser. Pure: no
//! I/O, no clocks.
//!
//! The firmware implements the other end in `src/deck/{msg,proto,json}.rs`;
//! `tools/deck-sim.py` is the reference host whose framing this crate matches
//! byte for byte (`tools/host-test.sh` diffs the two).

mod assemble;
mod chunk;
mod encode;
mod types;

// The firmware's deck module (assembler, parser, event encoder), mounted for
// the contract tests only; `proto.rs` reaches its UI through `crate::ui`.
#[cfg(test)]
mod fw;
#[cfg(test)]
use fw::ui;
#[cfg(test)]
mod contract;

pub use assemble::{parse_device_line, parse_host_line, Assembler, DecodeError, LineAssembler};
pub use chunk::Chunker;
pub use encode::{caps, encode, fit_doc_window, Caps, EncodeError};
pub use types::{
    keystroke_valid, strip_problem, KEYSTROKE_KEYS, MACROPADS_MAX, STRIP_MAX, TIMERS_MAX,
    AgentState, Binding, CardKind, DeviceLine, DocFont, DocKind, ErrWhy, FaceEntry, HostLine,
    KeyId, Kind, MacroKey, SessionMenu, PttCtx, SetKey, SystemStatus, Template, Tone, UsageItem, Window,
};

/// The deck's channel byte.
pub const CHANNEL: u8 = 5;
/// The vendor collection's report id, byte 0 of every hidapi buffer.
pub const REPORT_ID: u8 = 6;
/// Payload bytes per report: `[6, 5, len, payload[0..len], zero padding]`.
pub const PAYLOAD_MAX: usize = 61;
/// Longest line including its LF that the device accepts (`hello.rx`).
pub const LINE_MAX: usize = 1024;
/// `hello.v` in both directions.
pub const VERSION: u8 = 1;
/// A hidapi report buffer.
pub const REPORT_LEN: usize = 64;
