//! Install the discovery entry bundled with this binary, without Node or Git.
//! Keep installation and doctor's search paths together so success names a
//! file a runner can actually discover. Additional runners can use skills.sh.

use std::env;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

pub(super) const CONTENT: &[u8] = include_bytes!("../../../skills/chrome-use/SKILL.md");

fn configured_dir(name: &str, fallback: PathBuf) -> PathBuf {
    env::var_os(name)
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .unwrap_or(fallback)
}

/// Shared skills (including Codex), plus Claude Code and Cursor.
/// Do not also write ~/.codex/skills: Codex discovers both locations and
/// lists two copies instead of merging skills with the same name.
pub(crate) fn global_dirs() -> Result<Vec<PathBuf>, String> {
    // Windows' dirs crate consults the registry, ignoring USERPROFILE. Honor
    // the process's profile first, matching the PowerShell installer and
    // allowing an isolated profile without changing the machine's registry.
    #[cfg(windows)]
    let home = env::var_os("USERPROFILE")
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .or_else(dirs::home_dir);
    #[cfg(not(windows))]
    let home = dirs::home_dir();
    let home = home.ok_or("Cannot determine the home directory")?;
    let mut dirs = vec![
        home.join(".agents/skills"),
        configured_dir("CLAUDE_CONFIG_DIR", home.join(".claude")).join("skills"),
        home.join(".cursor/skills"),
    ];
    let config = configured_dir("XDG_CONFIG_HOME", home.join(".config"));
    // These runners do not all read the shared directory. Install into their
    // own directory when their configuration exists. Paths follow skills.sh's
    // src/agents.ts (https://github.com/vercel-labs/skills).
    for root in [
        home.join(".pi/agent"),
        home.join(".codeium/windsurf"),
        config.join("opencode"),
        home.join(".codebuddy"),
        home.join(".trae"),
        home.join(".trae-cn"),
    ] {
        if root.is_dir() {
            dirs.push(root.join("skills"));
        }
    }
    let mut unique = Vec::new();
    for dir in dirs {
        if !unique.contains(&dir) {
            unique.push(dir);
        }
    }
    Ok(unique)
}

pub(crate) fn project_dirs(root: &Path) -> Vec<PathBuf> {
    let mut dirs = vec![root.join(".agents/skills"), root.join(".claude/skills")];
    for runner in [".pi", ".windsurf", ".codebuddy", ".trae"] {
        if root.join(runner).is_dir() {
            dirs.push(root.join(runner).join("skills"));
        }
    }
    dirs
}

/// Replace only SKILL.md, including a previous symlink, preserving unrelated
/// files. Stage beside the destination so failed writes leave the old file
/// intact. Read the destination back before reporting a verified installation.
fn install_one(base: &Path) -> Result<PathBuf, String> {
    let dir = base.join("chrome-use");
    let target = dir.join("SKILL.md");
    let write = || -> std::io::Result<()> {
        fs::create_dir_all(&dir)?;
        let staging = dir.join(format!(".SKILL-{}.tmp", uuid::Uuid::new_v4()));
        let result = (|| {
            let mut file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&staging)?;
            file.write_all(CONTENT)?;
            file.sync_all()?;
            drop(file);
            fs::rename(&staging, &target)?;
            if fs::read(&target)? != CONTENT {
                return Err(std::io::Error::other(
                    "installed content differs from bundled skill",
                ));
            }
            Ok(())
        })();
        if result.is_err() {
            let _ = fs::remove_file(&staging);
        }
        result
    };
    write().map_err(|e| format!("{}: {e}", target.display()))?;
    Ok(target)
}

/// Try every destination, but any failed destination makes the command fail.
/// Partial installation is useful evidence, never overall success.
pub(super) fn install_all(dirs: &[PathBuf]) -> (Vec<PathBuf>, Vec<String>) {
    let mut installed = Vec::new();
    let mut errors = Vec::new();
    for dir in dirs {
        match install_one(dir) {
            Ok(path) => installed.push(path),
            Err(error) => errors.push(error),
        }
    }
    if dirs.is_empty() {
        errors.push("No skill installation directories found".to_string());
    }
    (installed, errors)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refresh_replaces_old_content_and_preserves_other_files() {
        let tmp = tempfile::tempdir().unwrap();
        let base = tmp.path().join("runner with spaces/skills");
        let first = install_one(&base).unwrap();
        fs::write(&first, "old skill").unwrap();
        let other = first.parent().unwrap().join("notes.md");
        fs::write(&other, "keep").unwrap();
        assert_eq!(install_one(&base).unwrap(), first);
        assert_eq!(fs::read(first).unwrap(), CONTENT);
        assert_eq!(fs::read_to_string(other).unwrap(), "keep");
    }

    #[test]
    fn failed_destination_is_reported_even_if_another_succeeds() {
        let tmp = tempfile::tempdir().unwrap();
        let blocked = tmp.path().join("blocked");
        fs::write(&blocked, "not a directory").unwrap();
        let (installed, errors) = install_all(&[blocked.clone(), tmp.path().join("ok")]);
        assert_eq!(installed.len(), 1);
        assert_eq!(errors.len(), 1);
        assert!(errors[0].contains("blocked"));
        assert_eq!(fs::read_to_string(blocked).unwrap(), "not a directory");
    }

    #[test]
    fn failed_replacement_preserves_destination_and_cleans_staging() {
        let tmp = tempfile::tempdir().unwrap();
        let target = tmp.path().join("chrome-use/SKILL.md");
        fs::create_dir_all(&target).unwrap();
        fs::write(target.join("keep"), "untouched").unwrap();
        assert!(install_one(tmp.path()).is_err());
        assert_eq!(
            fs::read_to_string(target.join("keep")).unwrap(),
            "untouched"
        );
        assert_eq!(fs::read_dir(target.parent().unwrap()).unwrap().count(), 1);
    }
}
