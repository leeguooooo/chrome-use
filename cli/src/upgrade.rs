//! `chrome-use upgrade` and the daily "new version" notice.
//!
//! Follows the *-use family upgrade convention
//! (https://github.com/leeguooooo/plugins/blob/main/docs/upgrade.md):
//!
//! - `upgrade` installs the latest GitHub Release through install.sh, then
//!   refreshes every installed copy of the skill it can find.
//! - `upgrade --check` / `upgrade --json` change nothing and report
//!   current vs latest plus the installed skills.
//! - Exit 0 on success (upgraded, already current, or a check that ran),
//!   2 when the check or the download failed.
//! - Any other command checks GitHub at most once a day (in the background)
//!   and, while the cached release is newer than the running binary, starts a
//!   background in-place upgrade (on by default, `CHROME_USE_AUTO_UPGRADE=0`
//!   turns it off) or prints one stderr line per run.
//! - The native-messaging host does the same when Chrome starts it, and
//!   reports the CLI's version to the extension, whose banner can start the
//!   upgrade too.

use crate::color;
use serde::Serialize;
use std::ffi::OsString;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{exit, Command, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

const NAME: &str = "chrome-use";
const CURRENT_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Claude Code plugin id in the family marketplace.
const PLUGIN_ID: &str = "chrome-use@leeguooooo-plugins";

/// Canonical installer for the stealth fork. `upgrade` just re-runs it, so the
/// upgrade path and the install path are identical (GitHub Release, no npm).
const INSTALL_URL: &str = "https://raw.githubusercontent.com/leeguooooo/chrome-use/main/install.sh";

/// GitHub API for the latest non-prerelease release.
const LATEST_RELEASE_API: &str =
    "https://api.github.com/repos/leeguooooo/chrome-use/releases/latest";

/// Overrides [`LATEST_RELEASE_API`]. Tests point it at a `file://` fixture so
/// no test touches the network; curl reads `file://` URLs natively.
const RELEASE_API_ENV: &str = "CHROME_USE_RELEASE_API_URL";

/// Re-check the latest version at most this often (seconds).
const UPDATE_CHECK_INTERVAL_SECS: u64 = 86_400; // once a day

/// Timeout for the background daily check (spec: 2 seconds).
const NOTICE_CHECK_TIMEOUT_SECS: u64 = 2;

/// Timeout for an explicit `upgrade` / `upgrade --check`.
const EXPLICIT_CHECK_TIMEOUT_SECS: u64 = 10;

/// Any of these disables both the daily check and the notice.
/// `CI`, `CHROME_USE_NO_UPDATE_CHECK` and the family-wide `USE_NO_UPDATE_CHECK`
/// come from the convention; the next two are this fork's older names and a
/// daemon child, which must never print anything of its own. `CU_BWU_CHILD`
/// marks the child `auth login --bwu` runs under `bwu run`: the login's values
/// are still in its environment when the check would run, so a background
/// upgrade (or check worker) it started would inherit them, and on macOS write
/// them into the upgrade's launchd plist.
const CHECK_OPT_OUT_VARS: &[&str] = &[
    "CI",
    "CHROME_USE_NO_UPDATE_CHECK",
    "USE_NO_UPDATE_CHECK",
    "AGENT_BROWSER_NO_UPDATE_CHECK",
    "AGENT_BROWSER_DAEMON",
    "CU_BWU_CHILD",
];

/// These silence only the notice line; the daily check (and an opted-in
/// auto-upgrade) still run.
const NOTICE_OPT_OUT_VARS: &[&str] = &["CHROME_USE_NO_UPDATE_NOTICE", "NO_UPDATE_NOTIFIER"];

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

// ---------------------------------------------------------------------------
// Versions
// ---------------------------------------------------------------------------

/// Parse a dotted version (`1.2.1`, `v1.2.1`, `1.2.1-rc.3`) into its
/// `(major, minor, patch)` core plus the pre-release part (build metadata
/// after `+` is ignored, as semver says).
/// `(major, minor, patch)` plus the optional pre-release identifiers.
type ParsedVersion<'a> = ((u64, u64, u64), Option<&'a str>);

fn parse_version(v: &str) -> Option<ParsedVersion<'_>> {
    let v = v.trim().trim_start_matches('v');
    let v = v.split('+').next().unwrap_or(v);
    let (core, pre) = match v.split_once('-') {
        Some((core, pre)) => (core, Some(pre).filter(|p| !p.is_empty())),
        None => (v, None),
    };
    let mut parts = core.split('.');
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next().unwrap_or("0").parse().ok()?;
    let patch = parts.next().unwrap_or("0").parse().ok()?;
    Some(((major, minor, patch), pre))
}

/// Semver pre-release precedence: a release outranks any pre-release of the
/// same core; identifiers compare numerically when both are numeric, numeric
/// ranks below alphanumeric, and a longer list wins a shared prefix.
fn cmp_prerelease(a: Option<&str>, b: Option<&str>) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    match (a, b) {
        (None, None) => Ordering::Equal,
        (None, Some(_)) => Ordering::Greater,
        (Some(_), None) => Ordering::Less,
        (Some(a), Some(b)) => {
            let mut ai = a.split('.');
            let mut bi = b.split('.');
            loop {
                match (ai.next(), bi.next()) {
                    (None, None) => return Ordering::Equal,
                    (None, Some(_)) => return Ordering::Less,
                    (Some(_), None) => return Ordering::Greater,
                    (Some(x), Some(y)) => {
                        let ord = match (x.parse::<u64>(), y.parse::<u64>()) {
                            (Ok(x), Ok(y)) => x.cmp(&y),
                            (Ok(_), Err(_)) => Ordering::Less,
                            (Err(_), Ok(_)) => Ordering::Greater,
                            (Err(_), Err(_)) => x.cmp(y),
                        };
                        if ord != Ordering::Equal {
                            return ord;
                        }
                    }
                }
            }
        }
    }
}

fn is_newer(latest: &str, current: &str) -> bool {
    match (parse_version(latest), parse_version(current)) {
        (Some((l, lp)), Some((c, cp))) => {
            l > c || (l == c && cmp_prerelease(lp, cp) == std::cmp::Ordering::Greater)
        }
        _ => false,
    }
}

/// Public semver-ish comparison (`latest` strictly newer than `current`), so
/// `doctor` can flag a stale extension/CLI without re-implementing parsing.
pub fn version_is_newer(latest: &str, current: &str) -> bool {
    is_newer(latest, current)
}

// ---------------------------------------------------------------------------
// Latest release lookup
// ---------------------------------------------------------------------------

/// Extract `X.Y.Z` from a GitHub `releases/latest` response.
fn parse_latest_release(body: &[u8]) -> Result<String, String> {
    let json: serde_json::Value = serde_json::from_slice(body)
        .map_err(|e| format!("unexpected response from GitHub: {e}"))?;
    if json.get("prerelease").and_then(|v| v.as_bool()) == Some(true)
        || json.get("draft").and_then(|v| v.as_bool()) == Some(true)
    {
        return Err("the latest release is marked prerelease/draft".to_string());
    }
    let tag = json
        .get("tag_name")
        .and_then(|v| v.as_str())
        .ok_or_else(|| {
            let msg = json
                .get("message")
                .and_then(|v| v.as_str())
                .unwrap_or("no tag_name in response");
            format!("GitHub API: {msg}")
        })?;
    let version = tag.trim().trim_start_matches('v').to_string();
    if parse_version(&version).is_none() {
        return Err(format!("latest release tag is not a version: {tag}"));
    }
    Ok(version)
}

/// Ask GitHub for the newest non-prerelease release. Uses `curl` (already a
/// hard requirement of install.sh) so this needs no async runtime. A
/// `GITHUB_TOKEN` is passed through curl's stdin config, never argv, so it
/// does not show up in `ps`.
fn fetch_latest_version(timeout_secs: u64) -> Result<String, String> {
    let url = std::env::var(RELEASE_API_ENV)
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| LATEST_RELEASE_API.to_string());
    let token = std::env::var("GITHUB_TOKEN")
        .ok()
        .filter(|s| !s.trim().is_empty());

    let mut cmd = Command::new("curl");
    cmd.args([
        "-fsSL",
        "--max-time",
        &timeout_secs.to_string(),
        "-H",
        "Accept: application/vnd.github+json",
        "-H",
        &format!("User-Agent: {NAME}/{CURRENT_VERSION}"),
    ]);
    if token.is_some() {
        cmd.args(["-K", "-"]);
    }
    cmd.arg(&url)
        .stdin(if token.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    let mut child = cmd
        .spawn()
        .map_err(|e| format!("could not run curl: {e}"))?;
    if let (Some(token), Some(mut stdin)) = (token, child.stdin.take()) {
        let _ = writeln!(
            stdin,
            "header = \"Authorization: Bearer {}\"",
            token.trim().replace('"', "")
        );
    }
    let out = child
        .wait_with_output()
        .map_err(|e| format!("curl failed: {e}"))?;
    if !out.status.success() {
        let why = String::from_utf8_lossy(&out.stderr).trim().to_string();
        return Err(if why.is_empty() {
            format!("could not reach {url}")
        } else {
            why
        });
    }
    parse_latest_release(&out.stdout)
}

// ---------------------------------------------------------------------------
// Daily check cache
// ---------------------------------------------------------------------------

#[derive(Debug, Default, Clone, PartialEq, serde::Deserialize, Serialize)]
struct UpdateCache {
    #[serde(default)]
    checked_at: u64,
    #[serde(default)]
    latest: String,
}

/// `${XDG_CACHE_HOME:-~/.cache}/chrome-use/update-check.json`, as the family
/// convention specifies (on macOS too, not `~/Library/Caches`).
fn cache_path_from(xdg_cache_home: Option<OsString>, home: Option<PathBuf>) -> PathBuf {
    let base = xdg_cache_home
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| home.map(|h| h.join(".cache")))
        .unwrap_or_else(std::env::temp_dir);
    base.join(NAME).join("update-check.json")
}

fn update_cache_path() -> PathBuf {
    cache_path_from(std::env::var_os("XDG_CACHE_HOME"), dirs::home_dir())
}

fn read_cache_at(path: &Path) -> UpdateCache {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

/// Write the cache through a sibling temp file and a rename, so a concurrent
/// reader (another command, `doctor`, the detached checker) never sees a
/// truncated file. Failures are ignored: the cache is only a hint.
fn write_cache_at(path: &Path, cache: &UpdateCache) {
    let Some(parent) = path.parent() else {
        return;
    };
    let _ = std::fs::create_dir_all(parent);
    let Ok(body) = serde_json::to_string(cache) else {
        return;
    };
    let tmp = parent.join(format!(
        ".update-check.{}.{}.tmp",
        std::process::id(),
        uuid::Uuid::new_v4()
    ));
    if std::fs::write(&tmp, body).is_err() || std::fs::rename(&tmp, path).is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
}

fn write_update_cache(checked_at: u64, latest: &str) {
    write_cache_at(
        &update_cache_path(),
        &UpdateCache {
            checked_at,
            latest: latest.to_string(),
        },
    );
}

/// Is the cached result older than a day (so a new check is due)?
fn check_due(cache: &UpdateCache, now: u64) -> bool {
    now.saturating_sub(cache.checked_at) >= UPDATE_CHECK_INTERVAL_SECS
}

/// The latest CLI version recorded by the daily update check, if any.
/// `doctor` uses it to show "a newer chrome-use is available" without a network
/// call (the `__update-check` worker refreshes the cache out of band).
pub fn cached_latest_version() -> Option<String> {
    Some(read_cache_at(&update_cache_path()).latest).filter(|s| !s.is_empty())
}

/// Hidden `__update-check` subcommand: fetch the latest release tag and cache it.
/// Spawned detached by [`maybe_notify_update`] so the network call never blocks a
/// real command. On failure the cache keeps the `checked_at` bumped by the
/// parent, so an offline machine is not retried on every call.
pub fn run_update_check() {
    if let Ok(latest) = fetch_latest_version(NOTICE_CHECK_TIMEOUT_SECS) {
        write_update_cache(now_secs(), &latest);
    }
}

// ---------------------------------------------------------------------------
// Daily notice
// ---------------------------------------------------------------------------

/// Is the daily check switched off by the environment?
fn update_check_disabled(get: impl Fn(&str) -> Option<OsString>) -> bool {
    CHECK_OPT_OUT_VARS.iter().any(|k| get(k).is_some())
}

fn update_notice_silenced(get: impl Fn(&str) -> Option<OsString>) -> bool {
    NOTICE_OPT_OUT_VARS.iter().any(|k| get(k).is_some())
}

/// Commands that never check: `upgrade` itself, `--version`, `--help`, the
/// hidden `__*` workers, and meta commands that manage the install.
fn skip_for_args(args: &[String]) -> bool {
    let first = args.first().map(String::as_str).unwrap_or_default();
    if first.starts_with("__")
        || matches!(
            first,
            "upgrade" | "install" | "doctor" | "dashboard" | "daemon" | "mcp"
        )
    {
        return true;
    }
    args.iter().any(|a| {
        matches!(
            a.as_str(),
            "upgrade" | "--version" | "-V" | "--help" | "-h" | "help"
        )
    })
}

/// The one stderr line, when `latest` is newer than `current`.
fn notice_line(latest: &str, current: &str) -> Option<String> {
    is_newer(latest, current).then(|| {
        format!("{NAME} {latest} is available (you have {current}). Upgrade: {NAME} upgrade")
    })
}

/// Called once per command run, before dispatch:
/// - prints one line to **stderr** (never stdout, so `--json` stays clean)
///   when the cached latest release is newer than the running binary;
/// - refreshes the cache at most once a day through a **detached** child
///   (`__update-check`, 2 s timeout), so the command never waits on the network.
///
/// Skipped for upgrade/--version/--help and meta commands, in CI, in daemon
/// mode, and when CHROME_USE_NO_UPDATE_CHECK / USE_NO_UPDATE_CHECK /
/// AGENT_BROWSER_NO_UPDATE_CHECK is set.
pub fn maybe_notify_update() {
    let get = |k: &str| std::env::var_os(k);
    let args: Vec<String> = std::env::args().skip(1).collect();
    if update_check_disabled(get) || skip_for_args(&args) {
        return;
    }

    let cache = read_cache_at(&update_cache_path());
    if is_newer(&cache.latest, CURRENT_VERSION) {
        // Background auto-update, on by default (`CHROME_USE_AUTO_UPGRADE=0`
        // turns it off): like the Web Store extension, keep the CLI current
        // with zero manual steps. We NEVER swap the running binary mid-command
        // — a detached background upgrade applies install.sh in place, so the
        // user's NEXT run is on the new version.
        let started = auto_upgrade_enabled() && start_background_upgrade() == Ok(true);
        if !update_notice_silenced(get) {
            if started {
                eprintln!(
                    "{NAME} {} is being installed in the background (you have {CURRENT_VERSION}); it takes effect on your next run",
                    cache.latest
                );
            } else {
                write_notice(&mut std::io::stderr(), &cache.latest, CURRENT_VERSION);
            }
        }
    }
    refresh_latest_if_due();
}

/// The native-messaging host's share of [`maybe_notify_update`]: start the
/// background upgrade when the cached latest release is newer. Chrome starts
/// the host whenever it starts, so this reaches people who never run a
/// command themselves.
pub fn maybe_auto_upgrade_from_host() {
    if update_check_disabled(|k: &str| std::env::var_os(k)) || !auto_upgrade_enabled() {
        return;
    }
    if is_newer(&read_cache_at(&update_cache_path()).latest, CURRENT_VERSION) {
        let _ = start_background_upgrade();
    }
}

/// Refresh the cached latest version at most once a day, through a detached
/// `__update-check` child so nothing waits on the network. Bumps the timestamp
/// first (keeping the last-known latest) so a failed or offline check is not
/// retried on every call and concurrent runs don't all spawn a checker.
///
/// Also called by the native-messaging host: someone who only uses the side
/// panel never runs a command, and the extension's "out of date" banner would
/// otherwise compare against a release from whenever they last did.
pub fn refresh_latest_if_due() {
    if update_check_disabled(|k: &str| std::env::var_os(k)) {
        return;
    }
    let path = update_cache_path();
    let cache = read_cache_at(&path);
    let now = now_secs();
    if !check_due(&cache, now) {
        return;
    }
    write_cache_at(
        &path,
        &UpdateCache {
            checked_at: now,
            latest: cache.latest.clone(),
        },
    );
    if let Ok(exe) = std::env::current_exe() {
        let _ = Command::new(exe)
            .arg("__update-check")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn();
    }
}

/// Write the notice to `err` (stderr in production). Split out so tests can
/// assert on the exact line.
fn write_notice(err: &mut dyn Write, latest: &str, current: &str) {
    if let Some(line) = notice_line(latest, current) {
        let _ = writeln!(err, "{line}");
    }
}

/// Background CLI auto-update: on unless `CHROME_USE_AUTO_UPGRADE` says off.
fn auto_upgrade_enabled() -> bool {
    auto_upgrade_enabled_by(std::env::var("CHROME_USE_AUTO_UPGRADE").ok().as_deref())
}

fn auto_upgrade_enabled_by(value: Option<&str>) -> bool {
    !matches!(
        value.map(str::trim),
        Some("0") | Some("false") | Some("no") | Some("off")
    )
}

/// Whether this install can replace itself, and if not, why. Reported to the
/// extension, which offers the update button only for `Ready`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SelfUpgrade {
    Ready,
    /// `CHROME_USE_AUTO_UPGRADE=0`: no automatic upgrade, a click still works.
    Disabled,
    /// Installed somewhere we cannot write (a package manager's path), or a
    /// cargo build rather than an install.
    Unwritable,
    /// install.sh does not run on Windows; install.ps1 is a manual step.
    Windows,
}

impl SelfUpgrade {
    pub fn as_str(self) -> &'static str {
        match self {
            SelfUpgrade::Ready => "ready",
            SelfUpgrade::Disabled => "disabled",
            SelfUpgrade::Unwritable => "unwritable",
            SelfUpgrade::Windows => "windows",
        }
    }
}

pub fn self_upgrade() -> SelfUpgrade {
    if cfg!(windows) {
        SelfUpgrade::Windows
    } else if !binary_dir_writable() || current_bin_dir().is_some_and(|d| in_cargo_target(&d)) {
        SelfUpgrade::Unwritable
    } else if !auto_upgrade_enabled() {
        SelfUpgrade::Disabled
    } else {
        SelfUpgrade::Ready
    }
}

/// What to run by hand when this install cannot replace itself.
pub fn manual_upgrade_command() -> String {
    if cfg!(windows) {
        "irm https://raw.githubusercontent.com/leeguooooo/chrome-use/main/install.ps1 | iex"
            .to_string()
    } else {
        format!("curl -fsSL {INSTALL_URL} | sh")
    }
}

/// Start a background in-place upgrade. `Ok(false)`: one started in the last
/// ten minutes and is probably still running. Never blocks on the install.
///
/// On macOS the upgrade runs as a launchd job, not as our child: started from
/// the native-messaging host, a child would inherit Chrome's quarantine and
/// so would the binary it writes. Elsewhere it is a detached child.
pub fn start_background_upgrade() -> Result<bool, String> {
    match self_upgrade() {
        SelfUpgrade::Ready | SelfUpgrade::Disabled => {}
        SelfUpgrade::Unwritable => {
            return Err(format!(
                "{NAME} is installed in a folder it cannot write to; run: {}",
                manual_upgrade_command()
            ))
        }
        SelfUpgrade::Windows => {
            return Err(format!(
                "automatic upgrade is not supported on Windows; in PowerShell run: {}",
                manual_upgrade_command()
            ))
        }
    }
    if recently_attempted_auto_upgrade() {
        return Ok(false);
    }
    touch_auto_upgrade_marker();
    let exe = std::env::current_exe().map_err(|e| format!("cannot locate {NAME}: {e}"))?;
    let args: Vec<OsString> = vec!["__auto-upgrade".into()];
    #[cfg(target_os = "macos")]
    if launchd_upgrade(&exe, &args) {
        return Ok(true);
    }
    // A child of Chrome's tree would write a quarantined binary, which
    // Gatekeeper then refuses to run: better no upgrade than a broken one.
    if cfg!(target_os = "macos") && std::env::var_os(UNDER_CHROME_ENV).is_some() {
        return Err(format!(
            "could not start the upgrade outside Chrome; run: {}",
            manual_upgrade_command()
        ));
    }
    detached(&exe, &args).map(|()| true)
}

/// Set by the native-messaging host for itself and its children, which all
/// carry Chrome's process quarantine. launchd jobs do not inherit it.
pub const UNDER_CHROME_ENV: &str = "CHROME_USE_UNDER_CHROME";

#[cfg(target_os = "macos")]
fn launchd_upgrade(exe: &Path, args: &[OsString]) -> bool {
    let label = "com.chrome-use.auto-upgrade";
    let domain = crate::launchd::gui_domain();
    let target = format!("{domain}/{label}");
    let Ok(plist) = crate::launchd::plist(label, &crate::app_bundle::resolved(exe), args) else {
        return false;
    };
    let dir = crate::connection::config_home();
    let path = dir.join(format!("auto-upgrade.{}.plist", uuid::Uuid::new_v4()));
    // The plist carries our environment: readable by us only.
    let written = std::fs::create_dir_all(&dir).is_ok() && {
        use std::os::unix::fs::OpenOptionsExt;
        std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&path)
            .and_then(|mut f| f.write_all(plist.as_bytes()))
            .is_ok()
    };
    written && crate::launchd::bootstrap(&domain, &target, &path)
}

fn detached(exe: &Path, args: &[OsString]) -> Result<(), String> {
    let mut cmd = Command::new(exe);
    cmd.args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        unsafe {
            cmd.pre_exec(|| {
                libc::setsid();
                Ok(())
            });
        }
    }
    cmd.spawn()
        .map(|_| ())
        .map_err(|e| format!("could not start the background upgrade: {e}"))
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
    let Some(dir) = current_bin_dir() else {
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

/// Whether `dir` is inside a cargo build directory: cargo keeps
/// `.rustc_info.json` in its target directory, and `CACHEDIR.TAG` when it
/// created the directory itself. A development or test build is not an
/// install: replacing it with the release would overwrite the build.
fn in_cargo_target(dir: &Path) -> bool {
    dir.ancestors()
        .any(|d| d.join(".rustc_info.json").is_file() || d.join("CACHEDIR.TAG").is_file())
}

fn current_bin_dir() -> Option<PathBuf> {
    crate::app_bundle::install_dir(&std::env::current_exe().ok()?)
}

/// How the last background upgrade ended, so a failure is reported (by the
/// extension and `doctor`) instead of being retried silently forever.
#[derive(Debug, Clone, PartialEq, serde::Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AutoUpgradeOutcome {
    /// Milliseconds since the epoch.
    pub at: u64,
    pub ok: bool,
    pub from: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub to: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

fn auto_upgrade_outcome_path() -> PathBuf {
    crate::connection::config_home().join("auto-upgrade-last.json")
}

pub fn last_auto_upgrade() -> Option<AutoUpgradeOutcome> {
    serde_json::from_str(&std::fs::read_to_string(auto_upgrade_outcome_path()).ok()?).ok()
}

/// Judge an install by the version that ended up on disk, not by install.sh's
/// exit status: its closing self-check can fail (no extension yet, Chrome
/// closed) after the new binary is already in place.
fn auto_upgrade_outcome(
    from: &str,
    installed: Option<String>,
    latest: Option<&str>,
    installer_error: Option<String>,
    at: u64,
) -> AutoUpgradeOutcome {
    let ok = match (&installed, latest) {
        (Some(now), Some(latest)) => !is_newer(latest, now),
        // No release known yet: trust a clean install, or any step forward.
        (Some(now), None) => installer_error.is_none() || is_newer(now, from),
        (None, _) => false,
    };
    let error = (!ok).then(|| {
        installer_error
            .filter(|e| !e.is_empty())
            .unwrap_or_else(|| match &installed {
                Some(now) => format!("still {now} after the install"),
                None => "the installed binary did not run".to_string(),
            })
    });
    AutoUpgradeOutcome {
        at,
        ok,
        from: from.to_string(),
        to: installed,
        error,
    }
}

/// Quiet, non-interactive in-place upgrade for the detached `__auto-upgrade`
/// job started by [`start_background_upgrade`]. Runs install.sh into the
/// running binary's directory, records how it ended, and refreshes the skill
/// copies install.sh does not own (plugin, git checkouts, copied folders).
pub fn run_auto_upgrade() {
    #[cfg(not(windows))]
    {
        let exe = std::env::current_exe()
            .ok()
            .map(|p| p.canonicalize().unwrap_or(p));
        // Read before install.sh replaces the binary under us.
        let skill_scan = SkillScan::from_env();
        let skills = detect_skills(&skill_scan);
        let mut cmd = Command::new("sh");
        cmd.arg("-c")
            .arg(format!("curl -fsSL {INSTALL_URL} | sh"))
            .env("AGENT_BROWSER_NO_SETUP", "1")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped());
        if let Some(ref dir) = current_bin_dir() {
            cmd.env("AGENT_BROWSER_BIN_DIR", dir);
        }
        let installer_error = match cmd.output() {
            Ok(out) if out.status.success() => None,
            Ok(out) => String::from_utf8_lossy(&out.stderr)
                .lines()
                .rev()
                .find(|l| !l.trim().is_empty())
                .map(|l| l.trim().chars().take(300).collect()),
            Err(e) => Some(format!("could not run install.sh: {e}")),
        };
        let latest = cached_latest_version();
        let outcome = auto_upgrade_outcome(
            CURRENT_VERSION,
            installed_version(exe.as_deref()),
            latest.as_deref(),
            installer_error,
            now_secs() * 1000,
        );
        if let Ok(body) = serde_json::to_string(&outcome) {
            let _ = std::fs::write(auto_upgrade_outcome_path(), body);
        }
        if outcome.ok {
            refresh_git_skills(&skills);
            let installer = if skill_opt_out() {
                InstallerRefresh::Skip
            } else {
                InstallerRefresh::Ran
            };
            refresh_other_skills(&skills, &skill_scan, installer);
        }
    }
}

/// What the native-messaging host tells the extension about this CLI, in its
/// `pong`: enough for the extension to say "out of date", offer the update
/// button only when an in-place upgrade can work, and notice when a newer
/// binary has landed on disk while this host process is still the old one.
pub fn host_report(on_disk: Option<String>) -> serde_json::Value {
    serde_json::json!({
        "version": CURRENT_VERSION,
        "latest": cached_latest_version(),
        "onDisk": on_disk,
        "autoUpgrade": auto_upgrade_enabled(),
        "selfUpgrade": self_upgrade().as_str(),
        "lastAutoUpgrade": last_auto_upgrade(),
    })
}

/// The version of the binary now at the running executable's path, which a
/// background upgrade may have replaced. Spawns it, so callers cache it.
pub fn on_disk_version() -> Option<String> {
    let exe = std::env::current_exe().ok()?;
    installed_version(Some(&exe))
}

// ---------------------------------------------------------------------------
// Installed skills
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
enum Channel {
    /// Claude Code plugin from the family marketplace.
    ClaudePlugin,
    /// A git checkout (e.g. install-use-family.sh), linked into a skills dir.
    Git,
    /// A folder this CLI's own installer writes (`chrome-use skills update`).
    Installer,
    /// A copied folder the CLI does not own (e.g. `npx skills add`).
    Copied,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
struct SkillInstall {
    channel: Channel,
    path: String,
    /// The command that refreshes this copy.
    update: String,
}

/// Where to look. Kept as data so tests can point everything at a temp dir.
struct SkillScan {
    /// `$CLAUDE_CONFIG_DIR` or `~/.claude`.
    claude_dir: PathBuf,
    /// Skills directories to look in (the convention's three, plus every
    /// destination the installer writes).
    skill_dirs: Vec<PathBuf>,
    /// Destinations `chrome-use skill install` writes.
    installer_dirs: Vec<PathBuf>,
}

impl SkillScan {
    fn from_env() -> Self {
        let home = dirs::home_dir().unwrap_or_else(std::env::temp_dir);
        let claude_dir = std::env::var_os("CLAUDE_CONFIG_DIR")
            .filter(|s| !s.is_empty())
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join(".claude"));
        let installer_dirs = crate::skills::install::global_dirs().unwrap_or_default();
        let mut skill_dirs = vec![
            home.join(".agents/skills"),
            claude_dir.join("skills"),
            home.join(".codex/skills"),
        ];
        for dir in &installer_dirs {
            if !skill_dirs.contains(dir) {
                skill_dirs.push(dir.clone());
            }
        }
        SkillScan {
            claude_dir,
            skill_dirs,
            installer_dirs,
        }
    }
}

/// The plugin key (`chrome-use@<marketplace>`) and its install path, if Claude
/// Code has this plugin installed. Handles both the v2 layout
/// (`{"plugins": {"id": [{"installPath": ..}]}}`) and a flat map.
fn claude_plugin(claude_dir: &Path) -> Option<(String, String)> {
    let file = claude_dir.join("plugins/installed_plugins.json");
    let json: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&file).ok()?).ok()?;
    let map = json
        .get("plugins")
        .and_then(|v| v.as_object())
        .or_else(|| json.as_object())?;
    let prefix = format!("{NAME}@");
    let (key, entry) = map.iter().find(|(k, _)| k.starts_with(&prefix))?;
    let install_path = entry
        .as_array()
        .and_then(|a| a.first())
        .or(Some(entry))
        .and_then(|e| e.get("installPath"))
        .and_then(|p| p.as_str())
        .map(str::to_string)
        .unwrap_or_else(|| file.display().to_string());
    Some((key.clone(), install_path))
}

/// The git work tree containing `dir`, but only if it is a checkout of this
/// project (its origin mentions `chrome-use`). A skill folder that merely sits
/// inside some other repository, such as a dotfiles repo tracking `~/.claude`,
/// is not ours to pull.
fn git_checkout_root(dir: &Path) -> Option<PathBuf> {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["rev-parse", "--show-toplevel"])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()
        .filter(|o| o.status.success())?;
    let root = PathBuf::from(String::from_utf8_lossy(&out.stdout).trim());
    let origin = Command::new("git")
        .arg("-C")
        .arg(&root)
        .args(["remote", "get-url", "origin"])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).to_lowercase())
        .unwrap_or_default();
    origin.contains(NAME).then_some(root)
}

/// Find every installed copy of the skill, deduplicated by where it really
/// lives (a symlink and its target are one install).
fn detect_skills(scan: &SkillScan) -> Vec<SkillInstall> {
    let mut found = Vec::new();
    if let Some((key, path)) = claude_plugin(&scan.claude_dir) {
        found.push(SkillInstall {
            channel: Channel::ClaudePlugin,
            path,
            update: format!("claude plugin update {key}"),
        });
    }
    let mut seen: Vec<PathBuf> = Vec::new();
    for base in &scan.skill_dirs {
        let entry = base.join(NAME);
        let Ok(real) = entry.canonicalize() else {
            continue;
        };
        if !real.is_dir() {
            continue;
        }
        if let Some(root) = git_checkout_root(&real) {
            if !seen.contains(&root) {
                seen.push(root.clone());
                found.push(SkillInstall {
                    channel: Channel::Git,
                    update: format!("git -C {} pull --ff-only", root.display()),
                    path: root.display().to_string(),
                });
            }
            continue;
        }
        if !real.join("SKILL.md").is_file() || seen.contains(&real) {
            continue;
        }
        seen.push(real.clone());
        let (channel, update) = if scan.installer_dirs.contains(base) {
            (Channel::Installer, format!("{NAME} skills update"))
        } else {
            (Channel::Copied, format!("npx skills update {NAME}"))
        };
        found.push(SkillInstall {
            channel,
            path: real.display().to_string(),
            update,
        });
    }
    found
}

fn on_path(program: &str) -> bool {
    let Some(path) = std::env::var_os("PATH") else {
        return false;
    };
    std::env::split_paths(&path).any(|dir| {
        let candidate = dir.join(program);
        candidate.is_file() || (cfg!(windows) && dir.join(format!("{program}.exe")).is_file())
    })
}

/// Refresh the git checkouts. Runs before install.sh: the installer rewrites
/// SKILL.md in every destination, including a linked checkout, and pulling
/// first means it writes the file the checkout already holds.
fn refresh_git_skills(skills: &[SkillInstall]) {
    for skill in skills.iter().filter(|s| s.channel == Channel::Git) {
        let out = Command::new("git")
            .args(["-C", &skill.path, "pull", "--ff-only", "-q"])
            .stdin(Stdio::null())
            .output();
        match out {
            Ok(o) if o.status.success() => {
                println!(
                    "{} skill (git) {}: pulled",
                    color::success_indicator(),
                    skill.path
                )
            }
            Ok(o) => eprintln!(
                "{} skill (git) {}: not updated, `git pull --ff-only` failed: {}",
                color::warning_indicator(),
                skill.path,
                String::from_utf8_lossy(&o.stderr).trim()
            ),
            Err(e) => eprintln!(
                "{} skill (git) {}: not updated, could not run git: {e}",
                color::warning_indicator(),
                skill.path
            ),
        }
    }
}

/// Who refreshes the installer-managed skill folders.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum InstallerRefresh {
    /// install.sh just ran and already rewrote them with the new SKILL.md.
    Ran,
    /// No reinstall happened: write this binary's bundled SKILL.md here.
    InProcess,
    /// `AGENT_BROWSER_NO_SKILL` is set: leave them alone. (Writing in-process
    /// after a reinstall would also put the *old* binary's SKILL.md back.)
    Skip,
}

fn skill_opt_out() -> bool {
    std::env::var_os("AGENT_BROWSER_NO_SKILL").is_some()
}

/// Refresh everything except git checkouts (see [`refresh_git_skills`]).
fn refresh_other_skills(skills: &[SkillInstall], scan: &SkillScan, installer: InstallerRefresh) {
    let installer_dirs: Vec<PathBuf> = scan
        .installer_dirs
        .iter()
        .filter(|base| {
            let real = base.join(NAME).canonicalize().ok();
            skills.iter().any(|s| {
                s.channel == Channel::Installer && real.as_deref() == Some(Path::new(&s.path))
            })
        })
        .cloned()
        .collect();
    for skill in skills {
        match skill.channel {
            Channel::Git => {}
            Channel::ClaudePlugin => {
                if on_path("claude") {
                    let ok = Command::new("claude")
                        .args(["plugin", "update", PLUGIN_ID])
                        .stdin(Stdio::null())
                        .status()
                        .map(|s| s.success())
                        .unwrap_or(false);
                    if ok {
                        println!(
                            "{} skill (Claude Code plugin): updated; restart Claude Code or run /reload-plugins",
                            color::success_indicator()
                        );
                    } else {
                        eprintln!(
                            "{} skill (Claude Code plugin): `{}` failed",
                            color::warning_indicator(),
                            skill.update
                        );
                    }
                } else {
                    println!("skill (Claude Code plugin): run `{}`", skill.update);
                }
            }
            Channel::Installer if installer == InstallerRefresh::Ran => {
                println!(
                    "{} skill {}: refreshed by the installer",
                    color::success_indicator(),
                    skill.path
                );
            }
            Channel::Installer if installer == InstallerRefresh::Skip => {
                println!(
                    "skill {}: not refreshed (AGENT_BROWSER_NO_SKILL is set)",
                    skill.path
                );
            }
            Channel::Installer => {} // handled below in one pass
            Channel::Copied => {
                println!(
                    "skill {}: copied folder, run `{}`",
                    skill.path, skill.update
                );
            }
        }
    }
    if installer == InstallerRefresh::InProcess && !installer_dirs.is_empty() {
        let (installed, errors) = crate::skills::install::install_all(&installer_dirs);
        for path in installed {
            println!(
                "{} skill {}: refreshed",
                color::success_indicator(),
                path.display()
            );
        }
        for error in errors {
            eprintln!(
                "{} skill refresh failed: {error}",
                color::warning_indicator()
            );
        }
    }
}

// ---------------------------------------------------------------------------
// `upgrade --check` / `--json`
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize)]
struct CheckReport {
    name: &'static str,
    current: String,
    latest: Option<String>,
    update_available: bool,
    skills: Vec<SkillInstall>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

fn build_report(
    current: &str,
    latest: Result<String, String>,
    skills: Vec<SkillInstall>,
) -> CheckReport {
    match latest {
        Ok(latest) => CheckReport {
            name: NAME,
            current: current.to_string(),
            update_available: is_newer(&latest, current),
            latest: Some(latest),
            skills,
            error: None,
        },
        Err(error) => CheckReport {
            name: NAME,
            current: current.to_string(),
            latest: None,
            update_available: false,
            skills,
            error: Some(error),
        },
    }
}

fn check_line(report: &CheckReport) -> String {
    match &report.latest {
        Some(latest) if report.update_available => {
            format!("{NAME} {} -> {latest}", report.current)
        }
        Some(_) => format!("{NAME} {} is up to date", report.current),
        None => format!(
            "{NAME} {}: could not check the latest release: {}",
            report.current,
            report.error.as_deref().unwrap_or("unknown error")
        ),
    }
}

fn run_check(json: bool) -> ! {
    let latest = fetch_latest_version(EXPLICIT_CHECK_TIMEOUT_SECS);
    if let Ok(ref v) = latest {
        write_update_cache(now_secs(), v);
    }
    let report = build_report(
        CURRENT_VERSION,
        latest,
        detect_skills(&SkillScan::from_env()),
    );
    let failed = report.error.is_some();
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&report).unwrap_or_default()
        );
    } else {
        if failed {
            eprintln!("{}", check_line(&report));
        } else {
            println!("{}", check_line(&report));
        }
        for skill in &report.skills {
            println!(
                "  skill ({}) {}  refresh: {}",
                channel_label(skill.channel),
                skill.path,
                skill.update
            );
        }
    }
    exit(if failed { 2 } else { 0 })
}

fn channel_label(channel: Channel) -> &'static str {
    match channel {
        Channel::ClaudePlugin => "claude-plugin",
        Channel::Git => "git",
        Channel::Installer => "installer",
        Channel::Copied => "copied",
    }
}

// ---------------------------------------------------------------------------
// `upgrade`
// ---------------------------------------------------------------------------

/// Version reported by the binary at `exe` (after install.sh replaced it).
///
/// `exe` must be resolved before the installer runs: on Linux,
/// `current_exe()` afterwards points at the replaced inode (`... (deleted)`).
fn installed_version(exe: Option<&Path>) -> Option<String> {
    let out = Command::new(exe?)
        .arg("--version")
        .env("CHROME_USE_NO_UPDATE_CHECK", "1")
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .next()
        .and_then(|l| l.split_whitespace().nth(1))
        .map(str::to_string)
}

fn in_process_or_skip() -> InstallerRefresh {
    if skill_opt_out() {
        InstallerRefresh::Skip
    } else {
        InstallerRefresh::InProcess
    }
}

/// `chrome-use upgrade [--check] [--json]`.
///
/// The stealth fork ships as a prebuilt binary attached to a GitHub Release —
/// NOT via the npm registry. Earlier this command (inherited from upstream)
/// ran `npm/pnpm install -g chrome-use@latest`, which installed the
/// UNRELATED upstream `chrome-use` package and clobbered the user's setup.
/// Now `upgrade` re-runs install.sh into the same directory as the current
/// binary, so it always tracks the freshest GitHub Release, then refreshes
/// every installed copy of the skill.
pub fn run_upgrade(args: &[String]) {
    let json = args.iter().any(|a| a == "--json");
    let check = args.iter().any(|a| a == "--check");
    if json || check {
        run_check(json);
    }
    let force = args.iter().any(|a| a == "--force" || a == "-f");

    let scan = SkillScan::from_env();
    let skills = detect_skills(&scan);
    let latest = fetch_latest_version(EXPLICIT_CHECK_TIMEOUT_SECS);

    match &latest {
        Ok(latest) if !is_newer(latest, CURRENT_VERSION) => {
            write_update_cache(now_secs(), latest);
            println!("{NAME} {CURRENT_VERSION} is up to date");
            refresh_git_skills(&skills);
            refresh_other_skills(&skills, &scan, in_process_or_skip());
            if force {
                take_over_panel();
            }
            return;
        }
        Ok(latest) => println!(
            "{}",
            color::cyan(&format!(
                "Upgrading {NAME} {CURRENT_VERSION} -> {latest} from the latest GitHub Release..."
            ))
        ),
        // The API can be rate-limited while the release download still works
        // (install.sh does not use the API). Keep the old behaviour: reinstall.
        Err(e) => eprintln!(
            "{} could not check the latest release ({e}); reinstalling from the latest GitHub Release anyway",
            color::warning_indicator()
        ),
    }

    #[cfg(windows)]
    {
        eprintln!(
            "{} Automatic upgrade isn't supported on Windows.",
            color::warning_indicator()
        );
        eprintln!("  In PowerShell:");
        eprintln!(
            "    irm https://raw.githubusercontent.com/leeguooooo/chrome-use/main/install.ps1 | iex"
        );
        refresh_git_skills(&skills);
        refresh_other_skills(&skills, &scan, in_process_or_skip());
        exit(2);
    }

    #[cfg(not(windows))]
    {
        refresh_git_skills(&skills);

        // Resolve the binary's path now; after install.sh replaces it, the
        // running process can no longer name it (see installed_version).
        let exe = std::env::current_exe()
            .ok()
            .map(|p| p.canonicalize().unwrap_or(p));
        let install_cmd = format!("curl -fsSL {} | sh", INSTALL_URL);
        println!("Running: {}", install_cmd);
        let mut cmd = Command::new("sh");
        cmd.arg("-c").arg(&install_cmd);
        // Install into the SAME directory as the running binary (in-place
        // upgrade), so we don't create a second copy elsewhere on PATH.
        if let Some(ref dir) = current_bin_dir() {
            cmd.env("AGENT_BROWSER_BIN_DIR", dir);
        }
        // --force asks nothing: no guided extension setup on the terminal.
        if force {
            cmd.env("AGENT_BROWSER_NO_SETUP", "1");
        }
        let ok = cmd.status().map(|s| s.success()).unwrap_or(false);
        if !ok {
            eprintln!(
                "{} Upgrade failed. Install manually:",
                color::error_indicator()
            );
            eprintln!("  curl -fsSL {} | sh", INSTALL_URL);
            exit(2);
        }

        let now = installed_version(exe.as_deref()).unwrap_or_else(|| "unknown".to_string());
        if let Ok(latest) = &latest {
            write_update_cache(now_secs(), latest);
        }
        if now == CURRENT_VERSION {
            println!("{} {NAME} {now} (unchanged)", color::success_indicator());
        } else {
            println!(
                "{} {NAME} {CURRENT_VERSION} -> {now}",
                color::success_indicator()
            );
        }
        let installer = if skill_opt_out() {
            InstallerRefresh::Skip
        } else {
            InstallerRefresh::Ran
        };
        refresh_other_skills(&skills, &scan, installer);
        if force {
            take_over_panel();
        }
    }
}

/// `--force`: stop the side panel's running conversations and the panel
/// service of the build that ran them, so the panel's next request starts the
/// installed build instead of waiting for the old one to go idle.
fn take_over_panel() {
    match crate::agent_panel::force_stop_services() {
        Ok(lines) if lines.is_empty() => println!("No side panel service was running"),
        Ok(lines) => {
            for line in lines {
                println!("{line}");
            }
        }
        Err(error) => eprintln!(
            "{} could not stop the side panel service: {error}",
            color::warning_indicator()
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn env_of(vars: &[&str]) -> impl Fn(&str) -> Option<OsString> {
        let map: HashMap<String, OsString> = vars
            .iter()
            .map(|k| (k.to_string(), OsString::from("1")))
            .collect();
        move |k| map.get(k).cloned()
    }

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn version_comparison() {
        assert!(is_newer("1.5.144", "1.5.143"));
        assert!(is_newer("v1.6.0", "1.5.143"));
        assert!(is_newer("2.0.0", "1.99.99"));
        assert!(is_newer("1.5.10", "1.5.9"), "numeric, not lexical");
        assert!(!is_newer("1.5.143", "1.5.143"));
        assert!(!is_newer("1.5.142", "1.5.143"));
        assert!(!is_newer("1.5.143-rc.1", "1.5.143"));
        assert!(!is_newer("", "1.5.143"));
        assert!(!is_newer("garbage", "1.5.143"));
        assert!(is_newer("0.10.0", "0.9.0"));
        assert!(!is_newer("0.9.0", "0.10.0"));
        // semver pre-release precedence
        assert!(is_newer("1.5.143", "1.5.143-rc.1"), "release beats its rc");
        assert!(is_newer("1.5.143-rc.2", "1.5.143-rc.1"));
        assert!(is_newer("1.5.143-rc.10", "1.5.143-rc.9"), "numeric ids");
        assert!(is_newer("1.5.143-beta", "1.5.143-alpha"));
        assert!(is_newer("1.5.143-alpha.1", "1.5.143-alpha"));
        assert!(is_newer("1.5.143-rc", "1.5.143-1"), "alnum above numeric");
        assert!(is_newer("1.5.144-rc.1", "1.5.143"));
        assert!(
            !is_newer("1.5.143+build.5", "1.5.143"),
            "build metadata ignored"
        );
    }

    #[test]
    fn cache_write_is_atomic_and_leaves_no_temp_files() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("chrome-use").join("update-check.json");
        let cache = UpdateCache {
            checked_at: 7,
            latest: "1.2.3".into(),
        };
        write_cache_at(&path, &cache);
        write_cache_at(&path, &cache);
        assert_eq!(read_cache_at(&path), cache);
        let names: Vec<_> = std::fs::read_dir(path.parent().unwrap())
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(names, vec![std::ffi::OsString::from("update-check.json")]);
    }

    #[test]
    fn latest_release_parsing() {
        assert_eq!(
            parse_latest_release(br#"{"tag_name":"v1.5.144","prerelease":false}"#).unwrap(),
            "1.5.144"
        );
        assert!(parse_latest_release(br#"{"tag_name":"v2.0.0","prerelease":true}"#).is_err());
        let err = parse_latest_release(br#"{"message":"API rate limit exceeded"}"#).unwrap_err();
        assert!(err.contains("rate limit"), "{err}");
        assert!(parse_latest_release(b"<html>").is_err());
        assert!(parse_latest_release(br#"{"tag_name":"nightly"}"#).is_err());
    }

    #[test]
    fn check_is_throttled_to_once_a_day() {
        let now = 1_900_000_000;
        let cache = |checked_at| UpdateCache {
            checked_at,
            latest: "1.0.0".into(),
        };
        assert!(check_due(&UpdateCache::default(), now), "never checked");
        assert!(!check_due(&cache(now), now));
        assert!(!check_due(&cache(now - 3600), now));
        assert!(!check_due(&cache(now - 86_399), now));
        assert!(check_due(&cache(now - 86_400), now));
        assert!(check_due(&cache(now - 7 * 86_400), now));
        // A clock that went backwards does not cause a check on every call.
        assert!(!check_due(&cache(now + 100), now));
    }

    #[test]
    fn cache_round_trips_and_lives_under_xdg_cache_home() {
        let dir = tempfile::tempdir().unwrap();
        let path = cache_path_from(Some(dir.path().into()), None);
        assert_eq!(path, dir.path().join("chrome-use/update-check.json"));
        assert_eq!(read_cache_at(&path), UpdateCache::default());
        let written = UpdateCache {
            checked_at: 42,
            latest: "9.9.9".into(),
        };
        write_cache_at(&path, &written);
        assert_eq!(read_cache_at(&path), written);
        let raw: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(
            raw,
            serde_json::json!({"checked_at": 42, "latest": "9.9.9"})
        );

        let home = PathBuf::from("/home/someone");
        assert_eq!(
            cache_path_from(None, Some(home.clone())),
            home.join(".cache/chrome-use/update-check.json")
        );
        // A relative XDG_CACHE_HOME is invalid per the spec; fall back to ~/.cache.
        assert_eq!(
            cache_path_from(Some("rel".into()), Some(home.clone())),
            home.join(".cache/chrome-use/update-check.json")
        );
    }

    #[test]
    fn opt_out_env_vars_disable_the_check() {
        assert!(!update_check_disabled(env_of(&[])));
        for var in [
            "CI",
            "CHROME_USE_NO_UPDATE_CHECK",
            "USE_NO_UPDATE_CHECK",
            "AGENT_BROWSER_NO_UPDATE_CHECK",
            "AGENT_BROWSER_DAEMON",
            "CU_BWU_CHILD",
        ] {
            assert!(update_check_disabled(env_of(&[var])), "{var}");
        }
        assert!(!update_check_disabled(env_of(&[
            "CHROME_USE_NO_UPDATE_NOTICE"
        ])));
        assert!(update_notice_silenced(env_of(&[
            "CHROME_USE_NO_UPDATE_NOTICE"
        ])));
        assert!(update_notice_silenced(env_of(&["NO_UPDATE_NOTIFIER"])));
        assert!(!update_notice_silenced(env_of(&["CI"])));
    }

    #[test]
    fn upgrade_version_and_help_skip_the_check() {
        for skip in [
            &["upgrade"][..],
            &["upgrade", "--check"],
            &["--version"],
            &["-V"],
            &["--help"],
            &["open", "--help"],
            &["__update-check"],
            &["doctor"],
            &["mcp"],
        ] {
            assert!(skip_for_args(&args(skip)), "{skip:?}");
        }
        assert!(!skip_for_args(&args(&["open", "https://example.com"])));
        assert!(!skip_for_args(&args(&["snapshot", "--json"])));
    }

    /// On unless explicitly switched off: an opt-in kept almost everyone on
    /// whatever version they first installed.
    #[test]
    fn auto_upgrade_is_on_unless_switched_off() {
        for on in [None, Some("1"), Some("true"), Some(""), Some("yes")] {
            assert!(auto_upgrade_enabled_by(on), "{on:?}");
        }
        for off in ["0", "false", "no", "off", " off "] {
            assert!(!auto_upgrade_enabled_by(Some(off)), "{off:?}");
        }
    }

    /// A cargo build is never replaced by the release.
    #[test]
    fn a_cargo_build_is_not_an_install() {
        let tmp = tempfile::tempdir().unwrap();
        let target = tmp.path().join("target");
        std::fs::create_dir_all(target.join("debug")).unwrap();
        assert!(!in_cargo_target(&target.join("debug")));
        std::fs::write(target.join(".rustc_info.json"), "{}").unwrap();
        assert!(in_cargo_target(&target.join("debug")));
        assert!(!in_cargo_target(tmp.path()));
    }

    /// The outcome follows the version on disk: install.sh's closing
    /// self-check may fail after the new binary is already in place.
    #[test]
    fn auto_upgrade_outcome_is_judged_by_the_installed_version() {
        let landed = auto_upgrade_outcome(
            "1.5.200",
            Some("1.5.240".into()),
            Some("1.5.240"),
            Some("self-check failed".into()),
            7,
        );
        assert!(landed.ok);
        assert_eq!(landed.error, None);
        assert_eq!(landed.to.as_deref(), Some("1.5.240"));

        let stuck =
            auto_upgrade_outcome("1.5.200", Some("1.5.200".into()), Some("1.5.240"), None, 7);
        assert!(!stuck.ok);
        assert_eq!(
            stuck.error.as_deref(),
            Some("still 1.5.200 after the install")
        );

        let failed = auto_upgrade_outcome(
            "1.5.200",
            None,
            Some("1.5.240"),
            Some("curl: (6) Could not resolve host".into()),
            7,
        );
        assert!(!failed.ok);
        assert_eq!(
            failed.error.as_deref(),
            Some("curl: (6) Could not resolve host")
        );

        // No cached latest: a clean install or any step forward counts (the
        // button can run before the first check has landed).
        assert!(
            auto_upgrade_outcome("1.5.200", Some("1.5.201".into()), None, Some("x".into()), 7).ok
        );
        assert!(auto_upgrade_outcome("1.5.200", Some("1.5.200".into()), None, None, 7).ok);
        assert!(
            !auto_upgrade_outcome("1.5.200", Some("1.5.200".into()), None, Some("x".into()), 7).ok
        );
    }

    #[test]
    fn notice_is_one_exact_line_only_when_newer() {
        let mut err = Vec::new();
        write_notice(&mut err, "1.5.144", "1.5.143");
        assert_eq!(
            String::from_utf8(err).unwrap(),
            "chrome-use 1.5.144 is available (you have 1.5.143). Upgrade: chrome-use upgrade\n"
        );
        let mut err = Vec::new();
        write_notice(&mut err, "1.5.143", "1.5.143");
        write_notice(&mut err, "", "1.5.143");
        assert!(err.is_empty());
    }

    #[test]
    fn json_report_shape() {
        let skills = vec![SkillInstall {
            channel: Channel::ClaudePlugin,
            path: "/placeholder/path".into(),
            update: "claude plugin update chrome-use@leeguooooo-plugins".into(),
        }];
        let report = build_report("1.5.140", Ok("1.5.141".into()), skills);
        let v = serde_json::to_value(&report).unwrap();
        assert_eq!(
            v,
            serde_json::json!({
                "name": "chrome-use",
                "current": "1.5.140",
                "latest": "1.5.141",
                "update_available": true,
                "skills": [{
                    "channel": "claude-plugin",
                    "path": "/placeholder/path",
                    "update": "claude plugin update chrome-use@leeguooooo-plugins"
                }]
            })
        );
        assert_eq!(check_line(&report), "chrome-use 1.5.140 -> 1.5.141");

        let current = build_report("1.5.141", Ok("1.5.141".into()), vec![]);
        assert_eq!(
            serde_json::to_value(&current).unwrap()["update_available"],
            false
        );
        assert_eq!(check_line(&current), "chrome-use 1.5.141 is up to date");

        let failed = build_report("1.5.141", Err("offline".into()), vec![]);
        let v = serde_json::to_value(&failed).unwrap();
        assert_eq!(v["latest"], serde_json::Value::Null);
        assert_eq!(v["update_available"], false);
        assert_eq!(v["error"], "offline");
    }

    #[test]
    fn detects_plugin_git_installer_and_copied_skills() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        let claude = root.join(".claude");
        std::fs::create_dir_all(claude.join("plugins")).unwrap();
        std::fs::write(
            claude.join("plugins/installed_plugins.json"),
            r#"{"version":2,"plugins":{"chrome-use@leeguooooo-plugins":[{"installPath":"/placeholder/plugin"}],"other@x":[]}}"#,
        )
        .unwrap();

        // Installer-managed folder, plus a symlink to it (one install, not two).
        let agents = root.join(".agents/skills");
        std::fs::create_dir_all(agents.join("chrome-use")).unwrap();
        std::fs::write(agents.join("chrome-use/SKILL.md"), "x").unwrap();
        std::fs::create_dir_all(claude.join("skills")).unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(agents.join("chrome-use"), claude.join("skills/chrome-use"))
            .unwrap();

        // A copied folder somewhere the installer does not write.
        let codex = root.join(".codex/skills");
        std::fs::create_dir_all(codex.join("chrome-use")).unwrap();
        std::fs::write(codex.join("chrome-use/SKILL.md"), "x").unwrap();

        // A git checkout linked into another skills dir.
        let checkout = root.join("checkout");
        let git_ok = Command::new("git")
            .args(["init", "-q"])
            .arg(&checkout)
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        let other = root.join("other/skills");
        std::fs::create_dir_all(&other).unwrap();
        if git_ok {
            Command::new("git")
                .arg("-C")
                .arg(&checkout)
                .args([
                    "remote",
                    "add",
                    "origin",
                    "https://example.com/owner/chrome-use.git",
                ])
                .status()
                .unwrap();
            std::fs::create_dir_all(checkout.join("skills/chrome-use")).unwrap();
            std::fs::write(checkout.join("skills/chrome-use/SKILL.md"), "x").unwrap();
            #[cfg(unix)]
            std::os::unix::fs::symlink(
                checkout.join("skills/chrome-use"),
                other.join("chrome-use"),
            )
            .unwrap();
        }

        let scan = SkillScan {
            claude_dir: claude.clone(),
            skill_dirs: vec![agents.clone(), claude.join("skills"), codex.clone(), other],
            installer_dirs: vec![agents.clone(), claude.join("skills")],
        };
        let skills = detect_skills(&scan);
        let by_channel = |c: Channel| skills.iter().filter(|s| s.channel == c).collect::<Vec<_>>();

        let plugin = by_channel(Channel::ClaudePlugin);
        assert_eq!(plugin.len(), 1);
        assert_eq!(plugin[0].path, "/placeholder/plugin");
        assert_eq!(
            plugin[0].update,
            "claude plugin update chrome-use@leeguooooo-plugins"
        );

        let installer = by_channel(Channel::Installer);
        assert_eq!(installer.len(), 1, "{skills:?}");
        assert_eq!(installer[0].update, "chrome-use skills update");

        let copied = by_channel(Channel::Copied);
        assert_eq!(copied.len(), 1);
        assert_eq!(copied[0].update, "npx skills update chrome-use");

        if git_ok && cfg!(unix) {
            let git = by_channel(Channel::Git);
            assert_eq!(git.len(), 1, "{skills:?}");
            assert_eq!(git[0].path, checkout.display().to_string());
            assert!(git[0].update.ends_with("pull --ff-only"));
        }
    }

    #[test]
    fn a_skill_inside_an_unrelated_repo_is_not_pulled() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        if !Command::new("git")
            .args(["init", "-q"])
            .arg(&root)
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
        {
            return; // no git on this machine
        }
        let skills = root.join("skills");
        std::fs::create_dir_all(skills.join("chrome-use")).unwrap();
        std::fs::write(skills.join("chrome-use/SKILL.md"), "x").unwrap();
        let scan = SkillScan {
            claude_dir: root.join("none"),
            skill_dirs: vec![skills.clone()],
            installer_dirs: vec![],
        };
        let found = detect_skills(&scan);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].channel, Channel::Copied);
    }
}
