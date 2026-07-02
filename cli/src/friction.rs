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
    if std::env::var("AGENT_BROWSER_NO_FRICTION_LOG").is_ok() {
        return;
    }
    let rec = json!({
        "ts": now_unix,
        "action": action,
        "category": categorize(error),
        "error": error.chars().take(200).collect::<String>(),
        "host": host_of(origin),
        "version": env!("CARGO_PKG_VERSION"),
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
fn read_records() -> Vec<Value> {
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
}

const ISSUES_NEW_URL: &str = "https://github.com/leeguooooo/chrome-use/issues/new";

/// Build a paste-ready GitHub issue body from the local friction log + build
/// metadata. De-identified (friction records only carry host, never full URLs).
fn report_markdown(agg: &Value) -> String {
    let total = agg.get("total").and_then(|v| v.as_u64()).unwrap_or(0);
    let list = |key: &str| -> String {
        agg.get(key)
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .map(|it| {
                        format!(
                            "- {} × {}",
                            it.get("count").and_then(|v| v.as_u64()).unwrap_or(0),
                            it.get("name").and_then(|v| v.as_str()).unwrap_or("?")
                        )
                    })
                    .collect::<Vec<_>>()
                    .join("\n")
            })
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "- (none)".to_string())
    };
    format!(
        "## What happened\n<!-- what you were doing, the exact command, expected vs actual -->\n\n\
         ## Environment\n- chrome-use: {ver}\n- platform: {os}/{arch}\n<!-- run `chrome-use doctor` and paste its output here if the issue is about connect/send -->\n\n\
         ## Local friction summary\n_{total} failed command(s) recorded locally (de-identified: command + error category + host only, never full URLs)._\n\n\
         **By command**\n{by_cmd}\n\n**By category**\n{by_cat}\n\n**By host**\n{by_host}\n",
        ver = env!("CARGO_PKG_VERSION"),
        os = std::env::consts::OS,
        arch = std::env::consts::ARCH,
        total = total,
        by_cmd = list("byCommand"),
        by_cat = list("byCategory"),
        by_host = list("byHost"),
    )
}

/// `chrome-use report [--open] [--json]` — OPT-IN. Packages the local friction
/// log + build metadata into a paste-ready GitHub issue. Never auto-uploads;
/// `--open` just opens the (empty) new-issue page in the browser for you.
pub fn run_report(args: &[String], json_out: bool) {
    let records = read_records();
    let agg = aggregate(&records);
    let body = report_markdown(&agg);

    if json_out {
        println!(
            "{}",
            json!({ "success": true, "data": {
                "issueUrl": ISSUES_NEW_URL,
                "markdown": body,
                "friction": agg,
            }})
        );
        return;
    }

    println!("{}", body);
    println!("─────────────────────────────────────────────");
    println!("Copy the block above into a new issue: {ISSUES_NEW_URL}");
    println!("(Nothing was uploaded. `--open` opens the new-issue page in your browser.)");

    if args.iter().any(|a| a == "--open") {
        let opener = if cfg!(target_os = "macos") { "open" } else { "xdg-open" };
        let _ = std::process::Command::new(opener)
            .arg(ISSUES_NEW_URL)
            .spawn();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
