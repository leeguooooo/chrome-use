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
use crate::profiles::{self, ProfileRow, RuleHit};

#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
const RULE_CATEGORY: &str = "ChooseBrowser rules";

/// One line per profile rule: `claude.ai → Davian (Profile 14, …): connected`.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn rule_target_checks(
    rules: &[choosebrowser::ProfileRule],
    local_state: Option<&str>,
    rows: &[ProfileRow],
    chrome_root: Option<&str>,
) -> Vec<Check> {
    rules
        .iter()
        .enumerate()
        .map(|(n, rule)| {
            let id = format!("choosebrowser.rule.{}", n + 1);
            let name = match &rule.rule_id {
                Some(r) => format!("{} ({r})", rule.pattern),
                None => rule.pattern.clone(),
            };
            // Without Local State nothing can be said about the profile, and
            // the runtime lookup treats the url as having no rule. Say that,
            // not "no such profile".
            let Some(local_state) = local_state else {
                return Check::new(
                    id,
                    RULE_CATEGORY,
                    Status::Warn,
                    format!(
                        "{name} → profile key {}: unknown — Chrome's Local State could not be \
                         read, so this rule is not applied",
                        rule.key
                    ),
                )
                .with_fix(
                    "check that Google Chrome has run on this account \
                     (~/Library/Application Support/Google/Chrome/Local State)",
                );
            };
            let resolution = choosebrowser::resolve_profile_key(local_state, &rule.key);
            let resolved = match resolution {
                choosebrowser::KeyResolution::Found(p) => p,
                choosebrowser::KeyResolution::NotFound => {
                    return Check::new(
                        id,
                        RULE_CATEGORY,
                        Status::Warn,
                        format!(
                            "{name} → profile key {}: no such Chrome profile on this machine — \
                             the rule is ignored here (opening these urls warns)",
                            rule.key
                        ),
                    );
                }
                choosebrowser::KeyResolution::Ambiguous(dirs) => {
                    return Check::new(
                        id,
                        RULE_CATEGORY,
                        Status::Warn,
                        format!(
                            "{name} → profile key {}: matches {} profiles ({}) — opening \
                             these urls is refused, since which account it means is a guess",
                            rule.key,
                            dirs.len(),
                            dirs.join(", ")
                        ),
                    )
                    .with_fix("point the rule at one profile in ChooseBrowser");
                }
            };
            let hit = RuleHit {
                host: rule.pattern.clone(),
                rule_id: rule.rule_id.clone(),
                key: rule.key.clone(),
                root: chrome_root.map(str::to_string),
                dir: resolved.directory.clone(),
                email: resolved.email.clone(),
                ambiguous: Vec::new(),
            };
            match profiles::row_for_rule(rows, &hit) {
                None => Check::new(
                    id,
                    RULE_CATEGORY,
                    Status::Warn,
                    format!(
                        "{name} → {}: not in chrome-use's profile list — opening these urls \
                         is refused",
                        resolved.directory
                    ),
                )
                .with_fix("run `chrome-use browsers` to see the profiles chrome-use knows"),
                Some(i) if rows[i].connected() => Check::new(
                    id,
                    RULE_CATEGORY,
                    Status::Pass,
                    format!("{name} → {}: connected", rows[i].label()),
                ),
                Some(i) => Check::new(
                    id,
                    RULE_CATEGORY,
                    Status::Info,
                    format!(
                        "{name} → {}: not connected — opening these urls is refused until it is",
                        rows[i].label()
                    ),
                )
                .with_fix(format!(
                    "`{}` (opens a window in that profile — ask the user first)",
                    profiles::connect_command(&profiles::suggested_selector(rows, i))
                )),
            }
        })
        .collect()
}

pub(super) fn check(checks: &mut Vec<Check>) {
    // ChooseBrowser's app and rule locations are macOS-specific. Reporting
    // these paths on Windows/Linux suggests a repair the user cannot perform.
    if !cfg!(target_os = "macos") {
        return;
    }
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
                "if you do use ChooseBrowser, its rules are not in any path this build reads: {}. \
                 The shared copy only appears after the first save on ChooseBrowser 0.2.1+; \
                 nothing needs fixing until then",
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
            // Per rule: is the profile it names usable right now? A rule
            // whose profile is not connected makes `open` of its sites fail
            // (chrome-use will not substitute another profile), so this is
            // the line that explains such a refusal before it happens.
            checks.extend(rule_target_checks(
                &choosebrowser::load_profile_rules(),
                choosebrowser::read_local_state().as_deref(),
                &profiles::load_rows(),
                choosebrowser::chrome_root()
                    .map(|p| p.display().to_string())
                    .as_deref(),
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
                "only the first path found is read. The Group Containers copy is derived — \
                 ChooseBrowser 0.2.1+ republishes it on every save — so if the rules in use \
                 look stale, delete that copy (nothing is lost) and save any rule once in \
                 ChooseBrowser to republish it",
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
    // Two copies at the fixed locations means two channels installed (store
    // and direct-sale, say). LaunchServices picks one of them for links, and it
    // is not necessarily the one whose settings the user is editing — which
    // reads as "my rules do not apply". Say it before anything else.
    let installed: Vec<String> = choosebrowser::app_candidates()
        .into_iter()
        .filter(|p| p.is_dir())
        .map(|p| p.display().to_string())
        .collect();
    if installed.len() > 1 {
        checks.push(
            Check::new(
                "choosebrowser.app.duplicates",
                category,
                Status::Warn,
                format!(
                    "ChooseBrowser is installed twice: {}",
                    installed.join(" and ")
                ),
            )
            .with_fix(
                "macOS opens links with one of them, not necessarily the one whose settings you \
                 edit — remove the copy you do not use",
            ),
        );
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    fn row(dir: &str, name: &str, ws: Option<&str>) -> ProfileRow {
        ProfileRow {
            name: Some(name.to_string()),
            dir: Some(dir.to_string()),
            root: Some("/chrome".to_string()),
            ws: ws.map(str::to_string),
            relay_id: ws.map(|_| format!("id-{dir}")),
            has_extension: true,
            ..Default::default()
        }
    }

    /// The user's question was "does my claude.ai rule apply?" — each rule
    /// gets a line saying whether its profile is connected right now.
    #[test]
    fn each_rule_says_whether_its_profile_is_connected() {
        let local_state = r#"{"profile":{"info_cache":{
            "Default":{"name":"Leo","user_name":"leo@x.com","gaia_id":"111"},
            "Profile 14":{"name":"Davian","user_name":"d@x.com","gaia_id":"222"}}}}"#;
        let rules = vec![
            choosebrowser::ProfileRule {
                rule_id: Some("r1".into()),
                pattern: "claude.ai".into(),
                key: "222".into(),
            },
            choosebrowser::ProfileRule {
                rule_id: None,
                pattern: "example.com/team*".into(),
                key: "111".into(),
            },
            choosebrowser::ProfileRule {
                rule_id: None,
                pattern: "*.bilibili.com".into(),
                key: "999".into(),
            },
        ];
        let rows = vec![
            row("Default", "Leo", Some("ws://leo")),
            row("Profile 14", "Davian", None),
        ];
        let checks = rule_target_checks(&rules, Some(local_state), &rows, Some("/chrome"));
        assert_eq!(checks.len(), 3);
        assert_eq!(checks[0].status, Status::Info);
        assert!(
            checks[0]
                .message
                .starts_with("claude.ai (r1) → Davian (Profile 14"),
            "{}",
            checks[0].message
        );
        assert!(checks[0].message.contains("not connected"));
        let fix = checks[0].fix.as_deref().unwrap();
        assert!(fix.contains("chrome-use connect --browser Davian"), "{fix}");
        assert!(fix.contains("ask the user"), "{fix}");
        assert_eq!(checks[1].status, Status::Pass);
        assert!(
            checks[1].message.ends_with(": connected"),
            "{}",
            checks[1].message
        );
        assert_eq!(checks[2].status, Status::Warn);
        assert!(checks[2].message.contains("999"), "{}", checks[2].message);
        assert!(
            checks[2].message.contains("no such Chrome profile"),
            "{}",
            checks[2].message
        );

        // Local State unreadable: the profile is unknown, not missing, and
        // the rule is not applied — what the runtime does.
        let checks = rule_target_checks(&rules, None, &rows, Some("/chrome"));
        assert_eq!(checks.len(), 3);
        for c in &checks {
            assert_eq!(c.status, Status::Warn);
            assert!(c.message.contains("unknown"), "{}", c.message);
            assert!(
                c.message.contains("Local State could not be read"),
                "{}",
                c.message
            );
            assert!(c.message.contains("not applied"), "{}", c.message);
            assert!(
                !c.message.contains("no such Chrome profile"),
                "{}",
                c.message
            );
        }
    }
}
