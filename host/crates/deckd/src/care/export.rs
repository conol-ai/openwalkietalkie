//! `care.support_file`: `deckd diagnostics` as a method, for the Service
//! page's Export Support File. The same one file, redacted the same way
//! (`crate::diagnostics`), written 0600 to the Desktop or to a directory
//! the caller names, and the list of what it left out in the reply.
use super::Context;
use crate::diagnostics::Options;
use crate::integrations::Env;
use deck_model::{ApiError, Ts};
use std::path::{Path, PathBuf};

/// Where the file goes (`{to}`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Target {
    /// `"desktop"`: [`desktop`], under this process's `HOME`.
    Desktop,
    /// An absolute directory, made when it is not there.
    Dir(PathBuf),
}

impl Target {
    /// `"desktop"` or an absolute directory. Anything else is refused
    /// rather than resolved against the service's working directory.
    pub fn parse(to: &str) -> Result<Target, ApiError> {
        if to == "desktop" {
            return Ok(Target::Desktop);
        }
        let dir = PathBuf::from(to);
        if dir.is_absolute() {
            Ok(Target::Dir(dir))
        } else {
            Err(ApiError::invalid_params(
                "to: \"desktop\", or an absolute directory",
            ))
        }
    }
}

/// The Desktop under `home`, where `deckd diagnostics` writes by default.
pub fn desktop(home: &Path) -> PathBuf {
    home.join("Desktop")
}

/// Render the bundle and write it into `dir`: `{"path", "redacted"}`. The
/// installer's view is taken the way `deckd diagnostics` takes it, best
/// effort; a unit-test build runs no CLI for it and no external probe.
pub fn support_file(context: &Context, dir: &Path, now: Ts) -> Result<serde_json::Value, ApiError> {
    let env = Env::detect(&context.paths, &context.config)
        .ok()
        .map(|mut env| {
            if cfg!(test) {
                env.probe_clis = false;
                env.run_clis = false;
            }
            env
        });
    let options = Options {
        probe: !cfg!(test),
        ..Options::default()
    };
    write(context, env.as_ref(), &options, dir, now)
}

/// [`support_file`] with the installer's environment and the options
/// given, so a test can hand it a throwaway one.
pub fn write(
    context: &Context,
    env: Option<&Env>,
    options: &Options,
    dir: &Path,
    now: Ts,
) -> Result<serde_json::Value, ApiError> {
    let bundle = crate::diagnostics::render(&context.paths, &context.config, env, options, now);
    let path = crate::diagnostics::write(dir, &bundle, now)
        .map_err(|why| ApiError::invalid("to", &why))?;
    Ok(serde_json::json!({
        "path": path.to_string_lossy(),
        "redacted": bundle.redacted,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::care::testkit::context;
    use crate::integrations::testdir::TestDir;

    /// The file lands in the directory asked for, 0600, under the name
    /// `deckd diagnostics` gives it, with the redaction list at its top
    /// and in the reply, and a secret-shaped config value is not in it.
    /// Nothing outside the test dir is written: the installer environment
    /// is a throwaway one and no probe runs.
    #[test]
    fn the_support_file_is_written_where_asked_and_says_what_it_left_out() {
        let dir = TestDir::new("care-export");
        let context = context(&dir);
        dir.write(
            "data/config.toml",
            "[voice.tts]\napi_key = \"sk-not-for-the-file\"\n",
        );
        let env = crate::integrations::test_env(&dir);
        let out = dir.path().join("out");
        let options = Options {
            probe: false,
            log_tail: 10,
        };
        let now = crate::core::wall_now();
        let reply = write(&context, Some(&env), &options, &out, now).unwrap();
        let path = PathBuf::from(reply["path"].as_str().unwrap());
        assert_eq!(path.parent(), Some(out.as_path()));
        assert_eq!(
            path.file_name().unwrap().to_string_lossy(),
            crate::diagnostics::file_name(now)
        );
        let text = std::fs::read_to_string(&path).unwrap();
        let redacted: Vec<String> = serde_json::from_value(reply["redacted"].clone()).unwrap();
        assert!(!redacted.is_empty());
        for line in &redacted {
            assert!(text.contains(line.as_str()), "{line} is not at the top");
        }
        assert!(!text.contains("sk-not-for-the-file"), "the key was copied");
        // Owner-only by file mode where there are file modes; on Windows the
        // folder's own ACL (the user's profile) is what keeps it.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        // A directory that cannot be made is the caller's to fix.
        let blocked = dir.write("blocked", "a file, not a directory");
        let error = write(&context, Some(&env), &options, &blocked, now).unwrap_err();
        assert_eq!(error.code, ApiError::INVALID);
        assert_eq!(error.data["field"], "to");
    }

    /// `to` is `"desktop"` or an absolute directory, and the Desktop is
    /// the home's, never this machine's own in a test.
    #[test]
    fn the_target_is_the_desktop_or_an_absolute_directory() {
        assert_eq!(Target::parse("desktop"), Ok(Target::Desktop));
        // An absolute directory as this platform writes one.
        let documents = if cfg!(windows) { r"C:\Users\you\Documents" } else { "/Users/you/Documents" };
        assert_eq!(Target::parse(documents), Ok(Target::Dir(documents.into())));
        for to in ["", "Desktop", "~/Desktop", "out"] {
            assert_eq!(
                Target::parse(to).unwrap_err().code,
                ApiError::INVALID_PARAMS,
                "{to:?}"
            );
        }
        assert_eq!(
            desktop(Path::new("/tmp/home")),
            PathBuf::from("/tmp/home/Desktop")
        );
    }
}
