//! Check the local environment: CLI version, platform, state/socket dirs,
//! and free disk space.

use std::path::Path;

use super::helpers::{disk_free_bytes, human_size, is_writable_dir};
use super::{Check, Status};
use crate::connection::get_socket_dir;
use crate::native::state::get_state_dir;

pub(super) fn check(checks: &mut Vec<Check>) {
    let category = "Environment";

    let version = env!("CARGO_PKG_VERSION");
    let platform = format!("{} {}", std::env::consts::OS, std::env::consts::ARCH);
    checks.push(Check::new(
        "env.version",
        category,
        Status::Pass,
        format!("CLI version {} ({})", version, platform),
    ));

    match dirs::home_dir() {
        Some(home) => checks.push(Check::new(
            "env.home",
            category,
            Status::Pass,
            format!("Home directory {}", home.display()),
        )),
        None => checks.push(Check::new(
            "env.home",
            category,
            Status::Fail,
            "Could not determine home directory",
        )),
    }

    // Which session this shell resolves to, and why. The name decides the tab
    // group, the daemon port and where `keep`/handoff state lives, and it can be
    // derived from an env var this build has never heard of (any
    // `<PRODUCT>_SESSION_ID`-style name) — so "why am I on a different session
    // than I expected?" needs an answer that doesn't involve reading the source.
    let (derived, source) = crate::flags::describe_default_session();
    let detail = match source {
        Some(var) => format!("Default session {derived} (derived from {var})"),
        None => format!("Default session {derived} (no per-agent env id found)"),
    };
    // Reported as the DEFAULT, not as "the session you are on": `--session`, the
    // config file and `AGENT_BROWSER_SESSION` all override it, and doctor has no
    // command line to inspect.
    let detail = match std::env::var("AGENT_BROWSER_SESSION")
        .ok()
        .filter(|s| !s.is_empty())
    {
        Some(explicit) => format!("{detail}; overridden by AGENT_BROWSER_SESSION={explicit}"),
        None => detail,
    };
    checks.push(Check::new("env.session", category, Status::Pass, detail));

    let state_dir = get_state_dir();
    let socket_dir = get_socket_dir();

    // Under the default setup, state and socket dirs are the same
    // (~/.chrome-use). Collapse to a single line when they match;
    // split when XDG_RUNTIME_DIR or AGENT_BROWSER_SOCKET_DIR diverts
    // sockets elsewhere.
    if state_dir == socket_dir {
        push_dir_check(
            checks,
            "env.state_dir",
            category,
            "State and socket directory",
            &state_dir,
        );
    } else {
        push_dir_check(
            checks,
            "env.state_dir",
            category,
            "State directory",
            &state_dir,
        );
        push_dir_check(
            checks,
            "env.socket_dir",
            category,
            "Socket directory",
            &socket_dir,
        );
    }

    match disk_free_bytes(&state_dir) {
        Some(bytes) => {
            let mb = bytes / (1024 * 1024);
            let human = human_size(bytes);
            if mb < 500 {
                checks.push(
                    Check::new(
                        "env.disk_free",
                        category,
                        Status::Warn,
                        format!("Low disk space at state dir: {} free", human),
                    )
                    .with_fix("free up disk space; Chrome installs require ~500 MB"),
                );
            } else {
                checks.push(Check::new(
                    "env.disk_free",
                    category,
                    Status::Pass,
                    format!("{} free at state dir", human),
                ));
            }
        }
        None => checks.push(Check::new(
            "env.disk_free",
            category,
            Status::Info,
            "Disk free check unavailable on this platform",
        )),
    }
}

fn push_dir_check(
    checks: &mut Vec<Check>,
    id: &'static str,
    category: &'static str,
    label: &str,
    dir: &Path,
) {
    if dir.exists() {
        if is_writable_dir(dir) {
            checks.push(Check::new(
                id,
                category,
                Status::Pass,
                format!("{} {}", label, dir.display()),
            ));
        } else {
            checks.push(
                Check::new(
                    id,
                    category,
                    Status::Fail,
                    format!("{} not writable: {}", label, dir.display()),
                )
                .with_fix(format!("chmod u+rwx {}", dir.display())),
            );
        }
    } else {
        checks.push(Check::new(
            id,
            category,
            Status::Info,
            format!(
                "{} does not exist yet (will be created on first use): {}",
                label,
                dir.display()
            ),
        ));
    }
}
