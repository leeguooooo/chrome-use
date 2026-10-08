use futures_util::stream::{FuturesUnordered, StreamExt};
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::{broadcast, Mutex};

use super::cdp::chrome::{
    auto_connect_cdp, launch_chrome, launches_headless, ChromeProcess, LaunchOptions,
};
use super::cdp::client::CdpClient;
use super::cdp::discovery::discover_cdp_url;
use super::cdp::lightpanda::{launch_lightpanda, LightpandaLaunchOptions, LightpandaProcess};
use super::cdp::types::*;
use super::element::{resolve_element_object_id, RefMap};

/// The daemon's session name, set once at daemon start. Names the Chrome tab
/// group that abs-created tabs land in when driving the user's real Chrome via
/// the `ab-connect` extension, so each agent/session gets its own group.
pub static DAEMON_SESSION: std::sync::OnceLock<String> = std::sync::OnceLock::new();

/// Unit tests run `execute_command` on a bare `DaemonState`, and every one
/// that needed a browser launched a real Chrome on the machine running
/// `cargo test` (launches are headed unless AGENT_BROWSER_ALLOW_HEADLESS=1).
/// On the build box a plain `cargo test` added bursts of `session=default
/// mode=launched(debug-port)` lines to `~/.chrome-use/connect-mode.log`, and
/// none once this refusal was in. In a build without the `e2e-tests` feature,
/// nothing launches; with that feature, every test in the build (unit tests
/// included) can still launch.
#[cfg(all(test, not(feature = "e2e-tests")))]
const UNIT_TEST_LAUNCH_REFUSAL: Option<&str> =
    Some("unit tests never launch a browser (only the e2e-tests feature does)");
#[cfg(not(all(test, not(feature = "e2e-tests"))))]
const UNIT_TEST_LAUNCH_REFUSAL: Option<&str> = None;

/// How long `close()` may spend closing the tabs this session created before it
/// gives up and lets the process exit (issue #192).
///
/// Sized against the two clocks that bracket it: the relay gives each CDP
/// command 8s (`RELAY_COMMAND_TIMEOUT_MS`), and `kill_stale_daemon` force-kills
/// us [`crate::connection::DAEMON_SHUTDOWN_GRACE`] after SIGTERM. Landing under
/// the grace period means a slow relay costs us some tabs, not the clean exit —
/// and the caller's SIGKILL is no longer what decides whether cleanup ran.
pub const OWNED_TAB_CLEANUP_BUDGET: Duration = Duration::from_secs(5);

/// Close only persisted, endpoint-matched deletion rights without discovering,
/// attaching to, or creating any other tabs. Used after an idle daemon exit.
pub async fn close_persisted_session_tabs_at(session: &str, endpoint: &str) -> Result<(), String> {
    let mut targets = crate::connection::read_created_targets(session, endpoint);
    if targets.is_empty() {
        return Err(
            "the connected browser does not match the session's saved tab ownership".into(),
        );
    }
    let client = Arc::new(CdpClient::connect(endpoint).await?);
    close_created_targets(&client, &mut targets).await;
    crate::connection::write_created_targets(session, endpoint, &targets)?;
    if !targets.is_empty() {
        return Err(format!(
            "{} created tab(s) could not be closed; ownership was preserved",
            targets.len()
        ));
    }
    Ok(())
}

/// Rediscover the original external browser rather than storing its possibly
/// credential-bearing CDP URL. A mismatch fails closed and keeps deletion rights.
pub async fn close_persisted_session_tabs(session: &str) -> Result<(), String> {
    let endpoint = auto_connect_cdp().await?;
    close_persisted_session_tabs_at(session, &endpoint).await
}

async fn close_created_targets(client: &Arc<CdpClient>, targets: &mut HashSet<String>) {
    // Each future owns its id while completed closes mutate the same set.
    #[allow(clippy::redundant_iter_cloned)]
    let mut closes: FuturesUnordered<_> = targets
        .iter()
        .cloned()
        .map(|target_id| {
            let client = Arc::clone(client);
            async move {
                let result = client
                    .send_command_typed::<_, CloseTargetResult>(
                        "Target.closeTarget",
                        &CloseTargetParams {
                            target_id: target_id.clone(),
                        },
                        None,
                    )
                    .await;
                (target_id, result)
            }
        })
        .collect();
    let _ = tokio::time::timeout(OWNED_TAB_CLEANUP_BUDGET, async {
        while let Some((target_id, result)) = closes.next().await {
            if target_was_closed(&result) {
                targets.remove(&target_id);
            }
        }
    })
    .await;
}

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
///
/// Also the test for "a driven web tab can never have navigated here", used
/// when deciding whether a session that answered from an unexpected url was
/// pinned to a page it cannot leave or had simply redirected. Matching is
/// case-insensitive and tolerates surrounding space, because the url comes back
/// from the page rather than from us.
pub(crate) fn is_internal_chrome_target(url: &str) -> bool {
    const INTERNAL: &[&str] = &[
        "chrome://",
        "chrome-extension://",
        "chrome-untrusted://",
        "devtools://",
        "edge://",
    ];
    let lowered = url.trim().to_ascii_lowercase();
    INTERNAL.iter().any(|prefix| lowered.starts_with(prefix))
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

/// Resolve the active-target pin after a page is removed.
///
/// On the shared extension relay, removing the pinned page must leave a
/// tombstone pin instead of silently selecting a surviving tab. A surviving tab
/// is commonly the session's `about:blank` scratch page, and retargeting reads
/// there unloads the user's SPA state while returning plausible but incorrect
/// data. The tombstone makes strict relay routing fail loudly and lets the stale
/// target retry re-discover the same stable target if the removal was transient.
/// A launched browser has no foreign tabs, so it retains the historical
/// survivor fallback.
fn active_target_after_removal(
    pages: &[PageInfo],
    active_page_index: usize,
    active_target_id: Option<&str>,
    removed_target_id: &str,
    on_relay: bool,
) -> Option<String> {
    if active_target_id != Some(removed_target_id) {
        return active_target_id.map(str::to_string);
    }
    if on_relay {
        return Some(removed_target_id.to_string());
    }
    pages
        .get(active_page_index)
        .map(|page| page.target_id.clone())
}

/// Return the stored target pin for relay session recovery.
///
/// Do not use the lenient resolved active page here: after a relay target is
/// removed, that fallback can point at a surviving `about:blank` page while the
/// stored pin deliberately remains a tombstone for the original target.
fn target_id_for_reattach(active_target_id: Option<&str>) -> Result<String, String> {
    active_target_id
        .map(str::to_string)
        .ok_or_else(|| BOUND_TAB_GONE.to_string())
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
     wrong tab. Run `tab list`; select it only if ownership is `created` or `adopted`, \
     otherwise use `tab adopt <url|targetId>` without navigating. If it is absent, re-open \
     the target URL with `open <url>`.";

/// Message when a relay session has no tab of its own to route a command to.
const NO_OWNED_TAB: &str =
    "this session owns no resolvable tab in its group. Refusing to run on a tab this session \
     didn't open — on the shared browser that could read/click the user's or another agent's \
     tab. `open <url>` creates your own tab; `tab adopt <url>` explicitly takes an existing one \
     without navigating it.";

/// Strict session-index resolution for routing READ/CLICK commands.
///
/// On a guarded external browser the lenient [`resolve_active_index`] falls
/// back to `active_page_index` when the pinned target isn't found — and after
/// foreign-tab churn that index can point at an unrelated tab, so an
/// `eval`/`snapshot`/`click` would land on it (issue #52, a safety risk).
///
/// The relay keeps a STABLE `target_id` across navigations (verified live), so a
/// present pin resolves on every normal command — a pin that genuinely can't be
/// found means the bound tab is gone, which we surface as a loud error instead of
/// drifting. A browser we launched has no foreign tabs and stays lenient.
fn strict_session_index(
    pages: &[PageInfo],
    active_target_id: Option<&str>,
    active_page_index: usize,
    ownership_guarded: bool,
    drivable_targets: &HashSet<String>,
) -> Result<usize, String> {
    if ownership_guarded {
        // Prefer the pin — it must resolve to a tab this session may drive.
        if let Some(tid) = active_target_id {
            return pages
                .iter()
                .position(|p| p.target_id == tid && drivable_targets.contains(&p.target_id))
                .ok_or_else(|| BOUND_TAB_GONE.to_string());
        }
        // No pin: resolve only to a tab this session may drive. The lenient fallback
        // would return `active_page_index`, which after foreign-tab churn can
        // point at the user's / another agent's tab — and a read/click would land
        // on it (issue #52). Each agent is scoped to its own group; using a tab
        // outside it requires an explicit `adopt`, so refuse to drift here.
        let i = resolve_active_index(pages, None, active_page_index);
        if pages
            .get(i)
            .is_some_and(|p| drivable_targets.contains(&p.target_id))
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

/// Whether `url` is named by an `adopt` spec: a case-insensitive substring.
/// An empty spec names nothing.
fn url_matches_adopt_spec(url: &str, spec: &str) -> bool {
    let spec = spec.trim();
    !spec.is_empty() && url.to_lowercase().contains(&spec.to_lowercase())
}

/// Set once a `chrome-use adopt <spec>` directive has been carried out.
///
/// The directive arrives as `AGENT_BROWSER_ADOPT` in the daemon's environment,
/// which lives as long as the daemon does. Every later reconnect (relay
/// restart, dead-connection relaunch) re-ran discovery and re-adopted by that
/// spec — and once the adopted page had navigated on (a login redirect), each
/// reconnect failed with `adopt: no open tab matching …`, so the session stayed
/// broken after the page itself had recovered (#357). It applies to the first
/// connect that succeeds, and never again.
static ADOPT_DIRECTIVE_DONE: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

fn pending_adopt_directive() -> Option<String> {
    if ADOPT_DIRECTIVE_DONE.load(std::sync::atomic::Ordering::SeqCst) {
        return None;
    }
    std::env::var("AGENT_BROWSER_ADOPT")
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// Strip zero-width / invisible / bidi-format Unicode from a page title before
/// we store it. Some sites prepend runs of ZWJ / word-joiner / invisible-times /
/// BOM to `document.title` (badging, watermarking, anti-scrape); left in, they
/// pollute `tab list`, break text matching, and wreck column alignment (#33).
/// The tab group of a `tabs.get` result, if the tab is in one.
fn live_group(tab: &Result<Value, String>) -> Option<i64> {
    tab.as_ref()
        .ok()?
        .get("groupId")
        .and_then(Value::as_i64)
        .filter(|g| *g >= 0)
}

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

/// Whether the resolved active page is a tab this session may drive. Pure core
/// of [`BrowserManager::active_is_drivable`] so the relay no-hijack rule is
/// unit-testable without a live browser.
fn active_index_is_drivable(
    pages: &[PageInfo],
    active_target_id: Option<&str>,
    active_page_index: usize,
    drivable_targets: &HashSet<String>,
) -> bool {
    pages
        .get(resolve_active_index(
            pages,
            active_target_id,
            active_page_index,
        ))
        .map(|p| drivable_targets.contains(&p.target_id))
        .unwrap_or(false)
}

/// External Chrome tabs stay user-owned unless this session created them.
/// Adoption grants drive access only; it never grants deletion rights.
fn tab_close_is_allowed(
    browser_is_external: bool,
    target_id: &str,
    created_targets: &HashSet<String>,
) -> bool {
    !browser_is_external || created_targets.contains(target_id)
}

/// External Chrome may switch only to tabs this session created or adopted.
fn tab_switch_is_allowed(
    browser_is_external: bool,
    target_id: &str,
    owned_targets: &HashSet<String>,
) -> bool {
    !browser_is_external || owned_targets.contains(target_id)
}

/// Refusal text for driving a tab this session neither created nor adopted.
///
/// The recovery hint must name the *targetId*, not the `t<N>` ref: `tab adopt`
/// matches a spec against targetIds and URL substrings only (see
/// `adopt_existing_target`), so `tab adopt t1` would just fail with "no open tab
/// matching `t1`" and send the agent in circles.
fn refuse_unowned_tab_message(tab_id: u32, target_id: &str) -> String {
    format!(
        "Refusing to select tab {} because this session did not create or adopt it \
         (run `chrome-use tab adopt {}` to drive it)",
        format_tab_id(tab_id),
        target_id
    )
}

/// Reports ownership only for external browsers, where deletion rights differ.
fn tab_ownership(
    browser_is_external: bool,
    target_id: &str,
    created_targets: &HashSet<String>,
    adopted_targets: &HashSet<String>,
) -> Option<&'static str> {
    if !browser_is_external {
        None
    } else if created_targets.contains(target_id) {
        Some("created")
    } else if adopted_targets.contains(target_id) {
        Some("adopted")
    } else {
        Some("foreign")
    }
}

/// Restores persisted created targets and adopts other relay-scoped targets.
fn register_scoped_target_ownership(
    target_ids: &[String],
    created_targets: &HashSet<String>,
    adopted_targets: &mut HashSet<String>,
) {
    adopted_targets.extend(
        target_ids
            .iter()
            .filter(|target_id| !created_targets.contains(*target_id))
            .cloned(),
    );
}

/// A successful CDP round trip is not enough: the extension reports a missing
/// or unclosable tab as `{ success: false }`.
fn target_was_closed(result: &Result<CloseTargetResult, String>) -> bool {
    result.as_ref().is_ok_and(|result| result.success)
}

/// Whether a CDP error means the bound relay target is gone — the tab was
/// closed, navigated across processes (renderer swap), or lost after an
/// extension/service-worker restart, and the relay could not re-attach. The
/// ab-connect relay surfaces these as `stale sessionId … its tab is gone`,
/// `unknown sessionId …`, or `no attached tab …`. `navigate` keys its
/// auto-reattach recovery off this (issue #35) so a dead session rebinds to a
/// fresh tab instead of erroring on every command until the user runs `tab new`.
pub(crate) fn is_stale_target_error(error: &str) -> bool {
    let lower = error.to_lowercase();
    if is_debugger_access_denied(error) {
        return false;
    }
    if lower.contains("action_outcome_unknown:") {
        return false;
    }
    lower.contains("its tab is gone")
        || lower.contains("stale sessionid")
        || lower.contains("unknown sessionid")
        || lower.contains("no attached tab")
        || lower.contains("can no longer be resolved")
}

/// A Chrome access decision is not a lost tab and cannot be fixed by reattachment.
pub(crate) fn is_debugger_access_denied(error: &str) -> bool {
    let lower = error.to_ascii_lowercase();
    lower.contains("debugger_access_denied:")
        || (lower.contains("cannot access a chrome-extension://")
            && lower.contains("different extension"))
}

/// The recovery's error when the blocked tab shares a window with the user's
/// own tabs: hiding it there means switching the user's tab, which chrome-use
/// never does.
const USER_WINDOW_MENU: &str = "the blocked tab is in a window with the user's own tabs, and \
closing the menu from here would switch the tab in front of that window";

/// Whether a `tabs.query` result may hold a tab the user opened: any tab not
/// in `owned`, whatever its URL (a user's blank tab is still theirs), and
/// anything unreadable (no list, a tab without an id), because guessing
/// "agent-only" is how a recovery ends up switching the user's tab.
fn window_has_unowned_tab(tabs: &Value, owned: &HashSet<i64>) -> bool {
    let Some(tabs) = tabs.as_array() else {
        return true;
    };
    tabs.iter()
        .any(|tab| match tab.get("id").and_then(Value::as_i64) {
            Some(id) => !owned.contains(&id),
            None => true,
        })
}

/// The recovery's error when the menu was open again after the tab was shown.
const MENU_STILL_OPEN: &str = "the menu was still open after chrome-use hid the tab for a moment";
/// [`MENU_STILL_OPEN`] for a tab in front, which gets a second try.
const MENU_STILL_OPEN_IN_FRONT: &str = "menu still open (tab in front)";
/// The recovery's error when the menu stayed open and Chrome is not the
/// window in front: a covered window's tabs are all hidden already, so
/// switching tabs never makes the page hidden (#449).
const MENU_COVERED_WINDOW: &str = "the menu was still open after chrome-use hid the tab for a \
    moment. Chrome is not the window in front: while another app covers it (or it is on another \
    Space), macOS reports every tab in it as hidden, so hiding the tab closes nothing. Ask the \
    user to bring that Chrome window to the front once (or to press Escape in the tab); \
    chrome-use then closes the menu by itself on the next command";

/// Password managers whose inline autofill menu is a frame of their own
/// extension, mounted next to a focused login or card field. While that frame
/// is in a tab, Chrome refuses every debugger command on the tab (#373).
const INLINE_MENU_EXTENSIONS: &[(&str, &str)] = &[
    ("nngceckbapebfimnlniiiahkandclblb", "Bitwarden"),
    ("aeblfdkhhhdcdjpifhhbdiojplfjncoa", "1Password"),
    ("hdokiejnpimakedhajhdlcegeplioahd", "LastPass"),
    ("fdjamakpfbbddfjaooikfcpapjohcfmg", "Dashlane"),
    ("fooolghllnmhmmndgjiamiiodkpenpbb", "NordPass"),
    ("ghmbeldphafepmbegfdlkpapadhbakde", "Proton Pass"),
    ("bfogiafebfohielmmehodmfbbebbbpei", "Keeper"),
    ("pnlccmojcmeohlpggmfnbbiapkmbliob", "RoboForm"),
    ("kmcfomidfpdkfieipokbalgegidffkal", "Enpass"),
    ("pejdijmoenmkgeppbflobdenhhabjlaj", "iCloud Passwords"),
];

/// Chrome-family user-data directories on this machine.
fn chrome_user_data_dirs() -> Vec<std::path::PathBuf> {
    let mut roots = Vec::new();
    #[cfg(target_os = "macos")]
    if let Some(base) = dirs::data_dir() {
        for d in [
            "Google/Chrome",
            "Google/Chrome Beta",
            "Chromium",
            "Microsoft Edge",
            "BraveSoftware/Brave-Browser",
        ] {
            roots.push(base.join(d));
        }
    }
    #[cfg(target_os = "linux")]
    if let Some(base) = dirs::config_dir() {
        for d in [
            "google-chrome",
            "google-chrome-beta",
            "chromium",
            "microsoft-edge",
            "BraveSoftware/Brave-Browser",
        ] {
            roots.push(base.join(d));
        }
    }
    #[cfg(target_os = "windows")]
    if let Some(base) = dirs::data_local_dir() {
        for d in [
            "Google/Chrome/User Data",
            "Chromium/User Data",
            "Microsoft/Edge/User Data",
            "BraveSoftware/Brave-Browser/User Data",
        ] {
            roots.push(base.join(d));
        }
    }
    roots
}

/// Inline-menu password managers installed in any local Chrome profile, by
/// name. Read from the profiles' `Extensions/<id>` directories: the relay
/// cannot see another extension's frame (webNavigation omits it), so this is
/// the closest it gets to naming the culprit.
pub(crate) fn installed_inline_menu_extensions() -> Vec<&'static str> {
    let mut found: Vec<&'static str> = Vec::new();
    for root in chrome_user_data_dirs() {
        let Ok(profiles) = std::fs::read_dir(&root) else {
            continue;
        };
        for profile in profiles.flatten() {
            let ext = profile.path().join("Extensions");
            for (id, name) in INLINE_MENU_EXTENSIONS {
                if ext.join(id).is_dir() && !found.contains(name) {
                    found.push(name);
                }
            }
        }
    }
    found
}

/// Whether any local Chrome profile has an inline-menu password manager
/// installed. Read once per daemon: it gates a pre-check on every click.
pub(crate) fn inline_menu_manager_installed() -> bool {
    static INSTALLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *INSTALLED.get_or_init(|| !installed_inline_menu_extensions().is_empty())
}

/// What to tell the reader about a tab blocked by another extension's frame.
pub(crate) fn foreign_frame_hint() -> String {
    let installed = installed_inline_menu_extensions();
    let culprit = if installed.is_empty() {
        "another extension's frame (a password manager's inline autofill menu is the usual one)"
            .to_string()
    } else {
        format!(
            "most likely {}'s inline autofill menu (installed in this Chrome), which opens next to a \
             focused login or card field",
            installed.join(" / ")
        )
    };
    format!(
        "\nCause: {culprit}. While that frame is in the tab Chrome refuses every debugger command \
         on it; the tab works again once it closes. It closes when the tab is hidden: for a tab \
         this session created, chrome-use does that itself (a tab switch of well under a second, \
         after which the tab that was in front is in front again). Fill such fields with \
         `fill` (one write) rather than `type --key-events`. To stop it happening, the user can \
         turn off that extension's inline menu for this site."
    )
}

/// Race normal lifecycle waiting against a small number of access checks. A
/// successful check never substitutes for a load event; only a definitive
/// Chrome access denial can end the wait early. Fast pages finish before the
/// first check, and checks that stall cannot hold up the lifecycle future.
async fn wait_with_access_checks<W, C, P>(
    wait: W,
    mut check: C,
    delay: Duration,
) -> Result<(), String>
where
    W: Future<Output = Result<(), String>>,
    C: FnMut() -> P,
    P: Future<Output = Result<(), String>>,
{
    let guard = async {
        tokio::time::sleep(delay).await;
        for _ in 0..3 {
            if let Ok(Err(error)) = tokio::time::timeout(Duration::from_millis(500), check()).await
            {
                if is_debugger_access_denied(&error) {
                    return error;
                }
            }
            tokio::time::sleep(delay).await;
        }
        std::future::pending::<String>().await
    };
    tokio::select! {
        result = wait => result,
        error = guard => Err(error),
    }
}

/// A CDP call that ran to its full time budget without the command promise ever
/// resolving — surfaced as `CDP command timed out: <method>` (see cdp/client.rs).
/// Distinct from a *lifecycle* wait timeout: here the `Page.navigate` command
/// itself never returned (issue #126, heavy server-rendered pages).
pub(crate) fn is_command_timeout_error(error: &str) -> bool {
    error.to_lowercase().contains("command timed out")
}

/// Did a navigation actually commit despite a `Page.navigate` command timeout?
/// True when the tab's live URL is a real page on the target's host — i.e. the
/// nav happened and only the (heavy) load/commit acknowledgement stalled. A tab
/// still on `about:blank`/blank, or on an unrelated host, means the nav never
/// took, so the timeout is a genuine failure.
pub(crate) fn navigation_committed(landed: &str, target: &str) -> bool {
    if landed.is_empty() || landed == "about:blank" {
        return false;
    }
    match (url::Url::parse(landed), url::Url::parse(target)) {
        (Ok(l), Ok(t)) => l.host_str().is_some() && l.host_str() == t.host_str(),
        _ => false,
    }
}

/// Converts common error messages into AI-friendly, actionable descriptions.
pub fn to_ai_friendly_error(error: &str) -> String {
    let lower = error.to_lowercase();
    if lower.contains("tab_initialization_incomplete:") {
        return error.to_string();
    }
    // Preserve the no-replay instruction even when the nested cause is stale or
    // timed out; generic transport recovery guidance could duplicate the action.
    if lower.contains("action_outcome_unknown:") {
        return error.to_string();
    }
    if is_debugger_access_denied(error) {
        if lower.contains("debugger_access_denied:") {
            return error.to_string();
        }
        // The same words come back when the tab was opened by ANOTHER
        // chrome-use session daemon (its extension context, not ours). That
        // reads like a permissions problem and sends people debugging the
        // wrong thing for a long time (#256); the fix is to stop the other
        // daemon.
        return format!("debugger_access_denied: Chrome blocked debugger access to protected extension content in this tab, which can be a child frame. Reattaching does not resolve this restriction. If this session did not open the tab, another chrome-use session daemon may hold it: run `chrome-use sessions`, then `chrome-use session stop <that session>`. Otherwise use `tab inspect` for browser metadata or a separate test profile. Original error: {error}");
    }
    // Top-level `await` in `eval` fails with a bare "await is not defined" /
    // "await is only valid in async" — unhelpful. Point at the wrapper (issue #65).
    if lower.contains("await is not defined")
        || lower.contains("await is only valid")
        || (lower.contains("unexpected") && lower.contains("await"))
    {
        return format!(
            "{error}\nHint: `eval` has no top-level `await` — wrap async code as \
             `(async () => {{ /* await … */ }})()` (the promise is awaited and its value returned)."
        );
    }
    // A read (snapshot/screenshot/eval/get) hit a session whose target is gone —
    // typically a cross-process navigation like an OAuth redirect (issue #58).
    // `navigate` auto-reattaches, but reads deliberately fail loudly rather than
    // silently retarget onto the wrong tab (#8.1). The raw CDP text ("stale
    // sessionId … its tab is gone") tells the agent nothing actionable, so spell
    // out the recovery instead.
    if is_stale_target_error(error) {
        return "the tab this command was driving is gone — it navigated across processes (e.g. \
                an OAuth/SSO redirect), was closed, or the relay lost it. The session did NOT \
                silently retarget, since that could read or click the wrong tab. Run `tab list` \
                first. If the tab is listed as `created` or `adopted`, preserve it with \
                `tab select <ref>`; otherwise use `tab adopt <url-substring|targetId>`. Use \
                `tab inspect <ref>` when its renderer is stuck. \
                Only if it is absent, re-open it with `open <url>` / `navigate <url>`, then \
                re-`snapshot`."
            .to_string();
    }
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
    if lower.contains("relay timeout") {
        return format!(
            "{error}\nHint: Chrome still knows about the tab, but its renderer or debugger did \
             not answer in time. This can happen when page JavaScript blocks the main thread. \
             Use `tab inspect <ref>` for browser-level URL/status metadata without navigating; \
             `tab select <ref>` keeps the same page selected for a screenshot retry. Runtime \
             evaluation cannot complete until the page thread responds."
        );
    }
    // A command deadline alone cannot distinguish an unresponsive renderer
    // from a lost connection. Keep the method and offer target-specific recovery.
    if lower.contains("timed out") {
        // Polling can reach this deadline after false results or failed probes.
        // The deadline alone cannot diagnose transport health.
        if lower.contains("wait timed out after") {
            return format!(
                "{error}\nHint: the condition was not observed within the budget. This timeout \
                 alone does not establish a connection failure. Check the current page and \
                 the wait condition before reconnecting.\n\
                 For `--text`, matching is case-sensitive (`Saved` does not match `saved`). \
                 Use the page's actual wording. If the requested receipt or confirmation is \
                 already visible, do not wait for a second confirmation."
            );
        }
        // A payload-sized command that ran out its (payload-scaled) budget is a
        // different situation: the connection is fine, the command legitimately
        // needed longer than we allowed. Sending that caller to `connect` or to
        // hunt a stale service worker is the wrong direction — reported from
        // live use after a 150 KB insert hit the daemon budget (#301).
        if lower.contains("input.inserttext") {
            return format!(
                "{error}\nHint: this is a size limit, not a dead connection. `Input.insertText` \
                 costs time in proportion to its size (~0.6-1.0s/KB in a rich editor, and that \
                 per-KB cost RISES as the payload grows), and this payload \
                 needed more than the budget. The connection is fine — do NOT reconnect.\n\
                 The insert was NOT cancelled. The page can keep working on it for minutes after \
                 this error, so let the tab go quiet and re-read the field before sending anything \
                 else — some or all of the text may have landed, and a command sent now hits a \
                 renderer that is still busy (#315).\n\
                 If you must send it in pieces, compare the field's CONTENT between pieces, never \
                 just its length: a call returns when Chrome dispatched the insert, not when the \
                 editor committed it, so the next piece can race the uncommitted tail and scramble \
                 the text while preserving the total length (#301)."
            );
        }
        return format!(
            "{error}\nHint: the command did not respond within its budget; this alone does not \
             establish a connection failure. Check `status` and `tab list`. If the target still \
             exists, `tab select <targetId> --activate` can surface it before initialization; \
             this changes the visible tab. Verify a read before continuing. Do not automatically \
             repeat a click or submission: the timed-out action may already have taken effect."
        );
    }
    if lower.contains("timeout") {
        return "Operation timed out. The page may still be loading or the element may not exist."
            .to_string();
    }
    if lower.contains("element not found") || lower.contains("no element") {
        // The resolver already diagnosed the miss (an XPath `text()` gotcha, a
        // placeholder rendered on an inner span, …): keep that verbatim — the old
        // wholesale replacement threw the selector AND the hint away and always
        // blamed a closed shadow root / cross-origin iframe, two rare causes that
        // sent people down the wrong path (issue #202).
        // A refused @ref already names its suggested refs and the refresh.
        if error.contains("Hint:")
            || error.contains("Run `snapshot -i`")
            || error.contains("`snapshot -i` to refresh")
        {
            return error.to_string();
        }
        // Selectors / `find` match the page DOM, which can't see inside a CLOSED
        // shadow root or a cross-origin iframe — but `snapshot -i` (the CDP
        // accessibility tree) pierces both. Also nudge to verify the exact
        // label, since translations differ (real case: LinkedIn's Save button is
        // labelled 收藏, not 保存 — issue #55).
        return format!(
            "{error}\nHint: the selector matched nothing in the page DOM. Check the exact \
             label/text first (translations differ: a \"Save\" button may be labelled 收藏), and \
             for text matching prefer `find \"<label>\"` / `snapshot -i` + @ref over CSS/XPath. \
             If the element lives in a CLOSED shadow root or a cross-origin iframe, selectors/`find` \
             can't reach it — `snapshot -i` pierces both via the accessibility tree."
        );
    }
    error.to_string()
}

/// What a click's new-tab check found: the tab it adopted, or why a tab the
/// click opened was not adopted (reported, never dropped silently).
#[derive(Debug, Default)]
pub struct NewTabCheck {
    pub opened: Option<PageInfo>,
    pub warning: Option<String>,
    /// `"unadopted"`: a tab the click opened was seen and deliberately left
    /// alone. `"unknown"`: the check ran out of its budget waiting for Chrome
    /// or the extension, so whether a tab opened, or got attached, is not
    /// known. `None` when nothing needs saying.
    pub status: Option<&'static str>,
}

impl NewTabCheck {
    fn unadopted(warning: String) -> Self {
        Self {
            opened: None,
            warning: Some(warning),
            status: Some("unadopted"),
        }
    }

    fn unknown(warning: String) -> Self {
        Self {
            opened: None,
            warning: Some(warning),
            status: Some("unknown"),
        }
    }
}

/// How `ABExt.attachTabById`'s answer decides a click's pop-up.
#[derive(Debug, PartialEq, Eq)]
enum RelayPopupVerdict {
    /// Attached, the right tab, and the extension confirmed it as an agent
    /// pop-up (`agentPopup: true`): the session may own it.
    Confirmed(String),
    /// Attached, but not confirmed (`agentPopup` false, missing or not a bool).
    Unconfirmed,
    /// Not attached, another tab, or no target id.
    NotAttached,
}

/// Only an explicit `agentPopup: true` upgrades a pop-up to session-created
/// (closed with `close`, followed by `--follow`). Anything else keeps the
/// tab's unconfirmed identity (#460 review).
fn relay_popup_attach_verdict(resp: &Value, tab_id: i64) -> RelayPopupVerdict {
    let target_id = resp.get("targetId").and_then(Value::as_str);
    let same_tab = resp.get("chromeTabId").and_then(Value::as_i64) == Some(tab_id);
    let attached = resp.get("attached").and_then(Value::as_bool) == Some(true);
    match (target_id, same_tab, attached) {
        (Some(t), true, true) => {
            if resp.get("agentPopup").and_then(Value::as_bool) == Some(true) {
                RelayPopupVerdict::Confirmed(t.to_string())
            } else {
                RelayPopupVerdict::Unconfirmed
            }
        }
        _ => RelayPopupVerdict::NotAttached,
    }
}

/// Chrome tab ids that existed before a click over the relay (#456).
#[derive(Debug, Clone, Default)]
pub struct RelayTabBaseline {
    tab_ids: HashSet<i64>,
}

/// Budget for finding a relay pop-up in chrome.tabs and waiting for it to
/// leave about:blank. Every chrome.tabs read inside it is cut off at the
/// deadline, so this phase never runs past it.
const RELAY_POPUP_FIND_BUDGET: Duration = Duration::from_millis(2_000);
/// Budget for the attach that follows (capability check, attach by tab id,
/// relay session). When it runs out the outcome is reported as unknown: the
/// extension may still attach the tab.
const RELAY_POPUP_ATTACH_BUDGET: Duration = Duration::from_millis(3_000);
/// Extension capability that attaches one tab by Chrome tab id (ab-connect
/// 0.5.30). Without it a relay pop-up is reported, never attached by URL.
const ATTACH_TAB_BY_ID_CAPABILITY: &str = "attachTabById";

/// E2E fail injection: the next `tab_switch` fails right after it moved the
/// pin (see `follow_tab`).
#[cfg(feature = "e2e-tests")]
pub(crate) static FAIL_NEXT_TAB_SWITCH_AFTER_PIN: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// The Chrome tab id a relay session id (`cb-tab-<tabId>`) belongs to.
fn relay_chrome_tab_id(session_id: &str) -> Option<i64> {
    session_id.strip_prefix("cb-tab-")?.parse().ok()
}

/// Whether a click's new tabs may be adopted by diffing the target list (each
/// new target becomes session-created). Only for a launched browser.
fn adopts_new_targets_by_diff(launched: bool) -> bool {
    launched
}

/// Whether the tab a click was dispatched to is one the session created (or a
/// pop-up adopted from one). `None` (unknown) is not.
fn clicked_tab_is_created(active: Option<&str>, created: &HashSet<String>) -> bool {
    active.is_some_and(|t| created.contains(t))
}

/// The Chrome tab ids of the session's pages that it genuinely created (its
/// own tabs and the pop-ups it adopted from them, which are recorded as
/// created). A user tab taken with `tab adopt` is a session page too, but not
/// the session's: a pop-up it opens stays the user's, so it must not count as
/// an opener or group anchor when picking a click's pop-up (#460 review).
fn relay_created_chrome_tabs(pages: &[PageInfo], created: &HashSet<String>) -> HashSet<i64> {
    pages
        .iter()
        .filter(|p| created.contains(&p.target_id))
        .filter_map(|p| relay_chrome_tab_id(&p.session_id))
        .collect()
}

/// The first tab in `tabs` (chrome.tabs.Tab objects) that a click on one of
/// `ours` opened (#456): absent from `before`, not ours, and
///
/// - in the tab group of one of our tabs, or
/// - in no group at all, with one of our tabs as its `openerTabId`.
///
/// A tab in any OTHER group is never ours, whatever its opener says: the group
/// is an explicit ownership claim by another session (or the user), and on a
/// conflict that claim wins. The group is also the signal that holds for a
/// background tab: Chrome adds a link's or `window.open`'s new tab to its
/// source tab's group, while it reports the window's FRONT tab as
/// `openerTabId`. A tab the user opens has no group and an opener that is not
/// ours, so it is never picked either.
fn relay_popup_candidate<'a>(
    tabs: &'a [Value],
    before: &HashSet<i64>,
    ours: &HashSet<i64>,
) -> Option<&'a Value> {
    let id = |t: &Value| t.get("id").and_then(Value::as_i64);
    let group = |t: &Value| t.get("groupId").and_then(Value::as_i64).filter(|g| *g >= 0);
    let our_groups: HashSet<i64> = tabs
        .iter()
        .filter(|t| id(t).is_some_and(|i| ours.contains(&i)))
        .filter_map(group)
        .collect();
    tabs.iter().find(|t| {
        let Some(tab_id) = id(t) else {
            return false;
        };
        if before.contains(&tab_id) || ours.contains(&tab_id) {
            return false;
        }
        match group(t) {
            Some(g) => our_groups.contains(&g),
            None => t
                .get("openerTabId")
                .and_then(Value::as_i64)
                .is_some_and(|o| ours.contains(&o)),
        }
    })
}

/// Whether the extension can attach a tab showing `url` (it refuses blank and
/// privileged pages, so a pop-up still on about:blank is waited for).
fn relay_url_is_attachable(url: &str) -> bool {
    let l = url.to_ascii_lowercase();
    !url.is_empty()
        && ![
            "chrome:",
            "chrome-extension:",
            "devtools:",
            "chrome-untrusted:",
            "edge:",
            "about:",
        ]
        .iter()
        .any(|s| l.starts_with(s))
}

#[derive(Debug, Clone)]
pub struct PageInfo {
    pub tab_id: u32,
    /// Optional user-assigned label (e.g. "docs", "app"). Set via
    /// `tab new --label <name>` or `tab duplicate --label <name>`. Labels are
    /// agent-assigned and never
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

/// Whether the agent should place its tabs in a dedicated agent window instead
/// of the user's active window. **Default: on** — agent tabs go to a separate
/// window in the user's profile so they never clutter the window the user is
/// working in. Opt out via `AGENT_BROWSER_DEDICATED_WINDOW` set to an off value
/// (`0`/`false`/`off`/`user`/`shared`) or the `--window user` flag; unset or any
/// on value (`1`/`true`/`on`/`dedicated`) keeps the default dedicated window.
pub fn dedicated_window_enabled() -> bool {
    std::env::var("AGENT_BROWSER_DEDICATED_WINDOW")
        .map(|v| {
            !matches!(
                v.trim(),
                "0" | "false" | "off" | "no" | "user" | "shared" | "current"
            )
        })
        .unwrap_or(true)
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

    /// Whether this browser was spawned with no window (`--headless` in its
    /// real argv; Lightpanda never has one).
    pub fn spawned_headless(&self) -> bool {
        match self {
            BrowserProcess::Chrome(p) => p.headless,
            BrowserProcess::Lightpanda(_) => true,
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
    /// Target IDs of PRE-EXISTING tabs this session explicitly `adopt`ed (the
    /// user's own, or another session's). These ARE owned for command resolution
    /// (the user asked us to drive them), but unlike `created_targets` they are
    /// NEVER auto-closed on `close()` — they belong to the user. Kept separate so
    /// the "made by us, safe to close" invariant of `created_targets` holds.
    adopted_targets: HashSet<String>,
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

/// Result of delivering files to a page. A zero/unknown live input count is not
/// a failure: React dropzones commonly consume the FileList and clear or replace
/// the input synchronously from their change handler.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UploadOutcome {
    pub attached_count: Option<u64>,
    pub warning: Option<String>,
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
/// How long an existing tab gets to answer before `connect` skips it as hung.
const ADOPT_PROBE_TIMEOUT: Duration = Duration::from_secs(2);
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

/// Another live session's tab is in front of the window an activation would
/// change (#385): bringing ours forward hides theirs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ForegroundConflict {
    pub owner: String,
    pub title: String,
}

impl ForegroundConflict {
    /// The warning when `--force` activated anyway.
    pub fn warning(&self) -> String {
        foreground_conflict_warning(&self.owner, &self.title)
    }

    /// Why an activation without `--force` was refused, and what to do
    /// instead.
    pub fn refusal(&self) -> String {
        foreground_conflict_refusal(&self.owner, &self.title)
    }
}

/// The error for an activation refused because it would hide another
/// session's tab. Agents reached for `--activate` because a click on their
/// background tab "did nothing"; the click had been delivered and the result
/// was only late, so the right move is to wait and re-read.
pub(crate) fn foreground_conflict_refusal(owner: &str, title: &str) -> String {
    let what = if title.is_empty() {
        "a tab".to_string()
    } else {
        format!("a tab (\"{title}\")")
    };
    format!(
        "foreground_in_use: not bringing this tab forward. Session '{owner}' has {what} in \
         front of the same window, and activating yours would hide it, so that session's \
         clicks could start doing nothing. Your tab does not need to be in front to be \
         driven: clicks and typing reach a background tab, and its results just arrive later \
         (a hidden page runs timers about once a second). Wait for the result \
         (`wait --text <expected>`) and `snapshot -i` before repeating anything. If this \
         page really ignores input while hidden, hand the step to the user. Do not override \
         this refusal: session '{owner}' is another agent's work in progress."
    )
}

/// The warning for an activation that hid another session's tab (#385).
pub(crate) fn foreground_conflict_warning(owner: &str, title: &str) -> String {
    let what = if title.is_empty() {
        "its tab".to_string()
    } else {
        format!("its tab \"{title}\"")
    };
    format!(
        "bringing this tab forward hid session '{owner}''s tab in the same window ({what}). \
         A hidden page can ignore clicks, so that session may now see actions do nothing. \
         Use a separate window per session where you need --activate."
    )
}

impl BrowserManager {
    pub async fn launch(options: LaunchOptions, engine: Option<&str>) -> Result<Self, String> {
        if let Some(refusal) = UNIT_TEST_LAUNCH_REFUSAL {
            return Err(refusal.to_string());
        }
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
        // What the window will really be, not what was asked for: the
        // `headless` option is ignored unless AGENT_BROWSER_ALLOW_HEADLESS=1.
        let headless = engine == "lightpanda" || launches_headless(&options);

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
            Some(headless),
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
                adopted_targets: HashSet::new(),
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
            None,
        );
        let client = Arc::new(CdpClient::connect_with_headers(&ws_url, headers).await?);
        let mut manager = Self {
            client,
            browser_process: None,
            ws_url: ws_url.clone(),
            pages: Vec::new(),
            active_page_index: 0,
            default_timeout_ms: 25_000,
            download_path: None,
            ignore_https_errors: false,
            visited_origins: HashSet::new(),
            created_targets: DAEMON_SESSION
                .get()
                .map(|session| crate::connection::read_created_targets(session, &ws_url))
                .unwrap_or_default(),
            adopted_targets: HashSet::new(),
            active_target_id: None,
            relay_target_misses: HashMap::new(),
            relay_scoped: false,
            next_tab_id: 1,
            capture_console: console_capture_enabled(),
        };

        if direct_page {
            manager.adopted_targets.insert("provider-page".to_string());
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
        if !self.via_relay() {
            return self.collect_page_targets().await;
        }
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
        let via_relay = self.via_relay();
        let target = match all.iter().find(|t| t.target_id == spec) {
            Some(t) => t.clone(),
            // On the relay, match URLs only against what Chrome reports for its
            // tabs now. The relay's target list keeps the url each tab had when
            // it was attached, so a tab that has since navigated elsewhere still
            // "matched", and `adopt <baijiahao url>` took an unrelated
            // xiaohongshu tab (#357). The extension resolves the spec against
            // live chrome.tabs metadata and attaches only that one tab — which
            // also covers user tabs we never attached (no debugger banner).
            None if via_relay => self.adopt_by_url_on_demand(spec).await?,
            None => match all.iter().find(|t| url_matches_adopt_spec(&t.url, spec)) {
                Some(t) => t.clone(),
                None => {
                    return Err(format!(
                        "No open tab matching {spec:?}; run `tab list` for available targets"
                    ))
                }
            },
        };

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
        if let Some(index) = self
            .pages
            .iter()
            .position(|page| page.target_id == target.target_id)
        {
            if let Some(page) = self.pages.get_mut(index) {
                page.session_id = attach.session_id;
                page.url = target.url;
                page.title = sanitize_title(&target.title);
            }
            if !self.created_targets.contains(&target.target_id) {
                self.adopted_targets.insert(target.target_id);
            }
            self.active_page_index = index;
            self.pin_active_target();
            return Ok(());
        }
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
        // Mark as owned-for-resolution so the pinned tab resolves in
        // strict_session_index — but via adopted_targets (NOT created_targets),
        // so close() never auto-closes the user's tab.
        self.adopted_targets.insert(target.target_id.clone());
        self.active_page_index = self.pages.len() - 1;
        self.pin_active_target();
        Ok(())
    }

    /// Adopt an existing extension or direct-CDP tab without navigating it.
    ///
    /// Unlike the historical top-level `adopt` command, this does not restart
    /// the daemon, so it preserves the diagnostic state of a white-screen or
    /// unresponsive page (issue #157).
    pub async fn tab_adopt(&mut self, spec: &str) -> Result<Value, String> {
        if spec.trim().is_empty() {
            return Err("Expected a non-empty URL substring or targetId".to_string());
        }
        self.adopt_existing_target(spec).await?;
        self.active_page_info()
            .ok_or_else(|| "Adopted tab could not be resolved".to_string())
    }

    /// Ask the extension to find a pre-existing tab by `spec` (targetId or URL
    /// substring) via `chrome.tabs` metadata and attach ONLY that one, returning
    /// its now-live `TargetInfo`. Needed because the extension no longer eagerly
    /// attaches the user's tabs (keeping the debugger banner off their pages), so
    /// `collect_all_targets` can't see an unattached user tab until we adopt it.
    /// On no match, surfaces the candidate URLs the extension reported.
    async fn adopt_by_url_on_demand(&self, spec: &str) -> Result<TargetInfo, String> {
        let resp: Value = self
            .client
            .send_command_typed("ABExt.adoptByUrl", &json!({ "spec": spec }), None)
            .await
            .map_err(|e| format!("adopt: discovery failed ({e})"))?;

        if let Some(tid) = resp.get("targetId").and_then(|v| v.as_str()) {
            let url = resp
                .get("url")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            // Never adopt a tab the spec does not name. The extension matches on
            // the same url it returns, so this only trips on a build that picks
            // differently — and then refusing beats pinning the user's
            // unrelated tab (#357).
            if tid != spec && !url_matches_adopt_spec(&url, spec) {
                return Err(format!(
                    "adopt: the extension offered {url:?}, which does not match `{spec}`; \
                     nothing was adopted. Run `tab list` and pass the exact targetId."
                ));
            }
            return Ok(TargetInfo {
                target_id: tid.to_string(),
                target_type: "page".to_string(),
                title: resp
                    .get("title")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string(),
                url,
                attached: Some(true),
                browser_context_id: None,
            });
        }

        // No match — list what the extension could see (its chrome.tabs view).
        let profile_info = match (
            resp.get("profileEmail").and_then(|v| v.as_str()),
            resp.get("profileId").and_then(|v| v.as_str()),
        ) {
            (Some(email), Some(id)) => format!(" (profile: {email} / {id})"),
            (Some(email), None) => format!(" (profile: {email})"),
            (None, Some(id)) => format!(" (profile: {id})"),
            (None, None) => String::new(),
        };

        let mut open: Vec<String> = resp
            .get("candidates")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|c| c.get("url").and_then(|u| u.as_str()))
                    .map(|u| u.chars().take(80).collect::<String>())
                    .collect()
            })
            .unwrap_or_default();
        open.sort();
        open.dedup();
        let candidate_list = if open.is_empty() {
            String::new()
        } else {
            format!("\n  {}", open.join("\n  "))
        };
        Err(format!(
            "adopt: no open tab matching `{spec}` (by targetId or URL substring).\n\
             {} tab(s) the extension can see{}:{candidate_list}",
            open.len(),
            profile_info
        ))
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
        if let Some(spec) = pending_adopt_directive() {
            self.adopt_existing_target(&spec).await?;
            ADOPT_DIRECTIVE_DONE.store(true, std::sync::atomic::Ordering::SeqCst);
            return Ok(());
        }

        let page_targets: Vec<TargetInfo> = self.collect_page_targets().await?;

        if page_targets.is_empty() {
            // Create a new tab
            let agent_group = self.agent_group();
            let dedicated_window = self.dedicated_window();
            let result: CreateTargetResult = self
                .client
                .send_command_typed(
                    "Target.createTarget",
                    &CreateTargetParams {
                        url: "about:blank".to_string(),
                        agent_group,
                        background: None,
                        dedicated_window,
                    },
                    None,
                )
                .await?;
            // We created this tab — own it so close() can clean it up.
            self.remember_created_target(&result.target_id);

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
            if scoped {
                let target_ids: Vec<String> = page_targets
                    .iter()
                    .map(|target| target.target_id.clone())
                    .collect();
                register_scoped_target_ownership(
                    &target_ids,
                    &self.created_targets,
                    &mut self.adopted_targets,
                );
            }
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
            // Foreign direct-CDP targets are metadata until explicitly adopted.
            // Probing/initializing one here can block the very command that will
            // activate it, and the session is not allowed to drive it yet.
            if self.browser_process.is_none()
                && !self.via_relay()
                && !self
                    .pages
                    .iter()
                    .any(|page| self.owned_targets().contains(&page.target_id))
            {
                self.active_page_index = 0;
                self.pin_active_target();
                return Ok(());
            }
            // Drive the first tab whose renderer answers. A hung renderer (seen on
            // memory-starved hosts) never replies, so adopting it blindly made the
            // first command wait out a 30s Page.enable and report the tab as gone.
            let mut responsive = None;
            for (index, page) in self.pages.iter().enumerate() {
                // An explicitly attached target can be waiting for the debugger;
                // release it first so a healthy tab is not mistaken for a hung one.
                let _ = tokio::time::timeout(
                    ADOPT_PROBE_TIMEOUT,
                    self.client.send_command_no_params(
                        "Runtime.runIfWaitingForDebugger",
                        Some(&page.session_id),
                    ),
                )
                .await;
                let probe = self.client.send_command(
                    "Runtime.evaluate",
                    Some(json!({ "expression": "1", "returnByValue": true })),
                    Some(&page.session_id),
                );
                if matches!(
                    tokio::time::timeout(ADOPT_PROBE_TIMEOUT, probe).await,
                    Ok(Ok(_))
                ) {
                    responsive = Some(index);
                    break;
                }
            }
            match responsive {
                Some(index) => {
                    self.active_page_index = index;
                    self.pin_active_target();
                    let session_id = self.pages[index].session_id.clone();
                    self.enable_domains(&session_id).await?;
                }
                // Leave the unresponsive tabs alone (they may be the user's) and
                // work in a fresh one.
                None => {
                    self.tab_new(None, None).await?;
                }
            }
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
    fn active_is_drivable(&self) -> bool {
        active_index_is_drivable(
            &self.pages,
            self.active_target_id.as_deref(),
            self.active_page_index,
            &self.owned_targets(),
        )
    }

    /// Whether [`Self::navigate`] would open a fresh owned tab instead of
    /// navigating the active one (a connected browser whose active tab this
    /// session neither created nor adopted).
    pub fn navigate_opens_own_tab(&self) -> bool {
        self.browser_process.is_none() && !self.active_is_drivable()
    }

    /// Targets this session owns for command resolution: tabs it created PLUS
    /// tabs it explicitly adopted. (Adopted tabs are owned-for-driving but, unlike
    /// created ones, never auto-closed — see `adopted_targets`.)
    fn owned_targets(&self) -> HashSet<String> {
        self.created_targets
            .union(&self.adopted_targets)
            .cloned()
            .collect()
    }

    fn persist_created_targets(&self) -> Result<(), String> {
        if self.browser_process.is_none() {
            if let Some(session) = DAEMON_SESSION.get() {
                crate::connection::write_created_targets(
                    session,
                    &self.ws_url,
                    &self.created_targets,
                )
                .map_err(|error| {
                    format!("Failed to persist tab ownership for session {session}: {error}")
                })?;
            }
        }
        Ok(())
    }

    fn remember_created_target(&mut self, target_id: &str) {
        if self.created_targets.insert(target_id.to_string()) {
            if let Err(error) = self.persist_created_targets() {
                eprintln!("{error}");
            }
        }
    }

    fn forget_created_target(&mut self, target_id: &str) -> Result<bool, String> {
        let removed = self.created_targets.remove(target_id);
        if removed {
            if let Err(error) = self.persist_created_targets() {
                self.created_targets.insert(target_id.to_string());
                return Err(error);
            }
        }
        Ok(removed)
    }

    /// Drop the page bound to `session_id` from the tracked list — used when the
    /// relay reports its tab is gone (issue #35) so the stale entry can't keep
    /// resolving as active. Keeps persisted created ownership so a later daemon
    /// can recover an orphan if the relay falsely reported it gone.
    fn drop_page_by_session(&mut self, session_id: &str) {
        let Some(pos) = self.pages.iter().position(|p| p.session_id == session_id) else {
            return;
        };
        let target_id = self.pages[pos].target_id.clone();
        self.pages.remove(pos);
        self.adopted_targets.remove(&target_id);
        if self.active_target_id.as_deref() == Some(target_id.as_str()) {
            self.active_target_id = None;
        }
        self.active_page_index =
            active_page_index_after_removal(self.active_page_index, pos, self.pages.len());
    }

    /// The active page's last-known URL from cached page state — NO CDP round
    /// trip (so it never hangs, even when the relay is dead). Best-effort, "" if
    /// unknown. Used for cheap friction logging where calling `get_url` would
    /// block on exactly the failures we want to record.
    pub fn cached_active_url(&self) -> String {
        let idx = resolve_active_index(
            &self.pages,
            self.active_target_id.as_deref(),
            self.active_page_index,
        );
        self.pages
            .get(idx)
            .map(|p| p.url.clone())
            .unwrap_or_default()
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
            self.browser_process.is_none(),
            &self.owned_targets(),
        )?;
        self.pages
            .get(idx)
            .map(|p| p.session_id.as_str())
            .ok_or_else(|| "No active page".to_string())
    }

    /// Whether this manager is driving the user's real Chrome through the
    /// `ab-connect` extension relay (vs. a launched / direct-CDP browser). Public
    /// mirror of the relay gate used internally (`agent_group().is_some()`), so the
    /// daemon dispatcher can scope relay-only recovery (the stale-session retry)
    /// without reaching into private internals.
    /// For a browser this daemon launched: whether its real argv had
    /// `--headless`. `None` when the browser was not launched here.
    pub fn launched_headless(&self) -> Option<bool> {
        self.browser_process
            .as_ref()
            .map(BrowserProcess::spawned_headless)
    }

    pub fn on_relay(&self) -> bool {
        self.agent_group().is_some()
    }

    /// Non-destructive recovery for a stale relay session (OAuth-popup logins,
    /// issue #58). On the extension relay a cross-process navigation (an OAuth/SSO
    /// redirect, `display=popup` + `response_mode=form_post`) can swap the renderer
    /// and rotate the CDP sessionId while Chrome keeps the SAME `targetId` for the
    /// tab. The cached `cb-tab-<id>` session then goes stale and every command keyed
    /// off it fails ("its tab is gone").
    ///
    /// Re-discover the live target set and re-attach to the SAME pinned
    /// `active_target_id` to pick up its current (rotated) sessionId — WITHOUT
    /// creating a new tab, so an in-flight OAuth opener window (the x.com page about
    /// to receive the id_token via postMessage) is never discarded. The pinned
    /// target is protected from pruning, and on the relay the targetId is stable
    /// across the renderer swap, so this is unambiguous. Errors only when the target
    /// genuinely vanished (closed for real), leaving the caller to fall back.
    ///
    /// Relay-only: off the relay sessions don't rotate this way and the direct-CDP
    /// path has no `agent_group`, so callers gate this on `on_relay()`.
    pub async fn reattach_active_session(&mut self) -> Result<(), String> {
        let target_id = target_id_for_reattach(self.active_target_id.as_deref())?;
        // Refresh the live target set first (updates url/title, drops only
        // genuinely-gone tabs — the pinned active target is debounce-protected, so
        // a single flaky snapshot during the swap can't prune it). Best-effort: a
        // failed resync shouldn't abort the re-attach below.
        self.resync_targets().await.ok();
        // The pin must still resolve to a tracked tab. If resync pruned it the tab
        // is genuinely gone and the caller should fall back to opening a fresh one.
        if !self.pages.iter().any(|p| p.target_id == target_id) {
            return Err(BOUND_TAB_GONE.to_string());
        }
        // Re-attach to the SAME target to obtain its current sessionId. On the relay
        // this is keyed by tabId, so it returns the live `cb-tab-<id>` session even
        // after the renderer swap.
        let attach: AttachToTargetResult = self
            .client
            .send_command_typed(
                "Target.attachToTarget",
                &AttachToTargetParams {
                    target_id: target_id.clone(),
                    flatten: true,
                },
                None,
            )
            .await?;
        // Rebind the tracked page to the fresh session and re-enable domains on it.
        if let Some(page) = self.pages.iter_mut().find(|p| p.target_id == target_id) {
            page.session_id = attach.session_id.clone();
        }
        // Do not synchronously initialize Page/Network domains here. Over the
        // extension relay those page-scoped commands wait on the renderer and
        // can hang for a white-screen tab whose main thread is blocked. The
        // extension arms domains best-effort after registering the stable
        // session; the next requested operation remains the liveness probe.
        Ok(())
    }

    pub async fn navigate(&mut self, url: &str, wait_until: WaitUntil) -> Result<Value, String> {
        // Refuse privileged Chrome pages on the relay BEFORE navigating (#213).
        // The extension cannot attach a debugger to chrome:// / chrome-extension://
        // / devtools://, so driving the session tab there did not fail — it
        // silently made the tab unrecoverable ("the tab this command was driving
        // is gone"), and `tab select` could not revive it; only an AppleScript
        // navigation plus `tab adopt` got it back. A refusal the caller can read
        // is strictly better than a dead tab it has to rescue.
        //
        // Only on the relay: a browser process we launched ourselves is
        // unrestricted, and `chrome://` there is legitimate.
        if self.agent_group().is_some() && is_internal_chrome_target(url) {
            return Err(format!(
                "refusing to navigate the session tab to a privileged Chrome page ({url}). \
                 The extension relay cannot attach to chrome:// / chrome-extension:// / \
                 devtools:// pages, and navigating there leaves the tab unrecoverable. \
                 Open it by hand, or use `tab adopt <url-substring>` to drive a tab that \
                 is already on it."
            ));
        }
        // On the shared real browser (extension relay), a fresh session only
        // passively attached to the user's existing tabs — it doesn't own any. The
        // pre-fix code made one of those the active tab, so the first `open` then
        // navigated (clobbered) the user's page: in dogfooding an `open` replaced a
        // half-filled form with the target site. If the active tab isn't one we
        // created, open our own tab in this session's group and navigate THAT, so
        // the user's (and other sessions') tabs are never hijacked. This applies
        // to raw CDP too; only a browser process we launched is unrestricted.
        if self.navigate_opens_own_tab() {
            self.tab_new(None, None).await?;
        }
        let mut session_id = self.active_session_id()?.to_string();
        let mut lifecycle_rx = self.client.subscribe();
        // Carries a graceful-degradation note when navigation didn't complete
        // cleanly but the page is usable anyway (issues #10, #126). Surfaced to the
        // CLI in the response so the agent knows to expect a still-rendering page.
        let mut nav_warning: Option<String> = None;

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
                // Non-destructive recovery FIRST (issue #58): the relay keeps the
                // targetId stable across a cross-process nav (only the sessionId
                // rotates), so re-attach to the SAME tab and retry the navigate on
                // it. This preserves an in-flight OAuth opener window that the old
                // drop+`tab new` path would have destroyed (killing the popup login
                // mid-handshake). Only if the target is genuinely gone fall back to
                // the original behaviour: drop the dead page and open a fresh owned
                // tab to navigate (issue #35).
                match self.reattach_active_session().await {
                    Ok(()) => {
                        session_id = self.active_session_id()?.to_string();
                    }
                    Err(_) => {
                        self.drop_page_by_session(&session_id);
                        self.tab_new(None, None).await?;
                        session_id = self.active_session_id()?.to_string();
                    }
                }
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
            // The `Page.navigate` CDP command itself timed out (issue #126). On a
            // very large server-rendered page (a huge diff/table/log) Chrome holds
            // the navigate result open until the giant document commits+loads, so
            // the command blows past its budget even though the navigation *did*
            // start. Mirror the #10 fix one layer down: if the tab has actually
            // landed on the target host, degrade to success-with-warning instead of
            // a hard failure; only a nav that never committed still errors.
            Err(e) if is_command_timeout_error(&e) => {
                let landed = self.get_url().await.unwrap_or_default();
                if navigation_committed(&landed, url) {
                    nav_warning = Some(format!(
                        "`Page.navigate` timed out (heavy page) but the tab reached {landed} — \
                         continuing; the document may still be rendering. For read-only \
                         extraction from large pages, `fetch(url)` inside `eval` (or `read`) is a \
                         lighter path than `open`."
                    ));
                    PageNavigateResult {
                        frame_id: String::new(),
                        loader_id: None,
                        error_text: None,
                        relay_fallback: None,
                    }
                } else {
                    return Err(e);
                }
            }
            Err(e) => return Err(e),
        };

        if let Some(fallback) = nav_result.recovery_metadata() {
            nav_warning = Some(format!(
                "The page renderer did not answer `Page.navigate` within the relay budget; \
                 navigation recovered through {} without restarting the session.",
                fallback.method
            ));
        }

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
        if nav_result.loader_id.is_some() && wait_until != WaitUntil::None {
            if let Err(e) = self
                .wait_for_lifecycle(wait_until, &session_id, &mut lifecycle_rx)
                .await
            {
                if is_debugger_access_denied(&e) {
                    return Err(e);
                }
                // The lifecycle event (e.g. `load`) didn't fire within the
                // timeout. On SPAs this is common — a long-pending XHR or a stuck
                // sub-resource holds `load` open long after the DOM is interactive
                // and the page is usable, so `open` would hard-fail even though
                // eval/screenshot work immediately (issue #10). If the DOM is
                // already ready, treat navigation as done (with a warning, carried
                // in the response so the CLI can surface it) instead of failing.
                // Only a still-loading document is a real failure.
                let ready = match self.evaluate_simple("document.readyState").await {
                    Err(cause) if is_debugger_access_denied(&cause) => return Err(cause),
                    result => result
                        .ok()
                        .and_then(|v| v.as_str().map(str::to_string))
                        .unwrap_or_default(),
                };
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

        // Genuine timeout recovery must not immediately issue two more renderer
        // reads and turn an 8-second recovery into roughly 24 seconds. Normal
        // browser-level navigation, like direct CDP, reads the final URL/title
        // after the lifecycle wait rather than returning pre-redirect metadata.
        let (page_url, title) = match nav_result.recovery_metadata() {
            Some(fallback) => (
                if fallback.url.is_empty() {
                    url.to_string()
                } else {
                    fallback.url.clone()
                },
                sanitize_title(&fallback.title),
            ),
            None => {
                let current_url = self.get_url().await;
                if let Err(error) = &current_url {
                    if is_debugger_access_denied(error) {
                        return Err(error.clone());
                    }
                }
                let title = self.get_title().await;
                if let Err(error) = &title {
                    if is_debugger_access_denied(error) {
                        return Err(error.clone());
                    }
                }
                (
                    current_url.unwrap_or_else(|_| url.to_string()),
                    title.unwrap_or_default(),
                )
            }
        };

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
        let wait = self.wait_for_lifecycle_event(wait_until, session_id, rx);
        if self.on_relay() && wait_until != WaitUntil::None {
            wait_with_access_checks(
                wait,
                || async {
                    self.client
                        .send_command_no_params("DOM.enable", Some(session_id))
                        .await
                        .map(|_| ())
                },
                Duration::from_millis(400),
            )
            .await
        } else {
            wait.await
        }
    }

    async fn wait_for_lifecycle_event(
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
        self.eval_in_context(script, &session_id, None).await
    }

    /// Evaluate `script` in a specific frame's context (issue #58 `eval --frame`).
    ///
    /// - **Out-of-process iframe** (cross-origin; has its own auto-attached session
    ///   in `iframe_sessions`): run on that session, which lands in the frame's own
    ///   **main world** — exactly what you want for a cross-origin embed like a
    ///   Google account-picker.
    /// - **Same-process (in-page) frame**: there is no stealth-safe way to reach the
    ///   frame's main world without `Runtime.enable`, so we run in a per-call
    ///   **isolated world** via `Page.createIsolatedWorld`. The DOM is fully
    ///   readable there, but page JS globals are not visible (same trade-off
    ///   `get text --all-frames` already makes).
    pub async fn evaluate_in_frame(
        &self,
        script: &str,
        frame_id: &str,
        iframe_sessions: &HashMap<String, String>,
    ) -> Result<Value, String> {
        if let Some(oopif_session) = iframe_sessions.get(frame_id) {
            return self.eval_in_context(script, oopif_session, None).await;
        }

        let session_id = self.active_session_id()?.to_string();
        let ctx: Value = self
            .client
            .send_command(
                "Page.createIsolatedWorld",
                Some(json!({ "frameId": frame_id, "worldName": "chrome_use_eval" })),
                Some(&session_id),
            )
            .await?;
        let ctx_id = ctx
            .get("executionContextId")
            .and_then(|c| c.as_i64())
            .ok_or_else(|| {
                format!("Could not resolve an execution context for frame {frame_id}")
            })?;
        self.eval_in_context(script, &session_id, Some(ctx_id))
            .await
    }

    /// Shared core of `eval`: build the `Runtime.evaluate` params, send on the
    /// given session (optionally pinned to `context_id`), and unwrap the result.
    async fn eval_in_context(
        &self,
        script: &str,
        session_id: &str,
        context_id: Option<i64>,
    ) -> Result<Value, String> {
        // `replMode: true` lets successive `eval`s re-declare top-level
        // `let`/`const` instead of throwing "Identifier 'x' has already been
        // declared" (issue #38 — independent `eval` steps in a test suite collided
        // in the page's shared lexical scope). BUT replMode and `awaitPromise` are
        // mutually exclusive in Chrome: under replMode a returned promise is NOT
        // awaited (it serialises to `{}`), which breaks `fetch(...).then(...)` and
        // every other async eval. So enable replMode ONLY for synchronous scripts
        // that declare a top-level `let`/`const`; promise-returning scripts keep
        // `awaitPromise` (no replMode) — exactly the pre-#38 behaviour.
        let mentions_async = script_may_return_promise(script);
        let declares = script.contains("let ") || script.contains("const ");
        let repl_mode = declares && !mentions_async;
        let mut params = json!({
            "expression": script,
            "returnByValue": true,
            "awaitPromise": !repl_mode,
            "replMode": repl_mode,
        });
        if let Some(cid) = context_id {
            // `contextId` and `replMode` are mutually exclusive in Chrome; when we
            // pin a frame context, drop replMode (a fresh isolated world per call
            // can't collide on re-declared top-level bindings anyway).
            params["replMode"] = json!(false);
            params["awaitPromise"] = json!(true);
            params["contextId"] = json!(cid);
        }
        let result: EvaluateResult = self
            .client
            .send_command_typed("Runtime.evaluate", &params, Some(session_id))
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
        // Subscribe before probing so a lifecycle event that fires between
        // the probe and the wait cannot be missed.
        let mut rx = self.client.subscribe();

        // `wait_for_lifecycle` waits for the NEXT lifecycle event: right
        // mid-navigation, wrong for a standalone `wait --load` on a page that
        // already finished loading (or navigated client-side, which fires no
        // new load event), where it burned the whole timeout. Resolve at once
        // when the document is already there. The probe is bounded: a frozen
        // relay tab may not answer, and then we wait for the event as before.
        // (After upstream vercel-labs/agent-browser #1554.)
        // Navigation timing records when each event FINISHED: readyState
        // turns 'interactive' before deferred scripts run and DOMContentLoaded
        // fires, so it cannot stand in for the event. Without an entry
        // (about:blank), 'complete' means both events are done.
        let already_reached = match wait_until {
            WaitUntil::Load => Some(
                "(() => { const n = performance.getEntriesByType('navigation')[0]; \
                 return n ? n.loadEventEnd > 0 : document.readyState === 'complete'; })()",
            ),
            WaitUntil::DomContentLoaded => Some(
                "(() => { const n = performance.getEntriesByType('navigation')[0]; \
                 return n ? n.domContentLoadedEventEnd > 0 : document.readyState === 'complete'; })()",
            ),
            WaitUntil::NetworkIdle | WaitUntil::None => None,
        };
        if let Some(expression) = already_reached {
            let probe = tokio::time::timeout(
                Duration::from_secs(2),
                self.client.send_command_typed::<_, EvaluateResult>(
                    "Runtime.evaluate",
                    &EvaluateParams {
                        expression: expression.to_string(),
                        return_by_value: Some(true),
                        await_promise: Some(false),
                    },
                    Some(session_id),
                ),
            )
            .await;
            if let Ok(Ok(result)) = probe {
                if result.result.value.as_ref().and_then(|v| v.as_bool()) == Some(true) {
                    return Ok(());
                }
            }
        }

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
            // group) every time a session explicitly ends or the daemon shuts
            // down. `created_targets` only holds tabs we made via
            // Target.createTarget or native duplicate — never the user's
            // existing tabs or other sessions' — so this is always safe.
            // Best-effort per tab.
            //
            // Concurrently, under one deadline (issue #192). This used to await
            // each close in turn, which loses tabs two ways: the relay's
            // per-command budget is 8s (RELAY_COMMAND_TIMEOUT_MS in
            // relay-timeout.js), so ONE wedged tab could outlast the caller's
            // whole grace period and strand every tab behind it in the queue;
            // and `session stop` force-kills us shortly after SIGTERM, so a
            // sequential walk over N tabs never finished N round trips. Firing
            // them together means one stuck tab costs only itself, and the total
            // stays inside the shutdown budget the caller allows us.
            if !self.created_targets.is_empty() {
                let remaining_before_cleanup = self.created_targets.len();
                close_created_targets(&self.client, &mut self.created_targets).await;
                if self.created_targets.len() != remaining_before_cleanup {
                    if let Err(error) = self.persist_created_targets() {
                        eprintln!("{error}");
                    }
                }
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
    /// and idle-shutdown (the agent is leaving it for the user). Returns whether it
    /// was owned, and fails if the durable ownership record cannot be updated.
    /// Used by `keep`.
    pub fn unown_target(&mut self, target_id: &str) -> Result<bool, String> {
        self.forget_created_target(target_id)
    }

    /// Re-take a tab this session created and later released with `keep`.
    ///
    /// The caller MUST establish that we created it — `handle_keep` does that by
    /// requiring a `kept-tabs` record, which is only ever written for a target
    /// that was owned at the time it was kept. Re-owning on any other basis
    /// would mint deletion rights over a tab we never created, which is the
    /// v1.5.95 shape: rights must be restored from a record, never inferred
    /// from the fact that a tab happens to be active right now.
    ///
    /// Returns whether this call changed anything (false = already owned).
    pub fn reown_target(&mut self, target_id: &str) -> Result<bool, String> {
        if self.created_targets.contains(target_id) {
            return Ok(false);
        }
        self.created_targets.insert(target_id.to_string());
        if let Err(error) = self.persist_created_targets() {
            self.created_targets.remove(target_id);
            return Err(error);
        }
        Ok(true)
    }

    /// The active tab, when it is one this session may hand to its successor
    /// across an upgrade restart: on the user's Chrome (a launched browser
    /// closes with the daemon), and created or adopted by this session.
    pub fn handoff_target(&self) -> Option<String> {
        if self.browser_process.is_some() {
            return None;
        }
        let target_id = self.active_target_id().ok()?.to_string();
        self.owned_targets()
            .contains(&target_id)
            .then_some(target_id)
    }

    /// After an upgrade restart, make `target_id` the active tab again — but
    /// only a tab this session owns: one discovery found in its own relay group
    /// (created, or adopted by it before), or one its persisted record says it
    /// created. Never a foreign tab. Returns whether the tab is now active.
    pub async fn resume_owned_target(&mut self, target_id: &str) -> Result<bool, String> {
        if self.browser_process.is_some() {
            return Ok(false);
        }
        let index = self.pages.iter().position(|p| p.target_id == target_id);
        match index {
            Some(index) if self.owned_targets().contains(target_id) => {
                let already = self.active_target_id.as_deref() == Some(target_id);
                if !already {
                    self.tab_switch(index).await?;
                }
                self.close_leftover_blank_scratch(target_id).await;
                Ok(true)
            }
            Some(_) => Ok(false),
            // Discovery can miss a live tab in a flaky relay snapshot. Re-adopt
            // it by exact id only when we hold the right to it.
            None if self.created_targets.contains(target_id) => {
                match self.adopt_existing_target(target_id).await {
                    Ok(()) => {
                        let session_id = self.active_session_id()?.to_string();
                        self.enable_domains(&session_id).await?;
                        // Discovery opened a scratch tab when it missed ours.
                        self.close_leftover_blank_scratch(target_id).await;
                        Ok(true)
                    }
                    Err(_) => Ok(false),
                }
            }
            None => Ok(false),
        }
    }

    /// Returns true if this manager was connected via CDP (as opposed to local launch).
    pub fn is_cdp_connection(&self) -> bool {
        self.browser_process.is_none()
    }

    /// The WebSocket URL this manager is connected to. Lets `handle_launch`
    /// compare an explicitly requested `connect <port|url>` endpoint against
    /// the connection it already holds (e.g. the auto-connect relay).
    pub fn ws_url(&self) -> &str {
        &self.ws_url
    }

    /// Ensures the browser has at least one page. If `pages` is empty, creates a new
    /// about:blank page and attaches to it.
    pub async fn ensure_page(&mut self) -> Result<(), String> {
        if !self.pages.is_empty() {
            return Ok(());
        }

        let agent_group = self.agent_group();
        let dedicated_window = self.dedicated_window();
        let result: CreateTargetResult = self
            .client
            .send_command_typed(
                "Target.createTarget",
                &CreateTargetParams {
                    url: "about:blank".to_string(),
                    agent_group,
                    background: None,
                    dedicated_window,
                },
                None,
            )
            .await?;
        // We created this tab — own it so close() can clean it up.
        self.remember_created_target(&result.target_id);

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

    /// The tab this session is pinned to, as `(handle, url)` — for saying which
    /// page a failure was actually about.
    ///
    /// A `debugger_access_denied` reads as a permissions bug and gives the
    /// reader nothing to check (issue #217). Naming the pinned tab and its url
    /// settles it either way: a `chrome-extension://` url IS the cause, and the
    /// expected page rules out "the relay followed the user's active tab" and
    /// sends the next report somewhere useful.
    pub fn pinned_tab_summary(&self) -> Option<(String, String)> {
        let pinned = self.active_target_id.as_deref()?;
        let page = self.pages.iter().find(|p| p.target_id == pinned)?;
        Some((format_tab_id(page.tab_id), page.url.clone()))
    }

    /// One allow-listed `chrome.*` call through the relay (`ABExt.call`),
    /// returning Chrome's result. Needs no debugger access.
    /// Whether Chrome window `window_id` holds a tab the relay does not own
    /// (the user's own tab). `true` when it cannot tell: guessing "agent-only"
    /// is how a recovery ends up switching the user's tab.
    async fn window_holds_user_tabs(&self, window_id: i64) -> bool {
        let Ok(tabs) = self
            .chrome_call("tabs", "query", json!([{ "windowId": window_id }]))
            .await
        else {
            return true;
        };
        let Ok(ext_state) = self.client.send_command("ABExt.state", None, None).await else {
            return true;
        };
        let owned: HashSet<i64> = ext_state
            .get("ownedTabs")
            .and_then(Value::as_array)
            .map(|ids| ids.iter().filter_map(Value::as_i64).collect())
            .unwrap_or_default();
        window_has_unowned_tab(&tabs, &owned)
    }

    async fn chrome_call(
        &self,
        namespace: &str,
        method: &str,
        args: Value,
    ) -> Result<Value, String> {
        let out: Value = self
            .client
            .send_command(
                "ABExt.call",
                Some(json!({ "namespace": namespace, "method": method, "args": args })),
                None,
            )
            .await?;
        Ok(out.get("result").cloned().unwrap_or(Value::Null))
    }

    /// Whether Chrome refuses debugger commands on the pinned tab right now
    /// (#373). One no-op evaluate; `false` when it cannot tell.
    pub async fn pinned_tab_blocked(&self) -> bool {
        let Ok(session_id) = self.active_session_id().map(str::to_string) else {
            return false;
        };
        match self
            .client
            .send_command(
                "Runtime.evaluate",
                Some(json!({ "expression": "0", "returnByValue": true })),
                Some(&session_id),
            )
            .await
        {
            Err(e) => is_debugger_access_denied(&e),
            Ok(_) => false,
        }
    }

    /// Close another extension's frame by hiding the pinned tab for a moment
    /// (#373). A password manager's inline menu blocks every debugger command
    /// while it is open, and closes when its page is hidden (Bitwarden:
    /// `visibilitychange` to `hidden` force-closes it). `chrome.tabs` can do that
    /// with no debugger command, the only kind still refused.
    ///
    /// Only for a tab this session created. A blank tab of our own is put next
    /// to the tab in front of the pinned tab's window and shown for a moment:
    ///
    /// - pinned tab in front: the blank tab hides it, the focused field is
    ///   blurred while it is hidden (so the menu does not reopen when it is
    ///   shown), and the pinned tab comes back;
    /// - pinned tab in the background: a background tab is already hidden, so it
    ///   is shown for a moment and then hidden by the blank tab, which sits where
    ///   the user's tab was; closing it hands the front back to that tab. The
    ///   user sees a flash of well under a second and ends on the tab they were
    ///   on.
    ///
    /// Before #373's fix the blank tab was never shown: the relay creates every
    /// tab inactive (the CLI's `background: false` is ignored), so the tab
    /// in front never changed and the menu stayed open.
    ///
    /// Returns a note for the caller's warning when the front tab could not be
    /// put back exactly; errors when nothing was tried or the menu stayed open.
    ///
    /// `blur_focused: false` (a key press that must reach the focused field):
    /// in the background the field keeps focus, since the tab ends hidden. In
    /// front the tab is shown again, and a focused login field reopens the menu
    /// at once (#449: `press` / `keyboard type` stayed blocked), so focus leaves
    /// the field while the tab is hidden and goes back to it at the end, right
    /// before the command.
    ///
    /// A tab in front whose menu is open again after that gets one more try
    /// with a longer hidden moment: a sign-in widget (Apple's, in an iframe)
    /// puts the cursor back into its field when the tab is shown.
    pub async fn cycle_pinned_tab_visibility(
        &mut self,
        blur_focused: bool,
    ) -> Result<Option<String>, String> {
        match self
            .cycle_pinned_tab_visibility_once(blur_focused, false)
            .await
        {
            Err(e) if e == MENU_STILL_OPEN_IN_FRONT => self
                .cycle_pinned_tab_visibility_once(blur_focused, true)
                .await
                .map_err(|e| {
                    if e == MENU_STILL_OPEN_IN_FRONT {
                        format!(
                            "{MENU_STILL_OPEN}, twice. Either the page puts the cursor back \
                             into its login field whenever the tab is shown, which reopens the \
                             menu, or the frame is the password manager's \"save login?\" bar \
                             (it appears after a sign-in and does not close when the tab is \
                             hidden; the user closes it with its X or Save)"
                        )
                    } else {
                        e
                    }
                }),
            other => other,
        }
    }

    async fn cycle_pinned_tab_visibility_once(
        &mut self,
        blur_focused: bool,
        second_try: bool,
    ) -> Result<Option<String>, String> {
        if !self.on_relay() {
            return Err("not on the extension relay".to_string());
        }
        let pinned = self.active_target_id.clone().ok_or("no pinned tab")?;
        if !self.created_targets.contains(&pinned) {
            return Err(
                "the pinned tab was not created by this session, and chrome-use \
                        does not switch the user's own tabs"
                    .to_string(),
            );
        }
        let page = self
            .pages
            .iter()
            .find(|p| p.target_id == pinned)
            .ok_or("pinned tab not tracked")?;
        let session_id = page.session_id.clone();
        let live: Value = self
            .client
            .send_command_typed(
                "ABExt.inspectTab",
                &json!({ "sessionId": page.session_id, "targetId": page.target_id }),
                None,
            )
            .await?;
        let chrome_tab = live
            .get("chromeTabId")
            .and_then(Value::as_i64)
            .ok_or("no Chrome tab id")?;
        let window_id = live
            .get("windowId")
            .and_then(Value::as_i64)
            .ok_or("no Chrome window id")?;
        let in_front = live.get("active").and_then(Value::as_bool) == Some(true);
        // In front, focus that must stay in the field leaves it while the tab
        // is hidden and is put back at the end (see above).
        let refocus = in_front && !blur_focused;
        let window = self
            .chrome_call("windows", "get", json!([window_id]))
            .await
            .unwrap_or(Value::Null);
        if window.get("state").and_then(Value::as_str) == Some("minimized") {
            return Err(
                "its window is minimized, so switching tabs in it hides nothing".to_string(),
            );
        }
        // The flip below inserts a blank tab next to the user's front tab and
        // switches tabs in this window. That is only acceptable in a window that
        // holds nothing but agent tabs (the background agent window). In a
        // window with the user's own tabs it would grab their tab, even for
        // 150ms, so leave it to the user.
        if self.window_holds_user_tabs(window_id).await {
            return Err(USER_WINDOW_MENU.to_string());
        }
        let front = self
            .chrome_call(
                "tabs",
                "query",
                json!([{ "active": true, "windowId": window_id }]),
            )
            .await?;
        let front = front
            .as_array()
            .and_then(|tabs| tabs.first())
            .cloned()
            .ok_or("no tab is in front of its window")?;
        let front_tab = front
            .get("id")
            .and_then(Value::as_i64)
            .ok_or("no front tab id")?;
        let front_index = front.get("index").and_then(Value::as_i64).unwrap_or(0);
        // Activating a tab expands its collapsed group; put that back after.
        let collapsed_group =
            match live_group(&self.chrome_call("tabs", "get", json!([chrome_tab])).await) {
                Some(group) => {
                    let info = self
                        .chrome_call("tabGroups", "get", json!([group]))
                        .await
                        .unwrap_or(Value::Null);
                    (info.get("collapsed").and_then(Value::as_bool) == Some(true)).then_some(group)
                }
                None => None,
            };

        let created: Value = self
            .client
            .send_command(
                "Target.createTarget",
                // In the agent window, never the user's active window: the
                // extension otherwise creates it wherever the user is.
                Some(json!({ "url": "about:blank", "background": false, "dedicatedWindow": true })),
                None,
            )
            .await?;
        // Owned from the start, so `close` cleans it up if closing it here
        // fails; released only once Chrome confirms it is gone.
        let temp = created
            .get("targetId")
            .and_then(Value::as_str)
            .map(str::to_string)
            .ok_or("the blank tab has no target id")?;
        self.remember_created_target(&temp);
        let temp_tab = self
            .client
            .send_command_typed::<_, Value>("ABExt.inspectTab", &json!({ "targetId": temp }), None)
            .await
            .ok()
            .and_then(|v| v.get("chromeTabId").and_then(Value::as_i64));

        let flipped = match temp_tab {
            Some(temp_tab) => {
                self.flip_visibility(
                    chrome_tab,
                    temp_tab,
                    window_id,
                    front_index,
                    in_front,
                    (blur_focused || refocus).then_some(session_id.as_str()),
                    refocus,
                    if second_try { 900 } else { 300 },
                )
                .await
            }
            None => Err("the blank tab has no Chrome tab id".to_string()),
        };

        let closed = self
            .client
            .send_command(
                "Target.closeTarget",
                Some(json!({ "targetId": temp })),
                None,
            )
            .await
            .ok()
            .and_then(|v| v.get("success").and_then(Value::as_bool))
            .unwrap_or(false);
        if closed {
            let _ = self.forget_created_target(&temp);
        }
        flipped?;

        let mut note = None;
        if in_front {
            // Bitwarden re-inserts its overlay frame for a moment when the tab is
            // shown again, even with no field focused; a command sent at once hits
            // it. Let that settle before the caller repeats anything (#373).
            tokio::time::sleep(Duration::from_millis(700)).await;
        } else {
            // Closing the blank tab hands the front to the tab now at its index:
            // the one the user was on. Check, and say so if Chrome chose another.
            tokio::time::sleep(Duration::from_millis(120)).await;
            let now = self
                .chrome_call(
                    "tabs",
                    "query",
                    json!([{ "active": true, "windowId": window_id }]),
                )
                .await
                .ok()
                .and_then(|v| v.as_array().and_then(|t| t.first()).cloned());
            let now_id = now
                .as_ref()
                .and_then(|t| t.get("id"))
                .and_then(Value::as_i64);
            if now_id.is_some() && now_id != Some(front_tab) {
                let back = self
                    .chrome_call("tabs", "update", json!([front_tab, { "active": true }]))
                    .await;
                if back.is_err() {
                    let title = now
                        .as_ref()
                        .and_then(|t| t.get("title"))
                        .and_then(Value::as_str)
                        .map(sanitize_title)
                        .unwrap_or_default();
                    note = Some(format!(
                        "Chrome put a different tab in front of that window afterwards \
                         (\"{title}\") instead of the one that was there (Chrome tab \
                         {front_tab}); tell the user if it matters"
                    ));
                }
            }
            if let Some(session) = blur_focused.then_some(session_id.as_str()) {
                // The pinned tab is hidden again; take focus out of the field so
                // the menu does not reopen the next time it is shown.
                tokio::time::sleep(Duration::from_millis(150)).await;
                self.blur_focused_field(session, false).await;
            }
        }
        if let Some(group) = collapsed_group {
            let _ = self
                .chrome_call("tabGroups", "update", json!([group, { "collapsed": true }]))
                .await;
        }
        // The menu closes asynchronously (a message round trip inside the
        // password manager). Give it a moment before calling it a failure.
        for _ in 0..6 {
            if !self.pinned_tab_blocked().await {
                if refocus {
                    self.refocus_marked_field(&session_id).await;
                }
                return Ok(note);
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
        // Hiding the tab closes the menu only when the page was visible to
        // begin with. Chrome on macOS reports every tab of a window that
        // another app covers (or that sits on another Space) as hidden, so
        // with the user working in another app the switches change nothing
        // (#449: the agent's terminal was in front of Chrome).
        let window_focused = self
            .chrome_call("windows", "get", json!([window_id]))
            .await
            .ok()
            .and_then(|w| w.get("focused").and_then(Value::as_bool));
        if window_focused == Some(false) {
            return Err(MENU_COVERED_WINDOW.to_string());
        }
        Err(if in_front {
            MENU_STILL_OPEN_IN_FRONT
        } else {
            MENU_STILL_OPEN
        }
        .to_string())
    }

    /// The tab switches of [`Self::cycle_pinned_tab_visibility`]; the caller
    /// closes the blank tab whatever happens here.
    #[allow(clippy::too_many_arguments)]
    async fn flip_visibility(
        &self,
        chrome_tab: i64,
        temp_tab: i64,
        window_id: i64,
        front_index: i64,
        in_front: bool,
        blur_session: Option<&str>,
        remember_focus: bool,
        hidden_ms: u64,
    ) -> Result<(), String> {
        let activate = |tab: i64| json!([tab, { "active": true }]);
        // In front: right of the pinned tab. In the background: where the
        // user's tab is, so that closing the blank tab returns the front to it.
        let index = if in_front {
            front_index + 1
        } else {
            front_index
        };
        self.chrome_call(
            "tabs",
            "move",
            json!([temp_tab, { "windowId": window_id, "index": index }]),
        )
        .await?;
        if in_front {
            self.chrome_call("tabs", "update", activate(temp_tab))
                .await?;
            tokio::time::sleep(Duration::from_millis(hidden_ms)).await;
            // While the tab is hidden the menu's frame is gone and debugger
            // commands work again. Take focus out of the field it was attached
            // to: Bitwarden reopens its menu on a focused login field as soon
            // as the tab is shown, so the repeat hit the same block (#373).
            // The value typed so far stays.
            if let Some(session) = blur_session {
                self.blur_focused_field(session, remember_focus).await;
            }
            self.chrome_call("tabs", "update", activate(chrome_tab))
                .await?;
        } else {
            // Shown, then hidden: the hide is what closes the menu.
            self.chrome_call("tabs", "update", activate(chrome_tab))
                .await?;
            tokio::time::sleep(Duration::from_millis(150)).await;
            self.chrome_call("tabs", "update", activate(temp_tab))
                .await?;
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        Ok(())
    }

    /// Take focus out of the focused field: in the page, and in its
    /// same-process child frames, where a sign-in form in an iframe keeps its
    /// own focused field (#449). With `remember`, the field is marked for
    /// [`Self::refocus_marked_field`].
    async fn blur_focused_field(&self, session_id: &str, remember: bool) {
        let mark = if remember {
            "if (a.tagName !== 'IFRAME' && a.tagName !== 'FRAME') a.setAttribute('data-cu-refocus', '1');"
        } else {
            ""
        };
        let expression = format!(
            "(() => {{ const a = document.activeElement; \
             if (a && a !== document.body && a.blur) {{ {mark} a.blur(); }} }})()"
        );
        // Frames first: the page's own blur moves focus off the frame element.
        for context in self.child_frame_worlds(session_id).await {
            let _ = self
                .client
                .send_command(
                    "Runtime.evaluate",
                    Some(json!({ "expression": expression, "contextId": context })),
                    Some(session_id),
                )
                .await;
        }
        let _ = self
            .client
            .send_command(
                "Runtime.evaluate",
                Some(json!({ "expression": expression })),
                Some(session_id),
            )
            .await;
    }

    /// Put focus back on the field [`Self::blur_focused_field`] marked, in the
    /// page or in a child frame. Last, right before the command that needs it:
    /// the menu reopens on it a moment later.
    async fn refocus_marked_field(&self, session_id: &str) {
        let expression = "(() => { const el = document.querySelector('[data-cu-refocus]'); \
            if (!el) return false; el.removeAttribute('data-cu-refocus'); \
            el.focus({ preventScroll: true }); return true; })()";
        let found = self
            .client
            .send_command(
                "Runtime.evaluate",
                Some(json!({ "expression": expression, "returnByValue": true })),
                Some(session_id),
            )
            .await
            .ok()
            .and_then(|v| v.pointer("/result/value").and_then(Value::as_bool))
            == Some(true);
        if found {
            return;
        }
        for context in self.child_frame_worlds(session_id).await {
            let found = self
                .client
                .send_command(
                    "Runtime.evaluate",
                    Some(json!({ "expression": expression, "contextId": context, "returnByValue": true })),
                    Some(session_id),
                )
                .await
                .ok()
                .and_then(|v| v.pointer("/result/value").and_then(Value::as_bool))
                == Some(true);
            if found {
                return;
            }
        }
    }

    /// An isolated world in each same-process child frame of the page (at most
    /// a handful), to reach a field focused inside a frame.
    async fn child_frame_worlds(&self, session_id: &str) -> Vec<i64> {
        let Ok(tree) = self
            .client
            .send_command_no_params("Page.getFrameTree", Some(session_id))
            .await
        else {
            return Vec::new();
        };
        let mut frames = Vec::new();
        super::element::flatten_frame_tree(&tree["frameTree"], true, &mut frames);
        let mut worlds = Vec::new();
        for (frame_id, _, is_top) in frames.into_iter().take(9) {
            if is_top {
                continue;
            }
            if let Some(id) = self
                .client
                .send_command(
                    "Page.createIsolatedWorld",
                    Some(json!({ "frameId": frame_id, "worldName": "chrome_use_focus" })),
                    Some(session_id),
                )
                .await
                .ok()
                .and_then(|v| v.get("executionContextId").and_then(Value::as_i64))
            {
                worlds.push(id);
            }
        }
        worlds
    }

    /// Like `pinned_tab_summary`, but with the url Chrome reports for the tab
    /// right now. While debugger access is blocked the daemon receives no page
    /// events, so its cached url stays on the page where the block began. After
    /// the user logs in and the tab moves on, the note kept naming the old page
    /// (#357). `chrome.tabs` metadata needs no debugger access, so ask it and
    /// refresh the cache. Falls back to the cached url when it cannot ask.
    pub async fn live_pinned_tab_summary(&mut self) -> Option<(String, String)> {
        let pinned = self.active_target_id.clone()?;
        let index = self.pages.iter().position(|p| p.target_id == pinned)?;
        if self.on_relay() {
            let page = &self.pages[index];
            let live: Option<Value> = self
                .client
                .send_command_typed(
                    "ABExt.inspectTab",
                    &json!({ "sessionId": page.session_id, "targetId": page.target_id }),
                    None,
                )
                .await
                .ok();
            if let Some(url) = live
                .as_ref()
                .and_then(|v| v.get("url"))
                .and_then(|v| v.as_str())
                .filter(|u| !u.is_empty())
            {
                let title = live
                    .as_ref()
                    .and_then(|v| v.get("title"))
                    .and_then(|v| v.as_str())
                    .map(sanitize_title);
                let page = &mut self.pages[index];
                page.url = url.to_string();
                if let Some(title) = title {
                    page.title = title;
                }
            }
        }
        let page = &self.pages[index];
        Some((format_tab_id(page.tab_id), page.url.clone()))
    }

    /// Which tab this observation is about: `(tabId, targetId, url)`.
    ///
    /// Deliberately no account or profile email. The point is only "is this
    /// still the same target as last time" — a caller that cannot tell reuses
    /// refs from one page against another, which is how a rebinding or a
    /// navigation turns into a mis-click (issue #237).
    pub fn observed_target(&self) -> Option<(String, String, String)> {
        let i = self.resolved_active_index();
        let page = self.pages.get(i)?;
        Some((
            format_tab_id(page.tab_id),
            page.target_id.clone(),
            page.url.clone(),
        ))
    }

    pub fn tab_list(&self) -> Vec<Value> {
        let active = self.resolved_active_index();
        let pinned = self.active_target_id.as_deref();
        let enforce_pin = self.agent_group().is_some();
        self.pages
            .iter()
            .enumerate()
            .map(|(i, p)| {
                let mut tab = json!({
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
                    // A dangling relay pin is a deliberate tombstone after the
                    // bound tab disappears. Do not render a surviving scratch
                    // tab as active merely because the lenient index fallback
                    // points there (issue #149).
                    "active": if enforce_pin {
                        pinned == Some(p.target_id.as_str())
                    } else {
                        i == active
                    },
                });
                if let Some(ownership) = tab_ownership(
                    self.browser_process.is_none(),
                    &p.target_id,
                    &self.created_targets,
                    &self.adopted_targets,
                ) {
                    tab["ownership"] = json!(ownership);
                }
                tab
            })
            .collect()
    }

    /// The active tab's stable handle + current location, for `chrome-use
    /// current` (#26). `targetId` survives cross-process navigation, so it's the
    /// handle an agent should hold across a multi-step flow.
    pub fn active_page_info(&self) -> Option<Value> {
        let i = self.resolved_active_index();
        if self.agent_group().is_some()
            && self
                .active_target_id
                .as_deref()
                .is_some_and(|tid| self.pages.get(i).is_none_or(|p| p.target_id != tid))
        {
            return None;
        }
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
    pub async fn adopt_newly_opened(
        &mut self,
        before: &HashSet<String>,
        relay_before: Option<&RelayTabBaseline>,
    ) -> NewTabCheck {
        // STRICT MULTI-AGENT ISOLATION: on the relay this session's `before` set is
        // only its OWN tabs, so EVERY foreign tab (the user's, other agents') looks
        // "new" relative to it and would be adopted by a plain diff — exactly the
        // leak where a concurrent agent's tabs (github/Lark/iphone-use) showed up
        // in this session mid-flow. So on the relay a new tab is adopted only when
        // it can be attributed to this session: through the relay's group-scoped
        // getTargets (#40), or through Chrome's own tab metadata (#456, below).
        // A direct CDP connection to someone else's browser adopts nothing.
        let on_relay = self.browser_process.is_none() && self.agent_group().is_some();
        if self.browser_process.is_none() && !on_relay {
            return NewTabCheck::default();
        }
        // A click in a user tab taken with `tab adopt` opens the USER's tab.
        // Chrome's metadata cannot tell: for a click in a background tab it
        // names the window's front tab (possibly ours) as opener and puts the
        // pop-up in that tab's group. So on the relay only a click in a tab
        // this session created can yield a tab the session adopts.
        if on_relay && !clicked_tab_is_created(self.active_target_id().ok(), &self.created_targets)
        {
            return NewTabCheck::default();
        }
        let mut check = NewTabCheck::default();
        // The target-list diff upgrades every new target to created, so it is
        // only for a browser this daemon launched (every tab in it is ours).
        // Over the relay a target that merely appeared after the click (the
        // group-scoped list included) proves nothing about whose it is; the
        // relay path below adopts only a pop-up the extension confirmed.
        if adopts_new_targets_by_diff(self.browser_process.is_some()) {
            check.opened = self.adopt_new_targets(before).await;
        }
        // The extension announces a pop-up only when Chrome names one of our tabs
        // as its opener, and Chrome names the window's FRONT tab instead when the
        // click landed in a background tab — which is where the relay's tabs
        // live. So a pop-up from our tab reached neither getTargets nor the
        // event stream (#456). Find it in chrome.tabs instead.
        if check.opened.is_none() && on_relay {
            if let Some(baseline) = relay_before {
                check = self.adopt_relay_popup(baseline).await;
            }
        }
        check
    }

    /// Over the relay, the Chrome tab ids that exist right before a click, so
    /// the click's tab check can tell a tab it opened from one that was already
    /// there (#456). `None` off the relay, or when the extension cannot answer
    /// `chrome.tabs.query` (before ab-connect 0.5.25) — the check then falls
    /// back to the relay's scoped target list alone.
    pub async fn relay_tab_baseline(&self) -> Option<RelayTabBaseline> {
        if self.browser_process.is_some() || self.agent_group().is_none() {
            return None;
        }
        let tabs = self.chrome_call("tabs", "query", json!([{}])).await.ok()?;
        let tab_ids = tabs
            .as_array()?
            .iter()
            .filter_map(|t| t.get("id").and_then(Value::as_i64))
            .collect();
        Some(RelayTabBaseline { tab_ids })
    }

    /// Chrome tab ids of the tabs this session drives over the relay, read
    /// from their `cb-tab-<tabId>` relay sessions.
    fn relay_own_chrome_tabs(&self) -> HashSet<i64> {
        relay_created_chrome_tabs(&self.pages, &self.created_targets)
    }

    /// Adopt the tab a click on one of this session's tabs opened, found in
    /// chrome.tabs (see [`relay_popup_candidate`] for what counts as ours).
    ///
    /// The tab is attached by its Chrome tab id (`ABExt.attachTabById`,
    /// ab-connect 0.5.30), never by URL: a URL lookup attaches whichever tab
    /// shows that URL first — possibly the user's — and announces it to every
    /// relay client before anything could be checked. An extension without
    /// that capability gets the pop-up reported, not attached.
    ///
    /// Bounded: finding the pop-up takes at most [`RELAY_POPUP_FIND_BUDGET`]
    /// and attaching it at most [`RELAY_POPUP_ATTACH_BUDGET`], each including
    /// the calls in flight. Running out while finding means nothing is known
    /// about a pop-up; running out while attaching means it may be attached
    /// but is not tracked. Both are reported as `unknown`.
    async fn adopt_relay_popup(&mut self, baseline: &RelayTabBaseline) -> NewTabCheck {
        let find_deadline = tokio::time::Instant::now() + RELAY_POPUP_FIND_BUDGET;
        let (tab_id, url, title) = loop {
            let read = tokio::time::timeout_at(
                find_deadline,
                self.chrome_call("tabs", "query", json!([{}])),
            )
            .await;
            let tabs = match read {
                Ok(Ok(v)) => v.as_array().cloned().unwrap_or_default(),
                // The extension cannot list tabs (older than 0.5.25): the
                // scoped target list was the only check, as before.
                Ok(Err(_)) => return NewTabCheck::default(),
                Err(_) => {
                    return NewTabCheck::unknown(format!(
                        "could not read Chrome's tabs within {} ms, so whether the click \
                         opened a tab is unknown. Run `tab list`.",
                        RELAY_POPUP_FIND_BUDGET.as_millis()
                    ))
                }
            };
            let ours = self.relay_own_chrome_tabs();
            let Some(popup) = relay_popup_candidate(&tabs, &baseline.tab_ids, &ours) else {
                return NewTabCheck::default();
            };
            let tab_id = popup.get("id").and_then(Value::as_i64).unwrap_or(-1);
            let url = popup
                .get("url")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            let title = popup
                .get("title")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            if relay_url_is_attachable(&url) {
                break (tab_id, url, title);
            }
            if tokio::time::Instant::now() + Duration::from_millis(100) >= find_deadline {
                let shown = if url.is_empty() { "about:blank" } else { &url };
                return NewTabCheck::unadopted(format!(
                    "the click opened a tab (Chrome tab {tab_id}, still on {shown}) and it was \
                     not adopted within {} ms. Run `tab adopt <url-substring>` once it has loaded.",
                    RELAY_POPUP_FIND_BUDGET.as_millis()
                ));
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        };
        let attach_deadline = tokio::time::Instant::now() + RELAY_POPUP_ATTACH_BUDGET;
        let capable = tokio::time::timeout_at(attach_deadline, self.relay_capabilities())
            .await
            .unwrap_or_default()
            .iter()
            .any(|c| c == ATTACH_TAB_BY_ID_CAPABILITY);
        if !capable {
            return NewTabCheck::unadopted(format!(
                "the click opened a tab (Chrome tab {tab_id}, {url}) but it was not adopted: \
                 this ab-connect cannot attach a tab by its id (needs 0.5.30), and attaching \
                 by URL could take another tab. Update the extension, or run `tab adopt <url>`."
            ));
        }
        let attached = tokio::time::timeout_at(
            attach_deadline,
            self.client.send_command_typed::<_, Value>(
                "ABExt.attachTabById",
                &json!({ "chromeTabId": tab_id }),
                None,
            ),
        )
        .await;
        let resp = match attached {
            Ok(Ok(v)) => v,
            Ok(Err(e)) => {
                return NewTabCheck::unadopted(format!(
                    "the click opened a tab (Chrome tab {tab_id}, {url}) but the extension \
                     did not attach it ({e})."
                ))
            }
            Err(_) => {
                return NewTabCheck::unknown(format!(
                    "the click opened a tab (Chrome tab {tab_id}, {url}); the extension did not \
                     answer within {} ms, so it may be attached but is not tracked. Run \
                     `tab list`, or `tab adopt <url>`.",
                    RELAY_POPUP_ATTACH_BUDGET.as_millis()
                ))
            }
        };
        let target_id = match relay_popup_attach_verdict(&resp, tab_id) {
            RelayPopupVerdict::Confirmed(target_id) => target_id,
            RelayPopupVerdict::NotAttached => {
                return NewTabCheck::unadopted(format!(
                    "the click opened a tab (Chrome tab {tab_id}, {url}) but the extension did \
                     not attach it (it answered {resp})."
                ))
            }
            // Attached, but the extension could not confirm the tab is the
            // agent's (e.g. a user tab shares its window, where Chrome's opener
            // and group metadata cannot be trusted). It keeps its unconfirmed
            // identity: not created, not followed, never closed by `close`.
            RelayPopupVerdict::Unconfirmed => {
                return NewTabCheck::unadopted(format!(
                    "a tab opened (Chrome tab {tab_id}, {url}) but the extension could not \
                     confirm it came from this session's tab, so it was not adopted, followed \
                     or closed with the session. If it is yours, run `tab adopt <url>`."
                ))
            }
        };
        if let Some(page) = self
            .pages
            .iter()
            .find(|p| p.target_id == target_id)
            .cloned()
        {
            self.remember_created_target(&target_id);
            return NewTabCheck {
                opened: Some(page),
                ..NewTabCheck::default()
            };
        }
        let attach = tokio::time::timeout_at(
            attach_deadline,
            self.client.send_command_typed::<_, AttachToTargetResult>(
                "Target.attachToTarget",
                &AttachToTargetParams {
                    target_id: target_id.clone(),
                    flatten: true,
                },
                None,
            ),
        )
        .await;
        let attach = match attach {
            Ok(Ok(r)) => r,
            Ok(Err(e)) => {
                return NewTabCheck::unadopted(format!(
                    "the click opened a tab (Chrome tab {tab_id}, {url}); the extension attached \
                     it but the relay session failed ({e}). Run `tab adopt {target_id}`."
                ))
            }
            Err(_) => {
                return NewTabCheck::unknown(format!(
                    "the click opened a tab (Chrome tab {tab_id}, {url}); the extension attached \
                     it but the relay did not answer within {} ms. Run `tab adopt {target_id}`.",
                    RELAY_POPUP_ATTACH_BUDGET.as_millis()
                ))
            }
        };
        let page = PageInfo {
            tab_id: self.assign_tab_id(),
            label: None,
            target_id: target_id.clone(),
            session_id: attach.session_id.clone(),
            url,
            title: sanitize_title(&title),
            target_type: "page".to_string(),
        };
        // Opened by our own tab, so it is this session's: closed with it, and
        // closable with `tab close`.
        self.remember_created_target(&target_id);
        self.add_background_page(page.clone());
        // Domain setup is the caller's next step's business; bounded so the
        // click's budget holds even when the renderer is slow.
        let _ =
            tokio::time::timeout_at(attach_deadline, self.enable_domains(&attach.session_id)).await;
        NewTabCheck {
            opened: Some(page),
            ..NewTabCheck::default()
        }
    }

    /// The capabilities the connected extension advertised in its hello.
    async fn relay_capabilities(&self) -> Vec<String> {
        self.client
            .send_command_typed::<_, Value>("ABRelay.getCapabilities", &json!({}), None)
            .await
            .ok()
            .and_then(|v| {
                v.get("capabilities").and_then(Value::as_array).map(|a| {
                    a.iter()
                        .filter_map(|c| c.as_str().map(str::to_string))
                        .collect()
                })
            })
            .unwrap_or_default()
    }

    /// The target-list half of [`adopt_newly_opened`]: every target that is
    /// not in `before`, attached and tracked; the first one is returned.
    async fn adopt_new_targets(&mut self, before: &HashSet<String>) -> Option<PageInfo> {
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
            if before.contains(&target.target_id) {
                continue;
            }
            // Not in `before` but already tracked: the session's event drain
            // picked up its `targetCreated` between the click and this check
            // (an observed click settles first, and the settle drains events).
            // It is still the tab this action opened, already attached.
            if let Some(page) = self
                .pages
                .iter()
                .find(|p| p.target_id == target.target_id)
                .cloned()
            {
                self.remember_created_target(&target.target_id);
                if opened.is_none() {
                    opened = Some(page);
                }
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
            self.remember_created_target(&target.target_id);
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
        if on_relay && self.relay_scoped {
            let target_ids: Vec<String> =
                live.iter().map(|target| target.target_id.clone()).collect();
            register_scoped_target_ownership(
                &target_ids,
                &self.created_targets,
                &mut self.adopted_targets,
            );
        }
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
            if self.browser_process.is_some() || self.owned_targets().contains(&target.target_id) {
                let _ = self.enable_domains(&attach_result.session_id).await;
            }
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
            self.adopted_targets.remove(tid);
            self.remove_page_by_target_id(tid);
        }

        // Refresh url/title from each live tab on direct CDP. The extension relay
        // keeps browser-level chrome.tabs metadata current in its synthesized
        // Target.getTargets response. Asking each relay page session for
        // Target.getTargetInfo would wait on a frozen renderer and turn a harmless
        // tab list/switch into an eight-second timeout (issue #157).
        if on_relay {
            return Ok(());
        }
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
        crate::connect::is_relay_url(&self.ws_url)
    }

    /// The label this session's tabs are grouped under in the user's Chrome.
    ///
    /// Read fresh each time rather than cached: `session name` can be run
    /// mid-task, and a tab opened after it should carry the new label. The
    /// title is presentation only — ownership is tracked by target id — so the
    /// session id remains the identity even when the label changes.
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
            Some(crate::session_title::display_name(name))
        }
    }

    /// Opt-in hint for the `ab-connect` extension to place this session's tabs
    /// in a dedicated agent window (same profile, separate window) so agent
    /// activity doesn't clutter the window the user is working in. Only
    /// meaningful on the relay path — the extension consumes it; `None` on
    /// launched/real-CDP so a strict endpoint never receives an unknown param.
    fn dedicated_window(&self) -> Option<bool> {
        if self.agent_group().is_some() && dedicated_window_enabled() {
            Some(true)
        } else {
            None
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
            let close_result = self
                .client
                .send_command_typed::<_, CloseTargetResult>(
                    "Target.closeTarget",
                    &CloseTargetParams {
                        target_id: tid.clone(),
                    },
                    None,
                )
                .await;
            if target_was_closed(&close_result) {
                if let Err(error) = self.forget_created_target(&tid) {
                    eprintln!("{error}");
                }
                self.remove_page_by_target_id(&tid);
            }
        }
        // Removing earlier pages shifts indices — re-pin the kept tab.
        if let Some(i) = self.pages.iter().position(|p| p.target_id == keep) {
            self.active_page_index = i;
            self.pin_active_target();
        }
    }

    /// Best-effort rollback for a tab that Chrome created but the daemon could
    /// not finish registering. The extension owns the underlying Chrome tab,
    /// so closing the stable target also clears its persisted ownership entry.
    async fn discard_created_target(&mut self, target_id: &str) {
        let close_result = self
            .client
            .send_command_typed::<_, CloseTargetResult>(
                "Target.closeTarget",
                &CloseTargetParams {
                    target_id: target_id.to_string(),
                },
                None,
            )
            .await;
        if target_was_closed(&close_result) {
            if let Err(error) = self.forget_created_target(target_id) {
                eprintln!("{error}");
            }
        }
    }

    pub async fn tab_new(
        &mut self,
        url: Option<&str>,
        label: Option<&str>,
    ) -> Result<Value, String> {
        self.tab_new_with_activation(url, label, false).await
    }

    /// Retain the created tab before initialization so a stalled renderer can
    /// be recovered in place. Foreground activation is explicitly requested.
    pub async fn tab_new_with_activation(
        &mut self,
        url: Option<&str>,
        label: Option<&str>,
        activate: bool,
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
        let dedicated_window = self.dedicated_window();
        let result: CreateTargetResult = self
            .client
            .send_command_typed(
                "Target.createTarget",
                &CreateTargetParams {
                    url: target_url.to_string(),
                    agent_group,
                    background: Some(true),
                    dedicated_window,
                },
                None,
            )
            .await?;
        // We created this tab — own it so close() can clean it up.
        self.remember_created_target(&result.target_id);

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

        let tab_id = self.next_tab_id;
        self.next_tab_id += 1;
        let index = self.pages.len();
        let label = label.map(|s| s.to_string());
        self.pages.push(PageInfo {
            tab_id,
            label: label.clone(),
            target_id: result.target_id.clone(),
            session_id: attach.session_id.clone(),
            url: target_url.to_string(),
            title: String::new(),
            target_type: "page".to_string(),
        });
        self.active_page_index = index;
        self.pin_active_target();

        let initialize = async {
            if activate {
                // No result to carry a warning here; activate without the check.
                let target_id = self.active_target_id()?.to_string();
                self.client
                    .send_command(
                        "Target.activateTarget",
                        Some(json!({ "targetId": target_id })),
                        None,
                    )
                    .await?;
            }
            self.enable_domains(&attach.session_id).await
        };
        if let Err(error) = initialize.await {
            return Err(format!(
                "tab_initialization_incomplete: created tab {} ({}) is retained but initialization failed: {}. Recover this same tab with `tab select {} --activate`; do not repeat `tab new`.",
                format_tab_id(tab_id), result.target_id, error, result.target_id
            ));
        }

        // Once this real tab exists, close the daemon's leftover about:blank
        // scratch so the session's tab group isn't left showing a stray blank
        // page beside the work tab (every group otherwise carried one).
        if target_url != "about:blank" {
            let ready_check = async {
                for _ in 0..50 {
                    if let Ok(val) = self.evaluate_simple("document.readyState").await {
                        if let Some(s) = val.as_str() {
                            if s == "complete" || s == "interactive" {
                                break;
                            }
                        }
                    }
                    tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
                }
            };
            let _ =
                tokio::time::timeout(tokio::time::Duration::from_millis(5000), ready_check).await;

            let current_url = self
                .get_url()
                .await
                .unwrap_or_else(|_| target_url.to_string());
            let current_title = self.get_title().await.unwrap_or_default();
            if let Some(page) = self.pages.get_mut(index) {
                page.url = current_url.clone();
                page.title = sanitize_title(&current_title);
            }

            if let Some(new_tid) = self.pages.get(index).map(|p| p.target_id.clone()) {
                self.close_leftover_blank_scratch(&new_tid).await;
            }
        }

        let page_url = self
            .pages
            .get(index)
            .map(|p| p.url.clone())
            .unwrap_or_else(|| target_url.to_string());
        let page_title = self
            .pages
            .get(index)
            .map(|p| p.title.clone())
            .unwrap_or_default();
        let mut resp = json!({
            "tabId": format_tab_id(tab_id),
            "targetId": result.target_id,
            "label": label,
            "url": page_url,
            "total": self.pages.len(),
        });
        if !page_title.is_empty() {
            resp["title"] = json!(page_title);
        }
        Ok(resp)
    }

    /// Duplicate a tab with Chrome's native duplicate primitive exposed by the
    /// `ab-connect` extension. URL-based recreation is deliberately unsupported:
    /// it does not preserve Chrome's duplicate-tab semantics.
    pub async fn tab_duplicate(
        &mut self,
        source_ref: Option<&str>,
        label: Option<&str>,
    ) -> Result<Value, String> {
        if !self.via_relay() || !self.relay_scoped {
            return Err(
                "Native tab duplication requires extension-connected real Chrome; it is not \
                 available for launched Chrome, raw CDP, Lightpanda, or providers without the \
                 ab-connect duplicate capability"
                    .to_string(),
            );
        }
        // The relay endpoint can become discoverable just before the extension's
        // asynchronous hello reaches it. Retry briefly so a command issued at
        // startup does not misclassify a capable extension as unsupported.
        let mut supports_duplicate = false;
        for attempt in 0..10 {
            let capabilities: Value = self
                .client
                .send_command_typed("ABRelay.getCapabilities", &json!({}), None)
                .await
                .map_err(|_| {
                    "Native tab duplication is unavailable because the connected relay does not \
                     advertise extension capabilities"
                        .to_string()
                })?;
            supports_duplicate = capabilities
                .get("capabilities")
                .and_then(Value::as_array)
                .is_some_and(|items| items.iter().any(|item| item == "nativeTabDuplicate"));
            if supports_duplicate || attempt == 9 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        if !supports_duplicate {
            return Err(
                "Native tab duplication is unavailable in the connected extension; update the \
                 chrome-use extension to a build that advertises `nativeTabDuplicate`"
                    .to_string(),
            );
        }
        self.resync_targets().await.ok();
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

        let source_index = match source_ref {
            Some(value) => match self.pages.iter().position(|p| p.target_id == value) {
                Some(index) => index,
                None => {
                    let tab_ref = TabRef::parse(value)?;
                    let tab_id = self.resolve_tab_ref(&tab_ref)?;
                    self.pages
                        .iter()
                        .position(|p| p.tab_id == tab_id)
                        .ok_or_else(|| format!("Tab ID {} not found", tab_id))?
                }
            },
            None => self.resolved_active_index(),
        };
        let source = self
            .pages
            .get(source_index)
            .cloned()
            .ok_or_else(|| "No active tab to duplicate".to_string())?;
        let agent_group = self.agent_group().ok_or_else(|| {
            "Native tab duplication requires an extension session group".to_string()
        })?;

        let duplicate: CreateTargetResult = self
            .client
            .send_command_typed(
                "ABExt.duplicateTab",
                &json!({
                    "sourceTargetId": source.target_id,
                    "agentGroup": agent_group,
                }),
                None,
            )
            .await?;
        self.remember_created_target(&duplicate.target_id);

        let attach: AttachToTargetResult = match self
            .client
            .send_command_typed(
                "Target.attachToTarget",
                &AttachToTargetParams {
                    target_id: duplicate.target_id.clone(),
                    flatten: true,
                },
                None,
            )
            .await
        {
            Ok(attach) => attach,
            Err(error) => {
                self.discard_created_target(&duplicate.target_id).await;
                return Err(format!(
                    "Native duplicate was created but debugger attachment failed: {}",
                    error
                ));
            }
        };

        if let Err(error) = self.enable_domains(&attach.session_id).await {
            self.discard_created_target(&duplicate.target_id).await;
            return Err(format!(
                "Native duplicate was created but debugger setup failed: {}",
                error
            ));
        }

        let source_target_id = source.target_id.clone();
        let tab_id = self.next_tab_id;
        self.next_tab_id += 1;
        let label = label.map(ToString::to_string);
        let index = self.pages.len();
        self.pages.push(PageInfo {
            tab_id,
            label: label.clone(),
            target_id: duplicate.target_id.clone(),
            session_id: attach.session_id,
            url: source.url,
            title: source.title,
            target_type: source.target_type,
        });
        self.active_page_index = index;
        self.pin_active_target();

        Ok(json!({
            "sourceTabId": format_tab_id(source.tab_id),
            "sourceTargetId": source_target_id,
            "tabId": format_tab_id(tab_id),
            "targetId": duplicate.target_id,
            "label": label,
            "duplicated": true,
            "native": true,
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
        // Fail injection for the follow-failure E2E tests: fail exactly here,
        // after the pin moved and before the new tab is set up.
        #[cfg(feature = "e2e-tests")]
        if FAIL_NEXT_TAB_SWITCH_AFTER_PIN.swap(false, std::sync::atomic::Ordering::SeqCst) {
            return Err("injected tab switch failure".to_string());
        }
        let on_relay = self.agent_group().is_some();
        if on_relay {
            // Refresh the stable session binding without requiring any response
            // from the page renderer. This keeps a listed but frozen tab selected
            // for the next screenshot/console/diagnostic command (issue #157).
            self.reattach_active_session().await?;
        } else {
            let session_id = self.pages[index].session_id.clone();
            self.enable_domains(&session_id).await?;
        }

        // Relay resync may prune an unrelated earlier tab while reattaching,
        // shifting vector positions. Resolve the selected tab again through its
        // pinned target id instead of reusing the now-stale caller index.
        self.active_page_index = self.resolved_active_index();
        let index = self.active_page_index;

        // Silent: switching the agent's *internal* active page must not yank the
        // user's foreground tab. The page is driven in the background (focus is
        // emulated in enable_domains); the explicit `bringToFront` command is the
        // only way a tab is deliberately surfaced.

        let (url, title) = if on_relay {
            let page = &self.pages[index];
            (page.url.clone(), page.title.clone())
        } else {
            (
                self.get_url().await.unwrap_or_default(),
                self.get_title().await.unwrap_or_default(),
            )
        };

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

        let target = &self.pages[target_index];
        if !tab_close_is_allowed(
            self.browser_process.is_none(),
            &target.target_id,
            &self.created_targets,
        ) {
            return Err(format!(
                "Refusing to close tab {} because this session did not create it",
                format_tab_id(target.tab_id)
            ));
        }

        let page = &self.pages[target_index];
        let closed_tab_id = page.tab_id;
        let closed_label = page.label.clone();
        let target_id = page.target_id.clone();
        let close_result = self
            .client
            .send_command_typed::<_, CloseTargetResult>(
                "Target.closeTarget",
                &CloseTargetParams {
                    target_id: target_id.clone(),
                },
                None,
            )
            .await?;
        if !close_result.success {
            return Err(format!(
                "Chrome did not close tab {}",
                format_tab_id(closed_tab_id)
            ));
        }
        self.pages.remove(target_index);
        self.update_active_page_after_removal(target_index);
        self.forget_created_target(&target_id)?;

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

    /// Explicit recovery uses the browser connection before renderer probing.
    /// Returns a warning when the activation hid another session's tab.
    pub async fn activate_active_tab(&self, force: bool) -> Result<Option<String>, String> {
        let target_id = self.active_target_id()?.to_string();
        self.activate_target(&target_id, force).await
    }

    /// Browser-level activation must not wait for a blocked renderer session.
    ///
    /// When another live session's tab is in front of the same window, the
    /// activation is refused unless `force`: bringing this tab forward would
    /// hide that tab, and a hidden page can ignore its session's clicks
    /// (#385). Agents sharing the agent window kept doing exactly that to each
    /// other, each "fixing" its own background tab by breaking another's. With
    /// `force` it goes ahead and the returned warning names who was hidden.
    async fn activate_target(
        &self,
        target_id: &str,
        force: bool,
    ) -> Result<Option<String>, String> {
        let conflict = self.foreground_conflict(target_id).await;
        if let (Some(conflict), false) = (&conflict, force) {
            return Err(conflict.refusal());
        }
        let conflict = conflict.map(|c| c.warning());
        self.client
            .send_command(
                "Target.activateTarget",
                Some(json!({ "targetId": target_id })),
                None,
            )
            .await?;
        Ok(conflict)
    }

    /// On the relay, when bringing `target_id` forward would hide a tab another
    /// live session created in the same window, say which. A hidden page can
    /// ignore input (#385), so two sessions sharing a window keep breaking each
    /// other's clicks without either seeing why. Best effort: any failure to
    /// find out returns `None`.
    async fn foreground_conflict(&self, target_id: &str) -> Option<ForegroundConflict> {
        if !self.on_relay() {
            return None;
        }
        let ours: Value = self
            .client
            .send_command(
                "ABExt.inspectTab",
                Some(json!({ "targetId": target_id })),
                None,
            )
            .await
            .ok()?;
        if ours.get("active").and_then(Value::as_bool) == Some(true) {
            return None;
        }
        let window_id = ours.get("windowId")?.as_i64()?;
        let active: Value = self
            .client
            .send_command(
                "ABExt.call",
                Some(json!({
                    "namespace": "tabs",
                    "method": "query",
                    "args": [{ "active": true, "windowId": window_id }],
                })),
                None,
            )
            .await
            .ok()?;
        let front = active.get("result")?.as_array()?.first()?.clone();
        let front_tab = front.get("id")?.as_i64()?;
        let attached: Value = self
            .client
            .send_command("ABExt.attachedTargets", None, None)
            .await
            .ok()?;
        let front_target = attached
            .get("targets")?
            .as_array()?
            .iter()
            .find(|t| t.get("tabId").and_then(Value::as_i64) == Some(front_tab))?
            .get("targetId")?
            .as_str()?
            .to_string();
        let own = DAEMON_SESSION.get().cloned();
        let owner = tokio::task::spawn_blocking(move || {
            crate::connection::live_session_names()
                .into_iter()
                .filter(|name| Some(name) != own.as_ref())
                .find(|name| crate::connection::created_target_ids(name).contains(&front_target))
        })
        .await
        .ok()??;
        let title = front
            .get("title")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        Some(ForegroundConflict { owner, title })
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
    ) -> Result<UploadOutcome, String> {
        let session_id = self.active_session_id()?;

        let (object_id, effective_session_id) =
            resolve_element_object_id(&self.client, session_id, ref_map, selector, iframe_sessions)
                .await?;

        // The selector/ref may point at the *trigger* rather than the file input
        // itself — e.g. Element-Plus `<el-upload>` renders a `display:none`
        // `<input type=file>` and the user clicks/snapshots the "选择文件"
        // `<el-button>` next to it. Resolve to the real `<input type=file>` so we
        // set files on the node the framework actually listens on, and so hidden
        // inputs are reachable (a11y/visibility-based locators miss them).
        let input_object_id = match self
            .resolve_file_input_object_id(&object_id, &effective_session_id, selector)
            .await
        {
            Ok(id) => id,
            // No file input in the DOM: a button that creates one on click and
            // opens the native chooser at once (#386). Catch the chooser, but
            // only behind something that looks like an upload control: a
            // wrong ref to a link or a submit button must not be clicked.
            Err(no_input)
                if self
                    .is_chooser_trigger(&object_id, &effective_session_id)
                    .await =>
            {
                self.file_input_from_chooser(
                    session_id,
                    &effective_session_id,
                    selector,
                    files.len(),
                    ref_map,
                    iframe_sessions,
                )
                .await
                .map_err(|e| format!("{no_input}. Clicking it to catch a file chooser: {e}"))?
            }
            Err(no_input) => return Err(no_input),
        };

        let describe: Value = self
            .client
            .send_command(
                "DOM.describeNode",
                Some(json!({ "objectId": input_object_id })),
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
            // over the relay (issue #13). We hand it the resolved INPUT (not the
            // trigger button), so it takes the `input.files = …` branch — never
            // the dropzone branch, whose uncancelled `drop` DragEvent made the
            // tab navigate to the dropped file (the about:blank side effect).
            if e.contains("Not allowed") || e.contains("-32000") {
                // `allow_dropzone = false`: we hand it a resolved `<input type=file>`,
                // so it takes the `input.files = …` branch. The drop/paste dropzone
                // branch stays gated OFF (and is nav-guarded even when enabled), so a
                // stray `drop` can never navigate the page.
                self.upload_files_via_page(
                    input_object_id.clone(),
                    files,
                    &effective_session_id,
                    false,
                )
                .await?;
            } else {
                return Err(e);
            }
        }

        // `DOM.setFileInputFiles` (or the relay fallback above) has already
        // delivered the files and fired input/change. Read the live input only
        // as confirmation — and only read it. This step used to dispatch a
        // second input/change "to be safe", which is the one thing a page's
        // upload handler cannot tolerate: React/Livewire queued the file twice,
        // and the second entry sat at 0% forever with no delete control and
        // the form's Save button disabled (#254). React dropzones are allowed
        // to consume the FileList and synchronously clear or replace the input;
        // that is successful page behavior, not a rejected upload (#208).
        match self
            .count_file_input(&input_object_id, &effective_session_id)
            .await
        {
            Ok(attached) if attached > 0 => Ok(UploadOutcome {
                attached_count: Some(attached),
                warning: None,
            }),
            Ok(_) => Ok(UploadOutcome {
                attached_count: Some(0),
                warning: Some(
                    "upload events were delivered, but the file input is now empty; the page may have consumed or replaced it (common for React dropzones)"
                        .to_string(),
                ),
            }),
            Err(error) => Ok(UploadOutcome {
                attached_count: None,
                warning: Some(format!(
                    "upload events were delivered, but post-upload verification was unavailable: {error}"
                )),
            }),
        }
    }

    /// Whether an element with no file input could be a control that opens a
    /// file chooser: a button (not one that submits a form), a `role=button`,
    /// a `<label>`, or a focusable widget. Links, submit buttons and plain
    /// content are not, so `upload` never clicks them.
    async fn is_chooser_trigger(&self, object_id: &str, session_id: &str) -> bool {
        let func = r#"function() {
            const el = this;
            if (!el || !el.tagName) return false;
            const tag = el.tagName;
            if (tag === 'A' && el.hasAttribute('href')) return false;
            if (tag === 'INPUT') return false;
            if (tag === 'BUTTON') return !(el.type === 'submit' && el.form);
            const role = (el.getAttribute('role') || '').toLowerCase();
            return role === 'button' || tag === 'LABEL' || el.hasAttribute('tabindex');
        }"#;
        let result: Result<EvaluateResult, String> = self
            .client
            .send_command_typed(
                "Runtime.callFunctionOn",
                &CallFunctionOnParams {
                    function_declaration: func.to_string(),
                    object_id: Some(object_id.to_string()),
                    arguments: None,
                    return_by_value: Some(true),
                    await_promise: Some(false),
                },
                Some(session_id),
            )
            .await;
        matches!(result, Ok(r) if r.result.value == Some(Value::Bool(true)))
    }

    /// Click `selector` with file-chooser interception on, and return the
    /// `<input type=file>` behind the chooser it opened (#386). Interception is
    /// enabled before the click and checked, so a native dialog never opens on
    /// the user's screen; it is switched off again whatever happens.
    async fn file_input_from_chooser(
        &self,
        session_id: &str,
        effective_session_id: &str,
        selector: &str,
        file_count: usize,
        ref_map: &RefMap,
        iframe_sessions: &HashMap<String, String>,
    ) -> Result<String, String> {
        let intercept = |enabled: bool| {
            self.client.send_command(
                "Page.setInterceptFileChooserDialog",
                Some(json!({ "enabled": enabled })),
                Some(effective_session_id),
            )
        };
        intercept(true)
            .await
            .map_err(|e| format!("this connection cannot intercept the file chooser ({e})"))?;
        let mut rx = self.client.subscribe();
        let clicked = super::interaction::click(
            &self.client,
            session_id,
            ref_map,
            selector,
            "left",
            1,
            iframe_sessions,
        )
        .await;
        let opened = match clicked {
            Err(e) => Err(format!("the click failed: {e}")),
            Ok(()) => {
                let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
                loop {
                    match tokio::time::timeout_at(deadline, rx.recv()).await {
                        Ok(Ok(ev))
                            if ev.method == "Page.fileChooserOpened"
                                && ev.session_id.as_deref() == Some(effective_session_id) =>
                        {
                            break Ok(ev.params);
                        }
                        Ok(Ok(_)) => continue,
                        Ok(Err(tokio::sync::broadcast::error::RecvError::Lagged(_))) => continue,
                        Ok(Err(_)) => break Err("the connection closed".to_string()),
                        Err(_) => {
                            break Err("no file chooser opened within 5s; pass the \
                                       `<input type=file>` or the control that opens it"
                                .to_string())
                        }
                    }
                }
            }
        };
        let _ = intercept(false).await;
        let params = opened?;
        if params.get("mode").and_then(Value::as_str) == Some("selectSingle") && file_count > 1 {
            return Err(format!(
                "the chooser accepts one file, but {file_count} were given"
            ));
        }
        let backend_node_id = params
            .get("backendNodeId")
            .and_then(Value::as_i64)
            .ok_or("the file chooser did not name its input element")?;
        let resolved: Value = self
            .client
            .send_command(
                "DOM.resolveNode",
                Some(json!({ "backendNodeId": backend_node_id })),
                Some(effective_session_id),
            )
            .await?;
        resolved
            .pointer("/object/objectId")
            .and_then(Value::as_str)
            .map(str::to_string)
            .ok_or_else(|| "could not resolve the chooser's input element".to_string())
    }

    /// Given an arbitrary resolved element, return the object id of the
    /// associated `<input type=file>`. Handles the input itself, a `<label>`
    /// (via `for=` or a wrapped input), a descendant input, or an ancestor
    /// upload container (e.g. `.el-upload`) that holds a hidden input. Works on
    /// `display:none` inputs — it never filters by visibility. Errors (without
    /// navigating or otherwise touching the page) if none is found.
    async fn resolve_file_input_object_id(
        &self,
        object_id: &str,
        session_id: &str,
        selector: &str,
    ) -> Result<String, String> {
        let func = r#"function() {
            const isFileInput = e => !!e && e.tagName === 'INPUT' && e.type === 'file';
            const el = this;
            if (isFileInput(el)) return el;
            // A <label> trigger: explicit `for=`, then a wrapped input.
            if (el.tagName === 'LABEL') {
                if (el.htmlFor) {
                    const t = el.ownerDocument.getElementById(el.htmlFor);
                    if (isFileInput(t)) return t;
                }
            }
            // A descendant input (el-upload wrapper passed directly).
            if (el.querySelector) {
                const within = el.querySelector('input[type=file]');
                if (within) return within;
            }
            // A trigger button/element: walk up to the nearest container that
            // owns a file input (el-upload puts the hidden input as a sibling).
            // Stop at <body>/<html>: a file input found only at page-root level
            // is not associated with this element — it's just "some other input
            // on the page". Matching it would be a false-success upload (e.g.
            // uploading to an <h1> that merely shares <body> with an unrelated
            // hidden input). A real upload widget wraps its input in a container.
            let p = el;
            for (let i = 0; i < 8 && p; i++, p = p.parentElement) {
                if (p.tagName === 'BODY' || p.tagName === 'HTML') break;
                if (p.querySelector) {
                    const found = p.querySelector('input[type=file]');
                    if (found) return found;
                }
            }
            return null;
        }"#;

        let result: EvaluateResult = self
            .client
            .send_command_typed(
                "Runtime.callFunctionOn",
                &CallFunctionOnParams {
                    function_declaration: func.to_string(),
                    object_id: Some(object_id.to_string()),
                    arguments: None,
                    return_by_value: Some(false),
                    await_promise: Some(false),
                },
                Some(session_id),
            )
            .await
            .map_err(|e| format!("resolving file input failed: {}", e))?;

        if let Some(ref details) = result.exception_details {
            return Err(format!("resolving file input threw: {}", details.text));
        }

        // No file input under/around the target. Fail cleanly and LOUDLY here —
        // the caller must NOT fall through to any drop/paste path (an uncancelled
        // `drop` carrying a File makes Chrome navigate to open it → about:blank).
        // A failed upload leaves the tab exactly where it was.
        result.result.object_id.ok_or_else(|| {
            format!(
                "no <input type=file> resolved for \"{}\" — pass the file input \
                 (e.g. `input[type=file]`) or an upload trigger inside the same \
                 upload container (is the upload dialog open / the ref still valid?)",
                selector
            )
        })
    }

    /// Read the file input's `files.length` and dispatch bubbling `input`+`change`
    /// so Vue/React register the new files. Returns the attached file count.
    /// How many files the input holds now. Deliberately does not dispatch
    /// anything: both delivery paths have already fired `input`/`change`
    /// (Chrome does it natively for `DOM.setFileInputFiles`; the relay
    /// fallback does it itself), and a page counts every `change` as a new
    /// upload.
    async fn count_file_input(
        &self,
        input_object_id: &str,
        session_id: &str,
    ) -> Result<u64, String> {
        let func = r#"function() {
            const el = this;
            if (!el || el.tagName !== 'INPUT' || el.type !== 'file') return -1;
            return el.files ? el.files.length : 0;
        }"#;

        let result: EvaluateResult = self
            .client
            .send_command_typed(
                "Runtime.callFunctionOn",
                &CallFunctionOnParams {
                    function_declaration: func.to_string(),
                    object_id: Some(input_object_id.to_string()),
                    arguments: None,
                    return_by_value: Some(true),
                    await_promise: Some(false),
                },
                Some(session_id),
            )
            .await
            .map_err(|e| format!("verifying upload failed: {}", e))?;

        if let Some(ref details) = result.exception_details {
            return Err(format!("verifying upload threw: {}", details.text));
        }

        let count = result
            .result
            .value
            .as_ref()
            .and_then(|v| v.as_i64())
            .unwrap_or(-1);
        if count < 0 {
            return Err("resolved upload target is not a file input".to_string());
        }
        Ok(count as u64)
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
        allow_dropzone: bool,
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
        // Arg 0 (`allowDropzone`): when false, a NON-file-input target is a hard
        // no-op (returns `noinput:0`) — we never dispatch `drop`/`paste`, because an
        // uncancelled `drop` carrying a File makes Chrome navigate to open it
        // (the about:blank side effect). When true, the drop is wrapped in a
        // capture-phase `preventDefault` guard so the browser's default file
        // navigation can NEVER fire, while the page's own drop handlers still run.
        let func = r#"function(allowDropzone) {
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
            if (el && el.tagName === 'INPUT' && el.type === 'file') {
                el.files = dt.files;
                el.dispatchEvent(new Event('input', { bubbles: true }));
                el.dispatchEvent(new Event('change', { bubbles: true }));
                return 'input:' + dt.files.length;
            }
            // Non-input target. Only a real dropzone/composer can take this, and
            // only when explicitly opted in. Otherwise: do NOTHING and report it,
            // so a stray drop can never navigate the page.
            if (!allowDropzone) return 'noinput:0';
            // Suppress the browser's default drop action (navigate-to-file) no
            // matter what the page does, while still letting the page's own
            // handlers see the event.
            const guard = e => { e.preventDefault(); };
            window.addEventListener('dragover', guard, true);
            window.addEventListener('drop', guard, true);
            try {
                try { el.dispatchEvent(new ClipboardEvent('paste', { bubbles: true, clipboardData: dt })); } catch (e) {}
                try {
                    const ev = new DragEvent('drop', { bubbles: true, cancelable: true });
                    Object.defineProperty(ev, 'dataTransfer', { value: dt });
                    el.dispatchEvent(ev);
                } catch (e) {}
            } finally {
                window.removeEventListener('dragover', guard, true);
                window.removeEventListener('drop', guard, true);
            }
            return 'event:' + dt.files.length;
        }"#;

        let result: EvaluateResult = self
            .client
            .send_command_typed(
                "Runtime.callFunctionOn",
                &CallFunctionOnParams {
                    function_declaration: func.to_string(),
                    object_id: Some(object_id),
                    arguments: Some(vec![CallArgument {
                        value: Some(json!(allow_dropzone)),
                        object_id: None,
                    }]),
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

        // A `noinput:` sentinel means the target wasn't a file input and the
        // dropzone path was (correctly) gated off — surface it as a clean error
        // rather than a silent success, and without having touched the page.
        if let Some(s) = result.result.value.as_ref().and_then(|v| v.as_str()) {
            if s.starts_with("noinput") {
                return Err(
                    "relay upload target is not a file input (dropzone path disabled)".to_string(),
                );
            }
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

    pub async fn tab_switch_by_id(&mut self, tab_id: u32) -> Result<Value, String> {
        self.tab_switch_by_id_with_activation(tab_id, false, false)
            .await
    }

    /// Switch to `tab_id` for `click --follow`, all or nothing: when the switch
    /// fails after the pin has already moved (reattach or domain setup on the
    /// new tab), the session is pinned back on the tab it was driving, so no
    /// later command or observation lands on a half-switched tab.
    pub async fn follow_tab(&mut self, tab_id: u32) -> Result<Value, String> {
        let prev_target = self.active_target_id.clone();
        let prev_index = self.active_page_index;
        match self.tab_switch_by_id(tab_id).await {
            Ok(v) => Ok(v),
            Err(e) => {
                self.active_target_id = prev_target;
                self.active_page_index = prev_index;
                self.active_page_index = self.resolved_active_index();
                Err(e)
            }
        }
    }

    /// Check ownership before activation, then initialize the requested renderer.
    pub async fn tab_switch_by_id_with_activation(
        &mut self,
        tab_id: u32,
        activate: bool,
        force_activate: bool,
    ) -> Result<Value, String> {
        let index = self
            .pages
            .iter()
            .position(|p| p.tab_id == tab_id)
            .ok_or_else(|| format!("Tab ID {} not found", tab_id))?;
        let target = &self.pages[index];
        if !tab_switch_is_allowed(
            self.browser_process.is_none(),
            &target.target_id,
            &self.owned_targets(),
        ) {
            return Err(refuse_unowned_tab_message(target.tab_id, &target.target_id));
        }
        let mut warning = None;
        if activate {
            let target_id = target.target_id.clone();
            warning = self.activate_target(&target_id, force_activate).await?;
        }
        let mut switched = self.tab_switch(index).await?;
        if let (Some(w), Some(obj)) = (warning, switched.as_object_mut()) {
            obj.insert("warning".to_string(), json!(w));
        }
        Ok(switched)
    }

    /// Return browser-level tab metadata without evaluating page JavaScript.
    /// This remains useful when the renderer main thread is blocked.
    /// Which targets the relay is actually holding right now.
    ///
    /// `tab_list` reports the session's pin as "active" — what was asked for.
    /// Whether the relay still has that tab is a different fact, and until now
    /// nothing could state it: a mismatch surfaced only as a command failing
    /// with Chrome's own words about a different extension's URL, which reads
    /// like a permissions bug (issue #217).
    ///
    /// `None` when the answer is unavailable rather than negative — an older
    /// extension has no such method, and "we could not ask" must not render as
    /// "not attached".
    pub async fn relay_attached_target_ids(&self) -> Option<std::collections::HashSet<String>> {
        if !self.on_relay() {
            return None;
        }
        let result: Value = self
            .client
            .send_command_typed("ABExt.attachedTargets", &json!({}), None)
            .await
            .ok()?;
        let rows = result.get("targets")?.as_array()?;
        Some(
            rows.iter()
                .filter(|r| r.get("attached").and_then(|v| v.as_bool()).unwrap_or(false))
                .filter_map(|r| r.get("targetId").and_then(|v| v.as_str()))
                .map(str::to_string)
                .collect(),
        )
    }

    pub async fn tab_inspect_by_id(&self, tab_id: u32) -> Result<Value, String> {
        let page = self
            .pages
            .iter()
            .find(|page| page.tab_id == tab_id)
            .ok_or_else(|| format!("Tab ID {} not found", tab_id))?;
        if self.agent_group().is_some() {
            let mut result: Value = self
                .client
                .send_command_typed(
                    "ABExt.inspectTab",
                    &json!({
                        "sessionId": page.session_id,
                        "targetId": page.target_id,
                    }),
                    None,
                )
                .await?;
            result["tabId"] = json!(format_tab_id(page.tab_id));
            result["label"] = json!(page.label);
            return Ok(result);
        }
        Ok(json!({
            "tabId": format_tab_id(page.tab_id),
            "label": page.label,
            "targetId": page.target_id,
            "url": page.url,
            "title": page.title,
            "debuggerAttached": true,
        }))
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
            let previous_pin = self.active_target_id.clone();
            let on_relay = self.agent_group().is_some();
            self.pages.remove(pos);
            self.update_active_page_after_removal(pos);
            self.active_target_id = active_target_after_removal(
                &self.pages,
                self.active_page_index,
                previous_pin.as_deref(),
                target_id,
                on_relay,
            );
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

    /// Returns the stable `target_id` for a given `tab_id`, if open.
    pub fn target_id_for_tab(&self, tab_id: u32) -> Option<&str> {
        self.pages
            .iter()
            .find(|p| p.tab_id == tab_id)
            .map(|p| p.target_id.as_str())
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
            adopted_targets: HashSet::new(),
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
mod relay_popup_tests {
    use super::{
        adopts_new_targets_by_diff, relay_chrome_tab_id, relay_popup_attach_verdict,
        relay_popup_candidate, relay_url_is_attachable, RelayPopupVerdict,
    };
    use serde_json::{json, Value};
    use std::collections::HashSet;

    /// The session's close list (created targets) after a pop-up's attach
    /// answer is applied, the way `adopt_relay_popup` applies it on both its
    /// "already tracked" and "newly attached" paths.
    fn close_list_after(resp: Value) -> HashSet<String> {
        let mut created: HashSet<String> = ["SRC".to_string()].into();
        if let RelayPopupVerdict::Confirmed(t) = relay_popup_attach_verdict(&resp, 77) {
            created.insert(t);
        }
        created
    }

    #[test]
    fn only_an_explicit_agent_popup_true_puts_a_popup_on_the_close_list() {
        let base = json!({ "attached": true, "chromeTabId": 77, "targetId": "POP" });
        let with = |v: Value| {
            let mut r = base.clone();
            r["agentPopup"] = v;
            r
        };
        let confirmed: HashSet<String> = ["SRC".to_string(), "POP".to_string()].into();
        let unchanged: HashSet<String> = ["SRC".to_string()].into();
        assert_eq!(close_list_after(with(json!(true))), confirmed);
        // false, missing, and non-bool keep the unconfirmed identity.
        assert_eq!(close_list_after(with(json!(false))), unchanged);
        assert_eq!(close_list_after(base.clone()), unchanged);
        assert_eq!(close_list_after(with(json!("true"))), unchanged);
        assert_eq!(close_list_after(with(json!(1))), unchanged);
        assert_eq!(close_list_after(with(Value::Null)), unchanged);
        assert_eq!(
            relay_popup_attach_verdict(&with(json!(false)), 77),
            RelayPopupVerdict::Unconfirmed
        );
        // Another tab, not attached, or no target id: never on the list.
        let mut other = with(json!(true));
        other["chromeTabId"] = json!(78);
        assert_eq!(close_list_after(other), unchanged);
        let mut detached = with(json!(true));
        detached["attached"] = json!(false);
        assert_eq!(close_list_after(detached), unchanged);
        let mut no_target = with(json!(true));
        no_target.as_object_mut().unwrap().remove("targetId");
        assert_eq!(close_list_after(no_target), unchanged);
    }

    #[test]
    fn a_target_that_merely_appeared_is_never_upgraded_over_the_relay() {
        // The target-list diff (which puts every new target on the close list)
        // runs only for a browser this daemon launched; over the relay, the
        // scoped list included, it does not run.
        assert!(adopts_new_targets_by_diff(true));
        assert!(!adopts_new_targets_by_diff(false));
    }

    const OURS: i64 = 344;
    const OUR_GROUP: i64 = 2124;

    /// Our source tab, in our group, in the background; the user's front tab.
    fn base() -> Vec<Value> {
        vec![
            json!({ "id": 326, "groupId": -1, "active": true,
                    "url": "https://example.com/?probe=helper-v2" }),
            json!({ "id": OURS, "groupId": OUR_GROUP, "active": false,
                    "url": "http://127.0.0.1:59080/popup-launch.html" }),
        ]
    }

    fn pick(extra: Vec<Value>) -> Option<i64> {
        let mut tabs = base();
        tabs.extend(extra);
        let before: HashSet<i64> = [326, OURS].into();
        let ours: HashSet<i64> = [OURS].into();
        relay_popup_candidate(&tabs, &before, &ours).map(|t| t["id"].as_i64().unwrap())
    }

    /// The chrome.tabs view recorded in #456: the pop-up our click opened is
    /// in our group, but Chrome names the FRONT tab as its openerTabId. No
    /// targetCreated and no attachedToTarget ever reached the daemon for it.
    #[test]
    fn a_popup_in_our_group_is_ours_even_when_chrome_names_the_front_tab_as_opener() {
        let popup = json!({ "id": 347, "groupId": OUR_GROUP, "openerTabId": 326,
                            "url": "http://127.0.0.1:59080/popup-busy.html" });
        assert_eq!(pick(vec![popup]), Some(347));
    }

    /// An ungrouped tab whose opener is ours (e.g. our tabs are the front tab
    /// and grouping failed, or a pop-up window) is ours.
    #[test]
    fn an_ungrouped_tab_opened_by_our_tab_is_ours() {
        let popup = json!({ "id": 500, "groupId": -1, "openerTabId": OURS, "url": "http://h/p" });
        assert_eq!(pick(vec![popup]), Some(500));
        let no_field = json!({ "id": 501, "openerTabId": OURS, "url": "http://h/p" });
        assert_eq!(pick(vec![no_field]), Some(501));
    }

    /// Conflicting ownership: a tab in ANOTHER session's group whose opener is
    /// our tab. The foreign group is an explicit claim and wins — refuse.
    #[test]
    fn a_foreign_group_wins_over_an_opener_that_is_ours() {
        let contested = json!({ "id": 600, "groupId": 9999, "openerTabId": OURS,
                                "url": "http://h/other-session.html" });
        assert_eq!(pick(vec![contested]), None);
    }

    #[test]
    fn tabs_that_existed_before_the_click_are_never_picked() {
        let mut tabs = base();
        tabs.push(json!({ "id": 347, "groupId": OUR_GROUP, "url": "http://h/p" }));
        let before: HashSet<i64> = [326, OURS, 347].into();
        let ours: HashSet<i64> = [OURS].into();
        assert!(relay_popup_candidate(&tabs, &before, &ours).is_none());
    }

    /// The full negative chain at once: every way a new tab can belong to
    /// someone else. None of them is picked.
    #[test]
    fn no_tab_that_belongs_to_someone_else_is_picked() {
        let foreign = vec![
            // the user's: no group, opener is the user's front tab
            json!({ "id": 400, "groupId": -1, "openerTabId": 326, "url": "http://h/manual.html" }),
            // the user's: no group, no opener at all
            json!({ "id": 401, "url": "http://h/typed.html" }),
            // another session's: its own group, its own opener
            json!({ "id": 402, "groupId": 9999, "openerTabId": 326, "url": "http://h/b.html" }),
            // another session's group, but opener names our tab
            json!({ "id": 403, "groupId": 9999, "openerTabId": OURS, "url": "http://h/c.html" }),
            // another session's group, no opener
            json!({ "id": 404, "groupId": 8888, "url": "http://h/d.html" }),
            // no id at all
            json!({ "groupId": OUR_GROUP, "url": "http://h/e.html" }),
        ];
        assert_eq!(pick(foreign.clone()), None);
        // ...and with our real pop-up among them, exactly that one is picked.
        let mut mixed = foreign;
        mixed.push(json!({ "id": 700, "groupId": OUR_GROUP, "url": "http://h/popup-busy.html" }));
        assert_eq!(pick(mixed), Some(700));
    }

    /// Our tabs not grouped at all: a group-less candidate needs an opener of
    /// ours, and "no group" on both sides is not a shared group.
    #[test]
    fn ungrouped_tabs_do_not_share_a_group() {
        let tabs = vec![
            json!({ "id": OURS, "groupId": -1, "url": "http://h/src" }),
            json!({ "id": 800, "groupId": -1, "openerTabId": 326, "url": "http://h/user" }),
        ];
        let before: HashSet<i64> = [OURS].into();
        let ours: HashSet<i64> = [OURS].into();
        assert!(relay_popup_candidate(&tabs, &before, &ours).is_none());
    }

    #[test]
    fn blank_and_privileged_pages_are_not_attachable_yet() {
        assert!(relay_url_is_attachable(
            "http://127.0.0.1:59080/popup-busy.html"
        ));
        assert!(!relay_url_is_attachable("about:blank"));
        assert!(!relay_url_is_attachable(""));
        assert!(!relay_url_is_attachable("chrome://newtab/"));
    }

    #[test]
    fn relay_sessions_carry_their_chrome_tab_id() {
        assert_eq!(relay_chrome_tab_id("cb-tab-1655536344"), Some(1655536344));
        assert_eq!(relay_chrome_tab_id("8F3A..."), None);
        assert_eq!(relay_chrome_tab_id("cb-tab-"), None);
    }
}

#[cfg(test)]
mod tests {

    #[test]
    fn foreground_conflict_warning_names_the_session_and_tab() {
        let w = foreground_conflict_warning("plugins-77", "Log in - OpenAI");
        assert!(w.contains("'plugins-77'"), "{w}");
        assert!(w.contains("Log in - OpenAI"), "{w}");
        assert!(w.contains("separate window"), "{w}");
        assert!(foreground_conflict_warning("x", "").contains("its tab"));
    }

    /// Activating over another session's front tab is refused by default. The
    /// error has to give the agent the move that actually helps (the click was
    /// delivered; wait and re-read) before the override, and must be
    /// recognisable so `tab adopt` can keep the adopt and report it.
    #[test]
    fn foreground_conflict_refusal_says_wait_and_never_offers_force() {
        let conflict = ForegroundConflict {
            owner: "ab-hn2".to_string(),
            title: "Checkout".to_string(),
        };
        let r = conflict.refusal();
        assert!(r.starts_with("foreground_in_use:"), "{r}");
        assert!(r.contains("'ab-hn2'"), "{r}");
        assert!(r.contains("Checkout"), "{r}");
        assert!(r.contains("reach a background tab"), "{r}");
        // Weak models ran the override the moment the error named it.
        assert!(r.contains("wait --text"), "{r}");
        assert!(!r.contains("--force"), "{r}");
        assert_eq!(
            conflict.warning(),
            foreground_conflict_warning("ab-hn2", "Checkout")
        );
    }

    use super::url_matches_adopt_spec;

    #[test]
    fn adopt_spec_matches_only_urls_that_contain_it() {
        let spec = "https://baijiahao.baidu.com/builder/theme/bjh/login";
        assert!(url_matches_adopt_spec(
            "https://baijiahao.baidu.com/builder/theme/bjh/login?redirect=x",
            spec
        ));
        assert!(url_matches_adopt_spec(
            "HTTPS://BAIJIAHAO.BAIDU.COM/builder/theme/bjh/login",
            spec
        ));
        assert!(!url_matches_adopt_spec(
            "https://www.xiaohongshu.com/",
            spec
        ));
        assert!(!url_matches_adopt_spec(
            "https://baijiahao.baidu.com/builder/rc/home",
            spec
        ));
        assert!(!url_matches_adopt_spec("https://anything/", "   "));
    }

    use super::to_ai_friendly_error;

    #[test]
    fn a_payload_sized_timeout_is_not_reported_as_a_dead_connection() {
        // 150KB into a rich editor ran out the daemon budget. The connection was
        // healthy; the command simply needed longer than we allowed. Telling the
        // caller to reconnect sent them after a stale service worker that was
        // not there (#301).
        let out = to_ai_friendly_error("CDP command timed out after 180s: Input.insertText");
        assert!(
            out.contains("size limit, not a dead connection"),
            "got: {out}"
        );
        assert!(out.contains("do NOT reconnect"), "got: {out}");
        assert!(
            !out.contains("stale relay/service-worker"),
            "must not send the caller after a stale worker: {out}"
        );

        // The command failing is not the page stopping: `withRelayTimeout` races
        // a timer against a `sendCommand` promise that was already dispatched,
        // and losing that race cancels nothing. The renderer was measured still
        // working ~14 minutes after the caller got this error, so a command sent
        // "right after the failure" lands on a busy tab (#315). Say so.
        assert!(
            out.contains("NOT cancelled"),
            "must say the insert keeps running: {out}"
        );

        // The old wording told the caller to split into back-to-back
        // `keyboard inserttext` calls — the exact thing #301 proved corrupts
        // text at every boundary, because a call returns on dispatch and not on
        // commit, and total length survives so a length check passes. If a
        // split is mentioned at all, the content check must be mentioned with
        // it; advising the split alone is the regression this pins.
        if out.contains("pieces") || out.contains("split") {
            assert!(
                out.contains("CONTENT"),
                "a split must be paired with a content comparison, not a length check: {out}"
            );
        }

        // An ordinary deadline alone cannot diagnose a lost connection.
        let other = to_ai_friendly_error("CDP command timed out after 30s: Runtime.evaluate");
        assert!(
            other.contains("does not establish a connection failure"),
            "got: {other}"
        );
        assert!(!other.contains("size limit"), "got: {other}");
    }

    /// A wait that reached its deadline must not be diagnosed either way.
    ///
    /// `poll_until_true` swallows a probe that timed out or errored and keeps
    /// polling, so this message is identical whether every probe answered or
    /// none did. The old generic branch asserted a stale relay and sent the
    /// caller to reconnect; a first version of this branch asserted the
    /// opposite ("the connection is fine"). Both claim more than the code can
    /// support, and the second is the more dangerous when the connection really
    /// has died.
    #[test]
    fn a_wait_whose_condition_never_held_is_not_a_dead_connection() {
        let out = to_ai_friendly_error("Wait timed out after 25000ms");

        // Must not diagnose a dead connection...
        assert!(
            !out.contains("stale relay/service-worker"),
            "must not blame the connection: {out}"
        );
        assert!(
            !out.contains("Reconnect with `connect`"),
            "must not send the caller to reconnect: {out}"
        );
        // ...and must not claim a healthy one either. Every probe may have
        // failed; this message cannot tell the difference.
        assert!(
            !out.contains("connection is fine"),
            "must not vouch for the connection: {out}"
        );
        assert!(
            out.contains("does not establish a connection failure"),
            "{out}"
        );

        // What is actually known, and where to look first.
        assert!(out.contains("not observed within the budget"), "{out}");
        assert!(out.contains("case-sensitive"), "{out}");
        assert!(
            out.contains("do not wait for a second confirmation"),
            "an already-visible answer is the answer: {out}"
        );

        // A genuine CDP/relay timeout keeps its own diagnosis.
        let relay = to_ai_friendly_error("CDP command timed out after 30s: Page.enable");
        assert!(
            relay.contains("tab select <targetId> --activate"),
            "{relay}"
        );
        assert!(!relay.contains("stale relay/service-worker"), "{relay}");
        assert!(!relay.contains("case-sensitive"), "{relay}");
    }

    use super::*;
    use tokio::time::sleep;

    #[tokio::test]
    async fn access_checks_end_only_a_definitively_blocked_wait() {
        let error = "debugger_access_denied: fixture".to_string();
        let result = wait_with_access_checks(
            std::future::pending(),
            || async { Err(error.clone()) },
            Duration::ZERO,
        )
        .await;
        assert_eq!(result, Err(error));
    }

    #[tokio::test]
    async fn lifecycle_completion_does_not_wait_for_an_access_probe() {
        let mut calls = 0;
        let result = wait_with_access_checks(
            async { Ok(()) },
            || {
                calls += 1;
                std::future::pending()
            },
            Duration::from_secs(30),
        )
        .await;
        assert_eq!(result, Ok(()));
        assert_eq!(calls, 0);
    }

    #[tokio::test]
    async fn successful_or_transient_checks_do_not_complete_the_lifecycle() {
        let (tx, rx) = tokio::sync::oneshot::channel();
        let mut signal = Some(tx);
        let mut calls = 0;
        let result = wait_with_access_checks(
            async {
                rx.await.unwrap();
                Err("original lifecycle error".to_string())
            },
            || {
                calls += 1;
                if calls == 3 {
                    let _ = signal.take().unwrap().send(());
                }
                let check = if calls == 2 {
                    Err("temporary transport failure".to_string())
                } else {
                    Ok(())
                };
                std::future::ready(check)
            },
            Duration::ZERO,
        )
        .await;
        assert_eq!(result, Err("original lifecycle error".to_string()));
        assert_eq!(calls, 3);
    }

    #[test]
    fn a_refused_ref_with_suggestions_gets_no_selector_hint() {
        let refused = "Ref e4 could not be resolved: no element with that role and name is on \
                       the page now.\n  try @e7 [button] \"Save now\"\nOr run `snapshot -i` to \
                       refresh the refs.";
        assert_eq!(to_ai_friendly_error(refused), refused);
    }

    /// #373: the cause hint must not send agents to a `--launch` profile
    /// (they abandoned the user's logged-in Chrome over it).
    #[test]
    fn foreign_frame_hint_does_not_suggest_relaunching() {
        let hint = foreign_frame_hint();
        assert!(!hint.contains("--launch"), "{hint}");
        assert!(hint.contains("chrome-use does that itself"), "{hint}");
    }

    #[test]
    fn live_group_ignores_ungrouped_tabs_and_errors() {
        assert_eq!(live_group(&Ok(json!({ "groupId": 7 }))), Some(7));
        assert_eq!(live_group(&Ok(json!({ "groupId": -1 }))), None);
        assert_eq!(live_group(&Err("x".to_string())), None);
    }

    #[test]
    fn debugger_restriction_is_not_a_stale_target_retry() {
        let raw = "Cannot access a chrome-extension:// URL of different extension";
        let friendly = to_ai_friendly_error(raw);
        assert!(friendly.starts_with("debugger_access_denied:"));
        assert!(!is_stale_target_error(&friendly));
        assert_eq!(to_ai_friendly_error(&friendly), friendly);
        let unknown = format!("action_outcome_unknown: original {raw}");
        assert_eq!(to_ai_friendly_error(&unknown), unknown);
    }

    #[test]
    fn relay_primary_navigation_reads_final_metadata_but_recovery_does_not() {
        let mut payload = json!({
            "frameId": "",
            "relayFallback": {
                "method": "browser-level chrome.tabs.update",
                "url": "https://example.com/before-redirect",
                "recovered": false
            }
        });
        let primary: PageNavigateResult = serde_json::from_value(payload.clone()).unwrap();
        assert!(primary.recovery_metadata().is_none());

        payload["relayFallback"]["recovered"] = json!(true);
        let recovery: PageNavigateResult = serde_json::from_value(payload.clone()).unwrap();
        assert!(recovery.recovery_metadata().is_some());

        payload["relayFallback"]
            .as_object_mut()
            .unwrap()
            .remove("recovered");
        let legacy: PageNavigateResult = serde_json::from_value(payload).unwrap();
        assert!(legacy.recovery_metadata().is_some());
    }

    #[test]
    fn test_format_tab_id() {
        assert_eq!(format_tab_id(1), "t1");
        assert_eq!(format_tab_id(42), "t42");
    }

    #[test]
    fn owned_tab_cleanup_fits_inside_the_shutdown_grace_period() {
        // The #192 leak was exactly this relationship being inverted: cleanup
        // needed longer than `session stop` was willing to wait, so the daemon
        // was SIGKILLed mid-cleanup and its tabs survived. Whoever tunes either
        // constant next has to keep cleanup strictly the shorter of the two.
        assert!(
            OWNED_TAB_CLEANUP_BUDGET < crate::connection::DAEMON_SHUTDOWN_GRACE,
            "tab cleanup ({:?}) must finish before SIGKILL at {:?}",
            OWNED_TAB_CLEANUP_BUDGET,
            crate::connection::DAEMON_SHUTDOWN_GRACE,
        );
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
    fn test_dedicated_window_enabled() {
        let guard = crate::test_utils::EnvGuard::new(&["AGENT_BROWSER_DEDICATED_WINDOW"]);

        guard.remove("AGENT_BROWSER_DEDICATED_WINDOW");
        assert!(
            dedicated_window_enabled(),
            "unset → dedicated window is default"
        );

        for on in ["1", "true", "on", "dedicated", " dedicated "] {
            guard.set("AGENT_BROWSER_DEDICATED_WINDOW", on);
            assert!(dedicated_window_enabled(), "{on:?} should stay dedicated");
        }

        for off in ["0", "false", "off", "user", "shared", "current"] {
            guard.set("AGENT_BROWSER_DEDICATED_WINDOW", off);
            assert!(!dedicated_window_enabled(), "{off:?} should opt out");
        }
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
        assert!(is_stale_target_error(BOUND_TAB_GONE));
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

    #[test]
    fn command_timeout_matches_navigate_timeout() {
        // The #126 signature: the navigate CDP command ran to its full budget.
        assert!(is_command_timeout_error(
            "CDP command timed out: Page.navigate"
        ));
        assert!(is_command_timeout_error(
            "CDP command timed out: Runtime.evaluate"
        ));
        // A genuine navigation failure is NOT a command timeout.
        assert!(!is_command_timeout_error(
            "Navigation failed: net::ERR_NAME_NOT_RESOLVED"
        ));
    }

    #[test]
    fn navigation_committed_requires_same_host_real_page() {
        // Landed on the target host → the nav committed; only the load stalled.
        assert!(navigation_committed(
            "https://sg-git.pwtk.cc/o/r/compare/main...b",
            "https://sg-git.pwtk.cc/o/r/compare/main...b"
        ));
        // Host match is enough even if the path differs (server redirect).
        assert!(navigation_committed(
            "https://sg-git.pwtk.cc/o/r/pulls/5",
            "https://sg-git.pwtk.cc/o/r/compare/main...b"
        ));
        // Still on about:blank / blank / a foreign host → nav never took.
        assert!(!navigation_committed(
            "about:blank",
            "https://sg-git.pwtk.cc/x"
        ));
        assert!(!navigation_committed("", "https://sg-git.pwtk.cc/x"));
        assert!(!navigation_committed(
            "https://example.com/",
            "https://sg-git.pwtk.cc/x"
        ));
    }

    #[test]
    fn a_user_tab_taken_with_adopt_is_not_an_own_tab_for_popup_picking() {
        let mut created_page = page("CREATED");
        created_page.session_id = "cb-tab-11".to_string();
        let mut adopted_page = page("ADOPTED");
        adopted_page.session_id = "cb-tab-22".to_string();
        let created: HashSet<String> = ["CREATED".to_string()].into_iter().collect();
        let own = relay_created_chrome_tabs(&[created_page, adopted_page], &created);
        assert_eq!(own, [11].into_iter().collect::<HashSet<i64>>());
        // So a child the adopted user tab opens is not picked as the session's
        // pop-up: its opener (22) is not ours, and it has no group.
        let tabs = json!([
            { "id": 11, "groupId": -1 },
            { "id": 22, "groupId": -1 },
            { "id": 33, "groupId": -1, "openerTabId": 22, "url": "https://x.example/" }
        ]);
        let before: HashSet<i64> = [11, 22].into_iter().collect();
        let tabs = tabs.as_array().cloned().unwrap();
        assert!(relay_popup_candidate(&tabs, &before, &own).is_none());
        // And a click dispatched to the adopted user tab yields no adoption at
        // all, even when Chrome names our tab as opener and uses our group.
        assert!(clicked_tab_is_created(Some("CREATED"), &created));
        assert!(!clicked_tab_is_created(Some("ADOPTED"), &created));
        assert!(!clicked_tab_is_created(None, &created));
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
        assert!(!active_index_is_drivable(
            &pages,
            Some("USER_A"),
            0,
            &created
        ));
    }

    #[test]
    fn active_owned_when_session_created_the_tab() {
        let pages = vec![page("USER_A"), page("OURS")];
        let mut created = HashSet::new();
        created.insert("OURS".to_string());
        // Active pinned to the tab we created → safe to navigate it.
        assert!(active_index_is_drivable(&pages, Some("OURS"), 1, &created));
        // But pinned to the user's tab → not owned, even though we own another.
        assert!(!active_index_is_drivable(
            &pages,
            Some("USER_A"),
            0,
            &created
        ));
    }

    #[test]
    fn active_not_owned_when_no_pages() {
        let created = HashSet::new();
        assert!(!active_index_is_drivable(&[], None, 0, &created));
    }

    #[test]
    fn external_tab_close_requires_session_created_target() {
        let created = HashSet::from(["OURS".to_string()]);

        assert!(tab_close_is_allowed(true, "OURS", &created));
        assert!(!tab_close_is_allowed(true, "ADOPTED", &created));
        assert!(!tab_close_is_allowed(true, "FOREIGN", &created));
        assert!(tab_close_is_allowed(false, "FOREIGN", &created));
    }

    #[test]
    fn deletion_rights_require_confirmed_target_close() {
        assert!(target_was_closed(&Ok(CloseTargetResult { success: true })));
        assert!(!target_was_closed(&Ok(CloseTargetResult {
            success: false
        })));
        assert!(!target_was_closed(&Err("relay failed".to_string())));
    }

    #[test]
    fn external_tab_switch_requires_owned_target() {
        let owned = HashSet::from(["CREATED".to_string(), "ADOPTED".to_string()]);

        assert!(tab_switch_is_allowed(true, "CREATED", &owned));
        assert!(tab_switch_is_allowed(true, "ADOPTED", &owned));
        assert!(!tab_switch_is_allowed(true, "FOREIGN", &owned));
        assert!(tab_switch_is_allowed(false, "FOREIGN", &owned));
    }

    /// The refusal must hand back a command that `tab adopt` can actually
    /// resolve. `adopt_existing_target` matches a spec against targetIds and URL
    /// substrings, never the per-session `t<N>` ref, so a hint naming `t1` would
    /// be a dead end.
    #[test]
    fn unowned_tab_refusal_hints_adopt_by_target_id() {
        let msg = refuse_unowned_tab_message(1, "FOREIGN-TARGET-ID");

        assert!(msg.contains("did not create or adopt it"), "{msg}");
        assert!(
            msg.contains("chrome-use tab adopt FOREIGN-TARGET-ID"),
            "{msg}"
        );
        assert!(!msg.contains("--adopt"), "{msg}");
        assert!(!msg.contains("adopt t1"), "{msg}");
    }

    #[test]
    fn a_window_with_a_user_tab_is_not_the_agent_window() {
        let owned: HashSet<i64> = [1, 2].into_iter().collect();
        let agent_only = json!([
            { "id": 1, "url": "https://example.com/" },
            { "id": 2, "url": "about:blank" }
        ]);
        assert!(!window_has_unowned_tab(&agent_only, &owned));
        let with_user_tab = json!([
            { "id": 1, "url": "https://example.com/" },
            { "id": 9, "url": "https://mail.example.com/" }
        ]);
        assert!(window_has_unowned_tab(&with_user_tab, &owned));
        // The user's own blank tab is theirs: never exempted by URL.
        let user_blank =
            json!([{ "id": 1, "url": "https://example.com/" }, { "id": 9, "url": "about:blank" }]);
        assert!(window_has_unowned_tab(&user_blank, &owned));
        // Unknown is not empty.
        assert!(window_has_unowned_tab(&Value::Null, &owned));
        assert!(window_has_unowned_tab(
            &json!([{ "url": "https://x.example/" }]),
            &owned
        ));
        assert!(USER_WINDOW_MENU.contains("user's own tabs"));
    }

    #[test]
    fn tab_ownership_distinguishes_created_adopted_and_foreign_targets() {
        let created = HashSet::from(["CREATED".to_string()]);
        let adopted = HashSet::from(["ADOPTED".to_string()]);

        assert_eq!(
            tab_ownership(true, "CREATED", &created, &adopted),
            Some("created")
        );
        assert_eq!(
            tab_ownership(true, "ADOPTED", &created, &adopted),
            Some("adopted")
        );
        assert_eq!(
            tab_ownership(true, "FOREIGN", &created, &adopted),
            Some("foreign")
        );
        assert_eq!(tab_ownership(false, "FOREIGN", &created, &adopted), None);
    }

    #[test]
    fn scoped_but_unpersisted_target_never_gets_deletion_rights() {
        let created = HashSet::from(["PERSISTED".to_string()]);
        let mut adopted = HashSet::new();
        let scoped_live = ["PERSISTED".to_string(), "UNKNOWN".to_string()];

        register_scoped_target_ownership(&scoped_live, &created, &mut adopted);

        assert_eq!(
            tab_ownership(true, "PERSISTED", &created, &adopted),
            Some("created")
        );
        assert_eq!(
            tab_ownership(true, "UNKNOWN", &created, &adopted),
            Some("adopted")
        );
        assert!(tab_close_is_allowed(true, "PERSISTED", &created));
        assert!(!tab_close_is_allowed(true, "UNKNOWN", &created));

        let owned = created.union(&adopted).cloned().collect();
        assert!(tab_switch_is_allowed(true, "PERSISTED", &owned));
        assert!(tab_switch_is_allowed(true, "UNKNOWN", &owned));

        let pages = vec![page("PERSISTED"), page("UNKNOWN")];
        assert_eq!(
            strict_session_index(&pages, Some("PERSISTED"), 1, true, &owned).unwrap(),
            0
        );
        assert_eq!(
            strict_session_index(&pages, Some("UNKNOWN"), 0, true, &owned).unwrap(),
            1
        );
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

    // issue #52: read/click resolution on a guarded external browser must NOT
    // silently fall back to active_page_index when the pin can't be resolved —
    // that's how a command drifts onto a foreign tab. The relay keeps a stable
    // target_id across navs, so a present pin resolves normally; only a
    // genuinely-gone tab errors.
    #[test]
    fn strict_session_index_external_pin_found_resolves() {
        let pages = vec![page("A"), page("B")];
        let owned = HashSet::from(["A".to_string()]);
        assert_eq!(
            strict_session_index(&pages, Some("A"), 1, true, &owned).unwrap(),
            0
        );
    }

    #[test]
    fn strict_session_index_external_dangling_pin_errors() {
        let pages = vec![page("A"), page("Z")];
        let owned = HashSet::from(["A".to_string()]);
        // active_page_index points at the foreign "Z"; lenient would drift there.
        assert!(strict_session_index(&pages, Some("gone"), 1, true, &owned).is_err());
    }

    #[test]
    fn strict_session_index_external_no_pin_resolves_drivable_else_refuses() {
        // No pin on an external browser: resolve only if the active tab is
        // drivable; never drift
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
    fn strict_session_index_launched_browser_dangling_pin_stays_lenient() {
        // A launched browser has no foreign tabs, so behavior stays lenient.
        let pages = vec![page("A"), page("B")];
        let owned = HashSet::new();
        assert_eq!(
            strict_session_index(&pages, Some("gone"), 1, false, &owned).unwrap(),
            1
        );
    }

    // Models `remove_page_by_target_id`'s index + pin update steps purely
    // (BrowserManager needs a live CDP client, so the method itself cannot be
    // unit-constructed).
    fn simulate_remove(
        target_ids: &[&str],
        active_index: usize,
        pinned: &str,
        remove_id: &str,
        on_relay: bool,
    ) -> (Vec<String>, usize, Option<String>) {
        let pos = target_ids.iter().position(|t| *t == remove_id).unwrap();
        let mut pages: Vec<PageInfo> = target_ids.iter().map(|s| page(s)).collect();
        pages.remove(pos);
        let new_active = active_page_index_after_removal(active_index, pos, pages.len());
        let new_pin =
            active_target_after_removal(&pages, new_active, Some(pinned), remove_id, on_relay);
        (
            pages.into_iter().map(|page| page.target_id).collect(),
            new_active,
            new_pin,
        )
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
        let (pages, active, pin) =
            simulate_remove(&["creepjs", "blank"], 0, "creepjs", "blank", true);
        assert_eq!(resolve_active(&pages, active, &pin), "creepjs");
    }

    #[test]
    fn relay_removing_pinned_page_keeps_tombstone_and_refuses_blank_fallback() {
        // pages = [blank, spa(pinned, active)]; a relay detach/destroy removes the
        // SPA target. Keep its stable target id as a tombstone rather than
        // silently retargeting the session to the scratch page (issue #149).
        let (pages, active, pin) = simulate_remove(&["blank", "spa"], 1, "spa", "spa", true);
        assert_eq!(pin.as_deref(), Some("spa"));
        assert_eq!(
            target_id_for_reattach(pin.as_deref()).unwrap(),
            "spa",
            "relay recovery must retry the removed target, not the blank fallback"
        );
        let owned = HashSet::from(["blank".to_string(), "spa".to_string()]);
        let error = strict_session_index(&[page("blank")], pin.as_deref(), active, true, &owned)
            .unwrap_err();
        assert_eq!(error, BOUND_TAB_GONE);
        assert_eq!(resolve_active(&pages, active, &pin), "blank");
    }

    #[test]
    fn launched_browser_removing_pinned_page_keeps_survivor_fallback() {
        // A launched browser has no foreign tabs, so historical fallback remains.
        let (pages, active, pin) = simulate_remove(&["blank", "spa"], 1, "spa", "spa", false);
        assert_eq!(pin.as_deref(), Some("blank"));
        assert_eq!(resolve_active(&pages, active, &pin), "blank");
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
        let m = to_ai_friendly_error("Element not found: #save");
        assert!(
            m.starts_with("Element not found: #save"),
            "selector kept: {m}"
        );
        // directs to snapshot -i (which pierces closed shadow / cross-origin iframes)
        assert!(m.contains("snapshot -i") && m.contains("shadow root"));
    }

    #[test]
    fn test_to_ai_friendly_error_not_found_keeps_a_specific_diagnosis() {
        // A resolver that already explained the miss must not have its
        // explanation replaced by the generic shadow-root/iframe guess (#202).
        let specific = "Element not found: //*[contains(text(),'x')]\nHint: XPath `text()` matches only the FIRST direct text node";
        assert_eq!(to_ai_friendly_error(specific), specific);
    }

    #[test]
    fn test_to_ai_friendly_error_unknown() {
        let msg = "Some custom error message";
        assert_eq!(to_ai_friendly_error(msg), msg);
    }

    #[test]
    fn test_to_ai_friendly_error_top_level_await_hint() {
        let m = to_ai_friendly_error("Evaluation error: ReferenceError: await is not defined");
        assert!(
            m.contains("async () =>"),
            "should suggest the async wrapper"
        );
        assert!(
            m.contains("await is not defined"),
            "keeps the original error"
        );
    }

    #[test]
    fn unconfirmed_action_preserves_no_replay_guidance() {
        let error = "action_outcome_unknown: Runtime.evaluate was not replayed. Original error: stale sessionId cb-tab-1; its tab is gone";
        assert!(!is_stale_target_error(error));
        assert_eq!(to_ai_friendly_error(error), error);
    }

    #[test]
    fn test_to_ai_friendly_error_stale_target_is_actionable() {
        // The cryptic relay/CDP stale-target text (issue #58) becomes recovery
        // guidance: re-open/navigate/adopt, and an explicit note that we did NOT
        // silently retarget (preserving the #8.1 safety contract).
        let raw = "CDP error (Page.captureScreenshot): stale sessionId cb-tab-123 for \
                   Page.captureScreenshot — its tab is gone (closed, navigated across processes)";
        let m = to_ai_friendly_error(raw);
        assert!(m.contains("navigated across processes"));
        assert!(m.contains("open <url>") && m.contains("adopt"));
        assert!(m.contains("did NOT") || m.contains("not silently"));
        assert_ne!(m, raw, "should rewrite the cryptic CDP text");
    }

    #[test]
    fn test_to_ai_friendly_error_relay_timeout_preserves_hung_tab() {
        let m = to_ai_friendly_error(
            "relay timeout after 8000ms: chrome.debugger.sendCommand(Runtime.evaluate)",
        );
        assert!(m.contains("tab inspect <ref>"));
        assert!(m.contains("main thread"));
        assert!(
            !m.contains("reopen it"),
            "a renderer timeout must not imply that the tab disappeared"
        );
    }

    /// Errors containing "not found" but NOT "element" should pass through unchanged.
    /// The relay now appends per-command context to its timeout message
    /// (in-flight count, oldest in-flight, worker age — issue #193). The
    /// friendly rewrite must keep that context visible: it is the only place the
    /// driver ever sees why the command timed out, and the prefix match that
    /// selects this branch must not be thrown off by the suffix.
    #[test]
    fn test_to_ai_friendly_error_relay_timeout_keeps_diagnostics() {
        let m = to_ai_friendly_error(
            "relay timeout after 8000ms: chrome.debugger.sendCommand(Page.navigate). \
             [diag in-flight=4 oldest-in-flight=21000ms worker-age=93000ms]",
        );
        assert!(m.contains("in-flight=4"));
        assert!(m.contains("worker-age=93000ms"));
        assert!(
            m.contains("tab inspect <ref>"),
            "still the relay-timeout hint"
        );
    }

    #[test]
    fn test_to_ai_friendly_error_ignores_non_element_not_found() {
        let err = "Chrome not found. Install Chrome or use --executable-path.";
        assert_eq!(to_ai_friendly_error(err), err);
    }

    #[test]
    fn test_to_ai_friendly_error_catches_no_element() {
        // The original text (which names the selector) is kept and the
        // guidance is appended, instead of replacing the whole message (#202).
        let m = to_ai_friendly_error("No element found for css 'x'");
        assert!(m.starts_with("No element found for css 'x'"), "{m}");
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

    /// #213: navigating the relay's session tab to a `chrome-extension://` URL
    /// made it unrecoverable — not an error, just a dead tab that `tab select`
    /// could not revive. The predicate that names those pages already existed;
    /// it was only used to skip them during auto-connect, never as a guard on
    /// the navigation itself.
    #[test]
    fn privileged_chrome_pages_are_the_ones_navigate_must_refuse() {
        for url in [
            "chrome://settings/",
            "chrome-extension://abc123/popup.html",
            "devtools://devtools/bundled/inspector.html",
        ] {
            assert!(
                is_internal_chrome_target(url),
                "{url} must be recognised as privileged so navigate() refuses it"
            );
        }
        // Ordinary destinations must stay navigable — including about:blank,
        // which `tab new` relies on.
        for url in [
            "https://example.com",
            "http://localhost:3000",
            "about:blank",
        ] {
            assert!(!is_internal_chrome_target(url), "{url} must stay navigable");
        }
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

/// Whether an `eval` script may evaluate to a promise, which must be awaited
/// and so cannot run under `replMode`. An `async` function returns a promise
/// even when it never awaits, so `async` counts on its own.
///
/// `async` and `Promise` match as whole words only, so an identifier such as
/// `asyncData` or `myPromise` keeps replMode and can still be redeclared.
fn script_may_return_promise(script: &str) -> bool {
    if ["await", ".then(", "fetch("]
        .iter()
        .any(|needle| script.contains(needle))
    {
        return true;
    }
    static WORDS: std::sync::OnceLock<regex_lite::Regex> = std::sync::OnceLock::new();
    WORDS
        .get_or_init(|| regex_lite::Regex::new(r"\b(async|Promise)\b").unwrap())
        .is_match(script)
}

#[cfg(test)]
mod eval_mode_tests {
    use super::script_may_return_promise;

    #[test]
    fn async_function_without_await_is_awaited() {
        assert!(script_may_return_promise(
            "(async () => { const x = {a: 1}; return x; })()"
        ));
        assert!(script_may_return_promise(
            "(async function () { let y = 1; return y; })()"
        ));
        assert!(!script_may_return_promise(
            "(() => { const x = {a: 1}; return x; })()"
        ));
        assert!(!script_may_return_promise("let asyncData = 1; asyncData"));
        assert!(!script_may_return_promise("const myPromise = 2; myPromise"));
        assert!(script_may_return_promise("new Promise(r => r(1))"));
    }
}
