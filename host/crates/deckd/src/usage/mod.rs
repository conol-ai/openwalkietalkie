//! The Usage face feeder (§4.10 `device.usage.set`): how much of each
//! coding agent's rate-limit windows is used, the way Zeron shows it —
//! one bar per window, with the countdown to its reset.
//!
//! Three sources reduce to one [`Window`] shape, and the core keeps the
//! latest set per [`Provider`] ([`Feeder`]) and renders the face from
//! them, so the deck reads one line whoever reported last:
//!
//! - **Claude, probed** ([`claude`]): `GET /api/oauth/usage` on
//!   `api.anthropic.com` with the Claude Code login — the same call Zeron
//!   and the usage menu-bar apps make — polled every `[usage] poll_s` and
//!   on the deck's ENTER (`usage.refresh_requested`). Works with no Claude
//!   session open, which the status line cannot.
//! - **Claude, reported**: the `rate_limits` of the status-line JSON, when
//!   the operator has chained `deckd hook claude-statusline` in front of
//!   their own line (`hooks.claude_statusline`). The plugin install does
//!   not chain it; it is the per-session feeder for those who do.
//! - **Codex**: the app-server's `account/rateLimits/read` at connect and
//!   on ENTER, and its `account/rateLimits/updated` after every turn
//!   (`crate::codexd`).
//!
//! `[usage] providers` names the providers to show; a provider that is
//! not listed is never probed and never drawn. Codex alone by default: the
//! Claude bars read the Claude Code login, so they wait for the owner to
//! switch them on (the Usage face's panel, `config.set usage.providers`).
use std::collections::BTreeMap;
use std::time::Instant;

use deck_model::{AdapterKind, UsageItem};
use serde_json::Value;

use crate::config::UsageCfg;

pub mod claude;

/// The longest `UsageItem.name` the wire line carries (`n` ≤ 20).
const NAME_MAX: usize = 20;
/// The longest `UsageItem.reset` (`r` ≤ 24).
const RESET_MAX: usize = 24;
/// The longest `period` (≤ 12).
const PERIOD_MAX: usize = 12;
/// The face draws at most this many bars.
const ITEMS_MAX: usize = 6;
/// How often a tick re-renders the countdowns: they are written to the
/// minute, so a re-push more often than that changes nothing.
const RERENDER_S: u64 = 60;

/// Who a set of windows belongs to; the order is the order on the face.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Provider {
    Claude,
    Codex,
}

impl Provider {
    pub const ALL: [Provider; 2] = [Provider::Claude, Provider::Codex];

    /// The word in front of every window's label on the face.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Provider::Claude => "Claude",
            Provider::Codex => "Codex",
        }
    }

    /// The principal a push from this provider is made as.
    #[must_use]
    pub const fn adapter(self) -> AdapterKind {
        match self {
            Provider::Claude => AdapterKind::Claude,
            Provider::Codex => AdapterKind::Codex,
        }
    }

    /// Whether `[usage] providers` names this provider: `"claude"` (and the
    /// older `"claude-statusline"`, which the default once was) or
    /// `"codex"`, case-insensitively.
    #[must_use]
    pub fn enabled(self, cfg: &UsageCfg) -> bool {
        cfg.providers.iter().any(|p| {
            let p = p.trim().to_ascii_lowercase();
            let (word, prefix) = match self {
                Provider::Claude => ("claude", "claude-"),
                Provider::Codex => ("codex", "codex-"),
            };
            p == word || p.starts_with(prefix)
        })
    }
}

/// One rate-limit window as every source reports it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Window {
    /// `"session"`, `"week"`, `"Opus week"`, `"extra"`…: what follows the
    /// provider's name on the face.
    pub label: String,
    /// 0..=100.
    pub percent: u8,
    /// Unix seconds when the window resets, when the source said.
    pub resets_at: Option<u64>,
}

/// An unused, redeemable reset grant, separate from a meter's scheduled reset.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResetGrant {
    pub count: u64,
    pub starts_at: Option<u64>,
    pub expires_at: u64,
}

/// Claude Code's optional `cedar_ember` block from `?cedar_ember=1`.
/// Keep unused grants even when a limit must be reached before redemption.
#[must_use]
pub fn claude_reset_grants(v: &Value) -> Vec<ResetGrant> {
    let block = &v["cedar_ember"];
    if block["eligible"] != true { return Vec::new(); }
    block["grants"].as_array().into_iter().flatten().filter_map(|g| {
        let count = g["resets_left"].as_u64().filter(|n| *n > 0)?;
        if g["paused"] == true { return None; }
        Some(ResetGrant { count, starts_at: parse_ts(&g["starts_at"]), expires_at: parse_ts(&g["ends_at"])? })
    }).collect()
}

/// Only credit details from `account/rateLimits/read` give expiry dates.
/// A count without details, or an older server, cannot supply a reminder.
#[must_use]
pub fn codex_reset_grants(v: &Value) -> Vec<ResetGrant> {
    let block = &v["rateLimitResetCredits"];
    if block["availableCount"].as_u64().unwrap_or(0) == 0 { return Vec::new(); }
    block["credits"].as_array().into_iter().flatten().filter_map(|g| {
        if g["status"] != "available" || g["resetType"] != "codexRateLimits" { return None; }
        Some(ResetGrant { count: 1, starts_at: parse_ts(&g["grantedAt"]), expires_at: parse_ts(&g["expiresAt"])? })
    }).collect()
}

fn reset_tip(grants: &[ResetGrant], now: u64) -> Option<String> {
    // Group only grants with the nearest expiry: later grants must not be
    // advertised as expiring at the first deadline.
    let expiry = grants.iter().filter(|g| g.count > 0 && g.starts_at.is_none_or(|at| at <= now)
        && g.expires_at > now && g.expires_at - now <= 3 * 86400).map(|g| g.expires_at).min()?;
    let count = grants.iter().filter(|g| g.expires_at == expiry && g.starts_at.is_none_or(|at| at <= now))
        .fold(0u64, |count, g| count.saturating_add(g.count));
    let left = expiry - now;
    let time = if left >= 86400 { format!("{}d {}h", left / 86400, left % 86400 / 3600) }
        else if left >= 3600 { format!("{}h {}m", left / 3600, left % 3600 / 60) }
        else if left >= 60 { format!("{}m", left / 60) } else { "<1m".into() };
    let count_text = if count > 99 { "99+".into() } else { count.to_string() };
    Some(format!("{count_text} {} in {time}", if count == 1 { "reset expires" } else { "resets expire" }))
}

/// `GET /api/oauth/usage`: `{five_hour, seven_day, seven_day_opus?,
/// seven_day_sonnet?, extra_usage?}`, each `{utilization: <percent>,
/// resets_at: <ISO-8601>}` — or, in newer answers, the flat keys null and
/// a `limits: [{kind, percent, resets_at}]` list instead. Both are read.
#[must_use]
pub fn claude_oauth_windows(v: &Value) -> Vec<Window> {
    let mut out = Vec::new();
    for (key, label) in [
        ("five_hour", "session"),
        ("seven_day", "week"),
        ("seven_day_opus", "Opus week"),
        ("seven_day_sonnet", "Sonnet week"),
    ] {
        out.extend(window(&v[key], label, &["utilization", "used_percentage", "percent"]));
    }
    // New servers keep the common flat fields alongside model-specific rows.
    // Read both, deduplicating the common meters by their semantic label.
    for limit in v["limits"].as_array().into_iter().flatten() {
        let label = match limit["kind"].as_str().unwrap_or_default() {
            "session" | "five_hour" => "session".to_string(),
            "weekly_all" | "seven_day" => "week".to_string(),
            "seven_day_opus" => "Opus week".to_string(),
            "seven_day_sonnet" => "Sonnet week".to_string(),
            "weekly_scoped" => {
                let Some(model) = limit["scope"]["model"]["display_name"].as_str() else { continue };
                let model = deck_model::sanitize::collapse(model);
                if model.is_empty() { continue }
                format!("{} week", deck_model::sanitize::cut(&model, 64))
            }
            other => other.replace('_', " "),
        };
        if let Some(w) = window(limit, &label, &["percent", "utilization", "used_percentage"]) {
            if let Some(old) = out.iter_mut().find(|old| old.label == w.label) { *old = w; }
            else { out.push(w); }
        }
    }
    // Put Fable ahead of legacy model meters when the six-row deck is full.
    out.sort_by_key(|w| match w.label.as_str() {
        "session" => 0, "week" => 1, label if label.to_ascii_lowercase().starts_with("fable") => 2, _ => 3,
    });
    // The overage pool is a bar only once it is switched on.
    if v["extra_usage"]["is_enabled"] == Value::Bool(true) {
        out.extend(window(&v["extra_usage"], "extra", &["utilization", "percent"]));
    }
    out
}

/// The status line's `rate_limits: {five_hour, seven_day, spend_limit?}`,
/// each `{used_percentage, resets_at: <unix seconds>}` (Pro/Max only).
#[must_use]
pub fn claude_statusline_windows(v: &Value) -> Vec<Window> {
    let limits = &v["rate_limits"];
    let mut out = Vec::new();
    for (key, label) in [
        ("five_hour", "session"),
        ("seven_day", "week"),
        ("spend_limit", "spend"),
    ] {
        out.extend(window(&limits[key], label, &["used_percentage", "utilization", "percent"]));
    }
    out
}

/// The app-server's `account/rateLimits/read` result and
/// `account/rateLimits/updated` params: `{rateLimits: {primary, secondary}}`
/// (the wrapper is optional), each `{usedPercent, windowDurationMins,
/// resetsAt}`; the protocol's snake_case spelling is read too.
#[must_use]
pub fn codex_windows(v: &Value) -> Vec<Window> {
    let limits = if v["rateLimits"].is_object() {
        &v["rateLimits"]
    } else if v["rate_limits"].is_object() {
        &v["rate_limits"]
    } else {
        v
    };
    let mut out = Vec::new();
    for (key, fallback) in [("primary", "session"), ("secondary", "week")] {
        let w = &limits[key];
        if !w.is_object() {
            continue;
        }
        let minutes = first_number(w, &["windowDurationMins", "windowMinutes", "window_minutes"]);
        let label = codex_label(minutes, fallback);
        out.extend(window(w, &label, &["usedPercent", "used_percent", "utilization"]));
    }
    out
}

/// A Codex window's label from its length: the 5 h and 7 d windows read
/// like Claude's, anything else by its length.
fn codex_label(minutes: Option<f64>, fallback: &str) -> String {
    let Some(minutes) = minutes.filter(|m| *m > 0.0) else {
        return fallback.to_string();
    };
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let m = minutes.round() as u64;
    match m {
        300 => "session".to_string(),
        10080 => "week".to_string(),
        m if m % 1440 == 0 => format!("{}d", m / 1440),
        m if m % 60 == 0 => format!("{}h", m / 60),
        m => format!("{m}m"),
    }
}

/// One window out of an object with a percent under one of `percent_keys`
/// and, optionally, a reset time. Not an object, or no percent → nothing.
fn window(v: &Value, label: &str, percent_keys: &[&str]) -> Option<Window> {
    if !v.is_object() {
        return None;
    }
    let percent = first_number(v, percent_keys)?;
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let percent = percent.round().clamp(0.0, 100.0) as u8;
    let resets_at = ["resets_at", "resetsAt", "reset_at", "resetAt"]
        .iter()
        .find_map(|key| parse_ts(&v[*key]));
    Some(Window {
        label: label.to_string(),
        percent,
        resets_at,
    })
}

fn first_number(v: &Value, keys: &[&str]) -> Option<f64> {
    keys.iter().find_map(|key| {
        let n = &v[*key];
        n.as_f64()
            .or_else(|| n.as_str().and_then(|s| s.trim().parse::<f64>().ok()))
    })
}

/// A timestamp as any source writes it: Unix seconds (or milliseconds —
/// anything past the year 5000 is read as such), as a number or a numeric
/// string, or ISO-8601 `YYYY-MM-DDTHH:MM[:SS[.frac]][Z|±HH[:MM]]`. Unix
/// seconds out.
#[must_use]
pub fn parse_ts(v: &Value) -> Option<u64> {
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    fn epoch(n: f64) -> Option<u64> {
        if n.is_nan() || n <= 0.0 || !n.is_finite() {
            return None;
        }
        let n = if n > 1e11 { n / 1000.0 } else { n };
        Some(n as u64)
    }
    if let Some(n) = v.as_f64() {
        return epoch(n);
    }
    let s = v.as_str()?.trim();
    if let Ok(n) = s.parse::<f64>() {
        return epoch(n);
    }
    parse_iso8601(s)
}

fn parse_iso8601(s: &str) -> Option<u64> {
    let b = s.as_bytes();
    if b.len() < 16 || !matches!(b[10], b'T' | b't' | b' ') {
        return None;
    }
    let num = |from: usize, to: usize| -> Option<i64> {
        let part = s.get(from..to)?;
        if part.bytes().all(|c| c.is_ascii_digit()) {
            part.parse().ok()
        } else {
            None
        }
    };
    let year = num(0, 4)?;
    let month = num(5, 7)?;
    let day = num(8, 10)?;
    let hour = num(11, 13)?;
    let minute = num(14, 16)?;
    let mut i = 16;
    let second = if b.get(16) == Some(&b':') {
        i = 19;
        num(17, 19)?
    } else {
        0
    };
    if b.get(i) == Some(&b'.') {
        i += 1;
        while i < b.len() && b[i].is_ascii_digit() {
            i += 1;
        }
    }
    let offset = match s.get(i..)? {
        "" | "Z" | "z" => 0,
        rest => {
            let sign = match rest.as_bytes()[0] {
                b'+' => 1,
                b'-' => -1,
                _ => return None,
            };
            let hh = num(i + 1, i + 3)?;
            let mm = match rest.len() {
                3 => 0,
                5 => num(i + 3, i + 5)?,
                6 if rest.as_bytes()[3] == b':' => num(i + 4, i + 6)?,
                _ => return None,
            };
            sign * (hh * 3600 + mm * 60)
        }
    };
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) || hour > 23 || minute > 59 || second > 60 {
        return None;
    }
    let days = days_from_civil(year, month, day);
    let secs = days * 86400 + hour * 3600 + minute * 60 + second - offset;
    u64::try_from(secs).ok()
}

/// Days since 1970-01-01 of a proleptic Gregorian date (Howard Hinnant's
/// `days_from_civil`).
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// The reset line under a bar: `"resets in 2h 15m"`, `"resets in 3d 4h"`,
/// `"resets in 12m"`, `"resets now"`; empty when the source gave no time.
#[must_use]
pub fn reset_text(resets_at: Option<u64>, now: u64) -> String {
    let Some(at) = resets_at else {
        return String::new();
    };
    if at <= now {
        return "resets now".to_string();
    }
    let minutes = (at - now) / 60;
    if minutes < 1 {
        return "resets in <1m".to_string();
    }
    if minutes < 60 {
        return format!("resets in {minutes}m");
    }
    let hours = minutes / 60;
    if hours < 24 {
        return match minutes % 60 {
            0 => format!("resets in {hours}h"),
            m => format!("resets in {hours}h {m}m"),
        };
    }
    let days = hours / 24;
    match hours % 24 {
        0 => format!("resets in {days}d"),
        h => format!("resets in {days}d {h}h"),
    }
}

/// Unix seconds now.
#[must_use]
pub fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or_default()
}

fn clip(s: &str, max: usize) -> String {
    s.chars().take(max).collect()
}

/// A display label only, never a token or an inferred identity.
#[must_use]
pub fn account_label(value: Option<&str>) -> Option<String> {
    value.map(deck_model::sanitize::text)
        .map(|s| s.trim().chars().take(254).collect::<String>())
        .filter(|s| !s.is_empty())
}

/// `account/read` identifies the same app-server that supplies Codex's
/// rate limits. API-key accounts and signed-out servers have no email.
#[must_use]
pub fn codex_account(value: &Value) -> Option<String> {
    account_label(value["account"]["email"].as_str())
}

/// The latest windows per provider and the line last pushed from them.
#[derive(Debug, Default)]
pub struct Feeder {
    windows: BTreeMap<Provider, Vec<Window>>,
    accounts: BTreeMap<Provider, Option<String>>,
    resets: BTreeMap<Provider, Vec<ResetGrant>>,
    /// The provider whose report came last: the principal of the next push.
    latest: Option<Provider>,
    /// What the deck has, so a tick re-pushes only when the text moved.
    pushed: Option<(String, Vec<UsageItem>)>,
    rendered_at: Option<Instant>,
}

impl Feeder {
    /// Replace the windows and their identity together, including an
    /// unknown identity. Never relabel an old account's percentages.
    pub fn set(&mut self, provider: Provider, windows: Vec<Window>, account: Option<String>) -> bool {
        self.latest = Some(provider);
        let account = account_label(account.as_deref());
        if account.is_none() || self.accounts.get(&provider) != Some(&account) || windows.is_empty() {
            self.resets.remove(&provider);
        }
        let changed = self.windows.get(&provider) != Some(&windows)
            || self.accounts.get(&provider) != Some(&account);
        self.windows.insert(provider, windows);
        self.accounts.insert(provider, account);
        changed
    }

    pub fn set_resets(&mut self, provider: Provider, resets: Vec<ResetGrant>) {
        self.resets.insert(provider, resets);
    }

    /// Status-line reports contain only a subset of the account's meters.
    /// Preserve model limits only for the same known account and before reset.
    pub fn set_partial(&mut self, provider: Provider, mut windows: Vec<Window>, account: Option<String>, now: u64) -> bool {
        let account = account_label(account.as_deref());
        if account.is_some() && self.accounts.get(&provider) == Some(&account) {
            for old in self.windows.get(&provider).into_iter().flatten() {
                if !windows.iter().any(|w| w.label == old.label)
                    && old.resets_at.is_some_and(|reset| reset > now) {
                    windows.push(old.clone());
                }
            }
        }
        self.set(provider, windows, account)
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.windows.values().all(Vec::is_empty)
    }

    /// The provider of the last report, for the principal of a push.
    #[must_use]
    pub fn latest(&self) -> Option<Provider> {
        self.latest
    }

    /// The line for the deck: `[device] usage_period` as the tag, one item
    /// per window in provider order, countdowns relative to `now` (Unix
    /// seconds). `None` before any report; an empty list clears old usage.
    #[must_use]
    pub fn render(&self, cfg: &UsageCfg, period: &str, now: u64) -> Option<(String, Vec<UsageItem>)> {
        let mut items = Vec::new();
        for (index, provider) in Provider::ALL.into_iter().enumerate() {
            if !provider.enabled(cfg) {
                continue;
            }
            // Reserve the other provider's session/week meters instead of
            // allowing extra Claude model limits to crowd out Codex entirely.
            let reserve: usize = Provider::ALL[index + 1..].iter().filter(|p| p.enabled(cfg))
                .map(|p| self.windows.get(p).map_or(0, |w| w.len().min(2))).sum();
            for w in self.windows.get(&provider).into_iter().flatten() {
                if items.len() >= ITEMS_MAX.saturating_sub(reserve) {
                    break;
                }
                items.push(UsageItem {
                    name: clip(&format!("{} {}", provider.label(), w.label), NAME_MAX),
                    account: Some(self.accounts.get(&provider).and_then(Option::as_deref)
                        .unwrap_or("Account unavailable").to_string()),
                    percent: w.percent.min(100),
                    reset: clip(&reset_text(w.resets_at, now), RESET_MAX),
                    reset_tip: (w.label == "week").then(|| reset_tip(
                        self.resets.get(&provider).map_or(&[], Vec::as_slice), now)).flatten(),
                    tone: None,
                });
            }
        }
        if items.is_empty() && self.windows.is_empty() {
            return None;
        }
        let period = clip(period.trim(), PERIOD_MAX);
        let period = if period.is_empty() {
            "usage".to_string()
        } else {
            period
        };
        Some((period, items))
    }

    /// Whether `line` is what the deck already has.
    #[must_use]
    pub fn already_pushed(&self, line: &(String, Vec<UsageItem>)) -> bool {
        self.pushed.as_ref() == Some(line)
    }

    pub fn note_pushed(&mut self, line: (String, Vec<UsageItem>), now: Instant) {
        self.pushed = Some(line);
        self.rendered_at = Some(now);
    }

    /// A tick's question: is it time to re-render the countdowns?
    #[must_use]
    pub fn due(&mut self, now: Instant) -> bool {
        if self.is_empty() {
            return false;
        }
        let due = self
            .rendered_at
            .is_none_or(|at| now.saturating_duration_since(at).as_secs() >= RERENDER_S);
        if due {
            self.rendered_at = Some(now);
        }
        due
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn manual_reset_sources_ignore_spent_paused_and_unknown_credits() {
        let grant = json!({"resets_left": 2, "starts_at": 100, "ends_at": 200, "paused": false, "usable_now": false});
        let mut response = json!({"cedar_ember": {"eligible": true, "grants": [grant.clone(),
            {"resets_left": 0, "ends_at": 200}, {"resets_left": 1, "ends_at": 200, "paused": true},
            {"resets_left": 1, "ends_at": null}]}});
        assert_eq!(claude_reset_grants(&response), vec![ResetGrant { count: 2, starts_at: Some(100), expires_at: 200 }]);
        response["cedar_ember"]["eligible"] = false.into();
        assert!(claude_reset_grants(&response).is_empty());
        let credit = json!({"status": "available", "resetType": "codexRateLimits", "grantedAt": 100, "expiresAt": 200});
        let mut response = json!({"rateLimitResetCredits": {"availableCount": 1, "credits": [credit]}});
        assert_eq!(codex_reset_grants(&response), vec![ResetGrant { count: 1, starts_at: Some(100), expires_at: 200 }]);
        for status in ["redeemed", "redeeming", "unknown"] {
            response["rateLimitResetCredits"]["credits"][0]["status"] = status.into();
            assert!(codex_reset_grants(&response).is_empty());
        }
        assert!(codex_reset_grants(&json!({"rateLimitResetCredits": {"availableCount": 3, "credits": null}})).is_empty());
        assert!(claude_reset_grants(&Value::Null).is_empty());
        assert!(codex_reset_grants(&Value::Null).is_empty());
    }

    #[test]
    fn manual_reset_tips_use_the_nearest_expiry_with_a_72_hour_boundary() {
        let now = 1_800_000_000;
        let mut grant = ResetGrant { count: 1, starts_at: None, expires_at: now + 3 * 86400 + 1 };
        assert_eq!(reset_tip(&[grant.clone()], now), None);
        grant.expires_at -= 1;
        assert_eq!(reset_tip(&[grant.clone()], now), Some("1 reset expires in 3d 0h".into()));
        grant.expires_at = now + 60;
        let later = ResetGrant { count: 2, starts_at: None, expires_at: now + 86400 };
        assert_eq!(reset_tip(&[grant.clone(), later.clone()], now), Some("1 reset expires in 1m".into()));
        assert_eq!(reset_tip(&[grant.clone(), grant.clone()], now), Some("2 resets expire in 1m".into()));
        assert_eq!(reset_tip(&[grant.clone()], now + 1), Some("1 reset expires in <1m".into()));
        assert_eq!(reset_tip(&[grant.clone()], now + 60), None);
        assert_eq!(reset_tip(&[grant.clone()], now + 61), None);
        grant.starts_at = Some(now + 1);
        assert_eq!(reset_tip(&[grant], now), None);
        let many = ResetGrant { count: u64::MAX, ..later };
        let tip = reset_tip(&[many.clone(), many], now).unwrap();
        assert_eq!(tip, "99+ resets expire in 1d 0h");
        assert!(tip.len() <= deck_proto::caps().usage_tip);
    }

    #[test]
    fn manual_reset_tips_belong_only_to_the_weekly_bar_and_current_account() {
        let now = 1_800_000_000;
        let mut f = Feeder::default();
        let windows = vec![Window { label: "session".into(), percent: 10, resets_at: Some(now + 60) },
            Window { label: "week".into(), percent: 100, resets_at: Some(now + 7 * 86400) }];
        let account = Some("one@example.com".into());
        let grants = vec![ResetGrant { count: 1, starts_at: None, expires_at: now + 86400 }];
        let cfg = both();
        f.set(Provider::Claude, windows.clone(), account.clone());
        assert!(f.render(&cfg, "", now).unwrap().1.iter().all(|i| i.reset_tip.is_none()));
        f.set_resets(Provider::Claude, grants.clone());
        let items = f.render(&cfg, "", now).unwrap().1;
        assert_eq!(items[0].reset_tip, None);
        assert_eq!(items[1].reset_tip.as_deref(), Some("1 reset expires in 1d 0h"));
        f.set_partial(Provider::Claude, windows.clone(), account.clone(), now);
        assert!(f.render(&cfg, "", now).unwrap().1[1].reset_tip.is_some());
        f.set_resets(Provider::Claude, Vec::new());
        assert!(f.render(&cfg, "", now).unwrap().1[1].reset_tip.is_none());
        for identity in [Some("two@example.com".into()), None] {
            f.set(Provider::Claude, windows.clone(), account.clone());
            f.set_resets(Provider::Claude, grants.clone());
            f.set_partial(Provider::Claude, windows.clone(), identity, now);
            assert!(f.render(&cfg, "", now).unwrap().1.iter().all(|i| i.reset_tip.is_none()));
        }
    }

    #[test]
    fn accounts_change_with_the_reading_and_logout_clears_the_face() {
        let mut f = Feeder::default();
        let windows = vec![Window { label: "session".into(), percent: 17, resets_at: None }];
        f.set(Provider::Codex, windows.clone(), Some("personal@example.com".into()));
        let first = f.render(&UsageCfg::default(), "usage", 0).unwrap();
        assert_eq!(first.1[0].account.as_deref(), Some("personal@example.com"));
        f.note_pushed(first, Instant::now());
        assert!(f.set(Provider::Codex, windows.clone(), Some("work@example.com".into())));
        let next = f.render(&UsageCfg::default(), "usage", 0).unwrap();
        assert!(!f.already_pushed(&next), "an account switch redraws even at the same percentage");
        assert_eq!(next.1[0].account.as_deref(), Some("work@example.com"));
        f.set(Provider::Codex, windows, None);
        assert_eq!(f.render(&UsageCfg::default(), "usage", 0).unwrap().1[0].account.as_deref(), Some("Account unavailable"));
        f.set(Provider::Codex, Vec::new(), None);
        assert!(f.render(&UsageCfg::default(), "usage", 0).unwrap().1.is_empty());
    }

    #[test]
    fn codex_identity_is_only_the_reported_email() {
        assert_eq!(codex_account(&json!({"account": {"type": "chatgpt", "email": " person@example.com "}})), Some("person@example.com".into()));
        for account in [Value::Null, json!({"type": "apiKey"}), json!({"type": "chatgpt", "email": null})] {
            assert_eq!(codex_account(&json!({"account": account})), None);
        }
        assert_eq!(account_label(Some(" \n\t ")), None);
    }

    fn cfg(providers: &[&str]) -> UsageCfg {
        UsageCfg {
            providers: providers.iter().map(|p| (*p).to_string()).collect(),
            ..UsageCfg::default()
        }
    }

    /// The face with the Claude bars switched on beside Codex's: they are
    /// the owner's to opt into, so the default draws Codex alone.
    fn both() -> UsageCfg {
        cfg(&["claude", "codex"])
    }

    #[test]
    fn iso_8601_and_epoch_timestamps_parse() {
        assert_eq!(parse_ts(&json!("1970-01-02T00:00:00Z")), Some(86_400));
        assert_eq!(parse_ts(&json!("2026-10-10T15:00:00.000000+00:00")), Some(1_791_644_400));
        assert_eq!(parse_ts(&json!("2026-10-10T17:00:00+02:00")), Some(1_791_644_400));
        assert_eq!(parse_ts(&json!("2026-10-10T13:00-0200")), Some(1_791_644_400));
        assert_eq!(parse_ts(&json!(1_791_644_400)), Some(1_791_644_400));
        assert_eq!(parse_ts(&json!(1_791_644_400_000_i64)), Some(1_791_644_400));
        assert_eq!(parse_ts(&json!("1791644400")), Some(1_791_644_400));
        for bad in [json!(null), json!(0), json!(""), json!("soon"), json!("2026-13-01T00:00Z")] {
            assert_eq!(parse_ts(&bad), None, "{bad}");
        }
    }

    #[test]
    fn countdowns_read_to_the_minute() {
        let now = 1_000_000;
        assert_eq!(reset_text(None, now), "");
        assert_eq!(reset_text(Some(now - 5), now), "resets now");
        assert_eq!(reset_text(Some(now + 30), now), "resets in <1m");
        assert_eq!(reset_text(Some(now + 12 * 60), now), "resets in 12m");
        assert_eq!(reset_text(Some(now + 2 * 3600 + 15 * 60), now), "resets in 2h 15m");
        assert_eq!(reset_text(Some(now + 3 * 3600), now), "resets in 3h");
        assert_eq!(reset_text(Some(now + 3 * 86_400 + 4 * 3600 + 59 * 60), now), "resets in 3d 4h");
        assert_eq!(reset_text(Some(now + 7 * 86_400), now), "resets in 7d");
        assert!(reset_text(Some(now + 400 * 86_400), now).len() <= RESET_MAX);
    }

    #[test]
    fn the_oauth_usage_answer_is_read_in_both_shapes() {
        let flat = json!({
            "five_hour": {"utilization": 12.4, "resets_at": "2026-10-10T15:00:00.000000+00:00"},
            "seven_day": {"utilization": 30.6, "resets_at": "2026-10-14T03:00:00+00:00"},
            "seven_day_opus": null,
            "seven_day_sonnet": {"utilization": 0, "resets_at": null},
            "extra_usage": {"is_enabled": false, "utilization": 0}
        });
        let w = claude_oauth_windows(&flat);
        assert_eq!(w.len(), 3, "{w:?}");
        assert_eq!(w[0], Window { label: "session".into(), percent: 12, resets_at: Some(1_791_644_400) });
        assert_eq!(w[1].label, "week");
        assert_eq!(w[1].percent, 31);
        assert_eq!(w[2], Window { label: "Sonnet week".into(), percent: 0, resets_at: None });
        let listed = json!({
            "five_hour": null,
            "seven_day": null,
            "limits": [
                {"kind": "five_hour", "percent": 99.6, "resets_at": 1_791_644_400},
                {"kind": "seven_day", "percent": 140, "resets_at": 1_791_900_000},
                {"kind": "monthly_credits", "percent": 5}
            ],
            "extra_usage": {"is_enabled": true, "utilization": 7.2}
        });
        let w = claude_oauth_windows(&listed);
        assert_eq!(
            w.iter().map(|w| (w.label.as_str(), w.percent)).collect::<Vec<_>>(),
            vec![("session", 100), ("week", 100), ("monthly credits", 5), ("extra", 7)]
        );
        assert!(claude_oauth_windows(&json!({"error": "unauthorized"})).is_empty());
    }

    #[test]
    fn the_status_line_and_the_app_server_are_read() {
        let status = json!({
            "session_id": "s1",
            "rate_limits": {
                "five_hour": {"used_percentage": 42, "resets_at": 1_791_644_400},
                "seven_day": {"used_percentage": 8.5, "resets_at": 1_791_900_000}
            }
        });
        let w = claude_statusline_windows(&status);
        assert_eq!(w.len(), 2);
        assert_eq!((w[0].label.as_str(), w[0].percent, w[0].resets_at), ("session", 42, Some(1_791_644_400)));
        assert_eq!((w[1].label.as_str(), w[1].percent), ("week", 9));
        assert!(claude_statusline_windows(&json!({"session_id": "s1"})).is_empty());

        let codex = json!({
            "rateLimits": {
                "limitId": "codex",
                "primary": {"usedPercent": 17, "windowDurationMins": 300, "resetsAt": 1_791_644_400},
                "secondary": {"usedPercent": 63.5, "windowDurationMins": 10080, "resetsAt": "1791900000"}
            }
        });
        let w = codex_windows(&codex);
        assert_eq!(w.len(), 2);
        assert_eq!(w[0], Window { label: "session".into(), percent: 17, resets_at: Some(1_791_644_400) });
        assert_eq!(w[1], Window { label: "week".into(), percent: 64, resets_at: Some(1_791_900_000) });
        // The notification's params carry the same object without the
        // wrapper, and the v1 protocol spells it in snake_case.
        let bare = codex_windows(&json!({
            "primary": {"used_percent": 50, "window_minutes": 60, "resets_at": 1},
            "secondary": {"used_percent": 1, "window_minutes": 2880}
        }));
        assert_eq!(bare.iter().map(|w| w.label.as_str()).collect::<Vec<_>>(), vec!["1h", "2d"]);
        assert!(codex_windows(&json!({"rateLimits": null})).is_empty());
    }

    #[test]
    fn fable_is_read_alongside_flat_meters_and_reserves_codex_room() {
        let answer = json!({
            "five_hour":{"utilization":40}, "seven_day":{"utilization":52},
            "seven_day_opus":{"utilization":10}, "seven_day_sonnet":{"utilization":20},
            "limits":[
                {"kind":"session","percent":41}, {"kind":"weekly_all","percent":53},
                {"kind":"weekly_scoped","group":"weekly","percent":100,"resets_at":2_000_000_000,
                    "scope":{"model":{"display_name":"Fable"}}},
                {"kind":"weekly_scoped","percent":88,"scope":{"surface":{"display_name":"Other"}}}
            ], "extra_usage":{"is_enabled":true,"utilization":7}
        });
        let windows = claude_oauth_windows(&answer);
        assert_eq!(windows.iter().map(|w| w.label.as_str()).collect::<Vec<_>>(),
            ["session", "week", "Fable week", "Opus week", "Sonnet week", "extra"]);
        assert_eq!((windows[0].percent, windows[1].percent, windows[2].percent), (41,53,100));
        let mut f = Feeder::default();
        f.set(Provider::Claude, windows, Some("claude@example.com".into()));
        f.set(Provider::Codex, vec![Window { label:"session".into(),percent:10,resets_at:None },
            Window { label:"week".into(),percent:20,resets_at:None }], Some("codex@example.com".into()));
        let items = f.render(&both(), "usage", 1_999_996_400).unwrap().1;
        assert_eq!(items.len(),6);
        assert_eq!(items[2].name,"Claude Fable week");
        assert_eq!(items[2].account.as_deref(),Some("claude@example.com"));
        assert_eq!(items[2].reset,"resets in 1h");
        assert_eq!(items[4].name,"Codex session");
        assert_eq!(items[5].name,"Codex week");
    }

    #[test]
    fn partial_usage_preserves_fable_only_for_the_same_account_until_reset() {
        let mut f = Feeder::default();
        let fable = Window { label:"Fable week".into(), percent:75, resets_at:Some(1000) };
        let session = Window { label:"session".into(), percent:10, resets_at:Some(2000) };
        let account = Some("work@example.com".into());
        f.set(Provider::Claude, vec![fable.clone()], account.clone());
        f.set_partial(Provider::Claude, vec![session.clone()], account.clone(), 100);
        assert_eq!(f.windows[&Provider::Claude], [session.clone(), fable.clone()]);
        f.set_partial(Provider::Claude, vec![session.clone()], account.clone(), 1001);
        assert_eq!(f.windows[&Provider::Claude].as_slice(), std::slice::from_ref(&session));
        for switched in [None, Some("personal@example.com".into())] {
            f.set(Provider::Claude, vec![fable.clone()], account.clone());
            f.set_partial(Provider::Claude, vec![session.clone()], switched, 100);
            assert_eq!(f.windows[&Provider::Claude].as_slice(), std::slice::from_ref(&session));
        }
        f.set(Provider::Claude, vec![fable], account.clone());
        f.set(Provider::Claude, vec![session.clone()], account);
        assert_eq!(f.windows[&Provider::Claude], [session]);
    }

    #[test]
    fn providers_are_named_in_the_config() {
        assert!(Provider::Claude.enabled(&cfg(&["claude", "codex"])));
        assert!(Provider::Codex.enabled(&cfg(&["claude", "codex"])));
        assert!(Provider::Claude.enabled(&cfg(&["claude-statusline"])));
        assert!(!Provider::Codex.enabled(&cfg(&["claude-statusline"])));
        assert!(!Provider::Claude.enabled(&cfg(&[])));
        // D3: the Claude bars are opt-in, Codex's are not.
        assert!(!Provider::Claude.enabled(&UsageCfg::default()));
        assert!(Provider::Codex.enabled(&UsageCfg::default()));
    }

    #[test]
    fn the_feeder_renders_providers_in_order_and_dedupes_pushes() {
        let mut f = Feeder::default();
        assert!(f.is_empty());
        assert!(f.render(&both(), "5h / 7d", 0).is_none());
        let now = 1_791_640_000;
        assert!(f.set(
            Provider::Codex,
            vec![Window { label: "session".into(), percent: 17, resets_at: Some(now + 3600) }], None
        ));
        assert!(f.set(
            Provider::Claude,
            vec![
                Window { label: "session".into(), percent: 12, resets_at: Some(now + 90) },
                Window { label: "week".into(), percent: 31, resets_at: Some(now + 2 * 86_400) },
            ], None
        ));
        // The same report again is not a change.
        assert!(!f.set(
            Provider::Codex,
            vec![Window { label: "session".into(), percent: 17, resets_at: Some(now + 3600) }], None
        ));
        let (period, items) = f.render(&both(), "5h / 7d", now).unwrap();
        assert_eq!(period, "5h / 7d");
        assert_eq!(
            items.iter().map(|i| (i.name.as_str(), i.percent, i.reset.as_str())).collect::<Vec<_>>(),
            vec![
                ("Claude session", 12, "resets in 1m"),
                ("Claude week", 31, "resets in 2d"),
                ("Codex session", 17, "resets in 1h"),
            ]
        );
        assert!(items.iter().all(|i| i.tone.is_none()));
        // A provider taken out of the config is not drawn…
        let (_, only) = f.render(&cfg(&["codex"]), "", now).unwrap();
        assert_eq!(only.len(), 1);
        assert_eq!(only[0].name, "Codex session");
        // …which is what the default does with Claude's windows, however
        // they arrived (D3: the Claude bars are opt-in).
        assert_eq!(
            f.render(&UsageCfg::default(), "", now),
            f.render(&cfg(&["codex"]), "", now)
        );
        // An empty tag still gives the face a word.
        assert_eq!(f.render(&cfg(&["codex"]), "  ", now).unwrap().0, "usage");
        // Pushes dedupe on the rendered line, and the countdown moves it.
        let line = f.render(&both(), "5h / 7d", now).unwrap();
        assert!(!f.already_pushed(&line));
        let t0 = Instant::now();
        f.note_pushed(line.clone(), t0);
        assert!(f.already_pushed(&line));
        let later = f.render(&both(), "5h / 7d", now + 120).unwrap();
        assert!(!f.already_pushed(&later));
        assert_eq!(later.1[0].reset, "resets now");
        // The tick re-renders once a minute, never before.
        assert!(!f.due(t0 + std::time::Duration::from_secs(30)));
        assert!(f.due(t0 + std::time::Duration::from_secs(61)));
        assert!(!f.due(t0 + std::time::Duration::from_secs(90)));
    }

    #[test]
    fn names_and_resets_fit_the_wire() {
        let mut f = Feeder::default();
        f.set(
            Provider::Claude,
            (0..8)
                .map(|i| Window {
                    label: format!("a very long window label {i}"),
                    percent: 100,
                    resets_at: Some(u64::MAX / 2),
                })
                .collect(),
            None,
        );
        let (period, items) = f.render(&both(), "a period tag that is long", 0).unwrap();
        assert_eq!(items.len(), ITEMS_MAX);
        assert_eq!(period.chars().count(), PERIOD_MAX);
        for item in &items {
            assert!(item.name.chars().count() <= NAME_MAX, "{}", item.name);
            assert!(item.reset.chars().count() <= RESET_MAX, "{}", item.reset);
            assert!(item.percent <= 100);
        }
    }
}
