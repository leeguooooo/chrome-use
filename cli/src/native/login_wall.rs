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
fn site(host: &str) -> String {
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
         --bwu` (add --item <name> if the vault has several logins for it), then continue — ask \
         the user only if no vault item matches, 2FA needs them, or login fails"
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
}
