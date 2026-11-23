//! Adapt bare GATT values to the deck channel's existing HID framing.
use crate::deck::proto;

/// Wrap an RX value in the report body consumed by the shared line assembler.
pub(super) fn report_body(value: &[u8], frame: &mut [u8; proto::REPORT_LEN]) -> usize {
    if value.is_empty() || value.len() > proto::PAYLOAD_MAX {
        return 0;
    }
    let end = 2 + value.len();
    frame[0] = proto::CHANNEL;
    frame[1] = value.len() as u8;
    frame[2..end].copy_from_slice(value);
    end
}

/// Send just the framed payload, split at the negotiated ATT notification limit.
pub(super) fn notifications(
    report: &[u8; proto::REPORT_LEN],
    mtu: u16,
) -> core::slice::Chunks<'_, u8> {
    let len = usize::from(report[2]).min(proto::PAYLOAD_MAX);
    report[3..3 + len].chunks(usize::from(mtu.saturating_sub(3)).max(1))
}
