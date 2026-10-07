//! Carry a session across a daemon restart caused by an upgrade.
//!
//! When the CLI finds a daemon from a different version it replaces it. Before
//! this module, the old daemon was stopped with SIGTERM, which closes every tab
//! the session created, and the new daemon started on a blank tab with no refs:
//! an agent mid-task got "Unknown ref … NO snapshot refs at all" on a blank page
//! after each release.
//!
//! Now the old daemon is asked to hand over first. It writes
//! `<session>.upgrade-handoff.json` (its active tab, URL and ref map) and exits
//! without closing tabs. The new daemon reads the file once, selects the same
//! tab again and restores the refs, or says plainly what could not be carried
//! over. A daemon from before this change cannot answer the request; the CLI
//! then records what it can learn from `tab_list` and stops it without the tab
//! sweep (see `connection::ensure_daemon_with_lifecycle_lock`).

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::native::element::PersistedRefMap;

/// Daemon action that asks a running daemon to write the handoff file and exit
/// without closing the session's tabs.
pub const HANDOFF_ACTION: &str = "upgrade_handoff";

/// Content of the `<session>.caps` sidecar of a daemon that understands
/// [`HANDOFF_ACTION`]. An older daemon has no such file, and sending it an
/// action it does not know could make it open a browser just to refuse.
pub const HANDOFF_CAPABILITY: &str = "upgrade-handoff";

/// A handoff older than this is from some earlier restart that never got
/// picked up; acting on it could reselect a tab long after the fact.
const HANDOFF_MAX_AGE: Duration = Duration::from_secs(15 * 60);

/// Test-only override of the version a daemon reports and the CLI expects.
/// Lets one build play both sides of an upgrade.
pub const VERSION_OVERRIDE_ENV: &str = "AGENT_BROWSER_DAEMON_VERSION_OVERRIDE";

/// The version this binary writes into `<session>.version` and compares
/// against: the crate version unless [`VERSION_OVERRIDE_ENV`] is set.
pub fn build_version() -> String {
    std::env::var(VERSION_OVERRIDE_ENV)
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| env!("CARGO_PKG_VERSION").to_string())
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpgradeHandoff {
    /// Version of the daemon being replaced ("" when unknown).
    pub from_version: String,
    /// Version of the daemon that replaces it.
    pub to_version: String,
    /// The session's active tab, only when the session owned it (created or
    /// adopted). The new daemon never selects any other tab.
    #[serde(default)]
    pub target_id: Option<String>,
    /// The active tab's URL when the handoff was written.
    #[serde(default)]
    pub url: Option<String>,
    /// Whether the old daemon exited without closing the session's tabs. When
    /// false the tab was closed with it, and the new daemon reopens `url`.
    #[serde(default)]
    pub tab_kept: bool,
    /// The old daemon's refs for `target_id`, when it could provide them.
    #[serde(default)]
    pub refs: Option<PersistedRefMap>,
    /// Unix milliseconds when written; stale files are ignored.
    #[serde(default)]
    pub written_at_ms: u64,
}

impl UpgradeHandoff {
    pub fn new(from_version: &str, to_version: &str) -> Self {
        Self {
            from_version: from_version.to_string(),
            to_version: to_version.to_string(),
            written_at_ms: now_ms(),
            ..Self::default()
        }
    }

    fn versions(&self) -> String {
        let from = if self.from_version.is_empty() {
            "an older version"
        } else {
            self.from_version.as_str()
        };
        format!("{from} → {}", self.to_version)
    }

    /// The opening of every message about this restart.
    pub fn upgraded_prefix(&self) -> String {
        format!(
            "chrome-use was upgraded ({}) and its daemon restarted",
            self.versions()
        )
    }

    /// Why `@eN` no longer resolves after the restart, for the first ref
    /// command. `tab_resumed` says whether the new daemon is on the same tab.
    pub fn refs_lost_note(&self, tab_resumed: bool) -> String {
        let url = self
            .url
            .as_deref()
            .filter(|u| !u.is_empty())
            .unwrap_or("its page");
        let tab = if tab_resumed {
            format!("this session's tab was kept ({url})")
        } else if self.url.as_deref().is_some_and(|u| !u.is_empty()) {
            format!(
                "this session's previous tab could not be kept, so {url} was reopened in a new tab"
            )
        } else {
            "this session's previous tab could not be kept".to_string()
        };
        format!(
            "{}; {tab}, but refs from before the upgrade are gone — run `snapshot -i` and use its refs.",
            self.upgraded_prefix()
        )
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

pub fn handoff_path_in(dir: &Path, session: &str) -> PathBuf {
    dir.join(format!("{session}.upgrade-handoff.json"))
}

pub fn caps_path_in(dir: &Path, session: &str) -> PathBuf {
    dir.join(format!("{session}.caps"))
}

/// Write atomically so a reader never sees half a file.
pub fn write_in(dir: &Path, session: &str, handoff: &UpgradeHandoff) -> Result<(), String> {
    fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    let path = handoff_path_in(dir, session);
    let encoded = serde_json::to_vec(handoff).map_err(|e| e.to_string())?;
    let tmp = path.with_extension(format!("json.{}.tmp", std::process::id()));
    let result = (|| {
        let mut file = fs::File::create(&tmp).map_err(|e| e.to_string())?;
        file.write_all(&encoded).map_err(|e| e.to_string())?;
        file.sync_all().map_err(|e| e.to_string())?;
        fs::rename(&tmp, &path).map_err(|e| e.to_string())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result
}

/// Read without consuming. Missing, malformed or stale reads as `None`.
pub fn peek_in(dir: &Path, session: &str) -> Option<UpgradeHandoff> {
    let raw = fs::read_to_string(handoff_path_in(dir, session)).ok()?;
    let handoff: UpgradeHandoff = serde_json::from_str(&raw).ok()?;
    let age = now_ms().saturating_sub(handoff.written_at_ms);
    if age > HANDOFF_MAX_AGE.as_millis() as u64 {
        return None;
    }
    Some(handoff)
}

/// Read once and delete, so a later daemon start never acts on it again.
pub fn take_in(dir: &Path, session: &str) -> Option<UpgradeHandoff> {
    let handoff = peek_in(dir, session);
    let _ = fs::remove_file(handoff_path_in(dir, session));
    handoff
}

pub fn write(session: &str, handoff: &UpgradeHandoff) -> Result<(), String> {
    write_in(&crate::connection::get_socket_dir(), session, handoff)
}

pub fn take(session: &str) -> Option<UpgradeHandoff> {
    take_in(&crate::connection::get_socket_dir(), session)
}

/// Record that this daemon understands [`HANDOFF_ACTION`].
pub fn advertise_capability(session: &str) {
    let path = caps_path_in(&crate::connection::get_socket_dir(), session);
    let _ = fs::write(path, HANDOFF_CAPABILITY);
}

pub fn daemon_supports_handoff(session: &str) -> bool {
    fs::read_to_string(caps_path_in(&crate::connection::get_socket_dir(), session))
        .map(|caps| caps.split_whitespace().any(|c| c == HANDOFF_CAPABILITY))
        .unwrap_or(false)
}

/// The active tab of a `tab_list` reply, when the session owns it: its
/// targetId and URL. A tab the session neither created nor adopted is never
/// returned, so the new daemon cannot be pointed at the user's tab.
pub fn owned_active_tab(tab_list_data: &Value) -> Option<(String, String)> {
    tab_list_data
        .get("tabs")?
        .as_array()?
        .iter()
        .find(|tab| tab.get("active").and_then(Value::as_bool) == Some(true))
        .filter(|tab| {
            matches!(
                tab.get("ownership").and_then(Value::as_str),
                Some("created") | Some("adopted")
            )
        })
        .and_then(|tab| {
            let target = tab.get("targetId")?.as_str()?.to_string();
            let url = tab
                .get("url")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            Some((target, url))
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::native::element::{PersistedRefMap, RefMap};
    use serde_json::json;

    fn sample_refs() -> PersistedRefMap {
        let mut map = RefMap::new();
        map.begin_snapshot();
        let r = map.snapshot_ref(Some(42), None, "link", "More information...");
        map.add(r, Some(42), "link", "More information...", None);
        map.export().expect("a snapshot was taken")
    }

    #[test]
    fn handoff_round_trips_and_is_read_once() {
        let dir = tempfile::tempdir().unwrap();
        let mut h = UpgradeHandoff::new("1.5.172", "1.5.173");
        h.target_id = Some("T1".into());
        h.url = Some("https://example.com/".into());
        h.tab_kept = true;
        h.refs = Some(sample_refs());
        write_in(dir.path(), "s", &h).unwrap();

        assert_eq!(peek_in(dir.path(), "s").as_ref(), Some(&h));
        assert_eq!(take_in(dir.path(), "s"), Some(h));
        assert_eq!(take_in(dir.path(), "s"), None, "consumed by the first read");
        assert!(!handoff_path_in(dir.path(), "s").exists());
    }

    #[test]
    fn a_stale_or_malformed_handoff_is_ignored_and_removed() {
        let dir = tempfile::tempdir().unwrap();
        let mut h = UpgradeHandoff::new("1", "2");
        h.written_at_ms = 1;
        write_in(dir.path(), "old", &h).unwrap();
        assert_eq!(take_in(dir.path(), "old"), None);
        assert!(!handoff_path_in(dir.path(), "old").exists());

        fs::write(handoff_path_in(dir.path(), "bad"), "{not json").unwrap();
        assert_eq!(take_in(dir.path(), "bad"), None);
        assert!(!handoff_path_in(dir.path(), "bad").exists());
    }

    #[test]
    fn refs_lost_note_names_versions_url_and_the_next_step() {
        let mut h = UpgradeHandoff::new("1.5.172", "1.5.173");
        h.url = Some("https://example.com/".into());
        assert_eq!(
            h.refs_lost_note(true),
            "chrome-use was upgraded (1.5.172 → 1.5.173) and its daemon restarted; this \
             session's tab was kept (https://example.com/), but refs from before the upgrade \
             are gone — run `snapshot -i` and use its refs."
        );
        let gone = h.refs_lost_note(false);
        assert!(gone.contains("previous tab could not be kept"), "{gone}");
        assert!(gone.contains("https://example.com/ was reopened"), "{gone}");
        assert!(!gone.contains("NO snapshot refs"), "{gone}");

        let unknown = UpgradeHandoff::new("", "1.5.173").refs_lost_note(false);
        assert!(unknown.starts_with("chrome-use was upgraded (an older version → 1.5.173)"));
    }

    #[test]
    fn owned_active_tab_never_returns_a_foreign_tab() {
        let data = json!({ "tabs": [
            { "targetId": "A", "url": "https://user.example/", "active": false, "ownership": "foreign" },
            { "targetId": "B", "url": "https://example.com/", "active": true, "ownership": "created" },
        ]});
        assert_eq!(
            owned_active_tab(&data),
            Some(("B".into(), "https://example.com/".into()))
        );

        let adopted = json!({ "tabs": [
            { "targetId": "C", "url": "https://x/", "active": true, "ownership": "adopted" },
        ]});
        assert_eq!(owned_active_tab(&adopted).map(|t| t.0), Some("C".into()));

        let foreign = json!({ "tabs": [
            { "targetId": "A", "url": "https://user.example/", "active": true, "ownership": "foreign" },
        ]});
        assert_eq!(owned_active_tab(&foreign), None);

        // A launched browser reports no ownership at all: nothing to keep.
        let launched = json!({ "tabs": [
            { "targetId": "L", "url": "https://x/", "active": true },
        ]});
        assert_eq!(owned_active_tab(&launched), None);
    }

    #[test]
    fn build_version_honours_the_test_override() {
        let guard = crate::test_utils::EnvGuard::new(&[VERSION_OVERRIDE_ENV]);
        guard.remove(VERSION_OVERRIDE_ENV);
        assert_eq!(build_version(), env!("CARGO_PKG_VERSION"));
        guard.set(VERSION_OVERRIDE_ENV, " 9.9.9-test ");
        assert_eq!(build_version(), "9.9.9-test");
        guard.set(VERSION_OVERRIDE_ENV, "");
        assert_eq!(build_version(), env!("CARGO_PKG_VERSION"));
    }
}
