//! Human-readable name for a session's Chrome tab group.
//!
//! The tab group in the user's Chrome is labelled with the session id by
//! default — `cu-agent-browser-stealth-fb0742`. That is a routing key, and it
//! tells the human nothing about what the agent is doing in their browser.
//! `session name "🔎 track a parcel"` replaces the label with something they
//! can read at a glance, without changing the id anything else keys on.
//!
//! Safe to rename because the group title is **presentation only**: the
//! extension's `groupIdByName` is a local cache inside `groupTabInto`, and tab
//! ownership is tracked by target id, not by group. Verified before building
//! this, not assumed.
//!
//! Lives in a `<session>.title` sidecar beside `<session>.owner`, so it
//! survives the daemon being recycled for idleness mid-task.

use crate::connection::get_socket_dir;
use std::path::PathBuf;

/// Chrome renders roughly this much of a group title before eliding it, and a
/// title is a label rather than a description. Longer input is cut rather than
/// rejected: the caller's intent is clear, and failing here would be pedantry.
pub const MAX_TITLE_CHARS: usize = 60;

pub fn title_path(session: &str) -> PathBuf {
    get_socket_dir().join(format!("{session}.title"))
}

/// Normalise a requested title into what Chrome will actually show.
///
/// Returns `None` for anything that would render as a blank tab group, so the
/// caller falls back to the session id rather than displaying an empty label
/// the user cannot associate with anything.
pub fn sanitize_title(raw: &str) -> Option<String> {
    // A tab group title is a single line; embedded newlines and control
    // characters come from copy-paste accidents and would corrupt the sidecar's
    // one-line format.
    let flattened: String = raw
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    let collapsed = flattened.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.is_empty() {
        return None;
    }
    let truncated: String = collapsed.chars().take(MAX_TITLE_CHARS).collect();
    Some(truncated)
}

/// The label to put on this session's tab group: the chosen title when there is
/// one, otherwise the session id.
pub fn display_name(session: &str) -> String {
    title_of(session).unwrap_or_else(|| session.to_string())
}

pub fn title_of(session: &str) -> Option<String> {
    let raw = std::fs::read_to_string(title_path(session)).ok()?;
    sanitize_title(&raw)
}

pub fn set_title(session: &str, raw: &str) -> Result<String, String> {
    let title = sanitize_title(raw)
        .ok_or_else(|| "a session name needs at least one visible character".to_string())?;
    let path = title_path(session);
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)
            .map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
    }
    std::fs::write(&path, &title).map_err(|e| format!("cannot write {}: {e}", path.display()))?;
    Ok(title)
}

pub fn clear_title(session: &str) {
    let _ = std::fs::remove_file(title_path(session));
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An emoji-prefixed task name is the whole point; multi-byte characters
    /// must survive both the length cap and the round trip.
    #[test]
    fn keeps_emoji_and_cjk() {
        assert_eq!(
            sanitize_title("🔎 查快递单号").as_deref(),
            Some("🔎 查快递单号")
        );
    }

    /// The cap counts characters, not bytes: cutting a 3-byte character in half
    /// would put invalid UTF-8 on the tab strip.
    #[test]
    fn truncates_by_characters_not_bytes() {
        let long = "查".repeat(MAX_TITLE_CHARS + 20);
        let out = sanitize_title(&long).unwrap();
        assert_eq!(out.chars().count(), MAX_TITLE_CHARS);
        assert!(out.chars().all(|c| c == '查'));
    }

    /// A pasted title carrying a newline would corrupt the one-line sidecar and
    /// render as a mangled label.
    #[test]
    fn flattens_newlines_and_collapses_runs_of_space() {
        assert_eq!(
            sanitize_title("book\na  flight\t now").as_deref(),
            Some("book a flight now")
        );
    }

    /// Whitespace-only input would leave an unreadable blank group, so it is
    /// not a title — the caller falls back to the session id.
    #[test]
    fn blank_input_is_not_a_title() {
        for raw in ["", "   ", "\n\t "] {
            assert_eq!(sanitize_title(raw), None, "{raw:?}");
        }
    }

    /// With no title set, the group keeps the session id. Silently showing an
    /// empty label would be worse than showing the routing key.
    #[test]
    fn display_name_falls_back_to_the_session_id() {
        let session = "no-such-session-for-title-test";
        clear_title(session);
        assert_eq!(display_name(session), session);
    }

    /// A label must survive the stale-file cleanup that runs whenever a daemon
    /// is found missing or old — including on a session's very first command.
    ///
    /// Regression: `session name` wrote the sidecar, then its own best-effort
    /// rename went through `send_command`, which reached that cleanup and
    /// deleted the file before the success line printed. Setting a name
    /// reported ✓ and left nothing behind.
    #[test]
    fn a_label_survives_stale_file_cleanup() {
        let session = "chrome-use-title-survives-cleanup-test";
        set_title(session, "🔎 keep me").unwrap();
        crate::connection::cleanup_stale_files(session);
        assert_eq!(
            title_of(session).as_deref(),
            Some("🔎 keep me"),
            "cleanup must not discard a label the user chose"
        );
        clear_title(session);
    }

    /// Round trip through the sidecar, including the emoji, because that is the
    /// path the daemon reads on every tab it creates.
    #[test]
    fn set_then_read_round_trips() {
        let session = "chrome-use-title-round-trip-test";
        let stored = set_title(session, "  🔎  查快递单号 \n").unwrap();
        assert_eq!(stored, "🔎 查快递单号");
        assert_eq!(title_of(session).as_deref(), Some("🔎 查快递单号"));
        assert_eq!(display_name(session), "🔎 查快递单号");
        clear_title(session);
        assert_eq!(title_of(session), None);
    }
}
