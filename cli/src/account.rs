//! Account identity over the cookie-use vault: `whoami` (which account is this
//! session logged in as, per site) and the global `--as <id>` guard (verify the
//! account BEFORE acting; auto-switch on mismatch). This is the wrong-account
//! protection for people with 10 logins on one site.
//!
//! Privacy contract: chrome-use never reads cookie VALUES from the vault.
//! `cookie-use fingerprint` exports hash-only fingerprints (sha256 of each
//! cookie value), and this module hashes the LIVE cookie values locally to
//! compare. Identity is decided by value-hash equality on the account's
//! auth-carrying cookies (httpOnly/secure), not by cookie presence — two
//! accounts on one site share cookie NAMES; only the values differ.

use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::commands::gen_id;
use crate::connection::send_command;
use crate::flags::Flags;

/// An account's hash-only cookie fingerprint, from `cookie-use fingerprint`.
#[derive(Debug, Clone)]
pub struct Fingerprint {
    pub id: String,
    pub site: String,
    pub domains: Vec<String>,
    pub cookies: Vec<FpCookie>,
}

#[derive(Debug, Clone)]
pub struct FpCookie {
    pub name: String,
    /// Normalized: no leading dot.
    pub domain: String,
    pub sha256: String,
    /// httpOnly || secure — the cookies that actually carry identity. Matching
    /// is scored on these; plain cookies (locale, consent, …) are shared
    /// between accounts and prove nothing.
    pub auth: bool,
}

/// How well one account's fingerprint matches the live cookie set.
#[derive(Debug, Clone, Copy)]
pub struct MatchScore {
    pub matched: usize,
    pub total: usize,
}

impl MatchScore {
    /// ≥60% of the account's auth cookies match by value-hash, minimum 1.
    /// Below that the session is either another account or logged out.
    pub fn is_match(&self) -> bool {
        self.matched >= 1 && self.matched * 100 >= self.total * 60
    }
}

fn cookie_use_bin() -> String {
    std::env::var("CHROME_USE_COOKIE_USE_BIN").unwrap_or_else(|_| "cookie-use".to_string())
}

/// Fetch fingerprints from cookie-use (`--all`, or one account by id).
pub fn load_fingerprints(id: Option<&str>) -> Result<Vec<Fingerprint>, String> {
    let mut c = std::process::Command::new(cookie_use_bin());
    match id {
        Some(one) => c.args(["fingerprint", one, "--json"]),
        None => c.args(["fingerprint", "--all", "--json"]),
    };
    let out = c.output().map_err(|e| {
        format!(
            "cookie-use is not runnable ({e}). `--as`/`whoami` need the cookie-use \
             account vault: https://github.com/leeguooooo/cookie-use"
        )
    })?;
    if !out.status.success() {
        return Err(format!(
            "cookie-use fingerprint failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    let v: Value = serde_json::from_slice(&out.stdout)
        .map_err(|e| format!("cookie-use fingerprint returned invalid JSON: {e}"))?;
    // Accept {accounts:[…]}, a bare array, or a single account object.
    let arr: Vec<Value> = if let Some(a) = v.get("accounts").and_then(|x| x.as_array()) {
        a.clone()
    } else if let Some(a) = v.as_array() {
        a.clone()
    } else {
        vec![v]
    };
    let fps: Vec<Fingerprint> = arr.iter().filter_map(parse_fingerprint).collect();
    if fps.is_empty() {
        return Err(match id {
            Some(one) => format!(
                "no vault account matches \"{one}\" — see `cookie-use list` for the ids"
            ),
            None => "the cookie-use vault has no fingerprinted accounts yet — run \
                     `cookie-use fingerprint <id>` once per account"
                .to_string(),
        });
    }
    Ok(fps)
}

fn parse_fingerprint(v: &Value) -> Option<Fingerprint> {
    let id = v.get("id")?.as_str()?.to_string();
    let site = v
        .get("site")
        .and_then(|s| s.as_str())
        .unwrap_or_default()
        .to_string();
    let mut domains: Vec<String> = v
        .get("domains")
        .and_then(|d| d.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|x| x.as_str())
                .map(|s| s.trim_start_matches('.').to_string())
                .collect()
        })
        .unwrap_or_default();
    if domains.is_empty() {
        domains = site
            .split(',')
            .map(|s| s.trim().trim_start_matches('.').to_string())
            .filter(|s| !s.is_empty())
            .collect();
    }
    let cookies = v
        .get("cookies")
        .and_then(|c| c.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|c| {
                    Some(FpCookie {
                        name: c.get("name")?.as_str()?.to_string(),
                        domain: c
                            .get("domain")?
                            .as_str()?
                            .trim_start_matches('.')
                            .to_string(),
                        sha256: c.get("sha256")?.as_str()?.to_lowercase(),
                        auth: c.get("httpOnly").and_then(|b| b.as_bool()).unwrap_or(false)
                            || c.get("secure").and_then(|b| b.as_bool()).unwrap_or(false),
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    Some(Fingerprint {
        id,
        site,
        domains,
        cookies,
    })
}

/// The live cookies of this session's browser, hashed: (name, domain) → sha256.
/// One daemon round-trip for ALL requested domains.
pub fn live_cookie_hashes(
    session: &str,
    domains: &[String],
) -> Result<std::collections::HashMap<(String, String), String>, String> {
    let urls: Vec<String> = domains
        .iter()
        .flat_map(|d| [format!("https://{d}"), format!("https://www.{d}")])
        .collect();
    let cmd = json!({ "id": gen_id(), "action": "cookies_get", "urls": urls });
    let resp = send_command(cmd, session)?;
    if !resp.success {
        return Err(resp
            .error
            .unwrap_or_else(|| "cookies_get failed".to_string()));
    }
    let mut map = std::collections::HashMap::new();
    if let Some(arr) = resp
        .data
        .as_ref()
        .and_then(|d| d.get("cookies"))
        .and_then(|c| c.as_array())
    {
        for c in arr {
            let (Some(name), Some(domain), Some(value)) = (
                c.get("name").and_then(|v| v.as_str()),
                c.get("domain").and_then(|v| v.as_str()),
                c.get("value").and_then(|v| v.as_str()),
            ) else {
                continue;
            };
            let hash = format!("{:x}", Sha256::digest(value.as_bytes()));
            map.insert(
                (name.to_string(), domain.trim_start_matches('.').to_string()),
                hash,
            );
        }
    }
    Ok(map)
}

/// CDN / bot-mitigation cookies that rotate every visit (Cloudflare, Akamai,
/// AWS ALB, DataDome). They're httpOnly+secure so they LOOK like auth cookies,
/// but a stored hash of one is stale within minutes — counting them dilutes
/// every score toward a false "wrong account". Identity lives in the
/// long-lived session tokens, never here.
const ROTATING_INFRA_COOKIES: &[&str] = &[
    "__cf_bm",
    "cf_clearance",
    "_cfuvid",
    "__cflb",
    "__cf_logged_in",
    "ak_bmsc",
    "bm_sv",
    "bm_sz",
    "_abck",
    "AWSALB",
    "AWSALBCORS",
    "datadome",
];

fn is_rotating_infra(name: &str) -> bool {
    ROTATING_INFRA_COOKIES.iter().any(|r| name.eq_ignore_ascii_case(r))
}

/// Score one account's fingerprint against the live hashes. Auth cookies only
/// (minus known rotating infra cookies); accounts with zero auth cookies fall
/// back to all fingerprinted cookies.
pub fn score(
    fp: &Fingerprint,
    live: &std::collections::HashMap<(String, String), String>,
) -> MatchScore {
    let auth: Vec<&FpCookie> = fp
        .cookies
        .iter()
        .filter(|c| c.auth && !is_rotating_infra(&c.name))
        .collect();
    let pool: Vec<&FpCookie> = if auth.is_empty() {
        fp.cookies.iter().collect()
    } else {
        auth
    };
    let matched = pool
        .iter()
        .filter(|c| {
            live.get(&(c.name.clone(), c.domain.clone()))
                .is_some_and(|h| *h == c.sha256)
        })
        .count();
    MatchScore {
        matched,
        total: pool.len(),
    }
}

/// `chrome-use whoami [filter]` — report which vault account each site's live
/// session belongs to. `filter` narrows by account-id or site/domain substring.
pub fn run_whoami(filter: Option<&str>, flags: &Flags) {
    let fps = match load_fingerprints(None) {
        Ok(f) => f,
        Err(e) => {
            fail(flags.json, &e);
            return;
        }
    };
    let fps: Vec<Fingerprint> = match filter {
        Some(q) => {
            let q = q.to_lowercase();
            fps.into_iter()
                .filter(|f| {
                    f.id.to_lowercase().contains(&q)
                        || f.site.to_lowercase().contains(&q)
                        || f.domains.iter().any(|d| d.to_lowercase().contains(&q))
                })
                .collect()
        }
        None => fps,
    };
    if fps.is_empty() {
        fail(
            flags.json,
            "no vault account matches that filter — see `cookie-use list`",
        );
        return;
    }
    let mut all_domains: Vec<String> = fps.iter().flat_map(|f| f.domains.clone()).collect();
    all_domains.sort();
    all_domains.dedup();
    let live = match live_cookie_hashes(&flags.session, &all_domains) {
        Ok(l) => l,
        Err(e) => {
            fail(flags.json, &e);
            return;
        }
    };

    // Group accounts by site; report the best match per site. Sites where the
    // browser holds NO cookies at all are skipped unless the user filtered —
    // "you're nobody on 12 sites you never visited" is noise.
    let mut sites: Vec<(String, Vec<&Fingerprint>)> = Vec::new();
    for fp in &fps {
        match sites.iter_mut().find(|(s, _)| *s == fp.site) {
            Some((_, v)) => v.push(fp),
            None => sites.push((fp.site.clone(), vec![fp])),
        }
    }

    let mut results: Vec<Value> = Vec::new();
    let zh = crate::connect::ui_zh();
    for (site, accounts) in &sites {
        let has_presence = accounts.iter().any(|fp| {
            live.keys()
                .any(|(_, d)| fp.domains.iter().any(|fd| d.ends_with(fd.as_str())))
        });
        if !has_presence && filter.is_none() {
            continue;
        }
        let mut scored: Vec<(&Fingerprint, MatchScore)> =
            accounts.iter().map(|fp| (*fp, score(fp, &live))).collect();
        scored.sort_by(|a, b| {
            (b.1.matched * 1000 / b.1.total.max(1)).cmp(&(a.1.matched * 1000 / a.1.total.max(1)))
        });
        let best = scored.first().filter(|(_, s)| s.is_match());
        results.push(json!({
            "site": site,
            "account": best.map(|(fp, _)| fp.id.clone()),
            "matched": best.map(|(_, s)| s.matched),
            "authTotal": best.map(|(_, s)| s.total),
            "candidates": scored.iter().map(|(fp, s)| json!({
                "id": fp.id, "matched": s.matched, "total": s.total,
            })).collect::<Vec<_>>(),
        }));
        if !flags.json {
            match best {
                Some((fp, s)) => println!(
                    "  {}  →  {}  ({}/{} auth cookies)",
                    site, fp.id, s.matched, s.total
                ),
                None => println!(
                    "  {}  →  {}",
                    site,
                    if zh {
                        "（没有匹配的 vault 账号 —— 未登录或未入库的账号）"
                    } else {
                        "(no vault account matches — logged out, or an account not in the vault)"
                    }
                ),
            }
        }
    }
    if flags.json {
        println!(
            "{}",
            serde_json::to_string(&json!({
                "success": true,
                "data": { "session": flags.session, "identities": results }
            }))
            .unwrap_or_default()
        );
    } else if results.is_empty() {
        println!(
            "{}",
            if zh {
                "这个会话的浏览器里没有任何 vault 站点的 cookie。"
            } else {
                "this session's browser holds no cookies for any vault site."
            }
        );
    }
}

/// Enforce `--as <id>`: verify the live session IS that account before the
/// command runs; on mismatch apply the account via cookie-use (unless strict),
/// re-verify, and only then let the command proceed. Errors are fatal to the
/// invocation — acting as the wrong account is the one thing this must prevent.
pub fn enforce_as(account: &str, strict: bool, flags: &Flags) -> Result<(), String> {
    let fps = load_fingerprints(Some(account))?;
    let fp = match fps.iter().find(|f| f.id == account) {
        Some(f) => f,
        None if fps.len() == 1 => &fps[0],
        None => {
            return Err(format!(
                "\"{account}\" matches {} vault accounts ({}) — use the exact id",
                fps.len(),
                fps.iter()
                    .map(|f| f.id.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ))
        }
    };

    let check = |first: bool| -> Result<MatchScore, String> {
        let live = live_cookie_hashes(&flags.session, &fp.domains)?;
        let s = score(fp, &live);
        if !first && !s.is_match() {
            return Err(format!(
                "still not {} after applying its session ({}/{} auth cookies match) — \
                 the stored session may be dead; re-capture it: cookie-use add {}",
                fp.id, s.matched, s.total, fp.id
            ));
        }
        Ok(s)
    };

    let s = check(true)?;
    if s.is_match() {
        return Ok(());
    }
    if strict {
        return Err(format!(
            "session is NOT {} ({}/{} auth cookies match) and --as-strict is set. \
             Switch explicitly: cookie-use use {} --target session:{}",
            fp.id, s.matched, s.total, fp.id, flags.session
        ));
    }

    // Auto-switch: apply the account's stored session into THIS session's
    // browser. Interactive runs keep cookie-use's own confirmation (biometric/
    // TTY); non-interactive runs pass --no-confirm — the explicit `--as` IS the
    // operator's consent there.
    eprintln!(
        "{} --as {}: session is currently someone else ({}/{} match) — switching via cookie-use…",
        crate::color::warning_indicator(),
        fp.id,
        s.matched,
        s.total
    );
    let mut c = std::process::Command::new(cookie_use_bin());
    c.args([
        "use",
        &fp.id,
        "--target",
        &format!("session:{}", flags.session),
        "--no-open",
    ]);
    if !crate::connect::interactive_tty() {
        c.arg("--no-confirm");
    }
    c.env("AGENT_BROWSER_SESSION", &flags.session);
    let out = c
        .status()
        .map_err(|e| format!("could not run cookie-use: {e}"))?;
    if !out.success() {
        return Err(format!(
            "cookie-use use {} failed — not executing the command as the wrong account",
            fp.id
        ));
    }
    let s = check(false)?;
    eprintln!(
        "{} now acting as {} ({}/{} auth cookies match)",
        crate::color::success_indicator(),
        fp.id,
        s.matched,
        s.total
    );
    Ok(())
}

fn fail(json: bool, msg: &str) {
    if json {
        println!(
            "{}",
            serde_json::to_string(&json!({ "success": false, "error": msg })).unwrap_or_default()
        );
    } else {
        eprintln!("{} {}", crate::color::error_indicator(), msg);
    }
    std::process::exit(1);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fp(cookies: Vec<FpCookie>) -> Fingerprint {
        Fingerprint {
            id: "site/a".into(),
            site: "example.com".into(),
            domains: vec!["example.com".into()],
            cookies,
        }
    }

    fn c(name: &str, domain: &str, sha: &str, auth: bool) -> FpCookie {
        FpCookie {
            name: name.into(),
            domain: domain.into(),
            sha256: sha.into(),
            auth,
        }
    }

    fn live(
        entries: &[(&str, &str, &str)],
    ) -> std::collections::HashMap<(String, String), String> {
        entries
            .iter()
            .map(|(n, d, h)| ((n.to_string(), d.to_string()), h.to_string()))
            .collect()
    }

    #[test]
    fn matching_is_by_value_hash_not_presence() {
        // Same cookie NAME present but a different value-hash (another account
        // logged in) must NOT count — presence-based matching is exactly the
        // bug that makes 10-account users act as the wrong one.
        let f = fp(vec![c("sid", "example.com", "aaa", true)]);
        let s = score(&f, &live(&[("sid", "example.com", "bbb")]));
        assert_eq!(s.matched, 0);
        assert!(!s.is_match());
        let s = score(&f, &live(&[("sid", "example.com", "aaa")]));
        assert_eq!(s.matched, 1);
        assert!(s.is_match());
    }

    #[test]
    fn score_uses_auth_cookies_only_when_present() {
        // The locale cookie matches (same "en" for every account) but the auth
        // cookie doesn't → not a match.
        let f = fp(vec![
            c("sid", "example.com", "aaa", true),
            c("locale", "example.com", "fff", false),
        ]);
        let s = score(&f, &live(&[("locale", "example.com", "fff")]));
        assert_eq!((s.matched, s.total), (0, 1));
        assert!(!s.is_match());
    }

    #[test]
    fn sixty_percent_of_auth_cookies_required() {
        let f = fp(vec![
            c("a", "example.com", "1", true),
            c("b", "example.com", "2", true),
            c("c", "example.com", "3", true),
        ]);
        // 1/3 = 33% < 60% → no
        assert!(!score(&f, &live(&[("a", "example.com", "1")])).is_match());
        // 2/3 = 66% ≥ 60% → yes
        assert!(score(
            &f,
            &live(&[("a", "example.com", "1"), ("b", "example.com", "2")])
        )
        .is_match());
    }

    #[test]
    fn rotating_infra_cookies_do_not_dilute_the_score() {
        // __cf_bm rotates ~every 30min: its stored hash will NEVER match live.
        // With it counted, this account could never exceed 1/2 = 50% < 60%.
        let f = fp(vec![
            c("__cf_bm", "example.com", "stale", true),
            c("session-token", "example.com", "aaa", true),
        ]);
        let s = score(&f, &live(&[("session-token", "example.com", "aaa")]));
        assert_eq!((s.matched, s.total), (1, 1));
        assert!(s.is_match());
    }

    #[test]
    fn zero_auth_cookies_falls_back_to_all() {
        let f = fp(vec![c("token", "example.com", "9", false)]);
        assert!(score(&f, &live(&[("token", "example.com", "9")])).is_match());
    }

    #[test]
    fn parse_fingerprint_normalizes_leading_dot_domains() {
        let v = json!({
            "id": "cf/x", "site": "cloudflare.com",
            "domains": [".cloudflare.com"],
            "cookies": [{"name":"s","domain":".cloudflare.com","sha256":"AB","httpOnly":true,"secure":true}]
        });
        let f = parse_fingerprint(&v).unwrap();
        assert_eq!(f.domains, vec!["cloudflare.com"]);
        assert_eq!(f.cookies[0].domain, "cloudflare.com");
        // hashes compare case-insensitively (stored lowercased)
        assert_eq!(f.cookies[0].sha256, "ab");
        assert!(f.cookies[0].auth);
    }

    #[test]
    fn parse_fingerprint_derives_domains_from_site_when_absent() {
        let v = json!({
            "id": "cf/x", "site": "cloudflare.com,dash.cloudflare.com",
            "cookies": []
        });
        let f = parse_fingerprint(&v).unwrap();
        assert_eq!(f.domains, vec!["cloudflare.com", "dash.cloudflare.com"]);
    }
}
