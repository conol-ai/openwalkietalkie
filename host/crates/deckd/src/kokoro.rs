//! The local synthesiser's **model package**: the catalogue, where it lives,
//! its speakers, and the download job — fetched only when the operator
//! opts in (`deckd tts install`, the app's first-start prompt, the Voice
//! screen), never at install time. The engine is `audio::kokoro` (macOS,
//! Kokoro through sherpa-onnx); this module has no platform in it.
use std::path::{Path, PathBuf};

use crate::download::{part_path, Finish, Job};

/// The engine's name in `[voice.tts] engine` and `voice.engines`.
pub const ENGINE: &str = "kokoro";
/// The package `[voice.tts] model` means when empty for this engine.
pub const DEFAULT_MODEL: &str = "kokoro-multi-lang-v1_1";
/// Where sherpa-onnx publishes its TTS model packages.
const HUB: &str = "https://github.com/k2-fsa/sherpa-onnx/releases/download/tts-models";
/// The model's output rate.
pub const RATE: u32 = 24_000;

/// One package the daemon knows how to fetch.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Package {
    pub name: &'static str,
    /// The archive's size, which is also how "downloaded" is decided.
    pub bytes: u64,
    pub about: &'static str,
}

/// The catalogue (archive sizes read from the hub 2026-11-14: sherpa-onnx
/// published both archives again on 2026-10-26, and the sizes read on
/// 2026-10-11 refused every download after it). Both are Kokoro v1.1 with
/// sherpa-onnx's Chinese and English text front ends and 103 speakers.
pub const PACKAGES: [Package; 2] = [
    Package {
        name: "kokoro-multi-lang-v1_1",
        bytes: 364_855_331,
        about: "Chinese and English, 103 voices, full precision",
    },
    Package {
        name: "kokoro-int8-multi-lang-v1_1",
        bytes: 147_052_523,
        about: "the same in 8-bit: less than half the download",
    },
];

/// The speakers of the v1.1 package, by id: sherpa-onnx writes the three
/// English voices first, then the Chinese female voices and the Chinese
/// male voices in ascending order (`scripts/kokoro/v1.1-zh/
/// generate_voices_bin.py`).
pub const SPEAKERS: [&str; 103] = [
    "af_maple", "af_sol", "bf_vale", "zf_001", "zf_002", "zf_003", "zf_004", "zf_005", "zf_006",
    "zf_007", "zf_008", "zf_017", "zf_018", "zf_019", "zf_021", "zf_022", "zf_023", "zf_024",
    "zf_026", "zf_027", "zf_028", "zf_032", "zf_036", "zf_038", "zf_039", "zf_040", "zf_042",
    "zf_043", "zf_044", "zf_046", "zf_047", "zf_048", "zf_049", "zf_051", "zf_059", "zf_060",
    "zf_067", "zf_070", "zf_071", "zf_072", "zf_073", "zf_074", "zf_075", "zf_076", "zf_077",
    "zf_078", "zf_079", "zf_083", "zf_084", "zf_085", "zf_086", "zf_087", "zf_088", "zf_090",
    "zf_092", "zf_093", "zf_094", "zf_099", "zm_009", "zm_010", "zm_011", "zm_012", "zm_013",
    "zm_014", "zm_015", "zm_016", "zm_020", "zm_025", "zm_029", "zm_030", "zm_031", "zm_033",
    "zm_034", "zm_035", "zm_037", "zm_041", "zm_045", "zm_050", "zm_052", "zm_053", "zm_054",
    "zm_055", "zm_056", "zm_057", "zm_058", "zm_061", "zm_062", "zm_063", "zm_064", "zm_065",
    "zm_066", "zm_068", "zm_069", "zm_080", "zm_081", "zm_082", "zm_089", "zm_091", "zm_095",
    "zm_096", "zm_097", "zm_098", "zm_100",
];

/// The voices agents are assigned from when `[voice.tts] voices` names
/// none: two English, two Chinese, two of each sex.
pub const DEFAULT_VOICES: [&str; 4] = ["af_maple", "zm_010", "bf_vale", "zf_001"];

#[must_use]
pub fn package(name: &str) -> Option<&'static Package> {
    PACKAGES.iter().find(|p| p.name == name)
}

/// `[voice.tts] model` for this engine, with the empty value (or a value
/// meant for another engine) meaning the default package.
#[must_use]
pub fn model_name(configured: &str) -> &str {
    let name = configured.trim();
    if name.starts_with("kokoro") {
        name
    } else {
        DEFAULT_MODEL
    }
}

#[must_use]
pub fn url(name: &str) -> String {
    format!("{HUB}/{name}.tar.bz2")
}

/// Where the unpacked package lives: `<root>/<name>/`.
#[must_use]
pub fn dir(root: &Path, name: &str) -> PathBuf {
    root.join(name)
}

/// The network's file in package `name`: sherpa-onnx names the 8-bit one
/// `model.int8.onnx`, the full one `model.onnx`.
#[must_use]
pub fn model_file(name: &str) -> &'static str {
    if model_name(name).contains("-int8-") {
        "model.int8.onnx"
    } else {
        "model.onnx"
    }
}

/// The job that fetches and unpacks `name` under `root`.
#[must_use]
pub fn job(root: &Path, name: &str) -> Job {
    let name = model_name(name).to_string();
    let into = dir(root, &name);
    Job {
        url: url(&name),
        part: part_path(&root.join(format!("{name}.tar.bz2"))),
        total: package(&name).map_or(0, |p| p.bytes),
        finish: Finish::Extract {
            marker: into.join(model_file(&name)),
            into,
        },
        name,
    }
}

/// Whether the package is unpacked and whole enough to load.
#[must_use]
pub fn present(root: &Path, name: &str) -> bool {
    let d = dir(root, model_name(name));
    [model_file(name), "voices.bin", "tokens.txt"].iter().all(|f| d.join(f).is_file())
}

/// A speaker id from a configured voice: its name in [`SPEAKERS`], or a
/// number.
#[must_use]
pub fn speaker_id(voice: &str) -> Option<i32> {
    let voice = voice.trim();
    if let Some(i) = SPEAKERS.iter().position(|s| s.eq_ignore_ascii_case(voice)) {
        return i32::try_from(i).ok();
    }
    voice.parse::<i32>().ok().filter(|i| (0..103).contains(i))
}

/// `[voice.tts]` as the engine reads it at every utterance.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Settings {
    /// `[voice.tts] engine == "kokoro"`.
    pub enabled: bool,
    pub model: String,
    /// The pool, in speaker names; empty is [`DEFAULT_VOICES`].
    pub voices: Vec<String>,
    /// Decoder threads; `0` is a sensible default.
    pub threads: u16,
}

impl Default for Settings {
    fn default() -> Self {
        Settings::from_tts(&crate::config::Tts::default())
    }
}

impl Settings {
    #[must_use]
    pub fn from_config(config: &crate::config::Config) -> Settings {
        Settings::from_tts(&config.voice.tts)
    }

    #[must_use]
    pub fn from_tts(tts: &crate::config::Tts) -> Settings {
        Settings {
            enabled: tts.engine == ENGINE,
            model: model_name(&tts.model).to_string(),
            voices: tts
                .voices
                .iter()
                .map(|v| v.trim().to_string())
                .filter(|v| !v.is_empty())
                .collect(),
            threads: tts.threads,
        }
    }

    /// The pool in speaker names.
    #[must_use]
    pub fn pool(&self) -> Vec<String> {
        if self.voices.is_empty() {
            DEFAULT_VOICES.iter().map(|v| (*v).to_string()).collect()
        } else {
            self.voices.clone()
        }
    }

    #[must_use]
    pub fn thread_count(&self) -> i32 {
        if self.threads > 0 {
            return i32::from(self.threads);
        }
        let cores = std::thread::available_parallelism().map_or(4, std::num::NonZero::get);
        i32::try_from(cores.min(4)).unwrap_or(2)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_catalogue_jobs_and_speakers_are_consistent() {
        assert_eq!(model_name(""), DEFAULT_MODEL);
        assert_eq!(model_name("gpt-4o-mini-tts"), DEFAULT_MODEL, "another engine's model");
        assert_eq!(model_name("kokoro-int8-multi-lang-v1_1"), "kokoro-int8-multi-lang-v1_1");
        assert_eq!(
            url("kokoro-multi-lang-v1_1"),
            "https://github.com/k2-fsa/sherpa-onnx/releases/download/tts-models/kokoro-multi-lang-v1_1.tar.bz2"
        );
        let root = PathBuf::from("/tmp/x");
        let job = job(&root, "");
        assert_eq!(job.name, DEFAULT_MODEL);
        assert_eq!(job.total, 364_855_331);
        assert_eq!(job.part, PathBuf::from("/tmp/x/kokoro-multi-lang-v1_1.tar.bz2.part"));
        assert_eq!(job.path(), Path::new("/tmp/x/kokoro-multi-lang-v1_1"));
        // Each package's network goes by the name its archive gives it.
        assert_eq!(model_file("kokoro-multi-lang-v1_1"), "model.onnx");
        assert_eq!(model_file("kokoro-int8-multi-lang-v1_1"), "model.int8.onnx");
        assert!(matches!(
            &super::job(&root, "kokoro-int8-multi-lang-v1_1").finish,
            Finish::Extract { marker, .. } if marker.ends_with("model.int8.onnx")
        ));
        assert_eq!(SPEAKERS.len(), 103);
        let mut unique = SPEAKERS.to_vec();
        unique.sort_unstable();
        unique.dedup();
        assert_eq!(unique.len(), 103);
        assert_eq!(speaker_id("af_maple"), Some(0));
        assert_eq!(speaker_id("ZF_001"), Some(3));
        assert_eq!(speaker_id("zm_100"), Some(102));
        assert_eq!(speaker_id("42"), Some(42));
        assert_eq!(speaker_id("Samantha"), None);
        assert!(DEFAULT_VOICES.iter().all(|v| speaker_id(v).is_some()));
        let s = Settings::from_tts(&crate::config::Tts {
            engine: "kokoro".into(),
            ..Default::default()
        });
        assert!(s.enabled);
        assert_eq!(s.pool(), DEFAULT_VOICES.map(String::from).to_vec());
        assert!((1..=4).contains(&s.thread_count()));
    }
}
