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
/// Caps on the compact change list that rides with a small tree. The tree is
/// bounded, but a page that shrank to a small one can drop many lines, and a
/// single removed line can be arbitrarily long.
const MAX_CHANGE_LINES: usize = 60;
const MAX_CHANGE_BYTES: usize = 4096;
const MAX_CHANGE_LINE_BYTES: usize = 240;

/// Whether the post-action tree is small enough to return whole.
pub(super) fn small_tree(after: &str) -> bool {
    let after = after.trim_end();
    after.len() <= SMALL_TREE_BYTES && after.lines().count() <= SMALL_TREE_LINES
}

/// The compact change list and what its budget left out.
#[derive(Debug, Default)]
pub(super) struct CompactChanges {
    pub lines: Vec<String>,
    /// Changed lines not listed: over the line or byte budget.
    pub omitted: usize,
    /// Listed lines cut to [`MAX_CHANGE_LINE_BYTES`].
    pub shortened: usize,
}

/// The added and removed lines alone, `+ ` / `- ` prefixed, in diff order:
/// no context lines and no hunk headers, since the whole tree rides beside
/// them. At most [`MAX_CHANGE_LINES`] lines and [`MAX_CHANGE_BYTES`] UTF-8
/// bytes in total (one byte per line counted for its separator); each line at
/// most [`MAX_CHANGE_LINE_BYTES`].
pub(super) fn compact_changes(before: &str, after: &str) -> CompactChanges {
    use similar::{ChangeTag, TextDiff};
    let mut out = CompactChanges::default();
    let mut bytes = 0;
    for change in TextDiff::from_lines(before, after).iter_all_changes() {
        let sign = match change.tag() {
            ChangeTag::Insert => '+',
            ChangeTag::Delete => '-',
            ChangeTag::Equal => continue,
        };
        if out.lines.len() == MAX_CHANGE_LINES || bytes >= MAX_CHANGE_BYTES {
            out.omitted += 1;
            continue;
        }
        let value = change.value().trim_end_matches('\n');
        let (line, cut) = bounded_line(sign, "", value, MAX_CHANGE_LINE_BYTES);
        if bytes + line.len() + 1 > MAX_CHANGE_BYTES {
            out.omitted += 1;
            continue;
        }
        bytes += line.len() + 1;
        out.shortened += usize::from(cut);
        out.lines.push(line);
    }
    out
}

/// `"{sign} {prefix}{value}"` in at most `limit` bytes. A longer value keeps
/// its head and says how many bytes were cut; only the kept head is copied.
fn bounded_line(sign: char, prefix: &str, value: &str, limit: usize) -> (String, bool) {
    // " [truncated; <up to 20 digits> bytes omitted]" is at most 48 bytes.
    const SUFFIX_ROOM: usize = 48;
    let fixed = 2 + prefix.len();
    if fixed + value.len() <= limit {
        return (format!("{sign} {prefix}{value}"), false);
    }
    let kept = head(value, limit.saturating_sub(fixed + SUFFIX_ROOM));
    (
        format!(
            "{sign} {prefix}{kept} [truncated; {} bytes omitted]",
            value.len() - kept.len()
        ),
        true,
    )
}

/// The first `limit` bytes of `value`, cut on a character boundary, without
/// copying the rest: a removed line can be megabytes long.
fn head(value: &str, limit: usize) -> &str {
    if value.len() <= limit {
        return value;
    }
    let mut end = limit;
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    &value[..end]
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
                    let compact = compact_changes(&before_text, &after_text);
                    out.insert("changes".into(), json!(compact.lines));
                    if compact.omitted > 0 {
                        out.insert("changesOmitted".into(), json!(compact.omitted));
                    }
                    if compact.shortened > 0 {
                        out.insert("changesShortened".into(), json!(compact.shortened));
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
/// It walks the rendered DOM itself instead of reading `innerText`, so what a
/// person typed is excluded by structure, not by matching strings afterwards:
/// `<input>`, `<textarea>`, `<select>` and every element that is editable
/// (`isContentEditable`: the editable host, everything inside it, and the
/// whole document under `designMode`) are skipped with their subtrees. Open
/// shadow roots are walked through their slots; frames are read separately.
///
/// It also reports whether any password field in the document (open shadow
/// roots included) holds a value, and whether that scan finished. The values
/// themselves never leave the page: when any frame on either side of the
/// action has one, or could not be scanned, no text lines are returned at all
/// (see [`text_diff`]), since a page may copy a password into any frame's text.
///
/// The scan and the walk share one node budget and one deadline
/// (`__DEADLINE_MS__`, filled in per read). Errors are not caught: an
/// exception reaches the caller as one, never as an empty page.
const OBSERVE_TEXT_JS: &str = r#"(function(){
var DEADLINE=Date.now()+__DEADLINE_MS__,LIMIT=131072,MAX_NODES=100000,out=[],len=0,nodes=0,truncated=false,scanned=true,pw=[];
var SKIP={SCRIPT:1,STYLE:1,NOSCRIPT:1,TEMPLATE:1,TEXTAREA:1,INPUT:1,SELECT:1,OPTION:1,OPTGROUP:1,DATALIST:1,IFRAME:1,FRAME:1,OBJECT:1,EMBED:1,HEAD:1};
function spent(){nodes++;return nodes>MAX_NODES||((nodes&255)===0&&Date.now()>DEADLINE)}
function push(s){if(truncated)return;if(len+s.length>LIMIT){truncated=true;return}out.push(s);len+=s.length}
function scan(root){var w=document.createTreeWalker(root,NodeFilter.SHOW_ELEMENT),n;while((n=w.nextNode())){if(spent()){scanned=false;return}if(n.tagName==='INPUT'&&n.type==='password'&&n.value)pw.push(n.value);if(n.shadowRoot){scan(n.shadowRoot);if(!scanned)return}}}
function kids(list,vis){for(var i=0;i<list.length&&!truncated;i++)walk(list[i],vis)}
function walk(node,vis){
if(truncated)return;
if(spent()){truncated=true;return}
if(node.nodeType===3){if(vis){var t=node.data.replace(/\s+/g,' ');if(t.trim())push(t)}return}
if(node.nodeType===11){kids(node.childNodes,vis);return}
if(node.nodeType!==1)return;
var el=node,tag=el.tagName;
if(SKIP[tag]||el.isContentEditable)return;
if(tag==='BR'){push('\n');return}
var cs=getComputedStyle(el);
if(cs.display==='none')return;
var v=cs.visibility==='visible',d=cs.display,block=d.indexOf('inline')!==0&&d!=='contents',sep=d==='table-cell'?' ':'\n';
if(block)push(sep);
if(tag==='SLOT'){var a=el.assignedNodes({flatten:true});kids(a.length?a:el.childNodes,v)}
else kids((el.shadowRoot||el).childNodes,v);
if(block)push(sep)}
scan(document);
var root=document.body||document.documentElement;
if(scanned&&root&&!(document.designMode==='on'))walk(root,true);
var lines=out.join('').split('\n').map(function(l){return l.replace(/\s+/g,' ').trim()}).filter(function(l){
if(!l)return false;for(var k=0;k<pw.length;k++){if(l.indexOf(pw[k])>=0)return false}return true});
return {text:lines.join('\n'),href:String(location.href),truncated:truncated||!scanned,password:pw.length>0,scanned:scanned}})()"#;

fn observe_text_js(deadline_ms: u64) -> String {
    OBSERVE_TEXT_JS.replace("__DEADLINE_MS__", &deadline_ms.to_string())
}

/// Bounds on `observed.text`: lines, total UTF-8 bytes, bytes per line.
pub(super) const MAX_TEXT_LINES: usize = 20;
pub(super) const MAX_TEXT_BYTES: usize = 1024;
const MAX_TEXT_LINE_BYTES: usize = 200;
/// At most this many frames are read per capture, across every session.
pub(super) const MAX_TEXT_FRAMES: usize = 32;
/// Wall-clock budget for one capture (before, or after, the action): every
/// CDP call in it gets only what is left of this.
pub(super) const TEXT_CAPTURE_BUDGET_MS: u64 = 2500;
/// Per-frame problems listed in `observed.textFrames`.
const MAX_TEXT_FRAME_NOTES: usize = 10;

/// One frame's text, as read.
#[derive(Clone, Debug)]
pub(super) struct FrameText {
    pub lines: Vec<String>,
    /// The reader hit its size, node or time budget: the text is a prefix.
    pub truncated: bool,
    /// A password field in this document holds a value.
    pub password: bool,
    /// The password scan covered the whole document.
    pub scanned: bool,
}

/// Why a frame has no comparable text. `partial` means it was not read (a
/// budget, or it appeared during the read); `unavailable` that reading it
/// failed or gave something other than the frame the tree names.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct FrameProblem {
    pub status: &'static str,
    pub reason: String,
}

impl FrameProblem {
    fn partial(reason: impl Into<String>) -> Self {
        Self {
            status: "partial",
            reason: reason.into(),
        }
    }
    fn unavailable(reason: impl Into<String>) -> Self {
        Self {
            status: "unavailable",
            reason: reason.into(),
        }
    }
}

/// One frame of a visible-text read: which frame (id and the document it
/// held, `loaderId`), and its text or why there is none.
#[derive(Clone, Debug)]
pub(super) struct FrameRead {
    pub id: String,
    pub loader: String,
    /// `None` for the top frame; otherwise a short name for the frame.
    pub label: Option<String>,
    pub text: Result<FrameText, FrameProblem>,
}

/// A whole visible-text read: every frame of the page's final inventory,
/// read or not. `Err` when the top frame tree itself could not be read or
/// validated: then nothing about any frame is known.
pub(super) type TextRead = Result<Vec<FrameRead>, String>;

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

fn without_fragment(url: &str) -> &str {
    url.split('#').next().unwrap_or(url)
}

/// A frame as the frame tree names it.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct TreeFrame {
    pub id: String,
    pub url: String,
    pub loader: String,
    pub top: bool,
}

/// Validate a `Page.getFrameTree` result: every node has a frame with a
/// string id, url and loaderId, and `childFrames`, when present, is a list of
/// nodes. Anything else is an error, never a shorter list of frames.
pub(super) fn validate_frame_tree(result: &serde_json::Value) -> Result<Vec<TreeFrame>, String> {
    fn node(
        value: &serde_json::Value,
        top: bool,
        out: &mut Vec<TreeFrame>,
        depth: usize,
    ) -> Result<(), String> {
        if depth > 64 {
            return Err("frame tree is nested deeper than 64 frames".into());
        }
        let frame = value
            .get("frame")
            .and_then(|f| f.as_object())
            .ok_or("frame tree node has no frame")?;
        let field = |name: &str| {
            frame
                .get(name)
                .and_then(|v| v.as_str())
                .map(str::to_string)
                .ok_or_else(|| format!("frame tree node has no string {name}"))
        };
        out.push(TreeFrame {
            id: field("id")?,
            url: field("url")?,
            loader: field("loaderId")?,
            top,
        });
        match value.get("childFrames") {
            None => Ok(()),
            Some(serde_json::Value::Array(children)) => {
                for child in children {
                    node(child, false, out, depth + 1)?;
                }
                Ok(())
            }
            Some(_) => Err("frame tree childFrames is not a list".into()),
        }
    }
    let root = result
        .get("frameTree")
        .ok_or("frame tree result has no frameTree")?;
    let mut out = Vec::new();
    node(root, true, &mut out, 0)?;
    Ok(out)
}

/// Check one reader result: no exception, an object with string `text` and
/// `href` and boolean `truncated`, `password` and `scanned`, and an `href`
/// that is the document the frame tree names (fragment aside).
pub(super) fn parse_frame_read(
    response: &serde_json::Value,
    expected_url: &str,
) -> Result<FrameText, String> {
    if let Some(ex) = response.get("exceptionDetails") {
        let what = ex
            .pointer("/exception/description")
            .or_else(|| ex.get("text"))
            .and_then(|v| v.as_str())
            .unwrap_or("exception");
        return Err(format!("text reader threw: {}", shorten(what, 120)));
    }
    let value = response
        .pointer("/result/value")
        .and_then(|v| v.as_object())
        .ok_or("text reader returned no object")?;
    let text = value
        .get("text")
        .and_then(|v| v.as_str())
        .ok_or("text reader returned no string text")?;
    let href = value
        .get("href")
        .and_then(|v| v.as_str())
        .ok_or("text reader returned no string href")?;
    let flag = |name: &str| {
        value
            .get(name)
            .and_then(|v| v.as_bool())
            .ok_or_else(|| format!("text reader returned no {name} flag"))
    };
    let truncated = flag("truncated")?;
    let password = flag("password")?;
    let scanned = flag("scanned")?;
    if without_fragment(href) != without_fragment(expected_url) {
        return Err(format!(
            "read {} but the frame tree names {}",
            shorten(href, 80),
            shorten(expected_url, 80)
        ));
    }
    Ok(FrameText {
        lines: visible_lines(text),
        truncated,
        password,
        scanned,
    })
}

/// Test hooks. Each set bit, lowest first, applies to one capture:
/// `FAIL_TEXT_READS` sends the reader to a context that does not exist (a
/// CDP error), `HANG_TEXT_READS` makes the reader wait on a promise that
/// never settles, `INJECT_TEXT_FRAME` adds a frame to the top document after
/// the frame reads and before the final frame tree.
#[cfg(test)]
pub(crate) static FAIL_TEXT_READS: std::sync::atomic::AtomicU32 =
    std::sync::atomic::AtomicU32::new(0);
#[cfg(test)]
pub(crate) static HANG_TEXT_READS: std::sync::atomic::AtomicU32 =
    std::sync::atomic::AtomicU32::new(0);
#[cfg(test)]
pub(crate) static INJECT_TEXT_FRAME: std::sync::atomic::AtomicU32 =
    std::sync::atomic::AtomicU32::new(0);

#[derive(Clone, Copy, Default)]
struct TextFaults {
    fail: bool,
    hang: bool,
    inject: bool,
}

fn take_text_faults() -> TextFaults {
    #[cfg(test)]
    {
        use std::sync::atomic::{AtomicU32, Ordering};
        let pop = |hook: &AtomicU32| {
            let mut set = false;
            let _ = hook.fetch_update(Ordering::SeqCst, Ordering::SeqCst, |v| {
                set = v & 1 == 1;
                Some(v >> 1)
            });
            set
        };
        TextFaults {
            fail: pop(&FAIL_TEXT_READS),
            hang: pop(&HANG_TEXT_READS),
            inject: pop(&INJECT_TEXT_FRAME),
        }
    }
    #[cfg(not(test))]
    {
        TextFaults::default()
    }
}

const BUDGET_SPENT: &str = "not read: the capture's time budget was spent";

/// One capture's shared budget: a deadline every CDP call is held to, and
/// a count of frames read across every session.
struct CaptureBudget {
    deadline: std::time::Instant,
    frames: usize,
}

impl CaptureBudget {
    fn left(&self) -> std::time::Duration {
        self.deadline
            .saturating_duration_since(std::time::Instant::now())
    }

    /// Run one CDP call with only the time left.
    async fn call<T>(
        &self,
        fut: impl std::future::Future<Output = Result<T, String>>,
    ) -> Result<T, String> {
        let left = self.left();
        if left.is_zero() {
            return Err(BUDGET_SPENT.into());
        }
        tokio::time::timeout(left, fut)
            .await
            .map_err(|_| BUDGET_SPENT.to_string())?
    }
}

async fn eval_reader(
    client: &super::cdp::client::CdpClient,
    session: &str,
    frame_id: &str,
    budget: &CaptureBudget,
    faults: TextFaults,
) -> Result<serde_json::Value, String> {
    let world = budget
        .call(client.send_command(
            "Page.createIsolatedWorld",
            Some(serde_json::json!({ "frameId": frame_id, "worldName": "chrome_use_observe" })),
            Some(session),
        ))
        .await?;
    let context = world
        .get("executionContextId")
        .and_then(|c| c.as_i64())
        .ok_or("no execution context for the frame")?;
    let context = if faults.fail {
        i64::from(i32::MAX)
    } else {
        context
    };
    let (expression, await_promise) = if faults.hang {
        ("new Promise(function(){})".to_string(), true)
    } else {
        (observe_text_js(budget.left().as_millis() as u64), false)
    };
    budget
        .call(client.send_command(
            "Runtime.evaluate",
            Some(serde_json::json!({
                "expression": expression,
                "returnByValue": true,
                "awaitPromise": await_promise,
                "contextId": context,
            })),
            Some(session),
        ))
        .await
}

fn problem_for(error: String) -> FrameProblem {
    if error == BUDGET_SPENT {
        FrameProblem::partial(error)
    } else {
        FrameProblem::unavailable(error)
    }
}

/// Read every frame one session hosts, except `skip`: the tree, each frame
/// in its own isolated world, then the tree again, which is the inventory
/// returned. A frame whose document changed or that left between the two
/// trees is unknown; a frame that is in the final tree but was not read
/// (over budget, or it appeared during the read) is listed as `partial`.
async fn read_session(
    client: &super::cdp::client::CdpClient,
    session: &str,
    skip: &std::collections::HashMap<String, String>,
    budget: &mut CaptureBudget,
    faults: TextFaults,
    inject: bool,
) -> Result<Vec<FrameRead>, String> {
    let tree = validate_frame_tree(
        &budget
            .call(client.send_command_no_params("Page.getFrameTree", Some(session)))
            .await?,
    )?;
    let mut reads = Vec::new();
    for frame in tree.iter().filter(|f| f.top || !skip.contains_key(&f.id)) {
        let label = (!frame.top).then(|| frame_label(&frame.url));
        let text = if budget.frames >= MAX_TEXT_FRAMES {
            Err(FrameProblem::partial("not read: over the frame budget"))
        } else if budget.left().is_zero() {
            Err(FrameProblem::partial(BUDGET_SPENT))
        } else {
            budget.frames += 1;
            match eval_reader(client, session, &frame.id, budget, faults).await {
                Ok(response) => {
                    parse_frame_read(&response, &frame.url).map_err(FrameProblem::unavailable)
                }
                Err(e) => Err(problem_for(e)),
            }
        };
        reads.push(FrameRead {
            id: frame.id.clone(),
            loader: frame.loader.clone(),
            label,
            text,
        });
    }
    if inject {
        let _ = budget
            .call(client.send_command(
                "Runtime.evaluate",
                Some(serde_json::json!({
                    "expression": "new Promise(function(r){var f=document.createElement('iframe');\
                        f.srcdoc='<p>late frame</p>';f.onload=function(){r(true)};\
                        document.body.appendChild(f)})",
                    "awaitPromise": true,
                })),
                Some(session),
            ))
            .await;
    }
    let after = budget
        .call(client.send_command_no_params("Page.getFrameTree", Some(session)))
        .await
        .and_then(|t| validate_frame_tree(&t));
    match &after {
        Ok(frames) => {
            for read in &mut reads {
                let same = frames
                    .iter()
                    .any(|f| f.id == read.id && f.loader == read.loader);
                if !same && read.text.is_ok() {
                    read.text = Err(FrameProblem::unavailable(
                        "frame navigated, was replaced or left during the read",
                    ));
                }
            }
            for frame in frames.iter().filter(|f| f.top || !skip.contains_key(&f.id)) {
                if !reads.iter().any(|r| r.id == frame.id) {
                    reads.push(FrameRead {
                        id: frame.id.clone(),
                        loader: frame.loader.clone(),
                        label: (!frame.top).then(|| frame_label(&frame.url)),
                        text: Err(FrameProblem::partial(
                            "not read: the frame appeared during the read",
                        )),
                    });
                }
            }
        }
        Err(e) => {
            for read in &mut reads {
                read.text = Err(problem_for(if e.as_str() == BUDGET_SPENT {
                    e.clone()
                } else {
                    format!("frame tree unreadable after the read: {e}")
                }));
            }
        }
    }
    Ok(reads)
}

/// Read the visible text of every frame in the active page, strictly and
/// within [`TEXT_CAPTURE_BUDGET_MS`] and [`MAX_TEXT_FRAMES`] in total: the top
/// session's frames, then each out-of-process frame through its own session,
/// which must be attached to that frame.
pub(super) async fn capture_text(state: &super::actions::DaemonState) -> TextRead {
    let faults = take_text_faults();
    let mut budget = CaptureBudget {
        deadline: std::time::Instant::now()
            + std::time::Duration::from_millis(TEXT_CAPTURE_BUDGET_MS),
        frames: 0,
    };
    let mgr = state
        .browser
        .as_ref()
        .ok_or("No active page for observation")?;
    let top = mgr
        .active_session_id()
        .map_err(|_| "No active page for observation".to_string())?
        .to_string();
    let client = &mgr.client;
    let mut reads = read_session(
        client,
        &top,
        &state.iframe_sessions,
        &mut budget,
        faults,
        faults.inject,
    )
    .await?;
    let mut oopifs: Vec<(&String, &String)> = state.iframe_sessions.iter().collect();
    oopifs.sort();
    for (frame_id, session) in oopifs {
        if reads.iter().any(|r| &r.id == frame_id) {
            continue;
        }
        let unknown = |problem: FrameProblem| FrameRead {
            id: frame_id.clone(),
            loader: String::new(),
            label: Some(format!("frame {}", head(frame_id, 8))),
            text: Err(problem),
        };
        if budget.frames >= MAX_TEXT_FRAMES {
            reads.push(unknown(FrameProblem::partial(
                "not read: over the frame budget",
            )));
            continue;
        }
        match read_session(
            client,
            session,
            &std::collections::HashMap::new(),
            &mut budget,
            faults,
            false,
        )
        .await
        {
            Ok(frames) if frames.first().is_some_and(|f| &f.id == frame_id) => {
                for frame in frames {
                    if !reads.iter().any(|r| r.id == frame.id) {
                        reads.push(frame);
                    }
                }
            }
            Ok(_) => reads.push(unknown(FrameProblem::unavailable(
                "the frame's session holds a different frame",
            ))),
            Err(e) => reads.push(unknown(problem_for(e))),
        }
    }
    Ok(reads)
}

/// The bounded `observed.text` list under construction. A line is formatted
/// only while there is budget for it; past the budget it is counted.
struct TextBudget {
    lines: Vec<String>,
    bytes: usize,
    omitted: usize,
}

impl TextBudget {
    fn push(&mut self, sign: char, label: &Option<String>, line: &str) {
        if self.lines.len() == MAX_TEXT_LINES || self.bytes >= MAX_TEXT_BYTES {
            self.omitted += 1;
            return;
        }
        let prefix = label
            .as_ref()
            .map(|l| format!("[frame {l}] "))
            .unwrap_or_default();
        let (line, _) = bounded_line(sign, &prefix, line, MAX_TEXT_LINE_BYTES);
        if self.bytes + line.len() + 1 > MAX_TEXT_BYTES {
            self.omitted += 1;
            return;
        }
        self.bytes += line.len() + 1;
        self.lines.push(line);
    }
}

/// The outcome of comparing two visible-text reads.
#[derive(Debug, Default)]
pub(super) struct TextComparison {
    pub lines: Vec<String>,
    /// Changed lines not listed, over the line or byte budget.
    pub omitted: usize,
    /// Frames that could not be compared: (frame, status, reason).
    pub problems: Vec<(String, &'static str, String)>,
    /// Frames of the before read that the after inventory confirms are gone.
    pub gone: Vec<String>,
    /// Frames whose document changed (another `loaderId`): a reload or a
    /// navigation, a change even when the text is the same.
    pub reloaded: Vec<String>,
    /// A password field held a value (or could not be scanned) in some frame
    /// on either side: no lines are returned.
    pub redacted: bool,
}

fn frame_name(label: &Option<String>) -> String {
    label.clone().unwrap_or_else(|| "top".into())
}

/// Compare two reads frame by frame. A frame with no comparable text on
/// either side is a problem, never a removal or "no change". Lines are
/// returned only when every frame on both sides was read and scanned with
/// no password value in it: a frame that could not be read might hold a
/// password the page copied elsewhere, so any problem withholds them all.
/// `gone` is only what the after inventory confirms; `reloaded` is a frame
/// whose document changed.
pub(super) fn text_diff(before: &[FrameRead], after: &[FrameRead]) -> TextComparison {
    use similar::{capture_diff_slices_deadline, Algorithm, DiffOp};
    let mut out = TextComparison::default();
    let empty = FrameText {
        lines: Vec::new(),
        truncated: false,
        password: false,
        scanned: true,
    };
    let mut pairs = Vec::new();
    for frame in after {
        let old = before.iter().find(|b| b.id == frame.id);
        if let Some(old) = old {
            if !old.loader.is_empty() && !frame.loader.is_empty() && old.loader != frame.loader {
                out.reloaded.push(frame_name(&frame.label));
            }
        }
        let pair = match (old.map(|b| &b.text), &frame.text) {
            (None, Ok(new)) => Ok((&empty, new)),
            (Some(Ok(old)), Ok(new)) => Ok((old, new)),
            (Some(Err(p)), _) | (_, Err(p)) => Err(p.clone()),
        };
        match pair {
            Ok((old, new)) if old.truncated || new.truncated => out.problems.push((
                frame_name(&frame.label),
                "partial",
                "text over the reader's size, node or time budget".into(),
            )),
            Ok((old, new)) => {
                if old.password || new.password || !old.scanned || !new.scanned {
                    out.redacted = true;
                }
                pairs.push((&frame.label, old, new));
            }
            Err(p) => out
                .problems
                .push((frame_name(&frame.label), p.status, p.reason)),
        }
    }
    for frame in before
        .iter()
        .filter(|b| !after.iter().any(|a| a.id == b.id))
    {
        match &frame.text {
            Ok(t) if t.password || !t.scanned => out.redacted = true,
            Ok(_) => {}
            // Gone now, but what it held before is unknown: it may have had
            // a password the page copied elsewhere.
            Err(p) => out.problems.push((
                frame_name(&frame.label),
                "partial",
                format!("gone; its earlier read failed: {}", p.reason),
            )),
        }
        out.gone.push(frame_name(&frame.label));
    }
    if !out.problems.is_empty() || out.redacted {
        return out;
    }
    let mut budget = TextBudget {
        lines: Vec::new(),
        bytes: 0,
        omitted: 0,
    };
    // A text-heavy page diffs in well under this; the deadline only keeps a
    // pathological one from holding the reply.
    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(200);
    for (label, old, new) in pairs {
        for op in
            capture_diff_slices_deadline(Algorithm::Myers, &old.lines, &new.lines, Some(deadline))
        {
            let (removed, added) = match op {
                DiffOp::Equal { .. } => continue,
                DiffOp::Delete {
                    old_index, old_len, ..
                } => (old_index..old_index + old_len, 0..0),
                DiffOp::Insert {
                    new_index, new_len, ..
                } => (0..0, new_index..new_index + new_len),
                DiffOp::Replace {
                    old_index,
                    old_len,
                    new_index,
                    new_len,
                } => (
                    old_index..old_index + old_len,
                    new_index..new_index + new_len,
                ),
            };
            for i in removed {
                budget.push('-', label, &old.lines[i]);
            }
            for i in added {
                budget.push('+', label, &new.lines[i]);
            }
        }
    }
    out.lines = budget.lines;
    out.omitted = budget.omitted;
    out
}

/// Add the visible-text comparison to an observation. A text change, a
/// confirmed frame removal and a frame whose document changed are changes.
/// Missing or withheld text evidence is missing evidence: the observation is
/// no longer complete, and an unchanged tree no longer proves "no change", so
/// nothing downstream (the no-progress hint, the no-change probe) can read an
/// unknown as a stall.
pub(super) fn apply_text(
    observed: &mut serde_json::Map<String, serde_json::Value>,
    before: &TextRead,
    after: &TextRead,
) {
    use serde_json::json;
    let diff = match (before, after) {
        (Ok(before), Ok(after)) => text_diff(before, after),
        (before, after) => {
            let side = if before.is_err() { "before" } else { "after" };
            let error = before
                .as_ref()
                .err()
                .or(after.as_ref().err())
                .cloned()
                .unwrap_or_default();
            TextComparison {
                problems: vec![("page".into(), "unavailable", format!("{side}: {error}"))],
                ..Default::default()
            }
        }
    };
    if !diff.lines.is_empty() {
        observed.insert("text".into(), json!(diff.lines));
        if diff.omitted > 0 {
            observed.insert("textOmitted".into(), json!(diff.omitted));
            observed.insert("textTruncated".into(), json!(true));
        }
    }
    if !diff.reloaded.is_empty() {
        observed.insert("frameDocumentChanged".into(), json!(diff.reloaded));
    }
    if !diff.lines.is_empty() || !diff.reloaded.is_empty() || !diff.gone.is_empty() {
        observed.insert("changed".into(), json!(true));
    }
    let redaction = diff.redacted.then(|| {
        (
            "page".to_string(),
            "redacted",
            "a password field holds a value (or could not be scanned); text withheld".to_string(),
        )
    });
    let mut notes: Vec<serde_json::Value> = diff
        .problems
        .iter()
        .chain(redaction.iter())
        .map(|(frame, status, reason)| {
            json!({"frame": frame, "status": status, "reason": shorten(reason, 160)})
        })
        .chain(
            diff.gone
                .iter()
                .map(|frame| json!({"frame": frame, "status": "gone"})),
        )
        .collect();
    let noted = notes.len();
    notes.truncate(MAX_TEXT_FRAME_NOTES);
    if !notes.is_empty() {
        observed.insert("textFrames".into(), json!(notes));
        if noted > MAX_TEXT_FRAME_NOTES {
            observed.insert(
                "textFramesOmitted".into(),
                json!(noted - MAX_TEXT_FRAME_NOTES),
            );
        }
    }
    let Some((frame, _, reason)) = diff.problems.first().or(redaction.as_ref()) else {
        return;
    };
    observed.insert(
        "textStatus".into(),
        json!(if diff.problems.is_empty() {
            "redacted"
        } else {
            "unavailable"
        }),
    );
    let error = capture_error("visibleText", &format!("{frame}: {reason}"));
    match observed.get_mut("errors").and_then(|e| e.as_array_mut()) {
        Some(errors) => errors.push(error),
        None => {
            observed.insert("errors".into(), json!([error]));
        }
    }
    if observed.get("status").and_then(|s| s.as_str()) == Some("complete") {
        observed.insert("status".into(), json!("partial"));
    }
    if observed.get("changed") == Some(&json!(false)) {
        observed.insert("changed".into(), serde_json::Value::Null);
    }
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
        let out = compact_changes(&format!("{before}\n"), &format!("{after}\n"));
        assert!(out.lines.len() <= MAX_CHANGE_LINES);
        assert_eq!(out.lines.len() + out.omitted, 201);
        let bytes: usize = out.lines.iter().map(|l| l.len() + 1).sum();
        assert!(bytes <= MAX_CHANGE_BYTES, "{bytes}");
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

    #[test]
    fn a_huge_removed_line_is_cut_not_copied_whole() {
        let huge = format!("- text \"{}\"", "z".repeat(1_000_000));
        let out = compact_changes(&format!("{huge}\n"), "- button \"Done\"\n");
        assert_eq!(out.lines.len(), 2);
        assert!(out.lines.iter().all(|l| l.len() <= MAX_CHANGE_LINE_BYTES));
        assert_eq!(out.shortened, 1);
        assert!(out.lines[0].starts_with("- - text"));
        assert!(out.lines[0].contains("bytes omitted]"));
        // Many long lines: the total byte budget holds, the rest is counted.
        let many: String = (0..50)
            .map(|i| format!("- row {i} {}\n", "w".repeat(500)))
            .collect();
        let out = compact_changes(&many, "");
        let bytes: usize = out.lines.iter().map(|l| l.len() + 1).sum();
        assert!(bytes <= MAX_CHANGE_BYTES, "{bytes}");
        assert!(out.omitted > 0);
        assert_eq!(out.lines.len() + out.omitted, 50);
        // Multi-byte text is cut on a character boundary.
        let wide = format!("- text \"{}\"", "路".repeat(500));
        let out = compact_changes(&format!("{wide}\n"), "");
        assert!(out.lines[0].len() <= MAX_CHANGE_LINE_BYTES);
    }

    fn read(id: &str, label: Option<&str>, text: &str) -> FrameRead {
        FrameRead {
            id: id.into(),
            loader: format!("L-{id}"),
            label: label.map(str::to_string),
            text: Ok(FrameText {
                lines: visible_lines(text),
                truncated: false,
                password: false,
                scanned: true,
            }),
        }
    }

    fn with_password(mut frame: FrameRead) -> FrameRead {
        if let Ok(t) = &mut frame.text {
            t.password = true;
        }
        frame
    }

    fn failed(id: &str, label: Option<&str>) -> FrameRead {
        FrameRead {
            id: id.into(),
            loader: format!("L-{id}"),
            label: label.map(str::to_string),
            text: Err(FrameProblem::unavailable(
                "Cannot find context with specified id",
            )),
        }
    }

    fn unchanged_tree() -> serde_json::Map<String, serde_json::Value> {
        changes(&ok("tree"), &ok("tree"), &ok("u"), &ok("u"))
    }

    #[test]
    fn an_iframe_receipt_is_a_text_change_even_when_the_tree_did_not_move() {
        let before = Ok(vec![
            read("T", None, "Embedded notes"),
            read("C", Some("child.html"), "Note Save note\nNo note"),
        ]);
        let after = Ok(vec![
            read("T", None, "Embedded notes"),
            read(
                "C",
                Some("child.html"),
                "Note Save note\nNote saved: Synthetic benchmark note",
            ),
        ]);
        let mut observed = unchanged_tree();
        apply_text(&mut observed, &before, &after);
        assert_eq!(observed["changed"], true);
        assert_eq!(observed["status"], "complete");
        assert_eq!(
            observed["text"],
            json!([
                "- [frame child.html] No note",
                "+ [frame child.html] Note saved: Synthetic benchmark note"
            ])
        );
        assert!(!observed.contains_key("textStatus"));
    }

    #[test]
    fn a_failed_after_read_is_neither_a_removal_nor_no_change() {
        let before = Ok(vec![
            read("T", None, "Embedded notes"),
            read("C", Some("child.html"), "Note saved: earlier receipt"),
        ]);
        let after = Ok(vec![
            read("T", None, "Embedded notes"),
            failed("C", Some("child.html")),
        ]);
        let mut observed = unchanged_tree();
        apply_text(&mut observed, &before, &after);
        assert!(!observed.contains_key("text"), "{observed:?}");
        assert_eq!(observed["status"], "partial");
        assert!(observed["changed"].is_null());
        assert_eq!(observed["textStatus"], "unavailable");
        assert_eq!(observed["textFrames"][0]["frame"], "child.html");
        assert_eq!(observed["textFrames"][0]["status"], "unavailable");
        assert_eq!(observed["errors"][0]["stage"], "visibleText");
    }

    #[test]
    fn both_reads_failing_is_never_complete() {
        let mut observed = unchanged_tree();
        apply_text(
            &mut observed,
            &Err("frame tree result has no frameTree".into()),
            &Err("Cannot find context".into()),
        );
        assert_ne!(observed["status"], "complete");
        assert!(observed["changed"].is_null());
        assert_eq!(observed["textStatus"], "unavailable");
        // A real tree change survives missing text evidence.
        let mut observed = changes(&ok("button A"), &ok("button B"), &ok("u"), &ok("u"));
        apply_text(&mut observed, &Err("x".into()), &Err("y".into()));
        assert_eq!(observed["changed"], true);
        assert_eq!(observed["status"], "partial");
    }

    #[test]
    fn a_tree_change_is_kept_when_the_text_read_fails() {
        let mut observed = changes(&ok("button A"), &ok("button B"), &ok("u"), &ok("u"));
        apply_text(
            &mut observed,
            &Ok(vec![read("T", None, "a")]),
            &Ok(vec![failed("T", None)]),
        );
        assert_eq!(observed["changed"], true);
        assert_eq!(observed["status"], "partial");
        assert_eq!(observed["textStatus"], "unavailable");
    }

    #[test]
    fn any_password_value_on_either_side_withholds_all_text() {
        // Child holds a password; the parent's text changed. The parent's
        // line could be the password, copied: nothing is returned.
        for (before, after) in [
            (
                vec![
                    read("T", None, "a"),
                    with_password(read("C", Some("c"), "x")),
                ],
                vec![
                    read("T", None, "a\nb"),
                    with_password(read("C", Some("c"), "x")),
                ],
            ),
            // The field was cleared after, but its old value may be on show.
            (
                vec![
                    read("T", None, "a"),
                    with_password(read("C", Some("c"), "x")),
                ],
                vec![read("T", None, "a\nb"), read("C", Some("c"), "x")],
            ),
            // A frame with a password value that is gone after.
            (
                vec![
                    read("T", None, "a"),
                    with_password(read("C", Some("c"), "x")),
                ],
                vec![read("T", None, "a\nb")],
            ),
        ] {
            let mut observed = unchanged_tree();
            apply_text(&mut observed, &Ok(before), &Ok(after));
            assert!(!observed.contains_key("text"), "{observed:?}");
            assert_eq!(observed["textStatus"], "redacted");
            assert_eq!(observed["status"], "partial");
            assert!(!observed.contains_key("text"));
        }
    }

    #[test]
    fn an_unreadable_frame_withholds_every_other_frames_lines() {
        let mut observed = unchanged_tree();
        apply_text(
            &mut observed,
            &Ok(vec![read("T", None, "a"), read("C", Some("c"), "x")]),
            &Ok(vec![read("T", None, "a\nb"), failed("C", Some("c"))]),
        );
        assert!(!observed.contains_key("text"), "{observed:?}");
        assert_eq!(observed["status"], "partial");
        assert!(observed["changed"].is_null());
    }

    #[test]
    fn a_frame_removed_alone_is_a_change() {
        // Only a child frame went away; the top frame and tree are the same.
        let mut observed = unchanged_tree();
        apply_text(
            &mut observed,
            &Ok(vec![
                read("T", None, "Embedded notes"),
                read("C", Some("receipt.html"), "Saved receipt 42"),
            ]),
            &Ok(vec![read("T", None, "Embedded notes")]),
        );
        assert_eq!(observed["changed"], true);
        assert_eq!(observed["status"], "complete");
        assert_eq!(observed["textFrames"][0]["status"], "gone");
        assert!(!observed.contains_key("text"));
    }

    #[test]
    fn a_new_document_with_the_same_text_is_a_change() {
        // A same-URL reload: same text, another loaderId.
        let before = read("T", None, "Report ready");
        let mut after = read("T", None, "Report ready");
        after.loader = "L-reloaded".into();
        let mut observed = unchanged_tree();
        apply_text(&mut observed, &Ok(vec![before]), &Ok(vec![after]));
        assert_eq!(observed["changed"], true);
        assert_eq!(observed["frameDocumentChanged"], json!(["top"]));
        assert!(!observed.contains_key("text"));
    }

    #[test]
    fn a_frame_not_read_is_partial_never_gone() {
        let skipped = FrameRead {
            id: "C".into(),
            loader: "L-C".into(),
            label: Some("c".into()),
            text: Err(FrameProblem::partial("not read: over the frame budget")),
        };
        let mut observed = unchanged_tree();
        apply_text(
            &mut observed,
            &Ok(vec![read("T", None, "a"), read("C", Some("c"), "x")]),
            &Ok(vec![read("T", None, "a"), skipped]),
        );
        let notes = observed["textFrames"].to_string();
        assert!(notes.contains("\"partial\""), "{notes}");
        assert!(!notes.contains("gone"), "{notes}");
        assert_eq!(observed["status"], "partial");
        assert!(observed["changed"].is_null());
    }

    #[test]
    fn unchanged_text_adds_nothing() {
        let same = Ok(vec![read("T", None, "Page 1 of 3\n  Kit   catalog ")]);
        let mut observed = unchanged_tree();
        apply_text(&mut observed, &same, &same);
        assert_eq!(observed["changed"], false);
        assert_eq!(observed["status"], "complete");
        assert!(!observed.contains_key("text"));
        assert!(!observed.contains_key("textStatus"));
    }

    #[test]
    fn a_truncated_read_is_partial_not_a_diff() {
        let mut big = read("T", None, "a\nb");
        if let Ok(t) = &mut big.text {
            t.truncated = true;
        }
        let mut observed = unchanged_tree();
        apply_text(
            &mut observed,
            &Ok(vec![read("T", None, "a")]),
            &Ok(vec![big]),
        );
        assert!(!observed.contains_key("text"));
        assert_eq!(observed["textFrames"][0]["status"], "partial");
        assert_eq!(observed["status"], "partial");
    }

    #[test]
    fn text_changes_are_bounded_while_they_are_built() {
        let many: String = (0..5000).map(|i| format!("row {i}\n")).collect();
        let out = text_diff(&[read("T", None, "")], &[read("T", None, &many)]);
        assert_eq!(out.lines.len(), MAX_TEXT_LINES);
        assert_eq!(out.omitted, 5000 - MAX_TEXT_LINES);

        let wide: String = (0..30)
            .map(|i| format!("{i} {}\n", "w".repeat(100_000)))
            .collect();
        let out = text_diff(&[read("T", None, "")], &[read("T", None, &wide)]);
        let bytes: usize = out.lines.iter().map(|l| l.len() + 1).sum();
        assert!(bytes <= MAX_TEXT_BYTES, "{bytes}");
        assert!(out.lines.iter().all(|l| l.len() <= MAX_TEXT_LINE_BYTES));
        assert_eq!(out.lines.len() + out.omitted, 30);
        let mut observed = unchanged_tree();
        apply_text(
            &mut observed,
            &Ok(vec![read("T", None, "")]),
            &Ok(vec![read("T", None, &wide)]),
        );
        assert_eq!(observed["textTruncated"], true);
        assert!(observed["textOmitted"].as_u64().unwrap() > 0);
    }

    #[test]
    fn new_and_gone_frames() {
        let out = text_diff(
            &[read("T", None, "a"), read("old", Some("a.html"), "bye")],
            &[read("T", None, "a"), read("new", Some("b"), "hi")],
        );
        assert_eq!(out.lines, ["+ [frame b] hi"]);
        assert_eq!(out.gone, ["a.html"]);
        assert!(out.problems.is_empty());
    }

    #[test]
    fn page_counters_and_delayed_totals_show_as_text() {
        let out = text_diff(
            &[read("T", None, "Previous pagePage 1 of 3Next page\nIdle")],
            &[read(
                "T",
                None,
                "Previous pagePage 2 of 3Next page\nReport ready\nTotal entries: 3",
            )],
        );
        assert!(out
            .lines
            .contains(&"+ Previous pagePage 2 of 3Next page".to_string()));
        assert!(out.lines.contains(&"+ Total entries: 3".to_string()));
        assert!(out.lines.contains(&"- Idle".to_string()));
    }

    #[test]
    fn a_malformed_frame_tree_is_an_error_not_fewer_frames() {
        let good = json!({"frameTree": {"frame": {"id": "T", "url": "u", "loaderId": "L"},
            "childFrames": [{"frame": {"id": "C", "url": "c", "loaderId": "L2"}}]}});
        let frames = validate_frame_tree(&good).unwrap();
        assert_eq!(frames.len(), 2);
        assert!(frames[0].top && !frames[1].top);
        for bad in [
            json!({}),
            json!({"frameTree": {}}),
            json!({"frameTree": {"frame": {"id": "T", "url": "u"}}}),
            json!({"frameTree": {"frame": {"id": "T", "url": "u", "loaderId": "L"},
                "childFrames": {"frame": {}}}}),
            json!({"frameTree": {"frame": {"id": "T", "url": "u", "loaderId": "L"},
                "childFrames": [{"frame": {"id": 7, "url": "c", "loaderId": "L2"}}]}}),
        ] {
            assert!(validate_frame_tree(&bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn a_reader_result_is_checked_not_defaulted() {
        let good = json!({"result": {"type": "object",
            "value": {"text": "a\nb", "href": "http://x/c.html#top", "truncated": false,
                      "password": false, "scanned": true}}});
        assert_eq!(
            parse_frame_read(&good, "http://x/c.html").unwrap().lines,
            ["a", "b"]
        );
        // Another document than the tree names.
        assert!(parse_frame_read(&good, "http://x/other.html").is_err());
        for bad in [
            json!({"exceptionDetails": {"text": "Uncaught",
                   "exception": {"description": "TypeError: x"}},
                   "result": {"type": "object"}}),
            json!({"result": {"type": "string", "value": ""}}),
            json!({"result": {"type": "undefined"}}),
            json!({"result": {"value": {"text": 3, "href": "u", "truncated": false}}}),
            json!({"result": {"value": {"text": "a", "truncated": false}}}),
            json!({"result": {"value": {"text": "a", "href": "u"}}}),
            json!({"result": {"value": {"text": "a", "href": "u", "truncated": false,
                                        "scanned": true}}}),
        ] {
            assert!(parse_frame_read(&bad, "u").is_err(), "{bad}");
        }
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
