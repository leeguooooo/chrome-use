//! Session ownership + human handoff.
//!
//! Ported from ego-lite's Space ownership model (issue #89), adapted to
//! chrome-use's real-Chrome, sidecar-file session model. A session is owned by
//! the **agent** by default. `session handoff` marks it **user**-owned — the
//! human is now driving (login / 2FA / captcha) — and until `session resume`
//! the agent is refused at the [`crate::connection::send_command`] chokepoint,
//! so it cannot fight the user for the tab. This is zero-impact unless a handoff
//! is explicitly performed: no `.owner` sidecar ⇒ agent-owned ⇒ no guard.
//!
//! Ownership lives in a `<session>.owner` sidecar next to `<session>.pid` /
//! `.sock` / `.version` in [`crate::connection::get_socket_dir`]. Absent (or
//! containing `agent`) ⇒ agent; `user` ⇒ handed off.

use crate::connection::get_socket_dir;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};

/// Set only by `session handoff --wait` in its own process while it watches a
/// handed-off page. Not reachable from argv or the daemon protocol, so an
/// agent command can never claim it.
static WATCHING: AtomicBool = AtomicBool::new(false);

pub fn set_watching(on: bool) {
    WATCHING.store(on, Ordering::SeqCst);
}

/// Who currently controls a session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Owner {
    /// The agent controls the session (default).
    Agent,
    /// Handed off to the human; the agent must not drive it.
    User,
}

impl Owner {
    pub fn as_str(&self) -> &'static str {
        match self {
            Owner::Agent => "agent",
            Owner::User => "user",
        }
    }

    fn parse(s: &str) -> Owner {
        match s.trim().to_lowercase().as_str() {
            "user" => Owner::User,
            _ => Owner::Agent,
        }
    }
}

/// Path of a session's `.owner` sidecar.
pub fn owner_path(session: &str) -> PathBuf {
    get_socket_dir().join(format!("{session}.owner"))
}

/// The session's current owner. A missing / unreadable / non-`user` sidecar is
/// `Agent` — ownership defaults to the agent, so nothing changes for anyone who
/// never hands off.
pub fn owner_of(session: &str) -> Owner {
    match std::fs::read_to_string(owner_path(session)) {
        Ok(s) => Owner::parse(&s),
        Err(_) => Owner::Agent,
    }
}

/// Hand the session to the user: write the `.owner` sidecar as `user`.
pub fn hand_off(session: &str) -> Result<(), String> {
    let dir = get_socket_dir();
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    // Write a per-process temp file and rename it, so a concurrent reader
    // (a `handoff --wait` poll) never sees a truncated, empty `.owner` — which
    // would read as agent-owned.
    let tmp = dir.join(format!("{session}.owner.{}.tmp", std::process::id()));
    std::fs::write(&tmp, "user\n").map_err(|e| e.to_string())?;
    std::fs::rename(&tmp, owner_path(session)).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        e.to_string()
    })
}

/// Resume agent control: remove the `.owner` sidecar (absence ⇒ agent).
/// Idempotent — resuming an already-agent-owned session is a no-op success.
pub fn resume(session: &str) -> Result<(), String> {
    match std::fs::remove_file(owner_path(session)) {
        Ok(()) => {
            crate::handoff_wait::clear_sidecars(session);
            Ok(())
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e.to_string()),
    }
}

/// Actions that stay allowed even while a session is handed off — pure
/// lifecycle / introspection that never drives the page.
const HANDOFF_EXEMPT_ACTIONS: &[&str] = &["status", "ping", "version"];

/// Read-only actions `handoff --wait` uses to watch the user's page. Allowed
/// only while that watcher runs in this process ([`set_watching`]).
const HANDOFF_WATCH_ACTIONS: &[&str] = &["url", "title", "wait", "snapshot"];

/// The guard applied at the send chokepoint. `Err(msg)` means the agent tried
/// to drive a handed-off session and must wait for `session resume`. Pure and
/// unit-testable: takes the resolved owner + the command's action.
pub fn guard(session: &str, owner: Owner, action: Option<&str>) -> Result<(), String> {
    guard_with(session, owner, action, WATCHING.load(Ordering::SeqCst))
}

fn guard_with(
    session: &str,
    owner: Owner,
    action: Option<&str>,
    watching: bool,
) -> Result<(), String> {
    if owner != Owner::User {
        return Ok(());
    }
    // Envelopes without an action (lifecycle plumbing) and the exempt set pass.
    match action {
        None => Ok(()),
        Some(a) if HANDOFF_EXEMPT_ACTIONS.contains(&a) => Ok(()),
        Some(a) if watching && HANDOFF_WATCH_ACTIONS.contains(&a) => Ok(()),
        Some(_) => Err(format!(
            "session '{session}' is handed off to the user — the agent must not drive it. \
             Wait for them with `chrome-use session handoff --wait{}` (run it in the \
             background), or run `chrome-use session resume{}` once they confirm they're done.",
            session_flag_suffix(session),
            session_flag_suffix(session)
        )),
    }
}

/// `""` for the default session, ` --session <name>` otherwise — so the error /
/// hint prints a command the agent can copy verbatim.
pub fn session_flag_suffix(session: &str) -> String {
    if session == "default" || session.is_empty() {
        String::new()
    } else {
        format!(" --session {session}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn owner_parse_defaults_to_agent() {
        assert_eq!(Owner::parse("user"), Owner::User);
        assert_eq!(Owner::parse("USER\n"), Owner::User);
        assert_eq!(Owner::parse("agent"), Owner::Agent);
        assert_eq!(Owner::parse(""), Owner::Agent);
        assert_eq!(Owner::parse("garbage"), Owner::Agent);
    }

    #[test]
    fn watcher_reads_pass_only_while_watching() {
        for a in ["url", "title", "wait", "snapshot"] {
            assert!(guard_with("s", Owner::User, Some(a), true).is_ok(), "{a}");
            assert!(guard_with("s", Owner::User, Some(a), false).is_err(), "{a}");
        }
        for a in ["click", "type", "eval", "open", "fill", "press"] {
            assert!(guard_with("s", Owner::User, Some(a), true).is_err(), "{a}");
        }
    }

    #[test]
    fn guard_allows_agent_owned_everything() {
        assert!(guard("default", Owner::Agent, Some("click")).is_ok());
        assert!(guard("default", Owner::Agent, Some("open")).is_ok());
        assert!(guard("default", Owner::Agent, None).is_ok());
    }

    #[test]
    fn guard_refuses_driving_on_handed_off_session() {
        for a in ["click", "open", "type", "fill", "eval", "scroll", "close"] {
            assert!(
                guard("default", Owner::User, Some(a)).is_err(),
                "driving action {a} must be refused while handed off"
            );
        }
    }

    #[test]
    fn guard_exempts_lifecycle_and_actionless_envelopes() {
        for a in HANDOFF_EXEMPT_ACTIONS {
            assert!(guard("default", Owner::User, Some(a)).is_ok());
        }
        // A lifecycle envelope with no action must not be blocked.
        assert!(guard("default", Owner::User, None).is_ok());
    }

    #[test]
    fn guard_message_has_copyable_resume_command() {
        let err = guard("work", Owner::User, Some("click")).unwrap_err();
        assert!(err.contains("session resume --session work"), "got: {err}");
        let err = guard("default", Owner::User, Some("click")).unwrap_err();
        assert!(
            err.contains("session resume") && !err.contains("--session"),
            "got: {err}"
        );
    }

    #[test]
    fn session_flag_suffix_default_is_empty() {
        assert_eq!(session_flag_suffix("default"), "");
        assert_eq!(session_flag_suffix(""), "");
        assert_eq!(session_flag_suffix("work"), " --session work");
    }
}
