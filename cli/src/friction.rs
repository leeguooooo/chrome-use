//! Friction auto-reporting.
//!
//! When a chrome-use command genuinely fails, we append one structured line to a
//! local JSONL log. Aggregating it (`chrome-use friction`) turns "what's painful
//! to drive" from LLM guesswork into real usage data — so the next round of
//! features is evidence-driven.
//!
//! Privacy: LOCAL ONLY (no upload — matches the tool's no-tracking ethos), and we
//! record the page **host**, never the full URL/query/params. Opt out entirely
//! with `AGENT_BROWSER_NO_FRICTION_LOG=1`.

use std::io::Write;
use std::path::PathBuf;

use serde_json::{json, Value};

/// `~/.chrome-use/friction.jsonl` (sibling of the relay/state files).
pub fn friction_path() -> PathBuf {
    crate::connection::config_home().join("friction.jsonl")
}

/// Soft cap: once the log exceeds this many lines, the next aggregation/clear can
/// trim it. Bounds disk without per-write cost.
const MAX_LINES: usize = 4000;

/// Coarse, stable category for an error message — the axis aggregation groups by.
/// Kept small + substring-matched so similar failures bucket together.
pub fn categorize(error: &str) -> &'static str {
    let l = error.to_lowercase();
    // Friction-only buckets first; then the same taxonomy `--json` reports as
    // `code`, so the two never disagree; the older buckets below only split
    // what that taxonomy calls `command_failed`.
    if l.contains("not in the allowed domains") || l.contains("no pending confirmation") {
        return "policy";
    }
    if l.contains("failed to fetch") {
        return "page_fetch_failed";
    }
    if l.trim_end() == "element not found: @" || l.starts_with("element not found: @\n") {
        return "usage";
    }
    match crate::error_envelope::classify_error(error).code {
        "debugger_access_denied" => return "blocked_by_extension_frame",
        "invalid_request" => return "usage",
        "command_failed" => {}
        code => return code,
    }
    if l.contains("its tab is gone")
        || l.contains("stale sessionid")
        || l.contains("no attached tab")
    {
        "stale_target"
    } else if l.contains("not found") || l.contains("no element") || l.contains("unknown ref") {
        "element_not_found"
    } else if l.contains("timed out") || l.contains("timeout") {
        "timeout"
    } else if l.contains("not visible") || l.contains("intercept") || l.contains("not clickable") {
        "not_interactable"
    } else if l.contains("navigation") || l.contains("err_") || l.contains("net::") {
        "navigation"
    } else if l.contains("evaluation error")
        || l.contains("syntaxerror")
        || l.contains("is not defined")
    {
        "eval_error"
    } else if l.contains("could not connect") || l.contains("not connected") || l.contains("relay")
    {
        "connection"
    } else {
        "other"
    }
}

/// Host of a URL string (no scheme/path/query) — the only location data we log.
fn host_of(url: &str) -> Option<String> {
    url::Url::parse(url)
        .ok()
        .and_then(|u| u.host_str().map(|h| h.to_string()))
}

/// Append one friction record. Best-effort + cheap; never fails a command.
/// `now_unix` is passed in (callers stamp it) to keep this pure-ish and testable.
pub fn record(action: &str, error: &str, origin: &str, now_unix: u64) {
    // Tests drive real commands into deliberate failures; written to the
    // user's log, they crowded out real usage (4000 lines, most of them
    // `nonexistent_action_xyz` and fixture pages).
    if cfg!(test) || std::env::var("AGENT_BROWSER_NO_FRICTION_LOG").is_ok() {
        return;
    }
    let rec = json!({
        "ts": now_unix,
        "action": action,
        "category": categorize(error),
        "error": error.chars().take(200).collect::<String>(),
        "host": host_of(origin),
        "version": env!("CARGO_PKG_VERSION"),
        // Which session failed, so `report` can draft from "this session"
        // instead of everything the machine ever logged.
        "session": std::env::var("AGENT_BROWSER_SESSION").ok(),
    });
    let path = friction_path();
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
    {
        let _ = writeln!(f, "{}", rec);
    }
}

/// Convenience for the daemon: stamp the time and record.
pub fn record_now(action: &str, error: &str, origin: &str) {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    record(action, error, origin, now);
}

/// Parse the log into records (skipping malformed lines).
pub(crate) fn read_records() -> Vec<Value> {
    let Ok(text) = std::fs::read_to_string(friction_path()) else {
        return Vec::new();
    };
    text.lines()
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .collect()
}

/// Aggregate the log into a summary: totals, by-command, by-category, by-host,
/// and a few recent examples. Pure over the input so it's unit-testable.
pub fn aggregate(records: &[Value]) -> Value {
    use std::collections::HashMap;
    let mut by_action: HashMap<String, u64> = HashMap::new();
    let mut by_category: HashMap<String, u64> = HashMap::new();
    let mut by_host: HashMap<String, u64> = HashMap::new();
    for r in records {
        if let Some(a) = r.get("action").and_then(|v| v.as_str()) {
            *by_action.entry(a.to_string()).or_default() += 1;
        }
        if let Some(c) = r.get("category").and_then(|v| v.as_str()) {
            *by_category.entry(c.to_string()).or_default() += 1;
        }
        if let Some(h) = r.get("host").and_then(|v| v.as_str()) {
            *by_host.entry(h.to_string()).or_default() += 1;
        }
    }
    let sort_desc = |m: HashMap<String, u64>| {
        let mut v: Vec<(String, u64)> = m.into_iter().collect();
        v.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
        v
    };
    let to_pairs = |v: Vec<(String, u64)>, n: usize| -> Vec<Value> {
        v.into_iter()
            .take(n)
            .map(|(k, c)| json!({ "name": k, "count": c }))
            .collect()
    };
    let recent: Vec<Value> = records.iter().rev().take(10).cloned().collect();
    json!({
        "total": records.len(),
        "byCommand": to_pairs(sort_desc(by_action), 15),
        "byCategory": to_pairs(sort_desc(by_category), 15),
        "byHost": to_pairs(sort_desc(by_host), 10),
        "recent": recent,
    })
}

/// `chrome-use friction [--json] [--clear]` — local, no daemon. Surfaces the
/// aggregated friction log so a human/agent can see what's actually painful.
pub fn run_friction(args: &[String], json_out: bool) {
    if args.iter().any(|a| a == "--clear") {
        let _ = std::fs::remove_file(friction_path());
        if json_out {
            println!(
                "{}",
                json!({ "success": true, "data": { "cleared": true } })
            );
        } else {
            println!("✓ friction log cleared");
        }
        return;
    }

    let mut records = read_records();
    // Opportunistic trim so the file can't grow unbounded.
    if records.len() > MAX_LINES {
        let keep: Vec<Value> = records.split_off(records.len() - MAX_LINES);
        if let Ok(mut f) = std::fs::File::create(friction_path()) {
            for r in &keep {
                let _ = writeln!(f, "{}", r);
            }
        }
        records = keep;
    }
    let agg = aggregate(&records);

    if json_out {
        println!("{}", json!({ "success": true, "data": agg }));
        return;
    }

    let total = agg.get("total").and_then(|v| v.as_u64()).unwrap_or(0);
    if total == 0 {
        println!("no friction recorded yet — failures get logged here as you drive.");
        println!("(local only; never uploaded. opt out: AGENT_BROWSER_NO_FRICTION_LOG=1)");
        return;
    }
    println!("friction: {total} failed command(s) recorded\n");
    let section = |title: &str, key: &str| {
        if let Some(arr) = agg.get(key).and_then(|v| v.as_array()) {
            if !arr.is_empty() {
                println!("{title}");
                for it in arr {
                    let n = it.get("name").and_then(|v| v.as_str()).unwrap_or("?");
                    let c = it.get("count").and_then(|v| v.as_u64()).unwrap_or(0);
                    println!("  {c:>4}  {n}");
                }
                println!();
            }
        }
    };
    section("by command:", "byCommand");
    section("by error category:", "byCategory");
    section("by host:", "byHost");
    println!("(local only; `chrome-use friction --json` for raw, `--clear` to reset)");
    println!("(to send the maintainers a redacted draft of this: `chrome-use report`)");
}

// ---------------------------------------------------------------------------
// Failure signatures and the "offer to file it" nudge
// ---------------------------------------------------------------------------

/// The stable words of an error: its first line, lowercased, with quoted
/// parts, selectors, refs, numbers and URLs dropped. Two failures of the same
/// kind on different pages (`#a` vs `#b`) end up with the same words, which is
/// what both the in-session nudge and the GitHub dedup need.
pub fn error_words(error: &str) -> String {
    let first = error.lines().next().unwrap_or("").to_lowercase();
    let mut in_quote: Option<char> = None;
    let mut cleaned = String::with_capacity(first.len());
    for ch in first.chars() {
        match in_quote {
            Some(q) if ch == q => in_quote = None,
            Some(_) => {}
            None if matches!(ch, '"' | '\'' | '`') => in_quote = Some(ch),
            None => cleaned.push(ch),
        }
    }
    let mut out: Vec<&str> = Vec::new();
    for tok in cleaned.split_whitespace() {
        let t = tok.trim_matches(|c: char| ":,.;!?()[]{}".contains(c));
        if t.len() >= 2 && t.chars().all(|c| c.is_ascii_alphabetic()) {
            out.push(t);
            if out.len() == 8 {
                break;
            }
        }
    }
    out.join(" ")
}

/// Human-readable key of a failure: `click/element_not_found: element not found`.
pub fn signature_key(action: &str, error: &str) -> String {
    format!("{action}/{}: {}", categorize(error), error_words(error))
}

/// Short, stable id of a failure signature (`cu-sig-1a2b3c4d`). FNV-1a, so it
/// is identical across builds, platforms and Rust versions — it is written into
/// public issues and searched for later.
pub fn signature_id(key: &str) -> String {
    let mut h: u32 = 0x811c_9dc5;
    for b in key.as_bytes() {
        h ^= *b as u32;
        h = h.wrapping_mul(0x0100_0193);
    }
    format!("cu-sig-{h:08x}")
}

/// Categories about how the command was typed or a policy the user set, not
/// chrome-use getting in the way. Still logged, but they never nudge.
fn nudge_worthy(category: &str) -> bool {
    !matches!(category, "usage" | "policy")
}

/// `AGENT_BROWSER_NO_REPORT_HINTS=1` silences every report nudge; with the
/// friction log off there is nothing to report from, so those stay quiet too.
pub fn report_hints_enabled() -> bool {
    std::env::var_os("AGENT_BROWSER_NO_REPORT_HINTS").is_none()
        && std::env::var_os("AGENT_BROWSER_NO_FRICTION_LOG").is_none()
}

/// How long after a failed action an `eval` still reads as "worked around it".
pub const EVAL_WORKAROUND_WINDOW: std::time::Duration = std::time::Duration::from_secs(180);

/// Failures in one session before `close` suggests a report.
pub const CLOSE_HINT_THRESHOLD: u32 = 3;

/// Per-session bookkeeping for the report nudge. Lives in the daemon state;
/// pure over the `Instant`s it is handed so the throttling is unit-testable.
#[derive(Default, Debug)]
pub struct NudgeTracker {
    counts: std::collections::HashMap<String, u32>,
    offered: std::collections::HashSet<String>,
    /// `(signature key, when)` of the latest non-eval failure.
    last_failure: Option<(String, std::time::Instant)>,
    /// Failures recorded this session (for the `close` hint).
    failures: u32,
}

impl NudgeTracker {
    /// A command failed. Returns a `reportSuggestion` the second time the same
    /// signature fails in this session — once per signature.
    pub fn on_failure(
        &mut self,
        action: &str,
        error: &str,
        now: std::time::Instant,
        session: &str,
    ) -> Option<Value> {
        self.failures += 1;
        if !nudge_worthy(categorize(error)) {
            return None;
        }
        let key = signature_key(action, error);
        if !is_eval(action) {
            self.last_failure = Some((key.clone(), now));
        }
        let count = self.counts.entry(key.clone()).or_insert(0);
        *count += 1;
        let n = *count;
        if n >= 2 && self.offered.insert(key.clone()) {
            return Some(report_suggestion(&key, "repeated_failure", n, session));
        }
        None
    }

    /// The agent ran `eval`. Shortly after a failed action that usually means
    /// it is scripting around chrome-use; offer once for that failure.
    pub fn on_eval(&mut self, now: std::time::Instant, session: &str) -> Option<Value> {
        let (key, at) = self.last_failure.take()?;
        if now.saturating_duration_since(at) > EVAL_WORKAROUND_WINDOW {
            return None;
        }
        if !self.offered.insert(key.clone()) {
            return None;
        }
        let n = self.counts.get(&key).copied().unwrap_or(1);
        Some(report_suggestion(&key, "eval_workaround", n, session))
    }

    /// One line for `close` when this session hit enough friction; resets so
    /// a reused session does not repeat it.
    pub fn close_hint(&mut self, session: &str) -> Option<String> {
        if self.failures < CLOSE_HINT_THRESHOLD {
            return None;
        }
        let n = self.failures;
        self.failures = 0;
        Some(format!(
            "this session hit {n} chrome-use failures; if chrome-use got in the way, \
             offer the user to file it: `{}`",
            report_command(session)
        ))
    }
}

pub fn is_eval(action: &str) -> bool {
    matches!(action, "eval" | "evaluate" | "evalhandle")
}

/// The command the nudge tells the agent to run. Names the session when it is
/// not the default, so `report` drafts from the same session's failures.
pub fn report_command(session: &str) -> String {
    if session.is_empty() || session == "default" {
        "chrome-use report --note \"<what you were trying to do>\"".to_string()
    } else {
        format!("chrome-use report --session {session} --note \"<what you were trying to do>\"")
    }
}

fn report_suggestion(key: &str, reason: &str, count: u32, session: &str) -> Value {
    let command = report_command(session);
    json!({
        "signature": signature_id(key),
        "key": key,
        "reason": reason,
        "count": count,
        "command": command,
        "message": format!(
            "chrome-use got in the way here; at the end of the task, offer the user to file it: `{command}`"
        ),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signatures_ignore_the_page_specific_parts() {
        let a = signature_key("click", "Element not found: #missing-a");
        let b = signature_key("click", "Element not found: \"#other .thing\"");
        assert_eq!(a, b);
        assert_eq!(a, "click/element_not_found: element not found");
        // Stable across builds: written into public issues and searched later.
        assert_eq!(signature_id(&a), "cu-sig-2b1cedf4");
        // A different command or kind of failure is a different signature.
        assert_ne!(a, signature_key("fill", "Element not found: #x"));
        assert_ne!(a, signature_key("click", "Operation timed out"));
    }

    #[test]
    fn the_nudge_fires_on_the_second_failure_once_per_signature() {
        let mut t = NudgeTracker::default();
        let now = std::time::Instant::now();
        assert!(t
            .on_failure("click", "Element not found: #a", now, "default")
            .is_none());
        let s = t
            .on_failure("click", "Element not found: #b", now, "default")
            .expect("second failure with the same signature nudges");
        assert_eq!(s["reason"], "repeated_failure");
        assert_eq!(s["count"], 2);
        assert_eq!(s["signature"], "cu-sig-2b1cedf4");
        assert!(s["message"]
            .as_str()
            .unwrap()
            .contains("offer the user to file it: `chrome-use report --note"));
        // Third time: already offered for this signature.
        assert!(t
            .on_failure("click", "Element not found: #c", now, "default")
            .is_none());
        // A different signature gets its own single chance.
        assert!(t
            .on_failure("fill", "Operation timed out", now, "default")
            .is_none());
        assert!(t
            .on_failure("fill", "Operation timed out", now, "default")
            .is_some());
        assert!(t
            .on_failure("fill", "Operation timed out", now, "default")
            .is_none());
        // Usage mistakes never nudge.
        assert!(t
            .on_failure("type", "Missing 'text' parameter", now, "s")
            .is_none());
        assert!(t
            .on_failure("type", "Missing 'text' parameter", now, "s")
            .is_none());
    }

    #[test]
    fn an_eval_right_after_a_failure_nudges_once() {
        let mut t = NudgeTracker::default();
        let now = std::time::Instant::now();
        assert!(t.on_eval(now, "default").is_none(), "no failure yet");
        t.on_failure("click", "Element not found: #a", now, "work");
        let s = t
            .on_eval(now + std::time::Duration::from_secs(10), "work")
            .expect("eval within the window nudges");
        assert_eq!(s["reason"], "eval_workaround");
        assert!(s["command"].as_str().unwrap().contains("--session work"));
        // Already offered: the same failure and another eval stay quiet.
        assert!(t
            .on_failure("click", "Element not found: #a", now, "work")
            .is_none());
        assert!(t.on_eval(now, "work").is_none());
        // Outside the window: no nudge.
        let mut t = NudgeTracker::default();
        t.on_failure("hover", "Operation timed out", now, "default");
        let late = now + EVAL_WORKAROUND_WINDOW + std::time::Duration::from_secs(1);
        assert!(t.on_eval(late, "default").is_none());
    }

    #[test]
    fn close_hints_only_after_enough_friction_and_only_once() {
        let mut t = NudgeTracker::default();
        let now = std::time::Instant::now();
        t.on_failure("click", "Element not found: #a", now, "default");
        t.on_failure("click", "Operation timed out", now, "default");
        assert!(t.close_hint("default").is_none());
        t.on_failure("fill", "Operation timed out", now, "default");
        let h = t.close_hint("default").expect("three failures");
        assert!(h.contains("chrome-use report"), "{h}");
        assert!(t.close_hint("default").is_none(), "reset after the hint");
    }

    #[test]
    fn test_categorize() {
        assert_eq!(
            categorize("Element not found in the page DOM"),
            "element_not_found"
        );
        assert_eq!(categorize("Unknown ref: e9"), "element_not_found");
        assert_eq!(
            categorize("stale sessionId ... its tab is gone"),
            "stale_target"
        );
        assert_eq!(categorize("Operation timed out"), "timeout");
        assert_eq!(categorize("element is not visible"), "not_interactable");
        assert_eq!(categorize("Navigation failed: net::ERR_X"), "navigation");
        assert_eq!(
            categorize("Evaluation error: ReferenceError: x is not defined"),
            "eval_error"
        );
        assert_eq!(categorize("something weird"), "other");
    }

    #[test]
    fn test_host_of() {
        assert_eq!(host_of("https://x.com/a/b?q=1"), Some("x.com".to_string()));
        assert_eq!(host_of("not a url"), None);
    }

    #[test]
    fn test_aggregate() {
        let recs = vec![
            json!({ "action": "click", "category": "element_not_found", "host": "a.com" }),
            json!({ "action": "click", "category": "element_not_found", "host": "a.com" }),
            json!({ "action": "fill", "category": "timeout", "host": "b.com" }),
        ];
        let agg = aggregate(&recs);
        assert_eq!(agg["total"], 3);
        assert_eq!(agg["byCommand"][0]["name"], "click");
        assert_eq!(agg["byCommand"][0]["count"], 2);
        assert_eq!(agg["byCategory"][0]["name"], "element_not_found");
    }

    #[test]
    fn real_failures_get_their_own_category() {
        for (err, cat) in [
            (
                "CDP error (Page.getFrameTree): Cannot access a chrome-extension:// URL of different extension",
                "blocked_by_extension_frame",
            ),
            ("CDP error (DOM.enable): debugger_access_denied: Chrome blocked", "blocked_by_extension_frame"),
            ("Missing 'text' parameter", "usage"),
            ("Element not found: @", "usage"),
            ("Domain 'x.com' is not in the allowed domains list", "policy"),
            ("Evaluation error: TypeError: Failed to fetch", "page_fetch_failed"),
            ("stale sessionId cb-tab-1 for Runtime.evaluate", "stale_target"),
        ] {
            assert_eq!(categorize(err), cat, "{err}");
        }
    }
}
