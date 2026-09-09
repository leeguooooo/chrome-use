//! Read-only probe: can we see ChooseBrowser's rules, and would they route
//! anything?
//!
//! The integration is silent by design — no rules file means no message, which
//! is right for the many people who do not use that app. The cost is that every
//! way of not working looks identical from outside: not installed, wrong path,
//! version we do not read, format we could not parse, profile not connected,
//! no rule for this url. Three separate never-worked bugs shipped behind that
//! sameness. This check is the loud counterpart to the quiet path.
//!
//! Never `Fail`: not having ChooseBrowser is the normal case, not a fault.

use super::{Check, Status};
use crate::choosebrowser;

pub(super) fn check(checks: &mut Vec<Check>) {
    check_app(checks);
    let category = "ChooseBrowser rules";
    let d = choosebrowser::diagnose();

    let Some(source) = d.source.as_ref() else {
        // Say where we looked. "Not installed" and "installed somewhere we do
        // not read" are different, and only the paths distinguish them.
        let looked: Vec<String> = d
            .probed
            .iter()
            .map(|(p, _)| p.display().to_string())
            .collect();
        checks.push(
            Check::new(
                "choosebrowser.rules",
                category,
                Status::Info,
                "no rules file — profile selection is unaffected",
            )
            .with_fix(format!(
                "if you do use ChooseBrowser, its rules are not in any path this build reads: {}",
                looked.join(", ")
            )),
        );
        return;
    };

    match (d.parsed, d.version) {
        (Some(n), _) => {
            checks.push(Check::new(
                "choosebrowser.rules",
                category,
                Status::Pass,
                // Which path answered belongs in the message, not a
                // footnote: a released build still writes an older
                // location, so "found, but in the one you thought was
                // retired" is a real and confusing state.
                format!("{n} rule(s) loaded from {}", source.display()),
            ));
        }
        // Parsed as JSON, but not a version this build reads. Guessing at an
        // unknown shape is how a link opens as the wrong account, so it is
        // deliberately ignored — but silently ignoring it is what hid this
        // class of problem before.
        (None, Some(v)) => {
            checks.push(
                Check::new(
                    "choosebrowser.rules",
                    category,
                    Status::Warn,
                    format!(
                        "{} is version {v}; this build reads version 2 only",
                        source.display()
                    ),
                )
                .with_fix("rules are ignored rather than guessed at — upgrade chrome-use"),
            );
        }
        (None, None) => {
            checks.push(Check::new(
                "choosebrowser.rules",
                category,
                Status::Warn,
                format!(
                    "{} exists but did not parse — every rule in it is being ignored",
                    source.display()
                ),
            ));
        }
    }

    // Two truths, and we picked one. A file in a later path is not being
    // read, but something may still be writing to it — a downgrade to a build
    // that uses the old location, or a half-finished migration — in which case
    // every url routes by whichever copy stopped changing. Not a failure, so
    // not a warning; but it is exactly the kind of thing the silent path
    // cannot say for itself.
    let shadowed: Vec<String> = d
        .probed
        .iter()
        .filter(|(p, exists)| *exists && p != source)
        .map(|(p, _)| p.display().to_string())
        .collect();
    if !shadowed.is_empty() {
        checks.push(
            Check::new(
                "choosebrowser.rules.shadowed",
                category,
                Status::Info,
                format!(
                    "another rules file exists and is not being read: {}",
                    shadowed.join(", ")
                ),
            )
            .with_fix(
                "only the first path found is used — if ChooseBrowser is still writing to the \
                 other one (after a downgrade, say), the rules in use are stale; remove or \
                 merge the copy you no longer want",
            ),
        );
    }
}

/// The app itself: which version, and does it register `choosebrowser://`.
///
/// Reported as two facts, not one verdict. `0.2.0 / no` means upgrade;
/// `0.2.1 / no` means something else (two copies installed, LaunchServices
/// not refreshed) and "upgrade" would be the wrong advice.
#[cfg(target_os = "macos")]
fn check_app(checks: &mut Vec<Check>) {
    let category = "ChooseBrowser app";
    let Some(app) = choosebrowser::probe_app() else {
        let looked: Vec<String> = choosebrowser::app_candidates()
            .iter()
            .map(|p| p.display().to_string())
            .collect();
        checks.push(Check::new(
            "choosebrowser.app",
            category,
            Status::Info,
            format!(
                "not found in {} — --remember has nothing to talk to",
                looked.join(" or ")
            ),
        ));
        return;
    };
    let version = app
        .version
        .clone()
        .unwrap_or_else(|| "unknown version".into());
    if app.accepts_rule_requests() {
        checks.push(Check::new(
            "choosebrowser.app",
            category,
            Status::Pass,
            format!(
                "ChooseBrowser {version} at {} — accepts rule requests: yes",
                app.path.display()
            ),
        ));
    } else {
        checks.push(
            Check::new(
                "choosebrowser.app",
                category,
                Status::Info,
                format!(
                    "ChooseBrowser {version} at {} — accepts rule requests: no (registers: {})",
                    app.path.display(),
                    if app.schemes.is_empty() {
                        "nothing".to_string()
                    } else {
                        app.schemes.join(" ")
                    }
                ),
            )
            .with_fix(format!(
                "--remember needs ChooseBrowser ≥ {}; reading rules works on any version",
                choosebrowser::MIN_APP_VERSION_FOR_RULE_REQUESTS
            )),
        );
    }
}

#[cfg(not(target_os = "macos"))]
fn check_app(_checks: &mut Vec<Check>) {}
