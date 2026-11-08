//! Versioned, checksummed on-flash records: the bond (never log its
//! contents) and the device preferences, which share one journal
//! (`bond_flash`) and are told apart by their magic.

pub const SIZE: usize = 64;
/// Kept from the project's old name: records already in flash must still
/// parse, so this is a format constant, not a name.
const MAGIC: &[u8; 4] = b"OMDB";
const PREFS_MAGIC: &[u8; 4] = b"OWTP";

/// What the deck remembers about itself across power cycles. A fresh or
/// factory-reset deck has none of it, so every field's default is the
/// "never happened" value.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Prefs {
    /// The first-run tutorial has been finished or skipped once.
    pub tutorial_done: bool,
    /// Device UI language: 0 English, 1 Simplified Chinese, 2 Traditional
    /// Chinese, 3 Japanese. Byte 6 was zero in existing v1 records.
    pub language: u8,
    pub language_revision: u32,
}

impl Prefs {
    pub fn encode(&self) -> [u8; SIZE] {
        let mut bytes = [0; SIZE];
        bytes[..4].copy_from_slice(PREFS_MAGIC);
        bytes[4] = 1;
        bytes[5] = u8::from(self.tutorial_done);
        bytes[6] = self.language.min(3);
        bytes[7..11].copy_from_slice(&self.language_revision.to_le_bytes());
        let checksum = crc32(&bytes[..60]);
        bytes[60..].copy_from_slice(&checksum.to_le_bytes());
        bytes
    }

    pub fn decode(bytes: &[u8; SIZE]) -> Option<Self> {
        if &bytes[..4] != PREFS_MAGIC
            || bytes[4] != 1
            || bytes[5] > 1
            || bytes[6] > 3
            || crc32(&bytes[..60]) != u32::from_le_bytes(bytes[60..].try_into().ok()?)
        {
            return None;
        }
        Some(Self {
            tutorial_done: bytes[5] != 0,
            language: bytes[6],
            // Existing non-English selections were explicit choices made
            // before sync support; they beat an untouched app default.
            language_revision: u32::from_le_bytes(bytes[7..11].try_into().ok()?).max(u32::from(bytes[6] != 0)),
        })
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct Record {
    pub address: [u8; 6],
    pub ltk: [u8; 16],
    pub irk: Option<[u8; 16]>,
    pub authenticated: bool,
    pub hid_notify: bool,
    pub battery_notify: bool,
}

impl Record {
    pub fn encode(&self) -> [u8; SIZE] {
        let mut bytes = [0; SIZE];
        bytes[..4].copy_from_slice(MAGIC);
        bytes[4] = 1;
        bytes[5] = u8::from(self.authenticated);
        bytes[6] = u8::from(self.irk.is_some());
        bytes[7] = u8::from(self.hid_notify) | (u8::from(self.battery_notify) << 1);
        bytes[12..18].copy_from_slice(&self.address);
        bytes[20..36].copy_from_slice(&self.ltk);
        if let Some(irk) = self.irk {
            bytes[36..52].copy_from_slice(&irk);
        }
        let checksum = crc32(&bytes[..60]);
        bytes[60..].copy_from_slice(&checksum.to_le_bytes());
        bytes
    }

    pub fn decode(bytes: &[u8; SIZE]) -> Option<Self> {
        if &bytes[..4] != MAGIC
            || bytes[4] != 1
            || bytes[5] > 1
            || bytes[6] > 1
            || bytes[7] & !3 != 0
            || crc32(&bytes[..60]) != u32::from_le_bytes(bytes[60..].try_into().ok()?)
        {
            return None;
        }
        Some(Self {
            address: bytes[12..18].try_into().ok()?,
            ltk: bytes[20..36].try_into().ok()?,
            irk: if bytes[6] != 0 {
                Some(bytes[36..52].try_into().ok()?)
            } else {
                None
            },
            authenticated: bytes[5] != 0,
            hid_notify: bytes[7] & 1 != 0,
            battery_notify: bytes[7] & 2 != 0,
        })
    }
}

fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = !0u32;
    for &b in bytes {
        crc ^= b as u32;
        for _ in 0..8 {
            crc = (crc >> 1) ^ (0xedb8_8320 & 0u32.wrapping_sub(crc & 1));
        }
    }
    !crc
}

#[cfg(test)]
mod tests {
    use super::*;
    fn record() -> Record {
        Record {
            address: [1, 2, 3, 4, 5, 6],
            ltk: [7; 16],
            irk: Some([8; 16]),
            authenticated: false,
            hid_notify: true,
            battery_notify: false,
        }
    }
    #[test]
    fn preserves_keys_and_notification_preferences() {
        let r = record();
        assert!(Record::decode(&r.encode()).as_ref() == Some(&r));
        let r = Record {
            irk: None,
            authenticated: true,
            battery_notify: true,
            ..r
        };
        assert!(Record::decode(&r.encode()).as_ref() == Some(&r));
    }
    #[test]
    fn rejects_erased_torn_and_corrupted_records() {
        assert!(Record::decode(&[255; SIZE]).is_none());
        let bytes = record().encode();
        for cut in 0..SIZE {
            let mut partial = [255; SIZE];
            partial[..cut].copy_from_slice(&bytes[..cut]);
            assert!(Record::decode(&partial).is_none());
        }
        for bit in 0..SIZE * 8 {
            let mut damaged = bytes;
            damaged[bit / 8] ^= 1 << (bit % 8);
            assert!(Record::decode(&damaged).is_none());
        }
    }
    #[test]
    fn prefs_round_trip_and_the_two_record_kinds_never_parse_as_each_other() {
        assert_eq!(Prefs::default(), Prefs { tutorial_done: false, language: 0, language_revision: 0 });
        for done in [false, true] {
            for language in 0..4 {
                let p = Prefs { tutorial_done: done, language, language_revision: u32::from(language != 0) };
                assert_eq!(Prefs::decode(&p.encode()), Some(p));
                assert!(Record::decode(&p.encode()).is_none());
            }
        }
        assert!(Prefs::decode(&record().encode()).is_none());
        assert!(Prefs::decode(&[255; SIZE]).is_none());
        let bytes = Prefs { tutorial_done: true, language: 0, language_revision: 0 }.encode();
        for bit in 0..SIZE * 8 {
            let mut damaged = bytes;
            damaged[bit / 8] ^= 1 << (bit % 8);
            assert!(Prefs::decode(&damaged).is_none());
        }
    }

    #[test]
    fn legacy_preferences_default_to_english_and_invalid_languages_are_rejected() {
        let mut legacy = [0u8; SIZE];
        legacy[..4].copy_from_slice(b"OWTP");
        legacy[4] = 1;
        legacy[5] = 1;
        let checksum = crc32(&legacy[..60]);
        legacy[60..].copy_from_slice(&checksum.to_le_bytes());
        assert_eq!(Prefs::decode(&legacy), Some(Prefs { tutorial_done: true, language: 0, language_revision: 0 }));
        legacy[6] = 2;
        let checksum = crc32(&legacy[..60]);
        legacy[60..].copy_from_slice(&checksum.to_le_bytes());
        assert_eq!(Prefs::decode(&legacy), Some(Prefs { tutorial_done: true, language: 2, language_revision: 1 }));
        legacy[6] = 4;
        let checksum = crc32(&legacy[..60]);
        legacy[60..].copy_from_slice(&checksum.to_le_bytes());
        assert!(Prefs::decode(&legacy).is_none());
    }

    #[test]
    fn preference_revisions_survive_english_reset_and_all_language_values() {
        for language in 0..4 {
            for language_revision in [1, 5, u32::MAX] {
                let prefs = Prefs { tutorial_done: true, language, language_revision };
                assert_eq!(Prefs::decode(&prefs.encode()), Some(prefs));
            }
        }
    }
}
