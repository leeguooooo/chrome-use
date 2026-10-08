//! Open cross-site `target=_blank` links an agent clicks in a background
//! tab, so Chrome does not come to the front (#468).
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
//! So by default ([`LinkMode::CrossSite`]) only links to another
//! registrable domain are taken over: the daemon classifies, before the
//! click, the clicking frame's URL and each link the click may follow, and
//! both must have a registrable domain (eTLD+1 by the Public Suffix List)
//! and differ. For those the request matches Chrome's own click except for
//! `history.length`. Same registrable domain (subdomains, another scheme),
//! IP addresses, `localhost`, hosts outside the list, a link the page
//! changes during the click to a host not classified, a link a ChooseBrowser
//! rule applies to, and a page whose header referrer policy the daemon did
//! not see keep Chrome's click, which raises Chrome; the click says so.
//! [`BACKGROUND_LINKS_ENV`] `=all` also takes same-site links, `=off` none.
//!
//! The click's default action is taken over only for a plain left
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

/// Which `target=_blank` links chrome-use opens in a background tab. Read by
/// the daemon from its own environment, so a change takes effect for a new
/// session (or after the daemon restarts), not for a running one.
pub const BACKGROUND_LINKS_ENV: &str = "AGENT_BROWSER_BACKGROUND_LINKS";

/// The [`BACKGROUND_LINKS_ENV`] setting.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinkMode {
    /// `0` / `off`: every page-opened tab goes through Chrome.
    Off,
    /// Unset / `cross-site` (default): links to another registrable domain.
    CrossSite,
    /// `1` / `all`: same-site links too (they lose `SameSite=Strict` cookies).
    All,
}

impl LinkMode {
    /// Parse the env value; anything unrecognised is the default.
    pub fn parse(value: Option<&str>) -> Self {
        match value.map(|v| v.trim().to_ascii_lowercase()).as_deref() {
            Some("0" | "off" | "false" | "no" | "none") => Self::Off,
            Some("1" | "all" | "true" | "yes" | "on") => Self::All,
            _ => Self::CrossSite,
        }
    }

    /// The mode this daemon runs with.
    pub fn from_env() -> Self {
        Self::parse(std::env::var(BACKGROUND_LINKS_ENV).ok().as_deref())
    }
}

/// The registrable domain (eTLD+1, Public Suffix List including private
/// suffixes) of an http(s) page URL, lowercased. `None` when it cannot be
/// told: another scheme or an opaque document, an IP address, or a host that
/// is itself a public suffix or has no registrable domain (`localhost`).
pub fn page_site(url: &str) -> Option<String> {
    let u = url::Url::parse(url).ok()?;
    if u.scheme() != "http" && u.scheme() != "https" {
        return None;
    }
    let host = match u.host()? {
        url::Host::Domain(d) => d.trim_end_matches('.').to_ascii_lowercase(),
        _ => return None,
    };
    let site = psl::domain_str(&host)?;
    // A domain the list knows nothing about (a single label, or a TLD
    // outside it) is left to Chrome too.
    psl::suffix(host.as_bytes())
        .filter(|s| s.is_known())
        .map(|_| site.to_string())
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
/// - `header_policy`: the policy the guard's document got from its response
///   header (`Some("")` for none), `None` when the daemon did not see it.
/// - `hosts`: for each link host the daemon classified before the click
///   ([`classify_link`]), `"cross"` when the link may be taken over, else the
///   reason it keeps Chrome's click. A host not in the map (the page changed
///   the link during the click) keeps Chrome's click.
///
/// The decision is made as late as the DOM allows. The guard follows the
/// event along its path: from each of its listeners it adds the next one, on
/// the next node and phase of the path, so that each is added after every
/// page listener that could still be added there and runs after all of them.
/// The last one, on `window` in the bubble phase, re-reads the link (href,
/// target, rel, referrer policy, as the page left them) and only then, if
/// no page listener cancelled the click, cancels Chrome's own action.
/// A listener that stops propagation means the last one never runs: the
/// click stays Chrome's.
pub fn arm_script(
    header_policy: Option<&str>,
    mode: LinkMode,
    hosts: &HashMap<String, String>,
) -> String {
    format!(
        r#"(() => {{
  const KEY = '__chromeUsePopupGuard';
  const HEADER_POLICY = {header};
  const ALL = {all};
  const HOSTS = {hosts};
  const prev = globalThis[KEY];
  if (prev && typeof prev.disarm === 'function') prev.disarm();
  const armedAt = Date.now();
  const st = {{ result: null, seen: null, skipped: null, pagePrevented: false, late: false, reached: false, done: false }};
  const added = [];
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
  const linkOf = (e) => {{
    if (e.type !== 'click' || e.button !== 0 || e.metaKey || e.ctrlKey || e.shiftKey || e.altKey) return null;
    for (const n of e.composedPath()) {{
      if (n instanceof HTMLAnchorElement || n instanceof HTMLAreaElement) return n;
      if (interactive(n)) return null;
    }}
    return null;
  }};
  const effectiveTarget = (link) => {{
    let target = link.getAttribute('target');
    if (target === null) {{
      const base = link.ownerDocument.querySelector('base[target]');
      target = base ? base.getAttribute('target') : '';
    }}
    return (target || '').trim().toLowerCase();
  }};
  // Read the link as it is NOW: called once, at the very end of dispatch.
  const candidate = (e) => {{
    const link = linkOf(e);
    if (!link || !link.hasAttribute('href')) return null;
    const doc = link.ownerDocument;
    if (doc !== document) return null;
    if (effectiveTarget(link) !== '_blank') return null;
    const rel = (link.getAttribute('rel') || '').toLowerCase().split(/\s+/).filter(Boolean);
    if (rel.includes('opener')) return {{ skip: 'the link has rel=opener' }};
    if (link.hasAttribute('download')) return {{ skip: 'the link has download' }};
    if (link.hasAttribute('ping')) return {{ skip: 'the link has ping' }};
    let url;
    try {{ url = new URL(link.href); }} catch (_) {{ return {{ skip: 'the link has no valid href' }}; }}
    if (url.protocol !== 'http:' && url.protocol !== 'https:') return {{ skip: 'the link is ' + url.protocol }};
    if (!ALL) {{
      const h = url.hostname.toLowerCase().replace(/\.$/, '');
      const cls = Object.prototype.hasOwnProperty.call(HOSTS, h) ? HOSTS[h] : null;
      if (cls !== 'cross') {{
        return {{ skip: cls || "the page changed the link during the click, so chrome-use could not classify its site" }};
      }}
    }}
    let p;
    if (rel.includes('noreferrer')) p = {{ policy: 'no-referrer', source: 'rel' }};
    else if (norm(link.referrerPolicy)) p = {{ policy: norm(link.referrerPolicy), source: 'attribute' }};
    else p = documentPolicy(doc);
    if (!ALL && p.source === 'unknown') {{
      return {{ skip: "chrome-use did not see the page's referrer policy header" }};
    }}
    return {{
      url: url.href,
      referrer: String(doc.URL || '').split('#')[0],
      policy: p.policy,
      policySource: p.source,
    }};
  }};
  const listen = (node, capture, fn) => {{
    node.addEventListener('click', fn, capture);
    added.push([node, fn, capture]);
  }};
  const onCapture = (e) => {{
    if (st.done || Date.now() - armedAt > {ttl}) return;
    const first = linkOf(e);
    if (!first) return;
    st.done = true;
    st.seen = first.href || true;
    st.blankAtCapture = effectiveTarget(first) === '_blank';
    const capturedAt = Date.now();
    // The stages the event still goes through: the capture pass down the
    // path (window first, where we are now), then the bubble pass back up.
    const path = e.composedPath();
    const stages = [];
    for (let i = path.length - 1; i >= 0; i--) stages.push([path[i], true]);
    for (let i = 0; i < path.length; i++) stages.push([path[i], false]);
    const decide = () => {{
      st.reached = true;
      if (e.defaultPrevented) {{ st.pagePrevented = true; return; }}
      if (Date.now() - capturedAt > {max_dispatch}) {{ st.late = true; return; }}
      const c = candidate(e);
      if (!c) return;
      if (c.skip) {{ st.skipped = c.skip; return; }}
      e.preventDefault();
      st.result = c;
    }};
    const stage = (k) => {{
      const [node, capture] = stages[k];
      const fn = (ev) => {{
        if (ev !== e) return;
        node.removeEventListener('click', fn, capture);
        if (k === stages.length - 1) {{ decide(); st.disarm(); return; }}
        const [nn, nc] = stages[k + 1];
        listen(nn, nc, stage(k + 1));
      }};
      return fn;
    }};
    // Stage 0 is this window capture listener itself; arm stage 1.
    if (stages.length > 1) listen(stages[1][0], stages[1][1], stage(1));
  }};
  window.addEventListener('click', onCapture, true);
  const timer = setTimeout(() => st.disarm(), {ttl});
  st.disarm = () => {{
    window.removeEventListener('click', onCapture, true);
    for (const [node, fn, capture] of added) node.removeEventListener('click', fn, capture);
    added.length = 0;
    clearTimeout(timer);
  }};
  globalThis[KEY] = st;
  return true;
}})()"#,
        header = serde_json::to_string(&header_policy).unwrap_or_else(|_| "null".into()),
        all = mode == LinkMode::All,
        hosts = serde_json::to_string(hosts).unwrap_or_else(|_| "{}".into()),
        valid = serde_json::to_string(&POLICIES).unwrap_or_else(|_| "[]".into()),
        ttl = ARM_TTL_MS,
        max_dispatch = MAX_DISPATCH_MS,
    )
}

/// Disarm without reading: the click failed or stopped early (a pending
/// dialog), so nothing must keep acting on later clicks.
pub const DISARM_SCRIPT: &str = r#"(() => {
  const KEY = '__chromeUsePopupGuard';
  const st = globalThis[KEY];
  if (st && typeof st.disarm === 'function') st.disarm();
  delete globalThis[KEY];
  return true;
})()"#;

/// Read once after the click; also disarms the guard.
pub const READ_SCRIPT: &str = r#"(() => {
  const KEY = '__chromeUsePopupGuard';
  const st = globalThis[KEY];
  if (!st) return null;
  st.disarm();
  delete globalThis[KEY];
  return JSON.stringify({
    result: st.result, seen: st.seen ? String(st.seen) : null, skipped: st.skipped,
    pagePrevented: st.pagePrevented, late: st.late, reached: st.reached,
    blankAtCapture: !!st.blankAtCapture,
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
    /// The href of the link the click event went through, if any.
    pub seen: Option<String>,
    /// Why a `_blank` link was left to Chrome (same site, `rel=opener`...).
    pub skipped: Option<String>,
    #[serde(default)]
    pub page_prevented: bool,
    #[serde(default)]
    pub late: bool,
    /// The guard's last listener ran: the event got to the end of its path.
    #[serde(default)]
    pub reached: bool,
    /// The link targeted `_blank` when the click started.
    #[serde(default)]
    pub blank_at_capture: bool,
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
        (self.seen.is_some() && self.blank_at_capture && !self.reached)
            .then(|| "a page listener stopped the click before it reached the window".to_string())
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

/// Why a link keeps Chrome's own click although it targets `_blank`, or
/// `Ok(host)` when it may be opened in the background ([`LinkMode::CrossSite`]):
/// only when BOTH the page and the link have a registrable domain by the
/// Public Suffix List and the two differ. `Err((host, reason))`; `None` for a
/// URL that is not http(s) (the guard leaves those to Chrome anyway).
pub fn classify_link(
    page_site: Option<&str>,
    link_url: &str,
) -> Option<Result<String, (String, String)>> {
    let u = url::Url::parse(link_url).ok()?;
    if u.scheme() != "http" && u.scheme() != "https" {
        return None;
    }
    let host = u.host_str()?.trim_end_matches('.').to_ascii_lowercase();
    let Some(page) = page_site else {
        return Some(Err((
            host,
            "chrome-use could not classify the page's site".to_string(),
        )));
    };
    let Some(link) = page_site_of_host(&host) else {
        return Some(Err((
            host,
            "chrome-use could not classify the link's site (an IP address, localhost, or a \
             host outside the Public Suffix List)"
                .to_string(),
        )));
    };
    if link == page {
        return Some(Err((
            host,
            "the link stays on the same site, so it keeps Chrome's own click and its SameSite \
             cookies"
                .to_string(),
        )));
    }
    Some(Ok(host))
}

/// [`page_site`] for a bare host.
fn page_site_of_host(host: &str) -> Option<String> {
    page_site(&format!("https://{host}/"))
}

/// The guard's host table: each candidate link URL's host mapped to `"cross"`
/// (may be taken over) or the reason it keeps Chrome's click. `refuse` is
/// the ChooseBrowser check for a URL (`Some(reason)` when a rule would send
/// it elsewhere); such a link keeps Chrome's click too.
pub fn host_table(
    page_site: Option<&str>,
    link_urls: &[String],
    refuse: &dyn Fn(&str) -> Option<String>,
) -> HashMap<String, String> {
    let mut hosts = HashMap::new();
    for url in link_urls {
        match classify_link(page_site, url) {
            Some(Ok(host)) => {
                let verdict = match refuse(url) {
                    Some(why) => format!("a ChooseBrowser rule applies to the link ({why})"),
                    None => "cross".to_string(),
                };
                hosts.insert(host, verdict);
            }
            Some(Err((host, why))) => {
                hosts.insert(host, why);
            }
            None => {}
        }
    }
    hosts
}

/// The links a click on an element may follow, read before the click: the
/// element's own link (`closest('a[href],area[href]')`) and links inside it.
pub const CANDIDATE_LINKS_FUNCTION: &str = r#"function() {
  const out = [];
  const own = this.closest && this.closest('a[href],area[href]');
  if (own) out.push(own.href);
  if (this.querySelectorAll) {
    for (const a of this.querySelectorAll('a[href],area[href]')) {
      if (out.length >= 20) break;
      out.push(a.href);
    }
  }
  return out;
}"#;

/// Arm the guard in `frame_id` (the top frame when `None`) of `session_id`.
/// `header_policies` maps frame ids to the policy their document's response
/// header set, as seen by the daemon. `link_urls` are the links the click may
/// follow ([`CANDIDATE_LINKS_FUNCTION`]); `refuse` is the ChooseBrowser check.
/// `None` when it could not be armed; the click then goes ahead as before
/// (once — it is never repeated).
pub async fn arm(
    client: &CdpClient,
    session_id: &str,
    frame_id: Option<&str>,
    header_policies: &HashMap<String, String>,
    mode: LinkMode,
    link_urls: &[String],
    refuse: &dyn Fn(&str) -> Option<String>,
) -> Option<ArmedGuard> {
    if mode == LinkMode::Off {
        return None;
    }
    let tree = client
        .send_command("Page.getFrameTree", None, Some(session_id))
        .await
        .ok()?;
    let root = tree.get("frameTree")?;
    let (frame_id, frame_url) = match frame_id {
        Some(f) => (f.to_string(), frame_url(root, f)?),
        None => {
            let frame = root.get("frame")?;
            (
                frame.get("id")?.as_str()?.to_string(),
                frame.get("url")?.as_str()?.to_string(),
            )
        }
    };
    let hosts = host_table(page_site(&frame_url).as_deref(), link_urls, refuse);
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
                "expression": arm_script(
                    header_policies.get(&frame_id).map(String::as_str),
                    mode,
                    &hosts,
                ),
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

/// Make an armed guard inert without reading it (the click failed, or
/// stopped at a pending dialog). Bounded: a renderer that does not answer is
/// left to the guard's own TTL.
pub async fn disarm(client: &CdpClient, guard: &ArmedGuard) {
    let _ = tokio::time::timeout(
        std::time::Duration::from_millis(1500),
        client.send_command(
            "Runtime.evaluate",
            Some(json!({
                "expression": DISARM_SCRIPT,
                "contextId": guard.context_id,
                "returnByValue": true,
            })),
            Some(&guard.session_id),
        ),
    )
    .await;
}

/// The URL of frame `id` in a `Page.getFrameTree` node.
fn frame_url(node: &Value, id: &str) -> Option<String> {
    let frame = node.get("frame")?;
    if frame.get("id").and_then(Value::as_str) == Some(id) {
        return frame.get("url").and_then(Value::as_str).map(str::to_string);
    }
    node.get("childFrames")?
        .as_array()?
        .iter()
        .find_map(|c| frame_url(c, id))
}

/// Read (and disarm) the guard after the click. `None` when its document is
/// gone (the click navigated the tab) or it cannot be read.
pub async fn read(client: &CdpClient, guard: &ArmedGuard) -> ReadOutcome {
    let v = match client
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
    {
        Ok(v) => v,
        Err(e) if document_gone(&e) => return ReadOutcome::Gone,
        Err(_) => return ReadOutcome::Failed,
    };
    match v.pointer("/result/value").and_then(GuardReport::parse) {
        Some(r) => ReadOutcome::Report(r),
        None => ReadOutcome::Failed,
    }
}

/// The result of reading the guard back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReadOutcome {
    Report(GuardReport),
    /// The guard's document is gone: the click navigated the frame. A link
    /// the guard took over cancels Chrome's navigation, so the frame did not
    /// navigate because of it.
    Gone,
    /// Unknown: the guard may have taken a link over and nobody opens it.
    Failed,
}

/// Whether a CDP error says the execution context no longer exists.
pub fn document_gone(error: &str) -> bool {
    let e = error.to_ascii_lowercase();
    e.contains("cannot find context") || e.contains("execution context was destroyed")
}

/// How a failed navigation of the link's tab is reported: `true` when Chrome
/// said it failed (the tab did not load the link), `false` when the outcome
/// is unknown (a timeout or a lost reply, after which the tab may well have
/// navigated). Only a known failure may suggest opening the link again.
pub fn navigation_failure_is_known(error: &str) -> bool {
    error.starts_with("Navigation failed:")
}

/// The warning after trying to put the session back on the clicked tab.
/// `switched` is the switch's result, `back` whether the session's pin is
/// now the clicked tab, `at` the tab it is actually pinned to.
pub fn return_warning(switched: Result<(), String>, back: bool, at: &str) -> Option<String> {
    match switched {
        Ok(()) if back => None,
        result => {
            let why = result
                .err()
                .unwrap_or_else(|| "the session moved elsewhere".to_string());
            Some(format!(
                "the session could not return to the clicked tab ({why}); it is on {at}. Run \
                 `tab list`."
            ))
        }
    }
}

/// What the click says when a guard was armed but could not be read back.
pub const UNREAD_NOTE: &str = "chrome-use could not read back its link guard after the click. \
     If the click was on a target=_blank link, it may not have opened; the click was not \
     repeated. Run `tab list`.";

/// The note a click carries when the page opened a tab through Chrome.
/// `reason` says why the guard left it to Chrome.
pub fn chrome_raised_note(reason: Option<&str>, mode: LinkMode) -> String {
    let why = match (reason, mode) {
        (Some(r), _) => format!(" ({r})"),
        (None, LinkMode::Off) => String::new(),
        (None, _) => " (window.open, or a link chrome-use cannot open itself)".to_string(),
    };
    let same_site = reason.is_some_and(|r| r.contains("same site"));
    let hint = match mode {
        LinkMode::Off => format!(
            ". {BACKGROUND_LINKS_ENV} is off; unset it to have chrome-use open cross-site \
             target=_blank links in a background tab"
        ),
        LinkMode::CrossSite if same_site => format!(
            ". {BACKGROUND_LINKS_ENV}=all also opens same-site links in a background tab, but \
             they then lose their SameSite=Strict cookies (see `click --help`)"
        ),
        _ => String::new(),
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
            r#"{"result":null,"seen":"https://b.test/","skipped":null,"pagePrevented":false,"late":false,"reached":false,"blankAtCapture":true}"#
        ))
        .unwrap();
        assert!(stopped
            .left_to_chrome()
            .unwrap()
            .contains("stopped the click"));
        // The click went through a same-tab link and got to the end: no note.
        let same_tab = GuardReport::parse(&json!(
            r#"{"result":null,"seen":"https://b.test/","skipped":null,"pagePrevented":false,"late":false,"reached":true,"blankAtCapture":false}"#
        ))
        .unwrap();
        assert_eq!(same_tab.left_to_chrome(), None);

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
        let mut hosts = HashMap::new();
        hosts.insert("b.example.org".to_string(), "cross".to_string());
        let s = arm_script(None, LinkMode::CrossSite, &hosts);
        assert!(s.contains(&format!("> {ARM_TTL_MS}")));
        assert!(s.contains(&format!("> {MAX_DISPATCH_MS}")));
        assert!(s.contains("'the link has rel=opener'"));
        assert!(s.contains("const HEADER_POLICY = null;"));
        assert!(s.contains("const ALL = false;"));
        assert!(s.contains("const HOSTS = {\"b.example.org\":\"cross\"};"));
        assert!(s.contains("if (doc !== document) return null;"));
        // Cross-site mode never guesses a header policy it did not see.
        assert!(s.contains("if (!ALL && p.source === 'unknown')"));
        // The decision is made by the last stage, from the link as it is then.
        assert!(s.contains("if (k === stages.length - 1) { decide(); st.disarm(); return; }"));
        assert!(s.contains("const c = candidate(e);"));
        assert!(!s.contains("{{"));
        let all = arm_script(Some("no-referrer"), LinkMode::All, &HashMap::new());
        assert!(all.contains("const HEADER_POLICY = \"no-referrer\";"));
        assert!(all.contains("const ALL = true;"));
        assert!(all.contains("const HOSTS = {};"));
        assert!(arm_script(Some(""), LinkMode::CrossSite, &HashMap::new())
            .contains("const HEADER_POLICY = \"\";"));
        assert!(DISARM_SCRIPT.contains("st.disarm()"));
    }

    #[test]
    fn both_ends_must_have_a_registrable_domain_for_a_takeover() {
        let page = page_site("https://a.example.com/");
        let p = page.as_deref();
        assert_eq!(
            classify_link(p, "https://x.other.org/p"),
            Some(Ok("x.other.org".into()))
        );
        assert_eq!(
            classify_link(Some("cu468.co.uk"), "https://x.cu469.co.uk/"),
            Some(Ok("x.cu469.co.uk".into()))
        );
        assert_eq!(
            classify_link(Some("u1.github.io"), "https://u2.github.io/"),
            Some(Ok("u2.github.io".into()))
        );
        let reason = |page: Option<&str>, url: &str| match classify_link(page, url) {
            Some(Err((_, why))) => why,
            other => panic!("{url}: {other:?}"),
        };
        // Same registrable domain, whatever the scheme or subdomain.
        assert!(reason(p, "https://b.example.com/").contains("same site"));
        assert!(reason(p, "http://example.com/").contains("same site"));
        assert!(reason(Some("cu468.co.uk"), "https://b.cu468.co.uk/").contains("same site"));
        assert!(reason(Some("u1.github.io"), "https://v.u1.github.io/").contains("same site"));
        // The link's site cannot be classified: Chrome's click.
        for url in [
            "http://localhost:3000/",
            "https://intranet/",
            "https://a.cu468.test/",
            "https://co.uk/",
            "https://github.io/",
            "http://127.0.0.1/",
            "http://[::1]/",
        ] {
            assert!(
                reason(p, url).contains("could not classify the link"),
                "{url}"
            );
        }
        // The page's site cannot be classified.
        assert!(reason(None, "https://x.other.org/").contains("could not classify the page"));
        // Not http(s): not in the table at all.
        assert_eq!(classify_link(p, "mailto:a@b.c"), None);
    }

    #[test]
    fn host_table_applies_the_choosebrowser_check_to_takeovers() {
        let links = vec![
            "https://x.other.org/a".to_string(),
            "https://ruled.example.net/".to_string(),
            "https://b.example.com/".to_string(),
        ];
        let refuse = |url: &str| url.contains("ruled").then(|| "profile Work".to_string());
        let t = host_table(Some("example.com"), &links, &refuse);
        assert_eq!(t["x.other.org"], "cross");
        assert!(t["ruled.example.net"].contains("ChooseBrowser"));
        assert!(t["b.example.com"].contains("same site"));
    }

    #[test]
    fn the_switch_parses_default_all_and_off() {
        assert_eq!(LinkMode::parse(None), LinkMode::CrossSite);
        assert_eq!(LinkMode::parse(Some("")), LinkMode::CrossSite);
        assert_eq!(LinkMode::parse(Some("cross-site")), LinkMode::CrossSite);
        assert_eq!(LinkMode::parse(Some("bogus")), LinkMode::CrossSite);
        assert_eq!(LinkMode::parse(Some("1")), LinkMode::All);
        assert_eq!(LinkMode::parse(Some(" ALL ")), LinkMode::All);
        assert_eq!(LinkMode::parse(Some("0")), LinkMode::Off);
        assert_eq!(LinkMode::parse(Some("off")), LinkMode::Off);
    }

    #[test]
    fn page_site_is_the_registrable_domain_or_nothing() {
        assert_eq!(
            page_site("https://a.b.example.com/x").as_deref(),
            Some("example.com")
        );
        assert_eq!(
            page_site("http://example.com").as_deref(),
            Some("example.com")
        );
        assert_eq!(
            page_site("https://a.cu468.co.uk/").as_deref(),
            Some("cu468.co.uk")
        );
        assert_eq!(
            page_site("https://u1.github.io/").as_deref(),
            Some("u1.github.io")
        );
        assert_eq!(
            page_site("https://WWW.Example.COM./").as_deref(),
            Some("example.com")
        );
        // Unclassifiable: Chrome's own click.
        assert_eq!(page_site("https://co.uk/"), None);
        assert_eq!(page_site("https://github.io/"), None);
        assert_eq!(page_site("http://localhost:3000/"), None);
        assert_eq!(page_site("http://127.0.0.1/"), None);
        assert_eq!(page_site("http://[::1]/"), None);
        assert_eq!(page_site("https://a.cu468.test/"), None);
        assert_eq!(page_site("about:blank"), None);
        assert_eq!(page_site("data:text/html,x"), None);
    }

    #[test]
    fn frame_url_finds_child_frames() {
        let tree = json!({
            "frame": { "id": "top", "url": "https://a.com/" },
            "childFrames": [{ "frame": { "id": "c1", "url": "https://b.com/f" },
                "childFrames": [{ "frame": { "id": "c2", "url": "about:srcdoc" } }] }]
        });
        assert_eq!(frame_url(&tree, "top").as_deref(), Some("https://a.com/"));
        assert_eq!(frame_url(&tree, "c2").as_deref(), Some("about:srcdoc"));
        assert_eq!(frame_url(&tree, "nope"), None);
    }

    #[test]
    fn only_a_failure_chrome_reported_is_known() {
        assert!(navigation_failure_is_known(
            "Navigation failed: net::ERR_NAME_NOT_RESOLVED"
        ));
        // Faults where the tab may already have navigated: unknown.
        for e in [
            "CDP command timed out: Page.navigate",
            "the tab this command was driving is gone",
            "Browser not launched",
            "relay connection closed",
        ] {
            assert!(!navigation_failure_is_known(e), "{e}");
        }
    }

    #[test]
    fn returning_to_the_clicked_tab_reports_the_actual_pin() {
        assert_eq!(return_warning(Ok(()), true, "t1"), None);
        // The switch said yes but the pin is elsewhere: say where.
        let moved = return_warning(Ok(()), false, "t3").unwrap();
        assert!(moved.contains("it is on t3"));
        // The switch failed: the reason and the real pin, not a guess.
        let failed = return_warning(Err("Tab ID 1 not found".into()), false, "t2").unwrap();
        assert!(failed.contains("Tab ID 1 not found"));
        assert!(failed.contains("it is on t2"));
        let none = return_warning(Err("x".into()), false, "no tab").unwrap();
        assert!(none.contains("it is on no tab"));
    }

    #[test]
    fn a_navigated_document_is_gone_not_unknown() {
        assert!(document_gone("Cannot find context with specified id"));
        assert!(document_gone("Execution context was destroyed."));
        assert!(!document_gone("command timed out"));
    }

    #[test]
    fn the_note_names_the_switch_where_it_helps() {
        let off = chrome_raised_note(None, LinkMode::Off);
        assert!(off.contains("#468"));
        assert!(off.contains(BACKGROUND_LINKS_ENV));
        let same = chrome_raised_note(
            Some("the link stays on the same site, so it keeps Chrome's own click"),
            LinkMode::CrossSite,
        );
        assert!(same.contains(&format!("{BACKGROUND_LINKS_ENV}=all")));
        let opener = chrome_raised_note(Some("the link has rel=opener"), LinkMode::CrossSite);
        assert!(opener.contains("(the link has rel=opener)"));
        assert!(!opener.contains(BACKGROUND_LINKS_ENV));
        let wopen = chrome_raised_note(None, LinkMode::All);
        assert!(wopen.contains("window.open"));
    }
}
