//! Stable interface-language ids shared by the firmware and desktop app.
//! This file has no platform, allocation, or serialization dependencies.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
#[repr(u8)]
pub enum Language {
    #[default]
    English = 0,
    SimplifiedChinese = 1,
    TraditionalChinese = 2,
    Japanese = 3,
}

impl Language {
    pub const ALL: [Self; 4] = [Self::English, Self::SimplifiedChinese, Self::TraditionalChinese, Self::Japanese];

    pub const fn from_id(id: u8) -> Self {
        match id {
            1 => Self::SimplifiedChinese,
            2 => Self::TraditionalChinese,
            3 => Self::Japanese,
            _ => Self::English,
        }
    }

    pub const fn code(self) -> &'static str {
        match self {
            Self::English => "en",
            Self::SimplifiedChinese => "zh-Hans",
            Self::TraditionalChinese => "zh-Hant",
            Self::Japanese => "ja",
        }
    }

    pub const fn name(self) -> &'static str {
        match self {
            Self::English => "English",
            Self::SimplifiedChinese => "简体中文",
            Self::TraditionalChinese => "繁體中文",
            Self::Japanese => "日本語",
        }
    }
}
