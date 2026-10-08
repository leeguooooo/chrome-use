//! Keep a `target=_blank` link an agent clicks from raising Chrome (#468).
//!
//! When a page opens a tab or pop-up in response to a click, Chrome adds it
//! through its own window-opening path, which always shows (activates) the
//! window it lands in, and with it the whole Chrome app — whatever the
//! disposition (a background tab from a Cmd-click or a middle click does it
//! too), whether the window is minimized or off-screen, and before any
//! extension or CDP event about the new tab arrives. Measured on Chrome for
//! Testing 155: in every such case Chrome became the frontmost app 30–60 ms
//! after the click. A tab chrome-use creates itself in the background agent
//! window does not activate anything.
//!
//! So for the one case where it can be done without changing what the page
//! sees, the click's default action is taken over: a plain left click on an
//! `<a>`/`<area>` whose effective target is `_blank`, with an http(s) href,
//! no `rel=opener` (so Chrome would open it without an opener anyway), no
//! `download` and no `ping`. The click is delivered as before and every page
//! listener runs; only after the last of them (a listener added on `window`
//! for the bubble phase while the event is in flight) and only when no page
//! listener called `preventDefault()`, the guard cancels Chrome's own
//! navigation and reports the link. chrome-use then opens the same URL in a
//! new background tab of the session, with the document as referrer under
//! the link's referrer policy.
//!
//! Everything else — `window.open`, `rel=opener`, a named target, a form
//! with `target=_blank`, a link whose click a page listener stopped from
//! propagating to `window` — still goes through Chrome, which raises its
//! window. The click reports that (`openedTabWarning`) rather than hiding
//! it.
//!
//! The guard lives in an isolated world, so page scripts can neither see nor
//! tamper with it, and it disarms itself after one interception, when the
//! daemon reads it, or after [`ARM_TTL_MS`].

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

/// Installed right before the click. Returns `true` once armed.
pub fn arm_script() -> String {
    format!(
        r#"(() => {{
  const KEY = '__chromeUsePopupGuard';
  const prev = globalThis[KEY];
  if (prev && typeof prev.disarm === 'function') prev.disarm();
  const armedAt = Date.now();
  const st = {{ result: null, seen: null, skipped: null, pagePrevented: false, late: false }};
  const interactive = (n) =>
    n instanceof HTMLButtonElement || n instanceof HTMLInputElement ||
    n instanceof HTMLSelectElement || n instanceof HTMLTextAreaElement ||
    n instanceof HTMLLabelElement ||
    (n instanceof HTMLElement && (n.localName === 'summary' || n.isContentEditable));
  const metaPolicy = (doc) => {{
    const metas = doc.querySelectorAll('meta[name="referrer" i]');
    return metas.length ? (metas[metas.length - 1].getAttribute('content') || '').trim().toLowerCase() : '';
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
    let target = link.getAttribute('target');
    if (target === null) {{
      const base = doc.querySelector('base[target]');
      target = base ? base.getAttribute('target') : '';
    }}
    if ((target || '').trim().toLowerCase() !== '_blank') return null;
    const rel = (link.getAttribute('rel') || '').toLowerCase().split(/\s+/).filter(Boolean);
    if (rel.includes('opener')) return {{ skip: 'rel=opener' }};
    if (link.hasAttribute('download')) return {{ skip: 'download' }};
    if (link.hasAttribute('ping')) return {{ skip: 'ping' }};
    let url;
    try {{ url = new URL(link.href); }} catch (_) {{ return {{ skip: 'href' }}; }}
    if (url.protocol !== 'http:' && url.protocol !== 'https:') return {{ skip: 'scheme ' + url.protocol }};
    const referrer = String(doc.URL || '').split('#')[0];
    return {{
      url: url.href,
      noreferrer: rel.includes('noreferrer'),
      referrer,
      policy: (link.referrerPolicy || link.getAttribute('referrerpolicy') || metaPolicy(doc) || '').toLowerCase(),
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
    pub noreferrer: bool,
    #[serde(default)]
    pub referrer: String,
    #[serde(default)]
    pub policy: String,
}

impl InterceptedLink {
    /// `Page.navigate`'s `referrer` and `referrerPolicy` for this link: what
    /// Chrome would have sent for the click. `None` for `rel=noreferrer`, and
    /// for a link that may be same-site with the page (see
    /// [`maybe_same_site`]).
    ///
    /// The choice is about cookies. A `Page.navigate` with a referrer is
    /// treated as a cross-site navigation (`Sec-Fetch-Site: cross-site`, no
    /// `SameSite=Strict` cookies) whatever the referrer is, which matches
    /// Chrome's own handling of a cross-site link but would leave a same-site
    /// link without its Strict cookies, i.e. possibly signed out. Without a
    /// referrer it is a browser navigation (`none`, every cookie sent), the
    /// same as the user typing the URL. So a same-site link loses only its
    /// `Referer` and `document.referrer`, never its session.
    pub fn referrer(&self) -> Option<(String, &'static str)> {
        if self.noreferrer || self.referrer.is_empty() {
            return None;
        }
        if maybe_same_site(&self.url, &self.referrer) {
            return None;
        }
        let policy = cdp_referrer_policy(&self.policy);
        if policy == "noReferrer" {
            return None;
        }
        Some((self.referrer.clone(), policy))
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
            return Some(format!("the link has {why}"));
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

/// Whether `a` and `b` may be the same site. Without the public suffix list
/// this errs towards "same": hosts that are equal, nested (`x.a.com` and
/// `a.com`) or share their last two labels (`x.a.com`, `y.a.com`, but also
/// `a.co.uk` and `b.co.uk`) count as the same site. Different IP addresses,
/// or anything unparsable, count as different only when both parse.
pub fn maybe_same_site(a: &str, b: &str) -> bool {
    let host = |u: &str| {
        url::Url::parse(u).ok().and_then(|u| {
            u.host_str()
                .map(|h| h.trim_end_matches('.').to_ascii_lowercase())
        })
    };
    let (Some(a), Some(b)) = (host(a), host(b)) else {
        return true;
    };
    if a == b || a.ends_with(&format!(".{b}")) || b.ends_with(&format!(".{a}")) {
        return true;
    }
    let is_ip = |h: &str| h.parse::<std::net::IpAddr>().is_ok() || h.starts_with('[');
    if is_ip(&a) || is_ip(&b) {
        return false;
    }
    fn tail(h: &str) -> Option<Vec<&str>> {
        let labels: Vec<&str> = h.rsplit('.').take(2).collect();
        (labels.len() == 2).then_some(labels)
    }
    matches!((tail(&a), tail(&b)), (Some(x), Some(y)) if x == y)
}

/// Map an HTML referrer policy (attribute or `<meta name=referrer>` value,
/// including the legacy meta keywords) to `Page.navigate`'s enum. Unknown or
/// empty is Chrome's default, `strict-origin-when-cross-origin`. A policy the
/// document set only through an HTTP header is not visible to the page, so it
/// also falls back to the default.
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
/// `None` when it could not be armed; the click then goes ahead as before.
pub async fn arm(
    client: &CdpClient,
    session_id: &str,
    frame_id: Option<&str>,
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
                "expression": arm_script(),
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

/// The note a click carries when the page opened a tab through Chrome.
pub fn chrome_raised_note(reason: Option<&str>) -> String {
    let why = match reason {
        Some(r) => format!(" ({r})"),
        None => " (window.open, or a link chrome-use cannot open itself)".to_string(),
    };
    format!(
        "the page opened this tab itself{why}, and Chrome brings its window to the front \
         when a page opens a tab, so it may have come over the app the user is in (#468)"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn referrer_policy_maps_html_values_and_defaults() {
        assert_eq!(cdp_referrer_policy(""), "strictOriginWhenCrossOrigin");
        assert_eq!(cdp_referrer_policy("bogus"), "strictOriginWhenCrossOrigin");
        assert_eq!(cdp_referrer_policy("No-Referrer"), "noReferrer");
        assert_eq!(cdp_referrer_policy("never"), "noReferrer");
        assert_eq!(cdp_referrer_policy("always"), "unsafeUrl");
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
    fn noreferrer_and_no_referrer_policy_send_none() {
        let mut link = InterceptedLink {
            url: "https://b.test/".into(),
            noreferrer: false,
            referrer: "https://a.test/page".into(),
            policy: String::new(),
        };
        assert_eq!(
            link.referrer(),
            Some(("https://a.test/page".into(), "strictOriginWhenCrossOrigin"))
        );
        link.policy = "no-referrer".into();
        assert_eq!(link.referrer(), None);
        link.policy = "origin".into();
        link.noreferrer = true;
        assert_eq!(link.referrer(), None);
        link.noreferrer = false;
        link.referrer = String::new();
        assert_eq!(link.referrer(), None);
    }

    #[test]
    fn same_site_links_go_without_referrer_so_strict_cookies_still_flow() {
        assert!(maybe_same_site("https://a.test/x", "https://a.test/y"));
        assert!(maybe_same_site("https://www.a.test/", "https://a.test/"));
        assert!(maybe_same_site("https://x.a.test/", "https://y.a.test/"));
        assert!(maybe_same_site(
            "http://127.0.0.1:1/",
            "http://127.0.0.1:2/"
        ));
        assert!(maybe_same_site("not a url", "https://a.test/"));
        assert!(!maybe_same_site("https://b.test/", "https://a.test/"));
        assert!(!maybe_same_site(
            "http://localhost:1/",
            "http://127.0.0.1:1/"
        ));
        assert!(!maybe_same_site("http://127.0.0.2/", "http://127.0.0.1/"));
        let same = InterceptedLink {
            url: "https://docs.a.test/page".into(),
            noreferrer: false,
            referrer: "https://www.a.test/".into(),
            policy: "unsafe-url".into(),
        };
        assert_eq!(same.referrer(), None);
    }

    #[test]
    fn report_parses_and_names_why_a_link_went_to_chrome() {
        let taken = GuardReport::parse(&json!(
            r#"{"result":{"url":"https://b.test/","noreferrer":false,"referrer":"https://a.test/","policy":""},"seen":"https://b.test/","skipped":null,"pagePrevented":false,"late":false}"#
        ))
        .unwrap();
        assert_eq!(taken.result.as_ref().unwrap().url, "https://b.test/");
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
            r#"{"result":null,"seen":null,"skipped":"rel=opener","pagePrevented":false,"late":false}"#
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

        // Nothing link-like was clicked.
        let none = GuardReport::parse(&json!(
            r#"{"result":null,"seen":null,"skipped":null,"pagePrevented":false,"late":false}"#
        ))
        .unwrap();
        assert_eq!(none.left_to_chrome(), None);
        assert_eq!(GuardReport::parse(&Value::Null), None);
    }

    #[test]
    fn arm_script_carries_its_limits() {
        let s = arm_script();
        assert!(s.contains(&format!("> {ARM_TTL_MS}")));
        assert!(s.contains(&format!("> {MAX_DISPATCH_MS}")));
        assert!(s.contains("'rel=opener'"));
        assert!(!s.contains("{{"));
    }
}
