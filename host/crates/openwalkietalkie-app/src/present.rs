//! The words the pages share, as pure functions of the mirror, of the
//! service's replies and of a language.
//!
//! Nothing in here touches `gpui`, and nothing in here decides anything: a
//! page lays these answers out, and every button it draws is a
//! [`UiAction`](deck_gui_core::UiAction). A function that returns a string
//! cannot install a hook or stop a service.
//!
//! **The shared formatters come first**, and every page says the same things
//! with them: [`now`] (which the harness pins with [`set_fixed_now`]), [`ago`]
//! for how long ago something happened, [`day_and_time`] and [`local_clock`]
//! for when, [`size_text`] for a size on disk, [`fw_text`] for the deck's
//! firmware and [`home_short`] for a path. Their words are rows of
//! `crate::i18n::kit`, so "4 s ago" reads the same on every page.
//!
//! Then what two pages must say alike, so one place says it: the
//! installer's words ([`status_help`], [`plan_headline_localized`]); the one
//! rule for a run the service could not finish by itself, and the command
//! that finishes it ([`Leftover`], [`finished`], [`finish_command`],
//! amendment B4), which the Integrations page and first run both follow;
//! and the test question's line ([`test_line`]), which both show. Last come
//! the sentences one page reads out of a reply (the service's degraded
//! state, the voice test, a permission prompt) and the login item's status.
//!
//! They are here and not in `deck-gui-core` because `deck-gui-core` holds
//! the *shapes* the service answers with ([`deck_gui_core::admin`],
//! [`deck_gui_core::care`]) and this module holds the sentences the window
//! reads them out in, which is the half a designer edits.
use deck_client::ClientError;
use deck_gui_core::admin::{Status, Verb};
use deck_gui_core::care::{QuestionTest, TestState};
use deck_gui_core::i18n::Language;
use deck_gui_core::voice::{Prompt, TestReport};
use deck_model::{Tone, Ts};
use serde_json::Value;
use std::path::Path;
use std::sync::{Mutex, PoisonError};

/// Custom Channels still require Claude's interactive preview opt-in.
pub const CLAUDE_CHANNEL_COMMAND: &str =
    "claude --dangerously-load-development-channels plugin:openwalkietalkie@openwalkietalkie";

// ---------------------------------------------------------------------
// The shared formatters: every page says these things the same way
// ---------------------------------------------------------------------

/// The moment [`now`] answers instead of this Mac's clock, while the harness
/// has pinned one ([`set_fixed_now`]).
static FIXED_NOW: Mutex<Option<Ts>> = Mutex::new(None);

/// The wall clock, for ages and times of day. The reducer may not read a
/// clock (`deck-model` is pure and `HostState` folds what it is given); a
/// page that says "4 s ago" has no other way to know what second it is.
///
/// While the harness has pinned a moment it is that moment, so a board's
/// "4 s ago" and "today, 09:41" come out the same in every capture.
pub fn now() -> Ts {
    if let Some(fixed) = *FIXED_NOW.lock().unwrap_or_else(PoisonError::into_inner) {
        return fixed;
    }
    let millis = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or_default();
    Ts::from_millis(u64::try_from(millis).unwrap_or(u64::MAX))
}

/// Pin [`now`] to `at`, or (`None`) hand it back to this Mac's clock. The
/// harness pins the boards' minute for each staged scene and lets go for a
/// live one (`crate::shots`); nothing else has a reason to.
pub fn set_fixed_now(at: Option<Ts>) {
    *FIXED_NOW.lock().unwrap_or_else(PoisonError::into_inner) = at;
}

/// How long before `now` the moment `ts` was: `just now`, `4 s ago`,
/// `1 min ago`, `3 h ago`, `2 d ago`.
///
/// Each count is whole and rounded down, so a row never says a thing
/// happened later than it did. A stamp a little after `now` — the service's
/// clock and this one differ by a moment — is `just now`, never a negative
/// age.
pub fn ago(ts: Ts, now: Ts, language: Language) -> String {
    let seconds = now.millis().saturating_sub(ts.millis()) / 1_000;
    match seconds {
        0 => language.text("just now").to_string(),
        1..=59 => language.format("{0} s ago", &[&seconds.to_string()]),
        60..=3_599 => language.format("{0} min ago", &[&(seconds / 60).to_string()]),
        3_600..=86_399 => language.format("{0} h ago", &[&(seconds / 3_600).to_string()]),
        _ => language.format("{0} d ago", &[&(seconds / 86_400).to_string()]),
    }
}

/// [`local_clock`] on Windows, through chrono's local zone (no `localtime_r`).
#[cfg(windows)]
pub fn local_clock(ts: Ts) -> (i32, u32, u32, u32, u32) {
    use chrono::{Datelike, Timelike};
    let Some(utc) = i64::try_from(ts.millis())
        .ok()
        .and_then(chrono::DateTime::from_timestamp_millis)
    else {
        return utc_clock(ts);
    };
    let local = utc.with_timezone(&chrono::Local);
    (
        local.year(),
        local.month(),
        local.day(),
        local.hour(),
        local.minute(),
    )
}

/// [`local_ts`] on Windows. As `mktime` does, an ambiguous minute (the hour
/// a summer-time change repeats) is its first occurrence and a skipped one
/// is moved past the change.
#[cfg(windows)]
pub fn local_ts(year: i32, month: u32, day: u32, hour: u32, minute: u32) -> Option<Ts> {
    use chrono::TimeZone;
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) || hour > 23 || minute > 59 {
        return None;
    }
    let naive = chrono::NaiveDate::from_ymd_opt(year, month, day)?.and_hms_opt(hour, minute, 0)?;
    let local = chrono::Local
        .from_local_datetime(&naive)
        .earliest()
        .or_else(|| {
            chrono::Local
                .from_local_datetime(&(naive + chrono::Duration::hours(1)))
                .earliest()
        })?;
    let millis = u64::try_from(local.timestamp_millis()).ok()?;
    Some(Ts::from_millis(millis))
}

/// `ts` on this Mac's clock, through `localtime_r`: the year, the month (1
/// to 12), the day, the hour (0 to 23) and the minute, in its time zone and
/// with its summer time. A stamp the zone cannot place is read in UTC.
#[cfg(unix)]
pub fn local_clock(ts: Ts) -> (i32, u32, u32, u32, u32) {
    let seconds = libc::time_t::try_from(ts.millis() / 1_000).unwrap_or(libc::time_t::MAX);
    // SAFETY: `tm` is plain C data, and all zeroes (a null `tm_zone` among
    // them) is a value of it; `localtime_r` only writes to it.
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    // SAFETY: both pointers are valid for the call, and `localtime_r` writes
    // through the second alone. It keeps no pointer to either.
    if unsafe { libc::localtime_r(&seconds, &mut tm) }.is_null() {
        return utc_clock(ts);
    }
    let field = |value: libc::c_int| u32::try_from(value).unwrap_or_default();
    (
        tm.tm_year.saturating_add(1_900),
        field(tm.tm_mon) + 1,
        field(tm.tm_mday),
        field(tm.tm_hour),
        field(tm.tm_min),
    )
}

/// The minute `hour:minute` of `year-month-day` on this Mac's clock, as a
/// stamp: [`local_clock`] the other way round, through `mktime`, which lets
/// the time zone say whether summer time is on.
///
/// `None` for a date no calendar has (a 13th month, 30 February) or a time
/// no clock shows (a 25th hour). A minute a summer-time change skips is
/// moved past the change, as `mktime` moves it.
#[cfg(unix)]
pub fn local_ts(year: i32, month: u32, day: u32, hour: u32, minute: u32) -> Option<Ts> {
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) || hour > 23 || minute > 59 {
        return None;
    }
    let int = |value: u32| libc::c_int::try_from(value).ok();
    // SAFETY: as in `local_clock`; every field `mktime` reads is set below.
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    tm.tm_year = year.checked_sub(1_900)?;
    tm.tm_mon = int(month)? - 1;
    tm.tm_mday = int(day)?;
    tm.tm_hour = int(hour)?;
    tm.tm_min = int(minute)?;
    // The zone says whether summer time is on, not the caller.
    tm.tm_isdst = -1;
    // SAFETY: `tm` is valid for reads and writes for the call, and `mktime`
    // keeps no pointer to it.
    let seconds = unsafe { libc::mktime(&mut tm) };
    // `mktime` carries a day past the month's end into the next month; a
    // date it moved was not a date.
    if tm.tm_mday != int(day)? || tm.tm_mon != int(month)? - 1 {
        return None;
    }
    // A stamp counts from 1970: anything before is none, and so is `-1`,
    // which is how `mktime` says it failed.
    let seconds = u64::try_from(seconds).ok()?;
    Some(Ts::from_millis(seconds.saturating_mul(1_000)))
}

/// When `ts` was, at `now`, the way a page captions a moment:
/// `today, 09:41`, `yesterday, 09:41`, or `2026-11-09, 09:41` for any other
/// day, on this Mac's clock.
///
/// Yesterday is the calendar day before `now`'s, not the 24 hours before
/// it, so it is still right on the night summer time begins or ends.
pub fn day_and_time(ts: Ts, now: Ts, language: Language) -> String {
    let (year, month, day, hour, minute) = local_clock(ts);
    let (now_year, now_month, now_day, _, _) = local_clock(now);
    let time = format!("{hour:02}:{minute:02}");
    match days_from_civil(now_year, now_month, now_day) - days_from_civil(year, month, day) {
        0 => language.format("today, {0}", &[&time]),
        1 => language.format("yesterday, {0}", &[&time]),
        _ => language.format(
            "{0}, {1}",
            &[&format!("{year:04}-{month:02}-{day:02}"), &time],
        ),
    }
}

/// A size on disk as Finder writes it, in thousands: `4 KB`, `428 MB`,
/// `2.1 GB`.
///
/// Kilobytes and megabytes are whole and gigabytes carry one decimal, each
/// rounded to the nearest, and the unit steps up where the rounded number
/// would reach a thousand (`1.0 GB`, never `1000 MB`). A file that is not
/// empty is never `0 KB`.
pub fn size_text(bytes: u64) -> String {
    if bytes < 999_500 {
        let kilobytes = (bytes + 500) / 1_000;
        format!("{} KB", kilobytes.max(u64::from(bytes > 0)))
    } else if bytes < 999_500_000 {
        format!("{} MB", (bytes + 500_000) / 1_000_000)
    } else {
        let tenths = bytes.saturating_add(50_000_000) / 100_000_000;
        format!("{}.{} GB", tenths / 10, tenths % 10)
    }
}

/// What the deck's firmware adds to its version (`src/link/mod.rs`).
const FW_SUFFIX: &str = "-openwalkietalkie";

/// The deck's firmware as the pages print it, in mono: `0.13.0 · Rev B`, or
/// the version alone while the deck has not named its board.
///
/// The deck calls its build `0.13.0-openwalkietalkie`; the suffix tells the
/// owner nothing, so it is dropped.
pub fn fw_text(fw: &str, board: Option<&str>) -> String {
    let fw = fw.trim();
    let version = fw.strip_suffix(FW_SUFFIX).unwrap_or(fw);
    match board.map(str::trim).filter(|board| !board.is_empty()) {
        Some(board) if version.is_empty() => board.to_string(),
        Some(board) => format!("{version} \u{b7} {board}"),
        None => version.to_string(),
    }
}

/// The longest path [`home_short`] prints whole, in characters.
pub const PATH_MAX: usize = 50;

/// A path as the pages print it: the home directory as `~` and, past
/// [`PATH_MAX`] characters, the folders after the first given up for `…`,
/// nearest the head first, until it fits:
/// `~/Library/…/OpenWalkieTalkie/claude-marketplace`.
///
/// The name at the end is never cut. A path whose last part alone is too
/// long stays too long, because a name that is not the whole name points at
/// nothing.
pub fn home_short(path: &str) -> String {
    let home = std::env::var("HOME").ok();
    shorten(path, home.as_deref())
}

/// [`home_short`] with the home directory given.
fn shorten(path: &str, home: Option<&str>) -> String {
    let home = home
        .map(|home| home.trim_end_matches('/'))
        .filter(|home| !home.is_empty());
    let path = match home {
        Some(home) if path == home => "~".to_string(),
        Some(home) => match path.strip_prefix(home) {
            Some(rest) if rest.starts_with('/') => format!("~{rest}"),
            _ => path.to_string(),
        },
        None => path.to_string(),
    };
    let parts: Vec<&str> = path.split('/').collect();
    // The head that stays is where the path starts (`~/Library`, or `/opt`
    // of an absolute path). At least one folder after it is given up and at
    // least the name at the end is kept, so there is nothing to shorten in
    // a path of fewer than four parts.
    if path.chars().count() <= PATH_MAX || parts.len() < 4 {
        return path;
    }
    let head = parts[..2].join("/");
    let short = |kept: usize| {
        format!(
            "{head}/\u{2026}/{}",
            parts[parts.len() - kept..].join("/")
        )
    };
    // The most parts there are to keep: all but the head and one folder.
    let most = parts.len() - 3;
    let mut kept = 1;
    while kept < most && short(kept + 1).chars().count() <= PATH_MAX {
        kept += 1;
    }
    short(kept)
}

/// The UTC fields of `ts`, for the stamps no time zone can place.
fn utc_clock(ts: Ts) -> (i32, u32, u32, u32, u32) {
    let seconds = ts.millis() / 1_000;
    let (year, month, day) = civil_from_days(i64::try_from(seconds / 86_400).unwrap_or(i64::MAX));
    let of_day = u32::try_from(seconds % 86_400).unwrap_or_default();
    (year, month, day, of_day / 3_600, of_day % 3_600 / 60)
}

/// The days from 1970-01-01 to a date of the proleptic Gregorian calendar
/// (Howard Hinnant's `days_from_civil`).
fn days_from_civil(year: i32, month: u32, day: u32) -> i64 {
    let (month, day) = (i64::from(month), i64::from(day));
    let year = i64::from(year) - i64::from(month <= 2);
    let era = year.div_euclid(400);
    let year_of_era = year.rem_euclid(400);
    let day_of_year = (153 * (month + if month > 2 { -3 } else { 9 }) + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

/// The date `days` after 1970-01-01: [`days_from_civil`] the other way round.
fn civil_from_days(days: i64) -> (i32, u32, u32) {
    let days = days.saturating_add(719_468);
    let era = days.div_euclid(146_097);
    let day_of_era = days.rem_euclid(146_097);
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let shifted = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * shifted + 2) / 5 + 1;
    let month = if shifted < 10 { shifted + 3 } else { shifted - 9 };
    let year = year_of_era + era * 400 + i64::from(month <= 2);
    (
        i32::try_from(year).unwrap_or(i32::MAX),
        u32::try_from(month).unwrap_or(1),
        u32::try_from(day).unwrap_or(1),
    )
}

// ---------------------------------------------------------------------
// The installer's words (the Integrations page, Footprint's Remove…)
// ---------------------------------------------------------------------

/// A target's name as the service's own `Target::display_name` writes it,
/// so the plan dialog, the CLI's `--help` and the log all say "Claude Code"
/// or none of them does. A target this build does not know is named as
/// itself.
pub fn party_name(target: &str) -> &str {
    match target {
        "claude" => "Claude Code",
        "codex" => "Codex CLI",
        "codex-desktop" => "Codex Desktop",
        "gemini" => "Gemini CLI",
        "claude-desktop" => "Claude Desktop",
        other => other,
    }
}

/// What the installer's status word means, in one sentence: the tooltip on
/// the Integrations page's **Repair**, and what a run that is not done says
/// is still wrong. A word this build does not know is reported as
/// unreadable rather than explained, because nothing here knows what it
/// means.
pub fn status_help(status: &Status) -> &'static str {
    match status {
        Status::Installed => "deckd's plugin, hook entries, or desktop launcher are in place and current.",
        Status::Outdated => "what is on disk differs from this build, Claude has not registered the plugin yet, or settings.json still carries the pre-plugin hooks; Repair rewrites or migrates it.",
        Status::Absent => "nothing of deckd's is installed for this integration.",
        Status::Conflict => "a file deckd must read cannot be parsed, or holds a shape the CLI would reject. deckd never overwrites it.",
        Status::PathDrift => "the hooks name a binary that is not this app's any more; Repair rewrites them.",
        Status::HooksNotFiring => "Codex's own hooks/list reports none of deckd's hooks: they are on disk and dead.",
        Status::Paused => "Codex reports the hooks trusted but switched off in /hooks.",
        Status::Untrusted => "Codex: written but never reviewed, so it does not run. Review it in /hooks, or install with trust.",
        Status::Modified => "Codex: reviewed once, then the path, timeout or matcher changed, so it stopped running silently.",
        Status::Unknown(_) => "this app does not know this word: it is newer than this build. Nothing here can say whether it is healthy.",
    }
}

/// [`status_help`] in the window's language.
pub fn status_help_localized(status: &Status, language: Language) -> &'static str {
    language.text(status_help(status))
}

/// The plan dialog's heading, and what a run is called in its toasts:
/// `Repair — Claude Code`.
pub fn plan_headline_localized(verb: Verb, target: &str, language: Language) -> String {
    let what = match verb {
        Verb::Install => language.text("Install"),
        Verb::Uninstall => language.text("Remove"),
        Verb::Repair => language.text("Repair"),
    };
    format!("{what} \u{2014} {}", party_name(target))
}

/// What a plan that writes nothing says. `install` is idempotent by content,
/// so this is the honest answer for a target that is already exactly right —
/// and it is not an error.
pub const NOTHING_TO_WRITE: &str = "Nothing to write: these files are already what this run \
     would put in them.";

// ---------------------------------------------------------------------
// A run the service could not finish by itself (amendment B4), on the
// Integrations page and in first run alike
// ---------------------------------------------------------------------

/// A next step the service could not run itself: `run: <command>`.
const RUN: &str = "run:";

/// A next step whose command the service ran and saw fail: `WARNING: …`.
const WARNING: &str = "WARNING";

/// What an applied install, repair or removal left to the owner, from its
/// receipt's `next_steps`: the commands the service found no CLI to run
/// (`run: …`) and the ones that failed when it ran them (`WARNING: …`), as
/// it wrote them. Every other step only tells — a session already open
/// loads the plugin after `/reload-plugins`, a word about the Channels
/// helper, both on every install of the Claude Code plugin — and leaves
/// nothing undone.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Leftover {
    /// The commands, without their `run:`.
    pub commands: Vec<String>,
    /// The warnings, whole.
    pub warnings: Vec<String>,
}

impl Leftover {
    /// From an applied run's receipt.
    pub fn of(receipt: &Value) -> Leftover {
        let mut leftover = Leftover::default();
        let steps = receipt
            .get("next_steps")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_str);
        for step in steps {
            if let Some(command) = step.strip_prefix(RUN) {
                leftover.commands.push(command.trim().to_string());
            } else if step.starts_with(WARNING) {
                leftover.warnings.push(step.to_string());
            }
        }
        leftover
    }

    pub fn is_empty(&self) -> bool {
        self.commands.is_empty() && self.warnings.is_empty()
    }
}

/// Whether a run is done: it left the owner nothing, and the installer,
/// read again, says what the run was for — installed after an install or a
/// repair, absent after a removal. `status` is `None` when that read failed
/// or has no row for the target, and then the receipt is taken at its word.
pub fn finished(verb: Verb, leftover: &Leftover, status: Option<&Status>) -> bool {
    leftover.is_empty()
        && match (verb, status) {
            (_, None) => true,
            (Verb::Uninstall, Some(status)) => *status == Status::Absent,
            (Verb::Install | Verb::Repair, Some(status)) => *status == Status::Installed,
        }
}

/// The one command that finishes a run from a terminal, where the owner's
/// own `PATH` finds the CLI the service could not: this app's `deckd` (the
/// one beside its executable) doing the same verb on the same target again,
/// which runs what the service left and puts right what the installer still
/// finds wrong — the command the service's own status detail names. The
/// service's `run:` lines cannot be pasted as they are (a path under
/// `Application Support` is not quoted in them), so they are the answer only
/// for a copy with no `deckd` beside it, one after the other; with none of
/// those either, the verb for whichever `deckd` the terminal finds.
pub fn finish_command(
    deckd: Option<&Path>,
    verb: Verb,
    target: &str,
    commands: &[String],
) -> String {
    let verb = verb.method().trim_start_matches("integrations.");
    match deckd {
        Some(deckd) => format!(
            "{} integrations {verb} {}",
            shell_word(&deckd.to_string_lossy()),
            shell_word(target)
        ),
        None if !commands.is_empty() => commands.join(" && "),
        None => format!("deckd integrations {verb} {}", shell_word(target)),
    }
}

/// `word` as a shell reads it back: as it is when it holds nothing a shell
/// would split or expand, else in single quotes.
fn shell_word(word: &str) -> String {
    let plain = !word.is_empty()
        && word
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "/._-+,:@%=".contains(c));
    if plain {
        word.to_string()
    } else {
        format!("'{}'", word.replace('\'', r"'\''"))
    }
}

/// Why the service refused a call, in its own words: the installer's
/// refusal carries them as `data.why`; anything else is the error itself.
pub fn reason(error: &ClientError) -> String {
    error
        .api_error()
        .and_then(|e| {
            e.data
                .get("why")
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .unwrap_or_else(|| error.to_string())
}

// ---------------------------------------------------------------------
// The test question (the Integrations page, and first run's last step)
// ---------------------------------------------------------------------

/// The mark the test question's line leads with.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TestMark {
    /// Answered on the deck.
    Done,
    /// On the deck, waiting for an answer.
    Waiting,
    /// Not answered.
    Missed,
    /// Nothing went: the deck was not linked.
    Note,
}

/// What the line beside **Send a Test Question** says about the last one,
/// and its mark. Nothing before the first test; nothing for a state this
/// build does not know, or for one no question is in (`heard` is the voice
/// test's); and nothing for an answer the service gave no time for, rather
/// than a time made up.
pub fn test_line(
    question: Option<&QuestionTest>,
    language: Language,
) -> Option<(String, TestMark)> {
    let question = question?;
    Some(match &question.state {
        TestState::Sent | TestState::Waiting => (
            language
                .text("A test question is on the deck. Answer it there.")
                .to_string(),
            TestMark::Waiting,
        ),
        TestState::Answered => {
            // Whole seconds, to the nearest, and at least one: a question
            // answered on the deck took a press.
            let seconds = question.after_ms?.saturating_add(500) / 1_000;
            (
                language.format(
                    "The last test question arrived on the deck and was answered there after {0} s.",
                    &[&seconds.max(1).to_string()],
                ),
                TestMark::Done,
            )
        }
        TestState::Expired => (
            language
                .text("The last test question was not answered on the deck.")
                .to_string(),
            TestMark::Missed,
        ),
        TestState::NoDeck => (
            language
                .text("The deck is not linked, so no test question was sent.")
                .to_string(),
            TestMark::Note,
        ),
        TestState::Heard | TestState::Unknown(_) => return None,
    })
}

// ---------------------------------------------------------------------
// Sentences one page reads out of a reply
// ---------------------------------------------------------------------

/// What the service's `degraded` reason means: `None` while it is not
/// degraded. A reason this build has never seen is still said, as it came.
///
/// The two port words are the local API's alone (the Service page's Local
/// API row says them): the deck, the Claude Code plugin's hooks and the
/// connected apps reach the service over its socket, so another program on
/// the TCP port closes the local API and nothing else.
pub fn degraded_banner_localized(
    service: &deck_model::ServiceState,
    language: Language,
) -> Option<String> {
    let reason = service.degraded.as_deref()?;
    let port = service.port.to_string();
    Some(match reason {
        deck_gui_core::admin::HIJACK_RISK => language.format(
            "Another program answers on port {0}, so the local API is closed and local tools that call the port reach that program. Change the port.",
            &[&port],
        ),
        deck_gui_core::admin::PORT_IN_USE => language.format(
            "Another program holds port {0}, so the local API is closed until the port is free. Nothing shipped with the deck uses it.",
            &[&port],
        ),
        "shutdown" => language.text("The service is shutting down.").to_string(),
        other => language.format("The service is degraded: {0}.", &[(other)]),
    })
}

/// The `perm.request` reply, said out loud. A `prompt_not_shown` is not a
/// failure and must not read as one: it is the service's own word for a
/// prompt that did not surface, and the sentences under it are the
/// service's.
pub fn prompt_line_localized(prompt: &Prompt, language: Language) -> String {
    if prompt.not_shown() {
        return language.format("macOS was not asked for the {0}: this build cannot show that prompt yet, so nothing changed.", &[language.text(&prompt.which)]);
    }
    language.format(
        "{0}: {1}",
        &[language.text(&prompt.which), language.text(&prompt.status)],
    )
}

/// The headline of a `voice.test` result — what was checked and what came of
/// it, with no claim the reply does not make.
pub fn test_headline_localized(report: &TestReport, language: Language) -> String {
    match report.what.as_str() {
        "speaker" => match report.played {
            Some(true) => language
                .text("Chime queued on the audio thread")
                .to_string(),
            Some(false) => language
                .text("The audio thread refused the chime")
                .to_string(),
            None => language
                .text("The daemon answered without saying whether it queued anything")
                .to_string(),
        },
        "mic" => language
            .text("Input as the route machine sees it")
            .to_string(),
        "stt" => language
            .text("The recogniser, as it reported itself at start-up")
            .to_string(),
        "tts" => language
            .text("The synthesiser, as it reported itself at start-up")
            .to_string(),
        // A `what` from a newer daemon: named, and nothing claimed for it.
        other => language.format("{0}: the daemon answered", &[(other)]),
    }
}

// ---------------------------------------------------------------------------
// The login item (§10.2, contract §1 M2)
// ---------------------------------------------------------------------------

/// `SMAppService.status` (contract §1 M2). The numbers are Apple's:
/// 0 `notRegistered`, 1 `enabled`, 2 `requiresApproval`, 3 `notFound`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LoginItem {
    NotRegistered,
    Enabled,
    RequiresApproval,
    NotFound,
    /// A status this build has no name for, kept as itself.
    Unknown(isize),
}

impl LoginItem {
    pub const fn from_code(code: isize) -> LoginItem {
        match code {
            0 => LoginItem::NotRegistered,
            1 => LoginItem::Enabled,
            2 => LoginItem::RequiresApproval,
            3 => LoginItem::NotFound,
            other => LoginItem::Unknown(other),
        }
    }

    pub const fn code(self) -> isize {
        match self {
            LoginItem::NotRegistered => 0,
            LoginItem::Enabled => 1,
            LoginItem::RequiresApproval => 2,
            LoginItem::NotFound => 3,
            LoginItem::Unknown(code) => code,
        }
    }

    /// **The measured rule (M2).** `register()` returned `Ok` on a fresh
    /// state while the status was still 3 (`notFound`), so the return value
    /// is not evidence that anything was registered. Only these two statuses
    /// are: `enabled` is on, and `requiresApproval` is on and waiting for the
    /// operator in Login Items.
    pub const fn registered(self) -> bool {
        matches!(self, LoginItem::Enabled | LoginItem::RequiresApproval)
    }

    pub const fn tone(self) -> Tone {
        match self {
            LoginItem::Enabled => Tone::Green,
            LoginItem::RequiresApproval => Tone::Amber,
            LoginItem::NotRegistered => Tone::Gray,
            LoginItem::NotFound | LoginItem::Unknown(_) => Tone::Red,
        }
    }
}

/// System Settings › General › Login Items, verified against the extension
/// bundle id on this machine.
pub const LOGIN_ITEMS_SETTINGS: &str =
    "x-apple.systempreferences:com.apple.LoginItems-Settings.extension";

/// System Settings › Bluetooth, likewise: where a deck is paired.
pub const BLUETOOTH_SETTINGS: &str = "x-apple.systempreferences:com.apple.BluetoothSettings";

#[cfg(test)]
mod tests {
    use super::*;

    /// 2026-11-11T10:02:00.000Z.
    const TEN_TWO_UTC: Ts = Ts(1_794_391_320_000);

    /// `hour:minute` on `year-month-day`, on this Mac's clock.
    fn local(year: i32, month: u32, day: u32, hour: u32, minute: u32) -> Ts {
        local_ts(year, month, day, hour, minute).expect("a minute this time zone has")
    }

    // ---- the shared formatters ---------------------------------------

    #[test]
    fn an_age_is_whole_units_rounded_down_and_never_negative() {
        let now = TEN_TWO_UTC;
        let before = |seconds: u64| Ts::from_millis(now.millis() - seconds * 1_000);
        let english = Language::English;
        for (seconds, said) in [
            (0, "just now"),
            (1, "1 s ago"),
            // The Integrations board's Claude Code row.
            (4, "4 s ago"),
            (59, "59 s ago"),
            (60, "1 min ago"),
            (119, "1 min ago"),
            (3_599, "59 min ago"),
            (3_600, "1 h ago"),
            (86_399, "23 h ago"),
            (86_400, "1 d ago"),
            (30 * 86_400, "30 d ago"),
        ] {
            assert_eq!(ago(before(seconds), now, english), said, "{seconds} s");
        }
        // Under a second is not yet a second.
        assert_eq!(
            ago(Ts::from_millis(now.millis() - 999), now, english),
            "just now"
        );
        // The service's clock a moment ahead of this one: not a negative age.
        assert_eq!(
            ago(Ts::from_millis(now.millis() + 5_000), now, english),
            "just now"
        );
        assert_eq!(ago(Ts::from_millis(0), Ts::from_millis(0), english), "just now");
    }

    #[test]
    fn an_age_is_said_in_the_windows_language() {
        crate::i18n::install();
        let now = TEN_TWO_UTC;
        let before = |seconds: u64| Ts::from_millis(now.millis() - seconds * 1_000);
        assert_eq!(ago(now, now, Language::Japanese), "たった今");
        assert_eq!(ago(before(4), now, Language::SimplifiedChinese), "4 秒前");
        assert_eq!(ago(before(60), now, Language::TraditionalChinese), "1 分鐘前");
        assert_eq!(ago(before(3 * 3_600), now, Language::Japanese), "3 時間前");
        assert_eq!(ago(before(2 * 86_400), now, Language::SimplifiedChinese), "2 天前");
    }

    #[test]
    fn this_macs_clock_reads_a_minute_back_as_the_minute_it_was_made_from() {
        for minute in [
            (2026, 11, 11, 10, 2),
            (2026, 11, 11, 9, 41),
            (2026, 11, 11, 0, 0),
            (2027, 1, 1, 0, 30),
            (2024, 2, 29, 23, 59),
            (2000, 1, 1, 12, 0),
        ] {
            let (year, month, day, hour, minute_of_hour) = minute;
            let ts = local(year, month, day, hour, minute_of_hour);
            assert_eq!(local_clock(ts), minute, "{minute:?}");
            assert_eq!(ts.millis() % 60_000, 0, "on the minute");
        }
        // A date no calendar has, or a time no clock shows.
        for (year, month, day, hour, minute) in [
            (2026, 13, 1, 0, 0),
            (2026, 0, 1, 0, 0),
            (2026, 2, 30, 0, 0),
            (2025, 2, 29, 0, 0),
            (2026, 11, 31, 0, 0),
            (2026, 11, 11, 24, 0),
            (2026, 11, 11, 10, 60),
            (2026, 11, 0, 10, 0),
        ] {
            assert_eq!(
                local_ts(year, month, day, hour, minute),
                None,
                "{year}-{month}-{day} {hour}:{minute}"
            );
        }
        // A stamp no time zone can place is still read, in UTC.
        assert_eq!(utc_clock(TEN_TWO_UTC), (2026, 11, 11, 10, 2));
        let _ = local_clock(Ts::from_millis(u64::MAX));
    }

    #[test]
    fn a_moment_is_today_yesterday_or_its_date_by_the_calendar() {
        crate::i18n::install();
        let english = Language::English;
        let now = local(2026, 11, 11, 10, 2);
        for (when, said) in [
            // The NotLinked board's caption.
            (local(2026, 11, 11, 9, 41), "today, 09:41"),
            (local(2026, 11, 11, 0, 0), "today, 00:00"),
            // A moment later today: the service's clock a little ahead.
            (local(2026, 11, 11, 10, 3), "today, 10:03"),
            (local(2026, 11, 10, 23, 59), "yesterday, 23:59"),
            (local(2026, 11, 10, 0, 5), "yesterday, 00:05"),
            (local(2026, 11, 9, 9, 41), "2026-11-09, 09:41"),
            (local(2025, 11, 11, 9, 41), "2025-11-11, 09:41"),
            (local(2026, 11, 12, 8, 0), "2026-11-12, 08:00"),
        ] {
            assert_eq!(day_and_time(when, now, english), said);
        }
        // Yesterday by the calendar, across a year's end.
        assert_eq!(
            day_and_time(local(2026, 12, 31, 23, 59), local(2027, 1, 1, 0, 30), english),
            "yesterday, 23:59"
        );
        // In the window's language, each placing the time its own way.
        assert_eq!(
            day_and_time(local(2026, 11, 11, 9, 41), now, Language::SimplifiedChinese),
            "今天 09:41"
        );
        assert_eq!(
            day_and_time(local(2026, 11, 10, 9, 41), now, Language::Japanese),
            "昨日 09:41"
        );
        assert_eq!(
            day_and_time(local(2026, 11, 9, 9, 41), now, Language::TraditionalChinese),
            "2026-11-09 09:41"
        );
    }

    #[test]
    fn the_calendar_counts_days_both_ways_across_months_years_and_leap_days() {
        assert_eq!(days_from_civil(1970, 1, 1), 0);
        assert_eq!(days_from_civil(2026, 11, 11), 20_768);
        assert_eq!(civil_from_days(20_768), (2026, 11, 11));
        assert_eq!(civil_from_days(-1), (1969, 12, 31));
        for days in (-800_000..800_000).step_by(397) {
            let (year, month, day) = civil_from_days(days);
            assert_eq!(days_from_civil(year, month, day), days, "{year}-{month}-{day}");
        }
        // The day before 1 March, in a leap year and in another.
        assert_eq!(civil_from_days(days_from_civil(2024, 3, 1) - 1), (2024, 2, 29));
        assert_eq!(civil_from_days(days_from_civil(2026, 3, 1) - 1), (2026, 2, 28));
        assert_eq!(civil_from_days(days_from_civil(2100, 3, 1) - 1), (2100, 2, 28));
        assert_eq!(civil_from_days(days_from_civil(2000, 3, 1) - 1), (2000, 2, 29));
        assert_eq!(civil_from_days(days_from_civil(2027, 1, 1) - 1), (2026, 12, 31));
    }

    #[test]
    fn a_size_is_finders_thousands_and_the_unit_steps_up_at_a_thousand() {
        for (bytes, said) in [
            // The Footprint board.
            (4_096, "4 KB"),
            (180_000, "180 KB"),
            (9_400_000, "9 MB"),
            (77_000_000, "77 MB"),
            (428_000_000, "428 MB"),
            (2_100_000_000, "2.1 GB"),
            // The two downloads, at their real sizes.
            (1_624_555_275, "1.6 GB"),
            (364_816_464, "365 MB"),
            // Nothing is nothing; anything else is at least a kilobyte.
            (0, "0 KB"),
            (1, "1 KB"),
            (499, "1 KB"),
            (1_499, "1 KB"),
            (1_500, "2 KB"),
            // Where the unit steps up.
            (999_499, "999 KB"),
            (999_500, "1 MB"),
            (999_499_999, "999 MB"),
            (999_500_000, "1.0 GB"),
            (12_345_000_000, "12.3 GB"),
        ] {
            assert_eq!(size_text(bytes), said, "{bytes} bytes");
        }
        assert!(size_text(u64::MAX).ends_with(" GB"));
    }

    #[test]
    fn the_firmware_reads_as_its_version_and_board_without_the_build_name() {
        assert_eq!(
            fw_text("0.13.0-openwalkietalkie", Some("Rev B")),
            "0.13.0 \u{b7} Rev B"
        );
        assert_eq!(fw_text("0.13.0", Some("Rev B")), "0.13.0 \u{b7} Rev B");
        // The board not named yet, or named as nothing.
        assert_eq!(fw_text("0.13.0-openwalkietalkie", None), "0.13.0");
        assert_eq!(fw_text(" 0.12.1-openwalkietalkie ", Some(" ")), "0.12.1");
        // Only the build name at the end is the deck's own addition.
        assert_eq!(
            fw_text("0.13.0-openwalkietalkie-dirty", Some("LilyGo")),
            "0.13.0-openwalkietalkie-dirty \u{b7} LilyGo"
        );
        assert_eq!(fw_text("-openwalkietalkie", Some("Rev A")), "Rev A");
    }

    #[test]
    fn a_path_starts_at_home_and_gives_up_the_folders_after_its_head_to_fit() {
        let home = Some("/Users/owner/");
        for (path, said) in [
            // The Footprint board's rows.
            (
                "/Users/owner/Library/Application Support/OpenWalkieTalkie/claude-marketplace",
                "~/Library/\u{2026}/OpenWalkieTalkie/claude-marketplace",
            ),
            (
                "/Users/owner/Library/Preferences/ai.cohdl.openwalkietalkie.plist",
                "~/Library/\u{2026}/ai.cohdl.openwalkietalkie.plist",
            ),
            (
                "/Users/owner/Library/Caches/OpenWalkieTalkie/tts/kokoro",
                "~/Library/Caches/OpenWalkieTalkie/tts/kokoro",
            ),
            (
                "/Users/owner/Library/Application Support/OpenWalkieTalkie",
                "~/Library/Application Support/OpenWalkieTalkie",
            ),
            ("/Users/owner/Library/Logs/OpenWalkieTalkie", "~/Library/Logs/OpenWalkieTalkie"),
            ("/Users/owner", "~"),
            // A directory that only starts like home is not home.
            ("/Users/owner2/notes", "/Users/owner2/notes"),
            // Outside home the head is shortened the same way.
            (
                "/opt/homebrew/Cellar/openwalkietalkie/0.13.0/libexec/share/firmware",
                "/opt/\u{2026}/0.13.0/libexec/share/firmware",
            ),
            // The name at the end is never cut.
            (
                "/Users/owner/Library/Caches/a-name-that-alone-is-longer-than-fifty-characters.bin",
                "~/Library/\u{2026}/a-name-that-alone-is-longer-than-fifty-characters.bin",
            ),
        ] {
            assert_eq!(shorten(path, home), said, "{path}");
        }
        // No home to name: the head is all there is to shorten.
        assert_eq!(
            shorten(
                "/Users/owner/Library/Application Support/OpenWalkieTalkie/claude-marketplace",
                None
            ),
            "/Users/\u{2026}/OpenWalkieTalkie/claude-marketplace"
        );
        for path in [
            "/Users/owner/Library/Application Support/OpenWalkieTalkie/claude-marketplace",
            "/var/folders/xy/abcdefghijklmnopqrstuvwxyz0123/T/openwalkietalkie/stt/model.bin",
        ] {
            assert!(shorten(path, home).chars().count() <= PATH_MAX, "{path}");
        }
    }

    #[test]
    fn the_harness_can_pin_the_clock_and_hand_it_back() {
        let pinned = Ts::from_millis(86_400_000);
        set_fixed_now(Some(pinned));
        assert_eq!(now(), pinned);
        assert_eq!(now(), pinned, "a pinned clock does not move");
        set_fixed_now(None);
        assert!(now() > pinned, "this Mac's clock again");
    }

    // ---- the installer's words ---------------------------------------

    #[test]
    fn every_installer_word_has_a_sentence_of_its_own_in_every_language() {
        crate::i18n::install();
        let mut words = Status::KNOWN.to_vec();
        words.push(Status::parse("some_future_word"));
        let mut sentences: Vec<&str> = Vec::new();
        for status in words {
            let help = status_help(&status);
            assert!(!help.is_empty(), "{status:?} has no sentence");
            assert!(
                !sentences.contains(&help),
                "{status:?} shares its sentence with another word"
            );
            sentences.push(help);
            assert!(
                deck_gui_core::i18n::has_entry(help),
                "{status:?}'s sentence has no row"
            );
            for language in [
                Language::SimplifiedChinese,
                Language::TraditionalChinese,
                Language::Japanese,
            ] {
                assert_ne!(
                    status_help_localized(&status, language),
                    help,
                    "{status:?} in {language:?}"
                );
            }
        }
        // A word from a newer service says it cannot be read: never green,
        // and never quietly "absent".
        let newer = Status::parse("some_future_word");
        assert!(status_help(&newer).contains("does not know this word"));
    }

    #[test]
    fn an_out_of_date_plugin_is_put_right_by_the_button_the_page_has() {
        crate::i18n::install();
        // The Integrations page offers **Repair** for every word but
        // installed and absent, and the sentence names it, in each language
        // by the button's own word.
        let help = status_help(&Status::Outdated);
        assert!(help.ends_with("Repair rewrites or migrates it."), "{help}");
        assert!(!help.contains("Update"), "{help}");
        for (language, repair) in [
            (Language::SimplifiedChinese, "修复"),
            (Language::TraditionalChinese, "修復"),
            (Language::Japanese, "修復"),
        ] {
            assert_eq!(language.text("Repair"), repair);
            assert!(
                status_help_localized(&Status::Outdated, language).contains(repair),
                "{language:?}"
            );
        }
    }

    #[test]
    fn a_plan_heading_is_the_verb_and_the_targets_own_name() {
        crate::i18n::install();
        let english = Language::English;
        assert_eq!(
            plan_headline_localized(Verb::Install, "claude", english),
            "Install \u{2014} Claude Code"
        );
        assert_eq!(
            plan_headline_localized(Verb::Uninstall, "claude-desktop", english),
            "Remove \u{2014} Claude Desktop"
        );
        assert_eq!(
            plan_headline_localized(Verb::Repair, "codex", english),
            "Repair \u{2014} Codex CLI"
        );
        // A target this build has never heard of is still named, as itself.
        assert_eq!(
            plan_headline_localized(Verb::Install, "future-cli", english),
            "Install \u{2014} future-cli"
        );
        assert!(NOTHING_TO_WRITE.contains("already"));
        assert!(deck_gui_core::i18n::has_entry(NOTHING_TO_WRITE));
    }

    // ---- a run the service could not finish ---------------------------
    //
    // The rule and the command are tested where the two pages use them
    // (`tests/integrations_page.rs`, `tests/first_run.rs`); the shell's
    // quoting, which neither can reach, is here.

    #[test]
    fn a_shell_word_comes_back_as_itself() {
        assert_eq!(
            shell_word("/Applications/OpenWalkieTalkie.app/Contents/MacOS/deckd"),
            "/Applications/OpenWalkieTalkie.app/Contents/MacOS/deckd"
        );
        assert_eq!(
            shell_word("/Volumes/My Disk/deckd"),
            "'/Volumes/My Disk/deckd'"
        );
        assert_eq!(shell_word("/tmp/it's/deckd"), r"'/tmp/it'\''s/deckd'");
        assert_eq!(shell_word("$HOME"), "'$HOME'");
        assert_eq!(shell_word(""), "''");
        assert_eq!(shell_word("codex-desktop"), "codex-desktop");
    }

    // ---- sentences read out of a reply -------------------------------

    #[test]
    fn the_degraded_banner_tells_a_lost_bind_from_a_stranger_that_answers() {
        crate::i18n::install();
        let banner = |service: &deck_model::ServiceState| {
            degraded_banner_localized(service, Language::English)
        };
        let mut service = deck_model::ServiceState {
            port: 47_201,
            ..deck_model::ServiceState::default()
        };
        assert_eq!(banner(&service), None);
        service.degraded = Some("port_in_use".into());
        let bound = banner(&service).unwrap();
        assert_eq!(
            bound,
            "Another program holds port 47201, so the local API is closed until the port is \
             free. Nothing shipped with the deck uses it."
        );
        service.degraded = Some("hooks_hijack_risk".into());
        let held = banner(&service).unwrap();
        assert!(held.starts_with("Another program answers on port 47201"), "{held}");
        assert!(held.ends_with("Change the port."), "{held}");
        // Neither says a word about Claude or its hooks, in any language:
        // they reach the service over its socket, not the port.
        for reason in ["port_in_use", "hooks_hijack_risk"] {
            service.degraded = Some(reason.into());
            for language in Language::ALL {
                let said = degraded_banner_localized(&service, language).unwrap();
                assert!(said.contains("47201") && said.contains("API"), "{language:?}: {said}");
                assert!(!said.contains("Claude"), "{language:?}: {said}");
                for hooks in ["hook", "钩子", "掛鉤", "フック"] {
                    assert!(!said.to_lowercase().contains(hooks), "{language:?}: {said}");
                }
            }
        }
        service.degraded = Some("shutdown".into());
        assert!(banner(&service).unwrap().contains("shutting down"));
        // A reason this build has never seen is still said out loud, and
        // as it came in every language, even when it reads like a word a
        // table has.
        service.degraded = Some("Settings".into());
        for language in Language::ALL {
            let said = degraded_banner_localized(&service, language).unwrap();
            assert!(said.contains("Settings"), "{language:?}: {said}");
        }
    }

    #[test]
    fn every_word_the_shared_sentences_say_has_a_row() {
        let source = include_str!("present.rs");
        assert_eq!(crate::i18n::missing_rows(source), Vec::<String>::new());
        // Pinned, so a lookup the scanner stopped seeing fails here rather
        // than passing as a module with less to translate.
        assert_eq!(crate::i18n::literals(source).len(), 29);
    }
}
