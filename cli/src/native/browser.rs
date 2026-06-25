use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::{broadcast, Mutex};

use super::cdp::chrome::{auto_connect_cdp, launch_chrome, ChromeProcess, LaunchOptions};
use super::cdp::client::CdpClient;
use super::cdp::discovery::discover_cdp_url;
use super::cdp::lightpanda::{launch_lightpanda, LightpandaLaunchOptions, LightpandaProcess};
use super::cdp::types::*;
use super::element::{resolve_element_object_id, RefMap};

/// The daemon's session name, set once at daemon start. Names the Chrome tab
/// group that abs-created tabs land in when driving the user's real Chrome via
/// the `ab-connect` extension, so each agent/session gets its own group.
pub static DAEMON_SESSION: std::sync::OnceLock<String> = std::sync::OnceLock::new();

// ---------------------------------------------------------------------------
// Launch validation
// ---------------------------------------------------------------------------

/// Validates launch/connect options for incompatible combinations.
/// Returns `Ok(())` if valid, or `Err(msg)` with a user-friendly error.
pub fn validate_launch_options(
    extensions: Option<&[String]>,
    has_cdp: bool,
    profile: Option<&str>,
    storage_state: Option<&str>,
    allow_file_access: bool,
    executable_path: Option<&str>,
) -> Result<(), String> {
    let has_extensions = extensions.map(|e| !e.is_empty()).unwrap_or(false);

    if has_extensions && has_cdp {
        return Err(
            "Cannot use extensions with cdp_url (extensions require local browser launch)"
                .to_string(),
        );
    }
    if profile.is_some() && has_cdp {
        return Err(
            "Cannot use profile with cdp_url (profile requires local browser launch)".to_string(),
        );
    }
    if storage_state.is_some() && profile.is_some() {
        return Err("Cannot use storage_state with profile".to_string());
    }
    if storage_state.is_some() && has_extensions {
        return Err("Cannot use storage_state with extensions".to_string());
    }
    if allow_file_access {
        if let Some(path) = executable_path {
            let lower = path.to_lowercase();
            if lower.contains("firefox") || lower.contains("webkit") || lower.contains("safari") {
                return Err(
                    "allow_file_access is not supported with non-Chromium browsers".to_string(),
                );
            }
        }
    }
    Ok(())
}

/// Validates that Chrome-only options are not used with Lightpanda.
fn validate_lightpanda_options(options: &LaunchOptions) -> Result<(), String> {
    if options
        .extensions
        .as_ref()
        .map(|e| !e.is_empty())
        .unwrap_or(false)
    {
        return Err("Extensions are not supported with Lightpanda".to_string());
    }
    if options.profile.is_some() {
        return Err("Profiles are not supported with Lightpanda".to_string());
    }
    if options.storage_state.is_some() {
        return Err("Storage state is not supported with Lightpanda".to_string());
    }
    if options.allow_file_access {
        return Err("File access is not supported with Lightpanda".to_string());
    }
    if !options.headless {
        return Err("Headed mode is not supported with Lightpanda (headless only)".to_string());
    }
    if !options.args.is_empty() {
        return Err(
            "Custom Chrome arguments (--args) are not supported with Lightpanda".to_string(),
        );
    }
    Ok(())
}

/// Returns true for Chrome internal targets that should not be selected
/// during auto-connect (e.g. chrome://, chrome-extension://, devtools://).
fn is_internal_chrome_target(url: &str) -> bool {
    url.starts_with("chrome://")
        || url.starts_with("chrome-extension://")
        || url.starts_with("devtools://")
}

pub(crate) fn should_track_target(target: &TargetInfo) -> bool {
    (target.target_type == "page" || target.target_type == "webview")
        && (target.url.is_empty() || !is_internal_chrome_target(&target.url))
}

/// Origin + path of a URL, dropping the query string and fragment, for
/// `--reuse-tab` matching. SPA/SSO URLs carry volatile `?client_id=…&state=…`
/// and `#/route` parts, so two opens of the "same" page rarely match
/// byte-for-byte; comparing origin+path lands the reuse on the right tab.
/// Returns the input unchanged if it doesn't parse as a URL.
fn normalize_url_for_match(url: &str) -> String {
    match url::Url::parse(url) {
        Ok(u) => format!("{}{}", u.origin().ascii_serialization(), u.path()),
        Err(_) => url.to_string(),
    }
}

fn update_page_target_info_in_pages(pages: &mut [PageInfo], target: &TargetInfo) -> bool {
    if let Some(page) = pages.iter_mut().find(|p| p.target_id == target.target_id) {
        page.url = target.url.clone();
        page.title = sanitize_title(&target.title);
        page.target_type = target.target_type.clone();
        return true;
    }
    false
}

fn active_page_index_after_removal(
    active_page_index: usize,
    removed_index: usize,
    remaining_pages: usize,
) -> usize {
    if remaining_pages == 0 {
        return 0;
    }

    if removed_index < active_page_index {
        return active_page_index - 1;
    }

    if active_page_index >= remaining_pages {
        return remaining_pages - 1;
    }

    active_page_index
}

/// Resolve the session's active page index: prefer the pinned `active_target_id`
/// (stable across tab reorder / passive discovery / removal), falling back to the
/// raw `active_page_index` only when nothing is pinned or the pin is gone. Keeping
/// commands anchored to the pinned target is what stops `eval`/`get url`/`snapshot`
/// from drifting onto a foreign tab between commands (issue #14).
fn resolve_active_index(
    pages: &[PageInfo],
    active_target_id: Option<&str>,
    active_page_index: usize,
) -> usize {
    if let Some(tid) = active_target_id {
        if let Some(i) = pages.iter().position(|p| p.target_id == tid) {
            return i;
        }
    }
    active_page_index
}

/// Message when a relay session's pinned tab can no longer be resolved.
const BOUND_TAB_GONE: &str =
    "the tab this session was driving can no longer be resolved (it was closed, or a flaky \
     relay snapshot dropped it). Refusing to silently retarget — that could read/click the \
     wrong tab. Re-open your target URL (`open <url>`) or `adopt <url>` the tab you want.";

/// Message when a relay session has no tab of its own to route a command to.
const NO_OWNED_TAB: &str =
    "this session owns no resolvable tab in its group. Refusing to run on a tab this session \
     didn't open — on the shared browser that could read/click the user's or another agent's \
     tab. `open <url>` to create your own tab, or `adopt <url>` to explicitly take an existing \
     one.";

/// Strict session-index resolution for routing READ/CLICK commands.
///
/// On the relay (shared real browser) the lenient [`resolve_active_index`] falls
/// back to `active_page_index` when the pinned target isn't found — and after
/// foreign-tab churn that index can point at an unrelated tab, so an
/// `eval`/`snapshot`/`click` would land on it (issue #52, a safety risk).
///
/// The relay keeps a STABLE `target_id` across navigations (verified live), so a
/// present pin resolves on every normal command — a pin that genuinely can't be
/// found means the bound tab is gone, which we surface as a loud error instead of
/// drifting. With no pin set (e.g. before the first `open`), or off the relay
/// (a browser we launched, no foreign tabs), behave leniently — unchanged.
fn strict_session_index(
    pages: &[PageInfo],
    active_target_id: Option<&str>,
    active_page_index: usize,
    on_relay: bool,
    created_targets: &HashSet<String>,
) -> Result<usize, String> {
    if on_relay {
        // Prefer the pin — it must resolve to a tab this session owns.
        if let Some(tid) = active_target_id {
            return pages
                .iter()
                .position(|p| p.target_id == tid && created_targets.contains(&p.target_id))
                .ok_or_else(|| BOUND_TAB_GONE.to_string());
        }
        // No pin: resolve only to a tab THIS session owns. The lenient fallback
        // would return `active_page_index`, which after foreign-tab churn can
        // point at the user's / another agent's tab — and a read/click would land
        // on it (issue #52). Each agent is scoped to its own group; using a tab
        // outside it requires an explicit `adopt`, so refuse to drift here.
        let i = resolve_active_index(pages, None, active_page_index);
        if pages
            .get(i)
            .is_some_and(|p| created_targets.contains(&p.target_id))
        {
            return Ok(i);
        }
        return Err(NO_OWNED_TAB.to_string());
    }
    Ok(resolve_active_index(
        pages,
        active_target_id,
        active_page_index,
    ))
}

/// Strip zero-width / invisible / bidi-format Unicode from a page title before
/// we store it. Some sites prepend runs of ZWJ / word-joiner / invisible-times /
/// BOM to `document.title` (badging, watermarking, anti-scrape); left in, they
/// pollute `tab list`, break text matching, and wreck column alignment (#33).
fn sanitize_title(s: &str) -> String {
    s.chars()
        .filter(|&c| {
            !matches!(c as u32,
                0x00AD            // soft hyphen
                | 0x200B..=0x200F // ZWSP, ZWNJ, ZWJ, LRM, RLM
                | 0x2028 | 0x2029 // line / paragraph separators
                | 0x202A..=0x202E // bidi embedding/override
                | 0x2060..=0x2064 // word joiner, invisible operators
                | 0x2066..=0x2069 // bidi isolates
                | 0x180E          // Mongolian vowel separator
                | 0xFEFF          // BOM / ZW no-break space
            )
        })
        .collect::<String>()
        .trim()
        .to_string()
}

/// Best-effort MIME type from a filename extension, for the relay file-upload
/// fallback (the page-constructed `File` needs a sensible `type`). Covers the
/// common upload kinds; anything unknown falls back to a generic binary type.
fn mime_for_path(name: &str) -> &'static str {
    let ext = name.rsplit('.').next().unwrap_or("").to_lowercase();
    match ext.as_str() {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "svg" => "image/svg+xml",
        "bmp" => "image/bmp",
        "pdf" => "application/pdf",
        "txt" => "text/plain",
        "csv" => "text/csv",
        "json" => "application/json",
        "mp4" => "video/mp4",
        "webm" => "video/webm",
        "mov" => "video/quicktime",
        "mp3" => "audio/mpeg",
        "zip" => "application/zip",
        _ => "application/octet-stream",
    }
}

/// Target ids to prune after a `Target.getTargets` resync: tracked pages whose
/// target is no longer in the live set — EXCEPT the explicitly-pinned active
/// target, which is protected. The relay against a busy real Chrome occasionally
/// returns a different window's tabs for a single `getTargets` call ("tab list
/// hops windows", issue #31); pruning on that transient snapshot would drop the
/// agent's adopted tab and drift subsequent eval/click onto a foreign tab. A
/// genuine close still arrives as `Target.targetDestroyed` (handled in the event
/// drain), which removes the pin properly — so protecting it here only guards
/// against flaky snapshots, not real closures.
fn prunable_target_ids(
    pages: &[PageInfo],
    live_ids: &HashSet<String>,
    pinned: Option<&str>,
) -> Vec<String> {
    pages
        .iter()
        .map(|p| p.target_id.clone())
        .filter(|tid| !live_ids.contains(tid) && pinned != Some(tid.as_str()))
        .collect()
}

/// Consecutive missing `getTargets` snapshots before an owned relay tab is
/// pruned. >1 so a single churning/partial snapshot (other agents opening/closing
/// tabs) or a brief cross-process-nav gap can't drop the tab the agent is driving.
const RELAY_PRUNE_MISSES: u32 = 3;

/// Debounced prune for the relay: target ids to drop, mutating per-target miss
/// counters. A tab in `live_ids` resets to 0; an absent (non-pinned) tab
/// increments and is pruned only at `RELAY_PRUNE_MISSES`. Counters for
/// no-longer-tracked targets are forgotten. Pure, so the multi-agent churn
/// tolerance is unit-testable without a live browser.
fn debounced_prune_ids(
    pages: &[PageInfo],
    live_ids: &HashSet<String>,
    pinned: Option<&str>,
    misses: &mut HashMap<String, u32>,
) -> Vec<String> {
    let tracked: HashSet<&str> = pages.iter().map(|p| p.target_id.as_str()).collect();
    misses.retain(|tid, _| tracked.contains(tid.as_str()));
    let mut prune = Vec::new();
    for p in pages {
        let tid = p.target_id.as_str();
        if live_ids.contains(tid) {
            misses.remove(tid);
            continue;
        }
        if pinned == Some(tid) {
            continue;
        }
        let c = misses.entry(p.target_id.clone()).or_insert(0);
        *c += 1;
        if *c >= RELAY_PRUNE_MISSES {
            prune.push(p.target_id.clone());
        }
    }
    prune
}

/// Whether the resolved active page is a tab the session created (its target_id
/// is in `created_targets`). Pure core of [`BrowserManager::active_is_session_owned`]
/// so the relay no-hijack rule is unit-testable without a live browser.
fn active_index_is_owned(
    pages: &[PageInfo],
    active_target_id: Option<&str>,
    active_page_index: usize,
    created_targets: &HashSet<String>,
) -> bool {
    pages
        .get(resolve_active_index(
            pages,
            active_target_id,
            active_page_index,
        ))
        .map(|p| created_targets.contains(&p.target_id))
        .unwrap_or(false)
}

/// Whether a CDP error means the bound relay target is gone — the tab was
/// closed, navigated across processes (renderer swap), or lost after an
/// extension/service-worker restart, and the relay could not re-attach. The
/// ab-connect relay surfaces these as `stale sessionId … its tab is gone`,
/// `unknown sessionId …`, or `no attached tab …`. `navigate` keys its
/// auto-reattach recovery off this (issue #35) so a dead session rebinds to a
/// fresh tab instead of erroring on every command until the user runs `tab new`.
fn is_stale_target_error(error: &str) -> bool {
    let lower = error.to_lowercase();
    lower.contains("its tab is gone")
        || lower.contains("stale sessionid")
        || lower.contains("unknown sessionid")
        || lower.contains("no attached tab")
}

/// Converts common error messages into AI-friendly, actionable descriptions.
pub fn to_ai_friendly_error(error: &str) -> String {
    let lower = error.to_lowercase();
    if lower.contains("strict mode violation") {
        return "Element matched multiple results. Use a more specific selector.".to_string();
    }
    if lower.contains("element is not visible") {
        return "Element exists but is not visible. Wait for it to become visible or scroll it into view."
            .to_string();
    }
    if lower.contains("intercept") {
        return "Another element is covering the target element. Try scrolling or closing overlays."
            .to_string();
    }
    if lower.contains("timeout") {
        return "Operation timed out. The page may still be loading or the element may not exist."
            .to_string();
    }
    if lower.contains("element not found") || lower.contains("no element") {
        // Selectors / `find` match the page DOM, which can't see inside a CLOSED
        // shadow root or a cross-origin iframe — but `snapshot -i` (the CDP
        // accessibility tree) pierces both. Also nudge to verify the exact
        // label, since translations differ (real case: LinkedIn's Save button is
        // labelled 收藏, not 保存 — issue #55).
        return "Element not found in the page DOM. If it's inside a CLOSED shadow root or a \
                cross-origin iframe, selectors/`find` can't reach it — run `snapshot -i` (it \
                pierces both via the accessibility tree) and `click` the @ref. Also verify the \
                exact label/text: translations differ (e.g. a \"Save\" button may be labelled 收藏)."
            .to_string();
    }
    error.to_string()
}

#[derive(Debug, Clone)]
pub struct PageInfo {
    pub tab_id: u32,
    /// Optional user-assigned label (e.g. "docs", "app"). Set via
    /// `tab new --label <name>`. Labels are agent-assigned and never
    /// auto-generated, never rewritten on navigation, and unique within a
    /// session. Agents use labels instead of `t<N>` for readable multi-tab
    /// workflows.
    pub label: Option<String>,
    pub target_id: String,
    pub session_id: String,
    pub url: String,
    pub title: String,
    pub target_type: String, // "page" or "webview"
}

/// Canonical string form of a stable tab id: `t1`, `t2`, ... The `t` prefix
/// disambiguates stable ids from positional indices (which the CLI no longer
/// accepts) and matches the `@e<N>` convention used for element refs.
pub fn format_tab_id(tab_id: u32) -> String {
    format!("t{}", tab_id)
}

/// A tab reference as parsed from CLI/JSON input. Either a stable id like
/// `t2` or a user-assigned label like `docs`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TabRef {
    Id(u32),
    Label(String),
}

impl TabRef {
    /// Parse a user-supplied string tab reference. Rejects bare integers
    /// with a teaching error so agents and scripts don't silently confuse
    /// stable ids with positional indices.
    pub fn parse(input: &str) -> Result<Self, String> {
        let input = input.trim();
        if input.is_empty() {
            return Err("Empty tab reference; expected `t<N>` (e.g. `t2`) or a label".to_string());
        }
        if let Some(digits) = input.strip_prefix('t').or_else(|| input.strip_prefix('T')) {
            if !digits.is_empty() && digits.chars().all(|c| c.is_ascii_digit()) {
                let id: u32 = digits.parse().map_err(|_| {
                    format!(
                        "Tab id `{}` out of range; ids are incrementing positive integers",
                        input
                    )
                })?;
                if id == 0 {
                    return Err(format!(
                        "Tab id `{}` is invalid; tab ids start at t1",
                        input
                    ));
                }
                return Ok(TabRef::Id(id));
            }
        }
        if input.chars().all(|c| c.is_ascii_digit()) {
            return Err(format!(
                "Expected a tab id like `t{}` or a label; positional integers are not accepted \
                 (run `chrome-use tab` to list stable tab ids)",
                input
            ));
        }
        if !is_valid_label(input) {
            return Err(format!(
                "Invalid tab label `{}`; labels must start with a letter and contain only \
                 letters, digits, `-`, and `_`",
                input
            ));
        }
        Ok(TabRef::Label(input.to_string()))
    }
}

/// Labels must look like identifiers: start with a letter, contain only
/// letters/digits/dashes/underscores. This keeps them distinguishable from
/// `t<N>` ids at a glance and safe to pass through shells without quoting.
pub fn is_valid_label(s: &str) -> bool {
    let mut chars = s.chars();
    match chars.next() {
        Some(c) if c.is_ascii_alphabetic() => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WaitUntil {
    Load,
    DomContentLoaded,
    NetworkIdle,
    None,
}

impl WaitUntil {
    pub fn from_str(s: &str) -> Self {
        match s {
            "domcontentloaded" => Self::DomContentLoaded,
            "networkidle" => Self::NetworkIdle,
            "none" => Self::None,
            _ => Self::Load,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Load => "load",
            Self::DomContentLoaded => "domcontentloaded",
            Self::NetworkIdle => "networkidle",
            Self::None => "none",
        }
    }
}

pub enum BrowserProcess {
    Chrome(ChromeProcess),
    Lightpanda(LightpandaProcess),
}

impl BrowserProcess {
    pub fn kill(&mut self) {
        match self {
            BrowserProcess::Chrome(p) => p.kill(),
            BrowserProcess::Lightpanda(p) => p.kill(),
        }
    }

    pub fn wait_or_kill(&mut self, timeout: std::time::Duration) {
        match self {
            BrowserProcess::Chrome(p) => p.wait_or_kill(timeout),
            BrowserProcess::Lightpanda(p) => p.kill(),
        }
    }

    /// Non-blocking check whether the browser process has exited.
    pub fn has_exited(&mut self) -> bool {
        match self {
            BrowserProcess::Chrome(p) => p.has_exited(),
            BrowserProcess::Lightpanda(_) => false,
        }
    }
}

pub struct BrowserManager {
    pub client: Arc<CdpClient>,
    browser_process: Option<BrowserProcess>,
    ws_url: String,
    pages: Vec<PageInfo>,
    active_page_index: usize,
    default_timeout_ms: u64,
    /// Stored download path from launch options, re-applied to new contexts (e.g., recording)
    pub download_path: Option<String>,
    /// Whether to ignore HTTPS certificate errors, re-applied to new contexts (e.g., recording)
    pub ignore_https_errors: bool,
    /// Origins visited during this session, used by save_state to collect cross-origin localStorage.
    visited_origins: HashSet<String>,
    /// Target IDs of tabs THIS session created via `Target.createTarget`. When
    /// connected to the user's real Chrome (not a launched browser), these are
    /// closed on `close()` so the session's tabs don't pile up in the user's
    /// browser after it ends. Only ever holds tabs we created — never the user's
    /// existing tabs or other sessions' tabs — so closing them is always safe.
    created_targets: HashSet<String>,
    /// The session's *intended* active tab, pinned by stable target_id rather
    /// than the fragile `active_page_index`. Set on every explicit open / tab new
    /// / tab switch. `active_session_id` resolves through this so a foreign tab
    /// opening (passive discovery), a tab closing, or list reordering can't drift
    /// the session's commands onto the wrong page — the wrong-origin-fetch hazard
    /// in the dogfood reports. Falls back to the index if the pinned tab is gone.
    active_target_id: Option<String>,
    /// Per-target count of CONSECUTIVE `resync_targets` snapshots in which an
    /// owned tab was missing from `Target.getTargets`. Over the relay a single
    /// snapshot routinely omits live tabs (multi-agent churn, a cross-process nav
    /// briefly dropping the target), so we must not prune on one miss — that lost
    /// the tab the agent was driving. A tab is removed only after it's been absent
    /// for `RELAY_PRUNE_MISSES` consecutive snapshots; any snapshot that includes
    /// it resets the counter. Keyed by stable target_id.
    relay_target_misses: HashMap<String, u32>,
    /// Whether the relay accepted this session's group announcement and is
    /// therefore scoping `Target.getTargets` to our own tab group (issue #40).
    /// When true the daemon can safely adopt new targets again (follow-popup,
    /// cross-session adopt) — the relay has already filtered out foreign tabs.
    /// When false (launch-on-real-CDP, or an older relay that didn't answer the
    /// announce) the daemon keeps strict daemon-side isolation.
    relay_scoped: bool,
    next_tab_id: u32,
    /// Whether to enable the CDP `Runtime` domain (console / error / exception capture).
    /// OFF by default for stealth: a live `Runtime.enable` is a detectable CDP signal
    /// (the patchright / rebrowser "runtime leak") — even when attached to the user's
    /// real Chrome. Opt in via `AGENT_BROWSER_CAPTURE_CONSOLE=1` when you need the
    /// `console` / `errors` commands to return page output.
    pub capture_console: bool,
}

/// Whether console/error capture (and thus `Runtime.enable`) is opted into for this
/// daemon. Defaults to `false` so the common automation path leaves no Runtime-domain
/// fingerprint. Set `AGENT_BROWSER_CAPTURE_CONSOLE=1` (or `true`) to turn it on.
pub fn console_capture_enabled() -> bool {
    std::env::var("AGENT_BROWSER_CAPTURE_CONSOLE")
        .ok()
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false)
}

const LIGHTPANDA_CDP_CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const LIGHTPANDA_CDP_CONNECT_POLL_INTERVAL: Duration = Duration::from_millis(100);
const LIGHTPANDA_TARGET_INIT_TIMEOUT: Duration = Duration::from_secs(10);

/// Outcome of a single `Browser.getVersion` liveness probe.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LivenessProbe {
    /// Chrome answered — the connection is definitely alive.
    Responded,
    /// The CDP transport errored (WebSocket closed/reset) — the socket is gone.
    TransportError,
    /// The probe timed out with no response.
    TimedOut,
}

/// Decide whether a CDP connection should be considered alive from one probe.
///
/// The subtle case is [`LivenessProbe::TimedOut`]. For a browser we launched
/// ourselves (`is_external_attach == false`) a hung CDP socket is a real
/// problem and the daemon should reconnect. But for an *externally attached*
/// browser — the stealth fork's default, where we attach to the user's real
/// Chrome — a slow/no response is almost always Chrome being briefly busy or,
/// critically, showing the Chrome 136+ "Allow remote debugging?" consent modal,
/// which blocks CDP responses until the user clicks Allow.
///
/// Treating that timeout as "dead" tears down the already-consented connection
/// and forces a reconnect, which re-pops the consent prompt; repeated on every
/// command it produces an endless prompt loop and a connection storm that can
/// freeze Chrome. So for external attaches we keep the connection alive on
/// timeout. A genuinely dead external socket instead surfaces as
/// [`LivenessProbe::TransportError`] (and Chrome being closed by the user is a
/// transport error, not a timeout), so zombie-socket detection is preserved.
fn connection_alive_from_probe(probe: LivenessProbe, is_external_attach: bool) -> bool {
    match probe {
        LivenessProbe::Responded => true,
        LivenessProbe::TransportError => false,
        LivenessProbe::TimedOut => is_external_attach,
    }
}

impl BrowserManager {
    pub async fn launch(options: LaunchOptions, engine: Option<&str>) -> Result<Self, String> {
        let engine = engine.unwrap_or("chrome");

        match engine {
            "chrome" => {
                validate_launch_options(
                    options.extensions.as_deref(),
                    false,
                    options.profile.as_deref(),
                    options.storage_state.as_deref(),
                    options.allow_file_access,
                    options.executable_path.as_deref(),
                )?;
            }
            "lightpanda" => {
                validate_lightpanda_options(&options)?;
            }
            _ => {
                return Err(format!(
                    "Unknown engine '{}'. Supported engines: chrome, lightpanda",
                    engine
                ));
            }
        }

        let ignore_https_errors = options.ignore_https_errors;
        let user_agent = options.user_agent.clone();
        let color_scheme = options.color_scheme.clone();
        let download_path = options.download_path.clone();

        let (ws_url, process) = match engine {
            "lightpanda" => {
                let lp_options = LightpandaLaunchOptions {
                    executable_path: options.executable_path.clone(),
                    proxy: options.proxy.clone(),
                    port: None,
                };
                let lp = launch_lightpanda(&lp_options).await?;
                let url = lp.ws_url.clone();
                (url, BrowserProcess::Lightpanda(lp))
            }
            _ => {
                let chrome = tokio::task::spawn_blocking(move || launch_chrome(&options))
                    .await
                    .map_err(|e| format!("Chrome launch task failed: {}", e))??;
                let url = chrome.ws_url.clone();
                (url, BrowserProcess::Chrome(chrome))
            }
        };

        // A launched browser carries a debug port → it's the other path that can
        // pop Chrome's consent modal; record it for #31 diagnosis.
        crate::connect::log_connect_mode(
            &ws_url,
            true,
            DAEMON_SESSION
                .get()
                .map(String::as_str)
                .unwrap_or("default"),
        );
        let manager = if engine == "lightpanda" {
            initialize_lightpanda_manager(ws_url, process).await?
        } else {
            let client = Arc::new(CdpClient::connect(&ws_url).await?);
            let mut manager = Self {
                client,
                browser_process: Some(process),
                ws_url,
                pages: Vec::new(),
                active_page_index: 0,
                default_timeout_ms: 25_000,
                download_path: download_path.clone(),
                ignore_https_errors,
                visited_origins: HashSet::new(),
                created_targets: HashSet::new(),
                active_target_id: None,
                relay_target_misses: HashMap::new(),
                relay_scoped: false,
                next_tab_id: 1,
                capture_console: console_capture_enabled(),
            };
            manager.discover_and_attach_targets().await?;
            manager
        };

        let session_id = manager.active_session_id()?.to_string();

        if ignore_https_errors {
            let _ = manager
                .client
                .send_command(
                    "Security.setIgnoreCertificateErrors",
                    Some(json!({ "ignore": true })),
                    Some(&session_id),
                )
                .await;
        }

        if let Some(ref ua) = user_agent {
            let _ = manager
                .client
                .send_command(
                    "Emulation.setUserAgentOverride",
                    Some(json!({ "userAgent": ua })),
                    Some(&session_id),
                )
                .await;
        }

        if let Some(ref scheme) = color_scheme {
            let _ = manager
                .client
                .send_command(
                    "Emulation.setEmulatedMedia",
                    Some(json!({ "features": [{ "name": "prefers-color-scheme", "value": scheme }] })),
                    Some(&session_id),
                )
                .await;
        }

        if let Some(ref path) = download_path {
            let _ = manager
                .client
                .send_command(
                    "Browser.setDownloadBehavior",
                    Some(json!({ "behavior": "allow", "downloadPath": path })),
                    None,
                )
                .await;
        }

        Ok(manager)
    }

    pub async fn connect_cdp(url: &str) -> Result<Self, String> {
        Self::connect_cdp_inner(url, false, None).await
    }

    /// Connect to a provider CDP proxy where the WebSocket IS the page session.
    /// Skips browser-level Target.* commands that most proxies don't support.
    pub async fn connect_cdp_direct(url: &str) -> Result<Self, String> {
        Self::connect_cdp_inner(url, true, None).await
    }

    pub async fn connect_cdp_with_headers(
        url: &str,
        headers: Option<Vec<(String, String)>>,
    ) -> Result<Self, String> {
        Self::connect_cdp_inner(url, false, headers).await
    }

    async fn connect_cdp_inner(
        url: &str,
        direct_page: bool,
        headers: Option<Vec<(String, String)>>,
    ) -> Result<Self, String> {
        let ws_url = resolve_cdp_url(url).await?;
        // Record the transport so a reappearing "Allow remote debugging?" modal
        // can be traced to a raw-port attach vs the consent-free relay (#31).
        crate::connect::log_connect_mode(
            &ws_url,
            false,
            DAEMON_SESSION
                .get()
                .map(String::as_str)
                .unwrap_or("default"),
        );
        let client = Arc::new(CdpClient::connect_with_headers(&ws_url, headers).await?);
        let mut manager = Self {
            client,
            browser_process: None,
            ws_url,
            pages: Vec::new(),
            active_page_index: 0,
            default_timeout_ms: 25_000,
            download_path: None,
            ignore_https_errors: false,
            visited_origins: HashSet::new(),
            created_targets: HashSet::new(),
            active_target_id: None,
            relay_target_misses: HashMap::new(),
            relay_scoped: false,
            next_tab_id: 1,
            capture_console: console_capture_enabled(),
        };

        if direct_page {
            let tab_id = manager.assign_tab_id();
            manager.pages.push(PageInfo {
                tab_id,
                label: None,
                target_id: "provider-page".to_string(),
                session_id: String::new(),
                url: String::new(),
                title: String::new(),
                target_type: "page".to_string(),
            });
            manager.active_page_index = 0;
            manager.pin_active_target();
            manager.enable_domains_direct().await?;
        } else {
            manager.discover_and_attach_targets().await?;
        }
        Ok(manager)
    }

    pub async fn connect_auto() -> Result<Self, String> {
        let ws_url = auto_connect_cdp().await?;
        Self::connect_cdp(&ws_url).await
    }

    /// Page targets to adopt, merging several `Target.getTargets` snapshots over
    /// the extension relay. A single relay snapshot is flaky on a busy real Chrome
    /// — it can omit live tabs (a different window's set, or a partial list; issue
    /// #31) — so a tab the daemon should adopt would silently vanish (e.g. after a
    /// daemon restart the page being driven disappeared from the tab list). Taking
    /// the union of a few snapshots makes adoption resilient to a transient miss.
    /// Off the relay (a browser we launched) one snapshot is authoritative.
    async fn collect_page_targets(&self) -> Result<Vec<TargetInfo>, String> {
        let rounds = if crate::connect::relay_url().is_some() {
            3
        } else {
            1
        };
        let mut by_id: HashMap<String, TargetInfo> = HashMap::new();
        let mut any_ok = false;
        for i in 0..rounds {
            if i > 0 {
                tokio::time::sleep(Duration::from_millis(150)).await;
            }
            match self
                .client
                .send_command_typed::<_, GetTargetsResult>("Target.getTargets", &json!({}), None)
                .await
            {
                Ok(result) => {
                    any_ok = true;
                    for t in result.target_infos.into_iter().filter(should_track_target) {
                        by_id.entry(t.target_id.clone()).or_insert(t);
                    }
                }
                Err(e) if i == rounds - 1 && !any_ok => return Err(e),
                Err(_) => {}
            }
        }
        Ok(by_id.into_values().collect())
    }

    /// Every tab the relay knows, UNSCOPED (ignores group scoping) — for explicit
    /// cross-group adoption (`chrome-use adopt`). Falls back to the scoped
    /// `collect_page_targets` on a relay/browser that doesn't support the
    /// unscoped query. Retries a few times over the relay (discovery is eventual).
    async fn collect_all_targets(&self) -> Result<Vec<TargetInfo>, String> {
        let rounds = if crate::connect::relay_url().is_some() {
            3
        } else {
            1
        };
        let mut by_id: HashMap<String, TargetInfo> = HashMap::new();
        let mut any_ok = false;
        for i in 0..rounds {
            if i > 0 {
                tokio::time::sleep(Duration::from_millis(150)).await;
            }
            if let Ok(result) = self
                .client
                .send_command_typed::<_, GetTargetsResult>(
                    "ABRelay.getAllTargets",
                    &json!({}),
                    None,
                )
                .await
            {
                any_ok = true;
                for t in result.target_infos.into_iter().filter(should_track_target) {
                    by_id.entry(t.target_id.clone()).or_insert(t);
                }
            }
        }
        if any_ok {
            Ok(by_id.into_values().collect())
        } else {
            // Older relay without ABRelay.getAllTargets → best-effort scoped list.
            self.collect_page_targets().await
        }
    }

    /// Adopt a specific pre-existing tab matched by `spec` (an exact CDP
    /// `targetId`, or a case-insensitive substring of the tab URL) WITHOUT opening
    /// a new tab — for `chrome-use adopt`. Attaches it (the relay tags it into our
    /// group), tracks + pins it. Errors if nothing matches (never creates a tab).
    async fn adopt_existing_target(&mut self, spec: &str) -> Result<(), String> {
        let all = self.collect_all_targets().await?;
        let spec_l = spec.to_lowercase();
        let target = all
            .iter()
            .find(|t| t.target_id == spec)
            .or_else(|| all.iter().find(|t| t.url.to_lowercase().contains(&spec_l)))
            .ok_or_else(|| {
                let mut open: Vec<String> = all
                    .iter()
                    .map(|t| {
                        let u = if t.url.len() > 80 {
                            &t.url[..80]
                        } else {
                            &t.url
                        };
                        u.to_string()
                    })
                    .collect();
                open.sort();
                open.dedup();
                format!(
                    "adopt: no open tab matching `{spec}` (by targetId or URL substring).\n\
                     {} tab(s) the extension can see:\n  {}",
                    open.len(),
                    open.join("\n  ")
                )
            })?
            .clone();

        let attach: AttachToTargetResult = self
            .client
            .send_command_typed(
                "Target.attachToTarget",
                &AttachToTargetParams {
                    target_id: target.target_id.clone(),
                    flatten: true,
                },
                None,
            )
            .await?;
        let tab_id = self.assign_tab_id();
        self.pages.push(PageInfo {
            tab_id,
            label: None,
            target_id: target.target_id.clone(),
            session_id: attach.session_id.clone(),
            url: target.url.clone(),
            title: sanitize_title(&target.title),
            target_type: target.target_type.clone(),
        });
        self.active_page_index = self.pages.len() - 1;
        self.pin_active_target();
        self.enable_domains(&attach.session_id).await?;
        Ok(())
    }

    async fn discover_and_attach_targets(&mut self) -> Result<(), String> {
        self.client
            .send_command_typed::<_, Value>(
                "Target.setDiscoverTargets",
                &SetDiscoverTargetsParams { discover: true },
                None,
            )
            .await?;

        // Announce our group FIRST so the relay scopes the getTargets below to our
        // own tab group (issue #40). On a launched browser this is a no-op.
        let scoped = self.announce_group().await;

        // `chrome-use adopt <spec>`: adopt a specific PRE-EXISTING tab instead of
        // creating one — true zero-new-tab reading of the user's own tab. The
        // directive rides in via env so it takes effect at first connect (before
        // any about:blank would be made). If nothing matches, error out rather
        // than fall back to creating a tab.
        if let Ok(spec) = std::env::var("AGENT_BROWSER_ADOPT") {
            if !spec.trim().is_empty() {
                return self.adopt_existing_target(spec.trim()).await;
            }
        }

        let page_targets: Vec<TargetInfo> = self.collect_page_targets().await?;

        if page_targets.is_empty() {
            // Create a new tab
            let agent_group = self.agent_group();
            let result: CreateTargetResult = self
                .client
                .send_command_typed(
                    "Target.createTarget",
                    &CreateTargetParams {
                        url: "about:blank".to_string(),
                        agent_group,
                        background: None,
                    },
                    None,
                )
                .await?;
            // We created this tab — own it so close() can clean it up.
            self.created_targets.insert(result.target_id.clone());

            let attach_result: AttachToTargetResult = self
                .client
                .send_command_typed(
                    "Target.attachToTarget",
                    &AttachToTargetParams {
                        target_id: result.target_id.clone(),
                        flatten: true,
                    },
                    None,
                )
                .await?;

            let tab_id = self.next_tab_id;
            self.next_tab_id += 1;
            self.pages.push(PageInfo {
                tab_id,
                label: None,
                target_id: result.target_id,
                session_id: attach_result.session_id.clone(),
                url: "about:blank".to_string(),
                title: String::new(),
                target_type: "page".to_string(),
            });
            self.active_page_index = 0;
            self.pin_active_target();
            self.enable_domains(&attach_result.session_id).await?;
        } else if self.agent_group().is_some() && !scoped {
            // STRICT MULTI-AGENT ISOLATION fallback (relay, but the group announce
            // didn't take — e.g. an older relay). Without relay-side scoping,
            // `page_targets` could be the USER's and OTHER agents' tabs, so this
            // session must NOT adopt any of them — adopting foreign tabs is what let
            // another agent's tab churn drop the tab we were driving (multi-agent
            // failure). Open our own dedicated background tab and pin it instead.
            self.tab_new(None, None).await?;
        } else {
            // Either a browser WE launched (every tab is ours) or the relay has
            // scoped getTargets to our own tab group (#40) — so `page_targets` are
            // all ours: adopt them (this restores follow-popup + cross-session
            // adopt under isolation, since foreign tabs were already filtered out).
            for target in &page_targets {
                let attach_result: AttachToTargetResult = self
                    .client
                    .send_command_typed(
                        "Target.attachToTarget",
                        &AttachToTargetParams {
                            target_id: target.target_id.clone(),
                            flatten: true,
                        },
                        None,
                    )
                    .await?;

                let tab_id = self.next_tab_id;
                self.next_tab_id += 1;
                self.pages.push(PageInfo {
                    tab_id,
                    label: None,
                    target_id: target.target_id.clone(),
                    session_id: attach_result.session_id.clone(),
                    url: target.url.clone(),
                    title: sanitize_title(&target.title),
                    target_type: target.target_type.clone(),
                });
            }
            self.active_page_index = 0;
            self.pin_active_target();
            let session_id = self.pages[0].session_id.clone();
            self.enable_domains(&session_id).await?;
        }

        Ok(())
    }

    pub async fn enable_domains_pub(&self, session_id: &str) -> Result<(), String> {
        self.enable_domains(session_id).await
    }

    async fn enable_domains(&self, session_id: &str) -> Result<(), String> {
        self.client
            .send_command_no_params("Page.enable", Some(session_id))
            .await?;
        // `Runtime.enable` leaves a detectable CDP signal (the patchright/rebrowser
        // "runtime leak"), so only enable it when console/error capture is opted in.
        // `Runtime.evaluate` / `Runtime.callFunctionOn` work fine without it.
        if self.capture_console {
            self.client
                .send_command_no_params("Runtime.enable", Some(session_id))
                .await?;
        }
        // Resume the target if it is paused waiting for the debugger.
        // This is needed for real browser sessions (Chrome 144+) where targets
        // are paused after attach until explicitly resumed. No-op otherwise.
        let _ = self
            .client
            .send_command_no_params("Runtime.runIfWaitingForDebugger", Some(session_id))
            .await;
        self.client
            .send_command_no_params("Network.enable", Some(session_id))
            .await?;
        // Enable auto-attach for cross-origin iframe support.
        // flatten: true gives each iframe its own session_id.
        // Ignored on engines that don't support it (e.g. Lightpanda).
        let _ = self
            .client
            .send_command(
                "Target.setAutoAttach",
                Some(json!({
                    "autoAttach": true,
                    "waitForDebuggerOnStart": false,
                    "flatten": true
                })),
                Some(session_id),
            )
            .await;
        // Silent operation: agent tabs are driven in the background (we never
        // force them to the foreground), so emulate focus. Without this a
        // backgrounded tab is render-throttled and reports `document.hidden` /
        // `!document.hasFocus()` — which both breaks timing-sensitive pages and
        // is itself a bot signal (a real user looks at the page). Best-effort;
        // ignored on engines without Emulation support.
        let _ = self
            .client
            .send_command(
                "Emulation.setFocusEmulationEnabled",
                Some(json!({ "enabled": true })),
                Some(session_id),
            )
            .await;
        Ok(())
    }

    /// Enable domains on a direct page connection (no session_id needed).
    async fn enable_domains_direct(&self) -> Result<(), String> {
        self.client
            .send_command_no_params("Page.enable", None)
            .await?;
        // See `enable_domains`: `Runtime.enable` is a CDP fingerprint, gated on opt-in.
        if self.capture_console {
            self.client
                .send_command_no_params("Runtime.enable", None)
                .await?;
        }
        let _ = self
            .client
            .send_command_no_params("Runtime.runIfWaitingForDebugger", None)
            .await;
        self.client
            .send_command_no_params("Network.enable", None)
            .await?;
        Ok(())
    }

    /// Index of the session's active page, resolved through the pinned
    /// `active_target_id` (stable across reorder/removal/passive discovery) and
    /// falling back to `active_page_index` when nothing is pinned or the pin is
    /// gone. This is what keeps commands on the tab the agent actually opened.
    fn resolved_active_index(&self) -> usize {
        resolve_active_index(
            &self.pages,
            self.active_target_id.as_deref(),
            self.active_page_index,
        )
    }

    /// Whether the resolved active page is a tab THIS session created (via
    /// `Target.createTarget` — `tab new`, `ensure_page`, or the first `open`).
    /// On the shared real browser a fresh session also passively attaches to the
    /// user's existing tabs; those are NOT owned, and navigating one would
    /// clobber the user's page. Used to gate `navigate` on the relay.
    fn active_is_session_owned(&self) -> bool {
        active_index_is_owned(
            &self.pages,
            self.active_target_id.as_deref(),
            self.active_page_index,
            &self.created_targets,
        )
    }

    /// Drop the page bound to `session_id` from the tracked list — used when the
    /// relay reports its tab is gone (issue #35) so the stale entry can't keep
    /// resolving as active. Forgets ownership, unpins it if it was pinned, and
    /// keeps `active_page_index` in range.
    fn drop_page_by_session(&mut self, session_id: &str) {
        let Some(pos) = self.pages.iter().position(|p| p.session_id == session_id) else {
            return;
        };
        let target_id = self.pages[pos].target_id.clone();
        self.pages.remove(pos);
        self.created_targets.remove(&target_id);
        if self.active_target_id.as_deref() == Some(target_id.as_str()) {
            self.active_target_id = None;
        }
        self.active_page_index =
            active_page_index_after_removal(self.active_page_index, pos, self.pages.len());
    }

    /// Pin the current active page by target_id so later commands stick to it.
    /// Call after any explicit open / tab new / tab switch.
    fn pin_active_target(&mut self) {
        self.active_target_id = self
            .pages
            .get(self.active_page_index)
            .map(|p| p.target_id.clone());
    }

    pub fn active_session_id(&self) -> Result<&str, String> {
        let idx = strict_session_index(
            &self.pages,
            self.active_target_id.as_deref(),
            self.active_page_index,
            self.agent_group().is_some(),
            &self.created_targets,
        )?;
        self.pages
            .get(idx)
            .map(|p| p.session_id.as_str())
            .ok_or_else(|| "No active page".to_string())
    }

    pub async fn navigate(&mut self, url: &str, wait_until: WaitUntil) -> Result<Value, String> {
        // On the shared real browser (extension relay), a fresh session only
        // passively attached to the user's existing tabs — it doesn't own any. The
        // pre-fix code made one of those the active tab, so the first `open` then
        // navigated (clobbered) the user's page: in dogfooding an `open` replaced a
        // half-filled form with the target site. If the active tab isn't one we
        // created, open our own tab in this session's group and navigate THAT, so
        // the user's (and other sessions') tabs are never hijacked. Off the relay
        // (a browser we launched) reusing the active tab is correct, so this is
        // gated on `agent_group()`.
        if self.agent_group().is_some() && !self.active_is_session_owned() {
            self.tab_new(None, None).await?;
        }
        let mut session_id = self.active_session_id()?.to_string();
        let mut lifecycle_rx = self.client.subscribe();

        let nav_result: PageNavigateResult = match self
            .client
            .send_command_typed(
                "Page.navigate",
                &PageNavigateParams {
                    url: url.to_string(),
                    referrer: None,
                },
                Some(&session_id),
            )
            .await
        {
            Ok(r) => r,
            // Auto-reattach when the bound tab is gone (issue #35). On the shared
            // real browser the human can close/swap the agent's tab, and a
            // cross-process nav can destroy the target without a re-attachable
            // tabId — both leave the cached `cb-tab-<id>` session stale, so every
            // command (including `open`) failed on it and only `tab new`
            // recovered. The relay error literally says "re-open your target URL
            // to re-attach"; fulfil that here: drop the dead page, open a fresh
            // owned tab in this session's group, and navigate THAT. Gated on the
            // relay (`agent_group`) and on the explicit navigation intent — read
            // commands deliberately still fail loudly rather than silently
            // recover onto a blank tab and return wrong data (issue #8.1).
            Err(e) if self.agent_group().is_some() && is_stale_target_error(&e) => {
                self.drop_page_by_session(&session_id);
                self.tab_new(None, None).await?;
                session_id = self.active_session_id()?.to_string();
                lifecycle_rx = self.client.subscribe();
                self.client
                    .send_command_typed(
                        "Page.navigate",
                        &PageNavigateParams {
                            url: url.to_string(),
                            referrer: None,
                        },
                        Some(&session_id),
                    )
                    .await?
            }
            Err(e) => return Err(e),
        };

        if let Some(ref error_text) = nav_result.error_text {
            // `data:` URLs abort over the extension relay: chrome.debugger /
            // chrome.tabs can't drive a top-frame data: navigation, so it comes
            // back net::ERR_ABORTED on an about:blank tab. Explain it instead of
            // leaking the cryptic code (data: works fine under `--launch`).
            if url.starts_with("data:") && error_text.contains("ERR_ABORTED") {
                return Err(format!(
                    "Navigation failed: {error_text}. Chrome blocks top-frame `data:` URLs over \
                     the extension relay — use a real http(s):// or file:// URL, or run with \
                     `--launch` (where data: URLs work)."
                ));
            }
            return Err(format!("Navigation failed: {}", error_text));
        }

        // Only wait for lifecycle events if Chrome created a new loader (full navigation).
        // If loader_id is None, it was a same-document navigation (e.g., hash routing)
        // which does not fire Page.loadEventFired or Page.domContentEventFired.
        let mut nav_warning: Option<String> = None;
        if nav_result.loader_id.is_some() && wait_until != WaitUntil::None {
            if let Err(e) = self
                .wait_for_lifecycle(wait_until, &session_id, &mut lifecycle_rx)
                .await
            {
                // The lifecycle event (e.g. `load`) didn't fire within the
                // timeout. On SPAs this is common — a long-pending XHR or a stuck
                // sub-resource holds `load` open long after the DOM is interactive
                // and the page is usable, so `open` would hard-fail even though
                // eval/screenshot work immediately (issue #10). If the DOM is
                // already ready, treat navigation as done (with a warning, carried
                // in the response so the CLI can surface it) instead of failing.
                // Only a still-loading document is a real failure.
                let ready = self
                    .evaluate_simple("document.readyState")
                    .await
                    .ok()
                    .and_then(|v| v.as_str().map(str::to_string))
                    .unwrap_or_default();
                if ready == "interactive" || ready == "complete" {
                    nav_warning = Some(format!(
                        "`{}` didn't complete within the timeout, but the DOM is ready ({}) — \
                         continuing. Pass `--wait-until domcontentloaded` to skip this wait on \
                         SPAs with long-lived requests.",
                        wait_until.as_str(),
                        ready
                    ));
                } else {
                    return Err(e);
                }
            }
        }

        let page_url = self.get_url().await.unwrap_or_else(|_| url.to_string());
        let title = self.get_title().await.unwrap_or_default();

        // Track visited origin for cross-origin localStorage collection in save_state
        if let Ok(parsed) = url::Url::parse(&page_url) {
            let origin = parsed.origin().ascii_serialization();
            if origin != "null" {
                self.visited_origins.insert(origin);
            }
        }

        // An explicit `open`/navigate IS the "explicit open" the pin invariant is
        // built around (see `active_target_id`). On the relay path `open` reuses an
        // existing tab via this method rather than `add_page`, so without pinning
        // here `active_target_id` stayed `None` and the session rode the fragile
        // `active_page_index` — a later passive tab close/reorder then drifted
        // `eval`/`get url`/`snapshot` onto a foreign tab between commands (issue
        // #14). Sync the index to the resolved active page, then pin it by stable
        // target_id so subsequent commands stick to the tab we just navigated.
        self.active_page_index = self.resolved_active_index();
        if let Some(page) = self.pages.get_mut(self.active_page_index) {
            page.url = page_url.clone();
            page.title = sanitize_title(&title);
        }
        self.pin_active_target();

        // `navigate` (unlike `tab new`) reaches here after opening a fresh owned
        // tab when the active tab wasn't session-owned — which strands the
        // daemon's about:blank scratch. Sweep it now that a real page is open,
        // keeping the tab we just navigated. (relay-only; gated inside helper)
        if page_url != "about:blank" {
            if let Some(keep) = self
                .pages
                .get(self.active_page_index)
                .map(|p| p.target_id.clone())
            {
                self.close_leftover_blank_scratch(&keep).await;
            }
        }

        let mut out = json!({ "url": page_url, "title": title });
        if let Some(w) = nav_warning {
            out["warning"] = json!(w);
        }
        Ok(out)
    }

    async fn wait_for_lifecycle(
        &self,
        wait_until: WaitUntil,
        session_id: &str,
        rx: &mut broadcast::Receiver<CdpEvent>,
    ) -> Result<(), String> {
        let event_name = match wait_until {
            WaitUntil::Load => "Page.loadEventFired",
            WaitUntil::DomContentLoaded => "Page.domContentEventFired",
            WaitUntil::NetworkIdle => return self.wait_for_network_idle(session_id, rx).await,
            WaitUntil::None => return Ok(()),
        };

        let timeout = tokio::time::Duration::from_millis(self.default_timeout_ms);

        tokio::time::timeout(timeout, async {
            loop {
                match rx.recv().await {
                    Ok(event) => {
                        if event.method == event_name
                            && event.session_id.as_deref() == Some(session_id)
                        {
                            return Ok(());
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                }
            }
            Err("Event stream closed".to_string())
        })
        .await
        .map_err(|_| format!("Timeout waiting for {}", event_name))?
    }

    async fn wait_for_network_idle(
        &self,
        session_id: &str,
        rx: &mut broadcast::Receiver<CdpEvent>,
    ) -> Result<(), String> {
        let timeout = tokio::time::Duration::from_millis(self.default_timeout_ms);
        poll_network_idle(session_id, rx, timeout).await
    }

    pub async fn get_url(&self) -> Result<String, String> {
        let result = self.evaluate_simple("location.href").await?;
        Ok(result.as_str().unwrap_or("").to_string())
    }

    pub async fn get_title(&self) -> Result<String, String> {
        let result = self.evaluate_simple("document.title").await?;
        Ok(sanitize_title(result.as_str().unwrap_or("")))
    }

    pub async fn get_content(&self) -> Result<String, String> {
        let result = self
            .evaluate_simple("document.documentElement.outerHTML")
            .await?;
        Ok(result.as_str().unwrap_or("").to_string())
    }

    pub async fn evaluate(&self, script: &str, _args: Option<Value>) -> Result<Value, String> {
        let session_id = self.active_session_id()?.to_string();

        // `replMode: true` lets successive `eval`s re-declare top-level
        // `let`/`const` instead of throwing "Identifier 'x' has already been
        // declared" (issue #38 — independent `eval` steps in a test suite collided
        // in the page's shared lexical scope). BUT replMode and `awaitPromise` are
        // mutually exclusive in Chrome: under replMode a returned promise is NOT
        // awaited (it serialises to `{}`), which breaks `fetch(...).then(...)` and
        // every other async eval. So enable replMode ONLY for synchronous scripts
        // that declare a top-level `let`/`const`; promise-returning scripts keep
        // `awaitPromise` (no replMode) — exactly the pre-#38 behaviour.
        let mentions_async = script.contains("await")
            || script.contains(".then(")
            || script.contains("fetch(")
            || script.contains("Promise");
        let declares = script.contains("let ") || script.contains("const ");
        let repl_mode = declares && !mentions_async;
        let result: EvaluateResult = self
            .client
            .send_command_typed(
                "Runtime.evaluate",
                &json!({
                    "expression": script,
                    "returnByValue": true,
                    "awaitPromise": !repl_mode,
                    "replMode": repl_mode,
                }),
                Some(&session_id),
            )
            .await?;

        if let Some(ref details) = result.exception_details {
            let msg = details
                .exception
                .as_ref()
                .and_then(|e| e.description.as_deref())
                .unwrap_or(&details.text);
            return Err(format!("Evaluation error: {}", msg));
        }

        Ok(result.result.value.unwrap_or(Value::Null))
    }

    async fn evaluate_simple(&self, expression: &str) -> Result<Value, String> {
        self.evaluate(expression, None).await
    }

    pub async fn wait_for_lifecycle_external(
        &self,
        wait_until: WaitUntil,
        session_id: &str,
    ) -> Result<(), String> {
        let mut rx = self.client.subscribe();
        self.wait_for_lifecycle(wait_until, session_id, &mut rx)
            .await
    }

    pub async fn close(&mut self) -> Result<(), String> {
        if self.browser_process.is_some() {
            // Only send Browser.close when we launched the browser ourselves.
            // For external connections (--auto-connect, --cdp) we just disconnect
            // without shutting down the user's browser.
            let _ = self
                .client
                .send_command_no_params("Browser.close", None)
                .await;
        } else {
            // Connected to the user's real Chrome: we must NOT close their
            // browser, but we DO own the tabs this session created. Close them so
            // they don't pile up in the user's window (in their per-session tab
            // group) every time a session ends, idles out, or the daemon shuts
            // down. `created_targets` only holds tabs we made via
            // Target.createTarget — never the user's existing tabs or other
            // sessions' — so this is always safe. Best-effort per tab.
            for target_id in self.created_targets.drain() {
                let _ = self
                    .client
                    .send_command_typed::<_, Value>(
                        "Target.closeTarget",
                        &CloseTargetParams { target_id },
                        None,
                    )
                    .await;
            }
        }

        if let Some(mut process) = self.browser_process.take() {
            let timeout = std::time::Duration::from_secs(5);
            let _ = tokio::task::spawn_blocking(move || {
                process.wait_or_kill(timeout);
            })
            .await;
        }

        Ok(())
    }

    pub fn has_pages(&self) -> bool {
        !self.pages.is_empty()
    }

    pub fn default_timeout_ms(&self) -> u64 {
        self.default_timeout_ms
    }

    /// Checks if the CDP connection is alive by sending a `Browser.getVersion`
    /// probe. See [`connection_alive_from_probe`] for how the outcome maps to a
    /// liveness verdict — in particular why a timeout does NOT tear down an
    /// externally-attached browser.
    pub async fn is_connection_alive(&self) -> bool {
        let timeout = tokio::time::Duration::from_secs(3);
        let probe = match tokio::time::timeout(
            timeout,
            self.client
                .send_command_no_params("Browser.getVersion", None),
        )
        .await
        {
            Ok(Ok(_)) => LivenessProbe::Responded,
            Ok(Err(_)) => LivenessProbe::TransportError,
            Err(_) => LivenessProbe::TimedOut,
        };
        // No child process => we attached to an external browser (the user's
        // real Chrome — the stealth fork's default).
        let is_external_attach = self.browser_process.is_none();
        connection_alive_from_probe(probe, is_external_attach)
    }

    /// Non-blocking check whether the locally-launched browser process has exited
    /// (crashed or terminated). Also reaps the zombie if it has exited.
    /// Returns false for external CDP connections (no child process to monitor).
    pub fn has_process_exited(&mut self) -> bool {
        if let Some(ref mut process) = self.browser_process {
            process.has_exited()
        } else {
            false
        }
    }

    pub fn get_cdp_url(&self) -> &str {
        &self.ws_url
    }

    /// Returns the Chrome debug server address as "host:port".
    pub fn chrome_host_port(&self) -> &str {
        let stripped = self
            .ws_url
            .strip_prefix("ws://")
            .or_else(|| self.ws_url.strip_prefix("wss://"))
            .unwrap_or(&self.ws_url);
        stripped.split('/').next().unwrap_or(stripped)
    }

    pub fn active_target_id(&self) -> Result<&str, String> {
        self.pages
            .get(self.resolved_active_index())
            .map(|p| p.target_id.as_str())
            .ok_or_else(|| "No active page".to_string())
    }

    /// Stop owning a tab — drop it from `created_targets` so it survives `close()`
    /// and idle-shutdown (the agent is leaving it for the user). Returns true if it
    /// was owned. Used by `keep`.
    pub fn unown_target(&mut self, target_id: &str) -> bool {
        self.created_targets.remove(target_id)
    }

    /// Returns true if this manager was connected via CDP (as opposed to local launch).
    pub fn is_cdp_connection(&self) -> bool {
        self.browser_process.is_none()
    }

    /// Ensures the browser has at least one page. If `pages` is empty, creates a new
    /// about:blank page and attaches to it.
    pub async fn ensure_page(&mut self) -> Result<(), String> {
        if !self.pages.is_empty() {
            return Ok(());
        }

        let agent_group = self.agent_group();
        let result: CreateTargetResult = self
            .client
            .send_command_typed(
                "Target.createTarget",
                &CreateTargetParams {
                    url: "about:blank".to_string(),
                    agent_group,
                    background: None,
                },
                None,
            )
            .await?;
        // We created this tab — own it so close() can clean it up.
        self.created_targets.insert(result.target_id.clone());

        let attach_result: AttachToTargetResult = self
            .client
            .send_command_typed(
                "Target.attachToTarget",
                &AttachToTargetParams {
                    target_id: result.target_id.clone(),
                    flatten: true,
                },
                None,
            )
            .await?;

        let tab_id = self.next_tab_id;
        self.next_tab_id += 1;
        self.pages.push(PageInfo {
            tab_id,
            label: None,
            target_id: result.target_id,
            session_id: attach_result.session_id.clone(),
            url: "about:blank".to_string(),
            title: String::new(),
            target_type: "page".to_string(),
        });
        self.active_page_index = 0;
        // Pin this freshly-created tab (matches `add_page`) so it's a stable
        // anchor from the first command, not a bare index (issue #14).
        self.pin_active_target();
        self.enable_domains(&attach_result.session_id).await?;

        Ok(())
    }

    // -----------------------------------------------------------------------
    // Tab management
    // -----------------------------------------------------------------------

    /// Checks if `active_page_index` is still valid and adjusts it if not
    /// (e.g., after a tab was closed).
    pub fn update_active_page_if_needed(&mut self) {
        if self.pages.is_empty() {
            self.active_page_index = 0;
            return;
        }
        if self.active_page_index >= self.pages.len() {
            self.active_page_index = self.pages.len() - 1;
        }
    }

    fn update_active_page_after_removal(&mut self, removed_index: usize) {
        self.active_page_index = active_page_index_after_removal(
            self.active_page_index,
            removed_index,
            self.pages.len(),
        );
    }

    pub fn tab_list(&self) -> Vec<Value> {
        let active = self.resolved_active_index();
        self.pages
            .iter()
            .enumerate()
            .map(|(i, p)| {
                json!({
                    "tabId": format_tab_id(p.tab_id),
                    // Stable CDP target id. Unlike `t<N>` (per-session, reassigned
                    // each connect) this is the same handle across every session
                    // attached to the relayed Chrome, so it's how you adopt a
                    // specific pre-existing tab from another session (issue #21).
                    "targetId": p.target_id,
                    "label": p.label,
                    "title": p.title,
                    "url": p.url,
                    "type": p.target_type,
                    "active": i == active,
                })
            })
            .collect()
    }

    /// The active tab's stable handle + current location, for `chrome-use
    /// current` (#26). `targetId` survives cross-process navigation, so it's the
    /// handle an agent should hold across a multi-step flow.
    pub fn active_page_info(&self) -> Option<Value> {
        let i = self.resolved_active_index();
        self.pages.get(i).map(|p| {
            json!({
                "tabId": format_tab_id(p.tab_id),
                "targetId": p.target_id,
                "label": p.label,
                "url": p.url,
                "title": p.title,
            })
        })
    }

    /// Stable `tab_id` for a page identified by its CDP `targetId`, if tracked.
    /// Lets callers adopt a tab by the cross-session-stable target id.
    pub fn tab_id_for_target(&self, target_id: &str) -> Option<u32> {
        self.pages
            .iter()
            .find(|p| p.target_id == target_id)
            .map(|p| p.tab_id)
    }

    /// Re-pull the live target set and reconcile `self.pages`: adopt tabs that
    /// appeared since connect (another session's tab, or one that just
    /// re-attached after a cross-process nav), refresh url/title on known tabs,
    /// and drop tabs that are gone (clearing phantom rows). Never steals focus —
    /// the active tab is preserved, and re-pinned if it was pruned. Powers a live
    /// `tab list` and adopt-by-targetId so a fresh session can reach a stranded,
    /// still-filled tab without reloading it (issue #21).
    /// Detect targets that appeared since the `before` set (e.g. a click that
    /// opened a new tab via a `target=_blank` link or `window.open`), attach +
    /// track each in the background, and return the first newly-opened page.
    ///
    /// Lighter than [`resync_targets`] — one `getTargets` and work only on the
    /// new targets, no whole-tab url/title refresh — so it's cheap enough to run
    /// after every click. The new tab is added in the background (never steals
    /// the active tab, per #7/#8.1); the caller surfaces it so the agent knows a
    /// tab opened instead of seeing the old page (issue #24-A).
    pub async fn adopt_newly_opened(&mut self, before: &HashSet<String>) -> Option<PageInfo> {
        // STRICT MULTI-AGENT ISOLATION: on the relay this session's `before` set is
        // only its OWN tabs, so EVERY foreign tab (the user's, other agents') looks
        // "new" relative to it and would be adopted here — exactly the leak where a
        // concurrent agent's tabs (github/Lark/iphone-use) showed up in this
        // session mid-flow. A tab the agent itself opened (a pop-up) can't be
        // distinguished from a foreign tab over the relay (no opener/window/group
        // in the synthesized targetInfo), so don't adopt anything: the agent drives
        // only tabs it explicitly created, and pop-ups (e.g. an OAuth/login window)
        // are the user's. A launched browser (every tab ours) still follows pop-ups.
        // Strict isolation only when on the relay WITHOUT group scoping: there a
        // pop-up can't be told apart from a foreign tab, so adopt nothing. When the
        // relay IS scoping (#40), getTargets returns only our group, so a tab that
        // appeared after our own action is genuinely ours (a pop-up) — adopt it.
        if self.agent_group().is_some() && !self.relay_scoped {
            return None;
        }
        let result: GetTargetsResult = self
            .client
            .send_command_typed("Target.getTargets", &json!({}), None)
            .await
            .ok()?;
        let live: Vec<TargetInfo> = result
            .target_infos
            .into_iter()
            .filter(should_track_target)
            .collect();
        let mut opened: Option<PageInfo> = None;
        for target in &live {
            if before.contains(&target.target_id)
                || self.pages.iter().any(|p| p.target_id == target.target_id)
            {
                continue;
            }
            let attach: AttachToTargetResult = match self
                .client
                .send_command_typed(
                    "Target.attachToTarget",
                    &AttachToTargetParams {
                        target_id: target.target_id.clone(),
                        flatten: true,
                    },
                    None,
                )
                .await
            {
                Ok(r) => r,
                Err(_) => continue,
            };
            let tab_id = self.assign_tab_id();
            let page = PageInfo {
                tab_id,
                label: None,
                target_id: target.target_id.clone(),
                session_id: attach.session_id.clone(),
                url: target.url.clone(),
                title: sanitize_title(&target.title),
                target_type: target.target_type.clone(),
            };
            // A tab that appeared right after THIS session's action (a click that
            // opened a popup/new tab) is ours — record it as owned so it's tracked,
            // protected from churn-pruning, and cleaned up on close, consistent with
            // strict multi-agent isolation (we only ever own tabs we created/opened).
            self.created_targets.insert(target.target_id.clone());
            self.add_background_page(page.clone());
            let _ = self.enable_domains(&attach.session_id).await;
            if opened.is_none() {
                opened = Some(page);
            }
        }
        opened
    }

    pub async fn resync_targets(&mut self) -> Result<(), String> {
        self.client
            .send_command_typed::<_, Value>(
                "Target.setDiscoverTargets",
                &SetDiscoverTargetsParams { discover: true },
                None,
            )
            .await?;
        let result: GetTargetsResult = self
            .client
            .send_command_typed("Target.getTargets", &json!({}), None)
            .await?;
        let live: Vec<TargetInfo> = result
            .target_infos
            .into_iter()
            .filter(should_track_target)
            .collect();
        let live_ids: HashSet<String> = live.iter().map(|t| t.target_id.clone()).collect();
        let on_relay = self.agent_group().is_some();
        // When the relay scopes getTargets to our group (#40), `live` is already
        // only our own tabs, so adopting unknown ones is safe (a freshly-opened
        // pop-up). Without scoping, keep strict isolation: never adopt a tab we
        // didn't create — it belongs to the user or another agent.
        let strict_isolation = on_relay && !self.relay_scoped;

        for target in &live {
            if self.update_page_target_info(target) {
                continue;
            }
            if strict_isolation {
                continue;
            }
            let attach_result: AttachToTargetResult = match self
                .client
                .send_command_typed(
                    "Target.attachToTarget",
                    &AttachToTargetParams {
                        target_id: target.target_id.clone(),
                        flatten: true,
                    },
                    None,
                )
                .await
            {
                Ok(r) => r,
                // The tab may have closed between getTargets and attach, or be a
                // restricted page — skip it rather than failing the whole resync.
                Err(_) => continue,
            };
            let tab_id = self.assign_tab_id();
            self.add_background_page(PageInfo {
                tab_id,
                label: None,
                target_id: target.target_id.clone(),
                session_id: attach_result.session_id.clone(),
                url: target.url.clone(),
                title: sanitize_title(&target.title),
                target_type: target.target_type.clone(),
            });
            let _ = self.enable_domains(&attach_result.session_id).await;
        }

        // Prune tabs that are gone. On a LAUNCHED browser a missing target really
        // is closed, so prune immediately. On the RELAY a single `getTargets`
        // snapshot routinely omits live tabs (multi-agent churn, a brief
        // cross-process-nav gap) — dropping the tab we're driving on one bad
        // snapshot is the failure we're fixing — so prune only after the tab has
        // been absent for several CONSECUTIVE snapshots (debounced). The pinned
        // active target is protected either way (issue #31).
        let gone = if on_relay {
            debounced_prune_ids(
                &self.pages,
                &live_ids,
                self.active_target_id.as_deref(),
                &mut self.relay_target_misses,
            )
        } else {
            prunable_target_ids(&self.pages, &live_ids, self.active_target_id.as_deref())
        };
        for tid in &gone {
            self.relay_target_misses.remove(tid);
            self.remove_page_by_target_id(tid);
        }

        // Refresh url/title from each live tab. The relay only stamps target_info
        // on attach, so after a navigation its cached url/title go stale (or stay
        // blank for a tab attached at about:blank) — which made `tab list` show
        // blank rows you couldn't tell apart, defeating the point of listing them
        // to pick a tab to adopt (issue #21). `Target.getTargetInfo` is a plain
        // CDP read (no Runtime fingerprint), one cheap call per tab.
        let sessions: Vec<(usize, String)> = self
            .pages
            .iter()
            .enumerate()
            .map(|(i, p)| (i, p.session_id.clone()))
            .collect();
        for (i, sid) in sessions {
            if sid.is_empty() {
                continue;
            }
            if let Ok(resp) = self
                .client
                .send_command("Target.getTargetInfo", None, Some(&sid))
                .await
            {
                if let Some(ti) = resp.get("targetInfo") {
                    if let Some(page) = self.pages.get_mut(i) {
                        if let Some(u) = ti.get("url").and_then(|v| v.as_str()) {
                            if !u.is_empty() {
                                page.url = u.to_string();
                            }
                        }
                        if let Some(t) = ti.get("title").and_then(|v| v.as_str()) {
                            page.title = sanitize_title(t);
                        }
                    }
                }
            }
        }
        Ok(())
    }

    /// If `--reuse-tab` and a tracked tab already shows `url`, switch to it
    /// (without reloading, so any in-page state survives) and return its info.
    /// Returns `None` when no tab matches and the caller should navigate/create.
    /// Matches on exact URL or the same origin+path (ignoring query/fragment) so
    /// a re-`open` of a stable entry URL lands on the existing tab instead of
    /// piling up duplicates (issue #21).
    pub async fn reuse_tab_for_url(&mut self, url: &str) -> Result<Option<Value>, String> {
        self.resync_targets().await.ok();
        let want = normalize_url_for_match(url);
        let tab_id = self
            .pages
            .iter()
            .find(|p| !want.is_empty() && (p.url == url || normalize_url_for_match(&p.url) == want))
            .map(|p| p.tab_id);
        match tab_id {
            Some(id) => Ok(Some(self.tab_switch_by_id(id).await?)),
            None => Ok(None),
        }
    }

    /// Resolve a user-supplied `TabRef` (either `t<N>` or a label) to the
    /// stable numeric `tab_id`. Returns a teaching error for unknown tabs.
    pub fn resolve_tab_ref(&self, tab_ref: &TabRef) -> Result<u32, String> {
        match tab_ref {
            TabRef::Id(id) => {
                if self.has_tab_id(*id) {
                    Ok(*id)
                } else {
                    Err(format!(
                        "Tab {} not found; run `chrome-use tab` to list open tabs",
                        format_tab_id(*id)
                    ))
                }
            }
            TabRef::Label(name) => self
                .pages
                .iter()
                .find(|p| p.label.as_deref() == Some(name.as_str()))
                .map(|p| p.tab_id)
                .ok_or_else(|| {
                    format!(
                        "No tab with label `{}`; run `chrome-use tab` to list open tabs",
                        name
                    )
                }),
        }
    }

    /// Returns true iff a tab already carries the given label.
    pub fn has_label(&self, label: &str) -> bool {
        self.pages.iter().any(|p| p.label.as_deref() == Some(label))
    }

    /// Chrome tab-group name for tabs this manager creates, or `None` when not
    /// driving the user's real Chrome via the `ab-connect` extension relay.
    ///
    /// Grouping only makes sense on the shared real browser (one Chrome, many
    /// agents): each session's tabs go into its own group. On a launched / direct
    /// CDP browser the endpoint is strict, so we must NOT send the custom param —
    /// hence `None` there. We detect the relay by matching our `ws_url` against
    /// the live relay URL the native-messaging host published.
    /// Whether this manager is driving the user's real Chrome through the
    /// `ab-connect` extension relay (vs. a browser we launched or a direct CDP
    /// endpoint). Detected by matching our `ws_url` against the live relay URL
    /// the native-messaging host published. Used to avoid relay-unsafe CDP that
    /// would disturb the user's window (e.g. Browser.setContentsSize, issue #47).
    fn via_relay(&self) -> bool {
        crate::connect::relay_url().as_deref() == Some(self.ws_url.as_str())
    }

    fn agent_group(&self) -> Option<String> {
        if !self.via_relay() {
            return None;
        }
        let name = DAEMON_SESSION
            .get()
            .map(String::as_str)
            .unwrap_or("default");
        if name.is_empty() {
            None
        } else {
            Some(name.to_string())
        }
    }

    /// Tell the relay which tab group this session owns so it can scope
    /// `Target.getTargets` to us (issue #40). Only meaningful on the relay; a
    /// no-op (returns false) on a launched/real-CDP connection. Sets and returns
    /// `relay_scoped`: when true, the daemon can trust getTargets to contain only
    /// our group and re-enable adopting new tabs (pop-ups, cross-session adopt).
    async fn announce_group(&mut self) -> bool {
        let Some(group) = self.agent_group() else {
            self.relay_scoped = false;
            return false;
        };
        let ok = self
            .client
            .send_command_typed::<_, Value>("ABRelay.setGroup", &json!({ "group": group }), None)
            .await
            .is_ok();
        self.relay_scoped = ok;
        ok
    }

    /// On the relay, close any OWNED tabs still sitting at about:blank except
    /// `keep` (the tab we just opened or navigated to a real page). The daemon
    /// creates an about:blank scratch tab on connect; once a real page exists
    /// that scratch is just clutter, and `navigate`/`tab new` could otherwise
    /// strand it (e.g. `eval` then `navigate <url>` left a stray about:blank).
    /// Re-pins `keep` afterwards since removing pages shifts indices.
    ///
    /// Relay-only: off the relay the initial about:blank is the browser's own
    /// first tab (not our scratch), so we must never close it.
    async fn close_leftover_blank_scratch(&mut self, keep: &str) {
        if self.agent_group().is_none() {
            return;
        }
        let blanks: Vec<String> = self
            .pages
            .iter()
            .filter(|p| {
                p.target_id != keep
                    && self.created_targets.contains(&p.target_id)
                    && (p.url == "about:blank" || p.url.is_empty())
            })
            .map(|p| p.target_id.clone())
            .collect();
        if blanks.is_empty() {
            return;
        }
        for tid in blanks {
            let _ = self
                .client
                .send_command_typed::<_, Value>(
                    "Target.closeTarget",
                    &CloseTargetParams {
                        target_id: tid.clone(),
                    },
                    None,
                )
                .await;
            self.created_targets.remove(&tid);
            self.remove_page_by_target_id(&tid);
        }
        // Removing earlier pages shifts indices — re-pin the kept tab.
        if let Some(i) = self.pages.iter().position(|p| p.target_id == keep) {
            self.active_page_index = i;
            self.pin_active_target();
        }
    }

    pub async fn tab_new(
        &mut self,
        url: Option<&str>,
        label: Option<&str>,
    ) -> Result<Value, String> {
        if let Some(label) = label {
            if !is_valid_label(label) {
                return Err(format!(
                    "Invalid tab label `{}`; labels must start with a letter and contain only \
                     letters, digits, `-`, and `_`",
                    label
                ));
            }
            if self.has_label(label) {
                return Err(format!(
                    "Label `{}` is already used by another tab; labels must be unique within a \
                     session",
                    label
                ));
            }
        }

        let target_url = url.unwrap_or("about:blank");

        let agent_group = self.agent_group();
        let result: CreateTargetResult = self
            .client
            .send_command_typed(
                "Target.createTarget",
                &CreateTargetParams {
                    url: target_url.to_string(),
                    agent_group,
                    background: Some(true),
                },
                None,
            )
            .await?;
        // We created this tab — own it so close() can clean it up.
        self.created_targets.insert(result.target_id.clone());

        let attach: AttachToTargetResult = self
            .client
            .send_command_typed(
                "Target.attachToTarget",
                &AttachToTargetParams {
                    target_id: result.target_id.clone(),
                    flatten: true,
                },
                None,
            )
            .await?;

        self.enable_domains(&attach.session_id).await?;

        let tab_id = self.next_tab_id;
        self.next_tab_id += 1;
        let index = self.pages.len();
        let label = label.map(|s| s.to_string());
        self.pages.push(PageInfo {
            tab_id,
            label: label.clone(),
            target_id: result.target_id,
            session_id: attach.session_id,
            url: target_url.to_string(),
            title: String::new(),
            target_type: "page".to_string(),
        });
        self.active_page_index = index;
        self.pin_active_target();

        // Once this real tab exists, close the daemon's leftover about:blank
        // scratch so the session's tab group isn't left showing a stray blank
        // page beside the work tab (every group otherwise carried one).
        if target_url != "about:blank" {
            if let Some(new_tid) = self.pages.get(index).map(|p| p.target_id.clone()) {
                self.close_leftover_blank_scratch(&new_tid).await;
            }
        }

        Ok(json!({
            "tabId": format_tab_id(tab_id),
            "label": label,
            "url": target_url,
            "total": self.pages.len(),
        }))
    }

    pub async fn tab_switch(&mut self, index: usize) -> Result<Value, String> {
        if index >= self.pages.len() {
            return Err(format!(
                "Tab index {} out of range (0-{})",
                index,
                self.pages.len().saturating_sub(1)
            ));
        }

        self.active_page_index = index;
        self.pin_active_target();
        let session_id = self.pages[index].session_id.clone();
        self.enable_domains(&session_id).await?;

        // Silent: switching the agent's *internal* active page must not yank the
        // user's foreground tab. The page is driven in the background (focus is
        // emulated in enable_domains); the explicit `bringToFront` command is the
        // only way a tab is deliberately surfaced.

        let url = self.get_url().await.unwrap_or_default();
        let title = self.get_title().await.unwrap_or_default();

        if let Some(page) = self.pages.get_mut(index) {
            page.url = url.clone();
            page.title = sanitize_title(&title);
        }

        let page = &self.pages[index];
        Ok(json!({
            "tabId": format_tab_id(page.tab_id),
            "label": page.label,
            "url": url,
            "title": title,
        }))
    }

    pub async fn tab_close(&mut self, index: Option<usize>) -> Result<Value, String> {
        let target_index = index.unwrap_or(self.active_page_index);

        if target_index >= self.pages.len() {
            return Err(format!("Tab index {} out of range", target_index));
        }

        if self.pages.len() <= 1 {
            return Err("Cannot close the last tab".to_string());
        }

        let page = self.pages.remove(target_index);
        self.update_active_page_after_removal(target_index);
        let closed_tab_id = page.tab_id;
        let closed_label = page.label.clone();
        let _ = self
            .client
            .send_command_typed::<_, Value>(
                "Target.closeTarget",
                &CloseTargetParams {
                    target_id: page.target_id,
                },
                None,
            )
            .await;

        let session_id = self.pages[self.active_page_index].session_id.clone();
        self.enable_domains(&session_id).await?;

        Ok(json!({
            "tabId": format_tab_id(closed_tab_id),
            "label": closed_label,
            "closed": true,
        }))
    }

    // -----------------------------------------------------------------------
    // Emulation
    // -----------------------------------------------------------------------

    pub async fn set_viewport(
        &self,
        width: i32,
        height: i32,
        device_scale_factor: f64,
        mobile: bool,
    ) -> Result<(), String> {
        let session_id = self.active_session_id()?;
        self.client
            .send_command(
                "Emulation.setDeviceMetricsOverride",
                Some(json!({
                    "width": width,
                    "height": height,
                    "deviceScaleFactor": device_scale_factor,
                    "mobile": mobile,
                })),
                Some(session_id),
            )
            .await?;

        // Screencast captures the actual content area, not the emulated CSS
        // viewport, so resize the content area to match — but ONLY for a browser
        // we launched. Over the ab-connect relay the "window" is the user's real
        // Chrome window, and Browser.setContentsSize would physically resize it
        // (issue #47) — the exact thing the CDP device-metrics override exists to
        // avoid. The Emulation override above already gives the tab the requested
        // CSS viewport without touching the OS window, so skip the resize there.
        if !self.via_relay() {
            if let Ok(target_id) = self.active_target_id() {
                if let Ok(window_info) = self
                    .client
                    .send_command(
                        "Browser.getWindowForTarget",
                        Some(json!({ "targetId": target_id })),
                        None,
                    )
                    .await
                {
                    if let Some(window_id) = window_info.get("windowId").and_then(|v| v.as_i64()) {
                        if let Err(e) = self
                            .client
                            .send_command(
                                "Browser.setContentsSize",
                                Some(json!({
                                    "windowId": window_id,
                                    "width": width,
                                    "height": height,
                                })),
                                None,
                            )
                            .await
                        {
                            eprintln!("Browser.setContentsSize failed (experimental CDP): {e}");
                        }
                    }
                }
            }
        }

        Ok(())
    }

    /// Clear the CDP device-metrics override (`viewport reset`), restoring the
    /// tab's real layout viewport. Never touches the OS window, so it is safe on
    /// the relay (we never physically resized the user's window — see
    /// `set_viewport`).
    pub async fn clear_viewport(&self) -> Result<(), String> {
        let session_id = self.active_session_id()?;
        self.client
            .send_command(
                "Emulation.clearDeviceMetricsOverride",
                Some(json!({})),
                Some(session_id),
            )
            .await?;
        Ok(())
    }

    pub async fn set_user_agent(&self, user_agent: &str) -> Result<(), String> {
        let session_id = self.active_session_id()?;
        self.client
            .send_command(
                "Emulation.setUserAgentOverride",
                Some(json!({ "userAgent": user_agent })),
                Some(session_id),
            )
            .await?;
        Ok(())
    }

    pub async fn set_emulated_media(
        &self,
        media: Option<&str>,
        features: Option<Vec<(String, String)>>,
    ) -> Result<(), String> {
        let session_id = self.active_session_id()?;
        let mut params = json!({});
        if let Some(m) = media {
            params["media"] = Value::String(m.to_string());
        }
        if let Some(feats) = features {
            let features_arr: Vec<Value> = feats
                .iter()
                .map(|(name, value)| json!({ "name": name, "value": value }))
                .collect();
            params["features"] = Value::Array(features_arr);
        }
        self.client
            .send_command("Emulation.setEmulatedMedia", Some(params), Some(session_id))
            .await?;
        Ok(())
    }

    pub async fn bring_to_front(&self) -> Result<(), String> {
        let session_id = self.active_session_id()?;
        self.client
            .send_command("Page.bringToFront", None, Some(session_id))
            .await?;
        Ok(())
    }

    pub async fn set_timezone(&self, timezone_id: &str) -> Result<(), String> {
        let session_id = self.active_session_id()?;
        self.client
            .send_command(
                "Emulation.setTimezoneOverride",
                Some(json!({ "timezoneId": timezone_id })),
                Some(session_id),
            )
            .await?;
        Ok(())
    }

    pub async fn set_locale(&self, locale: &str) -> Result<(), String> {
        let session_id = self.active_session_id()?;
        self.client
            .send_command(
                "Emulation.setLocaleOverride",
                Some(json!({ "locale": locale })),
                Some(session_id),
            )
            .await?;
        Ok(())
    }

    pub async fn set_geolocation(
        &self,
        latitude: f64,
        longitude: f64,
        accuracy: Option<f64>,
    ) -> Result<(), String> {
        let session_id = self.active_session_id()?;
        self.client
            .send_command(
                "Emulation.setGeolocationOverride",
                Some(json!({
                    "latitude": latitude,
                    "longitude": longitude,
                    "accuracy": accuracy.unwrap_or(1.0),
                })),
                Some(session_id),
            )
            .await?;
        Ok(())
    }

    pub async fn grant_permissions(&self, permissions: &[String]) -> Result<(), String> {
        self.client
            .send_command(
                "Browser.grantPermissions",
                Some(json!({ "permissions": permissions })),
                None,
            )
            .await?;
        Ok(())
    }

    pub async fn handle_dialog(
        &self,
        accept: bool,
        prompt_text: Option<&str>,
    ) -> Result<(), String> {
        let session_id = self.active_session_id()?;
        let mut params = json!({ "accept": accept });
        if let Some(text) = prompt_text {
            params["promptText"] = Value::String(text.to_string());
        }
        self.client
            .send_command(
                "Page.handleJavaScriptDialog",
                Some(params),
                Some(session_id),
            )
            .await?;
        Ok(())
    }

    pub async fn upload_files(
        &self,
        selector: &str,
        files: &[String],
        ref_map: &RefMap,
        iframe_sessions: &HashMap<String, String>,
    ) -> Result<(), String> {
        let session_id = self.active_session_id()?;

        let (object_id, effective_session_id) =
            resolve_element_object_id(&self.client, session_id, ref_map, selector, iframe_sessions)
                .await?;

        let describe: Value = self
            .client
            .send_command(
                "DOM.describeNode",
                Some(json!({ "objectId": object_id })),
                Some(&effective_session_id),
            )
            .await?;

        let backend_node_id = describe
            .get("node")
            .and_then(|n| n.get("backendNodeId"))
            .and_then(|v| v.as_i64())
            .ok_or("Could not get backendNodeId for file input")?;

        let set_files = self
            .client
            .send_command(
                "DOM.setFileInputFiles",
                Some(json!({
                    "files": files,
                    "backendNodeId": backend_node_id,
                })),
                Some(&effective_session_id),
            )
            .await;

        if let Err(e) = set_files {
            // Chrome's chrome.debugger API (the extension-relay transport) forbids
            // DOM.setFileInputFiles for security, surfacing as an opaque
            // `-32000 "Not allowed"`. Fall back to constructing the File entirely
            // IN THE PAGE and assigning it to the input — the standard
            // Playwright/Cypress trick, which needs no privileged CDP and so works
            // over the relay (issue #13).
            if e.contains("Not allowed") || e.contains("-32000") {
                return self
                    .upload_files_via_page(object_id, files, &effective_session_id)
                    .await;
            }
            return Err(e);
        }

        Ok(())
    }

    /// Relay-safe file upload: read each file locally, hand its bytes to the page
    /// as base64, and rebuild a `File` there — then either assign it to a file
    /// `<input>` (Chrome allows `input.files = dataTransfer.files`) or, for a
    /// dropzone/composer, dispatch synthetic `paste`/`drop` events carrying the
    /// `DataTransfer`. No `DOM.setFileInputFiles`, so chrome.debugger permits it.
    async fn upload_files_via_page(
        &self,
        object_id: String,
        files: &[String],
        session_id: &str,
    ) -> Result<(), String> {
        use base64::Engine;
        // The relay tunnels every CDP message through Chrome native messaging,
        // which caps a single message at ~1 MiB. A whole image's base64 blows
        // past that ("CDP response channel closed"), so we STREAM the bytes into
        // a page-side buffer in sub-limit chunks, then assemble the File from it.
        const CHUNK: usize = 96 * 1024; // base64 chars per message; safe under 1 MiB

        // Reset the staging buffer.
        self.client
            .send_command(
                "Runtime.evaluate",
                Some(json!({ "expression": "window.__cuUpload = [];", "returnByValue": true })),
                Some(session_id),
            )
            .await
            .map_err(|e| format!("relay upload (reset) failed: {}", e))?;

        for path in files {
            let bytes = std::fs::read(path).map_err(|e| format!("cannot read {}: {}", path, e))?;
            let name = std::path::Path::new(path)
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("upload.bin")
                .to_string();
            let mime = mime_for_path(&name);
            let b64 = base64::engine::general_purpose::STANDARD.encode(&bytes);

            // Push the file's metadata with an empty buffer.
            let init = format!(
                "window.__cuUpload.push({{ name: {}, type: {}, b64: '' }});",
                serde_json::to_string(&name).unwrap_or_default(),
                serde_json::to_string(mime).unwrap_or_default(),
            );
            self.client
                .send_command(
                    "Runtime.evaluate",
                    Some(json!({ "expression": init, "returnByValue": true })),
                    Some(session_id),
                )
                .await
                .map_err(|e| format!("relay upload (init) failed: {}", e))?;

            // Stream the base64 in chunks. base64's alphabet (A–Za–z0–9+/=) needs
            // no escaping inside a single-quoted JS string, so concatenation is safe.
            let idx = "window.__cuUpload[window.__cuUpload.length-1].b64";
            let mut start = 0;
            while start < b64.len() {
                let end = (start + CHUNK).min(b64.len());
                let chunk = &b64[start..end];
                let expr = format!("{idx} += '{chunk}';");
                self.client
                    .send_command(
                        "Runtime.evaluate",
                        Some(json!({ "expression": expr, "returnByValue": true })),
                        Some(session_id),
                    )
                    .await
                    .map_err(|e| format!("relay upload (chunk) failed: {}", e))?;
                start = end;
            }
        }

        // Assemble the Files from the buffer and attach to the element, then clean up.
        let func = r#"function() {
            const filesData = window.__cuUpload || [];
            const dt = new DataTransfer();
            for (const f of filesData) {
                const bin = atob(f.b64);
                const arr = new Uint8Array(bin.length);
                for (let i = 0; i < bin.length; i++) arr[i] = bin.charCodeAt(i);
                dt.items.add(new File([arr], f.name, { type: f.type }));
            }
            try { delete window.__cuUpload; } catch (e) { window.__cuUpload = undefined; }
            const el = this;
            if (el.tagName === 'INPUT' && el.type === 'file') {
                el.files = dt.files;
                el.dispatchEvent(new Event('input', { bubbles: true }));
                el.dispatchEvent(new Event('change', { bubbles: true }));
                return 'input:' + dt.files.length;
            }
            // Dropzone / rich composer: replay paste then drop with the files.
            try { el.dispatchEvent(new ClipboardEvent('paste', { bubbles: true, clipboardData: dt })); } catch (e) {}
            try {
                const ev = new DragEvent('drop', { bubbles: true, cancelable: true });
                Object.defineProperty(ev, 'dataTransfer', { value: dt });
                el.dispatchEvent(ev);
            } catch (e) {}
            return 'event:' + dt.files.length;
        }"#;

        let result: EvaluateResult = self
            .client
            .send_command_typed(
                "Runtime.callFunctionOn",
                &CallFunctionOnParams {
                    function_declaration: func.to_string(),
                    object_id: Some(object_id),
                    arguments: None,
                    return_by_value: Some(true),
                    await_promise: Some(false),
                },
                Some(session_id),
            )
            .await
            .map_err(|e| format!("relay file-injection failed: {}", e))?;

        if let Some(ref details) = result.exception_details {
            return Err(format!(
                "relay file-injection threw: {}",
                details
                    .exception
                    .as_ref()
                    .and_then(|ex| ex.description.as_deref())
                    .unwrap_or(&details.text)
            ));
        }
        Ok(())
    }

    pub async fn add_script_to_evaluate(&self, source: &str) -> Result<String, String> {
        let session_id = self.active_session_id()?;
        let result = self
            .client
            .send_command(
                "Page.addScriptToEvaluateOnNewDocument",
                Some(json!({ "source": source })),
                Some(session_id),
            )
            .await?;
        Ok(result
            .get("identifier")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string())
    }

    pub async fn remove_script_to_evaluate(&self, identifier: &str) -> Result<(), String> {
        let session_id = self.active_session_id()?;
        self.client
            .send_command(
                "Page.removeScriptToEvaluateOnNewDocument",
                Some(json!({ "identifier": identifier })),
                Some(session_id),
            )
            .await?;
        Ok(())
    }

    pub async fn tab_switch_by_id(&mut self, tab_id: u32) -> Result<Value, String> {
        let index = self
            .pages
            .iter()
            .position(|p| p.tab_id == tab_id)
            .ok_or_else(|| format!("Tab ID {} not found", tab_id))?;
        self.tab_switch(index).await
    }

    pub async fn tab_close_by_id(&mut self, tab_id: Option<u32>) -> Result<Value, String> {
        let index = match tab_id {
            Some(id) => Some(
                self.pages
                    .iter()
                    .position(|p| p.tab_id == id)
                    .ok_or_else(|| format!("Tab ID {} not found", id))?,
            ),
            None => None,
        };
        self.tab_close(index).await
    }

    pub fn assign_tab_id(&mut self) -> u32 {
        let id = self.next_tab_id;
        self.next_tab_id += 1;
        id
    }

    pub fn add_page(&mut self, page: PageInfo) {
        let index = self.pages.len();
        self.pages.push(page);
        self.active_page_index = index;
        self.pin_active_target();
    }

    /// Add a passively-discovered page WITHOUT changing the active tab.
    ///
    /// On a shared browser (ab-connect), `Target.targetCreated` events stream in
    /// for tabs the user or OTHER agent sessions open. Those are drained on every
    /// command; routing them through `add_page` made the active tab silently jump
    /// to a foreign tab, so the session's own `eval`/`get title`/`screenshot`
    /// landed on the wrong page. Passively-tracked pages must not steal focus —
    /// only explicit opens (`tab new`, switch) set the active tab.
    pub fn add_background_page(&mut self, page: PageInfo) {
        if self.pages.iter().any(|p| p.target_id == page.target_id) {
            return;
        }
        self.pages.push(page);
    }

    pub fn update_page_target_info(&mut self, target: &TargetInfo) -> bool {
        update_page_target_info_in_pages(&mut self.pages, target)
    }

    pub fn remove_page_by_target_id(&mut self, target_id: &str) {
        if let Some(pos) = self.pages.iter().position(|p| p.target_id == target_id) {
            let removed_was_pinned = self.active_target_id.as_deref() == Some(target_id);
            self.pages.remove(pos);
            self.update_active_page_after_removal(pos);
            // If we just removed the pinned active target, the pin now dangles and
            // `resolved_active_index` silently falls back to `active_page_index`.
            // After a passive about:blank discovery that index can point at a blank
            // tab, so `wait` → eval/snapshot lands on about:blank (issue #7). Re-pin
            // to the surviving active page so the pin is never left pointing at a
            // target that no longer exists.
            if removed_was_pinned {
                self.pin_active_target();
            }
        }
    }

    pub fn has_target(&self, target_id: &str) -> bool {
        self.pages.iter().any(|p| p.target_id == target_id)
    }

    pub fn page_count(&self) -> usize {
        self.pages.len()
    }

    /// Returns the stable `tab_id` of the currently active page, if any.
    pub fn active_tab_id(&self) -> Option<u32> {
        self.pages.get(self.active_page_index).map(|p| p.tab_id)
    }

    /// Returns true if a tab with the given stable `tab_id` is still open.
    pub fn has_tab_id(&self, tab_id: u32) -> bool {
        self.pages.iter().any(|p| p.tab_id == tab_id)
    }

    pub fn pages_list(&self) -> Vec<PageInfo> {
        self.pages.clone()
    }

    pub fn visited_origins(&self) -> &HashSet<String> {
        &self.visited_origins
    }

    pub async fn set_download_behavior(&self, download_path: &str) -> Result<(), String> {
        let session_id = self.active_session_id()?;
        self.client
            .send_command(
                "Browser.setDownloadBehavior",
                Some(json!({
                    "behavior": "allowAndName",
                    "downloadPath": download_path,
                    "eventsEnabled": true,
                })),
                Some(session_id),
            )
            .await?;
        Ok(())
    }
}

/// Core network-idle polling loop, extracted so it can be unit-tested without a
/// full `BrowserManager` / CDP connection.
///
/// Returns `Ok(())` once no network requests have been in-flight for at least
/// 500 ms, or `Err` if `overall_timeout` elapses first.
async fn poll_network_idle(
    session_id: &str,
    rx: &mut broadcast::Receiver<CdpEvent>,
    overall_timeout: tokio::time::Duration,
) -> Result<(), String> {
    let pending = Arc::new(Mutex::new(HashSet::<String>::new()));

    tokio::time::timeout(overall_timeout, async {
        let mut idle_start: Option<tokio::time::Instant> = None;

        loop {
            let recv_result =
                tokio::time::timeout(tokio::time::Duration::from_millis(600), rx.recv()).await;

            match recv_result {
                Ok(Ok(event)) if event.session_id.as_deref() == Some(session_id) => {
                    let mut p = pending.lock().await;
                    match event.method.as_str() {
                        "Network.requestWillBeSent" => {
                            if let Some(id) = event.params.get("requestId").and_then(|v| v.as_str())
                            {
                                p.insert(id.to_string());
                                idle_start = None;
                            }
                        }
                        "Network.loadingFinished" | "Network.loadingFailed" => {
                            if let Some(id) = event.params.get("requestId").and_then(|v| v.as_str())
                            {
                                p.remove(id);
                                if p.is_empty() {
                                    idle_start = Some(tokio::time::Instant::now());
                                }
                            }
                        }
                        "Page.loadEventFired" if p.is_empty() => {
                            idle_start = Some(tokio::time::Instant::now());
                        }
                        _ => {}
                    }
                }
                Ok(Ok(_)) => {}
                Ok(Err(tokio::sync::broadcast::error::RecvError::Lagged(_))) => continue,
                Ok(Err(_)) => break,
                Err(_) => {
                    // Timeout on recv -- if no pending requests, start (or
                    // continue) the idle timer instead of returning
                    // immediately.  This prevents false-positive idle
                    // detection when the subscription starts after the page
                    // has already loaded (e.g. cached pages).
                    let p = pending.lock().await;
                    if p.is_empty() && idle_start.is_none() {
                        idle_start = Some(tokio::time::Instant::now());
                    }
                }
            }

            if let Some(start) = idle_start {
                if start.elapsed() >= tokio::time::Duration::from_millis(500) {
                    return Ok(());
                }
            }
        }

        Ok(())
    })
    .await
    .map_err(|_| "Timeout waiting for networkidle".to_string())?
}

async fn connect_cdp_with_retry(
    ws_url: &str,
    total_timeout: Duration,
    poll_interval: Duration,
) -> Result<CdpClient, String> {
    let deadline = Instant::now() + total_timeout;

    loop {
        match CdpClient::connect(ws_url).await {
            Ok(client) => return Ok(client),
            Err(err) => {
                if Instant::now() >= deadline {
                    return Err(err);
                }
            }
        }

        tokio::time::sleep(poll_interval).await;
    }
}

async fn initialize_lightpanda_manager(
    ws_url: String,
    process: BrowserProcess,
) -> Result<BrowserManager, String> {
    let deadline = Instant::now() + LIGHTPANDA_TARGET_INIT_TIMEOUT;
    let mut process = Some(process);

    loop {
        let client = match connect_cdp_with_retry(
            &ws_url,
            LIGHTPANDA_CDP_CONNECT_TIMEOUT,
            LIGHTPANDA_CDP_CONNECT_POLL_INTERVAL,
        )
        .await
        {
            Ok(client) => client,
            Err(err) => {
                if Instant::now() >= deadline {
                    return Err(lightpanda_target_init_timeout(Some(&err)));
                }
                tokio::time::sleep(LIGHTPANDA_CDP_CONNECT_POLL_INTERVAL).await;
                continue;
            }
        };

        let mut manager = BrowserManager {
            client: Arc::new(client),
            browser_process: None,
            ws_url: ws_url.clone(),
            pages: Vec::new(),
            active_page_index: 0,
            default_timeout_ms: 25_000,
            download_path: None,
            ignore_https_errors: false,
            visited_origins: HashSet::new(),
            created_targets: HashSet::new(),
            active_target_id: None,
            relay_target_misses: HashMap::new(),
            relay_scoped: false,
            next_tab_id: 1,
            capture_console: console_capture_enabled(),
        };

        match discover_and_attach_lightpanda_targets(&mut manager, deadline).await {
            Ok(()) => {
                manager.browser_process = process.take();
                return Ok(manager);
            }
            Err(err) => {
                if Instant::now() >= deadline {
                    return Err(lightpanda_target_init_timeout(Some(&err)));
                }
                tokio::time::sleep(LIGHTPANDA_CDP_CONNECT_POLL_INTERVAL).await;
            }
        }
    }
}

async fn discover_and_attach_lightpanda_targets(
    manager: &mut BrowserManager,
    deadline: Instant,
) -> Result<(), String> {
    run_with_lightpanda_deadline(
        deadline,
        manager.discover_and_attach_targets(),
        "Target domain initialization attempt exceeded the remaining startup deadline",
    )
    .await
}

fn remaining_until(deadline: Instant) -> Option<Duration> {
    deadline.checked_duration_since(Instant::now())
}

async fn run_with_lightpanda_deadline<F, T>(
    deadline: Instant,
    operation: F,
    timeout_context: &'static str,
) -> Result<T, String>
where
    F: Future<Output = Result<T, String>>,
{
    let remaining = remaining_until(deadline)
        .ok_or_else(|| lightpanda_target_init_timeout(Some("deadline expired before retry")))?;

    match tokio::time::timeout(remaining, operation).await {
        Ok(result) => result,
        Err(_) => Err(lightpanda_target_init_timeout(Some(timeout_context))),
    }
}

fn lightpanda_target_init_timeout(last_error: Option<&str>) -> String {
    let mut message = format!(
        "Timed out after {}ms waiting for Lightpanda Target domain to initialize",
        LIGHTPANDA_TARGET_INIT_TIMEOUT.as_millis(),
    );
    if let Some(last_error) = last_error {
        message.push_str(&format!("\nLast error: {}", last_error));
    }
    message
}

async fn resolve_cdp_url(input: &str) -> Result<String, String> {
    if input.starts_with("ws://") || input.starts_with("wss://") {
        return Ok(input.to_string());
    }

    if input.starts_with("http://") || input.starts_with("https://") {
        let parsed = url::Url::parse(input).map_err(|e| format!("Invalid CDP URL: {}", e))?;
        // If no explicit port and path is empty/root, this is likely a provider
        // WebSocket endpoint (e.g. https://xxx.cdp0.browser-use.com). Convert
        // the scheme to ws/wss and connect directly instead of probing :9222.
        if parsed.port().is_none() && (parsed.path().is_empty() || parsed.path() == "/") {
            let ws_scheme = if input.starts_with("https://") {
                "wss"
            } else {
                "ws"
            };
            let mut ws_url = parsed.clone();
            let _ = ws_url.set_scheme(ws_scheme);
            return Ok(ws_url.to_string());
        }
        let host = parsed
            .host_str()
            .ok_or_else(|| format!("No host in CDP URL: {}", input))?;
        let port = parsed.port().unwrap_or(9222);
        let query = parsed.query().map(|q| q.to_string());
        return discover_cdp_url(host, port, query.as_deref()).await;
    }

    // Try as numeric port
    if let Ok(port) = input.parse::<u16>() {
        return discover_cdp_url("127.0.0.1", port, None).await;
    }

    Err(format!(
        "Invalid CDP target: {}. Use ws://, http://, or a port number.",
        input
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::time::sleep;

    #[test]
    fn test_format_tab_id() {
        assert_eq!(format_tab_id(1), "t1");
        assert_eq!(format_tab_id(42), "t42");
    }

    #[test]
    fn liveness_responded_is_alive_for_both_kinds() {
        assert!(connection_alive_from_probe(LivenessProbe::Responded, true));
        assert!(connection_alive_from_probe(LivenessProbe::Responded, false));
    }

    #[test]
    fn liveness_transport_error_is_dead_for_both_kinds() {
        // A closed/reset WebSocket is a genuine death — reconnect in both cases.
        assert!(!connection_alive_from_probe(
            LivenessProbe::TransportError,
            true
        ));
        assert!(!connection_alive_from_probe(
            LivenessProbe::TransportError,
            false
        ));
    }

    #[test]
    fn liveness_timeout_keeps_external_attach_alive() {
        // Regression guard for the remote-debugging consent storm: a timed-out
        // probe must NOT tear down an externally-attached browser, otherwise the
        // daemon reconnects and re-pops Chrome's "Allow remote debugging?" modal
        // on every command (endless prompts + browser freeze).
        assert!(connection_alive_from_probe(LivenessProbe::TimedOut, true));
    }

    #[test]
    fn liveness_timeout_marks_launched_browser_dead() {
        // A browser we launched that stops responding is a real problem worth a
        // reconnect (and has no consent modal to worry about).
        assert!(!connection_alive_from_probe(LivenessProbe::TimedOut, false));
    }

    #[test]
    fn test_parse_tab_ref_id() {
        assert_eq!(TabRef::parse("t1"), Ok(TabRef::Id(1)));
        assert_eq!(TabRef::parse("t42"), Ok(TabRef::Id(42)));
        assert_eq!(TabRef::parse("T7"), Ok(TabRef::Id(7)));
    }

    #[test]
    fn test_parse_tab_ref_label() {
        assert_eq!(TabRef::parse("docs"), Ok(TabRef::Label("docs".to_string())));
        assert_eq!(
            TabRef::parse("app-2"),
            Ok(TabRef::Label("app-2".to_string()))
        );
        assert_eq!(
            TabRef::parse("my_tab"),
            Ok(TabRef::Label("my_tab".to_string()))
        );
    }

    #[test]
    fn test_parse_tab_ref_rejects_bare_integer() {
        let err = TabRef::parse("2").unwrap_err();
        assert!(
            err.contains("positional integers are not accepted"),
            "error should teach the user to use `t<N>`: {}",
            err
        );
        assert!(err.contains("t2"));
    }

    #[test]
    fn test_parse_tab_ref_rejects_empty() {
        assert!(TabRef::parse("").is_err());
        assert!(TabRef::parse("   ").is_err());
    }

    #[test]
    fn test_parse_tab_ref_rejects_zero() {
        let err = TabRef::parse("t0").unwrap_err();
        assert!(err.contains("start at t1"));
    }

    #[test]
    fn test_parse_tab_ref_rejects_invalid_label() {
        assert!(TabRef::parse("2docs").is_err());
        assert!(TabRef::parse("-docs").is_err());
        assert!(TabRef::parse("docs!").is_err());
        assert!(TabRef::parse("docs space").is_err());
    }

    #[test]
    fn test_is_valid_label() {
        assert!(is_valid_label("docs"));
        assert!(is_valid_label("Docs"));
        assert!(is_valid_label("app-2"));
        assert!(is_valid_label("my_tab"));
        assert!(!is_valid_label(""));
        assert!(!is_valid_label("2docs"));
        assert!(!is_valid_label("-docs"));
        assert!(!is_valid_label("docs!"));
    }

    #[test]
    fn test_should_track_popup_target_with_empty_url() {
        let target = TargetInfo {
            target_id: "popup-1".to_string(),
            target_type: "page".to_string(),
            title: String::new(),
            url: String::new(),
            attached: None,
            browser_context_id: None,
        };

        assert!(should_track_target(&target));
    }

    #[test]
    fn test_should_not_track_internal_chrome_target() {
        let target = TargetInfo {
            target_id: "chrome-tab".to_string(),
            target_type: "page".to_string(),
            title: "New Tab".to_string(),
            url: "chrome://newtab/".to_string(),
            attached: None,
            browser_context_id: None,
        };

        assert!(!should_track_target(&target));
    }

    #[test]
    fn test_update_page_target_info_in_pages_updates_existing_page() {
        let mut pages = vec![PageInfo {
            tab_id: 1,
            label: None,
            target_id: "popup-1".to_string(),
            session_id: "session-1".to_string(),
            url: String::new(),
            title: String::new(),
            target_type: "page".to_string(),
        }];
        let target = TargetInfo {
            target_id: "popup-1".to_string(),
            target_type: "page".to_string(),
            title: "Popup".to_string(),
            url: "https://example.com/popup".to_string(),
            attached: None,
            browser_context_id: None,
        };

        assert!(update_page_target_info_in_pages(&mut pages, &target));
        assert_eq!(pages[0].url, "https://example.com/popup");
        assert_eq!(pages[0].title, "Popup");
    }

    #[test]
    fn test_active_page_index_after_removal_shifts_when_earlier_tab_is_removed() {
        assert_eq!(active_page_index_after_removal(2, 0, 3), 1);
    }

    #[test]
    fn test_active_page_index_after_removal_keeps_same_slot_when_later_tab_is_removed() {
        assert_eq!(active_page_index_after_removal(1, 2, 3), 1);
    }

    #[test]
    fn test_active_page_index_after_removal_clamps_when_active_last_tab_is_removed() {
        assert_eq!(active_page_index_after_removal(3, 3, 3), 2);
    }

    #[test]
    fn test_active_page_index_after_removal_resets_when_last_page_disappears() {
        assert_eq!(active_page_index_after_removal(0, 0, 0), 0);
    }

    #[test]
    fn stale_target_error_matches_relay_signatures() {
        // The exact relay error `open` must recover from (issue #35), as wrapped
        // by send_command's `CDP error (Page.navigate): …` prefix.
        assert!(is_stale_target_error(
            "CDP error (Page.navigate): stale sessionId cb-tab-1655244623 for Page.navigate: \
             its tab is gone (closed, navigated across processes, or lost after an extension \
             restart). Re-attach by re-opening your target URL before retrying."
        ));
        assert!(is_stale_target_error(
            "unknown sessionId cb-tab-7 for Page.navigate"
        ));
        assert!(is_stale_target_error("no attached tab for Page.navigate"));
    }

    #[test]
    fn stale_target_error_ignores_unrelated_failures() {
        // A genuine navigation failure (bad URL, DNS, blocked) must NOT trigger
        // the open-a-fresh-tab recovery — that would mask the real error.
        assert!(!is_stale_target_error(
            "Navigation failed: net::ERR_NAME_NOT_RESOLVED"
        ));
        assert!(!is_stale_target_error(
            "CDP command timed out: Page.navigate"
        ));
    }

    fn page(target_id: &str) -> PageInfo {
        PageInfo {
            tab_id: 1,
            label: None,
            target_id: target_id.to_string(),
            session_id: format!("session-{target_id}"),
            url: String::new(),
            title: String::new(),
            target_type: "page".to_string(),
        }
    }

    // --- issue #21: --reuse-tab URL matching ignores query/fragment ---

    #[test]
    fn normalize_url_match_strips_query_and_fragment() {
        // Two opens of the "same" SSO page differ only in volatile query/hash —
        // they must normalize equal so --reuse-tab lands on the existing tab.
        let a = normalize_url_for_match(
            "https://login.account.rakuten.com/sso/authorize?client_id=x&state=abc#/sign_in",
        );
        let b = normalize_url_for_match(
            "https://login.account.rakuten.com/sso/authorize?client_id=y&state=zzz#/forgot",
        );
        assert_eq!(a, b);
        assert_eq!(a, "https://login.account.rakuten.com/sso/authorize");
    }

    #[test]
    fn normalize_url_match_distinguishes_different_paths() {
        let cart = normalize_url_for_match("https://cart.step.rakuten.co.jp/cart");
        let order = normalize_url_for_match("https://cart.step.rakuten.co.jp/order");
        assert_ne!(cart, order);
    }

    #[test]
    fn normalize_url_match_passes_through_unparseable() {
        assert_eq!(normalize_url_for_match("not a url"), "not a url");
    }

    // --- issue #14: a pinned target must keep commands on the right tab ---

    #[test]
    fn resolve_active_index_prefers_pin_over_stale_index() {
        // The tab we opened ("A") is at index 0, but `active_page_index` is stale
        // and points at a foreign tab ("B"). With the pin set, resolution sticks
        // to A — the drift that bit issue #14 (eval landing on /notifications).
        let pages = vec![page("A"), page("B")];
        assert_eq!(resolve_active_index(&pages, Some("A"), 1), 0);
    }

    #[test]
    fn resolve_active_index_unpinned_drifts_with_index() {
        // Documents the pre-fix hazard: with no pin, resolution blindly trusts
        // `active_page_index`, so a clamp/reorder from passive tab discovery lands
        // commands on a foreign tab. This is exactly what pinning on `open` avoids.
        let pages = vec![page("A"), page("B")];
        assert_eq!(resolve_active_index(&pages, None, 1), 1);
    }

    #[test]
    fn resolve_active_index_falls_back_when_pin_is_gone() {
        // If the pinned tab was closed (target_id no longer present), fall back to
        // the index rather than panicking or returning a bogus slot.
        let pages = vec![page("A"), page("B")];
        assert_eq!(resolve_active_index(&pages, Some("CLOSED"), 1), 1);
    }

    // --- issue: `open` must not hijack a user's tab on the relay (dogfood) ---

    #[test]
    fn active_not_owned_when_only_user_tabs_discovered() {
        // A fresh relay session passively attached to the user's tabs but created
        // none — so navigate must NOT reuse the active tab (it'd clobber the
        // user's page); it has to open its own first.
        let pages = vec![page("USER_A"), page("USER_B")];
        let created = HashSet::new();
        assert!(!active_index_is_owned(&pages, Some("USER_A"), 0, &created));
    }

    #[test]
    fn active_owned_when_session_created_the_tab() {
        let pages = vec![page("USER_A"), page("OURS")];
        let mut created = HashSet::new();
        created.insert("OURS".to_string());
        // Active pinned to the tab we created → safe to navigate it.
        assert!(active_index_is_owned(&pages, Some("OURS"), 1, &created));
        // But pinned to the user's tab → not owned, even though we own another.
        assert!(!active_index_is_owned(&pages, Some("USER_A"), 0, &created));
    }

    #[test]
    fn active_not_owned_when_no_pages() {
        let created = HashSet::new();
        assert!(!active_index_is_owned(&[], None, 0, &created));
    }

    #[test]
    fn test_sanitize_title() {
        // The exact pollution from #33: ZWJ / word-joiner / invisible-times / BOM
        // prepended to "GitHub".
        let dirty = "\u{200d}\u{2061}\u{200d}\u{2063}\u{200b}\u{2062}\u{feff}GitHub";
        assert_eq!(sanitize_title(dirty), "GitHub");
        // Clean titles (incl. CJK + normal punctuation) pass through untouched.
        assert_eq!(
            sanitize_title("購入手続きへ - メルカリ"),
            "購入手続きへ - メルカリ"
        );
        assert_eq!(sanitize_title("  Hello World  "), "Hello World");
        // Emoji and real content survive; only the invisibles are dropped.
        assert_eq!(sanitize_title("✓ Done\u{200b}"), "✓ Done");
    }

    #[test]
    fn test_mime_for_path() {
        assert_eq!(mime_for_path("a.png"), "image/png");
        assert_eq!(mime_for_path("PHOTO.JPG"), "image/jpeg");
        assert_eq!(mime_for_path("clip.webp"), "image/webp");
        assert_eq!(mime_for_path("doc.pdf"), "application/pdf");
        assert_eq!(mime_for_path("noext"), "application/octet-stream");
        assert_eq!(mime_for_path("weird.xyz"), "application/octet-stream");
    }

    #[test]
    fn prune_protects_pinned_target_on_transient_snapshot() {
        // The relay returned a getTargets snapshot missing the pinned tab "A"
        // (it hopped to another window). "B" is also absent. Without protection
        // both would be pruned and the next command would drift; with the pin
        // protected, only the genuinely-unpinned "B" is dropped (issue #31).
        let pages = vec![page("A"), page("B")];
        let live: HashSet<String> = HashSet::new(); // snapshot returned neither
        let gone = prunable_target_ids(&pages, &live, Some("A"));
        assert_eq!(gone, vec!["B".to_string()]);
        // With no pin, both are prunable (unchanged behavior).
        let gone_unpinned = prunable_target_ids(&pages, &live, None);
        assert_eq!(gone_unpinned.len(), 2);
        // A pinned target that IS in the live set is simply not prunable anyway.
        let mut live2 = HashSet::new();
        live2.insert("A".to_string());
        assert_eq!(
            prunable_target_ids(&pages, &live2, Some("A")),
            vec!["B".to_string()]
        );
    }

    #[test]
    fn debounced_prune_tolerates_transient_churn() {
        // Multi-agent churn: a single getTargets snapshot omits our owned tab "B"
        // (another agent opened/closed tabs). It must NOT be pruned on one miss.
        let pages = vec![page("A"), page("B")];
        let mut misses = HashMap::new();
        let empty: HashSet<String> = HashSet::new();
        // Misses 1 and 2: B absent but under threshold → not pruned.
        assert!(debounced_prune_ids(&pages, &empty, Some("A"), &mut misses).is_empty());
        assert!(debounced_prune_ids(&pages, &empty, Some("A"), &mut misses).is_empty());
        // Miss 3 (== RELAY_PRUNE_MISSES): genuinely gone → pruned.
        assert_eq!(
            debounced_prune_ids(&pages, &empty, Some("A"), &mut misses),
            vec!["B".to_string()]
        );
    }

    #[test]
    fn debounced_prune_resets_on_reappearance_and_protects_pin() {
        let pages = vec![page("A"), page("B")];
        let mut misses = HashMap::new();
        let empty: HashSet<String> = HashSet::new();
        let mut live_b: HashSet<String> = HashSet::new();
        live_b.insert("B".to_string());
        // Two misses for B, then it reappears → counter resets, so it survives
        // indefinitely under intermittent churn.
        debounced_prune_ids(&pages, &empty, Some("A"), &mut misses);
        debounced_prune_ids(&pages, &empty, Some("A"), &mut misses);
        assert!(debounced_prune_ids(&pages, &live_b, Some("A"), &mut misses).is_empty());
        assert!(debounced_prune_ids(&pages, &empty, Some("A"), &mut misses).is_empty()); // back to miss 1
                                                                                         // The pinned active "A" is never pruned no matter how many misses.
        for _ in 0..5 {
            let gone = debounced_prune_ids(&pages, &empty, Some("A"), &mut misses);
            assert!(!gone.contains(&"A".to_string()));
        }
    }

    #[test]
    fn resolve_active_index_pin_survives_passive_background_tab() {
        // A foreign tab ("Z") gets appended by passive discovery after we pinned
        // "A". The append doesn't shift A's position, and the pin keeps us on A
        // regardless of what `active_page_index` happens to be.
        let pages = vec![page("A"), page("B"), page("Z")];
        assert_eq!(resolve_active_index(&pages, Some("A"), 2), 0);
    }

    // issue #52: read/click resolution on the relay must NOT silently fall back
    // to active_page_index when the pin can't be resolved — that's how a command
    // drifts onto a foreign tab. The relay keeps a stable target_id across navs,
    // so a present pin resolves normally; only a genuinely-gone tab errors.
    #[test]
    fn strict_session_index_relay_pin_found_resolves() {
        let pages = vec![page("A"), page("B")];
        let owned = HashSet::from(["A".to_string()]);
        assert_eq!(
            strict_session_index(&pages, Some("A"), 1, true, &owned).unwrap(),
            0
        );
    }

    #[test]
    fn strict_session_index_relay_dangling_pin_errors() {
        let pages = vec![page("A"), page("Z")];
        let owned = HashSet::from(["A".to_string()]);
        // active_page_index points at the foreign "Z"; lenient would drift there.
        assert!(strict_session_index(&pages, Some("gone"), 1, true, &owned).is_err());
    }

    #[test]
    fn strict_session_index_relay_no_pin_resolves_owned_else_refuses() {
        // No pin on the relay: resolve only if the active tab is OURS; never drift
        // onto a foreign/foreground tab (issue #52).
        let pages = vec![page("A"), page("B")];
        let owns_b = HashSet::from(["B".to_string()]);
        assert_eq!(
            strict_session_index(&pages, None, 1, true, &owns_b).unwrap(),
            1
        );
        // active index 1 ("B") is NOT ours -> refuse rather than drift.
        let owns_a = HashSet::from(["A".to_string()]);
        assert!(strict_session_index(&pages, None, 1, true, &owns_a).is_err());
    }

    #[test]
    fn strict_session_index_off_relay_dangling_pin_stays_lenient() {
        // Off the relay (launched browser, no foreign tabs) behavior is unchanged.
        let pages = vec![page("A"), page("B")];
        let owned = HashSet::new();
        assert_eq!(
            strict_session_index(&pages, Some("gone"), 1, false, &owned).unwrap(),
            1
        );
    }

    // issue #7: removing the pinned active target must re-anchor the pin to a
    // surviving page. Models `remove_page_by_target_id`'s index + re-pin steps
    // purely (BrowserManager needs a live CDP client, so the method itself can't
    // be unit-constructed). The invariant: after removal the pin never dangles
    // and never silently resolves to a passively-discovered about:blank tab.
    fn simulate_remove(
        target_ids: &[&str],
        active_index: usize,
        pinned: &str,
        remove_id: &str,
    ) -> (Vec<String>, usize, Option<String>) {
        let pos = target_ids.iter().position(|t| *t == remove_id).unwrap();
        let removed_was_pinned = pinned == remove_id;
        let mut pages: Vec<String> = target_ids.iter().map(|s| s.to_string()).collect();
        pages.remove(pos);
        let new_active = active_page_index_after_removal(active_index, pos, pages.len());
        let new_pin = if removed_was_pinned {
            pages.get(new_active).cloned()
        } else {
            Some(pinned.to_string())
        };
        (pages, new_active, new_pin)
    }

    fn resolve_active<'a>(
        pages: &'a [String],
        active_index: usize,
        pin: &Option<String>,
    ) -> &'a str {
        if let Some(tid) = pin {
            if let Some(p) = pages.iter().find(|p| *p == tid) {
                return p;
            }
        }
        pages.get(active_index).map(|s| s.as_str()).unwrap_or("")
    }

    #[test]
    fn test_removing_unpinned_blank_keeps_pin_on_real_page() {
        // pages = [creepjs(pinned, active), about:blank]; a passive blank closes.
        let (pages, active, pin) = simulate_remove(&["creepjs", "blank"], 0, "creepjs", "blank");
        assert_eq!(resolve_active(&pages, active, &pin), "creepjs");
    }

    #[test]
    fn test_removing_pinned_page_repins_to_survivor_not_dangling() {
        // pages = [blank, creepjs(pinned, active)]; the pinned page itself closes.
        let (pages, active, pin) = simulate_remove(&["blank", "creepjs"], 1, "creepjs", "creepjs");
        // pin must point at a page that still exists (no dangling fallback).
        let resolved = resolve_active(&pages, active, &pin);
        assert!(
            pages.iter().any(|p| p == resolved),
            "resolved a dangling target"
        );
        assert_eq!(resolved, "blank");
    }

    #[test]
    fn test_resolve_falls_back_cleanly_when_pin_dangles() {
        // A stale pin (target already gone) must resolve to a real surviving page,
        // never panic or return the missing id.
        let pages = vec!["creepjs".to_string(), "blank".to_string()];
        let pin = Some("gone".to_string());
        assert_eq!(resolve_active(&pages, 0, &pin), "creepjs");
    }

    #[test]
    fn test_validate_launch_options_extensions_and_cdp() {
        let ext = vec!["/path/to/ext".to_string()];
        assert!(validate_launch_options(Some(&ext), true, None, None, false, None,).is_err());
    }

    #[test]
    fn test_validate_launch_options_profile_and_cdp() {
        assert!(validate_launch_options(None, true, Some("/path"), None, false, None,).is_err());
    }

    #[test]
    fn test_validate_launch_options_storage_state_and_profile() {
        assert!(validate_launch_options(
            None,
            false,
            Some("/profile"),
            Some("/state.json"),
            false,
            None,
        )
        .is_err());
    }

    #[test]
    fn test_validate_launch_options_storage_state_and_extensions() {
        let ext = vec!["/ext".to_string()];
        assert!(
            validate_launch_options(Some(&ext), false, None, Some("/state.json"), false, None,)
                .is_err()
        );
    }

    #[test]
    fn test_validate_launch_options_allow_file_access_firefox() {
        assert!(
            validate_launch_options(None, false, None, None, true, Some("/usr/bin/firefox"),)
                .is_err()
        );
    }

    #[test]
    fn test_validate_launch_options_valid() {
        assert!(validate_launch_options(None, false, None, None, false, None,).is_ok());
    }

    #[test]
    fn test_to_ai_friendly_error_strict_mode() {
        assert_eq!(
            to_ai_friendly_error("Strict mode violation: multiple elements"),
            "Element matched multiple results. Use a more specific selector."
        );
    }

    #[test]
    fn test_to_ai_friendly_error_not_visible() {
        assert_eq!(
            to_ai_friendly_error("element is not visible"),
            "Element exists but is not visible. Wait for it to become visible or scroll it into view."
        );
    }

    #[test]
    fn test_to_ai_friendly_error_intercept() {
        assert_eq!(
            to_ai_friendly_error("element intercepted by another element"),
            "Another element is covering the target element. Try scrolling or closing overlays."
        );
    }

    #[test]
    fn test_to_ai_friendly_error_timeout() {
        assert_eq!(
            to_ai_friendly_error("Timeout waiting for element"),
            "Operation timed out. The page may still be loading or the element may not exist."
        );
    }

    #[test]
    fn test_to_ai_friendly_error_not_found() {
        let m = to_ai_friendly_error("Element not found");
        assert!(m.starts_with("Element not found"));
        // directs to snapshot -i (which pierces closed shadow / cross-origin iframes)
        assert!(m.contains("snapshot -i") && m.contains("shadow root"));
    }

    #[test]
    fn test_to_ai_friendly_error_unknown() {
        let msg = "Some custom error message";
        assert_eq!(to_ai_friendly_error(msg), msg);
    }

    /// Errors containing "not found" but NOT "element" should pass through unchanged.
    #[test]
    fn test_to_ai_friendly_error_ignores_non_element_not_found() {
        let err = "Chrome not found. Install Chrome or use --executable-path.";
        assert_eq!(to_ai_friendly_error(err), err);
    }

    #[test]
    fn test_to_ai_friendly_error_catches_no_element() {
        let m = to_ai_friendly_error("No element found for css 'x'");
        assert!(m.starts_with("Element not found"));
        assert!(m.contains("snapshot -i"));
    }

    #[test]
    fn test_remaining_until_returns_none_for_past_deadline() {
        let deadline = Instant::now()
            .checked_sub(Duration::from_millis(1))
            .expect("past instant should be representable");
        assert!(remaining_until(deadline).is_none());
    }

    #[tokio::test]
    async fn test_run_with_lightpanda_deadline_enforces_timeout() {
        let deadline = Instant::now() + Duration::from_millis(25);
        let err = tokio::time::timeout(
            Duration::from_secs(1),
            run_with_lightpanda_deadline(
                deadline,
                async {
                    sleep(Duration::from_millis(100)).await;
                    Ok::<(), String>(())
                },
                "Target domain initialization attempt exceeded the remaining startup deadline",
            ),
        )
        .await
        .expect("outer timeout should not fire")
        .unwrap_err();

        assert!(err.contains(
            "Timed out after 10000ms waiting for Lightpanda Target domain to initialize"
        ));
        assert!(err.contains("remaining startup deadline"));
    }

    #[tokio::test]
    async fn test_run_with_lightpanda_deadline_returns_operation_error() {
        let deadline = Instant::now() + Duration::from_secs(1);
        let err = run_with_lightpanda_deadline(
            deadline,
            async { Err::<(), String>("Target.getTargets failed".to_string()) },
            "unused timeout context",
        )
        .await
        .unwrap_err();

        assert_eq!(err, "Target.getTargets failed");
    }

    #[test]
    fn test_lightpanda_target_init_timeout_includes_last_error() {
        let err = lightpanda_target_init_timeout(Some("Target.setDiscoverTargets failed"));
        assert!(err.contains(
            "Timed out after 10000ms waiting for Lightpanda Target domain to initialize"
        ));
        assert!(err.contains("Target.setDiscoverTargets failed"));
    }

    #[test]
    fn test_is_internal_chrome_target() {
        assert!(is_internal_chrome_target("chrome://newtab/"));
        assert!(is_internal_chrome_target(
            "chrome://omnibox-popup.top-chrome/"
        ));
        assert!(is_internal_chrome_target(
            "chrome-extension://abc123/popup.html"
        ));
        assert!(is_internal_chrome_target(
            "devtools://devtools/bundled/inspector.html"
        ));
        assert!(!is_internal_chrome_target("https://example.com"));
        assert!(!is_internal_chrome_target("http://localhost:3000"));
        assert!(!is_internal_chrome_target("about:blank"));
    }

    // -----------------------------------------------------------------------
    // poll_network_idle tests
    // -----------------------------------------------------------------------

    fn cdp_event(method: &str, session_id: &str, params: Value) -> CdpEvent {
        CdpEvent {
            method: method.to_string(),
            params,
            session_id: Some(session_id.to_string()),
        }
    }

    /// Regression test for #846: when no network events arrive at all (e.g.
    /// page fully served from cache), poll_network_idle must NOT return
    /// instantly.  It should observe at least 500 ms of idle before resolving.
    #[tokio::test]
    async fn test_network_idle_no_events_does_not_return_instantly() {
        let (tx, mut rx) = broadcast::channel::<CdpEvent>(16);
        let session = "s1";

        let start = tokio::time::Instant::now();
        let result = tokio::time::timeout(
            Duration::from_secs(5),
            poll_network_idle(session, &mut rx, Duration::from_secs(5)),
        )
        .await
        .expect("outer timeout should not fire");

        assert!(result.is_ok());
        let elapsed = start.elapsed();
        assert!(
            elapsed >= Duration::from_millis(500),
            "network idle returned in {:?}, expected >= 500ms",
            elapsed
        );

        drop(tx);
    }

    /// Normal flow: requests start and finish, idle is detected after the last
    /// request completes and 500 ms of silence passes.
    #[tokio::test]
    async fn test_network_idle_after_requests_complete() {
        let (tx, mut rx) = broadcast::channel::<CdpEvent>(16);
        let session = "s1";

        let _keep_alive = tx.clone();
        tokio::spawn(async move {
            sleep(Duration::from_millis(50)).await;
            let _ = tx.send(cdp_event(
                "Network.requestWillBeSent",
                session,
                json!({ "requestId": "r1" }),
            ));
            sleep(Duration::from_millis(100)).await;
            let _ = tx.send(cdp_event(
                "Network.loadingFinished",
                session,
                json!({ "requestId": "r1" }),
            ));
        });

        let start = tokio::time::Instant::now();
        let result = tokio::time::timeout(
            Duration::from_secs(5),
            poll_network_idle(session, &mut rx, Duration::from_secs(5)),
        )
        .await
        .expect("outer timeout should not fire");

        assert!(result.is_ok());
        let elapsed = start.elapsed();
        assert!(
            elapsed >= Duration::from_millis(500),
            "should wait >= 500ms after last request finishes, got {:?}",
            elapsed
        );
    }

    /// A new request arriving during the idle window resets the timer.
    #[tokio::test]
    async fn test_network_idle_resets_on_new_request() {
        let (tx, mut rx) = broadcast::channel::<CdpEvent>(16);
        let session = "s1";

        let _keep_alive = tx.clone();
        tokio::spawn(async move {
            sleep(Duration::from_millis(50)).await;
            let _ = tx.send(cdp_event(
                "Network.requestWillBeSent",
                session,
                json!({ "requestId": "r1" }),
            ));
            sleep(Duration::from_millis(50)).await;
            let _ = tx.send(cdp_event(
                "Network.loadingFinished",
                session,
                json!({ "requestId": "r1" }),
            ));
            // Wait 200ms (< 500ms idle window), then fire another request
            sleep(Duration::from_millis(200)).await;
            let _ = tx.send(cdp_event(
                "Network.requestWillBeSent",
                session,
                json!({ "requestId": "r2" }),
            ));
            sleep(Duration::from_millis(100)).await;
            let _ = tx.send(cdp_event(
                "Network.loadingFinished",
                session,
                json!({ "requestId": "r2" }),
            ));
        });

        let start = tokio::time::Instant::now();
        let result = tokio::time::timeout(
            Duration::from_secs(5),
            poll_network_idle(session, &mut rx, Duration::from_secs(5)),
        )
        .await
        .expect("outer timeout should not fire");

        assert!(result.is_ok());
        let elapsed = start.elapsed();
        // r2 finishes at ~400ms; idle should be detected at ~900ms
        assert!(
            elapsed >= Duration::from_millis(800),
            "should wait for idle after second request, got {:?}",
            elapsed
        );
    }

    /// When the overall timeout expires before idle is reached, the function
    /// returns an error.
    #[tokio::test]
    async fn test_network_idle_overall_timeout() {
        let (tx, mut rx) = broadcast::channel::<CdpEvent>(16);
        let session = "s1";

        // Keep sending requests so idle is never reached
        tokio::spawn(async move {
            for i in 0u64.. {
                let _ = tx.send(cdp_event(
                    "Network.requestWillBeSent",
                    session,
                    json!({ "requestId": format!("r{}", i) }),
                ));
                sleep(Duration::from_millis(100)).await;
            }
        });

        let result = poll_network_idle(session, &mut rx, Duration::from_millis(800)).await;
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .contains("Timeout waiting for networkidle"));
    }
}
