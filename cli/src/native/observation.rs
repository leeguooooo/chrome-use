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

fn is_extension_url(url: &str) -> bool {
    [
        "chrome-extension://",
        "moz-extension://",
        "safari-web-extension://",
    ]
    .iter()
    .any(|p| url.starts_with(p))
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
        // Another extension's own fetches (locale files and the like) are not
        // the page's response to the action.
        if is_extension_url(url) {
            continue;
        }
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
    use super::{human_check_vendor, human_check_verdict, resource_lines};

    #[test]
    fn extension_requests_are_not_the_pages_response() {
        let s = super::summarize_requests([
            ("GET", "chrome-extension://abc/locales.json"),
            ("POST", "https://example.com/api/cart"),
        ]);
        assert_eq!(s.total, 1);
        assert!(s.lines[0].contains("/api/cart"));
    }

    #[test]
    fn human_check_vendors_are_recognized_by_url() {
        assert_eq!(
            human_check_vendor("https://platform.openai.com/sentinel/abc123/sdk.js"),
            Some("OpenAI Sentinel")
        );
        assert_eq!(
            human_check_vendor("https://js.hcaptcha.com/1/api.js"),
            Some("hCaptcha")
        );
        assert_eq!(
            human_check_vendor("https://challenges.cloudflare.com/turnstile/v0/api.js"),
            Some("Cloudflare Turnstile")
        );
        assert_eq!(
            human_check_vendor("https://www.google.com/recaptcha/api.js"),
            Some("reCAPTCHA")
        );
        assert_eq!(
            human_check_vendor("https://www.google.com/search?q=recaptcha"),
            None
        );
        assert_eq!(human_check_vendor("https://cdn.example.com/app.js"), None);
        assert_eq!(
            human_check_vendor("https://notsentinel.example.com/x.js"),
            None
        );
        assert_eq!(
            human_check_vendor("https://example.com/sentinel/app.js"),
            None
        );
        assert_eq!(
            human_check_vendor("https://chatgpt.com/sentinel/abc/sdk.js"),
            Some("OpenAI Sentinel")
        );
    }

    #[test]
    fn a_no_change_click_that_loaded_a_human_check_is_blocked() {
        let entries = vec![
            serde_json::json!({"url": "https://platform.openai.com/sentinel/abc/sdk.js", "type": "script"}),
        ];
        let v = human_check_verdict(false, &entries).unwrap();
        assert_eq!(v["verdict"], "blocked_by_human_check");
        assert_eq!(v["vendor"], "OpenAI Sentinel");
        let hint = v["hint"].as_str().unwrap();
        assert!(hint.contains("does not prove a person is required"));
        assert!(hint.contains("core/captcha"));
        assert!(!hint.contains("waiting for a person"));
        // A click that visibly did something is not blocked.
        assert!(human_check_verdict(true, &entries).is_none());
        // Nothing human-check related: no verdict.
        let plain =
            vec![serde_json::json!({"url": "https://cdn.example.com/a.js", "type": "script"})];
        assert!(human_check_verdict(false, &plain).is_none());
    }

    /// Run the resources-since script under node against a stubbed
    /// `performance` holding `n` entries for document `origin`.
    fn run_since(mark: super::ResourceMark, origin: f64, n: usize) -> Option<usize> {
        let node = std::env::var_os("PATH").and_then(|p| {
            std::env::split_paths(&p)
                .map(|d| d.join("node"))
                .find(|n| n.is_file())
        })?;
        let js = format!(
            "globalThis.performance = {{ timeOrigin: {origin}, getEntriesByType: () => \
             Array.from({{length: {n}}}, (_, i) => ({{ name: 'https://x.test/' + i, \
             initiatorType: 'script', transferSize: 1 }})) }}; \
             console.log({}.length)",
            super::resources_since_js(mark)
        );
        let out = std::process::Command::new(node)
            .arg("-e")
            .arg(js)
            .output()
            .ok()?;
        String::from_utf8_lossy(&out.stdout).trim().parse().ok()
    }

    #[test]
    fn a_new_document_counts_from_zero_whatever_its_length() {
        let mark = super::ResourceMark {
            origin: 1000.5,
            count: 5,
        };
        let Some(same_doc) = run_since(mark, 1000.5, 8) else {
            return;
        };
        assert_eq!(same_doc, 3, "same document: only the 3 new entries");
        assert_eq!(
            run_since(mark, 2000.25, 5),
            Some(5),
            "new document, equal count"
        );
        assert_eq!(
            run_since(mark, 2000.25, 9),
            Some(9),
            "new document, more entries"
        );
    }

    #[test]
    fn resource_lines_put_scripts_first_and_cap() {
        let entries: Vec<serde_json::Value> = (0..15)
            .map(
                |i| serde_json::json!({"url": format!("https://x.test/img{i}.png"), "type": "img"}),
            )
            .chain(std::iter::once(
                serde_json::json!({"url": "https://x.test/app.js", "type": "script"}),
            ))
            .collect();
        let (lines, total) = resource_lines(&entries, 10);
        assert_eq!(total, 16);
        assert_eq!(lines.len(), 10);
        assert_eq!(lines[0], "script https://x.test/app.js");
    }

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

/// How many resource-timing entries the page has: the mark the post-action
/// read counts from (#378). Read from `performance`, so it works with the
/// Network domain off (the stealth default) and on the relay.
pub(super) const RESOURCE_MARK_JS: &str = "(() => { try { return { origin: performance.timeOrigin, count: performance.getEntriesByType('resource').length } } catch (e) { return { origin: 0, count: 0 } } })()";

/// The mark: which document (`performance.timeOrigin`) and how many entries
/// it had. Resource timing is per document, so a navigation starts a new
/// buffer whose length says nothing about the old one.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(super) struct ResourceMark {
    pub origin: f64,
    pub count: u64,
}

impl ResourceMark {
    pub fn from_value(v: &serde_json::Value) -> Self {
        Self {
            origin: v.get("origin").and_then(|o| o.as_f64()).unwrap_or(0.0),
            count: v.get("count").and_then(|c| c.as_u64()).unwrap_or(0),
        }
    }
}

/// Resources the page fetched since `mark`: `[{url, type, bytes}]`. Another
/// document (a different `timeOrigin`) means everything in it is new, whatever
/// its entry count.
pub(super) fn resources_since_js(mark: ResourceMark) -> String {
    let ResourceMark { origin, count } = mark;
    format!(
        "(() => {{ try {{ const all = performance.getEntriesByType('resource'); \
         const same = performance.timeOrigin === {origin}; const from = same && all.length >= {count} ? {count} : 0; \
         return all.slice(from).map(e => ({{ url: e.name, type: e.initiatorType, bytes: e.transferSize || 0 }})); \
         }} catch (e) {{ return [] }} }})()"
    )
}

/// Resource lines for the observation, script and fetch first, capped.
pub(super) fn resource_lines(entries: &[serde_json::Value], cap: usize) -> (Vec<String>, usize) {
    let rank = |t: &str| match t {
        "script" => 0,
        "fetch" | "xmlhttprequest" | "beacon" => 1,
        "iframe" | "frame" => 2,
        _ => 3,
    };
    let mut rows: Vec<(usize, String)> = entries
        .iter()
        .filter_map(|e| {
            let url = e.get("url")?.as_str()?;
            let kind = e.get("type").and_then(|v| v.as_str()).unwrap_or("other");
            Some((rank(kind), format!("{kind} {}", shorten(url, 120))))
        })
        .collect();
    rows.sort_by_key(|(r, _)| *r);
    let total = rows.len();
    (rows.into_iter().take(cap).map(|(_, l)| l).collect(), total)
}

/// A sign-in page that refused this browser outright (#387): Google's "This
/// browser or app may not be secure". Recognized by URL only and reported so
/// the agent hands the sign-in to the user; never worked around.
pub(crate) fn signin_rejection(url: &str) -> Option<serde_json::Value> {
    let parsed = url::Url::parse(url).ok()?;
    let host = parsed.host_str()?.to_ascii_lowercase();
    let path = parsed.path().to_ascii_lowercase();
    let vendor = if host == "accounts.google.com" && path.contains("/signin/rejected") {
        "Google"
    } else {
        return None;
    };
    Some(serde_json::json!({
        "verdict": "blocked_by_signin_rejection",
        "vendor": vendor,
        "url": shorten(url, 120),
        "hint": "the sign-in page refused this browser (\"this browser or app may not be \
                 secure\"). Do not retry or look for a way around it: the user signs in in \
                 their own Chrome (relay mode), or hand off with `session handoff`.",
    }))
}

/// Known human-check / anti-automation vendors, by a URL they load (#377).
/// Reports loaded resources; recognition does not establish solvability.
pub(super) fn human_check_vendor(url: &str) -> Option<&'static str> {
    let u = url.to_ascii_lowercase();
    let host = u
        .split("://")
        .nth(1)
        .and_then(|rest| rest.split('/').next())
        .unwrap_or("");
    let path = u
        .split("://")
        .nth(1)
        .and_then(|r| r.find('/').map(|i| &r[i..]))
        .unwrap_or("");
    let on = |d: &str| host == d || host.ends_with(&format!(".{d}"));
    // OpenAI serves Sentinel under /sentinel/ on its own hosts only; the same
    // path elsewhere is somebody else's script.
    let openai = on("openai.com") || on("chatgpt.com") || on("oaistatic.com");
    if (openai && path.starts_with("/sentinel/")) || on("sentinel.openai.com") {
        Some("OpenAI Sentinel")
    } else if on("hcaptcha.com") {
        Some("hCaptcha")
    } else if on("challenges.cloudflare.com") {
        Some("Cloudflare Turnstile")
    } else if on("recaptcha.net")
        || ((on("google.com") || on("gstatic.com")) && path.contains("/recaptcha/"))
    {
        Some("reCAPTCHA")
    } else if on("arkoselabs.com") || on("funcaptcha.com") {
        Some("Arkose")
    } else if on("captcha-delivery.com") {
        Some("DataDome")
    } else if on("px-cdn.net") || on("px-cloud.net") || on("perimeterx.net") {
        Some("HUMAN (PerimeterX)")
    } else if on("geetest.com") {
        Some("GeeTest")
    } else {
        None
    }
}

/// When an action changed nothing on the page but a human-check script loaded
/// during it, report the vendor as a diagnostic lead (#377). The script load
/// does not establish that personal presence is required. Inspect the visible
/// challenge before deciding whether ordinary interaction or handoff is needed.
pub(super) fn human_check_verdict(
    changed: bool,
    entries: &[serde_json::Value],
) -> Option<serde_json::Value> {
    if changed {
        return None;
    }
    let (vendor, url) = entries.iter().find_map(|e| {
        let url = e.get("url")?.as_str()?;
        human_check_vendor(url).map(|v| (v, url))
    })?;
    Some(serde_json::json!({
        "verdict": "blocked_by_human_check",
        "vendor": vendor,
        "url": shorten(url, 120),
        "hint": "the action loaded this human-check script and the page did not change. \
                 This does not prove a person is required. Do not repeat the original submit. \
                 Inspect the visible challenge; for an authorized task, load `core/captcha`, \
                 try supported ordinary interactions, and verify the result. Hand off only \
                 if attempts fail or personal presence is required.",
    }))
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

    #[test]
    fn google_signin_rejection_is_recognized() {
        let v = signin_rejection(
            "https://accounts.google.com/v3/signin/rejected?continue=x&flowName=GlifWebSignIn",
        )
        .unwrap();
        assert_eq!(v["verdict"], "blocked_by_signin_rejection");
        assert_eq!(v["vendor"], "Google");
        assert!(signin_rejection("https://accounts.google.com/v3/signin/identifier").is_none());
        assert!(signin_rejection("https://evil.example/signin/rejected").is_none());
    }
}
