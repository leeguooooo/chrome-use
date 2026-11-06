//! Login-wall detection (#434): a navigation that lands on a site's sign-in
//! page instead of the page the agent asked for. The URL classifier here is
//! pure; the daemon confirms a weak URL signal with one DOM probe (a visible
//! password or username field, the same fields `auth login --bwu` fills)
//! before it says anything, so an ordinary page with a "Log in" link never
//! reads as a wall.

use serde_json::{json, Value};

/// Path segments (lowercased, extension dropped) that name a sign-in page.
pub const LOGIN_SEGMENTS: &[&str] = &[
    "login",
    "log-in",
    "log_in",
    "signin",
    "sign-in",
    "sign_in",
    "logon",
    "auth",
    "sso",
    "authenticate",
];

/// First host labels of a dedicated sign-in host (accounts.google.com,
/// login.microsoftonline.com, sso.company.com).
pub const LOGIN_HOST_LABELS: &[&str] = &["accounts", "login", "signin", "auth", "sso", "idp"];

/// Query parameters (lowercased) that carry where to go after signing in.
const RETURN_PARAMS: &[&str] = &[
    "redirect_uri",
    "redirect_url",
    "redirect",
    "redirect_to",
    "redirectto",
    "redirecturl",
    "return_to",
    "returnto",
    "return_url",
    "returnurl",
    "return",
    "next",
    "continue",
    "callbackurl",
    "callback",
    "goto",
    "dest",
    "destination",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Strength {
    /// A sign-in URL that names the way back, or that the site bounced the
    /// caller to from one of its other pages: reported as is.
    Strong,
    /// A sign-in-looking URL with nothing else: reported only if the page
    /// shows a login field.
    Weak,
    /// Not a sign-in URL, but not where the caller asked to go either:
    /// reported only if the page shows a password field.
    Redirected,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Signal {
    pub strength: Strength,
    /// Where to go after signing in: the page's return parameter, else the
    /// page the caller was sent away from.
    pub return_to: Option<String>,
}

fn http_url(s: &str) -> Option<url::Url> {
    url::Url::parse(s)
        .ok()
        .filter(|u| matches!(u.scheme(), "http" | "https") && u.host_str().is_some())
}

/// The registrable part of a host, roughly: the last two labels, or three
/// under a two-letter TLD with a generic second level (`example.co.uk`).
pub fn site(host: &str) -> String {
    let host = host.trim_end_matches('.').to_ascii_lowercase();
    if host.parse::<std::net::IpAddr>().is_ok() || !host.contains('.') {
        return host;
    }
    let labels: Vec<&str> = host.split('.').collect();
    let n = labels.len();
    let generic = ["co", "com", "net", "org", "gov", "edu", "ac", "ne", "or"];
    let take = if n >= 3 && labels[n - 1].len() == 2 && generic.contains(&labels[n - 2]) {
        3
    } else {
        2
    };
    labels[n.saturating_sub(take)..].join(".")
}

/// Whether two hosts belong to the same site (mail.google.com and
/// accounts.google.com do; github.com and github.io do not).
pub fn related(a: &str, b: &str) -> bool {
    !a.is_empty() && site(a) == site(b)
}

/// The registrable domain of `host` when the Public Suffix List (with its
/// private section) knows its suffix: `alice.github.io` for
/// `www.alice.github.io`. `None` for hosts it does not know (localhost, IPs).
pub fn registrable_domain(host: &str) -> Option<&str> {
    let h = host.trim_end_matches('.');
    psl::suffix(h.as_bytes())
        .filter(|s| s.is_known())
        .and_then(|_| psl::domain_str(h))
}

/// Whether a frame on `b` may get the credentials of a login on `a`: the same
/// site by `related`, and never two tenants of a shared suffix the Public
/// Suffix List names (`alice.github.io` and `mallory.github.io`).
pub fn same_login_site(a: &str, b: &str) -> bool {
    let known =
        |h: &str| psl::suffix(h.trim_end_matches('.').as_bytes()).is_some_and(|s| s.is_known());
    if a.trim_end_matches('.')
        .eq_ignore_ascii_case(b.trim_end_matches('.'))
    {
        return true;
    }
    if !known(a) && !known(b) {
        return related(a, b);
    }
    // A host the list knows is the same site only by an equal registrable
    // domain; a bare suffix (`github.io`) has none and matches only itself.
    related(a, b)
        && matches!(
            (registrable_domain(a), registrable_domain(b)),
            (Some(x), Some(y)) if x.eq_ignore_ascii_case(y)
        )
}

/// Whether the URL itself looks like a sign-in page.
pub fn login_ish(u: &url::Url) -> bool {
    let host = u.host_str().unwrap_or("").to_ascii_lowercase();
    let first = host.split('.').next().unwrap_or("");
    if host.contains('.') && LOGIN_HOST_LABELS.contains(&first) {
        return true;
    }
    u.path_segments().into_iter().flatten().any(|seg| {
        let seg = seg.to_ascii_lowercase();
        let stem = seg.split('.').next().unwrap_or("");
        LOGIN_SEGMENTS.contains(&stem)
            || ["login", "signin", "logon"]
                .iter()
                .any(|w| stem.len() > w.len() + 3 && stem.ends_with(w))
    })
}

/// The page's own "where to go after signing in", made absolute.
fn return_param(u: &url::Url) -> Option<url::Url> {
    u.query_pairs().find_map(|(k, v)| {
        if !RETURN_PARAMS.contains(&k.to_ascii_lowercase().as_str()) {
            return None;
        }
        let v = v.trim();
        if v.starts_with('/') && !v.starts_with("//") {
            return u.join(v).ok();
        }
        http_url(v)
    })
}

fn same_page(a: &url::Url, b: &url::Url) -> bool {
    a.origin() == b.origin() && a.path().trim_end_matches('/') == b.path().trim_end_matches('/')
}

/// Classify where a command left the tab. `intended` is the page the caller
/// asked for (`open`'s URL, the page before a `reload`) or was on before the
/// site moved it; `None` when it is not known (the first page of a session).
pub fn classify(current: &str, intended: Option<&str>) -> Option<Signal> {
    let cur = http_url(current)?;
    let host = cur.host_str().unwrap_or("");
    let intended = intended.and_then(http_url);
    let back = return_param(&cur);
    // Asked for the sign-in page itself (`open github.com/login`): not a wall.
    if let Some(i) = &intended {
        if same_page(i, &cur) {
            return None;
        }
    }
    let intended_host = intended.as_ref().and_then(|i| i.host_str());
    if login_ish(&cur) {
        let points_back = back.as_ref().is_some_and(|b| {
            let bh = b.host_str().unwrap_or("");
            match intended_host {
                Some(ih) => related(bh, ih),
                None => related(bh, host),
            }
        });
        let bounced = intended
            .as_ref()
            .is_some_and(|i| !login_ish(i) && related(i.host_str().unwrap_or(""), host));
        let return_to = back.map(|b| b.to_string()).or_else(|| {
            intended
                .as_ref()
                .filter(|i| !login_ish(i))
                .map(|i| i.to_string())
        });
        let strength = if points_back || bounced {
            Strength::Strong
        } else {
            Strength::Weak
        };
        return Some(Signal {
            strength,
            return_to,
        });
    }
    // Moved somewhere else on the same site: a login form there is a wall
    // too (`/dashboard` → `/?expired=1` with the form on the home page).
    let i = intended?;
    if related(i.host_str().unwrap_or(""), host) && !login_ish(&i) {
        return Some(Signal {
            strength: Strength::Redirected,
            return_to: Some(i.to_string()),
        });
    }
    None
}

/// The `loginWall` object and the one-line stderr hint. `from_host` is the
/// site the caller was on (or asked for), when known.
pub fn report(current: &str, signal: &Signal, from_host: Option<&str>) -> Value {
    let host = http_url(current)
        .and_then(|u| u.host_str().map(str::to_string))
        .unwrap_or_default();
    let named = from_host.filter(|h| !h.is_empty()).unwrap_or(&host);
    json!({
        "url": current,
        "returnTo": signal.return_to,
        "host": host,
        "hint": hint(named),
    })
}

pub fn hint(host: &str) -> String {
    format!(
        "login wall: {host} redirected to its sign-in page; sign in with `chrome-use auth login \
         {}` (add --item <name> if the vault has several logins for it), then continue — ask \
         the user only if no vault item matches, 2FA needs them, or login fails",
        crate::bwu_login::Manager::configured().flag()
    )
}

/// One evaluate: does the page show a field `auth login --bwu` would fill?
/// Returns "password", "username" or "" (nothing visible).
pub fn probe_js(user_selectors: &[&str]) -> String {
    let sels = serde_json::to_string(user_selectors).unwrap_or_else(|_| "[]".into());
    format!(
        r#"(() => {{
            const visible = (el) => {{
                const r = el.getBoundingClientRect();
                const s = getComputedStyle(el);
                return r.width > 0 && r.height > 0 && s.visibility !== 'hidden'
                    && s.display !== 'none' && parseFloat(s.opacity || '1') > 0
                    && !el.disabled;
            }};
            for (const el of document.querySelectorAll('input[type=password]')) {{
                if (visible(el)) return 'password';
            }}
            for (const sel of {sels}) {{
                let found;
                try {{ found = document.querySelectorAll(sel); }} catch (e) {{ continue; }}
                for (const el of found) {{
                    if (el.type !== 'hidden' && visible(el)) return 'username';
                }}
            }}
            return '';
        }})()"#
    )
}

/// Whether a probe result confirms the signal.
pub fn confirmed(strength: Strength, probe: &str) -> bool {
    match strength {
        Strength::Strong => true,
        Strength::Weak => matches!(probe, "password" | "username"),
        Strength::Redirected => probe == "password",
    }
}

/// A site whose QR sign-in can stop at a second QR code: after the phone
/// confirms the first scan, the site asks for a "security verification" scan
/// in a dialog over the page, while its sign-in box still says "confirm on
/// your phone". An agent that reads only the sign-in box thinks the phone's
/// confirmation never arrived and reloads, which throws the verification
/// away. Seen on www.xiaohongshu.com (2026-10): its QR status poll
/// (`/api/sns/web/v1/login/qrcode/status`) answered HTTP 471 and a
/// "Security Verification" dialog with a new QR code, valid for about a
/// minute, appeared over the sign-in box.
pub struct VerificationSite {
    /// Registrable domain the check runs on ([`site`]).
    pub site: &'static str,
    /// Text of the dialog's title, in any of the site's languages.
    pub titles: &'static [&'static str],
    /// Elements whose images are never the verification QR (the first QR
    /// code, still in the sign-in box under the dialog).
    pub ignore: &'static str,
    /// Path of the QR status poll, and the status that asks for the scan.
    pub status_path: &'static str,
    pub status: u16,
}

pub const VERIFICATION_SITES: &[VerificationSite] = &[VerificationSite {
    site: "xiaohongshu.com",
    titles: &["Security Verification", "安全验证"],
    ignore: ".login-container",
    status_path: "/login/qrcode/status",
    status: 471,
}];

/// A status answer older than this no longer counts: the verification QR
/// expires after about a minute.
pub const VERIFICATION_STATUS_MAX_AGE_MS: u64 = 120_000;

/// The [`VerificationSite`] for a page, if its site has one.
pub fn verification_site(page_url: &str) -> Option<&'static VerificationSite> {
    let u = http_url(page_url)?;
    let host = u.host_str()?.to_ascii_lowercase();
    VERIFICATION_SITES.iter().find(|s| site(&host) == s.site)
}

/// One evaluate, read only: the verification dialog (a visible element whose
/// text is one of the titles, give or take a few characters, and near it a
/// square QR-sized image or canvas outside `ignore`; same-origin frames
/// too), and the last answer to the QR status poll from the page's
/// resource timing (`responseStatus`, age in ms). Returns `{title, text, qr:
/// {tag, src, width, height} | null, status: {code, ageMs} | null}`.
pub fn verification_probe_js(s: &VerificationSite) -> String {
    let titles = serde_json::to_string(s.titles).unwrap_or_else(|_| "[]".into());
    let ignore = serde_json::to_string(s.ignore).unwrap_or_else(|_| "\"\"".into());
    let path = serde_json::to_string(s.status_path).unwrap_or_else(|_| "\"\"".into());
    format!(
        r#"(() => {{
            const TITLES = {titles}, IGNORE = {ignore}, STATUS_PATH = {path};
            const out = {{ title: '', text: '', qr: null, status: null }};
            try {{
                const entries = performance.getEntriesByType('resource') || [];
                for (let i = entries.length - 1; i >= 0; i--) {{
                    const e = entries[i];
                    if (!String(e.name || '').includes(STATUS_PATH)) continue;
                    out.status = {{ code: e.responseStatus || 0,
                        ageMs: Math.max(0, Math.round(performance.now() - (e.responseEnd || e.startTime || 0))) }};
                    break;
                }}
            }} catch (_) {{}}
            const visible = (el) => {{
                try {{
                    const r = el.getBoundingClientRect();
                    const s = getComputedStyle(el);
                    return r.width > 0 && r.height > 0 && s.visibility !== 'hidden'
                        && s.display !== 'none' && parseFloat(s.opacity || '1') > 0;
                }} catch (_) {{ return false; }}
            }};
            const ignored = (el) => {{ try {{ return !!(IGNORE && el.closest(IGNORE)); }} catch (_) {{ return false; }} }};
            const docs = [document];
            try {{
                for (const f of document.querySelectorAll('iframe')) {{
                    try {{ if (f.contentDocument) docs.push(f.contentDocument); }} catch (_) {{}}
                }}
            }} catch (_) {{}}
            for (const doc of docs) {{
                const body = doc.body;
                if (!body || !TITLES.some((t) => (body.textContent || '').includes(t))) continue;
                for (const el of body.querySelectorAll('*')) {{
                    const t = (el.textContent || '').replace(/\s+/g, ' ').trim();
                    // The title itself, not a sentence that mentions the words.
                    if (!TITLES.some((x) => t.includes(x) && t.length <= x.length + 12) || ignored(el) || !visible(el)) continue;
                    let box = el;
                    for (let up = 0; up < 8 && box.parentElement; up++) {{
                        box = box.parentElement;
                        for (const q of box.querySelectorAll('img, canvas')) {{
                            const r = q.getBoundingClientRect();
                            if (r.width < 64 || r.height < 64
                                || Math.abs(r.width - r.height) > 0.25 * Math.max(r.width, r.height)
                                || ignored(q) || !visible(q)) continue;
                            let src = '';
                            if (String(q.tagName).toUpperCase() === 'IMG') src = q.currentSrc || q.src || '';
                            else {{ try {{ src = q.toDataURL('image/png'); }} catch (_) {{}} }}
                            out.title = t;
                            out.text = String(box.innerText || box.textContent || '').replace(/\s+/g, ' ').trim().slice(0, 300);
                            out.qr = {{ tag: String(q.tagName).toLowerCase(), src: src.length <= 200000 ? src : '',
                                width: Math.round(r.width), height: Math.round(r.height) }};
                            return out;
                        }}
                    }}
                    if (!out.title) out.title = t;
                }}
            }}
            return out;
        }})()"#
    )
}

/// Whether the probe says the sign-in waits on a verification scan: the
/// dialog with its QR code is on screen, or the status poll's last answer
/// is the site's "verify" status and recent. `None` otherwise. The object
/// is `loginVerification` in the command's data: `{host, evidence:
/// "dialog" | "http<status>", title, text, qrSrc, hint}`.
pub fn verification_report(host: &str, s: &VerificationSite, probe: &Value) -> Option<Value> {
    let str_of = |k: &str| probe.get(k).and_then(|v| v.as_str()).unwrap_or("");
    let qr = probe.get("qr").filter(|q| q.is_object());
    let dialog = !str_of("title").is_empty() && qr.is_some();
    let status = probe.get("status").filter(|v| v.is_object());
    let polled = status.is_some_and(|st| {
        st.get("code").and_then(|v| v.as_u64()) == Some(u64::from(s.status))
            && st
                .get("ageMs")
                .and_then(|v| v.as_u64())
                .is_some_and(|age| age <= VERIFICATION_STATUS_MAX_AGE_MS)
    });
    if !dialog && !polled {
        return None;
    }
    let evidence = if dialog {
        "dialog".to_string()
    } else {
        format!("http{}", s.status)
    };
    let qr_src = qr
        .and_then(|q| q.get("src"))
        .and_then(|v| v.as_str())
        .filter(|v| !v.is_empty());
    Some(json!({
        "host": host,
        "evidence": evidence,
        "title": dialog.then(|| str_of("title")),
        "text": dialog.then(|| str_of("text")).filter(|t| !t.is_empty()),
        "qrSrc": qr_src,
        "hint": verification_hint(host, dialog, s.status),
    }))
}

/// The one-line warning for a pending verification scan.
pub fn verification_hint(host: &str, dialog: bool, status: u16) -> String {
    let seen = if dialog {
        "a \"Security Verification\" (安全验证) dialog with a new QR code is over the page"
            .to_string()
    } else {
        format!(
            "its QR sign-in status check just answered HTTP {status}, which asks for a security \
             verification scan; the dialog may be in a frame chrome-use cannot read"
        )
    };
    format!(
        "login verification: {host} is waiting for a second QR scan, not for the phone: {seen}. \
         The sign-in box under it still says to confirm on the phone; that is stale. Take \
         `chrome-use screenshot` (or use loginVerification.qrSrc in --json) and give that QR code \
         to the user to scan with the site's app on a phone already signed in; it expires in \
         about a minute. Until they have scanned it, do not reload, open another page, or sign \
         in again on this tab (auto-login and `site` login walls are held back), or the \
         verification is lost"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(cur: &str, intended: Option<&str>) -> Option<Strength> {
        classify(cur, intended).map(|s| s.strength)
    }

    #[test]
    fn cloudflare_redirect_is_strong() {
        let cur = "https://dash.cloudflare.com/login?redirect_uri=https%3A%2F%2Fdash.cloudflare.com%2Fabc%2Fbilling%2Finvoices";
        let sig = classify(
            cur,
            Some("https://dash.cloudflare.com/abc/billing/invoices"),
        )
        .unwrap();
        assert_eq!(sig.strength, Strength::Strong);
        assert_eq!(
            sig.return_to.as_deref(),
            Some("https://dash.cloudflare.com/abc/billing/invoices")
        );
        // Even without knowing where we came from: the way back is on the URL.
        assert_eq!(s(cur, None), Some(Strength::Strong));
        let r = report(cur, &sig, Some("dash.cloudflare.com"));
        assert_eq!(r["host"], "dash.cloudflare.com");
        assert!(r["hint"]
            .as_str()
            .unwrap()
            .starts_with("login wall: dash.cloudflare.com redirected"));
    }

    #[test]
    fn github_return_to_relative() {
        let cur = "https://github.com/login?return_to=%2Fsettings%2Fprofile";
        let sig = classify(cur, Some("https://github.com/settings/profile")).unwrap();
        assert_eq!(sig.strength, Strength::Strong);
        assert_eq!(
            sig.return_to.as_deref(),
            Some("https://github.com/settings/profile")
        );
        // Bounced without a return parameter still counts.
        assert_eq!(
            s(
                "https://github.com/login",
                Some("https://github.com/settings")
            ),
            Some(Strength::Strong)
        );
    }

    #[test]
    fn google_accounts_continue() {
        let cur = "https://accounts.google.com/v3/signin/identifier?continue=https%3A%2F%2Fmail.google.com%2Fmail%2Fu%2F0%2F&service=mail&flowName=GlifWebSignIn";
        let sig = classify(cur, Some("https://mail.google.com/mail/u/0/")).unwrap();
        assert_eq!(sig.strength, Strength::Strong);
        assert_eq!(
            sig.return_to.as_deref(),
            Some("https://mail.google.com/mail/u/0/")
        );
        assert_eq!(
            s("https://accounts.google.com/ServiceLogin", None),
            Some(Strength::Weak)
        );
    }

    #[test]
    fn ordinary_pages_are_not_walls() {
        // A docs page whose body links to /login: the URL says nothing.
        assert_eq!(
            s(
                "https://developers.cloudflare.com/fundamentals/setup/",
                None
            ),
            None
        );
        assert_eq!(
            s(
                "https://docs.github.com/en/authentication",
                Some("https://docs.github.com/en/authentication")
            ),
            None
        );
        assert_eq!(
            s("https://example.com/blog/how-to-login-faster", None),
            None
        );
        assert_eq!(
            s("https://news.ycombinator.com/item?id=1&next=2", None),
            None
        );
        // Asking for the sign-in page itself is not being walled.
        assert_eq!(
            s("https://github.com/login", Some("https://github.com/login")),
            None
        );
        assert_eq!(s("about:blank", None), None);
        assert_eq!(s("chrome://newtab/", None), None);
    }

    #[test]
    fn weak_and_redirected_need_the_page() {
        // A sign-in URL alone, or a same-site redirect, waits for the probe.
        assert_eq!(s("https://example.com/auth", None), Some(Strength::Weak));
        assert!(!confirmed(Strength::Weak, ""));
        assert!(confirmed(Strength::Weak, "username"));
        let sig = classify(
            "https://app.example.com/?expired=1",
            Some("https://app.example.com/dashboard"),
        )
        .unwrap();
        assert_eq!(sig.strength, Strength::Redirected);
        assert!(!confirmed(Strength::Redirected, "username"));
        assert!(confirmed(Strength::Redirected, "password"));
        // A different site entirely is a link the caller followed, not a wall.
        assert_eq!(
            s(
                "https://other.org/home",
                Some("https://example.com/dashboard")
            ),
            None
        );
    }

    #[test]
    fn oauth_return_to_another_site() {
        let cur = "https://accounts.google.com/o/oauth2/v2/auth?client_id=x&redirect_uri=https%3A%2F%2Fapp.vercel.com%2Fapi%2Fauth%2Fcallback";
        assert_eq!(
            s(cur, Some("https://app.vercel.com/login")),
            Some(Strength::Strong)
        );
    }

    #[test]
    fn site_grouping() {
        assert!(related("mail.google.com", "accounts.google.com"));
        assert!(related("www.example.co.uk", "login.example.co.uk"));
        assert!(!related("a.example.co.uk", "b.other.co.uk"));
        assert!(!related("github.com", "github.io"));
        assert!(related("127.0.0.1", "127.0.0.1"));
    }

    // A pending security-verification scan after a QR sign-in.

    fn xhs() -> &'static VerificationSite {
        verification_site("https://www.xiaohongshu.com/explore").unwrap()
    }

    #[test]
    fn verification_runs_only_on_its_site() {
        assert_eq!(xhs().site, "xiaohongshu.com");
        assert!(verification_site("https://xiaohongshu.com/").is_some());
        assert!(verification_site("https://example.com/explore").is_none());
        assert!(verification_site("https://xiaohongshu.com.example.net/").is_none());
        assert!(verification_site("about:blank").is_none());
    }

    #[test]
    fn verification_report_needs_the_dialog_or_a_recent_status() {
        let s = xhs();
        let h = "www.xiaohongshu.com";
        // Nothing on the page, no status poll seen.
        let empty = json!({ "title": "", "text": "", "qr": null, "status": null });
        assert!(verification_report(h, s, &empty).is_none());
        // A title alone (no QR code near it) is not the dialog.
        let title_only = json!({ "title": "Security Verification", "qr": null, "status": null });
        assert!(verification_report(h, s, &title_only).is_none());
        // An ordinary answer, or a 471 from long ago, is not a pending scan.
        for st in [
            json!({ "code": 200, "ageMs": 1000 }),
            json!({ "code": 471, "ageMs": VERIFICATION_STATUS_MAX_AGE_MS + 1 }),
        ] {
            assert!(verification_report(h, s, &json!({ "status": st })).is_none());
        }

        let dialog = json!({
            "title": "Security Verification",
            "text": "Security Verification Scan with logged-in app QR code expires in 1 minutes",
            "qr": { "tag": "img", "src": "data:image/png;base64,FAKE", "width": 160, "height": 160 },
            "status": null,
        });
        let r = verification_report(h, s, &dialog).unwrap();
        assert_eq!(r["evidence"], "dialog");
        assert_eq!(r["qrSrc"], "data:image/png;base64,FAKE");
        assert_eq!(r["title"], "Security Verification");
        let hint = r["hint"].as_str().unwrap();
        assert!(
            hint.starts_with(
                "login verification: www.xiaohongshu.com is waiting for a second QR scan"
            ),
            "{hint}"
        );
        for part in [
            "chrome-use screenshot",
            "loginVerification.qrSrc",
            "do not reload",
        ] {
            assert!(hint.contains(part), "{part}: {hint}");
        }

        let polled = json!({ "status": { "code": 471, "ageMs": 3000 } });
        let r = verification_report(h, s, &polled).unwrap();
        assert_eq!(r["evidence"], "http471");
        assert!(r["qrSrc"].is_null() && r["title"].is_null());
        assert!(r["hint"].as_str().unwrap().contains("HTTP 471"));
    }

    /// Runs the real probe under node against a hand-made page: elements
    /// with fixed boxes and styles, and resource timing entries.
    fn run_probe(page: &str, entries: &str) -> Option<Value> {
        let node = std::env::var_os("PATH").and_then(|p| {
            std::env::split_paths(&p)
                .map(|d| {
                    d.join(if cfg!(target_os = "windows") {
                        "node.exe"
                    } else {
                        "node"
                    })
                })
                .find(|n| n.is_file())
        });
        let Some(node) = node else {
            assert!(
                std::env::var_os("CI").is_none(),
                "Node is required for verification probe tests in CI"
            );
            eprintln!("Skipping verification probe: Node is unavailable outside CI");
            return None;
        };
        let probe = verification_probe_js(xhs());
        let driver = format!(
            r#"'use strict';
class El {{
  constructor(tag, o = {{}}, kids = []) {{
    this.tagName = tag.toUpperCase(); this.o = o; this.parentElement = null; this.children = [];
    this.src = o.src || ''; this.currentSrc = '';
    for (const k of kids) {{ k.parentElement = this; this.children.push(k); }}
  }}
  get textContent() {{ return [this.o.text || '', ...this.children.map((c) => c.textContent)].join(' '); }}
  get innerText() {{ return this.textContent; }}
  getBoundingClientRect() {{ return this.o.rect || {{ width: 120, height: 20 }}; }}
  all() {{ return this.children.flatMap((c) => [c, ...c.all()]); }}
  querySelectorAll(sel) {{
    if (sel === '*') return this.all();
    const tags = sel.split(',').map((s) => s.trim().toUpperCase());
    return this.all().filter((e) => tags.includes(e.tagName));
  }}
  closest(sel) {{
    for (let e = this; e; e = e.parentElement) if ((e.o.cls || '').split(' ').includes(sel.slice(1))) return e;
    return null;
  }}
}}
const h = (tag, o, kids) => new El(tag, o, kids);
globalThis.getComputedStyle = (el) => ({{ visibility: 'visible', display: 'block', opacity: '1', ...(el.o.style || {{}}) }});
const entries = {entries};
globalThis.performance = {{ now: () => 100000, getEntriesByType: () => entries }};
const body = {page};
globalThis.document = {{ body, querySelectorAll: (s) => (s === 'iframe' ? [] : body.querySelectorAll(s)) }};
console.log(JSON.stringify({probe}));
"#
        );
        let out = std::process::Command::new(node)
            .arg("-e")
            .arg(driver)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        Some(serde_json::from_slice(&out.stdout).unwrap())
    }

    /// The sign-in box after the phone confirmed: its own QR code and the
    /// stale "confirm on your phone" text.
    const LOGIN_BOX: &str = "h('div', { cls: 'login-container' }, [
        h('img', { cls: 'qrcode-img', src: 'data:image/png;base64,FIRST', rect: { width: 160, height: 160 } }),
        h('div', { text: '扫码成功' }), h('div', { text: '请在手机上确认' }) ])";

    #[test]
    fn probe_finds_the_verification_dialog_over_the_sign_in_box() {
        let page = format!(
            "h('body', {{}}, [ {LOGIN_BOX},
             h('div', {{ cls: 'verify-modal' }}, [
               h('div', {{ text: 'Security Verification' }}),
               h('div', {{ text: 'Scan with logged-in app for account security.' }}),
               h('div', {{}}, [ h('img', {{ src: 'data:image/png;base64,SECOND', rect: {{ width: 150, height: 150 }} }}) ]),
               h('div', {{ text: 'QR code expires in 1 minutes' }}) ]) ])"
        );
        let Some(p) = run_probe(&page, "[]") else {
            return;
        };
        assert_eq!(p["title"], "Security Verification");
        // The dialog's own QR code, never the first one in the sign-in box.
        assert_eq!(p["qr"]["src"], "data:image/png;base64,SECOND");
        assert!(p["text"].as_str().unwrap().contains("expires in 1 minutes"));
        let r = verification_report("www.xiaohongshu.com", xhs(), &p).unwrap();
        assert_eq!(r["evidence"], "dialog");
        // Chinese interface.
        let page_zh = format!(
            "h('body', {{}}, [ {LOGIN_BOX}, h('div', {{}}, [ h('div', {{ text: '安全验证' }}),
             h('img', {{ src: 'https://example.invalid/qr.png', rect: {{ width: 140, height: 140 }} }}) ]) ])"
        );
        let p = run_probe(&page_zh, "[]").unwrap();
        assert_eq!(p["qr"]["src"], "https://example.invalid/qr.png");
    }

    #[test]
    fn probe_matches_a_title_split_across_descendants() {
        let page = "h('body', {}, [h('div', {}, [h('div', {}, [h('span', {text: 'Security'}), h('span', {text: 'Verification'})]), h('img', {src: 'data:image/png;base64,SPLIT', rect: {width: 150, height: 150}})])])";
        let Some(probe) = run_probe(page, "[]") else {
            return;
        };
        assert_eq!(probe["title"], "Security Verification");
        assert_eq!(probe["qr"]["src"], "data:image/png;base64,SPLIT");
    }

    #[test]
    fn probe_ignores_pages_without_a_visible_dialog() {
        // Only the sign-in box: its QR code is not a verification.
        let Some(p) = run_probe(&format!("h('body', {{}}, [ {LOGIN_BOX} ])"), "[]") else {
            return;
        };
        assert!(p["qr"].is_null() && p["status"].is_null());
        assert!(verification_report("www.xiaohongshu.com", xhs(), &p).is_none());
        // A hidden dialog, and a note that only mentions the words next to
        // a cover image, are not one either.
        let page = format!(
            "h('body', {{}}, [ {LOGIN_BOX},
             h('div', {{ style: {{ display: 'none' }} }}, [ h('div', {{ text: 'Security Verification', style: {{ display: 'none' }} }}),
               h('img', {{ src: 'data:image/png;base64,HIDDEN', rect: {{ width: 150, height: 150 }}, style: {{ display: 'none' }} }}) ]),
             h('section', {{}}, [ h('p', {{ text: '今天聊聊账号安全验证的几个小技巧，记得开启两步验证，别把验证码告诉别人哦' }}),
               h('img', {{ src: 'https://example.invalid/cover.jpg', rect: {{ width: 150, height: 150 }} }}) ]) ])"
        );
        let p = run_probe(&page, "[]").unwrap();
        assert!(p["qr"].is_null(), "{p}");
        assert!(verification_report("www.xiaohongshu.com", xhs(), &p).is_none());
    }

    #[test]
    fn probe_reads_the_last_status_answer() {
        let entries = r#"[
            { name: 'https://edith.example.invalid/api/sns/web/v1/login/qrcode/status?qr_id=1', responseStatus: 200, responseEnd: 90000 },
            { name: 'https://edith.example.invalid/api/sns/web/v1/login/qrcode/status?qr_id=1', responseStatus: 471, responseEnd: 98000 },
            { name: 'https://example.invalid/app.js', responseStatus: 200, responseEnd: 99000 } ]"#;
        let Some(p) = run_probe(&format!("h('body', {{}}, [ {LOGIN_BOX} ])"), entries) else {
            return;
        };
        assert_eq!(p["status"]["code"], 471);
        assert_eq!(p["status"]["ageMs"], 2000);
        let r = verification_report("www.xiaohongshu.com", xhs(), &p).unwrap();
        assert_eq!(r["evidence"], "http471");
    }
}
