//! Version-coherence checks across all four moving parts: the CLI binary, the
//! per-session daemons (covered by `daemon.rs`), the connected `ab-connect`
//! extension, and the bundled skill. The extension was previously a black box —
//! nothing reported which build was live — so a user could sit on an old
//! extension with no signal. The extension now reports its version over the
//! relay (`hello`), the host records it, and this surfaces it in one place.

use super::{Check, Status};
use crate::{connect, upgrade};

pub(super) fn check(checks: &mut Vec<Check>) {
    let category = "Versions";
    let cli_version = env!("CARGO_PKG_VERSION");

    // CLI — compare against the latest seen by the background update check.
    match upgrade::cached_latest_version() {
        Some(latest) if upgrade::version_is_newer(&latest, cli_version) => {
            checks.push(
                Check::new(
                    "versions.cli",
                    category,
                    Status::Warn,
                    format!("CLI {cli_version} (newer available: {latest})"),
                )
                .with_fix("chrome-use upgrade".to_string()),
            );
        }
        _ => {
            checks.push(Check::new(
                "versions.cli",
                category,
                Status::Pass,
                format!("CLI {cli_version}"),
            ));
        }
    }

    // Extension — the build this CLI shipped alongside (embedded at compile time
    // from the extension manifest) is what we expect to be running.
    let expected_ext = env!("AB_CONNECT_VERSION");
    match connect::relay_ext_version() {
        Some(ext) if upgrade::version_is_newer(expected_ext, &ext) => {
            checks.push(
                Check::new(
                    "versions.extension",
                    category,
                    Status::Warn,
                    format!("extension {ext} is behind the bundled {expected_ext}"),
                )
                .with_fix(
                    "update ab-connect in Chrome: chrome://extensions \u{2192} reload \
                     (or wait for the Web Store auto-update)"
                        .to_string(),
                ),
            );
        }
        Some(ext) => {
            checks.push(Check::new(
                "versions.extension",
                category,
                Status::Pass,
                format!("extension {ext}"),
            ));
        }
        None => {
            checks.push(Check::new(
                "versions.extension",
                category,
                Status::Info,
                format!(
                    "extension not connected (or it predates version reporting — \
                     expected {expected_ext})"
                ),
            ));
        }
    }

    // Skill — ships inside the same release artifact as the binary, so it's
    // version-locked here. Copies made elsewhere via `skills add` aren't.
    checks.push(Check::new(
        "versions.skill",
        category,
        Status::Info,
        format!(
            "skills bundled with this CLI ({cli_version}); copies made via `skills add` \
             elsewhere may be stale — re-run to refresh"
        ),
    ));
}
