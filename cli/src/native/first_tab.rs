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
//! - A tab is gone only when an authoritative source says so, the same
//!   contract as `close` (#496): over the relay the extension's versioned
//!   `ABExt.tabPresence` for that exact target; on a direct CDP connection
//!   Chrome's own target list. A `Target.closeTarget` acknowledgement proves
//!   nothing. Whatever is not confirmed gone keeps its recorded delete right,
//!   and the error says so instead of inviting another `open`.

use serde_json::{json, Value};
use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;
use tokio::time::Instant;

use super::cdp::client::CdpClient;
use super::cdp::types::GetTargetsResult;

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

/// The most a single close or read-back may take inside the cleanup reserve.
const CLEANUP_CALL_TIMEOUT: Duration = Duration::from_secs(3);

/// How long a tab Chrome acknowledged closing but still lists is re-read
/// before it counts as still open (`Target.closeTarget` answers before the
/// tab is removed).
const GONE_RECHECK: Duration = Duration::from_secs(3);
const GONE_POLL: Duration = Duration::from_millis(200);

/// The `ABExt.tabPresence` contract version understood here; the same as the
/// one `close` reads (#496). A reply without it never proves a tab gone.
const TAB_PRESENCE_CONTRACT: u64 = 1;

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

/// Is `target_id` still in the browser, by an authoritative source?
/// `Ok(true)` present, `Ok(false)` gone, `Err` cannot tell.
pub fn presence_from_reply(reply: &Result<Value, String>, target_id: &str) -> Result<bool, String> {
    let v = reply
        .as_ref()
        .map_err(|e| format!("the extension did not answer ABExt.tabPresence: {e}"))?;
    if v.get("tabPresenceVersion")
        .and_then(Value::as_u64)
        .is_none_or(|n| n < TAB_PRESENCE_CONTRACT)
    {
        return Err(
            "this ab-connect cannot report whether a tab is gone (no tabPresenceVersion; \
             0.5.33 or newer can)"
                .to_string(),
        );
    }
    if v.get("targetId").and_then(Value::as_str) != Some(target_id) {
        return Err("the extension answered about another tab".to_string());
    }
    match v.get("presence").and_then(Value::as_str) {
        Some("absent") => Ok(false),
        Some("present") => Ok(true),
        _ => Err(v
            .get("error")
            .and_then(Value::as_str)
            .unwrap_or("the extension could not tell")
            .to_string()),
    }
}

/// On a direct CDP connection Chrome's target list is the record.
pub fn presence_from_targets(
    list: &Result<GetTargetsResult, String>,
    target_id: &str,
) -> Result<bool, String> {
    match list {
        Ok(list) => Ok(list.target_infos.iter().any(|t| t.target_id == target_id)),
        Err(e) => Err(format!("Chrome's target list could not be read: {e}")),
    }
}

async fn read_presence(
    client: &Arc<CdpClient>,
    on_relay: bool,
    target_id: &str,
    deadline: Instant,
) -> Result<bool, String> {
    let cap = std::cmp::min(deadline, Instant::now() + CLEANUP_CALL_TIMEOUT);
    if on_relay {
        let reply = tokio::time::timeout_at(
            cap,
            client.send_command(
                "ABExt.tabPresence",
                Some(json!({ "targetId": target_id, "tabId": null })),
                None,
            ),
        )
        .await
        .map_err(|_| "no answer about the tab within the cleanup budget".to_string())?;
        presence_from_reply(&reply, target_id)
    } else {
        let list = tokio::time::timeout_at(
            cap,
            client.send_command_typed::<_, GetTargetsResult>("Target.getTargets", &json!({}), None),
        )
        .await
        .map_err(|_| "no target list within the cleanup budget".to_string())?;
        presence_from_targets(&list, target_id)
    }
}

/// How cleaning up a tab that could not be set up ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Cleanup {
    /// Chrome confirms the tab is gone.
    Gone,
    /// The tab may still be open: why it is not confirmed gone, and what the
    /// close request got.
    NotConfirmed { why: String, close: String },
}

/// Ask Chrome to close `target_id`, then read back from an authoritative
/// source that it is gone, never past `deadline`. The close answer is only
/// reported, never taken as proof.
pub async fn close_and_verify(
    client: &Arc<CdpClient>,
    on_relay: bool,
    target_id: &str,
    deadline: Instant,
) -> Cleanup {
    let cap = std::cmp::min(deadline, Instant::now() + CLEANUP_CALL_TIMEOUT);
    let close = match tokio::time::timeout_at(
        cap,
        client.send_command(
            "Target.closeTarget",
            Some(json!({ "targetId": target_id })),
            None,
        ),
    )
    .await
    {
        Ok(Ok(v)) if v.get("success").and_then(Value::as_bool) == Some(false) => {
            "the close was refused".to_string()
        }
        Ok(Ok(_)) => "the close was acknowledged".to_string(),
        Ok(Err(e)) => format!("the close failed: {e}"),
        Err(_) => "the close got no answer".to_string(),
    };
    let recheck_until = std::cmp::min(deadline, Instant::now() + GONE_RECHECK);
    loop {
        match read_presence(client, on_relay, target_id, deadline).await {
            Ok(false) => return Cleanup::Gone,
            Ok(true) if Instant::now() + GONE_POLL < recheck_until => {
                tokio::time::sleep(GONE_POLL).await;
            }
            Ok(true) => {
                return Cleanup::NotConfirmed {
                    why: "Chrome still lists it".to_string(),
                    close,
                }
            }
            Err(why) => return Cleanup::NotConfirmed { why, close },
        }
    }
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

/// Drop `targets` from the held list (saved to disk, or closed).
pub fn release_unsaved(targets: &HashSet<String>) {
    unsaved().retain(|(_, t)| !targets.contains(t));
}

/// Take every held target, grouped by endpoint.
pub fn take_unsaved() -> Vec<(String, HashSet<String>)> {
    let mut out: Vec<(String, HashSet<String>)> = Vec::new();
    for (endpoint, target) in unsaved().drain(..) {
        match out.iter_mut().find(|(e, _)| *e == endpoint) {
            Some((_, set)) => {
                set.insert(target);
            }
            None => out.push((endpoint, HashSet::from([target]))),
        }
    }
    out
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
        Cleanup::NotConfirmed { why, close } => {
            cleanup_incomplete(target_id, error, why, close, recorded)
        }
    }
}

/// The error for a first tab that may still be open.
pub fn cleanup_incomplete(
    target_id: &str,
    cause: &str,
    why: &str,
    close: &str,
    recorded: bool,
) -> String {
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
         {target_id}) may still be open: {close}, but it is not confirmed gone ({why}). \
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

    /// An acknowledgement is not the contract: only a versioned answer about
    /// the same target says present or absent.
    #[test]
    fn only_the_versioned_presence_answer_decides() {
        let t = "T1";
        let absent = json!({"tabPresenceVersion": 1, "targetId": t, "presence": "absent"});
        let present = json!({"tabPresenceVersion": 1, "targetId": t, "presence": "present"});
        assert_eq!(presence_from_reply(&Ok(absent), t), Ok(false));
        assert_eq!(presence_from_reply(&Ok(present), t), Ok(true));
        for unknown in [
            Ok(json!({"success": true})),
            Ok(json!({"targetId": t, "presence": "absent"})),
            Ok(json!({"tabPresenceVersion": 1, "targetId": "T2", "presence": "absent"})),
            Ok(json!({"tabPresenceVersion": 1, "targetId": t})),
            Err("'ABExt.tabPresence' wasn't found".to_string()),
        ] {
            assert!(presence_from_reply(&unknown, t).is_err(), "{unknown:?}");
        }
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
            close: "the close was acknowledged".into(),
        };
        let e = setup_failure("T1", "Page.enable failed", &not, true);
        assert!(e.starts_with(FIRST_TAB_CLEANUP_INCOMPLETE), "{e}");
        assert!(e.contains("T1") && e.contains("chrome-use close"), "{e}");
        assert!(e.contains("Do not rerun `open`"), "{e}");
        assert!(!e.contains("nothing is left"), "{e}");
        let lost = setup_failure("T1", "x", &not, false);
        assert!(
            lost.contains("close that blank tab in Chrome yourself"),
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
            close: "the close was acknowledged".into(),
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
    fn an_unsaved_delete_right_is_held_until_saved_or_closed() {
        hold_unsaved("ws://a", "T9");
        hold_unsaved("ws://a", "T9");
        hold_unsaved("ws://b", "T8");
        assert_eq!(unsaved_for("ws://a"), HashSet::from(["T9".to_string()]));
        release_unsaved(&HashSet::from(["T9".to_string()]));
        assert!(unsaved_for("ws://a").is_empty());
        let taken = take_unsaved();
        assert!(taken.iter().any(|(e, t)| e == "ws://b" && t.contains("T8")));
        assert!(unsaved_for("ws://b").is_empty());
    }
}
