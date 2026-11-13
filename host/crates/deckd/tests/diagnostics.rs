//! The diagnostics bundle's one binding promise (design §10.5,
//! HOST-CONTRACT-W7 §4): it is **one file the operator can send**, so the
//! test is that what must not leave the machine is not in it.
//!
//! The method is deliberately blunt. Three needles go into the inputs — a
//! bearer token in the shape the daemon mints and in the place the installer
//! puts it, a prompt, a transcript line — and the assertion is that none of
//! the three appears anywhere in the rendered text. Not "in the log section",
//! not "unescaped": anywhere, by substring. A redaction that holds only in
//! the sections someone remembered is not a redaction, and this is the file
//! that gets mailed to a stranger.
//!
//! Everything runs against a throwaway data dir and a throwaway
//! `CLAUDE_CONFIG_DIR`/`CODEX_HOME` (`integrations::test_env`), and the
//! external probes are off: a unit test may not shell out to `launchctl` or
//! `codesign` in the operator's own domain.
use deck_model::Ts;
use deckd::config::{Config, Paths};
use deckd::diagnostics::{self, Options};
use deckd::integrations::testdir::TestDir;
use deckd::integrations::{self, Target, Verb};

/// A token in the shape `deckd token create` mints: `dk_` + 43 base64url
/// characters. The pattern is what the scrub keys on, so the needle has to
/// be the real thing and not a word.
const TOKEN: &str = "dk_NEEDLEtokenAAAABBBBCCCCDDDDEEEEFFFFGGGGHH";
/// A hand-edited bearer that is *not* in the mint format: the other half of
/// "no token plaintext", because an operator may have pasted their own.
const FOREIGN_BEARER: &str = "not-a-mint-token-but-still-a-secret";
const PROMPT: &str = "NEEDLEPROMPT deploy the billing service to production";
const TRANSCRIPT: &str = "NEEDLETRANSCRIPT yes go ahead and delete the staging database";

const NOW: Ts = Ts::from_millis(1_789_760_523_120);

/// A data directory with all of §10.5's inputs in it, and the three needles
/// spread across the places they really occur: the token in the CLI's own
/// settings file (where the installer puts it) and in a log line; the prompt
/// and the transcript in the logs, as a field value and as a bare message.
fn fixture(dir: &TestDir) -> (Paths, Config, integrations::Env) {
    let env = integrations::test_env(dir);
    let paths = Paths::from_data_dir(env.data.clone(), dir.sub("logs"), dir.sub("caches"));

    std::fs::write(
        &paths.config,
        format!(
            "# the operator's own comment\n[api]\nport = 47201\n\n[privacy]\nlog_content = \
             false\n\n[voice.stt]\napi_key = \"{TOKEN}\"\n"
        ),
    )
    .unwrap();
    std::fs::write(
        &paths.state,
        format!(
            r#"{{"slots":{{"claude:billing":{{"slot":1,"kind":"claude","cwd":"/Users/t/p","last_seen":"2026-09-22T00:00:00.000Z"}}}},"terminal_map":{{"ttys003":{{"note":"{PROMPT}"}}}}}}"#
        ),
    )
    .unwrap();
    std::fs::write(&paths.status, b"{\"link\":\"Offline\"}").unwrap();

    // The plugin is installed (its files carry no secret), and the CLI's own
    // settings file holds bearers where an operator's http hooks — or the
    // pre-plugin deckd install — put them: one in the mint format, one not.
    integrations::run(&env, Target::Claude, Verb::Install { trust: false }).unwrap();
    let settings = integrations::claude::settings_path(&env);
    let mut value = serde_json::json!({
        "model": "claude-haiku-4-5",
        "hooks": {"Stop": [{"hooks": [{
            "type": "http", "url": "http://127.0.0.1:47201/v1/hooks/claude", "timeout": 2,
            "headers": {"Authorization": "Bearer dk_placeholder"}
        }]}]},
    });
    replace_bearers(&mut value, TOKEN);
    value["mine"] =
        serde_json::json!({"headers": {"Authorization": format!("Bearer {FOREIGN_BEARER}")}});
    std::fs::write(&settings, serde_json::to_string_pretty(&value).unwrap()).unwrap();

    let logs = &paths.logs;
    std::fs::write(
        logs.join("deckd.log.2026-09-22"),
        format!(
            "2026-09-22T19:42:03.120123Z  INFO deckd::core: deckd running sid=2491940960 port=47201\n\
             2026-09-22T19:42:04.000000Z DEBUG deckd::adapters: hook payload prompt=\"{PROMPT}\" tokens=12\n\
             2026-09-22T19:42:05.000000Z  WARN deckd::api: token={TOKEN} rejected\n\
             2026-09-22T19:42:06.000000Z  INFO deckd::audio: transcript final text=\"{TRANSCRIPT}\"\n"
        ),
    )
    .unwrap();
    std::fs::write(
        logs.join("app.log"),
        format!("2026-09-22T19:42:07.000000Z  INFO app: {TRANSCRIPT}\n"),
    )
    .unwrap();
    // The launchd log is pre-`tracing` output: a panic with the card's own
    // text in the payload is exactly what §11 calls content.
    std::fs::write(
        logs.join("deckd.launchd.log"),
        format!(
            "thread 'main' panicked at core/mod.rs:1: {PROMPT}\nAuthorization: Bearer {TOKEN}\n"
        ),
    )
    .unwrap();

    let config = Config::load(&paths.config).unwrap();
    (paths, config, env)
}

/// Rewrite every `Bearer …` in a JSON tree to a known plaintext.
fn replace_bearers(value: &mut serde_json::Value, token: &str) {
    match value {
        serde_json::Value::String(s) if s.starts_with("Bearer ") => {
            *s = format!("Bearer {token}");
        }
        serde_json::Value::Object(map) => {
            for (_, v) in map.iter_mut() {
                replace_bearers(v, token);
            }
        }
        serde_json::Value::Array(list) => {
            for v in list {
                replace_bearers(v, token);
            }
        }
        _ => {}
    }
}

/// The lines around a needle that should not have been there, so a failure
/// names the section that leaked rather than printing the whole bundle.
fn context(text: &str, needle: &str) -> String {
    text.lines()
        .filter(|line| line.contains(needle))
        .map(|line| format!("    {line}"))
        .collect::<Vec<String>>()
        .join("\n")
}

fn render(paths: &Paths, config: &Config, env: &integrations::Env) -> diagnostics::Bundle {
    diagnostics::render(
        paths,
        config,
        Some(env),
        &Options {
            probe: false,
            log_tail: diagnostics::LOG_TAIL,
        },
        NOW,
    )
}

#[test]
fn no_token_no_prompt_and_no_transcript_survives_into_the_bundle() {
    let dir = TestDir::new("diagnostics-needles");
    let (paths, config, env) = fixture(&dir);
    assert!(
        !config.privacy.log_content,
        "the default the test relies on"
    );
    let bundle = render(&paths, &config, &env);

    for (what, needle) in [
        ("the minted token", TOKEN),
        ("a hand-pasted bearer", FOREIGN_BEARER),
        ("the prompt", PROMPT),
        ("the transcript line", TRANSCRIPT),
    ] {
        assert!(
            !bundle.text.contains(needle),
            "{what} ({needle:?}) reached the bundle:\n{}",
            context(&bundle.text, needle)
        );
    }
    // The *distinctive* halves too, in case something copied a fragment.
    for fragment in [
        "NEEDLEPROMPT",
        "NEEDLETRANSCRIPT",
        "NEEDLEtoken",
        "billing service to production",
        "delete the staging database",
    ] {
        assert!(
            !bundle.text.contains(fragment),
            "{fragment:?} reached the bundle:\n{}",
            context(&bundle.text, fragment)
        );
    }
}

/// A redaction that took the whole file with it would pass the test above
/// and be useless. The bundle still has to answer the questions it exists
/// for: what is configured, what state is on disk, which files the installer
/// wrote, what the log's shape was.
#[test]
fn the_bundle_still_carries_everything_a_diagnosis_needs() {
    let dir = TestDir::new("diagnostics-kept");
    let (paths, config, env) = fixture(&dir);
    let bundle = render(&paths, &config, &env);
    let text = &bundle.text;

    for kept in [
        // Sections.
        "===== config.toml =====",
        "===== state.json =====",
        "===== status.json =====",
        "===== integrations =====",
        "===== log deckd.log.2026-09-22 =====",
        "===== log app.log =====",
        "===== log deckd.launchd.log =====",
        // Config as the operator wrote it, comments and all.
        "# the operator's own comment",
        "port = 47201",
        // The numbers in the log lines: a sid and a port are what a
        // diagnosis is made of and neither is content.
        "sid=2491940960",
        "port=47201",
        // The log's structure survives even where its text does not.
        "INFO deckd::core:",
        "DEBUG deckd::adapters:",
        "prompt=<redacted>",
        // state.json's slots are state (§4.3), not content.
        "claude:billing",
        // The installer's verdict, which is the point of the section.
        "claude:",
    ] {
        assert!(text.contains(kept), "{kept:?} is missing from the bundle");
    }
    // And the header says what it left out, so "send me your diagnostics" is
    // informed consent rather than a leap of faith.
    assert!(text.contains("REDACTED — not in this file:"), "{text:.400}");
    assert!(text.contains("KEPT — what is in this file:"));
    assert!(
        bundle.redacted.iter().any(|r| r.contains("Bearer")),
        "{:?}",
        bundle.redacted
    );
    assert!(
        bundle
            .kept
            .iter()
            .any(|k| k.contains("working directories")),
        "the header warns that the operator's project paths are in here: {:?}",
        bundle.kept
    );
    assert!(
        bundle
            .kept
            .iter()
            .any(|k| k.contains("delivery record") && k.contains("never the words")),
        "the header says what the delivery record is and is not: {:?}",
        bundle.kept
    );
}

/// `[privacy] log_content = true` is the operator saying "write it down".
/// The bundle then carries the log text — and says so at the top, rather
/// than quietly behaving differently from the file it looks like.
#[test]
fn log_content_on_carries_the_log_text_and_says_so_but_never_the_token() {
    let dir = TestDir::new("diagnostics-content");
    let (paths, mut config, env) = fixture(&dir);
    config.privacy.log_content = true;
    let bundle = render(&paths, &config, &env);

    assert!(bundle.text.contains(PROMPT), "the operator asked for this");
    assert!(bundle.text.contains(TRANSCRIPT));
    assert!(
        bundle.text.contains("log_content = true"),
        "the header states it"
    );
    assert!(
        bundle
            .text
            .contains("including text the daemon would otherwise never write down"),
        "and says what that means"
    );
    // The secrets are *not* part of that bargain: a token is a credential,
    // not content, and `log_content` never releases one.
    assert!(!bundle.text.contains(TOKEN), "the token is still gone");
    assert!(!bundle.text.contains(FOREIGN_BEARER));
}

/// The CLIs' own files are never copied in. They are the one place a working
/// install is *guaranteed* to hold a bearer token, and `integrations.status`
/// answers every question about them that a diagnosis has.
#[test]
fn the_clis_own_config_files_are_never_copied_into_the_bundle() {
    let dir = TestDir::new("diagnostics-cli-files");
    let (paths, config, env) = fixture(&dir);
    let settings = integrations::claude::settings_path(&env);
    let bundle = render(&paths, &config, &env);
    // The path is named — that is the diagnosis — but not the bytes.
    assert!(
        bundle.text.contains(&settings.display().to_string()),
        "the bundle says where the file is"
    );
    let contents = std::fs::read_to_string(&settings).unwrap();
    let handler_line = contents
        .lines()
        .find(|l| l.contains("/v1/hooks/claude"))
        .expect("the installed handler");
    assert!(
        !bundle.text.contains(handler_line.trim()),
        "the file's own lines are in the bundle: {handler_line}"
    );
}

/// The whole file, every section, gets the secret scrub — including one
/// added later by someone who did not read the module docs. Written as a
/// property over the rendered text rather than over one section.
#[test]
fn every_mint_format_token_anywhere_in_the_text_is_replaced() {
    let dir = TestDir::new("diagnostics-scrub");
    let (paths, config, env) = fixture(&dir);
    let bundle = render(&paths, &config, &env);
    for line in bundle.text.lines() {
        // The only `dk_` allowed through is the one in the header that
        // *names* the prefix being scrubbed.
        if line.contains("dk_") {
            assert!(
                line.contains("is replaced by"),
                "a dk_ survived outside the header: {line}"
            );
        }
        assert!(
            !line.contains("Bearer ") || line.contains(diagnostics::REDACTED_SECRET),
            "a bare bearer survived: {line}"
        );
    }
}
