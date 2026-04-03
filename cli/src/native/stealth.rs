//! Stealth anti-detection module.
//!
//! Injects browser-level patches to evade bot detection (creepjs, sannysoft,
//! Cloudflare Turnstile, etc.) by normalizing fingerprint signals that betray
//! headless or automated Chrome instances.

use serde_json::json;

use super::cdp::client::CdpClient;

/// Default stealth JS payload compiled at build time.
/// The first line is a config placeholder that `build_stealth_script` replaces
/// at runtime with the actual locale/language settings.
const STEALTH_SCRIPTS_RAW: &str = include_str!("stealth_scripts.js");

/// Chrome launch arguments that reduce automation fingerprint surface.
pub const STEALTH_CHROMIUM_ARGS: &[&str] = &[
    "--disable-blink-features=AutomationControlled",
    "--use-gl=angle",
    "--use-angle=default",
];

/// Build the stealth JS payload with the given locale.
/// Replaces the default `__abStealth` config line with one reflecting the
/// actual browser locale so that `navigator.language` patches are consistent.
pub fn build_stealth_script(locale: Option<&str>) -> String {
    let locale = locale.unwrap_or("en-US");
    let base_lang = locale.split('-').next().unwrap_or(locale);
    let languages: Vec<&str> = if base_lang == locale {
        vec![locale]
    } else {
        vec![locale, base_lang]
    };
    let config_line = format!(
        r#"const __abStealth = {{ locale: "{}", languages: {}, allowWebGLContextFallback: false }};"#,
        locale,
        serde_json::to_string(&languages).unwrap_or_else(|_| r#"["en-US","en"]"#.to_string()),
    );

    // Replace the placeholder first line
    if let Some(rest) = STEALTH_SCRIPTS_RAW.strip_prefix(
        r#"const __abStealth = { locale: "en-US", languages: ["en-US", "en"], allowWebGLContextFallback: false };"#,
    ) {
        format!("{}{}", config_line, rest)
    } else {
        // Fallback: prepend config and include everything
        format!("{}\n{}", config_line, STEALTH_SCRIPTS_RAW)
    }
}

/// Apply stealth patches to a browser session:
/// 1. Inject init script (runs before any page JS on every navigation)
/// 2. Override User-Agent via CDP to remove HeadlessChrome markers
/// 3. Override navigator.userAgentData high-entropy hints
pub async fn apply_stealth(
    client: &CdpClient,
    session_id: &str,
    locale: Option<&str>,
) -> Result<(), String> {
    let script = build_stealth_script(locale);

    // 1. Inject stealth scripts to run before page JS
    client
        .send_command(
            "Page.addScriptToEvaluateOnNewDocument",
            Some(json!({ "source": script })),
            Some(session_id),
        )
        .await?;

    // 2. Detect current User-Agent and clean up HeadlessChrome marker
    let ua = get_browser_user_agent(client, session_id).await;
    if let Some(ua) = ua {
        let cleaned = ua.replace("HeadlessChrome", "Chrome");
        if cleaned != ua {
            client
                .send_command(
                    "Emulation.setUserAgentOverride",
                    Some(json!({
                        "userAgent": cleaned,
                        "acceptLanguage": locale.unwrap_or("en-US"),
                        "platform": platform_string(),
                        "userAgentMetadata": build_ua_metadata(&cleaned, locale),
                    })),
                    Some(session_id),
                )
                .await?;
        }
    }

    Ok(())
}

/// Get the browser's User-Agent string via CDP.
async fn get_browser_user_agent(client: &CdpClient, session_id: &str) -> Option<String> {
    let result = client
        .send_command(
            "Runtime.evaluate",
            Some(json!({ "expression": "navigator.userAgent", "returnByValue": true })),
            Some(session_id),
        )
        .await
        .ok()?;
    result
        .get("result")
        .and_then(|r| r.get("value"))
        .and_then(|v| v.as_str())
        .map(String::from)
}

/// Also run stealth script on the current page (for already-loaded pages after CDP attach).
pub async fn apply_stealth_to_current_page(
    client: &CdpClient,
    session_id: &str,
    locale: Option<&str>,
) -> Result<(), String> {
    let script = build_stealth_script(locale);
    client
        .send_command(
            "Runtime.evaluate",
            Some(json!({
                "expression": script,
                "returnByValue": true,
            })),
            Some(session_id),
        )
        .await?;
    Ok(())
}

/// Strip sourceURL comments from CDP expressions to avoid leaking
/// automation-framework identifiers in stack traces.
pub fn strip_source_url_labels(input: &str) -> String {
    // Remove //# sourceURL=... and //@ sourceURL=...
    let re_line = regex_lite::Regex::new(r"(?i)\n?\s*//[@#]\s*sourceURL=[^\n\r]*").unwrap();
    let output = re_line.replace_all(input, "");
    // Remove /*# sourceURL=...*/ block comments
    let re_block =
        regex_lite::Regex::new(r"(?is)\n?\s*/\*[@#]\s*sourceURL=[\s\S]*?\*/").unwrap();
    re_block.replace_all(&output, "").to_string()
}

fn platform_string() -> &'static str {
    if cfg!(target_os = "macos") {
        "macOS"
    } else if cfg!(target_os = "windows") {
        "Win32"
    } else {
        "Linux"
    }
}

fn platform_hint() -> &'static str {
    if cfg!(target_os = "macos") {
        "macOS"
    } else if cfg!(target_os = "windows") {
        "Windows"
    } else {
        "Linux"
    }
}

fn platform_version_hint() -> &'static str {
    if cfg!(target_os = "macos") {
        "14.0.0"
    } else if cfg!(target_os = "windows") {
        "10.0.0"
    } else {
        "6.5.0"
    }
}

fn build_ua_metadata(ua: &str, locale: Option<&str>) -> serde_json::Value {
    // Extract Chrome version from UA string
    let chrome_version = ua
        .split("Chrome/")
        .nth(1)
        .and_then(|s| s.split_whitespace().next())
        .unwrap_or("130.0.0.0");
    let major = chrome_version.split('.').next().unwrap_or("130");

    let _lang = locale.unwrap_or("en-US");

    json!({
        "brands": [
            { "brand": "Chromium", "version": major },
            { "brand": "Google Chrome", "version": major },
            { "brand": "Not?A_Brand", "version": "99" },
        ],
        "fullVersionList": [
            { "brand": "Chromium", "version": chrome_version },
            { "brand": "Google Chrome", "version": chrome_version },
            { "brand": "Not?A_Brand", "version": "99.0.0.0" },
        ],
        "fullVersion": chrome_version,
        "platform": platform_hint(),
        "platformVersion": platform_version_hint(),
        "architecture": if cfg!(target_arch = "aarch64") { "arm" } else { "x86" },
        "model": "",
        "mobile": false,
        "bitness": "64",
        "wow64": false,
    })
}
