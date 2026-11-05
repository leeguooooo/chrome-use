//! `session handoff --wait`: hand the session to the user and block until they
//! are done, then report what changed.
//!
//! A plain `session handoff` only flips the `.owner` sidecar. The agent then
//! ends its turn, the user finishes on the page, and nothing tells the agent:
//! it learns only when the user comes back to the terminal, and its refs are
//! stale by then. `--wait` keeps the command running until one of:
//!
//! - an `--until-url` / `--until-text` / `--until-gone` condition the agent
//!   declared up front holds (auto-resume: the agent said what "done" means);
//! - the user runs `chrome-use session done` (auto-resume);
//! - someone runs `session resume` elsewhere (already resumed);
//! - `--until-change`: the address or title moved off the baseline and stayed
//!   put for the settle period (the session stays handed off — a page change
//!   is not proof the user finished, e.g. password page → 2FA page);
//! - the timeout (the session stays handed off).
//!
//! Run it in the background (Claude Code `run_in_background`) and the agent is
//! woken the moment it returns. Polling reads the address bar through the
//! extension and, for text/selector conditions, one isolated-world check per
//! tick — nothing the page can see.

use crate::connection::{get_socket_dir, send_command};
use crate::ownership::{self, Owner};
use serde_json::{json, Value};
use std::path::PathBuf;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// What the agent declared as "the user is done".
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Until {
    Url(String),
    Text(String),
    Gone(String),
    Change,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WaitSpec {
    pub until: Option<Until>,
    pub timeout: Duration,
    pub poll: Duration,
    pub settle: Duration,
}

pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30 * 60);
const DEFAULT_POLL: Duration = Duration::from_millis(1500);
const DEFAULT_SETTLE: Duration = Duration::from_secs(3);
/// Consecutive failed polls before the wait gives up on the daemon.
const MAX_POLL_FAILURES: u32 = 20;

/// Parse the flags after `session handoff`. `Ok(None)` means no `--wait`
/// (plain handoff); `--until-*` implies `--wait`.
pub fn parse_args(args: &[String]) -> Result<Option<WaitSpec>, String> {
    let mut wait = false;
    let mut until: Option<Until> = None;
    let mut timeout = DEFAULT_TIMEOUT;
    let mut poll = DEFAULT_POLL;
    let mut settle = DEFAULT_SETTLE;
    let set_until = |u: Until, until: &mut Option<Until>| -> Result<(), String> {
        if until.is_some() {
            return Err("give only one --until-* condition".to_string());
        }
        *until = Some(u);
        Ok(())
    };
    let mut i = 0;
    while i < args.len() {
        let a = args[i].as_str();
        let value = |i: usize| -> Result<String, String> {
            args.get(i + 1)
                .filter(|v| !v.is_empty() && !v.starts_with("--"))
                .cloned()
                .ok_or_else(|| format!("{a} needs a value"))
        };
        match a {
            "--wait" => wait = true,
            "--until-url" => {
                set_until(Until::Url(value(i)?), &mut until)?;
                i += 1;
            }
            "--until-text" => {
                set_until(Until::Text(value(i)?), &mut until)?;
                i += 1;
            }
            "--until-gone" => {
                set_until(Until::Gone(value(i)?), &mut until)?;
                i += 1;
            }
            "--until-change" => set_until(Until::Change, &mut until)?,
            "--timeout" => {
                timeout = parse_duration(&value(i)?)?;
                i += 1;
            }
            "--poll" => {
                poll = parse_duration(&value(i)?)?.max(Duration::from_millis(250));
                i += 1;
            }
            "--settle" => {
                settle = parse_duration(&value(i)?)?;
                i += 1;
            }
            other => return Err(format!("unknown option for session handoff: {other}")),
        }
        i += 1;
    }
    if !wait && until.is_none() {
        if timeout != DEFAULT_TIMEOUT || poll != DEFAULT_POLL || settle != DEFAULT_SETTLE {
            return Err("--timeout/--poll/--settle only apply with --wait".to_string());
        }
        return Ok(None);
    }
    Ok(Some(WaitSpec {
        until,
        timeout,
        poll,
        settle,
    }))
}

/// `500ms`, `90s`, `30m`, `1h`, or bare milliseconds.
pub fn parse_duration(s: &str) -> Result<Duration, String> {
    let s = s.trim();
    let (num, mul) = if let Some(n) = s.strip_suffix("ms") {
        (n, 1u64)
    } else if let Some(n) = s.strip_suffix('s') {
        (n, 1000)
    } else if let Some(n) = s.strip_suffix('m') {
        (n, 60_000)
    } else if let Some(n) = s.strip_suffix('h') {
        (n, 3_600_000)
    } else {
        (s, 1)
    };
    num.trim()
        .parse::<u64>()
        .map(|n| Duration::from_millis(n.saturating_mul(mul)))
        .map_err(|_| format!("bad duration '{s}' (use e.g. 500ms, 90s, 30m)"))
}

/// Same rule as `wait --url`: a pattern with `*`/`?` is a Playwright-style
/// glob over the whole URL, anything else a substring.
pub fn url_matches(url: &str, pattern: &str) -> bool {
    if pattern.contains('*') || pattern.contains('?') {
        let mut re = String::from("^");
        let mut chars = pattern.chars().peekable();
        while let Some(c) = chars.next() {
            match c {
                '*' if chars.peek() == Some(&'*') => {
                    chars.next();
                    re.push_str(".*");
                }
                '*' => re.push_str("[^/]*"),
                '?' => re.push('.'),
                '.' | '+' | '(' | ')' | '|' | '[' | ']' | '{' | '}' | '^' | '$' | '\\' => {
                    re.push('\\');
                    re.push(c);
                }
                _ => re.push(c),
            }
        }
        re.push('$');
        regex_lite::Regex::new(&re).is_ok_and(|r| r.is_match(url))
    } else {
        url.contains(pattern)
    }
}

fn baseline_path(session: &str) -> PathBuf {
    get_socket_dir().join(format!("{session}.handoff.json"))
}

pub fn done_path(session: &str) -> PathBuf {
    get_socket_dir().join(format!("{session}.handoff-done"))
}

/// `session done`: the user says they're finished. Picked up by a running
/// `handoff --wait`; harmless if nothing is waiting.
pub fn mark_done(session: &str) -> Result<(), String> {
    let dir = get_socket_dir();
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    std::fs::write(done_path(session), now_ms().to_string()).map_err(|e| e.to_string())
}

/// Forget the handoff's baseline and done marker (on resume).
pub fn clear_sidecars(session: &str) {
    let _ = std::fs::remove_file(baseline_path(session));
    let _ = std::fs::remove_file(done_path(session));
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct PageState {
    url: String,
    title: String,
}

fn read_page(session: &str) -> Result<PageState, String> {
    let url = data_str(
        send_command(json!({"id": "hw-url", "action": "url"}), session)?,
        "url",
    )?;
    let title = data_str(
        send_command(json!({"id": "hw-title", "action": "title"}), session)?,
        "title",
    )
    .unwrap_or_default();
    Ok(PageState { url, title })
}

fn data_str(resp: crate::connection::Response, key: &str) -> Result<String, String> {
    if !resp.success {
        return Err(resp.error.unwrap_or_else(|| "command failed".to_string()));
    }
    Ok(resp
        .data
        .as_ref()
        .and_then(|d| d.get(key))
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string())
}

/// One quick isolated-world check of a text / selector condition. `Ok(false)`
/// is "not yet"; `Err` is a real failure.
fn check_page_condition(session: &str, until: &Until) -> Result<bool, String> {
    let cmd = match until {
        Until::Text(t) => json!({"id": "hw-cond", "action": "wait", "text": t, "timeout": 50}),
        Until::Gone(sel) => json!({
            "id": "hw-cond", "action": "wait", "selector": sel,
            "state": "detached", "timeout": 50,
        }),
        _ => return Ok(false),
    };
    let resp = send_command(cmd, session)?;
    if resp.success {
        return Ok(true);
    }
    let err = resp.error.unwrap_or_default();
    if err.contains("timed out") || err.contains("Timeout") {
        Ok(false)
    } else {
        Err(err)
    }
}

/// How a wait ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// The declared `--until-*` condition held. Resumed.
    Condition,
    /// The user ran `session done`. Resumed.
    UserDone,
    /// `session resume` ran elsewhere while waiting.
    ResumedElsewhere,
    /// `--until-change` fired. Still handed off.
    Changed,
    /// Timed out. Still handed off.
    Timeout,
}

impl Outcome {
    pub fn as_str(&self) -> &'static str {
        match self {
            Outcome::Condition => "condition",
            Outcome::UserDone => "user_done",
            Outcome::ResumedElsewhere => "resumed_elsewhere",
            Outcome::Changed => "changed",
            Outcome::Timeout => "timeout",
        }
    }

    /// Whether the agent owns the session again when the wait returns.
    pub fn resumes(&self) -> bool {
        matches!(
            self,
            Outcome::Condition | Outcome::UserDone | Outcome::ResumedElsewhere
        )
    }
}

/// Watches `--until-change`: fires once the page has moved off the baseline
/// and then held still for `settle`.
struct ChangeWatch {
    baseline: PageState,
    last: Option<PageState>,
    stable_since: Option<Instant>,
}

impl ChangeWatch {
    fn new(baseline: PageState) -> Self {
        ChangeWatch {
            baseline,
            last: None,
            stable_since: None,
        }
    }

    fn observe(&mut self, now: Instant, page: &PageState, settle: Duration) -> bool {
        if *page == self.baseline {
            self.last = None;
            self.stable_since = None;
            return false;
        }
        if self.last.as_ref() != Some(page) {
            self.last = Some(page.clone());
            self.stable_since = Some(now);
        }
        self.stable_since
            .is_some_and(|t| now.duration_since(t) >= settle)
    }
}

/// Small deterministic jitter so the poll has no fixed beat.
fn jittered(poll: Duration, tick: u64) -> Duration {
    let ms = poll.as_millis() as u64;
    let spread = ms / 5;
    if spread == 0 {
        return poll;
    }
    let seed = now_ms() ^ tick.wrapping_mul(0x9E37_79B9_7F4A_7C15);
    Duration::from_millis(ms - spread + seed % (2 * spread + 1))
}

/// Run the whole `handoff --wait`. Returns the JSON result (`ok` false only
/// on a hard failure before the handoff took effect).
pub fn run(session: &str, spec: &WaitSpec) -> Result<Value, String> {
    if !crate::connection::daemon_ready(session) {
        return Err(format!(
            "no running browser session '{session}' to watch — open the page first"
        ));
    }
    // Reads below must pass the ownership guard while the user owns the page.
    ownership::set_watching(true);

    // Baseline: reuse the one from an earlier `--wait` of this same handoff,
    // so re-waiting after a timeout still reports the change from the start.
    let already_handed_off = ownership::owner_of(session) == Owner::User;
    let stored: Option<PageState> = if already_handed_off {
        std::fs::read_to_string(baseline_path(session))
            .ok()
            .and_then(|s| serde_json::from_str::<Value>(&s).ok())
            .map(|v| PageState {
                url: v["url"].as_str().unwrap_or_default().to_string(),
                title: v["title"].as_str().unwrap_or_default().to_string(),
            })
    } else {
        None
    };
    let baseline = match stored {
        Some(b) => b,
        None => {
            let b = read_page(session)?;
            // Prime the daemon's snapshot baseline so the report can diff.
            let _ = send_command(
                json!({"id": "hw-snap0", "action": "snapshot", "interactive": true}),
                session,
            );
            let _ = std::fs::write(
                baseline_path(session),
                json!({"url": b.url, "title": b.title, "since": now_ms()}).to_string(),
            );
            b
        }
    };
    if !already_handed_off {
        let _ = std::fs::remove_file(done_path(session));
        ownership::hand_off(session)?;
    }

    let start = Instant::now();
    let mut change = ChangeWatch::new(baseline.clone());
    let mut failures = 0u32;
    let mut last_error: Option<String> = None;
    let mut tick = 0u64;
    let outcome = loop {
        if done_path(session).exists() {
            break Outcome::UserDone;
        }
        if ownership::owner_of(session) != Owner::User {
            break Outcome::ResumedElsewhere;
        }
        if start.elapsed() >= spec.timeout {
            break Outcome::Timeout;
        }
        let polled: Result<bool, String> = match &spec.until {
            None => Ok(false),
            Some(Until::Url(p)) => read_page(session).map(|pg| url_matches(&pg.url, p)),
            Some(Until::Change) => {
                read_page(session).map(|pg| change.observe(Instant::now(), &pg, spec.settle))
            }
            Some(u) => check_page_condition(session, u),
        };
        match polled {
            Ok(true) => {
                break if spec.until == Some(Until::Change) {
                    Outcome::Changed
                } else {
                    Outcome::Condition
                }
            }
            Ok(false) => failures = 0,
            Err(e) => {
                failures += 1;
                last_error = Some(e);
                if failures >= MAX_POLL_FAILURES {
                    ownership::set_watching(false);
                    return Err(format!(
                        "lost the browser session while waiting ({}); the session stays handed off",
                        last_error.unwrap_or_default()
                    ));
                }
            }
        }
        tick += 1;
        std::thread::sleep(jittered(spec.poll, tick));
    };

    if outcome.resumes() {
        ownership::resume(session)?;
        clear_sidecars(session);
    }
    let report = change_report(session, &baseline);
    ownership::set_watching(false);

    let mut out = json!({
        "ok": true,
        "session": session,
        "outcome": outcome.as_str(),
        "owner": if outcome.resumes() { "agent" } else { "user" },
        "waitedMs": start.elapsed().as_millis() as u64,
        "changed": report,
    });
    if let Some(e) = last_error {
        out["lastPollError"] = json!(e);
    }
    Ok(out)
}

/// URL/title before → after plus a fresh interactive snapshot (a diff when the
/// page did not navigate). Best effort: a failed read leaves fields out.
fn change_report(session: &str, baseline: &PageState) -> Value {
    let now = read_page(session).ok();
    let mut out = json!({
        "urlBefore": baseline.url,
        "titleBefore": baseline.title,
    });
    if let Some(now) = &now {
        out["url"] = json!(now.url);
        out["title"] = json!(now.title);
        out["navigated"] = json!(now.url != baseline.url);
    }
    if let Ok(resp) = send_command(
        json!({"id": "hw-snap1", "action": "snapshot", "interactive": true, "diff": true}),
        session,
    ) {
        if let Some(d) = resp.data.filter(|_| resp.success) {
            out["snapshot"] = d.get("snapshot").cloned().unwrap_or(Value::Null);
            out["snapshotIsDiff"] = d.get("diffMode").cloned().unwrap_or(json!(false));
            if let Some(n) = d.get("diffNote") {
                out["snapshotNote"] = n.clone();
            }
        }
    }
    out
}

/// Human-readable rendering of [`run`]'s result.
pub fn print_human(v: &Value, session: &str, zh: bool) {
    let outcome = v["outcome"].as_str().unwrap_or("");
    let suffix = ownership::session_flag_suffix(session);
    let headline = match (outcome, zh) {
        ("condition", true) => "✓ 完成条件已满足，agent 已接管会话。".to_string(),
        ("condition", false) => "✓ Done condition met — the agent has the session back.".to_string(),
        ("user_done", true) => "✓ 用户已标记完成，agent 已接管会话。".to_string(),
        ("user_done", false) => "✓ The user marked the step done — the agent has the session back.".to_string(),
        ("resumed_elsewhere", true) => "✓ 会话已在别处被接管。".to_string(),
        ("resumed_elsewhere", false) => "✓ The session was resumed elsewhere.".to_string(),
        ("changed", true) => format!(
            "↻ 页面变了，但用户可能还没做完——会话仍归用户。继续等：chrome-use session handoff --wait{suffix}；确认做完再 resume。"
        ),
        ("changed", false) => format!(
            "↻ The page changed, but the user may not be finished — the session is still theirs. Keep waiting: chrome-use session handoff --wait{suffix}; resume only once they're done."
        ),
        (_, true) => format!(
            "⏱ 等待超时，会话仍归用户。继续等：chrome-use session handoff --wait{suffix}"
        ),
        (_, false) => format!(
            "⏱ Timed out — the session is still the user's. Keep waiting: chrome-use session handoff --wait{suffix}"
        ),
    };
    println!("{headline}");
    let c = &v["changed"];
    let before = c["urlBefore"].as_str().unwrap_or("");
    match c["url"].as_str() {
        Some(after) if after != before => println!("  url: {before}\n    → {after}"),
        Some(after) => println!("  url: {after} (unchanged)"),
        None => {}
    }
    let tb = c["titleBefore"].as_str().unwrap_or("");
    if let Some(ta) = c["title"].as_str() {
        if ta != tb {
            println!("  title: {tb}\n    → {ta}");
        }
    }
    if let Some(note) = c["snapshotNote"].as_str() {
        println!("  snapshot ({note}):");
    } else if c["snapshotIsDiff"].as_bool() == Some(true) {
        println!("  snapshot diff:");
    }
    if let Some(s) = c["snapshot"].as_str() {
        println!("{s}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn plain_handoff_has_no_wait() {
        assert_eq!(parse_args(&[]).unwrap(), None);
        assert!(parse_args(&args(&["--timeout", "5m"])).is_err());
    }

    #[test]
    fn until_implies_wait_and_only_one_is_allowed() {
        let s = parse_args(&args(&["--until-url", "**/home"]))
            .unwrap()
            .unwrap();
        assert_eq!(s.until, Some(Until::Url("**/home".into())));
        assert_eq!(s.timeout, DEFAULT_TIMEOUT);
        assert!(parse_args(&args(&["--until-url", "a", "--until-text", "b"])).is_err());
        assert!(parse_args(&args(&["--until-text"])).is_err());
        assert!(parse_args(&args(&["--until-gone", "--wait"])).is_err());
        assert!(parse_args(&args(&["--bogus"])).is_err());
    }

    #[test]
    fn wait_options_parse() {
        let s = parse_args(&args(&[
            "--wait",
            "--until-change",
            "--timeout",
            "90s",
            "--poll",
            "100",
            "--settle",
            "2s",
        ]))
        .unwrap()
        .unwrap();
        assert_eq!(s.until, Some(Until::Change));
        assert_eq!(s.timeout, Duration::from_secs(90));
        assert_eq!(s.poll, Duration::from_millis(250), "poll is floored");
        assert_eq!(s.settle, Duration::from_secs(2));
    }

    #[test]
    fn durations() {
        assert_eq!(parse_duration("500ms").unwrap(), Duration::from_millis(500));
        assert_eq!(parse_duration("30m").unwrap(), Duration::from_secs(1800));
        assert_eq!(parse_duration("1h").unwrap(), Duration::from_secs(3600));
        assert_eq!(parse_duration("2000").unwrap(), Duration::from_secs(2));
        assert!(parse_duration("soon").is_err());
    }

    #[test]
    fn url_matching_follows_wait_url() {
        assert!(url_matches("https://x.test/dashboard", "/dashboard"));
        assert!(url_matches("https://x.test/app/dashboard", "**/dashboard"));
        assert!(!url_matches(
            "https://x.test/dashboard/settings",
            "**/dashboard"
        ));
        assert!(url_matches("https://x.test/a", "https://x.test/?"));
    }

    #[test]
    fn change_watch_waits_for_a_settled_change() {
        let base = PageState {
            url: "https://x/login".into(),
            title: "Login".into(),
        };
        let mut w = ChangeWatch::new(base.clone());
        let t0 = Instant::now();
        let settle = Duration::from_secs(3);
        assert!(!w.observe(t0, &base, settle));
        let p2 = PageState {
            url: "https://x/2fa".into(),
            title: "2FA".into(),
        };
        assert!(!w.observe(t0 + Duration::from_secs(1), &p2, settle));
        let p3 = PageState {
            url: "https://x/home".into(),
            title: "Home".into(),
        };
        assert!(
            !w.observe(t0 + Duration::from_secs(3), &p3, settle),
            "moved again: restart settle"
        );
        assert!(!w.observe(t0 + Duration::from_secs(5), &p3, settle));
        assert!(w.observe(t0 + Duration::from_secs(6), &p3, settle));
        // Back to the baseline cancels it.
        let mut w = ChangeWatch::new(base.clone());
        assert!(!w.observe(t0, &p2, settle));
        assert!(!w.observe(t0 + Duration::from_secs(4), &base, settle));
        assert!(!w.observe(t0 + Duration::from_secs(8), &base, settle));
    }

    #[test]
    fn only_condition_and_done_resume() {
        assert!(Outcome::Condition.resumes());
        assert!(Outcome::UserDone.resumes());
        assert!(Outcome::ResumedElsewhere.resumes());
        assert!(!Outcome::Changed.resumes());
        assert!(!Outcome::Timeout.resumes());
    }

    #[test]
    fn jitter_stays_within_twenty_percent() {
        for t in 0..50 {
            let d = jittered(Duration::from_millis(1000), t).as_millis();
            assert!((800..=1200).contains(&d), "{d}");
        }
    }
}
