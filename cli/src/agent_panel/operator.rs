//! Which agent conversation drives a Chrome tab. A session's records name the
//! tabs it created; the `.origin.json` its daemon wrote at start names the
//! conversation, and outlives the daemon.

use serde_json::{json, Value};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

const MAX_ACTIONS: usize = 8;

fn valid_target(id: &str) -> bool {
    !id.is_empty() && id.len() <= 64 && id.bytes().all(|c| c.is_ascii_alphanumeric())
}

fn read_json(path: &Path) -> Option<Value> {
    serde_json::from_slice(&fs::read(path).ok()?).ok()
}

/// The session that created `target`; the newest record wins when a stale
/// one still lists the same target.
fn owner_of(dir: &Path, target: &str) -> Option<String> {
    const SUFFIX: &str = ".created-targets.json";
    let mut best: Option<(SystemTime, String)> = None;
    for entry in fs::read_dir(dir).ok()?.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        let Some(session) = name.strip_suffix(SUFFIX) else {
            continue;
        };
        let listed = read_json(&entry.path()).is_some_and(|v| {
            v["target_ids"]
                .as_array()
                .is_some_and(|ids| ids.iter().any(|i| i.as_str() == Some(target)))
        });
        if !listed {
            continue;
        }
        let modified = entry
            .metadata()
            .and_then(|m| m.modified())
            .unwrap_or(SystemTime::UNIX_EPOCH);
        if best.as_ref().is_none_or(|(at, _)| modified > *at) {
            best = Some((modified, session.to_string()));
        }
    }
    best.map(|(_, session)| session)
}

fn recent_actions(dir: &Path, session: &str) -> Vec<Value> {
    let records = read_json(&dir.join(format!("{session}.last-commands.json")));
    let records = records.as_ref().and_then(Value::as_array);
    let Some(records) = records else {
        return Vec::new();
    };
    records
        .iter()
        .rev()
        .take(MAX_ACTIONS)
        .map(|r| json!({"action": r["action"], "at": r["startedAt"], "state": r["state"]}))
        .collect()
}

/// What is known about the tab's driver, without its transcript.
pub(super) fn lookup(dir: &Path, target: &str, live: &[String]) -> Result<Value, String> {
    if !valid_target(target) {
        return Err("tabOperator requires a Chrome target id".into());
    }
    let Some(session) = owner_of(dir, target) else {
        return Ok(json!({"operated": false}));
    };
    let origin = read_json(&dir.join(format!("{session}.origin.json")));
    Ok(json!({
        "operated": true,
        "session": session,
        "live": live.contains(&session),
        "origin": origin,
        "actions": recent_actions(dir, &session),
        "handoff": handoff(dir, &session),
    }))
}

/// The handoff the user is asked to finish, while the session is theirs
/// (`session handoff`). `--wait` adds what it waits for and the agent's note.
fn handoff(dir: &Path, session: &str) -> Value {
    let owned = fs::read_to_string(dir.join(format!("{session}.owner")))
        .is_ok_and(|s| s.trim().eq_ignore_ascii_case("user"));
    if !owned {
        return Value::Null;
    }
    let wait = read_json(&dir.join(format!("{session}.handoff.json"))).unwrap_or(Value::Null);
    json!({
        "since": wait["since"],
        "until": wait["until"],
        "note": wait["note"],
        "done": dir.join(format!("{session}.handoff-done")).exists(),
    })
}

/// The panel's Done button: the user finished the handed-off step on this
/// tab. Writes the marker a running `handoff --wait` polls (as `session done`).
/// `since` is the handoff the card showed: a stale card cannot end a later
/// handoff of the same session.
pub(super) fn handoff_done(dir: &Path, target: &str, since: &Value) -> Result<Value, String> {
    if !valid_target(target) {
        return Err("tabHandoffDone requires a Chrome target id".into());
    }
    let session = owner_of(dir, target).ok_or("No agent session drives this tab")?;
    let current = handoff(dir, &session);
    if current.is_null() {
        return Err("This tab is not handed off to you".into());
    }
    if current["since"] != *since {
        return Err("This handoff has changed; look at the panel again".into());
    }
    let millis = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    fs::write(
        dir.join(format!("{session}.handoff-done")),
        millis.to_string(),
    )
    .map_err(|e| format!("Could not record that you are done: {e}"))?;
    Ok(json!({"schemaVersion": 1, "session": session, "done": true}))
}

pub(super) fn socket_dir() -> PathBuf {
    crate::connection::get_socket_dir()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir() -> tempfile::TempDir {
        let d = tempfile::TempDir::new().unwrap();
        fs::write(
            d.path().join("cu-a-1.created-targets.json"),
            r#"{"target_ids":["AB12"]}"#,
        )
        .unwrap();
        fs::write(
            d.path().join("cu-a-1.origin.json"),
            r#"{"agent":"opencode","conversationId":"ses_x","cwd":"/r"}"#,
        )
        .unwrap();
        fs::write(
            d.path().join("cu-a-1.last-commands.json"),
            r#"[{"action":"fill","startedAt":1,"state":"completed","secret":"x"}]"#,
        )
        .unwrap();
        d
    }

    #[test]
    fn a_created_tab_names_its_conversation_after_the_daemon_is_gone() {
        let d = dir();
        let v = lookup(d.path(), "AB12", &[]).unwrap();
        assert_eq!(v["session"], "cu-a-1");
        assert_eq!(v["live"], false);
        assert_eq!(v["origin"]["conversationId"], "ses_x");
        assert_eq!(
            v["actions"][0],
            json!({"action":"fill","at":1,"state":"completed"})
        );
    }

    #[test]
    fn a_handed_off_tab_shows_the_ask_and_done_marks_it() {
        let d = dir();
        assert_eq!(
            lookup(d.path(), "AB12", &[]).unwrap()["handoff"],
            Value::Null
        );
        assert!(handoff_done(d.path(), "AB12", &Value::Null).is_err());
        fs::write(d.path().join("cu-a-1.owner"), "user").unwrap();
        fs::write(
            d.path().join("cu-a-1.handoff.json"),
            r#"{"url":"u","since":5,"until":{"kind":"url","value":"**/home"},"note":"Sign in"}"#,
        )
        .unwrap();
        let h = &lookup(d.path(), "AB12", &[]).unwrap()["handoff"];
        assert_eq!(h["note"], "Sign in");
        assert_eq!(h["until"]["kind"], "url");
        assert_eq!(h["done"], false);
        // A card from another handoff (or none) is refused.
        assert!(handoff_done(d.path(), "AB12", &json!(4)).is_err());
        assert!(!d.path().join("cu-a-1.handoff-done").exists());
        assert_eq!(
            handoff_done(d.path(), "AB12", &json!(5)).unwrap()["session"],
            "cu-a-1"
        );
        assert!(d.path().join("cu-a-1.handoff-done").exists());
        assert_eq!(
            lookup(d.path(), "AB12", &[]).unwrap()["handoff"]["done"],
            true
        );
        assert!(handoff_done(d.path(), "FFFF", &json!(5)).is_err());
        assert!(handoff_done(d.path(), "../x", &json!(5)).is_err());
    }

    #[test]
    fn a_tab_nobody_created_is_not_operated_and_bad_ids_are_refused() {
        let d = dir();
        assert_eq!(lookup(d.path(), "FFFF", &[]).unwrap()["operated"], false);
        assert!(lookup(d.path(), "../x", &[]).is_err());
    }
}
