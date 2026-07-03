//! Check the native-messaging host launcher — the `nm-host.sh` Chrome execs to
//! reach the CLI. A stale launcher (its target binary moved or deleted after a
//! dev-build `cargo clean`, or a relocated install) is THE classic reason the
//! extension relay silently never connects while `browsers` only says "no
//! connected profiles". Doctor must name this cause outright, not pass 12/12
//! while the one load-bearing file is broken.

use super::{Check, Status};
use crate::connect::native_host_report;

pub(super) fn check(checks: &mut Vec<Check>) {
    let category = "Native host";
    let r = native_host_report();

    if r.manifests.is_empty() {
        // Not registered at all — a different, already-handled state (the
        // connect flow writes it on demand). Report as info, not failure.
        checks.push(Check::new(
            "native_host.manifest",
            category,
            Status::Info,
            "Native-messaging host not registered yet (run `chrome-use extension connect`)",
        ));
        return;
    }

    checks.push(Check::new(
        "native_host.manifest",
        category,
        Status::Pass,
        format!("Host manifest installed ({} file(s))", r.manifests.len()),
    ));

    let launcher_display = r
        .launcher
        .as_ref()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|| "<unknown>".into());

    if !r.launcher_exists {
        checks.push(
            Check::new(
                "native_host.launcher",
                category,
                Status::Fail,
                format!("Launcher script missing: {launcher_display}"),
            )
            .with_fix("chrome-use extension connect  (regenerates the launcher for this binary)"),
        );
        return;
    }

    match (&r.target_bin, r.target_exists, r.target_executable) {
        (None, _, _) => {
            checks.push(
                Check::new(
                    "native_host.target",
                    category,
                    Status::Fail,
                    format!("Launcher {launcher_display} has no parseable exec target"),
                )
                .with_fix("chrome-use extension connect  (rewrites the launcher)"),
            );
        }
        (Some(bin), false, _) => {
            // The exact bug that strands agents: launcher points at a binary
            // that no longer exists, so Chrome's connectNative fails instantly.
            checks.push(
                Check::new(
                    "native_host.target",
                    category,
                    Status::Fail,
                    format!(
                        "Launcher points at a MISSING binary: {bin}\n        \
                         → Chrome can't start the host, so the relay never connects \
                         (this is why `browsers` shows no profiles)."
                    ),
                )
                .with_fix(
                    "chrome-use extension connect   (or: chrome-use doctor --fix) — \
                     repoints the host at the current binary",
                ),
            );
        }
        (Some(bin), true, false) => {
            checks.push(
                Check::new(
                    "native_host.target",
                    category,
                    Status::Fail,
                    format!("Launcher target is not executable: {bin}"),
                )
                .with_fix(format!(
                    "chmod +x {bin}   (or: chrome-use extension connect)"
                )),
            );
        }
        (Some(bin), true, true) => {
            if r.target_is_dev_build {
                checks.push(
                    Check::new(
                        "native_host.target",
                        category,
                        Status::Warn,
                        format!(
                            "Launcher points into a build tree: {bin}\n        \
                             → a `cargo clean`/rebuild will delete it and silently break the relay."
                        ),
                    )
                    .with_fix(
                        "chrome-use extension connect   (repoint at the installed binary once stable)",
                    ),
                );
            } else {
                checks.push(Check::new(
                    "native_host.target",
                    category,
                    Status::Pass,
                    format!("Launcher → {bin} (present, executable)"),
                ));
            }
        }
    }
}
