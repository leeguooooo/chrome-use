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
        // Behind the bundled build is only a WARNING when a newer build is
        // actually published — the bundled version routinely runs ahead of the
        // Web Store, and telling people to hit Update for a build that isn't
        // released is a dead end (#186). Ask the Store before judging.
        Some(ext) if upgrade::version_is_newer(expected_ext, &ext) => {
            let store = connect::store_extension_version();
            match connect::classify_ext_version(&ext, expected_ext, store.as_deref()) {
                connect::ExtVersionVerdict::BehindStore { store } => checks.push(
                    Check::new(
                        "versions.extension",
                        category,
                        Status::Warn,
                        format!("extension {ext} is behind the published {store}"),
                    )
                    .with_fix(
                        "update ab-connect in Chrome: chrome://extensions \u{2192} \
                         Developer mode \u{2192} Update (or reload the unpacked build)"
                            .to_string(),
                    ),
                ),
                connect::ExtVersionVerdict::NewestPublished { store } => checks.push(Check::new(
                    "versions.extension",
                    category,
                    Status::Pass,
                    format!(
                        "extension {ext} — newest published on the Web Store ({store}); \
                             this CLI bundles {expected_ext}, not published yet"
                    ),
                )),
                _ => checks.push(
                    Check::new(
                        "versions.extension",
                        category,
                        Status::Info,
                        format!(
                            "extension {ext} is behind the bundled {expected_ext} \
                             (Web Store version unknown — offline?)"
                        ),
                    )
                    .with_fix(
                        "if a newer build is published: chrome://extensions \u{2192} \
                         Developer mode \u{2192} Update"
                            .to_string(),
                    ),
                ),
            }
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

    // Driving Chrome profile — with many profiles, the relay binds to whichever
    // profile's extension worker connected; naming it disambiguates a "logged
    // out" result (wrong profile vs. genuinely not logged in) (issue #60).
    match connect::relay_ext_profile() {
        Some((id, Some(email))) => checks.push(Check::new(
            "relay.profile",
            category,
            Status::Info,
            format!("driving Chrome profile: {email} ({id})"),
        )),
        Some((id, None)) => checks.push(Check::new(
            "relay.profile",
            category,
            Status::Info,
            format!(
                "driving Chrome profile id: {id} (grant the extension's optional `identity` \
                 permission to also see the account email)"
            ),
        )),
        None => {}
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
