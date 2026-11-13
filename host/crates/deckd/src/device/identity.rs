//! Which deck is linked, so each keeps its own strip, settings and keys.
//!
//! Nothing a deck says on the deck channel or the system channel (`hello`,
//! `sys.version`, `device.status`) tells one board from another. Its USB
//! serial number does: the firmware gives the cable link its random-static
//! Bluetooth address, derived from the chip's eFuse UID, as twelve
//! upper-case hex digits with the least significant byte first
//! (`A1B2C3D4E5F6`), and the recovery bootloader gives the same one. Over
//! Bluetooth HID the host may list that address the way Bluetooth stacks
//! print one, six pairs with the most significant byte first joined by `:`
//! or `-` (`f6:e5:d4:c3:b2:a1`), or nothing at all.
//!
//! [`candidates`] reads a serial as the ids it can name, the cable's own
//! spelling first. [`label`] is what the per-deck settings call a deck
//! outside `state.json` (`device.deck`, `device.profile`, `care.decks`, the
//! log): eight hex digits of a hash, stable for one deck and never the
//! address itself. The firmware updater (`device.serial`, which
//! `firmware.install` names the deck by) and the voice link's cable port
//! (`usb:<serial>`) still name the deck on the cable by its serial number
//! (`docs/host-service.md` §6.9).

/// The ids `serial` can name, the cable's spelling first.
///
/// Twelve hex digits, in any case, are the cable's own: that id, then its
/// bytes reversed, which only finds a record a Bluetooth spelling made when
/// it was read the other way round. Six pairs joined by `:` or `-` are a
/// Bluetooth stack's, most significant byte first: reversed (the cable's
/// spelling) first, then as written. Anything else names no deck: nothing,
/// `000000000000` (what the firmware falls back to), a name.
pub fn candidates(serial: &str) -> Vec<String> {
    let serial = serial.trim();
    let (bytes, from_cable) = match (plain(serial), pairs(serial)) {
        (Some(bytes), _) => (bytes, true),
        (None, Some(bytes)) => (bytes, false),
        (None, None) => return Vec::new(),
    };
    if bytes.iter().all(|byte| *byte == 0) {
        return Vec::new();
    }
    let mut reversed = bytes;
    reversed.reverse();
    let (first, second) = if from_cable {
        (spell(&bytes), spell(&reversed))
    } else {
        (spell(&reversed), spell(&bytes))
    };
    if first == second {
        vec![first]
    } else {
        vec![first, second]
    }
}

/// Whether `serial` is spelled the way the cable spells it (twelve hex
/// digits), so that it names the deck for good: a record found under its
/// reversal is moved to it.
pub fn from_cable(serial: &str) -> bool {
    plain(serial.trim()).is_some()
}

/// What a deck is called outside `state.json`: the first eight hex digits
/// of a hash of its id. The same for one deck every time, and the address
/// cannot be read back from it.
pub fn label(id: &str) -> String {
    let hash = blake3::hash(format!("openwalkietalkie-deck:{id}").as_bytes());
    hash.to_hex()[..8].to_string()
}

/// The label of the deck `serial` names, `None` when it names none.
pub fn label_of(serial: &str) -> Option<String> {
    candidates(serial).first().map(|id| label(id))
}

/// Twelve hex digits as six bytes, in the order written.
fn plain(text: &str) -> Option<[u8; 6]> {
    if text.len() != 12 || !text.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let mut bytes = [0u8; 6];
    for (i, byte) in bytes.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&text[2 * i..2 * i + 2], 16).ok()?;
    }
    Some(bytes)
}

/// Six two-digit hex pairs joined by `:` or `-`, in the order written.
fn pairs(text: &str) -> Option<[u8; 6]> {
    let parts: Vec<&str> = text.split([':', '-']).collect();
    if parts.len() != 6 {
        return None;
    }
    let mut bytes = [0u8; 6];
    for (byte, part) in bytes.iter_mut().zip(parts) {
        if part.len() != 2 || !part.bytes().all(|b| b.is_ascii_hexdigit()) {
            return None;
        }
        *byte = u8::from_str_radix(part, 16).ok()?;
    }
    Some(bytes)
}

/// Six bytes as the cable spells them: twelve upper-case hex digits.
fn spell(bytes: &[u8; 6]) -> String {
    bytes.iter().map(|byte| format!("{byte:02X}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn usb_serials_are_ids_and_bluetooth_spellings_read_both_ways() {
        let both = ["A1B2C3D4E5F6".to_string(), "F6E5D4C3B2A1".to_string()];
        // The cable's own spelling, in any case and with stray spaces.
        assert_eq!(candidates("A1B2C3D4E5F6"), both);
        assert_eq!(candidates("a1b2c3d4e5f6"), both);
        assert_eq!(candidates(" A1B2C3D4E5F6\n"), both);
        // A Bluetooth stack's, most significant byte first: the cable's
        // spelling is the reversal, and comes first.
        assert_eq!(candidates("f6:e5:d4:c3:b2:a1"), both);
        assert_eq!(candidates("F6-E5-D4-C3-B2-A1"), both);
        assert!(from_cable("a1b2c3d4e5f6"));
        assert!(!from_cable("f6:e5:d4:c3:b2:a1"));
        // An address that reads the same both ways is one id.
        assert_eq!(candidates("AABBCCCCBBAA"), vec!["AABBCCCCBBAA".to_string()]);
    }

    #[test]
    fn anything_else_names_no_deck() {
        for serial in [
            "",
            "   ",
            "000000000000",
            "00:00:00:00:00:00",
            "OpenWalkieTalkie",
            "F657408873F",
            "A1B2C3D4E5F60",
            "G657408873FC",
            "fc:73:88:40:57",
            "f6:e5:d4:c3:b2:a1:00",
            "fc7:3:88:40:57:f6",
            "fc:73:88:40:57:+6",
            "+F57408873FC",
            "F657408873F€",
            "F657408873é",
        ] {
            assert!(candidates(serial).is_empty(), "{serial:?}");
            assert_eq!(label_of(serial), None, "{serial:?}");
        }
    }

    #[test]
    fn a_label_is_eight_hex_stable_and_never_the_address() {
        let label = label("A1B2C3D4E5F6");
        assert_eq!(label.len(), 8);
        assert!(label
            .chars()
            .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()));
        assert_eq!(super::label("A1B2C3D4E5F6"), label, "stable");
        assert_ne!(super::label("F6E5D4C3B2A1"), label, "one deck, one label");
        assert!(!"A1B2C3D4E5F6".to_lowercase().contains(&label));
        // Every spelling of the one deck carries the one label.
        for serial in ["A1B2C3D4E5F6", "a1b2c3d4e5f6", "f6:e5:d4:c3:b2:a1"] {
            assert_eq!(
                label_of(serial).as_deref(),
                Some(label.as_str()),
                "{serial}"
            );
        }
    }
}
