//! The local recogniser's **model weights**: the catalogue, where they live,
//! and the download job — fetched only when the operator opts in (`deckd
//! stt install`, or the app's first-start prompt), never at install time.
//! A fresh install speaks to Apple's recogniser and has nothing of this on
//! disk.
//!
//! The engine itself is `audio::whisper` (macOS only, whisper.cpp through
//! `whisper-rs`); this module has no platform in it, so the CLI's `stt`
//! commands and the catalogue compile everywhere. The download runs in
//! [`crate::download`]; a file is final only once its size is the
//! catalogue's.
use std::path::{Path, PathBuf};

use crate::download::{part_path, Finish, Job};

/// The engine's name in `[voice.stt] engine` and `voice.engines`.
pub const ENGINE: &str = "whisper";
/// The model `[voice.stt] model` means when empty: the best trade of
/// accuracy and speed on Apple silicon, multilingual.
pub const DEFAULT_MODEL: &str = "large-v3-turbo";
/// Where the ggml files come from.
const HUB: &str = "https://huggingface.co/ggerganov/whisper.cpp/resolve/main";

/// One model the daemon knows how to fetch and name.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Model {
    pub name: &'static str,
    /// The file's exact size, which is also how "downloaded" is decided.
    pub bytes: u64,
    pub about: &'static str,
}

/// The catalogue, best first. Sizes are the files' own on the hub
/// (read 2026-10-11).
pub const MODELS: [Model; 5] = [
    Model {
        name: "large-v3-turbo",
        bytes: 1_624_555_275,
        about: "multilingual; the accuracy of large-v3 at a fraction of its time",
    },
    Model {
        name: "large-v3-turbo-q8_0",
        bytes: 874_188_075,
        about: "the same in 8-bit: half the download, almost the same accuracy",
    },
    Model {
        name: "large-v3-turbo-q5_0",
        bytes: 574_041_195,
        about: "the same in 5-bit: a third of the download",
    },
    Model {
        name: "small",
        bytes: 487_601_967,
        about: "multilingual, fast, noticeably less accurate",
    },
    Model {
        name: "base.en",
        bytes: 147_964_211,
        about: "English only, the lightest",
    },
];

#[must_use]
pub fn model(name: &str) -> Option<&'static Model> {
    MODELS.iter().find(|m| m.name == name)
}

/// `[voice.stt] model`, with the empty value meaning the default.
#[must_use]
pub fn model_name(configured: &str) -> &str {
    let name = configured.trim();
    if name.is_empty() {
        DEFAULT_MODEL
    } else {
        name
    }
}

#[must_use]
pub fn file_name(name: &str) -> String {
    format!("ggml-{name}.bin")
}

#[must_use]
pub fn url(name: &str) -> String {
    format!("{HUB}/{}", file_name(name))
}

#[must_use]
pub fn model_path(dir: &Path, name: &str) -> PathBuf {
    dir.join(file_name(name))
}

/// The job that fetches `name` into `dir`.
#[must_use]
pub fn job(dir: &Path, name: &str) -> Job {
    let name = model_name(name).to_string();
    let path = model_path(dir, &name);
    let total = model(&name).map_or(0, |m| m.bytes);
    Job {
        url: url(&name),
        part: part_path(&path),
        total,
        finish: Finish::Rename { path, total },
        name,
    }
}

/// Whether the model is on disk and whole: the file exists, and for a
/// catalogued model has exactly the catalogue's size — a part file never
/// counts, and neither does a download that stopped short.
#[must_use]
pub fn present(dir: &Path, name: &str) -> bool {
    job(dir, name).present()
}

/// `[voice.stt]` as the recogniser reads it at every press.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Settings {
    /// `[voice.stt] engine == "whisper"`.
    pub enabled: bool,
    pub model: String,
    /// `"auto"` lets the model detect the language per utterance; else an
    /// ISO 639-1 code (`en`, `zh`), or empty to follow `[voice.stt]
    /// locale`.
    pub language: String,
    /// Decoder threads; `0` is the machine's own count, capped.
    pub threads: u16,
}

impl Default for Settings {
    fn default() -> Self {
        Settings::from_stt(&crate::config::Stt::default())
    }
}

impl Settings {
    #[must_use]
    pub fn from_config(config: &crate::config::Config) -> Settings {
        Settings::from_stt(&config.voice.stt)
    }

    #[must_use]
    pub fn from_stt(stt: &crate::config::Stt) -> Settings {
        Settings {
            // Windows has no recogniser of its own to choose instead: Whisper
            // is the one, whatever the word (the default is macOS's "apple").
            enabled: stt.engine == ENGINE || cfg!(windows),
            model: model_name(&stt.model).to_string(),
            language: stt.language.trim().to_ascii_lowercase(),
            threads: stt.threads,
        }
    }

    /// The language code sent to the model for a press at `locale`:
    /// `None` for auto-detection.
    #[must_use]
    pub fn language_for(&self, locale: &str) -> Option<String> {
        let code = if self.language.is_empty() {
            locale.split(['-', '_']).next().unwrap_or("").to_ascii_lowercase()
        } else {
            self.language.clone()
        };
        if code.is_empty() || code == "auto" {
            None
        } else {
            Some(code)
        }
    }

    /// Decoder threads to ask for.
    #[must_use]
    pub fn thread_count(&self) -> i32 {
        if self.threads > 0 {
            return i32::from(self.threads);
        }
        let cores = std::thread::available_parallelism().map_or(4, std::num::NonZero::get);
        i32::try_from(cores.min(8)).unwrap_or(4)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_catalogue_names_files_and_urls_and_the_default_is_the_turbo_model() {
        assert_eq!(model_name(""), "large-v3-turbo");
        assert_eq!(model_name("  "), "large-v3-turbo");
        assert_eq!(model_name("base.en"), "base.en");
        assert_eq!(file_name("large-v3-turbo"), "ggml-large-v3-turbo.bin");
        assert_eq!(
            url("large-v3-turbo-q5_0"),
            "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-large-v3-turbo-q5_0.bin"
        );
        assert!(model("large-v3-turbo").is_some_and(|m| m.bytes > 1_500_000_000));
        assert!(model("nope").is_none());
        let names: Vec<&str> = MODELS.iter().map(|m| m.name).collect();
        assert_eq!(names[0], DEFAULT_MODEL);
        let mut unique = names.clone();
        unique.sort_unstable();
        unique.dedup();
        assert_eq!(unique.len(), names.len());
        let job = job(Path::new("/tmp/m"), "");
        assert_eq!(job.name, DEFAULT_MODEL);
        assert_eq!(job.part, PathBuf::from("/tmp/m/ggml-large-v3-turbo.bin.part"));
        assert_eq!(job.path(), Path::new("/tmp/m/ggml-large-v3-turbo.bin"));
    }

    #[test]
    fn present_means_whole_and_a_part_file_never_counts() {
        let dir = std::env::temp_dir().join(format!("deckd-stt-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        assert!(!present(&dir, "base.en"));
        std::fs::write(part_path(&model_path(&dir, "base.en")), b"half").unwrap();
        assert!(!present(&dir, "base.en"), "a part file is not the model");
        std::fs::write(model_path(&dir, "base.en"), b"short").unwrap();
        assert!(!present(&dir, "base.en"), "a short file is not the model");
        // An uncatalogued model only has to exist.
        std::fs::write(model_path(&dir, "custom"), b"weights").unwrap();
        assert!(present(&dir, "custom"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_language_follows_the_setting_then_the_locale_and_auto_is_none() {
        let mut s = Settings::from_stt(&crate::config::Stt {
            engine: "whisper".into(),
            ..Default::default()
        });
        assert!(s.enabled);
        assert_eq!(s.model, DEFAULT_MODEL);
        assert_eq!(s.language_for("en-US"), None, "auto by default");
        s.language = String::new();
        assert_eq!(s.language_for("zh-CN").as_deref(), Some("zh"));
        assert_eq!(s.language_for("en_GB").as_deref(), Some("en"));
        s.language = "ja".into();
        assert_eq!(s.language_for("en-US").as_deref(), Some("ja"));
        assert!(s.thread_count() >= 1 && s.thread_count() <= 8);
        s.threads = 3;
        assert_eq!(s.thread_count(), 3);
        let apple = Settings::from_stt(&crate::config::Stt::default());
        assert_eq!(apple.enabled, cfg!(windows), "Apple's recogniser, where there is one");
    }
}
