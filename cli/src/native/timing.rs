//! Per-command timing (ported from iphone-use's `crates/server/src/timing.rs`).
//!
//! Every CDP call made while the daemon serves one command is recorded — the
//! method and how long Chrome took to answer — and a summary goes back with
//! the response as a top-level `timing` object, which `--json` callers see.
//! A slow command can then be split into Chrome's time (`cdpMs`, by method)
//! and non-CDP time (`ms` minus `cdpBusyMs`). `cdpMs` sums request
//! durations and can exceed wall time when reads overlap; `cdpBusyMs` is
//! the union of recorded foreground request intervals, not CPU time.
//! Spawned background work is outside this task-local recorder.
//!
//! The daemon also appends one JSON line per command to
//! `~/.chrome-use/timing.jsonl` (rotated to `.1` past 20 MB): action, session,
//! outcome and timings — never URLs, selectors or page content — so real
//! sessions can be analysed afterwards. `AGENT_BROWSER_TIMING_LOG=0` turns the
//! log off.

use std::collections::HashMap;
use std::future::Future;
use std::io::Write as _;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde_json::{json, Value};

tokio::task_local! {
    static RECORDER: Mutex<Vec<(String, Duration, Instant, Instant)>>;
}

/// Synthetic-duration helper for recorder tests.
#[cfg(test)]
pub fn record(method: &str, elapsed: Duration) {
    let end = Instant::now();
    record_span(method, end - elapsed, end);
}

/// Record real start/end instants so overlapping reads are counted only once
/// in wall-time occupancy. Called in the same task-local scope as the request.
pub fn record_span(method: &str, start: Instant, end: Instant) {
    let _ = RECORDER.try_with(|r| {
        if let Ok(mut calls) = r.lock() {
            calls.push((
                method.to_string(),
                end.saturating_duration_since(start),
                start,
                end,
            ));
        }
    });
}

/// Run `f` with a fresh recorder; return its output and the timing summary.
pub async fn timed<F: Future>(f: F) -> (F::Output, Value) {
    let started = Instant::now();
    RECORDER
        .scope(Mutex::new(Vec::new()), async move {
            let out = f.await;
            let calls = RECORDER
                .try_with(|r| {
                    r.lock()
                        .map(|mut c| std::mem::take(&mut *c))
                        .unwrap_or_default()
                })
                .unwrap_or_default();
            let end = Instant::now();
            let durations: Vec<_> = calls.iter().map(|(m, d, _, _)| (m.clone(), *d)).collect();
            let spans: Vec<_> = calls
                .iter()
                .map(|(_, _, a, b)| {
                    (
                        a.saturating_duration_since(started),
                        b.saturating_duration_since(started),
                    )
                })
                .collect();
            let total = end.duration_since(started);
            let mut summary = summarize(total, &durations);
            let busy = interval_union(total, &spans);
            summary["cdpBusyMs"] = json!(ms(busy));
            summary["nonCdpMs"] = json!(ms(total).saturating_sub(ms(busy)));
            (out, summary)
        })
        .await
}

fn ms(d: Duration) -> u64 {
    d.as_millis() as u64
}

/// `{ms, cdpMs, cdpCalls, slowest: [{method, count, ms}]}`, the three costliest
/// methods by total time. Small on purpose: it rides on every `--json` reply.
pub fn summarize(total: Duration, calls: &[(String, Duration)]) -> Value {
    let mut by: HashMap<&str, (u32, Duration)> = HashMap::new();
    for (m, d) in calls {
        let e = by.entry(m.as_str()).or_default();
        e.0 += 1;
        e.1 += *d;
    }
    let mut rows: Vec<(&str, u32, Duration)> =
        by.into_iter().map(|(m, (n, d))| (m, n, d)).collect();
    rows.sort_by(|a, b| b.2.cmp(&a.2).then_with(|| a.0.cmp(b.0)));
    let cdp: Duration = calls.iter().map(|(_, d)| *d).sum();
    json!({
        "ms": ms(total),
        "cdpMs": ms(cdp),
        "cdpCalls": calls.len(),
        "slowest": rows
            .iter()
            .take(3)
            .map(|(m, n, d)| json!({ "method": m, "count": n, "ms": ms(*d) }))
            .collect::<Vec<_>>(),
    })
}

/// Union of completed CDP intervals clipped to this command's wall budget.
fn interval_union(total: Duration, spans: &[(Duration, Duration)]) -> Duration {
    let mut spans: Vec<_> = spans
        .iter()
        .map(|(a, b)| ((*a).min(total), (*b).min(total)))
        .filter(|(a, b)| b > a)
        .collect();
    spans.sort_unstable();
    let mut busy = Duration::ZERO;
    let mut end = Duration::ZERO;
    for (a, b) in spans {
        if b > end {
            busy += b - a.max(end);
            end = b;
        }
    }
    busy
}

const LOG_ROTATE_BYTES: u64 = 20 << 20;
static LOG_LOCK: Mutex<()> = Mutex::new(());

fn log_path() -> Option<PathBuf> {
    if matches!(
        std::env::var("AGENT_BROWSER_TIMING_LOG").as_deref(),
        Ok("0" | "false" | "off")
    ) {
        return None;
    }
    std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".chrome-use").join("timing.jsonl"))
}

/// Append one line for a served command. Best-effort; never fails the command.
pub fn log_command(session: &str, action: &str, ok: bool, timing: &Value) {
    let Some(path) = log_path() else {
        return;
    };
    let line = json!({
        "ts": chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
        "session": session,
        "action": action,
        "ok": ok,
        "timing": timing,
    });
    append_to(&path, &line.to_string());
}

fn append_to(path: &std::path::Path, line: &str) {
    let Ok(_guard) = LOG_LOCK.lock() else {
        return;
    };
    if std::fs::metadata(path).is_ok_and(|m| m.len() >= LOG_ROTATE_BYTES) {
        let mut rotated = path.as_os_str().to_owned();
        rotated.push(".1");
        let _ = std::fs::rename(path, rotated);
    }
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    {
        let _ = writeln!(f, "{line}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn summary_ranks_methods_by_total_time() {
        let calls = vec![
            ("Runtime.evaluate".to_string(), Duration::from_millis(30)),
            ("DOM.getBoxModel".to_string(), Duration::from_millis(5)),
            ("Runtime.evaluate".to_string(), Duration::from_millis(40)),
            (
                "Input.dispatchMouseEvent".to_string(),
                Duration::from_millis(20),
            ),
            (
                "Page.captureScreenshot".to_string(),
                Duration::from_millis(1),
            ),
        ];
        let s = summarize(Duration::from_millis(200), &calls);
        assert_eq!(s["ms"], 200);
        assert_eq!(s["cdpMs"], 96);
        assert_eq!(s["cdpCalls"], 5);
        assert_eq!(s["slowest"][0]["method"], "Runtime.evaluate");
        assert_eq!(s["slowest"][0]["count"], 2);
        assert_eq!(s["slowest"][0]["ms"], 70);
        assert_eq!(s["slowest"].as_array().unwrap().len(), 3);
    }

    #[tokio::test]
    async fn timed_collects_calls_made_inside_only() {
        record("Outside.call", Duration::from_millis(9));
        let (out, t) = timed(async {
            record("Page.navigate", Duration::from_millis(12));
            7
        })
        .await;
        assert_eq!(out, 7);
        assert_eq!(t["cdpCalls"], 1);
        assert_eq!(t["slowest"][0]["method"], "Page.navigate");
    }

    #[test]
    fn overlapping_cdp_intervals_do_not_double_count_wall_time() {
        let d = Duration::from_millis;
        assert_eq!(
            interval_union(d(100), &[(d(0), d(80)), (d(20), d(100))]),
            d(100)
        );
        assert_eq!(
            interval_union(d(100), &[(d(70), d(200)), (d(10), d(20))]),
            d(40)
        );
        assert_eq!(interval_union(d(100), &[(d(60), d(40))]), Duration::ZERO);
    }

    #[tokio::test]
    async fn timed_reports_accumulated_and_wall_occupancy_separately() {
        let (_, t) = timed(async {
            let start = Instant::now();
            tokio::time::sleep(Duration::from_millis(20)).await;
            let end = Instant::now();
            record_span("Read.one", start, end);
            record_span("Read.two", start, end);
        })
        .await;
        assert!(t["cdpMs"].as_u64().unwrap() >= t["cdpBusyMs"].as_u64().unwrap() * 2);
        assert!(t["cdpBusyMs"].as_u64().unwrap() <= t["ms"].as_u64().unwrap());
        assert_eq!(t["cdpCalls"], 2);
        assert_eq!(
            t["cdpBusyMs"].as_u64().unwrap() + t["nonCdpMs"].as_u64().unwrap(),
            t["ms"].as_u64().unwrap()
        );
    }

    #[tokio::test]
    async fn joined_requests_inherit_recorder_but_spawned_work_does_not() {
        let (_, timing) = timed(async {
            let read = |method: &'static str| async move {
                let start = Instant::now();
                tokio::time::sleep(Duration::from_millis(20)).await;
                record_span(method, start, Instant::now());
            };
            tokio::join!(read("Read.one"), read("Read.two"));
            tokio::spawn(read("Background.read")).await.unwrap();
        })
        .await;
        assert_eq!(timing["cdpCalls"], 2);
        assert!(timing["cdpMs"].as_u64().unwrap() > timing["cdpBusyMs"].as_u64().unwrap());
        assert!(timing["slowest"]
            .as_array()
            .unwrap()
            .iter()
            .all(|r| r["method"] != "Background.read"));
    }

    #[test]
    fn log_rotates_past_the_limit() {
        let dir = std::env::temp_dir().join(format!("cu-timing-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("timing.jsonl");
        std::fs::write(&path, vec![b'x'; (LOG_ROTATE_BYTES + 1) as usize]).unwrap();
        append_to(&path, "{\"a\":1}");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "{\"a\":1}\n");
        assert!(dir.join("timing.jsonl.1").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
