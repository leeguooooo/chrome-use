use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use super::cdp::client::CdpClient;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Cookie {
    pub name: String,
    pub value: String,
    pub domain: String,
    pub path: String,
    #[serde(default)]
    pub expires: f64,
    #[serde(default)]
    pub size: i64,
    #[serde(default)]
    pub http_only: bool,
    #[serde(default)]
    pub secure: bool,
    #[serde(default)]
    pub session: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub same_site: Option<String>,
}

pub async fn get_all_cookies(client: &CdpClient, session_id: &str) -> Result<Vec<Cookie>, String> {
    let result = client
        .send_command_no_params("Network.getAllCookies", Some(session_id))
        .await?;

    let cookies: Vec<Cookie> = result
        .get("cookies")
        .and_then(|v| serde_json::from_value(v.clone()).ok())
        .unwrap_or_default();

    Ok(cookies)
}

pub async fn get_cookies(
    client: &CdpClient,
    session_id: &str,
    urls: Option<Vec<String>>,
) -> Result<Vec<Cookie>, String> {
    let params = match urls {
        Some(ref u) if !u.is_empty() => json!({ "urls": u }),
        _ => json!({}),
    };

    let result = client
        .send_command("Network.getCookies", Some(params), Some(session_id))
        .await?;

    let cookies: Vec<Cookie> = result
        .get("cookies")
        .and_then(|v| serde_json::from_value(v.clone()).ok())
        .unwrap_or_default();

    Ok(cookies)
}

pub async fn set_cookies(
    client: &CdpClient,
    session_id: &str,
    cookies: Vec<Value>,
    current_url: Option<&str>,
) -> Result<(), String> {
    let cookies: Vec<Value> = cookies
        .into_iter()
        .map(|mut c| {
            // Auto-fill url if no domain/path/url provided
            if c.get("url").is_none() && c.get("domain").is_none() && current_url.is_some() {
                c.as_object_mut().map(|m| {
                    m.insert(
                        "url".to_string(),
                        Value::String(current_url.unwrap().to_string()),
                    )
                });
            }
            c
        })
        .collect();

    client
        .send_command(
            "Network.setCookies",
            Some(json!({ "cookies": cookies })),
            Some(session_id),
        )
        .await?;

    Ok(())
}

/// Whether a cookie set for `cookie_domain` belongs to `domain`: the domain
/// itself or one of its subdomains, never a parent. `--domain
/// platform.openai.com` must not touch `.openai.com`, which every other
/// openai.com site also uses.
pub fn domain_matches(cookie_domain: &str, domain: &str) -> bool {
    let c = cookie_domain.trim_start_matches('.').to_ascii_lowercase();
    let d = domain.trim_start_matches('.').to_ascii_lowercase();
    !d.is_empty() && (c == d || c.ends_with(&format!(".{d}")))
}

/// The distinct sites (cookie domains without a leading dot) in `cookies`.
pub fn cookie_sites(cookies: &[Cookie]) -> Vec<String> {
    let mut sites: Vec<String> = cookies
        .iter()
        .map(|c| c.domain.trim_start_matches('.').to_ascii_lowercase())
        .collect();
    sites.sort();
    sites.dedup();
    sites
}

/// A few site names for a message: " (github.com, x.com, … and 40 more)".
pub fn site_sample(sites: &[String]) -> String {
    if sites.is_empty() {
        return String::new();
    }
    let shown: Vec<&str> = sites.iter().take(5).map(String::as_str).collect();
    let more = sites.len().saturating_sub(shown.len());
    if more > 0 {
        format!(" ({}, … and {more} more)", shown.join(", "))
    } else {
        format!(" ({})", shown.join(", "))
    }
}

/// Delete one cookie exactly (name, domain and path).
pub async fn delete_cookie(client: &CdpClient, session_id: &str, c: &Cookie) -> Result<(), String> {
    client
        .send_command(
            "Network.deleteCookies",
            Some(json!({ "name": c.name, "domain": c.domain, "path": c.path })),
            Some(session_id),
        )
        .await?;
    Ok(())
}

pub async fn clear_cookies(client: &CdpClient, session_id: &str) -> Result<(), String> {
    client
        .send_command_no_params("Network.clearBrowserCookies", Some(session_id))
        .await?;
    Ok(())
}

#[cfg(test)]
mod scope_tests {
    use super::*;

    #[test]
    fn a_domain_covers_itself_and_subdomains_never_parents() {
        assert!(domain_matches(
            ".platform.openai.com",
            "platform.openai.com"
        ));
        assert!(domain_matches(
            "auth.platform.openai.com",
            "platform.openai.com"
        ));
        assert!(!domain_matches(".openai.com", "platform.openai.com"));
        assert!(!domain_matches("notopenai.com", "openai.com"));
        assert!(!domain_matches("github.com", ""));
    }
}
