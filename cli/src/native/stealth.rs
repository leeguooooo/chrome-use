//! Stealth anti-detection module.
//!
//! Injects browser-level patches to evade bot detection (creepjs, sannysoft,
//! Cloudflare Turnstile, etc.) by normalizing fingerprint signals that betray
//! headless or automated Chrome instances.

use serde_json::json;

use super::cdp::client::CdpClient;

/// Full stealth JS payload compiled at build time (for --launch mode).
const STEALTH_SCRIPTS_RAW: &str = include_str!("stealth_scripts.js");

/// Minimal stealth script for CDP-attach mode (connecting to user's real Chrome).
/// Only removes navigator.webdriver — the browser's own fingerprint is already real.
/// Minimal stealth script for CDP-attach mode.
/// Emulation.setAutomationOverride handles navigator.webdriver at the native
/// level, so no JS patching is needed in CdpAttach mode. An empty script
/// avoids creating any detectable lie-props artifacts.
const MINIMAL_STEALTH_SCRIPT: &str = "";

/// Chrome launch arguments that reduce automation fingerprint surface.
pub const STEALTH_CHROMIUM_ARGS: &[&str] = &[
    "--disable-blink-features=AutomationControlled",
    "--use-gl=angle",
    "--use-angle=default",
];

/// Connection mode determines which stealth patches to apply.
#[derive(Clone, Copy, PartialEq)]
pub enum StealthMode {
    /// Connected to user's real Chrome — minimal patches only (webdriver removal).
    /// The browser already has a real fingerprint; heavy patches would create detectable lies.
    CdpAttach,
    /// Launched a new Chrome instance — apply full stealth patches.
    FullLaunch,
}

/// Build the stealth JS payload for the given mode and locale.
pub fn build_stealth_script(mode: StealthMode, locale: Option<&str>) -> String {
    if mode == StealthMode::CdpAttach {
        return MINIMAL_STEALTH_SCRIPT.to_string();
    }

    // Full launch mode: inject all patches. Languages are never patched in
    // JS: a main-world override disagrees with workers and the Accept-Language
    // header, a mismatch Cloudflare rejects. AGENT_BROWSER_LOCALE is applied
    // natively via Chrome's --accept-lang switch instead.
    let locale = locale.unwrap_or("en-US");
    let config_line = format!(
        r#"const __abStealth = {{ locale: "{}", languages: [], allowWebGLContextFallback: false, hideCanvas: {}, canvasSeed: {}, disableIframeProxy: {} }};"#,
        locale,
        hide_canvas_enabled(),
        canvas_noise_seed(),
        disable_iframe_proxy_enabled(),
    );

    // NB: this prefix MUST match the first line of stealth_scripts.js verbatim,
    // otherwise the fallback below prepends a SECOND `const __abStealth`
    // declaration and the whole script dies with a redeclaration SyntaxError.
    let body = if let Some(rest) = STEALTH_SCRIPTS_RAW.strip_prefix(
        r#"const __abStealth = { locale: "en-US", languages: ["en-US", "en"], allowWebGLContextFallback: false, hideCanvas: false, canvasSeed: 0, disableIframeProxy: false };"#,
    ) {
        format!("{}{}", config_line, rest)
    } else {
        format!("{}\n{}", config_line, STEALTH_SCRIPTS_RAW)
    };
    // The guard runs in the new document, where `location` is already the
    // frame's real URL. Rust can't decide this at attach time: an OOPIF
    // attaches before its navigation commits, with an empty URL.
    format!("if (!{CHALLENGE_FRAME_GUARD_JS}) {{\n{body}\n}}")
}

/// True inside an anti-bot challenge frame (Cloudflare Turnstile / managed
/// challenge, hCaptcha, reCAPTCHA, DataDome, Arkose). Those frames
/// integrity-check their own runtime: with our patches evaluated into them,
/// Cloudflare's managed challenge spins forever. They keep only the native
/// overrides (e.g. Emulation.setAutomationOverride).
const CHALLENGE_FRAME_GUARD_JS: &str = r#"(function(){try{var h=String(location.hostname||'').toLowerCase(),p=String(location.pathname||'');return /(^|\.)(challenges\.cloudflare\.com|hcaptcha\.com|recaptcha\.net|captcha-delivery\.com|arkoselabs\.com|funcaptcha\.com)$/.test(h)||((h==='www.google.com'||h==='google.com')&&p.indexOf('/recaptcha/')===0)||p.indexOf('/cdn-cgi/challenge-platform/')!==-1}catch(e){return false}})()"#;

/// Whether canvas/audio fingerprint noise is opted into (FullLaunch only).
/// OFF by default: injecting noise is a deliberate "lie" that can itself be a
/// tell, so it's reserved for users who explicitly want it via
/// `AGENT_BROWSER_HIDE_CANVAS=1`.
fn hide_canvas_enabled() -> bool {
    std::env::var("AGENT_BROWSER_HIDE_CANVAS")
        .ok()
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false)
}

/// Whether to DROP the srcdoc-iframe `contentWindow` Proxy patch (FullLaunch).
/// That patch masks automation in srcdoc iframes, but the JS `Proxy` is itself a
/// fingerprintable tell (CreepJS `hasIframeProxy` → ~20% stealth). Off by default
/// (keep the patch); `AGENT_BROWSER_DISABLE_IFRAME_PROXY=1` drops it for a clean
/// 0% CreepJS at the cost of that niche srcdoc-iframe masking.
fn disable_iframe_proxy_enabled() -> bool {
    std::env::var("AGENT_BROWSER_DISABLE_IFRAME_PROXY")
        .ok()
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false)
}

/// A per-process seed so canvas/audio noise is STABLE within a session (a real
/// device returns the same hash on repeated reads) but differs from the
/// headless-stable default. 0 is avoided so the JS can treat it as "unset".
fn canvas_noise_seed() -> u32 {
    use std::sync::OnceLock;
    static SEED: OnceLock<u32> = OnceLock::new();
    *SEED.get_or_init(|| {
        use std::time::{SystemTime, UNIX_EPOCH};
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.subsec_nanos())
            .unwrap_or(0x9e3779b9);
        // mix the bits a little, then force non-zero
        let mixed = nanos ^ nanos.rotate_left(13).wrapping_mul(2654435761);
        mixed | 1
    })
}

/// Apply stealth patches to a browser session.
///
/// In `CdpAttach` mode (user's real Chrome): only removes `navigator.webdriver`.
/// In `FullLaunch` mode (new Chrome): injects all 32 patches + UA override.
pub async fn apply_stealth(
    client: &CdpClient,
    session_id: &str,
    mode: StealthMode,
    locale: Option<&str>,
) -> Result<(), String> {
    // First: disable the automation flag at the CDP protocol level.
    // This tells Chrome to natively set navigator.webdriver = false,
    // which is undetectable by lie-detection systems like CreepJS.
    // Falls back gracefully on older Chrome versions that don't support this.
    let _ = client
        .send_command(
            "Emulation.setAutomationOverride",
            Some(json!({ "enabled": false })),
            Some(session_id),
        )
        .await;

    let script = build_stealth_script(mode, locale);

    // Inject stealth scripts to run before page JS
    client
        .send_command(
            "Page.addScriptToEvaluateOnNewDocument",
            Some(json!({ "source": script })),
            Some(session_id),
        )
        .await?;

    // In full launch mode, also override UA to remove HeadlessChrome marker
    if mode == StealthMode::FullLaunch {
        let ua = get_browser_user_agent(client, session_id).await;
        if let Some(ua) = ua {
            let cleaned = ua.replace("HeadlessChrome", "Chrome");
            if cleaned != ua {
                client
                    .send_command(
                        "Emulation.setUserAgentOverride",
                        Some(json!({
                            "userAgent": cleaned,
                            "acceptLanguage": accept_language(locale.unwrap_or("en-US")),
                            "platform": platform_string(),
                            "userAgentMetadata": build_ua_metadata(&cleaned, locale),
                        })),
                        Some(session_id),
                    )
                    .await?;
            }
        }

        // Align the timezone for fresh launches when explicitly requested.
        // Headless/launched Chrome often reports UTC (or the host's zone), which
        // can contradict a proxy's geolocation or a spoofed locale.
        // `Emulation.setTimezoneOverride` is a NATIVE override — Intl.DateTimeFormat
        // and Date both follow it with no detectable JS lie. Opt-in only:
        //   AGENT_BROWSER_TIMEZONE=<IANA id>  -> use that zone (e.g. align to proxy)
        //   AGENT_BROWSER_TIMEZONE=auto       -> derive a default from the locale
        //   (unset)                           -> leave the real timezone untouched
        if let Some(tz) = resolve_timezone(locale) {
            let _ = client
                .send_command(
                    "Emulation.setTimezoneOverride",
                    Some(json!({ "timezoneId": tz })),
                    Some(session_id),
                )
                .await;
        }
    }

    Ok(())
}

/// Chrome `--accept-lang` list for a locale: `zh-CN` -> `zh-CN,zh`.
pub fn accept_lang_list(locale: &str) -> Option<String> {
    let locale = locale.trim();
    if locale.is_empty() {
        return None;
    }
    Some(match locale.split_once('-') {
        Some((base, _)) if !base.is_empty() => format!("{locale},{base}"),
        _ => locale.to_string(),
    })
}

/// Accept-Language header value for a locale, shaped like the one Chrome
/// itself sends: `zh-CN` -> `zh-CN,zh;q=0.9`, `en` -> `en`.
fn accept_language(locale: &str) -> String {
    match locale.split_once('-') {
        Some((base, _)) if !base.is_empty() => format!("{locale},{base};q=0.9"),
        _ => locale.to_string(),
    }
}

/// Resolve the timezone to emulate for a fresh-launch session, if any.
/// Controlled by `AGENT_BROWSER_TIMEZONE`: an explicit IANA id, or `auto` to
/// derive a sensible default from the locale. Returns `None` (leave the real
/// timezone) when unset, empty, or when `auto` can't map the locale.
fn resolve_timezone(locale: Option<&str>) -> Option<String> {
    let raw = std::env::var("AGENT_BROWSER_TIMEZONE").ok()?;
    let raw = raw.trim();
    if raw.is_empty() {
        return None;
    }
    if raw.eq_ignore_ascii_case("auto") {
        return locale.and_then(locale_default_timezone).map(str::to_string);
    }
    Some(raw.to_string())
}

/// Best-effort IANA timezone for a locale. Used only for
/// `AGENT_BROWSER_TIMEZONE=auto`; unknown locales return `None` so the real
/// timezone is left untouched rather than guessing a wrong one.
fn locale_default_timezone(locale: &str) -> Option<&'static str> {
    let tz = match locale.to_ascii_lowercase().as_str() {
        "en-us" => "America/New_York",
        "en-ca" => "America/Toronto",
        "en-gb" => "Europe/London",
        "en-au" => "Australia/Sydney",
        "ja" | "ja-jp" => "Asia/Tokyo",
        "ko" | "ko-kr" => "Asia/Seoul",
        "zh-cn" | "zh-hans" | "zh-hans-cn" => "Asia/Shanghai",
        "zh-tw" | "zh-hant" | "zh-hant-tw" => "Asia/Taipei",
        "zh-hk" => "Asia/Hong_Kong",
        "de" | "de-de" => "Europe/Berlin",
        "fr" | "fr-fr" => "Europe/Paris",
        "es" | "es-es" => "Europe/Madrid",
        "it" | "it-it" => "Europe/Rome",
        "nl" | "nl-nl" => "Europe/Amsterdam",
        "pt-br" => "America/Sao_Paulo",
        "pt" | "pt-pt" => "Europe/Lisbon",
        "ru" | "ru-ru" => "Europe/Moscow",
        _ => return None,
    };
    Some(tz)
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
    mode: StealthMode,
    locale: Option<&str>,
) -> Result<(), String> {
    let script = build_stealth_script(mode, locale);
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
    let re_block = regex_lite::Regex::new(r"(?is)\n?\s*/\*[@#]\s*sourceURL=[\s\S]*?\*/").unwrap();
    re_block.replace_all(&output, "").to_string()
}

/// The legacy `navigator.platform` value (set via the CDP
/// `Emulation.setUserAgentOverride` `platform` field). This is NOT the UA-CH
/// platform (see `platform_hint`): real Chrome reports `MacIntel` on macOS and
/// `Linux x86_64` on Linux, so emitting the UA-CH form ("macOS"/"Linux") here is
/// a detectable mismatch against the UA's "Intel Mac OS X" / Linux strings.
fn platform_string() -> &'static str {
    if cfg!(target_os = "macos") {
        "MacIntel"
    } else if cfg!(target_os = "windows") {
        "Win32"
    } else {
        "Linux x86_64"
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

#[cfg(test)]
mod locale_tests {
    use super::{accept_language, build_stealth_script, StealthMode};

    #[test]
    fn challenge_frames_skip_js_patches() {
        // Evaluate the generated guard under node with a stubbed `location`.
        let Ok(node) = which_node() else { return };
        let guard = super::CHALLENGE_FRAME_GUARD_JS;
        for (url, expect) in [
            (
                "https://challenges.cloudflare.com/cdn-cgi/challenge-platform/h/b/turnstile/if",
                true,
            ),
            (
                "https://newassets.hcaptcha.com/captcha/v1/x/static/hcaptcha.html",
                true,
            ),
            ("https://www.google.com/recaptcha/api2/anchor?k=x", true),
            (
                "https://geo.captcha-delivery.com/captcha/?initialCid=x",
                true,
            ),
            ("https://www.google.com/maps/embed", false),
            ("https://js.stripe.com/v3/elements-inner", false),
            ("https://notcloudflare.com.evil.test/", false),
        ] {
            let src = format!("var location=new URL({url:?});console.log({guard})");
            let out = std::process::Command::new(&node)
                .arg("-e")
                .arg(src)
                .output()
                .unwrap();
            assert_eq!(
                String::from_utf8_lossy(&out.stdout).trim(),
                expect.to_string(),
                "{url}"
            );
        }
        // The whole payload must still parse once wrapped in the guard block.
        let script = build_stealth_script(StealthMode::FullLaunch, None);
        let out = std::process::Command::new(&node)
            .arg("-e")
            .arg(format!("new Function({:?})", script))
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    fn which_node() -> Result<std::path::PathBuf, ()> {
        std::env::var_os("PATH")
            .and_then(|p| {
                std::env::split_paths(&p)
                    .map(|d| d.join("node"))
                    .find(|n| n.is_file())
            })
            .ok_or(())
    }

    #[test]
    fn accept_language_matches_chrome_shape() {
        assert_eq!(accept_language("zh-CN"), "zh-CN,zh;q=0.9");
        assert_eq!(accept_language("en"), "en");
        assert_eq!(
            super::accept_lang_list("ja-JP").as_deref(),
            Some("ja-JP,ja")
        );
        assert_eq!(super::accept_lang_list(" "), None);
    }

    #[test]
    fn languages_are_never_patched_in_js() {
        for locale in [None, Some("ja-JP")] {
            let script = build_stealth_script(StealthMode::FullLaunch, locale);
            assert!(script.contains("languages: [], "));
            assert!(!script.contains("__abRedefineNavProto('languages'"));
        }
    }

    #[test]
    fn android_only_apis_are_not_faked() {
        let script = build_stealth_script(StealthMode::FullLaunch, None);
        for needle in [
            "downlinkMax",
            "ContentIndex",
            "ContactsManager",
            "NativeWorker",
        ] {
            // The explanatory comment names them; no code may define them.
            let code: String = script
                .lines()
                .filter(|l| !l.trim_start().starts_with("//"))
                .collect();
            assert!(!code.contains(needle), "{needle} is faked again");
        }
    }
}

#[cfg(test)]
mod timezone_tests {
    use super::{locale_default_timezone, resolve_timezone};

    #[test]
    fn maps_common_locales_case_insensitively() {
        assert_eq!(locale_default_timezone("en-US"), Some("America/New_York"));
        assert_eq!(locale_default_timezone("ja-JP"), Some("Asia/Tokyo"));
        assert_eq!(locale_default_timezone("zh-CN"), Some("Asia/Shanghai"));
        assert_eq!(locale_default_timezone("ZH-TW"), Some("Asia/Taipei"));
        assert_eq!(locale_default_timezone("ja"), Some("Asia/Tokyo"));
    }

    #[test]
    fn unknown_locale_returns_none() {
        assert_eq!(locale_default_timezone("xx-YY"), None);
        assert_eq!(locale_default_timezone(""), None);
    }

    #[test]
    fn resolve_timezone_honors_env() {
        // Serialized via a single test to avoid cross-test env races on this key.
        std::env::remove_var("AGENT_BROWSER_TIMEZONE");
        assert_eq!(resolve_timezone(Some("en-US")), None);

        std::env::set_var("AGENT_BROWSER_TIMEZONE", "Europe/Berlin");
        assert_eq!(resolve_timezone(None), Some("Europe/Berlin".to_string()));

        std::env::set_var("AGENT_BROWSER_TIMEZONE", "  ");
        assert_eq!(resolve_timezone(Some("en-US")), None);

        std::env::set_var("AGENT_BROWSER_TIMEZONE", "auto");
        assert_eq!(
            resolve_timezone(Some("ja-JP")),
            Some("Asia/Tokyo".to_string())
        );
        assert_eq!(resolve_timezone(Some("xx-YY")), None);
        assert_eq!(resolve_timezone(None), None);

        std::env::remove_var("AGENT_BROWSER_TIMEZONE");
    }
}
