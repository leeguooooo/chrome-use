//! What happens to a session's tab group once its task is over.
//!
//! Every agent session has its own ✦ tab group in the user's window. The
//! session name is keyed on the agent's id, so every new conversation gets a
//! new group. The daemon's idle exit leaves its tabs open on purpose (their
//! page state may still be wanted), and only an explicit `close` or `session
//! stop` ever closed them. Agents rarely ran either, so groups piled up in
//! the user's window by the dozen. Three things end that:
//!
//! - **One group per project.** A session's tabs go in its project's group
//!   (`✦ <project>`), shared by every session working on that project
//!   ([`join_project_group`]). A session holds a lease on the tabs it opened;
//!   the lease ends with its daemon, and the project's next session takes
//!   those tabs over instead of opening another group.
//! - **Sweep.** Sessions whose daemon has exited and that saw no command for
//!   [`cleanup_idle`] get their tabs closed ([`sweep`]). A tab the user is
//!   looking at, or has switched to, is left for them instead: ungrouped and
//!   recorded as kept. Every new daemon starts one in the background
//!   ([`maybe_spawn_auto_sweep`]); `tabs cleanup` runs one at once.
//! - **`done`**, in the daemon: the agent ends its task, keeping what the user
//!   needs and closing the rest (see `handle_done` in actions.rs).
//!
//! Deletion rights still come only from a session's created-targets record,
//! closed through the same verified path as `session stop`. A tab the agent
//! adopted, the user lent or a session took with `--force` is never in one.
//! Groups with no record behind them are closed only tab by tab through the
//! extension, whose ownership gate refuses any tab an agent did not create.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::connection::get_socket_dir;
use crate::native::cdp::client::CdpClient;

/// Milliseconds a session must have been idle, with its daemon gone, before
/// the automatic sweep closes its tabs. `0` turns the automatic sweep off.
pub const CLEANUP_ENV: &str = "AGENT_BROWSER_GROUP_CLEANUP_MS";
pub const DEFAULT_CLEANUP_MS: u64 = 30 * 60 * 1000;
/// The automatic sweep runs at most this often per machine.
const AUTO_SWEEP_EVERY: Duration = Duration::from_secs(5 * 60);
/// The activity stamp is rewritten at most this often per daemon.
const ACTIVITY_TOUCH_EVERY: Duration = Duration::from_secs(30);
/// How far past the session's last activity a tab's `lastAccessed` must be to
/// count as the user's visit: the stamp can lag a command by
/// [`ACTIVITY_TOUCH_EVERY`], and the agent's own tab creation and activation
/// happen during commands.
const LAST_ACCESSED_MARGIN: Duration = Duration::from_secs(60);
/// Bound on each extension call the sweep makes.
const CALL_TIMEOUT: Duration = Duration::from_secs(5);
/// Why a tab the sweep left for the user is recorded as kept.
pub const USER_KEPT_REASON: &str = "user";

/// The idle time after which the automatic sweep closes an exited session's
/// tabs, or `None` when it is turned off.
pub fn cleanup_idle() -> Option<Duration> {
    cleanup_idle_from(std::env::var(CLEANUP_ENV).ok().as_deref())
}

fn cleanup_idle_from(raw: Option<&str>) -> Option<Duration> {
    match raw.map(str::trim) {
        None | Some("") => Some(Duration::from_millis(DEFAULT_CLEANUP_MS)),
        Some(raw) => match raw.parse::<u64>() {
            Ok(0) => None,
            Ok(ms) => Some(Duration::from_millis(ms)),
            // A typo must not silently turn cleanup off.
            Err(_) => Some(Duration::from_millis(DEFAULT_CLEANUP_MS)),
        },
    }
}

// ---------------------------------------------------------------------------
// Activity
// ---------------------------------------------------------------------------

fn activity_path(session: &str) -> PathBuf {
    get_socket_dir().join(format!("{session}.activity"))
}

/// Record that `session` just ran a command. Throttled: the sweep measures
/// idleness in tens of minutes, so one write per half minute is plenty.
pub fn touch_activity(session: &str) {
    static LAST: Mutex<Option<Instant>> = Mutex::new(None);
    if let Ok(mut last) = LAST.lock() {
        if last.is_some_and(|at| at.elapsed() < ACTIVITY_TOUCH_EVERY) {
            return;
        }
        *last = Some(Instant::now());
    }
    let _ = std::fs::write(activity_path(session), now_ms().to_string());
}

fn modified(path: &Path) -> Option<SystemTime> {
    std::fs::metadata(path).and_then(|m| m.modified()).ok()
}

/// When `session` last did anything: its activity stamp, or for a session from
/// a build before the stamp existed, the last change to its tab record.
pub fn last_activity(session: &str) -> Option<SystemTime> {
    [
        activity_path(session),
        crate::connection::created_targets_path(session),
    ]
    .iter()
    .filter_map(|p| modified(p))
    .max()
}

fn now_ms() -> u64 {
    millis(SystemTime::now())
}

fn millis(t: SystemTime) -> u64 {
    t.duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

// ---------------------------------------------------------------------------
// Project groups
// ---------------------------------------------------------------------------

/// The project folder a side-panel conversation works in, set by the panel
/// for its agents. It names the project even when the agent runs elsewhere.
pub const PROJECT_DIR_ENV: &str = "CHROME_USE_PROJECT_DIR";

/// The project a session works for: the panel conversation's project folder,
/// else the git repository around `cwd`, else `cwd` itself.
pub fn project_root_from(cwd: &Path, panel_project: Option<&Path>) -> PathBuf {
    if let Some(project) = panel_project.filter(|p| p.is_dir()) {
        return project.to_path_buf();
    }
    cwd.ancestors()
        .find(|dir| dir.join(".git").exists())
        .unwrap_or(cwd)
        .to_path_buf()
}

fn current_project_root() -> Option<PathBuf> {
    let cwd = std::env::current_dir().ok()?;
    let panel = std::env::var_os(PROJECT_DIR_ENV).map(PathBuf::from);
    let root = project_root_from(&cwd, panel.as_deref());
    Some(std::fs::canonicalize(&root).unwrap_or(root))
}

/// A project's group key: stable for the folder, short enough for a file name.
pub fn group_key(root: &Path) -> String {
    let digest = format!("{:x}", Sha256::digest(root.to_string_lossy().as_bytes()));
    digest[..16].to_string()
}

fn groups_dir() -> PathBuf {
    get_socket_dir().join("groups")
}

fn group_path(key: &str) -> PathBuf {
    groups_dir().join(format!("{key}.json"))
}

/// One project's tab group. The title is chosen once, when the group is
/// first made, and read back from here ever after: recomputing it could
/// rename the group under the sessions using it.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq)]
pub struct GroupRecord {
    pub version: u32,
    pub root: String,
    pub title: String,
}

fn read_group(key: &str) -> Option<GroupRecord> {
    std::fs::read_to_string(group_path(key))
        .ok()
        .and_then(|raw| serde_json::from_str(&raw).ok())
}

/// The title for a new project group: the folder's name, or when another
/// project already uses that, the name with its parent folder; failing that,
/// with the start of its key.
pub fn choose_title(root: &Path, key: &str, taken: &HashSet<String>) -> String {
    let name = |p: &Path| {
        p.file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| p.to_string_lossy().into_owned())
    };
    let base = crate::session_title::sanitize_title(&name(root))
        .unwrap_or_else(|| format!("project {}", &key[..6]));
    if !taken.contains(&base) {
        return base;
    }
    if let Some(parent) = root.parent().filter(|p| p.file_name().is_some()) {
        let with_parent = format!("{base} ({})", name(parent));
        if !taken.contains(&with_parent) {
            return with_parent;
        }
    }
    format!("{base} ({})", &key[..6])
}

/// The record of the group for `root`, made under a lock the first time.
fn ensure_group(root: &Path) -> Option<(String, GroupRecord)> {
    let key = group_key(root);
    if let Some(record) = read_group(&key) {
        return Some((key, record));
    }
    std::fs::create_dir_all(groups_dir()).ok()?;
    let lock = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .open(groups_dir().join(".lock"))
        .ok()?;
    lock.lock().ok()?;
    if let Some(record) = read_group(&key) {
        return Some((key, record));
    }
    let taken: HashSet<String> = std::fs::read_dir(groups_dir())
        .ok()?
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().into_string().ok()?;
            read_group(name.strip_suffix(".json")?).map(|g| g.title)
        })
        .collect();
    let record = GroupRecord {
        version: 1,
        root: root.to_string_lossy().into_owned(),
        title: choose_title(root, &key, &taken),
    };
    let encoded = serde_json::to_vec(&record).ok()?;
    std::fs::write(group_path(&key), encoded).ok()?;
    Some((key, record))
}

fn pointer_path(session: &str) -> PathBuf {
    get_socket_dir().join(format!("{session}.group"))
}

/// The project group `session` belongs to, by key.
pub fn session_group(session: &str) -> Option<String> {
    std::fs::read_to_string(pointer_path(session))
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// The title of the project group `session` belongs to.
pub fn group_title(session: &str) -> Option<String> {
    read_group(&session_group(session)?).map(|g| g.title)
}

/// Rename the project group of `session`; every session of the project sees
/// the new title. Returns false when the session has no project group.
pub fn set_group_title(session: &str, title: &str) -> Result<bool, String> {
    let Some(key) = session_group(session) else {
        return Ok(false);
    };
    let mut record = read_group(&key).ok_or("the project group record is missing")?;
    record.title = title.to_string();
    let encoded = serde_json::to_vec(&record).map_err(|e| e.to_string())?;
    std::fs::write(group_path(&key), encoded).map_err(|e| e.to_string())?;
    Ok(true)
}

fn legacy_label_path(session: &str) -> PathBuf {
    get_socket_dir().join(format!("{session}.legacy-label"))
}

/// The group label a session used before it joined its project group, left
/// for its daemon to move the session's open tabs under the project title.
/// Taken once.
pub fn take_legacy_label(session: &str) -> Option<String> {
    let path = legacy_label_path(session);
    let label = std::fs::read_to_string(&path).ok()?;
    let _ = std::fs::remove_file(&path);
    Some(label.trim().to_string()).filter(|s| !s.is_empty())
}

fn moved_path(session: &str) -> PathBuf {
    get_socket_dir().join(format!("{session}.moved-to"))
}

/// Another session as [`expired_leases`] sees it.
#[derive(Debug, Clone)]
pub struct GroupMember {
    pub name: String,
    pub group: Option<String>,
    pub live: bool,
    pub has_record: bool,
    /// Not over even with no daemon: a CLI process is working with it (a
    /// command in flight, a daemon starting), or it ran a command within
    /// [`LEASE_GRACE`]. A daemon can be gone for a moment mid-task (a relay
    /// recovery, a crash) while its agent still counts on its tabs (#687).
    pub busy: bool,
}

/// How long after its last command a session without a daemon keeps its
/// tabs. Longer than any relay recovery; far shorter than the idle exit, so
/// a session that idled out has long passed it.
pub const LEASE_GRACE: Duration = Duration::from_secs(120);

/// The sessions of group `key` whose tabs `own` may take over: their daemon
/// is gone and nothing has used them lately, so their lease on those tabs
/// has ended.
pub fn expired_leases<'a>(own: &str, key: &str, members: &'a [GroupMember]) -> Vec<&'a str> {
    let mut out: Vec<&str> = members
        .iter()
        .filter(|m| {
            m.name != own && !m.live && !m.busy && m.has_record && m.group.as_deref() == Some(key)
        })
        .map(|m| m.name.as_str())
        .collect();
    out.sort_unstable();
    out
}

fn group_members() -> Vec<GroupMember> {
    let Ok(entries) = std::fs::read_dir(get_socket_dir()) else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter_map(|e| {
            e.file_name()
                .to_str()?
                .strip_suffix(".group")
                .map(str::to_string)
        })
        .map(|name| GroupMember {
            group: session_group(&name),
            live: crate::connection::daemon_ready(&name),
            has_record: crate::connection::has_created_targets(&name),
            busy: crate::connection::session_has_client(&name)
                || recently_active(last_activity(&name), SystemTime::now()),
            name,
        })
        .collect()
}

/// Whether `session` still holds its lease on its tabs: its daemon runs, a
/// CLI process is working with it, or it ran a command within
/// [`LEASE_GRACE`]. Another session of the project neither takes over nor
/// adopts its tabs meanwhile (#687).
pub fn lease_held(session: &str) -> bool {
    crate::connection::daemon_ready(session)
        || crate::connection::session_has_client(session)
        || recently_active(last_activity(session), SystemTime::now())
}

/// Whether a session last ran a command within [`LEASE_GRACE`].
pub fn recently_active(last: Option<SystemTime>, now: SystemTime) -> bool {
    last.and_then(|t| now.duration_since(t).ok())
        .is_some_and(|age| age < LEASE_GRACE)
}

/// Whether some other session of `session`'s project group is running.
pub fn group_has_other_live_session(session: &str) -> bool {
    let Some(key) = session_group(session) else {
        return false;
    };
    group_members()
        .iter()
        .any(|m| m.name != session && m.live && m.group.as_deref() == Some(key.as_str()))
}

/// What joining a project group did.
#[derive(Debug, Default)]
pub struct Joined {
    pub title: String,
    /// The session had tabs under its own group from an older build; its
    /// daemon moves them under the project title.
    pub migrated: bool,
    /// Ended sessions of the project whose tabs this session took over.
    pub took_over: Vec<String>,
}

/// Called before a session's daemon starts on the extension relay. Puts the
/// session in its project's group (first time only: a session never changes
/// group), migrates its own record from an older build, and takes over the
/// tabs of the project's ended sessions: their daemons are gone, so their
/// lease on those tabs has ended and this session may reuse them. Legacy
/// records of other sessions are left alone; the sweep handles them.
pub fn join_project_group(session: &str) -> Option<Joined> {
    let root = current_project_root()?;
    let mut joined = Joined::default();
    let key = match session_group(session) {
        Some(key) => key,
        None => {
            let (key, _) = ensure_group(&root)?;
            if crate::connection::has_created_targets(session) {
                // Its tabs sit in a group titled after the session; the label
                // must be read before the pointer changes what it says.
                let old = crate::session_title::display_name(session);
                std::fs::write(legacy_label_path(session), &old).ok()?;
                joined.migrated = true;
            }
            std::fs::write(pointer_path(session), &key).ok()?;
            key
        }
    };
    joined.title = read_group(&key)?.title;
    for other in expired_leases(session, &key, &group_members()) {
        // Never wait: a lock held means that session is starting again.
        let Some(_lock) = crate::connection::try_lock_session_lifecycle(other) else {
            continue;
        };
        if crate::connection::daemon_ready(other) || crate::connection::session_has_client(other) {
            continue;
        }
        if crate::connection::merge_created_targets(other, session).is_ok() {
            let _ = std::fs::write(moved_path(other), session);
            joined.took_over.push(other.to_string());
        }
    }
    Some(joined)
}

/// When this session's tabs were taken over by a later one, which one; the
/// note is given once.
pub fn take_moved_note(session: &str) -> Option<String> {
    let path = moved_path(session);
    let to = std::fs::read_to_string(&path).ok()?;
    let _ = std::fs::remove_file(&path);
    Some(to.trim().to_string()).filter(|s| !s.is_empty())
}

/// A page with nothing on it: a tab an agent opened and never used. It is
/// never the user's, even in front, and closes as soon as its session ends.
pub fn is_blank_url(url: &str) -> bool {
    let url = url.trim().trim_end_matches('/');
    url.is_empty()
        || url == "about:blank"
        || url == "about:newtab"
        || url == "chrome://newtab"
        || url == "chrome://new-tab-page"
        || url == "edge://newtab"
}

// ---------------------------------------------------------------------------
// Same page
// ---------------------------------------------------------------------------

/// Query parameters that only say where a click came from.
const TRACKING_PARAMS: &[&str] = &[
    "fbclid", "gclid", "dclid", "gbraid", "wbraid", "msclkid", "yclid", "igshid", "mc_cid",
    "mc_eid", "_ga", "_gl", "ref_src", "spm",
];

/// A url reduced to what makes it the same page: no `#fragment`, no tracking
/// parameters (`utm_*` and the click ids), the rest of the query in a fixed
/// order, the scheme and host lowercase and a default port dropped (the url
/// parser does those two). Anything that does not parse is compared as given.
pub fn normalize_page_url(raw: &str) -> String {
    let Ok(mut url) = url::Url::parse(raw.trim()) else {
        return raw.trim().to_string();
    };
    if !matches!(url.scheme(), "http" | "https") {
        return raw.trim().to_string();
    }
    url.set_fragment(None);
    let mut pairs: Vec<(String, String)> = url
        .query_pairs()
        .filter(|(k, _)| {
            let k = k.to_ascii_lowercase();
            !k.starts_with("utm_") && !TRACKING_PARAMS.contains(&k.as_str())
        })
        .map(|(k, v)| (k.into_owned(), v.into_owned()))
        .collect();
    pairs.sort();
    if pairs.is_empty() {
        url.set_query(None);
    } else {
        url.query_pairs_mut().clear().extend_pairs(pairs);
    }
    url.to_string()
}

// ---------------------------------------------------------------------------
// Sweep
// ---------------------------------------------------------------------------

/// What one sweep may touch.
#[derive(Debug, Clone, Copy)]
pub struct SweepOptions {
    /// Only sessions idle at least this long; `None` for any exited session.
    pub idle: Option<Duration>,
    /// Also the groups no record explains: under a ✦ title, and the groups
    /// of older builds (⬉, a trailing 🤖, a bare session id).
    pub orphans: bool,
}

/// How a group title marks an agent's group, if it does.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentGroupTitle {
    /// `✦ <label>`, the current mark.
    Current(String),
    /// From a build before ✦: `⬉ <label>`, `<label> 🤖` or a bare session id.
    Legacy(String),
}

/// Whether a tab group title is one chrome-use gave a session's group.
pub fn agent_group_title(title: &str) -> Option<AgentGroupTitle> {
    let title = title.trim();
    if let Some(rest) = title.strip_prefix(crate::session_title::GROUP_MARK) {
        return Some(AgentGroupTitle::Current(rest.trim().to_string()));
    }
    if let Some(rest) = title.strip_prefix('⬉') {
        return Some(AgentGroupTitle::Legacy(rest.trim().to_string()));
    }
    if let Some(rest) = title.strip_suffix('🤖') {
        return Some(AgentGroupTitle::Legacy(rest.trim().to_string()));
    }
    if looks_like_session_id(title) {
        return Some(AgentGroupTitle::Legacy(title.to_string()));
    }
    None
}

/// A derived session name, `cu-<repo>-<6 hex>` or `cu-<6 hex>`.
fn looks_like_session_id(title: &str) -> bool {
    let Some(rest) = title.strip_prefix("cu-") else {
        return false;
    };
    let tag = rest.rsplit('-').next().unwrap_or("");
    tag.len() == 6
        && tag.chars().all(|c| c.is_ascii_hexdigit())
        && rest
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

/// One Chrome tab as the sweep judges it.
#[derive(Debug, Clone, Default)]
pub struct TabFacts {
    pub id: i64,
    /// In front of its window: the user may be reading or typing in it.
    pub active: bool,
    /// Chrome's `lastAccessed`, when it reports one.
    pub last_accessed_ms: Option<u64>,
    /// The extension saw the user switch to it (0.5.77+).
    pub touched: bool,
}

impl TabFacts {
    fn from_tab(tab: &Value, touched: &HashSet<i64>) -> Option<Self> {
        let id = tab.get("id")?.as_i64()?;
        Some(TabFacts {
            id,
            active: tab.get("active").and_then(Value::as_bool).unwrap_or(false),
            last_accessed_ms: tab
                .get("lastAccessed")
                .and_then(Value::as_f64)
                .map(|ms| ms as u64),
            touched: touched.contains(&id),
        })
    }
}

/// Whether the user has this tab: in front of a window now, switched to by
/// the user (as the extension saw it), or — for an extension too old to say —
/// accessed well after the session's last command, which the agent never
/// does.
pub fn user_has_tab(tab: &TabFacts, session_last_activity: Option<SystemTime>) -> bool {
    if tab.active || tab.touched {
        return true;
    }
    match (tab.last_accessed_ms, session_last_activity) {
        (Some(accessed), Some(last)) => {
            accessed > millis(last) + LAST_ACCESSED_MARGIN.as_millis() as u64
        }
        _ => false,
    }
}

/// Whether a session last active at `last` is idle enough for `idle`.
pub fn idle_enough(last: Option<SystemTime>, idle: Option<Duration>, now: SystemTime) -> bool {
    match (idle, last) {
        (None, _) => true,
        // No trace of activity at all: old enough by any measure.
        (Some(_), None) => true,
        (Some(idle), Some(last)) => now.duration_since(last).is_ok_and(|age| age >= idle),
    }
}

/// What a sweep did, for `tabs cleanup` and the log.
#[derive(Debug, Default)]
pub struct SweepReport {
    pub ungrouped: Vec<Value>,
    pub closed: Vec<Value>,
    pub left_for_user: Vec<Value>,
    pub skipped: Vec<Value>,
    pub errors: Vec<String>,
}

impl SweepReport {
    pub fn to_json(&self) -> Value {
        json!({
            "ungrouped": self.ungrouped,
            "closed": self.closed,
            "leftForUser": self.left_for_user,
            "skipped": self.skipped,
            "errors": self.errors,
        })
    }
}

/// What the extension tells the sweep, read once.
struct Browser {
    client: CdpClient,
    tabs: HashMap<i64, Value>,
    groups: Vec<Value>,
    /// target id -> Chrome tab id, for tabs the extension holds attached.
    tab_of_target: HashMap<String, i64>,
    /// Chrome tab ids the extension created for an agent.
    owned: HashSet<i64>,
    touched: HashSet<i64>,
}

async fn call(client: &CdpClient, method: &str, params: Value) -> Result<Value, String> {
    tokio::time::timeout(
        CALL_TIMEOUT,
        client.send_command(method, Some(params), None),
    )
    .await
    .map_err(|_| format!("{method} did not answer"))?
}

async fn chrome(
    client: &CdpClient,
    namespace: &str,
    method: &str,
    args: Value,
) -> Result<Value, String> {
    let out = call(
        client,
        "ABExt.call",
        json!({ "namespace": namespace, "method": method, "args": args }),
    )
    .await?;
    Ok(out.get("result").cloned().unwrap_or(Value::Null))
}

impl Browser {
    async fn read(endpoint: &str) -> Result<Self, String> {
        // A passive connection: the native host does not have the extension
        // re-attach and re-announce every agent tab for it.
        let client = CdpClient::connect_with_headers(
            endpoint,
            Some(vec![("x-chrome-use-diagnostic".into(), "1".into())]),
        )
        .await?;
        let tabs: HashMap<i64, Value> = chrome(&client, "tabs", "query", json!([{}]))
            .await?
            .as_array()
            .ok_or("chrome.tabs.query returned no list")?
            .iter()
            .filter_map(|t| Some((t.get("id")?.as_i64()?, t.clone())))
            .collect();
        let groups = chrome(&client, "tabGroups", "query", json!([{}]))
            .await?
            .as_array()
            .cloned()
            .unwrap_or_default();
        // Without it (an old extension) nothing is known to be an agent's
        // tab, and only recorded tabs are judged.
        let state = call(&client, "ABExt.state", json!({}))
            .await
            .unwrap_or(Value::Null);
        let ids = |key: &str| -> HashSet<i64> {
            state
                .get(key)
                .and_then(Value::as_array)
                .map(|a| a.iter().filter_map(Value::as_i64).collect())
                .unwrap_or_default()
        };
        let tab_of_target = state
            .get("attachedTargets")
            .and_then(Value::as_array)
            .map(|rows| {
                rows.iter()
                    .filter_map(|r| {
                        Some((
                            r.get("targetId")?.as_str()?.to_string(),
                            r.get("tabId")?.as_i64()?,
                        ))
                    })
                    .collect()
            })
            .unwrap_or_default();
        Ok(Browser {
            owned: ids("ownedTabs"),
            touched: ids("userTouchedTabs"),
            client,
            tabs,
            groups,
            tab_of_target,
        })
    }

    /// A tab still on a blank page: nothing in it for anyone.
    fn is_blank(&self, tab_id: i64) -> bool {
        self.tabs.get(&tab_id).is_some_and(|t| {
            let url = t
                .get("url")
                .and_then(Value::as_str)
                .filter(|u| !u.is_empty())
                .or_else(|| t.get("pendingUrl").and_then(Value::as_str))
                .unwrap_or("");
            is_blank_url(url)
        })
    }

    fn facts(&self, tab_id: i64) -> Option<TabFacts> {
        TabFacts::from_tab(self.tabs.get(&tab_id)?, &self.touched)
    }

    fn row(&self, tab_id: i64, session: Option<&str>) -> Value {
        let tab = self.tabs.get(&tab_id);
        json!({
            "tab": tab_id,
            "session": session,
            "title": tab.and_then(|t| t.get("title")).cloned().unwrap_or(Value::Null),
        })
    }

    /// Leave a tab to the user: out of the agent's group. Best effort; an
    /// ungroup the extension refuses leaves the tab where it is, still open.
    async fn ungroup(&self, tab_id: i64) {
        let _ = chrome(&self.client, "tabs", "ungroup", json!([tab_id])).await;
    }
}

fn recorded_sessions() -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(get_socket_dir()) else {
        return Vec::new();
    };
    let mut sessions: Vec<String> = entries
        .flatten()
        .filter_map(|e| {
            e.file_name()
                .to_str()?
                .strip_suffix(crate::connection::CREATED_TARGETS_SUFFIX)
                .map(str::to_string)
        })
        .collect();
    sessions.sort();
    sessions
}

/// Close the tabs of exited, idle sessions on the browser at `endpoint` (an
/// extension relay), leaving the ones the user has to them, then the agent
/// groups no record explains. Never touches a session whose daemon is
/// running.
pub async fn sweep(endpoint: &str, opts: SweepOptions) -> Result<SweepReport, String> {
    if !crate::connect::is_relay_url(endpoint) {
        return Err(
            "tab groups exist only in Chrome through the chrome-use extension; this browser \
             is not connected that way"
                .to_string(),
        );
    }
    let browser = Browser::read(endpoint).await?;
    let mut report = SweepReport::default();
    let now = SystemTime::now();

    for session in recorded_sessions() {
        sweep_session(&browser, endpoint, &session, opts, now, &mut report).await;
    }
    sweep_unrecorded_groups(&browser, opts, &mut report).await;
    ungroup_idle_groups(&browser, &mut report).await;
    Ok(report)
}

async fn sweep_session(
    browser: &Browser,
    endpoint: &str,
    session: &str,
    opts: SweepOptions,
    now: SystemTime,
    report: &mut SweepReport,
) {
    let skip = |report: &mut SweepReport, why: &str| {
        report
            .skipped
            .push(json!({ "session": session, "reason": why }));
    };
    let Some(_lock) = crate::connection::try_lock_session_lifecycle(session) else {
        return skip(report, "starting or stopping right now");
    };
    if crate::connection::daemon_ready(session) {
        return skip(report, "running");
    }
    // Another session of the project is working: these tabs are its to reuse.
    let in_use = group_has_other_live_session(session);
    let last = last_activity(session);
    // A blank tab holds nothing: it closes whenever its session is gone, at
    // once. Everything else waits for the idle time.
    let idle = !in_use && idle_enough(last, opts.idle, now);
    let targets = crate::connection::read_created_targets(session, endpoint);
    if targets.is_empty() {
        // Recorded in another browser or profile; not this sweep's to judge.
        return;
    }
    let mut close = HashSet::new();
    let mut wait = HashSet::new();
    for target in targets {
        let tab = browser.tab_of_target.get(&target).copied();
        if tab.is_some_and(|tab| browser.is_blank(tab)) {
            close.insert(target);
            continue;
        }
        if !idle {
            wait.insert(target);
            continue;
        }
        match tab.and_then(|tab| browser.facts(tab)) {
            Some(tab) if user_has_tab(&tab, last) => {
                browser.ungroup(tab.id).await;
                let _ = crate::connection::set_kept_tab(session, &target, USER_KEPT_REASON);
                report
                    .left_for_user
                    .push(browser.row(tab.id, Some(session)));
            }
            _ => {
                close.insert(target);
            }
        }
    }
    if close.is_empty() && !wait.is_empty() {
        let why = if in_use {
            "its project group is in use"
        } else {
            "used recently"
        };
        return skip(report, why);
    }
    // The Chrome tab ids the record keeps (#899), for the waiting tabs that
    // leave it while the others close and come back afterwards.
    let recorded_tabs = crate::connection::created_target_chrome_tabs(session, endpoint);
    // Only the tabs to close stay in the record while closing (the kept ones
    // leave it for good), so a failure below can never take another with it.
    if let Err(e) = crate::connection::write_created_targets(session, endpoint, &close) {
        report.errors.push(format!("session {session}: {e}"));
        return;
    }
    if close.is_empty() {
        return;
    }
    let rows: HashMap<String, Value> = close
        .iter()
        .map(|t| {
            let row = match browser.tab_of_target.get(t) {
                Some(tab) => browser.row(*tab, Some(session)),
                None => json!({ "target": t, "session": session }),
            };
            (t.clone(), row)
        })
        .collect();
    let closed = crate::native::browser::close_persisted_session_tabs_at(session, endpoint).await;
    // The tabs still waiting out the idle time go back into the record.
    if !wait.is_empty() {
        let mut record = crate::connection::read_created_targets(session, endpoint);
        // Only the waiting tabs' ids: the close just wrote newer ones for
        // the tabs it kept.
        let wait_tabs: HashMap<String, i64> = recorded_tabs
            .into_iter()
            .filter(|(target, _)| wait.contains(target))
            .collect();
        record.extend(wait);
        if let Err(e) = crate::connection::write_created_targets_with_tabs(
            session, endpoint, &record, &wait_tabs,
        ) {
            report.errors.push(format!("session {session}: {e}"));
        }
    }
    match closed {
        Ok(closed) => {
            for target in closed {
                if let Some(row) = rows.get(&target) {
                    report.closed.push(row.clone());
                }
            }
        }
        Err(e) => report.errors.push(format!("session {session}: {e}")),
    }
}

/// The labels of sessions whose daemon is running: their groups are in use.
fn live_labels() -> HashSet<String> {
    let mut labels = HashSet::new();
    // Read-only: `walk_daemons` would also clear the files of dead daemons,
    // bindings included, which the sweep must leave for `session stop`.
    let Ok(entries) = std::fs::read_dir(get_socket_dir()) else {
        return labels;
    };
    let names: HashSet<String> = entries
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().into_string().ok()?;
            name.strip_suffix(".sock")
                .or_else(|| name.strip_suffix(".pid"))
                .map(str::to_string)
        })
        .collect();
    for name in names {
        if !crate::connection::daemon_ready(&name) {
            continue;
        }
        let display = crate::session_title::display_name(&name);
        if let Some(AgentGroupTitle::Current(label)) = agent_group_title(&display) {
            labels.insert(label);
        }
        labels.insert(name);
    }
    labels
}

/// Every tab a session left for the user (`keep`, `done --keep`, or the
/// sweep). The relay still files such a tab under the group it left, so the
/// project's sessions must be told it is not theirs.
pub fn kept_targets() -> HashSet<String> {
    let Ok(entries) = std::fs::read_dir(get_socket_dir()) else {
        return HashSet::new();
    };
    entries
        .flatten()
        .filter_map(|e| e.file_name().into_string().ok())
        .filter_map(|name| name.strip_suffix(".kept-tabs.json").map(str::to_string))
        .flat_map(|session| crate::connection::read_kept_tabs(&session).into_keys())
        .collect()
}

/// Every target some session's record names, as owned or as kept.
fn claimed_targets() -> HashSet<String> {
    let mut claimed = HashSet::new();
    let Ok(entries) = std::fs::read_dir(get_socket_dir()) else {
        return claimed;
    };
    for name in entries
        .flatten()
        .filter_map(|e| e.file_name().into_string().ok())
    {
        if let Some(session) = name.strip_suffix(crate::connection::CREATED_TARGETS_SUFFIX) {
            claimed.extend(crate::connection::created_target_ids(session));
        } else if let Some(session) = name.strip_suffix(".kept-tabs.json") {
            claimed.extend(crate::connection::read_kept_tabs(session).into_keys());
        }
    }
    claimed
}

async fn sweep_unrecorded_groups(browser: &Browser, opts: SweepOptions, report: &mut SweepReport) {
    let live = live_labels();
    let claimed = claimed_targets();
    for group in &browser.groups {
        let (Some(group_id), Some(title)) = (
            group.get("id").and_then(Value::as_i64),
            group.get("title").and_then(Value::as_str),
        ) else {
            continue;
        };
        // Blank tabs go from any agent group nobody works in; the rest only
        // when asked (`tabs cleanup`).
        let label = match agent_group_title(title) {
            Some(AgentGroupTitle::Legacy(label) | AgentGroupTitle::Current(label)) => label,
            None => continue,
        };
        if live.contains(&label) {
            continue;
        }
        let members: Vec<i64> = browser
            .tabs
            .iter()
            .filter(|(_, t)| t.get("groupId").and_then(Value::as_i64) == Some(group_id))
            .map(|(id, _)| *id)
            .collect();
        for tab_id in members {
            // Only a tab an agent created; a tab some record explains was
            // judged with its session above.
            if !browser.owned.contains(&tab_id) {
                continue;
            }
            let recorded = browser
                .tab_of_target
                .iter()
                .any(|(target, tab)| *tab == tab_id && claimed.contains(target));
            if recorded {
                continue;
            }
            let blank = browser.is_blank(tab_id);
            if !blank && !opts.orphans {
                continue;
            }
            let Some(facts) = browser.facts(tab_id) else {
                continue;
            };
            if !blank && user_has_tab(&facts, None) {
                browser.ungroup(tab_id).await;
                report.left_for_user.push(browser.row(tab_id, None));
                continue;
            }
            let row = browser.row(tab_id, None);
            // Out of the group first, so the close leaves no saved group.
            browser.ungroup(tab_id).await;
            match chrome(&browser.client, "tabs", "remove", json!([tab_id])).await {
                Ok(_) => report.closed.push(row),
                Err(e) => report
                    .errors
                    .push(format!("tab {tab_id} in group {title}: {e}")),
            }
        }
    }
}

/// Take the agent's tabs out of every agent group nobody is working in, so
/// the group goes away: Chrome keeps a group that is still open when its
/// window closes or the browser quits as a saved tab group, which no
/// extension API can delete (#692). The tabs wait, ungrouped, for the
/// project's next session or for the sweep; a tab no agent created stays,
/// and keeps its group.
async fn ungroup_idle_groups(browser: &Browser, report: &mut SweepReport) {
    let live = live_labels();
    for group in &browser.groups {
        let (Some(id), Some(title)) = (
            group.get("id").and_then(Value::as_i64),
            group.get("title").and_then(Value::as_str),
        ) else {
            continue;
        };
        let idle = match agent_group_title(title) {
            Some(AgentGroupTitle::Current(label) | AgentGroupTitle::Legacy(label)) => {
                !live.contains(&label)
            }
            None => false,
        };
        if !idle {
            continue;
        }
        let members: Vec<i64> = browser
            .tabs
            .iter()
            .filter(|(tab, t)| {
                t.get("groupId").and_then(Value::as_i64) == Some(id) && browser.owned.contains(tab)
            })
            .map(|(tab, _)| *tab)
            .collect();
        for tab in &members {
            browser.ungroup(*tab).await;
        }
        if !members.is_empty() {
            report
                .ungrouped
                .push(json!({ "group": title, "tabs": members.len() }));
        }
    }
}

// ---------------------------------------------------------------------------
// Automatic sweep
// ---------------------------------------------------------------------------

fn auto_sweep_stamp() -> PathBuf {
    get_socket_dir().join(".group-sweep")
}

/// Whether an automatic sweep is due, given when the last one started.
pub fn auto_sweep_due(last: Option<SystemTime>, now: SystemTime) -> bool {
    match last {
        None => true,
        Some(last) => now
            .duration_since(last)
            .map_or(true, |age| age >= AUTO_SWEEP_EVERY),
    }
}

/// Whether some session with recorded tabs has no running daemon: it may
/// have left a blank tab, which goes at once (the rest of its tabs wait for
/// the idle time). Read from the socket directory alone.
fn sweep_has_work() -> bool {
    recorded_sessions()
        .iter()
        .any(|session| !crate::connection::daemon_ready(session))
}

/// After a new daemon started for `session`: unless one ran in the last few
/// minutes, sweep in a detached process, so the command that started the
/// daemon never waits for it. The child is a plain `chrome-use tabs cleanup
/// --auto` in this session's binding, which reaches the same Chrome profile.
pub fn maybe_spawn_auto_sweep(session: &str) {
    if cleanup_idle().is_none() {
        return;
    }
    let stamp = auto_sweep_stamp();
    if !auto_sweep_due(modified(&stamp), SystemTime::now()) {
        return;
    }
    // Nothing ended long enough ago: no process, no connection to Chrome.
    if !sweep_has_work() {
        return;
    }
    if std::fs::write(&stamp, now_ms().to_string()).is_err() {
        return;
    }
    let Ok(exe) = std::env::current_exe() else {
        return;
    };
    let mut cmd = std::process::Command::new(exe);
    cmd.args(["--session", session, "tabs", "cleanup", "--auto", "--json"])
        .env_remove("AGENT_BROWSER_DAEMON")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        // SAFETY: setsid is async-signal-safe; the child leaves this process
        // group so the agent's terminal never waits on it.
        unsafe {
            cmd.pre_exec(|| {
                libc::setsid();
                Ok(())
            });
        }
    }
    let _ = cmd.spawn();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cleanup_idle_defaults_and_zero_disables() {
        assert_eq!(
            cleanup_idle_from(None),
            Some(Duration::from_millis(DEFAULT_CLEANUP_MS))
        );
        assert_eq!(cleanup_idle_from(Some("0")), None);
        assert_eq!(
            cleanup_idle_from(Some("60000")),
            Some(Duration::from_secs(60))
        );
        // A typo keeps the default rather than turning cleanup off.
        assert_eq!(
            cleanup_idle_from(Some("30m")),
            Some(Duration::from_millis(DEFAULT_CLEANUP_MS))
        );
    }

    /// Every terminal and agent in one repository lands in one project; a
    /// panel conversation's project folder wins over where its agent runs.
    #[test]
    fn project_is_panel_folder_then_git_root_then_cwd() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("image-use");
        let deep = repo.join("src/bin");
        std::fs::create_dir_all(&deep).unwrap();
        std::fs::create_dir_all(repo.join(".git")).unwrap();
        assert_eq!(project_root_from(&deep, None), repo);
        assert_eq!(project_root_from(&repo, None), repo);
        let plain = tmp.path().join("notes");
        std::fs::create_dir_all(&plain).unwrap();
        assert_eq!(project_root_from(&plain, None), plain);
        assert_eq!(project_root_from(&deep, Some(&plain)), plain);
        // A panel folder that is gone is no project.
        assert_eq!(
            project_root_from(&deep, Some(&tmp.path().join("missing"))),
            repo
        );
        assert_eq!(group_key(&repo), group_key(&repo));
        assert_ne!(group_key(&repo), group_key(&plain));
    }

    #[test]
    fn a_title_another_project_has_gets_its_parent_folder() {
        let key = group_key(Path::new("/w/a/image-use"));
        let none = HashSet::new();
        assert_eq!(
            choose_title(Path::new("/w/a/image-use"), &key, &none),
            "image-use"
        );
        let taken: HashSet<String> = ["image-use".to_string()].into();
        assert_eq!(
            choose_title(Path::new("/w/a/image-use"), &key, &taken),
            "image-use (a)"
        );
        let both: HashSet<String> = ["image-use".to_string(), "image-use (a)".to_string()].into();
        assert_eq!(
            choose_title(Path::new("/w/a/image-use"), &key, &both),
            format!("image-use ({})", &key[..6])
        );
    }

    fn member(name: &str, group: &str, live: bool, record: bool) -> GroupMember {
        GroupMember {
            name: name.to_string(),
            group: Some(group.to_string()),
            live,
            has_record: record,
            busy: false,
        }
    }

    /// Only ended sessions of the same project give their tabs up; a running
    /// one keeps its lease.
    #[test]
    fn only_ended_sessions_of_the_project_give_up_their_tabs() {
        let members = vec![
            member("cu-a-old", "K", false, true),
            member("cu-a-live", "K", true, true),
            member("cu-a-empty", "K", false, false),
            member("cu-b", "M", false, true),
            member("cu-a-me", "K", false, true),
        ];
        assert_eq!(expired_leases("cu-a-me", "K", &members), vec!["cu-a-old"]);
        assert!(expired_leases("cu-x", "N", &members).is_empty());
    }

    /// A session whose daemon is gone but whose agent is still at work (a
    /// command in flight, or one just now) keeps its tabs (#687).
    #[test]
    fn a_session_still_at_work_keeps_its_tabs_without_a_daemon() {
        let mut busy = member("cu-a-busy", "K", false, true);
        busy.busy = true;
        let members = vec![busy, member("cu-a-old", "K", false, true)];
        assert_eq!(expired_leases("cu-a-me", "K", &members), vec!["cu-a-old"]);
        let now = SystemTime::now();
        assert!(recently_active(Some(now - Duration::from_secs(5)), now));
        assert!(!recently_active(
            Some(now - LEASE_GRACE - Duration::from_secs(1)),
            now
        ));
        assert!(!recently_active(None, now));
    }

    #[test]
    fn blank_pages_are_recognised() {
        for url in [
            "",
            "about:blank",
            "chrome://newtab/",
            "chrome://new-tab-page/",
            " about:newtab ",
        ] {
            assert!(is_blank_url(url), "{url}");
        }
        for url in [
            "https://example.com/",
            "chrome://settings/",
            "about:blank#x",
        ] {
            assert!(!is_blank_url(url), "{url}");
        }
    }

    #[test]
    fn page_urls_compare_without_fragments_and_tracking() {
        let same = |a: &str, b: &str| normalize_page_url(a) == normalize_page_url(b);
        assert!(same(
            "https://Example.com/a?x=1#top",
            "https://example.com/a?x=1"
        ));
        assert!(same(
            "https://example.com/a?utm_source=t&x=1&fbclid=z",
            "https://example.com/a?x=1"
        ));
        assert!(same(
            "https://example.com/a?b=2&a=1",
            "https://example.com/a?a=1&b=2"
        ));
        assert!(same("https://example.com:443/", "https://example.com"));
        assert!(same(
            "https://example.com/a?gclid=1",
            "https://example.com/a"
        ));
        assert!(!same(
            "https://example.com/a?x=1",
            "https://example.com/a?x=2"
        ));
        assert!(!same("https://example.com/a", "https://example.com/b"));
        assert!(!same("http://example.com/a", "https://example.com/a"));
        assert!(!same("https://example.com/a?x=1", "https://example.com/a"));
        // Not a url: compared as written.
        assert_eq!(normalize_page_url("about:blank"), "about:blank");
    }

    #[test]
    fn group_titles_are_recognised_with_every_mark_we_ever_used() {
        assert_eq!(
            agent_group_title("✦ 查快递"),
            Some(AgentGroupTitle::Current("查快递".into()))
        );
        assert_eq!(
            agent_group_title("⬉ cu-repo-3f9a1c"),
            Some(AgentGroupTitle::Legacy("cu-repo-3f9a1c".into()))
        );
        assert_eq!(
            agent_group_title("cu-repo-3f9a1c 🤖"),
            Some(AgentGroupTitle::Legacy("cu-repo-3f9a1c".into()))
        );
        assert_eq!(
            agent_group_title("cu-chrome-use-3f9a1c"),
            Some(AgentGroupTitle::Legacy("cu-chrome-use-3f9a1c".into()))
        );
        // The user's own groups.
        assert_eq!(agent_group_title("Work"), None);
        assert_eq!(agent_group_title("cu-later"), None);
        assert_eq!(agent_group_title("cu-Repo-3f9a1c"), None);
    }

    #[test]
    fn a_tab_in_front_or_switched_to_is_the_users() {
        let last = SystemTime::now() - Duration::from_secs(3600);
        let base = TabFacts {
            id: 1,
            ..Default::default()
        };
        assert!(!user_has_tab(&base, Some(last)));
        assert!(user_has_tab(
            &TabFacts {
                active: true,
                ..base.clone()
            },
            Some(last)
        ));
        assert!(user_has_tab(
            &TabFacts {
                touched: true,
                ..base.clone()
            },
            Some(last)
        ));
        // Accessed long after the agent's last command: the user's visit.
        let visited = millis(last) + 10 * 60 * 1000;
        assert!(user_has_tab(
            &TabFacts {
                last_accessed_ms: Some(visited),
                ..base.clone()
            },
            Some(last)
        ));
        // Within the margin it could be the agent's own creation.
        let created = millis(last) + 20 * 1000;
        assert!(!user_has_tab(
            &TabFacts {
                last_accessed_ms: Some(created),
                ..base.clone()
            },
            Some(last)
        ));
        // Without a session to compare with, lastAccessed proves nothing.
        assert!(!user_has_tab(
            &TabFacts {
                last_accessed_ms: Some(visited),
                ..base
            },
            None
        ));
    }

    #[test]
    fn idleness_is_measured_from_the_last_activity() {
        let now = SystemTime::now();
        let idle = Some(Duration::from_secs(1800));
        assert!(!idle_enough(Some(now - Duration::from_secs(60)), idle, now));
        assert!(idle_enough(
            Some(now - Duration::from_secs(1801)),
            idle,
            now
        ));
        assert!(idle_enough(None, idle, now));
        assert!(idle_enough(Some(now), None, now));
    }

    #[test]
    fn automatic_sweeps_are_spaced_out() {
        let now = SystemTime::now();
        assert!(auto_sweep_due(None, now));
        assert!(!auto_sweep_due(Some(now - Duration::from_secs(10)), now));
        assert!(auto_sweep_due(Some(now - AUTO_SWEEP_EVERY), now));
    }
}
