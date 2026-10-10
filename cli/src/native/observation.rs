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

/// A post-action tree at or under both limits is returned whole, beside a
/// compact list of what changed: on a page this small the caller otherwise
/// spends a round trip on `snapshot` just to read the receipt in context.
pub(super) const SMALL_TREE_BYTES: usize = 4096;
pub(super) const SMALL_TREE_LINES: usize = 60;
/// Cap on the compact change list that rides with a small tree. The tree is
/// bounded, but a page that shrank to a small one can drop many lines.
const MAX_CHANGE_LINES: usize = 60;

/// Whether the post-action tree is small enough to return whole.
pub(super) fn small_tree(after: &str) -> bool {
    let after = after.trim_end();
    after.len() <= SMALL_TREE_BYTES && after.lines().count() <= SMALL_TREE_LINES
}

/// The added and removed lines alone, `+ ` / `- ` prefixed, in diff order:
/// no context lines and no hunk headers, since the whole tree rides beside
/// them. Returns the lines kept and how many were left out by the cap.
pub(super) fn compact_changes(before: &str, after: &str) -> (Vec<String>, usize) {
    use similar::{ChangeTag, TextDiff};
    let mut lines = Vec::new();
    let mut omitted = 0;
    for change in TextDiff::from_lines(before, after).iter_all_changes() {
        let sign = match change.tag() {
            ChangeTag::Insert => '+',
            ChangeTag::Delete => '-',
            ChangeTag::Equal => continue,
        };
        if lines.len() == MAX_CHANGE_LINES {
            omitted += 1;
            continue;
        }
        lines.push(format!("{sign} {}", change.value().trim_end_matches('\n')));
    }
    (lines, omitted)
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
            let before_text = format!("{}\n", before.trim_end());
            let after_text = format!("{}\n", after.trim_end());
            let delta = super::diff::diff_snapshots(&before_text, &after_text);
            changed = delta.changed;
            let replaced = delta.changed && page_replaced(&delta);
            if !replaced && small_tree(after) {
                // Both trees were captured, so this is the whole current
                // tree, the same capture the delta came from (refs already
                // registered). Whether the observation as a whole is
                // complete is still `status`, below.
                out.insert("snapshot".into(), json!(after.trim_end()));
                if delta.changed {
                    let (lines, omitted) = compact_changes(&before_text, &after_text);
                    out.insert("changes".into(), json!(lines));
                    if omitted > 0 {
                        out.insert("changesOmitted".into(), json!(omitted));
                    }
                    out.insert("added".into(), json!(delta.additions));
                    out.insert("removed".into(), json!(delta.removals));
                }
            } else if delta.changed {
                if replaced {
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

/// The visible text an `--observe` compares, read in every frame. Interactive
/// snapshots leave out plain static text, so a receipt rendered as a `<p>` (in
/// an iframe, a log line, a "Page 2 of 3" counter) never reaches the tree
/// delta and the action reads as "no change".
///
/// `innerText` never includes what was typed into an `<input>` or
/// `<textarea>`; lines that are the content of an editable region are dropped
/// as well, and so is any line carrying a password field's current value. Text
/// the page itself renders (a receipt echoing a note) is page text and stays.
pub(super) const OBSERVE_TEXT_JS: &str = "(function(){try{\
var b=document.body||document.documentElement;if(!b)return '';\
var t=b.innerText||'';var drop=new Set();\
document.querySelectorAll('[contenteditable]:not([contenteditable=false]),textarea').forEach(function(e){\
String(e.innerText||e.textContent||'').split('\\n').forEach(function(l){l=l.trim();if(l)drop.add(l)})});\
var pw=[];document.querySelectorAll('input').forEach(function(i){if(i.type==='password'&&i.value)pw.push(i.value)});\
return t.split('\\n').filter(function(l){var s=l.trim();if(!s||drop.has(s))return false;\
for(var k=0;k<pw.length;k++){if(s.indexOf(pw[k])>=0)return false}return true}).join('\\n');\
}catch(e){return ''}})()";

/// Bounds on `observed.text`: lines, total bytes, bytes per line.
pub(super) const MAX_TEXT_LINES: usize = 20;
pub(super) const MAX_TEXT_BYTES: usize = 1024;
const MAX_TEXT_LINE_BYTES: usize = 200;

/// One frame's visible text, keyed by frame id so a frame is compared with
/// itself before and after the action.
#[derive(Clone, Debug)]
pub(super) struct FrameLines {
    pub id: String,
    /// `None` for the top frame; otherwise a short name for the frame (the
    /// last segment of its url), printed in front of its lines.
    pub label: Option<String>,
    pub lines: Vec<String>,
}

fn visible_lines(text: &str) -> Vec<String> {
    text.lines()
        .map(|l| {
            l.split_whitespace()
                .collect::<Vec<_>>()
                .join(" ")
                .chars()
                .map(|c| if c.is_control() { ' ' } else { c })
                .collect::<String>()
        })
        .filter(|l| !l.is_empty())
        .collect()
}

fn frame_label(url: &str) -> String {
    let trimmed = url.split(['?', '#']).next().unwrap_or(url);
    let name = trimmed
        .trim_end_matches('/')
        .rsplit('/')
        .next()
        .filter(|s| !s.is_empty())
        .unwrap_or(trimmed);
    shorten(name, 40)
}

pub(super) fn frame_lines(frame_id: &str, url: &str, top: bool, text: &str) -> FrameLines {
    FrameLines {
        id: frame_id.to_string(),
        label: (!top).then(|| frame_label(url)),
        lines: visible_lines(text),
    }
}

/// Read the visible text of every frame in the active page.
pub(super) async fn capture_text(
    state: &super::actions::DaemonState,
) -> Result<Vec<FrameLines>, String> {
    let mgr = state
        .browser
        .as_ref()
        .ok_or("No active page for observation")?;
    let session = mgr
        .active_session_id()
        .map_err(|_| "No active page for observation".to_string())?
        .to_string();
    let read = super::element::collect_all_frames_text_with(
        &mgr.client,
        &session,
        &state.iframe_sessions,
        OBSERVE_TEXT_JS,
    );
    let frames = tokio::time::timeout(std::time::Duration::from_secs(3), read)
        .await
        .map_err(|_| "text capture timed out".to_string())??;
    Ok(frames
        .iter()
        .map(|f| frame_lines(&f.frame_id, &f.url, f.kind == "top", &f.text))
        .collect())
}

/// The visible-text lines that appeared or went away, `+ ` / `- ` prefixed,
/// frame by frame, bounded to [`MAX_TEXT_LINES`] lines and [`MAX_TEXT_BYTES`]
/// bytes. Returns the kept lines and how many were left out.
pub(super) fn text_changes(before: &[FrameLines], after: &[FrameLines]) -> (Vec<String>, usize) {
    use similar::{capture_diff_slices_deadline, Algorithm, DiffOp};
    // A text-heavy page diffs in well under this; the deadline only keeps a
    // pathological one from holding the reply.
    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(200);
    let mut all: Vec<String> = Vec::new();
    let empty: Vec<String> = Vec::new();
    let mut emit = |label: &Option<String>, sign: char, line: &str| {
        let prefix = label
            .as_ref()
            .map(|l| format!("[frame {l}] "))
            .unwrap_or_default();
        all.push(shorten(
            &format!("{sign} {prefix}{line}"),
            MAX_TEXT_LINE_BYTES,
        ));
    };
    for frame in after {
        let old = before
            .iter()
            .find(|b| b.id == frame.id)
            .map(|b| &b.lines)
            .unwrap_or(&empty);
        for op in capture_diff_slices_deadline(Algorithm::Myers, old, &frame.lines, Some(deadline))
        {
            match op {
                DiffOp::Equal { .. } => {}
                DiffOp::Delete {
                    old_index, old_len, ..
                } => old[old_index..old_index + old_len]
                    .iter()
                    .for_each(|l| emit(&frame.label, '-', l)),
                DiffOp::Insert {
                    new_index, new_len, ..
                } => frame.lines[new_index..new_index + new_len]
                    .iter()
                    .for_each(|l| emit(&frame.label, '+', l)),
                DiffOp::Replace {
                    old_index,
                    old_len,
                    new_index,
                    new_len,
                } => {
                    old[old_index..old_index + old_len]
                        .iter()
                        .for_each(|l| emit(&frame.label, '-', l));
                    frame.lines[new_index..new_index + new_len]
                        .iter()
                        .for_each(|l| emit(&frame.label, '+', l));
                }
            }
        }
    }
    // A frame the action removed: its text went with it.
    for frame in before
        .iter()
        .filter(|b| !after.iter().any(|a| a.id == b.id))
    {
        frame.lines.iter().for_each(|l| emit(&frame.label, '-', l));
    }
    let mut kept = Vec::new();
    let mut bytes = 0;
    let mut omitted = 0;
    for line in all {
        if kept.len() == MAX_TEXT_LINES || bytes + line.len() + 1 > MAX_TEXT_BYTES {
            omitted += 1;
            continue;
        }
        bytes += line.len() + 1;
        kept.push(line);
    }
    (kept, omitted)
}

/// Add the visible-text change to an observation. A text change is a change:
/// a receipt that only exists as static text must not leave `changed:false`.
/// A capture that failed on either side is reported, never read as "no text
/// changed".
pub(super) fn apply_text(
    observed: &mut serde_json::Map<String, serde_json::Value>,
    before: &Result<Vec<FrameLines>, String>,
    after: &Result<Vec<FrameLines>, String>,
) {
    use serde_json::json;
    match (before, after) {
        (Ok(before), Ok(after)) => {
            let (lines, omitted) = text_changes(before, after);
            if lines.is_empty() {
                return;
            }
            observed.insert("text".into(), json!(lines));
            if omitted > 0 {
                observed.insert("textOmitted".into(), json!(omitted));
            }
            observed.insert("changed".into(), json!(true));
        }
        (before, after) => {
            let error = before
                .as_ref()
                .err()
                .or(after.as_ref().err())
                .cloned()
                .unwrap_or_default();
            observed.insert("textStatus".into(), json!("unavailable"));
            observed.insert("textError".into(), json!(shorten(&error, 160)));
        }
    }
}

/// Whether an observation must name its target (#237): the first one in the
/// session, a target or url other than the one the last observation reported,
/// or an action that itself moved the session. Leaving an unchanged target out
/// never hides a switch: every change since the last report is news here.
pub(super) fn target_is_news(
    last: Option<&(String, String, String)>,
    current: &(String, String, String),
    moved_during_action: bool,
) -> bool {
    moved_during_action || last != Some(current)
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
    fn an_in_page_change_on_a_large_page_still_returns_a_delta() {
        // One row re-sorted on an 80-line page is a delta, not a replacement,
        // and the page is past the small-tree line limit.
        let before = tree("row", 80);
        let mut lines: Vec<&str> = before.lines().collect();
        lines.swap(3, 30);
        let after = lines.join("\n");
        let out = changes(&ok(&before), &ok(&after), &ok("u"), &ok("u"));
        assert_eq!(out["changed"], true);
        assert!(out.contains_key("delta"));
        assert!(!out.contains_key("replaced"));
        assert!(!out.contains_key("snapshot"));
        assert!(!out.contains_key("changes"));
    }

    #[test]
    fn small_trees_are_never_reported_as_replaced() {
        // A dialog swapping for another dialog: a small tree, returned whole
        // with the compact changes, never flagged as a page replacement.
        let out = changes(
            &ok("- button \"OK\" [ref=e1]"),
            &ok("- button \"Done\" [ref=e2]"),
            &ok("u"),
            &ok("u"),
        );
        assert!(!out.contains_key("replaced"));
        assert!(!out.contains_key("delta"));
        assert_eq!(out["snapshot"], "- button \"Done\" [ref=e2]");
        assert_eq!(
            out["changes"],
            json!(["- - button \"OK\" [ref=e1]", "+ - button \"Done\" [ref=e2]"])
        );
    }

    #[test]
    fn a_small_after_tree_comes_whole_with_only_the_changed_lines() {
        let before = "- heading \"Kit catalog\" [level=1, ref=e1]\n\
                      - button \"Select Cedar kit\" [ref=e5]\n\
                      - button \"Previous page\" [disabled, ref=e2]\n\
                      - button \"Next page\" [ref=e3]";
        let after = "- heading \"Kit catalog\" [level=1, ref=e1]\n\
                     - button \"Select Birch kit\" [ref=e10]\n\
                     - button \"Previous page\" [ref=e2]\n\
                     - button \"Next page\" [ref=e3]";
        let out = changes(&ok(before), &ok(after), &ok("u"), &ok("u"));
        assert_eq!(out["status"], "complete");
        assert_eq!(out["changed"], true);
        assert_eq!(out["snapshot"], after);
        let lines: Vec<&str> = out["changes"]
            .as_array()
            .unwrap()
            .iter()
            .map(|l| l.as_str().unwrap())
            .collect();
        // No context lines: the unchanged heading and Next button are only in
        // the tree, never in the change list.
        assert_eq!(lines.len(), 4, "{lines:?}");
        assert!(lines
            .iter()
            .all(|l| l.starts_with("+ ") || l.starts_with("- ")));
        assert!(!lines
            .iter()
            .any(|l| l.contains("Kit catalog") || l.contains("Next page")));
        assert!(!out.contains_key("delta"));
        assert_eq!(out["added"], 2);
        assert_eq!(out["removed"], 2);
    }

    #[test]
    fn a_small_unchanged_tree_still_comes_whole() {
        let out = changes(&ok("button Save"), &ok("button Save"), &ok("u"), &ok("u"));
        assert_eq!(out["changed"], false);
        assert_eq!(out["status"], "complete");
        assert_eq!(out["snapshot"], "button Save");
        assert!(!out.contains_key("changes"));
    }

    #[test]
    fn the_small_tree_threshold_is_bytes_and_lines() {
        let line = "- link \"x\" [ref=e1]";
        let lines = |n: usize| vec![line; n].join("\n");
        assert!(small_tree(&lines(SMALL_TREE_LINES)));
        assert!(!small_tree(&lines(SMALL_TREE_LINES + 1)));
        let wide = "x".repeat(SMALL_TREE_BYTES);
        assert!(small_tree(&wide));
        assert!(!small_tree(&format!("{wide}x")));
        // A tree over the byte limit in few lines keeps the delta.
        let fat = format!("- text \"{}\"", "y".repeat(SMALL_TREE_BYTES));
        let out = changes(&ok("- text \"a\""), &ok(&fat), &ok("u"), &ok("u"));
        assert!(out.contains_key("delta"));
        assert!(!out.contains_key("snapshot"));
    }

    #[test]
    fn a_partial_capture_with_a_small_tree_stays_partial() {
        // The tree is real, but the url read failed: the observation says so.
        let out = changes(&ok("button Save"), &ok("button Next"), &ok("u"), &missing());
        assert_eq!(out["status"], "partial");
        assert_eq!(out["snapshot"], "button Next");
        assert_eq!(out["changed"], true);
        assert!(out.contains_key("errors"));
        // An unavailable after-tree never yields a snapshot.
        let out = changes(&ok("button Save"), &missing(), &ok("u"), &ok("u"));
        assert_eq!(out["status"], "unavailable");
        assert!(!out.contains_key("snapshot"));
        assert!(!out.contains_key("changes"));
    }

    #[test]
    fn a_shrinking_page_caps_the_change_list() {
        let before = tree("row", 200);
        let after = "- button \"Done\" [ref=e999]";
        let (lines, omitted) = compact_changes(&format!("{before}\n"), &format!("{after}\n"));
        assert_eq!(lines.len(), MAX_CHANGE_LINES);
        assert_eq!(omitted, 201 - MAX_CHANGE_LINES);
    }

    #[test]
    fn page_replacement_needs_twenty_lines_on_each_side() {
        let before = tree("old", 19);
        let after = tree("new", 19);
        let out = changes(&ok(&before), &ok(&after), &ok("u"), &ok("u"));
        assert!(!out.contains_key("replaced"));
        assert_eq!(out["snapshot"], after);
        let before = tree("old", 20);
        let after = tree("new", 20);
        let out = changes(&ok(&before), &ok(&after), &ok("u"), &ok("u"));
        assert_eq!(out["replaced"], true);
        assert!(!out.contains_key("changes"));
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

    fn frames(spec: &[(&str, &str, bool, &str)]) -> Result<Vec<FrameLines>, String> {
        Ok(spec
            .iter()
            .map(|(id, url, top, text)| frame_lines(id, url, *top, text))
            .collect())
    }

    #[test]
    fn an_iframe_receipt_is_a_text_change_even_when_the_tree_did_not_move() {
        let before = frames(&[
            (
                "T",
                "http://x/iframe.html",
                true,
                "Embedded notes\nComplete the form",
            ),
            ("C", "http://x/child.html", false, "Note Save note\nNo note"),
        ]);
        let after = frames(&[
            (
                "T",
                "http://x/iframe.html",
                true,
                "Embedded notes\nComplete the form",
            ),
            (
                "C",
                "http://x/child.html",
                false,
                "Note Save note\nNote saved: Synthetic benchmark note",
            ),
        ]);
        let mut observed = changes(&ok("tree"), &ok("tree"), &ok("u"), &ok("u"));
        assert_eq!(observed["changed"], false);
        apply_text(&mut observed, &before, &after);
        assert_eq!(observed["changed"], true);
        assert_eq!(
            observed["text"],
            json!([
                "- [frame child.html] No note",
                "+ [frame child.html] Note saved: Synthetic benchmark note"
            ])
        );
        assert_eq!(observed["status"], "complete");
    }

    #[test]
    fn unchanged_text_adds_nothing() {
        let same = frames(&[("T", "u", true, "Page 1 of 3\n  Kit   catalog ")]);
        let mut observed = changes(&ok("tree"), &ok("tree"), &ok("u"), &ok("u"));
        apply_text(&mut observed, &same, &same);
        assert_eq!(observed["changed"], false);
        assert!(!observed.contains_key("text"));
        assert!(!observed.contains_key("textStatus"));
    }

    #[test]
    fn a_failed_text_capture_is_reported_not_read_as_unchanged() {
        let mut observed = changes(&ok("tree"), &ok("tree"), &ok("u"), &ok("u"));
        apply_text(
            &mut observed,
            &frames(&[("T", "u", true, "a")]),
            &Err("text capture timed out".into()),
        );
        assert_eq!(observed["textStatus"], "unavailable");
        assert!(!observed.contains_key("text"));
        // The tree observation itself stays what it was.
        assert_eq!(observed["status"], "complete");
        assert_eq!(observed["changed"], false);
    }

    #[test]
    fn text_changes_are_bounded_in_lines_and_bytes() {
        let many: String = (0..100).map(|i| format!("row {i}\n")).collect();
        let (lines, omitted) = text_changes(
            &frames(&[("T", "u", true, "")]).unwrap(),
            &frames(&[("T", "u", true, &many)]).unwrap(),
        );
        assert_eq!(lines.len(), MAX_TEXT_LINES);
        assert_eq!(omitted, 100 - MAX_TEXT_LINES);

        let wide: String = (0..30)
            .map(|i| format!("{i} {}\n", "w".repeat(300)))
            .collect();
        let (lines, omitted) = text_changes(
            &frames(&[("T", "u", true, "")]).unwrap(),
            &frames(&[("T", "u", true, &wide)]).unwrap(),
        );
        let bytes: usize = lines.iter().map(|l| l.len() + 1).sum();
        assert!(bytes <= MAX_TEXT_BYTES, "{bytes}");
        assert!(lines.iter().all(|l| l.len() <= MAX_TEXT_LINE_BYTES));
        assert_eq!(lines.len() + omitted, 30);
        assert!(omitted > 0);
    }

    #[test]
    fn a_new_frame_counts_as_added_text_and_a_gone_frame_as_removed() {
        let (lines, _) = text_changes(
            &frames(&[
                ("T", "u", true, "a"),
                ("old", "http://x/a.html?q=1", false, "bye"),
            ])
            .unwrap(),
            &frames(&[("T", "u", true, "a"), ("new", "http://x/b/", false, "hi")]).unwrap(),
        );
        assert_eq!(lines, ["+ [frame b] hi", "- [frame a.html] bye"]);
    }

    #[test]
    fn page_counters_and_delayed_totals_show_as_text() {
        let (lines, _) = text_changes(
            &frames(&[("T", "u", true, "Previous pagePage 1 of 3Next page\nIdle")]).unwrap(),
            &frames(&[(
                "T",
                "u",
                true,
                "Previous pagePage 2 of 3Next page\nReport ready\nTotal entries: 3",
            )])
            .unwrap(),
        );
        assert!(lines.contains(&"+ Previous pagePage 2 of 3Next page".to_string()));
        assert!(lines.contains(&"+ Total entries: 3".to_string()));
        assert!(lines.contains(&"- Idle".to_string()));
    }

    /// Run the observe text reader under node against a stubbed document.
    fn run_text_js(body: &str, editables: &[&str], inputs: &[(&str, &str)]) -> Option<String> {
        let node = std::env::var_os("PATH").and_then(|p| {
            std::env::split_paths(&p)
                .map(|d| d.join("node"))
                .find(|n| n.is_file())
        })?;
        let js = format!(
            "const body = {{ innerText: {body} }}; \
             const editables = {editables}.map(t => ({{ innerText: t }})); \
             const inputs = {inputs}.map(([type, value]) => ({{ type, value }})); \
             globalThis.document = {{ body, querySelectorAll: s => s === 'input' ? inputs : editables }}; \
             process.stdout.write({})",
            OBSERVE_TEXT_JS,
            body = serde_json::to_string(body).unwrap(),
            editables = serde_json::to_string(editables).unwrap(),
            inputs = serde_json::to_string(
                &inputs.iter().map(|(t, v)| [*t, *v]).collect::<Vec<_>>()
            )
            .unwrap(),
        );
        let out = std::process::Command::new(node)
            .arg("-e")
            .arg(js)
            .output()
            .ok()?;
        out.status
            .success()
            .then(|| String::from_utf8_lossy(&out.stdout).into_owned())
    }

    #[test]
    fn typed_field_content_and_passwords_never_reach_the_text() {
        let Some(out) = run_text_js(
            "Sign in\nhunter2-secret shown by mistake\nDraft: my private draft\nNote saved: hello",
            &["Draft: my private draft"],
            &[("password", "hunter2-secret"), ("text", "hello")],
        ) else {
            return;
        };
        assert!(!out.contains("hunter2-secret"), "{out}");
        assert!(!out.contains("my private draft"), "{out}");
        // Page text that echoes an ordinary field is the page's receipt.
        assert!(out.contains("Note saved: hello"), "{out}");
        assert!(out.contains("Sign in"), "{out}");
    }

    #[test]
    fn the_target_is_named_whenever_it_is_news() {
        let t = |tab: &str, target: &str, url: &str| {
            (tab.to_string(), target.to_string(), url.to_string())
        };
        let a = t("t1", "AAA", "http://x/a");
        // First observation in the session.
        assert!(target_is_news(None, &a, false));
        // Same tab, target and url as last reported: left out.
        assert!(!target_is_news(Some(&a), &a, false));
        // A rebinding (new targetId), another tab, a new url: named.
        assert!(target_is_news(
            Some(&a),
            &t("t1", "BBB", "http://x/a"),
            false
        ));
        assert!(target_is_news(
            Some(&a),
            &t("t2", "AAA", "http://x/a"),
            false
        ));
        assert!(target_is_news(
            Some(&a),
            &t("t1", "AAA", "http://x/b"),
            false
        ));
        // The action moved the session (e.g. a followed popup) back onto the
        // tab last reported: still named, since the caller's baseline was
        // another tab.
        assert!(target_is_news(Some(&a), &a, true));
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
