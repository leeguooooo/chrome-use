//! Top-level navigations of the session's page, as the CDP events it already
//! receives report them, so an error that a reload explains can say so (#837).
//!
//! A page that reloads itself (a dev server's hot-reload loop, say) replaces
//! its document and execution contexts under whatever command is running. The
//! command then fails as a drifted screenshot target, an `isolation_unavailable`
//! read or a wait that never matched, and none of those words point at the
//! reload. This only reads `Page.frameNavigated` events the daemon is already
//! draining: no CDP call is sent and nothing is visible to the page.
//!
//! A reload is stated only when a navigation event was received. With none,
//! the error is left exactly as it was.

use std::collections::VecDeque;
use std::time::Instant;

use serde_json::Value;

/// How many navigations are kept. A page reloading every second fills this in
/// about a minute; the note only needs the recent ones.
const MAX_NAVIGATIONS: usize = 64;

/// What every note starts with, so a nested command's note is not added twice
/// by the command that ran it.
const NOTE_MARKER: &str = "Observed while it ran: the page";

#[derive(Debug, Clone)]
struct TopNavigation {
    at: Instant,
    url: String,
    loader_id: Option<String>,
}

#[derive(Debug, Default)]
pub struct NavigationLog {
    entries: VecDeque<TopNavigation>,
    /// When the event buffer last overflowed. Navigations may have been
    /// dropped then, so counts covering it are lower bounds.
    lagged_at: Option<Instant>,
    /// When the previous command ended, and the page's URL then.
    previous_command: Option<(Instant, String)>,
}

/// The URL and loader of a main-frame `Page.frameNavigated`; `None` for any
/// other event, including a child frame's navigation.
pub fn top_level_navigation(method: &str, params: &Value) -> Option<(String, Option<String>)> {
    if method != "Page.frameNavigated" {
        return None;
    }
    let frame = params.get("frame")?;
    if frame
        .get("parentId")
        .is_some_and(|p| p.as_str().is_some_and(|s| !s.is_empty()))
    {
        return None;
    }
    let url = frame.get("url").and_then(Value::as_str)?.to_string();
    let loader = frame
        .get("loaderId")
        .and_then(Value::as_str)
        .map(str::to_string);
    Some((url, loader))
}

impl NavigationLog {
    pub fn record(&mut self, at: Instant, url: String, loader_id: Option<String>) {
        // One navigation can be reported twice (two subscribers, a re-attach);
        // its loader identifies it.
        if loader_id.is_some()
            && self
                .entries
                .back()
                .is_some_and(|last| last.loader_id == loader_id)
        {
            return;
        }
        if self.entries.len() >= MAX_NAVIGATIONS {
            self.entries.pop_front();
        }
        self.entries.push_back(TopNavigation { at, url, loader_id });
    }

    pub fn note_lag(&mut self, at: Instant) {
        self.lagged_at = Some(at);
    }

    /// Called as every command returns.
    pub fn command_ended(&mut self, at: Instant, url: String) {
        self.previous_command = Some((at, url));
    }

    /// The sentence to append to an error a reload would explain, or `None`
    /// when no top-level navigation was received since the previous command
    /// ended (or, for the first command, since the log began).
    pub fn note_for(&self, command_started: Instant) -> Option<String> {
        let window_start = self.previous_command.as_ref().map(|(at, _)| *at);
        let in_window: Vec<&TopNavigation> = self
            .entries
            .iter()
            .filter(|n| window_start.is_none_or(|start| n.at >= start))
            .collect();
        let last = in_window.last()?;
        let during = in_window.iter().filter(|n| n.at >= command_started).count();
        let before = in_window.len() - during;
        // The navigations, preceded by the URL the page had when the previous
        // command ended: a reload when every one of them is the same page.
        let first = self
            .previous_command
            .as_ref()
            .map(|(_, url)| without_fragment(url))
            .filter(|url| !url.is_empty())
            .unwrap_or_else(|| without_fragment(&in_window[0].url));
        let reloaded = in_window.iter().all(|n| without_fragment(&n.url) == first);
        let lower_bound = match (self.lagged_at, window_start) {
            (Some(lag), Some(start)) => lag >= start,
            (Some(_), None) => true,
            (None, _) => false,
        };
        Some(compose_note(NoteFacts {
            reloaded,
            during,
            before,
            first_command: window_start.is_none(),
            lower_bound,
            last_path: &url_path(&last.url),
        }))
    }
}

struct NoteFacts<'a> {
    reloaded: bool,
    during: usize,
    before: usize,
    first_command: bool,
    lower_bound: bool,
    last_path: &'a str,
}

fn times(n: usize) -> String {
    if n == 1 {
        "once".to_string()
    } else {
        format!("{n} times")
    }
}

fn compose_note(f: NoteFacts) -> String {
    let verb = if f.reloaded { "reloaded" } else { "navigated" };
    let at_least = if f.lower_bound { "at least " } else { "" };
    let since = if f.first_command {
        "before it started"
    } else {
        "since the previous command ended"
    };
    let counts = match (f.during, f.before) {
        (d, 0) => format!("{at_least}{} during this command", times(d)),
        (0, b) => format!("{at_least}{} {since}", times(b)),
        (d, b) => format!(
            "{at_least}{} during this command and {} {since}",
            times(d),
            times(b)
        ),
    };
    let mid = if f.reloaded { "reload" } else { "navigation" };
    format!(
        "{NOTE_MARKER} {verb} {counts} (last: {}). A command that lands mid-{} loses its \
         document and the page's execution contexts with it. Let the page settle \
         (`chrome-use wait --load load`) and retry; a page that keeps reloading on its own \
         (a dev server's reload loop, for one) has to be stopped first.",
        f.last_path, mid
    )
}

/// The errors a reload under the command explains: a screenshot whose target
/// could not be confirmed, a read that lost its isolated world, a lost
/// execution context, and waits or captures that ran out of time.
pub fn error_wants_note(error: &str) -> bool {
    !error.contains(NOTE_MARKER)
        && (error.contains("target drifted")
            || error.contains("isolation_unavailable")
            || error.contains("Wait timed out after")
            || error.contains("Execution context was destroyed")
            || error.contains("Cannot find context with specified id")
            || super::browser::is_relay_timeout(error))
}

/// Commands that navigate on purpose: a navigation under them is their own.
/// `script` runs other commands, which add their own notes.
pub fn action_navigates(action: &str) -> bool {
    matches!(
        action,
        "launch" | "navigate" | "reload" | "back" | "forward" | "tab_new" | "a11y" | "script"
    )
}

/// `error` with the note appended, separated as a new sentence.
pub fn append_note(error: &str, note: &str) -> String {
    let trimmed = error.trim_end();
    let sep = if trimmed.ends_with(['.', '!', '?']) {
        " "
    } else {
        ". "
    };
    format!("{trimmed}{sep}{note}")
}

fn without_fragment(url: &str) -> &str {
    url.split('#').next().unwrap_or(url)
}

/// A URL's path only: no host, query or fragment, which may carry tokens.
fn url_path(url: &str) -> String {
    match url::Url::parse(url) {
        Ok(parsed) if parsed.cannot_be_a_base() => parsed.scheme().to_string() + ":",
        Ok(parsed) => parsed.path().to_string(),
        Err(_) => "(an unparsable URL)".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::time::Duration;

    fn at(base: Instant, ms: u64) -> Instant {
        base + Duration::from_millis(ms)
    }

    #[test]
    fn only_main_frame_navigations_count() {
        let main = json!({"frame": {"id": "F", "loaderId": "L1", "url": "http://127.0.0.1:3919/"}});
        assert_eq!(
            top_level_navigation("Page.frameNavigated", &main),
            Some(("http://127.0.0.1:3919/".into(), Some("L1".into())))
        );
        let child = json!({"frame": {"id": "C", "parentId": "F", "url": "http://x/"}});
        assert_eq!(top_level_navigation("Page.frameNavigated", &child), None);
        assert_eq!(
            top_level_navigation("Page.loadEventFired", &json!({})),
            None
        );
    }

    #[test]
    fn no_navigation_means_no_note_and_the_error_is_unchanged() {
        let base = Instant::now();
        let mut log = NavigationLog::default();
        log.command_ended(base, "http://127.0.0.1:3919/".into());
        assert_eq!(log.note_for(at(base, 10)), None);
        // Navigations before the previous command ended are its business.
        let mut log = NavigationLog::default();
        log.record(base, "http://127.0.0.1:3919/".into(), Some("L1".into()));
        log.command_ended(at(base, 5), "http://127.0.0.1:3919/".into());
        assert_eq!(log.note_for(at(base, 10)), None);
    }

    #[test]
    fn reloads_during_the_command_are_counted_with_the_path_only() {
        let base = Instant::now();
        let mut log = NavigationLog::default();
        log.command_ended(base, "http://127.0.0.1:3919/__setup?token=s3cret".into());
        let start = at(base, 100);
        for (i, ms) in [200u64, 1200, 2200].into_iter().enumerate() {
            log.record(
                at(base, ms),
                "http://127.0.0.1:3919/__setup?token=s3cret#x".into(),
                Some(format!("L{i}")),
            );
        }
        let note = log.note_for(start).unwrap();
        assert!(
            note.starts_with(
                "Observed while it ran: the page reloaded 3 times during this command \
                 (last: /__setup)."
            ),
            "{note}"
        );
        assert!(
            !note.contains("s3cret") && !note.contains("127.0.0.1"),
            "{note}"
        );
        assert!(note.contains("`chrome-use wait --load load`"), "{note}");
    }

    #[test]
    fn reloads_since_the_previous_command_are_named_as_such() {
        // The issue's sequence: a `wait` timed out while the page reloaded,
        // and the `screenshot` right after it drifted within milliseconds.
        let base = Instant::now();
        let mut log = NavigationLog::default();
        log.command_ended(base, "http://h/".into());
        log.record(at(base, 10), "http://h/".into(), Some("L1".into()));
        log.record(at(base, 20), "http://h/".into(), Some("L2".into()));
        let note = log.note_for(at(base, 30)).unwrap();
        assert!(
            note.contains("reloaded 2 times since the previous command ended (last: /)"),
            "{note}"
        );
        log.record(at(base, 40), "http://h/".into(), Some("L3".into()));
        let note = log.note_for(at(base, 30)).unwrap();
        assert!(
            note.contains(
                "reloaded once during this command and 2 times since the previous command ended"
            ),
            "{note}"
        );
    }

    #[test]
    fn a_different_url_is_a_navigation_not_a_reload() {
        let base = Instant::now();
        let mut log = NavigationLog::default();
        log.command_ended(base, "http://h/__setup".into());
        log.record(at(base, 50), "http://h/".into(), Some("L1".into()));
        let note = log.note_for(at(base, 10)).unwrap();
        assert!(
            note.contains("the page navigated once during this command (last: /)"),
            "{note}"
        );
        assert!(note.contains("mid-navigation"), "{note}");
    }

    #[test]
    fn a_repeated_loader_is_one_navigation_and_overflow_makes_it_a_lower_bound() {
        let base = Instant::now();
        let mut log = NavigationLog::default();
        log.command_ended(base, "http://h/".into());
        log.record(at(base, 20), "http://h/".into(), Some("L1".into()));
        log.record(at(base, 21), "http://h/".into(), Some("L1".into()));
        assert!(log
            .note_for(at(base, 10))
            .unwrap()
            .contains("reloaded once during"));
        log.note_lag(at(base, 30));
        assert!(log
            .note_for(at(base, 10))
            .unwrap()
            .contains("reloaded at least once during"));
    }

    #[test]
    fn the_first_command_says_before_it_started() {
        let base = Instant::now();
        let mut log = NavigationLog::default();
        log.record(base, "http://h/a".into(), Some("L1".into()));
        let note = log.note_for(at(base, 10)).unwrap();
        assert!(note.contains("once before it started (last: /a)"), "{note}");
    }

    #[test]
    fn the_errors_a_reload_explains_and_only_once() {
        for e in [
            "screenshot target drifted: this session is driving http://h/, but its CDP session \
             is attached to an unknown target",
            "isolation_unavailable: Runtime.evaluate requires an isolated execution context",
            "Wait timed out after 25000ms",
            "CDP error (Page.captureScreenshot): relay timeout after 25000ms: x",
        ] {
            assert!(error_wants_note(e), "{e}");
            let noted = append_note(e, "Observed while it ran: the page reloaded once.");
            assert!(!error_wants_note(&noted), "{noted}");
            assert!(noted.starts_with(e), "{noted}");
        }
        assert!(!error_wants_note("Element not found: #x"));
        assert_eq!(
            append_note("Wait timed out after 5ms", "N."),
            "Wait timed out after 5ms. N."
        );
        assert_eq!(append_note("Done.", "N."), "Done. N.");
    }

    #[test]
    fn paths_carry_no_host_or_query() {
        assert_eq!(url_path("https://a.example/p/q?x=1#f"), "/p/q");
        assert_eq!(url_path("about:blank"), "about:");
        assert_eq!(url_path("nope"), "(an unparsable URL)");
    }
}
