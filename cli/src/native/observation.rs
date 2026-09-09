//! Bounded request context for action observations. Full captured URLs remain in
//! the request tracker and are available through the dedicated requests command.

const MAX_REQUESTS: usize = 20;
const MAX_LINE_BYTES: usize = 256;

pub(super) struct RequestSummary {
    pub lines: Vec<String>,
    pub total: usize,
    pub omitted: usize,
    pub shortened: usize,
}

fn shorten(value: &str, limit: usize) -> String {
    if value.len() <= limit {
        return value.to_string();
    }
    // Reserve space for the suffix; trim on a UTF-8 boundary, never mid-codepoint.
    let suffix_size = format!(" [truncated; {} bytes omitted]", value.len()).len();
    let mut end = limit.saturating_sub(suffix_size).min(value.len());
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    format!(
        "{} [truncated; {} bytes omitted]",
        &value[..end],
        value.len() - end
    )
}

fn request_line(method: &str, url: &str) -> (String, bool) {
    let is_data = url
        .get(..5)
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case("data:"));
    let display_url = if is_data {
        let (header, payload) = url.split_once(',').unwrap_or((url, ""));
        format!(
            "{} [{} encoded payload bytes omitted]",
            shorten(header, 100),
            payload.len()
        )
    } else {
        url.to_string()
    };
    // Network metadata is untrusted text. Keep one request on one terminal line.
    let line: String = format!("{method} {display_url}")
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    let shortened = is_data || line.len() > MAX_LINE_BYTES;
    (shorten(&line, MAX_LINE_BYTES), shortened)
}

pub(super) fn summarize_requests<'a>(
    requests: impl IntoIterator<Item = (&'a str, &'a str)>,
) -> RequestSummary {
    let mut result = RequestSummary {
        lines: Vec::new(),
        total: 0,
        omitted: 0,
        shortened: 0,
    };
    for (method, url) in requests {
        result.total += 1;
        if result.lines.len() == MAX_REQUESTS {
            result.omitted += 1;
            continue;
        }
        let (line, shortened) = request_line(method, url);
        result.shortened += usize::from(shortened);
        result.lines.push(line);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn data_images_do_not_enter_observation_payloads() {
        let payload = "SYNTHETIC_IMAGE_PAYLOAD".repeat(20_000);
        let url = format!("data:image/png;base64,{payload}");
        let summary = summarize_requests([("GET", url.as_str())]);
        assert_eq!(summary.total, 1);
        assert_eq!(summary.shortened, 1);
        assert!(summary.lines[0].contains("data:image/png;base64"));
        assert!(summary.lines[0].contains(&payload.len().to_string()));
        assert!(!summary.lines[0].contains("SYNTHETIC_IMAGE_PAYLOAD"));
        assert!(summary.lines[0].len() <= MAX_LINE_BYTES);
    }

    #[test]
    fn burst_is_bounded_and_omissions_are_counted() {
        let summary =
            summarize_requests((0..1000).map(|_| ("GET", "https://example.com/resource")));
        assert_eq!(summary.total, 1000);
        assert_eq!(summary.lines.len(), 20);
        assert_eq!(summary.omitted, 980);
        assert_eq!(summary.shortened, 0);
    }

    #[test]
    fn long_unicode_urls_and_control_characters_stay_bounded() {
        let url = format!("https://example.com/\n{}\u{1b}[31m", "路径".repeat(200));
        let summary = summarize_requests([("GET", url.as_str())]);
        assert_eq!(summary.shortened, 1);
        assert!(summary.lines[0].len() <= MAX_LINE_BYTES);
        assert!(!summary.lines[0].chars().any(char::is_control));
        assert!(summary.lines[0].contains("truncated"));
    }

    #[test]
    fn ordinary_requests_and_empty_observations_remain_readable() {
        let summary = summarize_requests([("POST", "https://example.com/api/order")]);
        assert_eq!(summary.lines, ["POST https://example.com/api/order"]);
        assert_eq!(summary.shortened, 0);
        let empty = summarize_requests([]);
        assert_eq!(empty.total, 0);
        assert!(empty.lines.is_empty());
    }
}

pub(super) fn capture_error(stage: &str, error: &str) -> serde_json::Value {
    let metadata = crate::error_envelope::classify_error(error);
    serde_json::json!({"stage":stage,"message":error,"code":metadata.code,"retryable":metadata.retryable})
}

/// Whether a delta is really a whole-page replacement: nearly every line of the
/// old tree gone and nearly every line of the new one added. A delta like that
/// costs more than the new tree and says less. Pure over the diff counts so
/// the threshold is testable without a browser.
pub(super) fn page_replaced(delta: &super::diff::SnapshotDiffResult) -> bool {
    let before_lines = delta.removals + delta.unchanged;
    let after_lines = delta.additions + delta.unchanged;
    // Small trees (a dialog, an empty page) are cheap either way; only a
    // page-sized delta is worth swapping for the tree.
    if before_lines < 20 || after_lines < 20 {
        return false;
    }
    delta.removals * 10 >= before_lines * 8 && delta.additions * 10 >= after_lines * 8
}

/// Compare only evidence actually captured. Missing evidence is never an empty
/// page, an empty URL, or proof that an action changed nothing.
pub(super) fn changes(
    before: &Result<String, String>,
    after: &Result<String, String>,
    url_before: &Result<String, String>,
    url_after: &Result<String, String>,
) -> serde_json::Map<String, serde_json::Value> {
    use serde_json::{json, Map, Value};
    let mut out = Map::new();
    let mut errors = Vec::new();
    for (stage, result) in [
        ("beforeSnapshot", before),
        ("afterSnapshot", after),
        ("beforeUrl", url_before),
        ("afterUrl", url_after),
    ] {
        if let Err(error) = result {
            errors.push(capture_error(stage, error));
        }
    }
    let mut changed = false;
    match (before, after) {
        (Ok(before), Ok(after)) => {
            let delta = super::diff::diff_snapshots(
                &format!("{}\n", before.trim_end()),
                &format!("{}\n", after.trim_end()),
            );
            changed = delta.changed;
            if delta.changed {
                if page_replaced(&delta) {
                    // A click that navigated: the diff is the whole old tree
                    // as removals plus the whole new tree as additions, twice
                    // the bytes of the page for no information the new tree
                    // does not carry. Return the new tree, as navigation does.
                    out.insert("snapshot".into(), json!(after));
                    out.insert("replaced".into(), json!(true));
                    out.insert("removed".into(), json!(delta.removals));
                } else {
                    out.insert("delta".into(), json!(delta.diff));
                    out.insert("added".into(), json!(delta.additions));
                    out.insert("removed".into(), json!(delta.removals));
                }
            }
        }
        (Err(_), Ok(after)) => {
            out.insert("snapshot".into(), json!(after));
        }
        _ => {}
    }
    if let (Ok(before), Ok(after)) = (url_before, url_after) {
        if before != after {
            changed = true;
            out.insert("urlChanged".into(), json!({"from":before,"to":after}));
        }
    }
    out.insert(
        "changed".into(),
        if changed {
            json!(true)
        } else if errors.is_empty() {
            json!(false)
        } else {
            Value::Null
        },
    );
    out.insert(
        "status".into(),
        json!(if after.is_err() {
            "unavailable"
        } else if errors.is_empty() {
            "complete"
        } else {
            "partial"
        }),
    );
    if !errors.is_empty() {
        out.insert("errors".into(), json!(errors));
    }
    out
}

/// Preserve the action result and report its separate observation quality.
/// Diagnostic failure must not turn a returned action into an invitation to retry.
pub(super) fn annotate_incomplete(response: &mut serde_json::Value) {
    use serde_json::json;
    let status = response
        .pointer("/data/observed/status")
        .and_then(|s| s.as_str());
    if matches!(status, Some("partial" | "unavailable")) {
        response["data"]["observed"]["retryAction"] = json!(false);
        let note = "The action returned, but its observation is incomplete. Do not replay the action just to obtain an observation; inspect the current state first.";
        let warning = match response.get("warning").and_then(|w| w.as_str()) {
            Some(existing) => format!("{existing}\n{note}"),
            None => note.to_string(),
        };
        response["warning"] = json!(warning);
    }
}

#[cfg(test)]
mod capture_tests {
    use super::*;
    use serde_json::json;
    fn ok(value: &str) -> Result<String, String> {
        Ok(value.into())
    }
    fn missing() -> Result<String, String> {
        Err("fixture capture failure".into())
    }

    #[test]
    fn failed_capture_is_not_reported_as_no_change_or_removal() {
        let out = changes(
            &ok("button Save"),
            &missing(),
            &ok("https://example.com"),
            &ok("https://example.com"),
        );
        assert_eq!(out["status"], "unavailable");
        assert!(out["changed"].is_null());
        assert!(!out.contains_key("delta"));
        assert!(!out.contains_key("removed"));
    }

    #[test]
    fn absent_baseline_returns_the_real_after_tree_without_a_fabricated_diff() {
        let out = changes(
            &missing(),
            &ok("button Next"),
            &ok("https://example.com"),
            &ok("https://example.com"),
        );
        assert_eq!(out["status"], "partial");
        assert_eq!(out["snapshot"], "button Next");
        assert!(out["changed"].is_null());
        assert!(!out.contains_key("delta"));
    }

    #[test]
    fn missing_url_does_not_invent_an_empty_navigation() {
        let out = changes(
            &ok("button Save"),
            &ok("button Save"),
            &ok("https://example.com"),
            &missing(),
        );
        assert_eq!(out["status"], "partial");
        assert!(out["changed"].is_null());
        assert!(!out.contains_key("urlChanged"));
    }

    #[test]
    fn real_unchanged_capture_and_real_change_remain_distinct() {
        let out = changes(&ok("button Save"), &ok("button Save"), &ok("u"), &ok("u"));
        assert_eq!(out["status"], "complete");
        assert_eq!(out["changed"], false);
        let out = changes(&ok("button Save"), &ok("button Next"), &ok("u"), &missing());
        assert_eq!(out["changed"], true);
        assert_eq!(out["status"], "partial");
    }

    fn tree(prefix: &str, n: usize) -> String {
        (0..n)
            .map(|i| format!("- link \"{prefix}{i}\" [ref=e{i}]"))
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn a_navigating_click_returns_the_new_tree_not_a_struck_through_old_page() {
        // Every line of the old page gone, every line of the new page added:
        // the diff would be both pages. The observation carries the new tree.
        let before = tree("old", 40);
        let after = tree("new", 35);
        let out = changes(&ok(&before), &ok(&after), &ok("/a"), &ok("/b"));
        assert_eq!(out["changed"], true);
        assert_eq!(out["replaced"], true);
        assert_eq!(out["snapshot"], after);
        assert_eq!(out["removed"], 40);
        assert!(!out.contains_key("delta"));
        assert!(!out.contains_key("added"));
    }

    #[test]
    fn an_in_page_change_still_returns_a_delta() {
        // One row re-sorted on a 40-line page is a delta, not a replacement.
        let before = tree("row", 40);
        let mut lines: Vec<&str> = before.lines().collect();
        lines.swap(3, 30);
        let after = lines.join("\n");
        let out = changes(&ok(&before), &ok(&after), &ok("u"), &ok("u"));
        assert_eq!(out["changed"], true);
        assert!(out.contains_key("delta"));
        assert!(!out.contains_key("replaced"));
        assert!(!out.contains_key("snapshot"));
    }

    #[test]
    fn small_trees_are_never_reported_as_replaced() {
        // A dialog swapping for another dialog is cheap as a diff, and a
        // "replaced" flag there would make agents expect a page-sized tree.
        let out = changes(
            &ok("- button \"OK\" [ref=e1]"),
            &ok("- button \"Done\" [ref=e2]"),
            &ok("u"),
            &ok("u"),
        );
        assert!(out.contains_key("delta"));
        assert!(!out.contains_key("replaced"));
    }

    #[test]
    fn observation_failure_retains_action_data_and_forbids_replay() {
        let mut response =
            json!({"success":true,"data":{"clicked":"Save","observed":{"status":"unavailable"}}});
        annotate_incomplete(&mut response);
        assert_eq!(response["success"], true);
        assert_eq!(response["data"]["observed"]["retryAction"], false);
        assert!(response["warning"]
            .as_str()
            .unwrap()
            .contains("Do not replay"));
        assert_eq!(response["data"]["clicked"], "Save");
    }
}
