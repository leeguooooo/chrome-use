use crate::color;
use std::path::PathBuf;
use std::process::{exit, Command, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

const CURRENT_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Canonical installer for the stealth fork. `upgrade` just re-runs it, so the
/// upgrade path and the install path are identical (GitHub Release, no npm).
const INSTALL_URL: &str = "https://raw.githubusercontent.com/leeguooooo/chrome-use/main/install.sh";

/// GitHub API for the latest published release (used by the update check).
const LATEST_RELEASE_API: &str =
    "https://api.github.com/repos/leeguooooo/chrome-use/releases/latest";

/// Re-check the latest version at most this often (seconds).
const UPDATE_CHECK_INTERVAL_SECS: u64 = 86_400; // once a day

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn update_cache_path() -> PathBuf {
    crate::connection::config_home().join("update-check.json")
}

fn write_update_cache(checked_at: u64, latest: &str) {
    let path = update_cache_path();
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let body = serde_json::json!({ "checked_at": checked_at, "latest": latest }).to_string();
    let _ = std::fs::write(&path, body);
}

/// Parse a dotted version (`1.2.1`, `v1.2.1`, `1.2.1-fork.3`) into a comparable
/// `(major, minor, patch)`, ignoring any pre-release/build suffix.
fn parse_version(v: &str) -> Option<(u64, u64, u64)> {
    let core = v.trim().trim_start_matches('v');
    let core = core.split(['-', '+']).next().unwrap_or(core);
    let mut parts = core.split('.');
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next().unwrap_or("0").parse().ok()?;
    let patch = parts.next().unwrap_or("0").parse().ok()?;
    Some((major, minor, patch))
}

fn is_newer(latest: &str, current: &str) -> bool {
    matches!((parse_version(latest), parse_version(current)), (Some(l), Some(c)) if l > c)
}

/// Public semver-ish comparison (`latest` strictly newer than `current`), so
/// `doctor` can flag a stale extension/CLI without re-implementing parsing.
pub fn version_is_newer(latest: &str, current: &str) -> bool {
    is_newer(latest, current)
}

/// The latest CLI version recorded by the background update check, if any.
/// `doctor` uses it to show "a newer chrome-use is available" without a network
/// call (the `__update-check` worker refreshes the cache out of band).
pub fn cached_latest_version() -> Option<String> {
    std::fs::read_to_string(update_cache_path())
        .ok()
        .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
        .and_then(|j| {
            j.get("latest")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string())
        })
        .filter(|s| !s.is_empty())
}

/// Hidden `__update-check` subcommand: fetch the latest release tag and cache it.
/// Spawned detached by [`maybe_notify_update`] so the network call never blocks a
/// real command. Uses `curl` (no extra deps, matches `upgrade`).
pub fn run_update_check() {
    let latest = Command::new("curl")
        .args([
            "-fsSL",
            "--max-time",
            "8",
            "-H",
            "User-Agent: chrome-use-update-check",
            LATEST_RELEASE_API,
        ])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| serde_json::from_slice::<serde_json::Value>(&o.stdout).ok())
        .and_then(|j| {
            j.get("tag_name")
                .and_then(|v| v.as_str())
                .map(|s| s.trim_start_matches('v').to_string())
        });
    if let Some(latest) = latest {
        write_update_cache(now_secs(), &latest);
    }
}

/// Non-blocking "update available" notice. Called once per command run:
/// - prints a one-line hint to **stderr** (never stdout, so `--json` is clean)
///   when a cached release is newer than the running binary;
/// - refreshes the cached latest version at most once a day via a **detached**
///   background process, so the current command never waits on the network.
///
/// Skipped for meta commands (upgrade/install/doctor/`__*`/--version/--help),
/// in CI, in daemon mode, and when CHROME_USE_NO_UPDATE_CHECK /
/// AGENT_BROWSER_NO_UPDATE_CHECK is set.
pub fn maybe_notify_update() {
    if std::env::var_os("CHROME_USE_NO_UPDATE_CHECK").is_some()
        || std::env::var_os("AGENT_BROWSER_NO_UPDATE_CHECK").is_some()
        || std::env::var_os("CI").is_some()
        || std::env::var_os("AGENT_BROWSER_DAEMON").is_some()
    {
        return;
    }
    let first = std::env::args().nth(1).unwrap_or_default();
    if first.starts_with("__")
        || matches!(
            first.as_str(),
            "upgrade" | "install" | "doctor" | "dashboard" | "daemon"
        )
    {
        return;
    }
    if std::env::args().any(|a| matches!(a.as_str(), "--version" | "-V" | "--help" | "-h")) {
        return;
    }

    let (checked_at, latest) = std::fs::read_to_string(update_cache_path())
        .ok()
        .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
        .map(|j| {
            (
                j.get("checked_at").and_then(|v| v.as_u64()).unwrap_or(0),
                j.get("latest")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default()
                    .to_string(),
            )
        })
        .unwrap_or((0, String::new()));

    if is_newer(&latest, CURRENT_VERSION) {
        // Opt-in auto-update (`CHROME_USE_AUTO_UPGRADE=1`): like the Web Store
        // extension, keep the CLI current with zero manual steps. We NEVER swap
        // the running binary mid-command — we fire a detached background upgrade
        // that install.sh applies in place, so the user's NEXT run is on the new
        // version. Guarded: only when the binary dir is writable (an install.sh
        // ~/.local/bin, not a package-manager/system path), not Windows, and
        // debounced so repeated commands don't spawn a swarm of upgraders.
        if auto_upgrade_enabled() && !recently_attempted_auto_upgrade() && binary_dir_writable() {
            touch_auto_upgrade_marker();
            if let Ok(exe) = std::env::current_exe() {
                let _ = Command::new(exe)
                    .arg("__auto-upgrade")
                    .stdin(Stdio::null())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .spawn();
            }
            eprintln!(
                "{} auto-updating chrome-use → v{latest} in the background (you have {CURRENT_VERSION}; takes effect on your next run)",
                color::warning_indicator()
            );
        } else {
            let hint = if cfg!(windows) {
                ""
            } else {
                " (or set CHROME_USE_AUTO_UPGRADE=1 to auto-update)"
            };
            eprintln!(
                "{} chrome-use {latest} is available (you have {CURRENT_VERSION}) — run `chrome-use upgrade`{hint}",
                color::warning_indicator()
            );
        }
    }

    // Refresh in the background at most once a day. Bump the timestamp first
    // (keeping the last-known latest) so concurrent runs don't all spawn a
    // checker, then fire a detached child that does the network fetch.
    if now_secs().saturating_sub(checked_at) >= UPDATE_CHECK_INTERVAL_SECS {
        write_update_cache(now_secs(), &latest);
        if let Ok(exe) = std::env::current_exe() {
            let _ = Command::new(exe)
                .arg("__update-check")
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn();
        }
    }
}

/// Opt-in flag for background CLI auto-update.
fn auto_upgrade_enabled() -> bool {
    matches!(
        std::env::var("CHROME_USE_AUTO_UPGRADE").ok().as_deref(),
        Some("1") | Some("true") | Some("yes")
    )
}

fn auto_upgrade_marker() -> PathBuf {
    crate::connection::config_home().join("auto-upgrade-attempt")
}

/// Debounce: don't spawn another background upgrade within 10 minutes of the
/// last attempt, so a burst of commands (before the new binary lands) doesn't
/// launch a swarm of concurrent installers.
fn recently_attempted_auto_upgrade() -> bool {
    std::fs::metadata(auto_upgrade_marker())
        .ok()
        .and_then(|m| m.modified().ok())
        .and_then(|t| t.elapsed().ok())
        .map(|d| d.as_secs() < 600)
        .unwrap_or(false)
}

fn touch_auto_upgrade_marker() {
    let path = auto_upgrade_marker();
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::write(&path, now_secs().to_string());
}

/// Whether the running binary lives in a directory we can write to — an
/// install.sh `~/.local/bin`, not a Homebrew/apt/system path. Auto-update only
/// fires when true, so we never fight a package manager or need `sudo`. Probes
/// by creating and removing a temp file in the binary's directory.
fn binary_dir_writable() -> bool {
    let Some(dir) = std::env::current_exe()
        .ok()
        .and_then(|p| p.canonicalize().ok())
        .and_then(|p| p.parent().map(|d| d.to_path_buf()))
    else {
        return false;
    };
    let probe = dir.join(".chrome-use-upgrade-probe");
    match std::fs::File::create(&probe) {
        Ok(_) => {
            let _ = std::fs::remove_file(&probe);
            true
        }
        Err(_) => false,
    }
}

/// Quiet, non-interactive in-place upgrade for the detached `__auto-upgrade`
/// child spawned by [`maybe_notify_update`]. Runs install.sh into the running
/// binary's directory with all output suppressed; failures are silent (the
/// notice reappears next run and the user can `chrome-use upgrade` manually).
pub fn run_auto_upgrade() {
    #[cfg(not(windows))]
    {
        let bin_dir = std::env::current_exe()
            .ok()
            .and_then(|p| p.canonicalize().ok())
            .and_then(|p| p.parent().map(|d| d.to_path_buf()));
        let install_cmd = format!("curl -fsSL {} | sh", INSTALL_URL);
        let mut cmd = Command::new("sh");
        cmd.arg("-c")
            .arg(&install_cmd)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        if let Some(ref dir) = bin_dir {
            cmd.env("AGENT_BROWSER_BIN_DIR", dir);
        }
        let _ = cmd.status();
    }
}

/// Upgrade to the latest GitHub Release.
///
/// The stealth fork ships as a prebuilt binary attached to a GitHub Release —
/// NOT via the npm registry. Earlier this command (inherited from upstream)
/// ran `npm/pnpm install -g chrome-use@latest`, which installed the
/// UNRELATED upstream `chrome-use` package and clobbered the user's setup.
/// Now `upgrade` simply re-runs install.sh into the same directory as the
/// current binary, so it always tracks the freshest GitHub Release.
pub fn run_upgrade() {
    println!(
        "{}",
        color::cyan(&format!(
            "Upgrading chrome-use (currently v{}) from the latest GitHub Release...",
            CURRENT_VERSION
        ))
    );

    #[cfg(windows)]
    {
        eprintln!(
            "{} Automatic upgrade isn't supported on Windows.",
            color::warning_indicator()
        );
        eprintln!("  Download the latest chrome-use-win32-x64.tar.gz from:");
        eprintln!("    https://github.com/leeguooooo/chrome-use/releases/latest");
        eprintln!("  and replace chrome-use.exe on your PATH.");
        exit(1);
    }

    #[cfg(not(windows))]
    {
        // Install into the SAME directory as the running binary (in-place
        // upgrade), so we don't create a second copy elsewhere on PATH.
        let bin_dir = std::env::current_exe()
            .ok()
            .and_then(|p| p.canonicalize().ok())
            .and_then(|p| p.parent().map(|d| d.to_path_buf()));

        let install_cmd = format!("curl -fsSL {} | sh", INSTALL_URL);
        println!("Running: {}", install_cmd);

        let mut cmd = Command::new("sh");
        cmd.arg("-c").arg(&install_cmd);
        if let Some(ref dir) = bin_dir {
            cmd.env("AGENT_BROWSER_BIN_DIR", dir);
        }

        let ok = cmd.status().map(|s| s.success()).unwrap_or(false);
        if ok {
            println!(
                "{} Upgrade complete — run `chrome-use --version` to confirm.",
                color::success_indicator()
            );
        } else {
            eprintln!(
                "{} Upgrade failed. Install manually:",
                color::error_indicator()
            );
            eprintln!("  curl -fsSL {} | sh", INSTALL_URL);
            exit(1);
        }
    }
}
