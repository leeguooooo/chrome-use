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
    }))
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
    fn a_tab_nobody_created_is_not_operated_and_bad_ids_are_refused() {
        let d = dir();
        assert_eq!(lookup(d.path(), "FFFF", &[]).unwrap()["operated"], false);
        assert!(lookup(d.path(), "../x", &[]).is_err());
    }
}
