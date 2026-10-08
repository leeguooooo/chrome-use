//! Opt-in: open `target=_blank` links an agent clicks in a background tab,
//! so Chrome does not come to the front (#468).
//!
//! When a page opens a tab or pop-up, Chrome inserts it through
//! `BrowserWebContentsDelegate::AddNewContents` → `chrome::AddWebContents`
//! with `WindowAction::kShowWindow` → `Navigate()` → `ScopedBrowserShower`
//! → `window->Show()`, and `BrowserView::Show()` activates an already
//! visible window (Chromium 155.0.8059.39). On macOS that activates the
//! app. It happens for a background-tab disposition too, for a minimized
//! or off-screen window, and before any extension or CDP event about the
//! new tab. A tab chrome-use creates itself does not activate anything.
//!
//! Opening the link ourselves is not identical to Chrome's click. Measured
//! on the build host against the same links clicked natively:
//! - the new tab has an extra `about:blank` history entry
//!   (`history.length` 2, not 1);
//! - over the relay the navigation is attributed to the extension:
//!   `Sec-Fetch-Site: cross-site` for same-site and same-origin links, so
//!   `SameSite=Strict` cookies are not sent, including after a cross-site
//!   redirect back to the page's site;
//! - a `Referrer-Policy` the page set by HTTP header is used only when the
//!   daemon saw that document's response.
//!
//! So this is off by default: a click that makes the page open a tab goes
//! through Chrome as before, and the click says Chrome may have come to the
//! front. [`BACKGROUND_LINKS_ENV`] turns it on for the session.
//!
//! When on, the click's default action is taken over only for a plain left
//! click on an `<a>`/`<area>` whose effective target is `_blank`, with an
//! http(s) href, no `rel=opener` (so Chrome would open it without an opener
//! anyway), no `download` and no `ping`, in the guard's own document. The
//! click is delivered as before and every page listener runs; only after
//! the last of them (a listener added on `window` for the bubble phase while
//! the event is in flight) and only when no page listener called
//! `preventDefault()`, the guard cancels Chrome's own navigation and reports
//! the link, which chrome-use opens in a new background tab of the session
//! with the document as referrer under the effective referrer policy. A
//! listener that stops propagation leaves the link to Chrome (one tab, not
//! two). Everything else — `window.open`, `rel=opener`, named targets, forms
//! — always goes through Chrome.
//!
//! The guard lives in an isolated world, so page scripts can neither see nor
//! tamper with it, and it disarms itself after one interception, when the
//! daemon reads it, or after [`ARM_TTL_MS`].

use std::collections::HashMap;

use serde::Deserialize;
use serde_json::{json, Value};

use super::cdp::client::CdpClient;

/// Isolated world the guard runs in.
const WORLD_NAME: &str = "chrome_use_popup_guard";

/// A guard nobody reads (the click hit a dialog, the daemon died) stops
/// acting after this long.
pub const ARM_TTL_MS: u64 = 10_000;

/// The guard leaves a click alone when its listeners took longer than this
/// between the capture and the bubble phase on `window` (a `confirm()` in a
/// handler, a long synchronous handler): by then the daemon may have
/// returned without reading it, and a cancelled link nobody opens would be a
/// click that silently did nothing.
pub const MAX_DISPATCH_MS: u64 = 1_000;

/// Opt-in for opening plain `target=_blank` links in a background tab.
pub const BACKGROUND_LINKS_ENV: &str = "AGENT_BROWSER_BACKGROUND_LINKS";

/// Whether [`BACKGROUND_LINKS_ENV`] is on for this daemon.
pub fn background_links_opt_in() -> bool {
    std::env::var(BACKGROUND_LINKS_ENV)
        .map(|v| {
            matches!(
                v.trim().to_ascii_lowercase().as_str(),
                "1" | "true" | "yes" | "on"
            )
        })
        .unwrap_or(false)
}

/// Referrer policy tokens a document or link can carry.
const POLICIES: [&str; 8] = [
    "no-referrer",
    "no-referrer-when-downgrade",
    "origin",
    "origin-when-cross-origin",
    "same-origin",
    "strict-origin",
    "strict-origin-when-cross-origin",
    "unsafe-url",
];

/// The document policy a `Referrer-Policy` response header sets: the last
/// recognised token of the comma-separated list, or `""` (Chrome's default)
/// when there is none.
pub fn header_referrer_policy(value: &str) -> String {
    value
        .split(',')
        .map(|t| t.trim().to_ascii_lowercase())
        .rfind(|t| POLICIES.contains(&t.as_str()))
        .unwrap_or_default()
}

/// From a `Network.responseReceived` event for a document, the frame it is
/// for and the referrer policy its response header set (`""` when none).
pub fn document_referrer_policy(params: &Value) -> Option<(String, String)> {
    if params.get("type").and_then(Value::as_str) != Some("Document") {
        return None;
    }
    let frame = params.get("frameId").and_then(Value::as_str)?.to_string();
    let headers = params.pointer("/response/headers")?.as_object()?;
    let policy = headers
        .iter()
        .filter(|(k, _)| k.eq_ignore_ascii_case("referrer-policy"))
        .filter_map(|(_, v)| v.as_str())
        .map(header_referrer_policy)
        .rfind(|p| !p.is_empty())
        .unwrap_or_default();
    Some((frame, policy))
}

/// Installed right before the click. Returns `true` once armed.
/// `header_policy` is the policy the guard's document got from its response
/// header (`Some("")` for none), `None` when the daemon did not see it.
pub fn arm_script(header_policy: Option<&str>) -> String {
    format!(
        r#"(() => {{
  const KEY = '__chromeUsePopupGuard';
  const HEADER_POLICY = {header};
  const prev = globalThis[KEY];
  if (prev && typeof prev.disarm === 'function') prev.disarm();
  const armedAt = Date.now();
  const st = {{ result: null, seen: null, skipped: null, pagePrevented: false, late: false }};
  const VALID = {valid};
  const LEGACY = {{ never: 'no-referrer', default: 'strict-origin-when-cross-origin',
    always: 'unsafe-url', 'origin-when-crossorigin': 'origin-when-cross-origin' }};
  const norm = (v) => {{
    v = String(v || '').trim().toLowerCase();
    return VALID.includes(v) ? v : (LEGACY[v] || null);
  }};
  const interactive = (n) =>
    n instanceof HTMLButtonElement || n instanceof HTMLInputElement ||
    n instanceof HTMLSelectElement || n instanceof HTMLTextAreaElement ||
    n instanceof HTMLLabelElement ||
    (n instanceof HTMLElement && (n.localName === 'summary' || n.isContentEditable));
  const documentPolicy = (doc) => {{
    let meta = null;
    for (const m of doc.querySelectorAll('meta[name="referrer" i]')) {{
      const t = norm(m.getAttribute('content'));
      if (t) meta = t;
    }}
    if (meta) return {{ policy: meta, source: 'meta' }};
    if (HEADER_POLICY !== null) return {{ policy: HEADER_POLICY, source: 'header' }};
    return {{ policy: '', source: 'unknown' }};
  }};
  const candidate = (e) => {{
    if (e.type !== 'click' || e.button !== 0 || e.metaKey || e.ctrlKey || e.shiftKey || e.altKey) return null;
    let link = null;
    for (const n of e.composedPath()) {{
      if (n instanceof HTMLAnchorElement || n instanceof HTMLAreaElement) {{ link = n; break; }}
      if (interactive(n)) return null;
    }}
    if (!link || !link.hasAttribute('href')) return null;
    const doc = link.ownerDocument;
    if (doc !== document) return null;
    let target = link.getAttribute('target');
    if (target === null) {{
      const base = doc.querySelector('base[target]');
      target = base ? base.getAttribute('target') : '';
    }}
    if ((target || '').trim().toLowerCase() !== '_blank') return null;
    const rel = (link.getAttribute('rel') || '').toLowerCase().split(/\s+/).filter(Boolean);
    if (rel.includes('opener')) return {{ skip: 'the link has rel=opener' }};
    if (link.hasAttribute('download')) return {{ skip: 'the link has download' }};
    if (link.hasAttribute('ping')) return {{ skip: 'the link has ping' }};
    let url;
    try {{ url = new URL(link.href); }} catch (_) {{ return {{ skip: 'the link has no valid href' }}; }}
    if (url.protocol !== 'http:' && url.protocol !== 'https:') return {{ skip: 'the link is ' + url.protocol }};
    let p;
    if (rel.includes('noreferrer')) p = {{ policy: 'no-referrer', source: 'rel' }};
    else if (norm(link.referrerPolicy)) p = {{ policy: norm(link.referrerPolicy), source: 'attribute' }};
    else p = documentPolicy(doc);
    return {{
      url: url.href,
      referrer: String(doc.URL || '').split('#')[0],
      policy: p.policy,
      policySource: p.source,
    }};
  }};
  const onCapture = (e) => {{
    if (st.result || st.seen || Date.now() - armedAt > {ttl}) return;
    const c = candidate(e);
    if (!c) return;
    if (c.skip) {{ st.skipped = c.skip; return; }}
    st.seen = c.url;
    const capturedAt = Date.now();
    const onBubble = (ev) => {{
      if (ev !== e) return;
      window.removeEventListener('click', onBubble, false);
      if (st.result) return;
      if (e.defaultPrevented) {{ st.pagePrevented = true; return; }}
      if (Date.now() - capturedAt > {max_dispatch}) {{ st.late = true; return; }}
      e.preventDefault();
      st.result = c;
      st.disarm();
    }};
    window.addEventListener('click', onBubble, false);
  }};
  window.addEventListener('click', onCapture, true);
  const timer = setTimeout(() => st.disarm(), {ttl});
  st.disarm = () => {{
    window.removeEventListener('click', onCapture, true);
    clearTimeout(timer);
  }};
  globalThis[KEY] = st;
  return true;
}})()"#,
        header = serde_json::to_string(&header_policy).unwrap_or_else(|_| "null".into()),
        valid = serde_json::to_string(&POLICIES).unwrap_or_else(|_| "[]".into()),
        ttl = ARM_TTL_MS,
        max_dispatch = MAX_DISPATCH_MS,
    )
}

/// Read once after the click; also disarms the guard.
pub const READ_SCRIPT: &str = r#"(() => {
  const KEY = '__chromeUsePopupGuard';
  const st = globalThis[KEY];
  if (!st) return null;
  st.disarm();
  delete globalThis[KEY];
  return JSON.stringify({
    result: st.result, seen: st.seen, skipped: st.skipped,
    pagePrevented: st.pagePrevented, late: st.late,
  });
})()"#;

/// Where an armed guard lives, so the same context is read after the click.
#[derive(Debug, Clone)]
pub struct ArmedGuard {
    pub session_id: String,
    pub context_id: i64,
}

/// A link the guard took over: open `url` in a background tab.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InterceptedLink {
    pub url: String,
    #[serde(default)]
    pub referrer: String,
    /// Effective referrer policy (`""` = Chrome's default).
    #[serde(default)]
    pub policy: String,
    /// Where the policy came from: `rel`, `attribute`, `meta`, `header`, or
    /// `unknown` (a header policy the daemon did not see; the default is used).
    #[serde(default)]
    pub policy_source: String,
}

impl InterceptedLink {
    /// `Page.navigate`'s `referrer` and `referrerPolicy`: the document URL
    /// under the effective policy, so Chrome computes the same `Referer` as
    /// for the click. A `no-referrer` policy is passed as such rather than
    /// dropping the referrer, so the navigation still takes the CDP path
    /// (ab-connect 0.5.31), which keeps `Sec-Fetch-User: ?1`.
    pub fn referrer(&self) -> Option<(String, &'static str)> {
        if self.referrer.is_empty() {
            return None;
        }
        Some((self.referrer.clone(), cdp_referrer_policy(&self.policy)))
    }
}

/// What the guard saw during the click.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GuardReport {
    pub result: Option<InterceptedLink>,
    /// URL of a `_blank` link the click reached but the guard did not take
    /// over (a listener stopped propagation, or the dispatch was too slow).
    pub seen: Option<String>,
    /// Why a `_blank` link was left to Chrome (`rel=opener`, `download`...).
    pub skipped: Option<String>,
    #[serde(default)]
    pub page_prevented: bool,
    #[serde(default)]
    pub late: bool,
}

impl GuardReport {
    pub fn parse(raw: &Value) -> Option<Self> {
        serde_json::from_str(raw.as_str()?).ok()
    }

    /// Why a `_blank` link the click reached was left to Chrome, if one was.
    /// `None` when the guard took it over or the page cancelled it itself.
    pub fn left_to_chrome(&self) -> Option<String> {
        if self.result.is_some() || self.page_prevented {
            return None;
        }
        if let Some(why) = &self.skipped {
            return Some(why.clone());
        }
        if self.late {
            return Some(format!(
                "the page's click handlers took over {MAX_DISPATCH_MS} ms"
            ));
        }
        self.seen
            .as_ref()
            .map(|_| "a page listener stopped the click before it reached the window".to_string())
    }
}

/// Map a referrer policy token (already normalised by the guard) to
/// `Page.navigate`'s enum. Empty or unknown is Chrome's default,
/// `strict-origin-when-cross-origin`.
pub fn cdp_referrer_policy(html: &str) -> &'static str {
    match html.trim().to_ascii_lowercase().as_str() {
        "no-referrer" | "never" => "noReferrer",
        "no-referrer-when-downgrade" => "noReferrerWhenDowngrade",
        "origin" => "origin",
        "origin-when-cross-origin" | "origin-when-crossorigin" => "originWhenCrossOrigin",
        "same-origin" => "sameOrigin",
        "strict-origin" => "strictOrigin",
        "unsafe-url" | "always" => "unsafeUrl",
        _ => "strictOriginWhenCrossOrigin",
    }
}

/// Arm the guard in `frame_id` (the top frame when `None`) of `session_id`.
/// `header_policies` maps frame ids to the policy their document's response
/// header set, as seen by the daemon. `None` when it could not be armed; the
/// click then goes ahead as before (once — it is never repeated).
pub async fn arm(
    client: &CdpClient,
    session_id: &str,
    frame_id: Option<&str>,
    header_policies: &HashMap<String, String>,
) -> Option<ArmedGuard> {
    let frame_id = match frame_id {
        Some(f) => f.to_string(),
        None => {
            let tree = client
                .send_command("Page.getFrameTree", None, Some(session_id))
                .await
                .ok()?;
            tree.get("frameTree")?
                .get("frame")?
                .get("id")?
                .as_str()?
                .to_string()
        }
    };
    let ctx = client
        .send_command(
            "Page.createIsolatedWorld",
            Some(json!({ "frameId": frame_id, "worldName": WORLD_NAME })),
            Some(session_id),
        )
        .await
        .ok()?
        .get("executionContextId")?
        .as_i64()?;
    let armed = client
        .send_command(
            "Runtime.evaluate",
            Some(json!({
                "expression": arm_script(header_policies.get(&frame_id).map(String::as_str)),
                "contextId": ctx,
                "returnByValue": true,
            })),
            Some(session_id),
        )
        .await
        .ok()?;
    if armed.pointer("/result/value") != Some(&Value::Bool(true)) {
        return None;
    }
    Some(ArmedGuard {
        session_id: session_id.to_string(),
        context_id: ctx,
    })
}

/// Read (and disarm) the guard after the click. `None` when its document is
/// gone (the click navigated the tab) or it cannot be read.
pub async fn read(client: &CdpClient, guard: &ArmedGuard) -> Option<GuardReport> {
    let v = client
        .send_command(
            "Runtime.evaluate",
            Some(json!({
                "expression": READ_SCRIPT,
                "contextId": guard.context_id,
                "returnByValue": true,
            })),
            Some(&guard.session_id),
        )
        .await
        .ok()?;
    GuardReport::parse(v.pointer("/result/value")?)
}

/// What the click says when a guard was armed but could not be read back.
pub const UNREAD_NOTE: &str = "chrome-use could not read back its link guard after the click \
     (the page may have navigated). If the click was on a target=_blank link, it may not have \
     opened; the click was not repeated. Run `tab list`.";

/// The note a click carries when the page opened a tab through Chrome.
/// `reason` says why the guard left it to Chrome; `None` with `opted_in`
/// false means the guard is off ([`BACKGROUND_LINKS_ENV`]).
pub fn chrome_raised_note(reason: Option<&str>, opted_in: bool) -> String {
    let why = match (reason, opted_in) {
        (Some(r), _) => format!(" ({r})"),
        (None, true) => " (window.open, or a link chrome-use cannot open itself)".to_string(),
        (None, false) => String::new(),
    };
    let hint = if opted_in {
        String::new()
    } else {
        format!(
            ". {BACKGROUND_LINKS_ENV}=1 makes chrome-use open plain target=_blank links in a \
             background tab instead, with known differences from Chrome's own click (see \
             `click --help`)"
        )
    };
    format!(
        "the page opened this tab itself{why}, and Chrome brings its window to the front \
         when a page opens a tab, so it may have come over the app the user is in (#468){hint}"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn referrer_policy_maps_tokens_and_defaults() {
        assert_eq!(cdp_referrer_policy(""), "strictOriginWhenCrossOrigin");
        assert_eq!(cdp_referrer_policy("bogus"), "strictOriginWhenCrossOrigin");
        assert_eq!(cdp_referrer_policy("No-Referrer"), "noReferrer");
        assert_eq!(cdp_referrer_policy("unsafe-url"), "unsafeUrl");
        assert_eq!(cdp_referrer_policy("origin"), "origin");
        assert_eq!(cdp_referrer_policy("same-origin"), "sameOrigin");
        assert_eq!(cdp_referrer_policy("strict-origin"), "strictOrigin");
        assert_eq!(
            cdp_referrer_policy("origin-when-cross-origin"),
            "originWhenCrossOrigin"
        );
        assert_eq!(
            cdp_referrer_policy("no-referrer-when-downgrade"),
            "noReferrerWhenDowngrade"
        );
    }

    #[test]
    fn no_referrer_still_goes_through_cdp_with_its_policy() {
        let mut link = InterceptedLink {
            url: "https://b.test/".into(),
            referrer: "https://a.test/page".into(),
            policy: String::new(),
            policy_source: "header".into(),
        };
        assert_eq!(
            link.referrer(),
            Some(("https://a.test/page".into(), "strictOriginWhenCrossOrigin"))
        );
        link.policy = "no-referrer".into();
        assert_eq!(
            link.referrer(),
            Some(("https://a.test/page".into(), "noReferrer"))
        );
        link.referrer = String::new();
        assert_eq!(link.referrer(), None);
    }

    #[test]
    fn document_policy_comes_from_the_documents_response_header() {
        assert_eq!(header_referrer_policy("no-referrer"), "no-referrer");
        assert_eq!(
            header_referrer_policy("unsafe-url, bogus, Same-Origin"),
            "same-origin"
        );
        assert_eq!(header_referrer_policy("origin, bogus"), "origin");
        assert_eq!(header_referrer_policy("bogus"), "");
        let ev = json!({
            "type": "Document", "frameId": "F1",
            "response": { "headers": { "Referrer-Policy": "no-referrer" } }
        });
        assert_eq!(
            document_referrer_policy(&ev),
            Some(("F1".into(), "no-referrer".into()))
        );
        let none = json!({ "type": "Document", "frameId": "F2", "response": { "headers": {} } });
        assert_eq!(
            document_referrer_policy(&none),
            Some(("F2".into(), "".into()))
        );
        let script = json!({ "type": "Script", "frameId": "F1", "response": { "headers": {} } });
        assert_eq!(document_referrer_policy(&script), None);
    }

    #[test]
    fn report_parses_and_names_why_a_link_went_to_chrome() {
        let taken = GuardReport::parse(&json!(
            r#"{"result":{"url":"https://b.test/","referrer":"https://a.test/","policy":"","policySource":"header"},"seen":"https://b.test/","skipped":null,"pagePrevented":false,"late":false}"#
        ))
        .unwrap();
        assert_eq!(taken.result.as_ref().unwrap().url, "https://b.test/");
        assert_eq!(taken.result.as_ref().unwrap().policy_source, "header");
        assert_eq!(taken.left_to_chrome(), None);

        let stopped = GuardReport::parse(&json!(
            r#"{"result":null,"seen":"https://b.test/","skipped":null,"pagePrevented":false,"late":false}"#
        ))
        .unwrap();
        assert!(stopped
            .left_to_chrome()
            .unwrap()
            .contains("stopped the click"));

        let opener = GuardReport::parse(&json!(
            r#"{"result":null,"seen":null,"skipped":"the link has rel=opener","pagePrevented":false,"late":false}"#
        ))
        .unwrap();
        assert_eq!(opener.left_to_chrome().unwrap(), "the link has rel=opener");

        let late = GuardReport::parse(&json!(
            r#"{"result":null,"seen":"https://b.test/","skipped":null,"pagePrevented":false,"late":true}"#
        ))
        .unwrap();
        assert!(late.left_to_chrome().unwrap().contains("took over"));

        // The page cancelled the link itself: nothing opened, nothing to say.
        let prevented = GuardReport::parse(&json!(
            r#"{"result":null,"seen":"https://b.test/","skipped":null,"pagePrevented":true,"late":false}"#
        ))
        .unwrap();
        assert_eq!(prevented.left_to_chrome(), None);

        let none = GuardReport::parse(&json!(
            r#"{"result":null,"seen":null,"skipped":null,"pagePrevented":false,"late":false}"#
        ))
        .unwrap();
        assert_eq!(none.left_to_chrome(), None);
        assert_eq!(GuardReport::parse(&Value::Null), None);
    }

    #[test]
    fn arm_script_carries_its_limits_and_the_header_policy() {
        let s = arm_script(None);
        assert!(s.contains(&format!("> {ARM_TTL_MS}")));
        assert!(s.contains(&format!("> {MAX_DISPATCH_MS}")));
        assert!(s.contains("'the link has rel=opener'"));
        assert!(s.contains("const HEADER_POLICY = null;"));
        assert!(s.contains("if (doc !== document) return null;"));
        assert!(!s.contains("{{"));
        assert!(arm_script(Some("no-referrer")).contains("const HEADER_POLICY = \"no-referrer\";"));
        assert!(arm_script(Some("")).contains("const HEADER_POLICY = \"\";"));
    }

    #[test]
    fn the_note_names_the_opt_in_only_when_it_is_off() {
        let off = chrome_raised_note(None, false);
        assert!(off.contains("#468"));
        assert!(off.contains(BACKGROUND_LINKS_ENV));
        let on = chrome_raised_note(Some("the link has rel=opener"), true);
        assert!(on.contains("(the link has rel=opener)"));
        assert!(!on.contains(BACKGROUND_LINKS_ENV));
    }
}
