//! Opening a session's first tab without leaving anything behind (#486).
//!
//! A session that has no tab of its own creates one while it connects. On the
//! extension relay that used to happen without asking whether the Chrome
//! profile had a window: with none, Chrome opened one in front of the user, and
//! on a real Chrome the setup that followed hung until the client stopped the
//! daemon, leaving debugger-attached blank tabs that held back the extension's
//! update.
//!
//! The rules here:
//!
//! - Before creating anything on the relay, the profile must show at least one
//!   real `normal` window. None is `profile not open`; an answer that cannot be
//!   read (or contradicts itself) is `profile window unavailable`. Both create
//!   nothing and are not retried automatically.
//! - The whole sequence (window check, create, setup, and cleanup) runs under
//!   one deadline, with a share reserved for cleanup, well under the client's
//!   45 s.
//! - A tab is gone only when `close`'s own verifier says so (#496,
//!   `close_and_verify_targets`): over the relay the extension's versioned
//!   `ABExt.tabPresence`; on a direct CDP connection Chrome's own target list.
//!   A `Target.closeTarget` acknowledgement proves nothing. Whatever is not confirmed gone keeps its recorded delete right,
//!   and the error says so instead of inviting another `open`.

use serde_json::Value;
use std::collections::HashSet;
use std::time::Duration;

/// The overall deadline for opening a session's first tab: window check,
/// `Target.createTarget`, attach and domain setup, and cleanup if any of it
/// fails. Under the client's 45 s "session unresponsive" deadline, so the
/// daemon reports and cleans up itself instead of being stopped mid-way.
pub const FIRST_TAB_TOTAL_BUDGET: Duration = Duration::from_secs(30);

/// Held back from [`FIRST_TAB_TOTAL_BUDGET`] for closing a tab that could not
/// be set up and reading back that it is gone. Window check, create and setup
/// share what is left.
pub const FIRST_TAB_CLEANUP_RESERVE: Duration = Duration::from_secs(10);

/// The most the window check may take.
pub const WINDOW_CHECK_TIMEOUT: Duration = Duration::from_secs(5);

/// The profile has no window open. Nothing was created.
pub const PROFILE_NOT_OPEN: &str = "profile not open: the Chrome profile this session uses has \
     no open window, and chrome-use does not open one itself (that would bring Chrome up in \
     front of you). Open a window in that profile first (pick it in Chrome's profile menu, or \
     press Cmd+N / Ctrl+N in it), then rerun the same command. Nothing was opened or attached.";

/// Prefixes of the first-tab failures. The error taxonomy keys on them, and
/// none of them is retried by the relay-revive loop: the relay answered.
pub const PROFILE_WINDOW_UNAVAILABLE: &str = "profile window unavailable";
pub const FIRST_TAB_OUTCOME_UNKNOWN: &str = "first tab outcome unknown";
pub const FIRST_TAB_CLEANUP_INCOMPLETE: &str = "first tab cleanup incomplete";
pub const FIRST_TAB_SETUP_FAILED: &str = "first tab setup failed";

/// Whether `error` is one of the refusals above (or `profile not open`).
pub fn is_first_tab_refusal(error: &str) -> bool {
    error.starts_with("profile not open")
        || [
            PROFILE_WINDOW_UNAVAILABLE,
            FIRST_TAB_OUTCOME_UNKNOWN,
            FIRST_TAB_CLEANUP_INCOMPLETE,
            FIRST_TAB_SETUP_FAILED,
        ]
        .iter()
        .any(|prefix| error.starts_with(prefix))
}

fn window_unavailable(why: &str, hint: &str) -> String {
    format!(
        "{PROFILE_WINDOW_UNAVAILABLE}: could not tell whether the Chrome profile this session \
         uses has an open window ({why}). Unknown is not open, so nothing was opened or \
         attached.{hint}"
    )
}

/// Decide from a `chrome.windows.getAll({windowTypes: ['normal']})` answer.
/// Open needs at least one real window: an object with a non-negative integer
/// `id` and `type` `normal`, and no entry may be unreadable, of another type
/// (the query asked for normal windows only), or share an id with another.
/// An empty list is `profile not open`; anything else is unavailable.
pub fn profile_window_verdict(windows: Result<Value, String>) -> Result<(), String> {
    let list = match windows {
        Ok(Value::Array(list)) => list,
        Ok(other) => {
            let shown: String = other.to_string().chars().take(120).collect();
            return Err(window_unavailable(
                &format!("the extension answered {shown}, not a window list"),
                " Rerun once the extension answers normally.",
            ));
        }
        Err(e) => {
            let lower = e.to_lowercase();
            let hint = if lower.contains("wasn't found") || lower.contains("method not found") {
                " ab-connect is older than 0.5.25: update it from chrome://extensions, then rerun."
            } else {
                " Rerun once the extension answers."
            };
            return Err(window_unavailable(&e, hint));
        }
    };
    if list.is_empty() {
        return Err(PROFILE_NOT_OPEN.to_string());
    }
    let mut ids = HashSet::new();
    for entry in &list {
        let id = entry
            .get("id")
            .and_then(Value::as_i64)
            .filter(|id| *id >= 0);
        let kind = entry.get("type").and_then(Value::as_str);
        let shown: String = entry.to_string().chars().take(80).collect();
        let Some(id) = id else {
            return Err(window_unavailable(
                &format!("a window without a valid id: {shown}"),
                " Rerun; if this repeats, reload ab-connect from chrome://extensions.",
            ));
        };
        if kind != Some("normal") {
            return Err(window_unavailable(
                &format!("asked for normal windows, got {shown}"),
                " Rerun; if this repeats, reload ab-connect from chrome://extensions.",
            ));
        }
        if !ids.insert(id) {
            return Err(window_unavailable(
                &format!("window {id} is listed twice"),
                " Rerun; if this repeats, reload ab-connect from chrome://extensions.",
            ));
        }
    }
    Ok(())
}

/// What `Target.createTarget` answered. Only an explicit refusal from the
/// extension proves no tab was made; an error that may be a lost answer (a
/// timeout, a dropped connection) leaves the outcome unknown.
pub fn create_failure(error: &str) -> String {
    if error.contains("could not open the background agent window") {
        return error.to_string();
    }
    format!(
        "{FIRST_TAB_OUTCOME_UNKNOWN}: Chrome did not confirm the tab chrome-use asked for \
         ({error}), so a blank tab may have been opened in that profile without this session \
         recording it. Nothing else was created. Before rerunning, look for a blank tab in that \
         profile's windows and close it if it is there; `chrome-use tab list` shows what this \
         session can see."
    )
}

/// How cleaning up a tab that could not be set up ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Cleanup {
    /// Chrome confirms the tab is gone.
    Gone,
    /// The tab may still be open: why it is not confirmed gone.
    NotConfirmed { why: String },
}

/// First tabs that may still be open and whose delete right could not be
/// saved: `(endpoint, target)`. This daemon keeps them, so the right is not
/// lost with the failed connection: the next connection to that endpoint
/// counts them as created (and saves them again), and `close` without a
/// connection closes them.
static UNSAVED: std::sync::Mutex<Vec<(String, String)>> = std::sync::Mutex::new(Vec::new());

fn unsaved() -> std::sync::MutexGuard<'static, Vec<(String, String)>> {
    UNSAVED
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Keep the delete right for `target` on `endpoint` in this daemon.
pub fn hold_unsaved(endpoint: &str, target: &str) {
    let mut held = unsaved();
    if !held.iter().any(|(e, t)| e == endpoint && t == target) {
        held.push((endpoint.to_string(), target.to_string()));
    }
}

/// The held targets on `endpoint`.
pub fn unsaved_for(endpoint: &str) -> HashSet<String> {
    unsaved()
        .iter()
        .filter(|(e, _)| e == endpoint)
        .map(|(_, t)| t.clone())
        .collect()
}

/// Drop the rights for exactly `targets` on `endpoint` (saved to disk, or
/// confirmed closed). Target ids are opaque per browser: the same id held for
/// another endpoint is a different tab and stays held.
pub fn release_unsaved(endpoint: &str, targets: &HashSet<String>) {
    unsaved().retain(|(e, t)| !(e == endpoint && targets.contains(t)));
}

/// Chrome tab ids a first-tab cleanup saw for targets it could not confirm
/// gone: `(endpoint, target, tab)`. A tab id belongs to one browser, so it is
/// only ever used for the same endpoint and target. A later `close` passes it
/// to the verifier, which can then prove the tab gone (#496 needs the tab id
/// once the target itself is no longer registered).
static TAB_IDS: std::sync::Mutex<Vec<(String, String, i64)>> = std::sync::Mutex::new(Vec::new());

fn tab_ids() -> std::sync::MutexGuard<'static, Vec<(String, String, i64)>> {
    TAB_IDS
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Keep the Chrome tab id `tab` the verifier saw for `target` on `endpoint`.
pub fn remember_tab_id(endpoint: &str, target: &str, tab: i64) {
    let mut ids = tab_ids();
    ids.retain(|(e, t, _)| !(e == endpoint && t == target));
    ids.push((endpoint.to_string(), target.to_string(), tab));
}

/// The kept tab ids on `endpoint`, by target.
pub fn tab_ids_for(endpoint: &str) -> std::collections::HashMap<String, i64> {
    tab_ids()
        .iter()
        .filter(|(e, _, _)| e == endpoint)
        .map(|(_, t, tab)| (t.clone(), *tab))
        .collect()
}

/// Drop the kept tab ids of `targets` on `endpoint` (confirmed closed).
pub fn forget_tab_ids(endpoint: &str, targets: &HashSet<String>) {
    tab_ids().retain(|(e, t, _)| !(e == endpoint && targets.contains(t)));
}

/// Every held right, grouped by endpoint, without releasing any: a group is
/// released only once its close is confirmed ([`close_held`]), so a close
/// that is cancelled or fails part-way loses nothing.
pub fn snapshot_unsaved() -> Vec<(String, HashSet<String>)> {
    let mut out: Vec<(String, HashSet<String>)> = Vec::new();
    for (endpoint, target) in unsaved().iter() {
        match out.iter_mut().find(|(e, _)| e == endpoint) {
            Some((_, set)) => {
                set.insert(target.clone());
            }
            None => out.push((endpoint.clone(), HashSet::from([target.clone()]))),
        }
    }
    out
}

/// Close held `groups` one at a time with `close` (which returns the target
/// ids it confirmed closed) and release each confirmed right right away. The
/// first failure stops and reports; whatever was not confirmed, or not tried,
/// stays held, including when this future is dropped mid-way.
pub async fn close_held<F, Fut>(
    groups: Vec<(String, HashSet<String>)>,
    mut close: F,
) -> Result<Vec<String>, String>
where
    F: FnMut(String, HashSet<String>) -> Fut,
    Fut: std::future::Future<Output = Result<Vec<String>, String>>,
{
    let mut closed_all = Vec::new();
    for (endpoint, held) in groups {
        match close(endpoint.clone(), held.clone()).await {
            Ok(closed) => {
                let confirmed: HashSet<String> = closed
                    .iter()
                    .filter(|t| held.contains(*t))
                    .cloned()
                    .collect();
                release_unsaved(&endpoint, &confirmed);
                closed_all.extend(closed);
            }
            Err(error) => {
                return Err(format!(
                    "close incomplete: {} tab(s) this session opened but could not record are \
                     not confirmed closed: {error}. This daemon still holds them; retry `close`.",
                    held.len()
                ))
            }
        }
    }
    Ok(closed_all)
}

/// The error for a first tab whose setup failed, after cleanup. `recorded`
/// says whether the session's delete right for it is on disk, so `close`
/// (in a later command) can still remove it.
pub fn setup_failure(target_id: &str, error: &str, cleanup: &Cleanup, recorded: bool) -> String {
    match cleanup {
        Cleanup::Gone => format!(
            "{FIRST_TAB_SETUP_FAILED}: {error}. The tab opened for it was closed and Chrome \
             confirms it is gone; nothing is left attached. Rerunning is safe."
        ),
        Cleanup::NotConfirmed { why } => cleanup_incomplete(target_id, error, why, recorded),
    }
}

/// The error for a first tab that may still be open.
pub fn cleanup_incomplete(target_id: &str, cause: &str, why: &str, recorded: bool) -> String {
    let recovery = if recorded {
        "This session keeps its delete right for it: run `chrome-use close` (same session) to \
         remove it."
    } else {
        "Its delete right could not be saved to disk, so only this session's running daemon \
         holds it: run `chrome-use close` (same session) now to remove the tab. If the daemon \
         has exited, `close` cannot find it: close that blank tab in Chrome yourself."
    };
    format!(
        "{FIRST_TAB_CLEANUP_INCOMPLETE}: {cause}. The blank tab opened for it (target \
         {target_id}) may still be open: chrome-use asked Chrome to close it, but it is not confirmed gone ({why}). \
         {recovery} Do not rerun `open` until then; that would open another tab."
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// #486: a tab is created only when the profile shows a real window.
    #[test]
    fn only_a_real_normal_window_counts_as_open() {
        assert!(profile_window_verdict(Ok(json!([{ "id": 1, "type": "normal" }]))).is_ok());
        assert!(profile_window_verdict(Ok(json!([
            { "id": 1, "type": "normal" },
            { "id": 7, "type": "normal", "state": "minimized" }
        ])))
        .is_ok());
        assert_eq!(
            profile_window_verdict(Ok(json!([]))).unwrap_err(),
            PROFILE_NOT_OPEN
        );
        assert!(PROFILE_NOT_OPEN.contains("Open a window in that profile first"));
    }

    /// Unknown is not open, and it is not "no window" either: its own code.
    #[test]
    fn an_unreadable_or_contradictory_list_is_unavailable() {
        for bad in [
            json!(null),
            json!({}),
            json!("x"),
            json!([null]),
            json!([{}]),
            json!([{ "type": "normal" }]),
            json!([{ "id": "1", "type": "normal" }]),
            json!([{ "id": -1, "type": "normal" }]),
            json!([{ "id": 1 }]),
            json!([{ "id": 1, "type": "popup" }]),
            json!([{ "id": 1, "type": "normal" }, { "id": 2, "type": "popup" }]),
            json!([{ "id": 1, "type": "normal" }, null]),
            json!([{ "id": 1, "type": "normal" }, { "id": 1, "type": "normal" }]),
        ] {
            let e = profile_window_verdict(Ok(bad.clone())).unwrap_err();
            assert!(e.starts_with(PROFILE_WINDOW_UNAVAILABLE), "{bad}: {e}");
            assert!(e.contains("nothing was opened"), "{bad}: {e}");
        }
        let e = profile_window_verdict(Err("timed out".to_string())).unwrap_err();
        assert!(e.starts_with(PROFILE_WINDOW_UNAVAILABLE), "{e}");
        let old = profile_window_verdict(Err("'ABExt.call' wasn't found".to_string())).unwrap_err();
        assert!(old.contains("update it from chrome://extensions"), "{old}");
    }

    #[test]
    fn create_errors_never_claim_no_tab() {
        let e = create_failure("CDP command timed out: Target.createTarget");
        assert!(e.starts_with(FIRST_TAB_OUTCOME_UNKNOWN), "{e}");
        assert!(e.contains("may have been opened"), "{e}");
        assert!(!e.contains("Nothing was opened"), "{e}");
        let refusal = "createTarget: could not open the background agent window";
        assert_eq!(create_failure(refusal), refusal);
    }

    #[test]
    fn an_unconfirmed_cleanup_keeps_the_right_and_says_not_to_reopen() {
        let not = Cleanup::NotConfirmed {
            why: "Chrome still lists it".into(),
        };
        let e = setup_failure("T1", "Page.enable failed", &not, true);
        assert!(e.starts_with(FIRST_TAB_CLEANUP_INCOMPLETE), "{e}");
        assert!(e.contains("T1") && e.contains("chrome-use close"), "{e}");
        assert!(e.contains("Do not rerun `open`"), "{e}");
        assert!(!e.contains("nothing is left"), "{e}");
        let lost = setup_failure("T1", "x", &not, false);
        assert!(
            lost.contains("only this session's running daemon"),
            "{lost}"
        );
        let ok = setup_failure("T1", "Page.enable failed", &Cleanup::Gone, true);
        assert!(ok.starts_with(FIRST_TAB_SETUP_FAILED), "{ok}");
        for e in [&e, &lost, &ok, &create_failure("x")] {
            assert!(is_first_tab_refusal(e), "{e}");
        }
        assert!(is_first_tab_refusal(PROFILE_NOT_OPEN));
        assert!(!is_first_tab_refusal("CDP WebSocket connect failed"));
    }

    /// The friendly-error rewrite keeps every first-tab error as written:
    /// the cleanup error quotes "no attached tab", which would otherwise be
    /// turned into "the tab this command was driving is gone".
    #[test]
    fn first_tab_errors_survive_the_friendly_rewrite() {
        let not = Cleanup::NotConfirmed {
            why: "the extension did not answer ABExt.tabPresence: CDP error \
                  (ABExt.tabPresence): no attached tab for targetId T1"
                .into(),
        };
        for e in [
            setup_failure(
                "T1",
                "setting up the new tab did not finish in time",
                &not,
                true,
            ),
            create_failure("CDP command timed out: Target.createTarget"),
            profile_window_verdict(Err("Request timed out".into())).unwrap_err(),
            PROFILE_NOT_OPEN.to_string(),
        ] {
            assert_eq!(crate::native::browser::to_ai_friendly_error(&e), e);
        }
    }

    #[test]
    fn an_unsaved_delete_right_is_held_until_released_exactly() {
        hold_unsaved("ws://held-a", "T9");
        hold_unsaved("ws://held-a", "T9");
        hold_unsaved("ws://held-b", "T9");
        assert_eq!(
            unsaved_for("ws://held-a"),
            HashSet::from(["T9".to_string()])
        );
        // The same opaque id on another browser is another tab.
        release_unsaved("ws://held-a", &HashSet::from(["T9".to_string()]));
        assert!(unsaved_for("ws://held-a").is_empty());
        assert_eq!(
            unsaved_for("ws://held-b"),
            HashSet::from(["T9".to_string()])
        );
        // A snapshot releases nothing.
        assert!(snapshot_unsaved()
            .iter()
            .any(|(e, t)| e == "ws://held-b" && t.contains("T9")));
        assert_eq!(
            unsaved_for("ws://held-b"),
            HashSet::from(["T9".to_string()])
        );
        release_unsaved("ws://held-b", &HashSet::from(["T9".to_string()]));
    }

    #[test]
    fn a_kept_tab_id_belongs_to_one_endpoint_and_target() {
        remember_tab_id("ws://tab-a", "T1", 11);
        remember_tab_id("ws://tab-b", "T1", 22);
        assert_eq!(tab_ids_for("ws://tab-a").get("T1"), Some(&11));
        assert_eq!(tab_ids_for("ws://tab-b").get("T1"), Some(&22));
        forget_tab_ids("ws://tab-a", &HashSet::from(["T1".to_string()]));
        assert!(tab_ids_for("ws://tab-a").is_empty());
        assert_eq!(tab_ids_for("ws://tab-b").get("T1"), Some(&22));
        forget_tab_ids("ws://tab-b", &HashSet::from(["T1".to_string()]));
    }

    /// A multi-group close cancelled part-way: the confirmed group is
    /// released, the one in flight and the one not yet tried stay held, and
    /// nothing is reported closed.
    #[tokio::test]
    async fn a_cancelled_close_of_held_rights_loses_nothing() {
        for (e, t) in [("ws://c-1", "A"), ("ws://c-2", "B"), ("ws://c-3", "C")] {
            hold_unsaved(e, t);
        }
        let groups: Vec<(String, HashSet<String>)> = ["ws://c-1", "ws://c-2", "ws://c-3"]
            .iter()
            .map(|e| (e.to_string(), unsaved_for(e)))
            .collect();
        let reached = std::sync::Arc::new(tokio::sync::Notify::new());
        let signal = reached.clone();
        let close = close_held(groups, move |endpoint, held| {
            let signal = signal.clone();
            async move {
                if endpoint == "ws://c-1" {
                    return Ok(held.into_iter().collect());
                }
                signal.notify_one();
                std::future::pending::<Result<Vec<String>, String>>().await
            }
        });
        tokio::select! {
            r = close => panic!("the second group never answers: {r:?}"),
            _ = reached.notified() => {}
        }
        // `close` was dropped while closing ws://c-2.
        assert!(unsaved_for("ws://c-1").is_empty());
        assert_eq!(unsaved_for("ws://c-2"), HashSet::from(["B".to_string()]));
        assert_eq!(unsaved_for("ws://c-3"), HashSet::from(["C".to_string()]));

        // A failing group stops the close and stays held, as does the rest.
        hold_unsaved("ws://c-4", "D");
        let groups = vec![
            ("ws://c-2".to_string(), unsaved_for("ws://c-2")),
            ("ws://c-4".to_string(), unsaved_for("ws://c-4")),
        ];
        let r = close_held(groups, |_, _| async { Err("refused".to_string()) }).await;
        assert!(r.unwrap_err().starts_with("close incomplete"));
        assert_eq!(unsaved_for("ws://c-2"), HashSet::from(["B".to_string()]));
        assert_eq!(unsaved_for("ws://c-4"), HashSet::from(["D".to_string()]));
        for (e, t) in [("ws://c-2", "B"), ("ws://c-3", "C"), ("ws://c-4", "D")] {
            release_unsaved(e, &HashSet::from([t.to_string()]));
        }
    }
}
