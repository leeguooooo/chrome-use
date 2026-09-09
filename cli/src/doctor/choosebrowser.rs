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
}
