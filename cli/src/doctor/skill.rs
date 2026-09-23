//! Read-only probe: is the chrome-use agent skill installed anywhere an
//! agent runner would look? Never writes; shares the native installer's paths.
//! Never Fail: other runners may use directories we do not probe.

use super::{Check, Status};
use std::path::PathBuf;

/// Given candidate base dirs, return those that actually contain a
/// `chrome-use/SKILL.md`. Pure + injectable for tests.
fn probe_installed(candidates: &[PathBuf]) -> Vec<PathBuf> {
    candidates
        .iter()
        .filter(|d| d.join("chrome-use").join("SKILL.md").is_file())
        .cloned()
        .collect()
}

/// Candidate skill base dirs, using the installer's custom-home handling too.
fn candidate_dirs() -> Vec<PathBuf> {
    let mut v = crate::skills::install::global_dirs().unwrap_or_default();
    // Older installers used Codex's legacy directory. Keep recognizing those
    // copies without creating another copy there on a fresh install.
    let codex = std::env::var_os("CODEX_HOME")
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .or_else(|| dirs::home_dir().map(|h| h.join(".codex")));
    if let Some(codex) = codex {
        v.push(codex.join("skills"));
    }
    if let Ok(cwd) = std::env::current_dir() {
        v.extend(crate::skills::install::project_dirs(&cwd));
    }
    v
}

pub(super) fn check(checks: &mut Vec<Check>) {
    let category = "Agent skill";
    let found = probe_installed(&candidate_dirs());
    if found.is_empty() {
        checks.push(
            Check::new(
                "skill.installed",
                category,
                Status::Warn,
                "chrome-use agent skill not found in the probed runner dirs \
                 (other runners may use directories not probed here)",
            )
            .with_fix("chrome-use skill install"),
        );
    } else {
        checks.push(Check::new(
            "skill.installed",
            category,
            Status::Pass,
            format!("Agent skill installed ({} location(s))", found.len()),
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn probe_finds_installed_skill() {
        let tmp = std::env::temp_dir().join(format!("cu-doctor-skill-{}", std::process::id()));
        let claude = tmp.join(".claude/skills");
        fs::create_dir_all(claude.join("chrome-use")).unwrap();
        fs::write(claude.join("chrome-use/SKILL.md"), "stub").unwrap();
        let empty = tmp.join(".codex/skills");
        fs::create_dir_all(&empty).unwrap();

        let found = probe_installed(&[claude.clone(), empty]);
        assert_eq!(found, vec![claude]);
        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn probe_empty_when_none_installed() {
        let tmp = std::env::temp_dir().join(format!("cu-doctor-skill-none-{}", std::process::id()));
        let d = tmp.join(".claude/skills");
        fs::create_dir_all(&d).unwrap();
        assert!(probe_installed(&[d]).is_empty());
        let _ = fs::remove_dir_all(&tmp);
    }
}
