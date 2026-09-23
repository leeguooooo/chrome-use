//! Check the Chrome install: binary path, version, cache dirs, user-data
//! dir, and the optional lightpanda engine.

use std::env;
use std::path::{Path, PathBuf};

use super::helpers::which_exists;
use super::{Check, Status};

pub(super) fn check(checks: &mut Vec<Check>) {
    let category = "Chrome";

    let chrome = crate::native::cdp::chrome::find_chrome();
    match chrome {
        Some(path) => {
            let label = path.display().to_string();
            match query_chrome_version(&path) {
                Some(version) => checks.push(Check::new(
                    "chrome.installed",
                    category,
                    Status::Pass,
                    format!("{} at {}", version, label),
                )),
                None => checks.push(Check::new(
                    "chrome.installed",
                    category,
                    Status::Pass,
                    format!("Chrome at {} (version unknown)", label),
                )),
            }
        }
        None => checks.push(
            Check::new(
                "chrome.installed",
                category,
                Status::Fail,
                "No Chrome binary found",
            )
            .with_fix("chrome-use install"),
        ),
    }

    let cache_dir = crate::install::get_browsers_dir();
    if cache_dir.exists() {
        checks.push(Check::new(
            "chrome.cache_dir",
            category,
            Status::Info,
            format!("Cache dir {}", cache_dir.display()),
        ));
    }

    if let Some(puppeteer_dir) = puppeteer_cache_dir() {
        if puppeteer_dir.exists() {
            checks.push(Check::new(
                "chrome.puppeteer_cache",
                category,
                Status::Info,
                format!(
                    "Puppeteer cache also present: {} (will be used as a fallback)",
                    puppeteer_dir.display()
                ),
            ));
        }
    }

    if let Some(user_data_dir) = crate::native::cdp::chrome::find_chrome_user_data_dir() {
        let profiles = crate::native::cdp::chrome::list_chrome_profiles(&user_data_dir);
        let count = profiles.len();
        let dir_label = user_data_dir.display().to_string();
        if count == 0 {
            checks.push(Check::new(
                "chrome.user_data_dir",
                category,
                Status::Info,
                format!(
                    "Chrome user data dir found ({}), no profiles parsed",
                    dir_label
                ),
            ));
        } else {
            checks.push(Check::new(
                "chrome.user_data_dir",
                category,
                Status::Info,
                format!("{} Chrome profile(s) at {}", count, dir_label),
            ));
        }
    }

    if let Ok(engine) = env::var("AGENT_BROWSER_ENGINE") {
        if engine == "lightpanda" {
            // Best-effort PATH lookup; absence is FAIL only when the user
            // explicitly opted into the lightpanda engine.
            if which_exists("lightpanda") {
                checks.push(Check::new(
                    "chrome.engine_lightpanda",
                    category,
                    Status::Pass,
                    "Lightpanda binary on PATH",
                ));
            } else {
                checks.push(
                    Check::new(
                        "chrome.engine_lightpanda",
                        category,
                        Status::Fail,
                        "AGENT_BROWSER_ENGINE=lightpanda but no lightpanda binary on PATH",
                    )
                    .with_fix("install lightpanda or unset AGENT_BROWSER_ENGINE"),
                );
            }
        }
    }
}

/// On Windows, never run `chrome.exe --version`. It does not print a version and
/// exit there: it starts the browser, against the user's real profile, and never
/// returns. Measured on Windows 11 over SSH: no output after 15s and nine new
/// chrome.exe processes, one of them trying to resume a download from the
/// user's profile — and `doctor --quick --offline`, which the Windows installer
/// runs as its self-check, hung on it indefinitely. `.output()` would wait even
/// after the main process exited, since Chrome's children inherit the pipe.
///
/// The installer lays out `Application\chrome.exe` beside a directory named for
/// the installed version (`Application\153.0.8010.53\`), so read that instead:
/// nothing is launched.
#[cfg(windows)]
fn query_chrome_version(path: &Path) -> Option<String> {
    let dir = path.parent()?;
    let names = std::fs::read_dir(dir)
        .ok()?
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().map(|t| t.is_dir()).unwrap_or(false))
        .filter_map(|e| e.file_name().into_string().ok());
    highest_version_dir(names).map(|v| format!("Chrome {v}"))
}

/// The highest `a.b.c.d` name among `names`, compared numerically. Chrome can
/// leave the previous version's directory behind until the next restart, so
/// there may be two.
#[cfg_attr(not(windows), allow(dead_code))]
fn highest_version_dir(names: impl Iterator<Item = String>) -> Option<String> {
    names
        .filter_map(|n| {
            let parts: Vec<u32> = n
                .split('.')
                .map(|p| p.parse().ok())
                .collect::<Option<_>>()?;
            (parts.len() == 4).then_some((parts, n))
        })
        .max_by(|a, b| a.0.cmp(&b.0))
        .map(|(_, n)| n)
}

#[cfg(not(windows))]
fn query_chrome_version(path: &Path) -> Option<String> {
    let output = std::process::Command::new(path)
        .arg("--version")
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if s.is_empty() {
        None
    } else {
        Some(s)
    }
}

pub(super) fn puppeteer_cache_dir() -> Option<PathBuf> {
    if let Ok(p) = env::var("PUPPETEER_CACHE_DIR") {
        return Some(PathBuf::from(p));
    }
    dirs::home_dir().map(|h| h.join(".cache").join("puppeteer"))
}

#[cfg(test)]
mod tests {
    use super::highest_version_dir;

    #[test]
    fn the_newest_version_directory_wins_numerically() {
        let names = [
            "152.0.7990.12",
            "153.0.8010.53",
            "SetupMetrics",
            "Locales",
            "153.0.8010.9",
        ]
        .map(String::from);
        // 53 > 9 numerically, though "9" > "5" as text.
        assert_eq!(
            highest_version_dir(names.into_iter()).as_deref(),
            Some("153.0.8010.53")
        );
    }

    #[test]
    fn nothing_version_shaped_means_unknown() {
        let names = ["Locales", "1.2.3", "a.b.c.d", "153.0.8010.53.1"].map(String::from);
        assert_eq!(highest_version_dir(names.into_iter()), None);
    }

    use super::*;

    #[test]
    fn test_puppeteer_cache_dir_returns_sensible_default() {
        // When PUPPETEER_CACHE_DIR is unset, we fall back to
        // ~/.cache/puppeteer. Mutating env vars here would race with other
        // tests, so just verify the fallback path is shaped correctly.
        if env::var("PUPPETEER_CACHE_DIR").is_err() {
            let dir = puppeteer_cache_dir().expect("home dir should resolve in tests");
            let s = dir.to_string_lossy();
            assert!(s.contains(".cache"));
            assert!(s.ends_with("puppeteer"));
        }
    }
}
