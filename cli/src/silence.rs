//! Silence Chrome's `chrome.debugger` "<ext> started debugging this browser"
//! infobar.
//!
//! That banner is shown whenever an extension (here: ab-connect) calls
//! `chrome.debugger.attach`. It cannot be dismissed at runtime — the only clean
//! way to suppress it is to launch Chrome with `--silent-debugger-extension-api`,
//! which must be present at *startup* (it can't be injected into a running
//! Chrome). So to silence the banner for the relay path we gracefully quit the
//! user's real Chrome and cold-relaunch the SAME profile with the flag, adding
//! `--restore-last-session` so their tabs come back. Logins survive because the
//! profile (user-data-dir) is unchanged.
//!
//! Safety rules (the graceful quit closes the user's whole browser):
//!   - opt-in + confirmed — never quit the browser silently;
//!   - if ANY running instance already has the flag → no-op (`AlreadySilent`);
//!   - if MORE THAN ONE browser instance is running → refuse (`Ambiguous`),
//!     because the macOS `quit` closes every instance and we won't gamble with
//!     the user's other windows;
//!   - on quit, wait generously and NEVER force-kill (a force kill loses tabs).
//!
//! Set `AGENT_BROWSER_SILENCE_DRYRUN=1` to print the decision and skip the
//! actual quit/relaunch. See docs/debugger-banner-silence.html for the design.

use std::io::{self, IsTerminal, Write};
use std::path::PathBuf;
use std::process::Command;
use std::time::{Duration, Instant};

use crate::color;

/// The startup flag that suppresses the `chrome.debugger` infobar. Process-wide
/// (silences the banner for any extension using `chrome.debugger`, not just
/// ab-connect) and grants no new permissions — it only hides the notification.
const SILENCE_FLAG: &str = "--silent-debugger-extension-api";
/// Forces Chrome to restore the previous session on the cold relaunch, so the
/// user's tabs come back regardless of their "On startup" setting.
const RESTORE_FLAG: &str = "--restore-last-session";
/// Graceful-quit budget. Generous: a heavy session (dozens of tabs) can take a
/// while to flush to disk, and quitting too eagerly would leave Chrome down.
const QUIT_TIMEOUT: Duration = Duration::from_secs(45);

/// How to treat the banner during `extension connect`.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum SilenceMode {
    /// Default: restart only after an interactive confirm; otherwise leave the
    /// banner (and print a one-line hint) so scripts are never disrupted.
    Auto,
    /// `--silent`: restart without prompting.
    Force,
    /// `--keep-banner`: never touch the browser.
    Off,
}

/// Result of an attempt to silence the banner.
pub enum SilenceOutcome {
    /// A running instance already has the flag — nothing to do.
    AlreadySilent,
    /// No Chromium-family browser is running.
    NotRunning,
    /// More than one browser instance is running — refused (quitting would close
    /// all of them). Carries the instance count.
    Ambiguous(usize),
    /// User declined, or non-interactive without `--silent`.
    Declined,
    /// Chrome was quit and cold-relaunched with the flag.
    Restarted,
    /// Quit/relaunch failed (e.g. a page blocked the graceful quit).
    Failed(String),
}

/// The running browser, summarised across all of its main (non-helper) processes.
struct RunningChrome {
    /// macOS `.app` display name (e.g. "Google Chrome"), used with `open -a`.
    #[allow(dead_code)]
    app_name: String,
    /// Full path to the running executable (used to relaunch on Linux/Windows).
    #[allow(dead_code)]
    exe: PathBuf,
    /// Main browser process id of the first instance (Linux/Windows quit).
    #[allow(dead_code)]
    pid: Option<u32>,
    /// Number of distinct main browser processes (instances) running.
    instances: usize,
    /// Whether ANY instance's command line contains [`SILENCE_FLAG`].
    has_flag: bool,
}

/// Entry point. Detect the user's real Chrome and, per `mode`, optionally
/// restart it with the silence flag.
pub fn ensure_banner_silenced(mode: SilenceMode) -> SilenceOutcome {
    if mode == SilenceMode::Off {
        return SilenceOutcome::Declined;
    }

    let rc = match detect() {
        Some(rc) => rc,
        None => return SilenceOutcome::NotRunning,
    };
    if rc.has_flag {
        return SilenceOutcome::AlreadySilent;
    }
    if rc.instances > 1 {
        // The macOS `quit` (and a brand-wide taskkill/pkill) would close EVERY
        // instance. Too risky to do on a hunch — let the user sort it out.
        return SilenceOutcome::Ambiguous(rc.instances);
    }

    let proceed = match mode {
        SilenceMode::Force => true,
        SilenceMode::Auto => {
            if interactive() {
                confirm_restart()
            } else {
                eprintln!(
                    "{} Chrome will show a \"started debugging this browser\" banner. Run \
                     `chrome-use extension connect --silent` to remove it (one-time Chrome restart; \
                     tabs + logins are preserved).",
                    color::warning_indicator()
                );
                return SilenceOutcome::Declined;
            }
        }
        SilenceMode::Off => false,
    };
    if !proceed {
        return SilenceOutcome::Declined;
    }

    if std::env::var("AGENT_BROWSER_SILENCE_DRYRUN").is_ok() {
        eprintln!(
            "{} [dry-run] would restart {:?} (app={:?}, instances={}, has_flag={}) with {SILENCE_FLAG} {RESTORE_FLAG}",
            color::warning_indicator(),
            rc.exe,
            rc.app_name,
            rc.instances,
            rc.has_flag
        );
        return SilenceOutcome::Declined;
    }

    eprintln!(
        "{} restarting Chrome to remove the debugging banner (your tabs are restored)…",
        color::success_indicator()
    );
    if let Err(e) = graceful_quit(&rc) {
        return SilenceOutcome::Failed(e);
    }
    if let Err(e) = relaunch(&rc) {
        return SilenceOutcome::Failed(e);
    }
    SilenceOutcome::Restarted
}

/// Both stdin and stderr are TTYs, so a confirm prompt is safe.
fn interactive() -> bool {
    io::stdin().is_terminal() && io::stderr().is_terminal()
}

/// Prompt on stderr; empty / y / yes ⇒ true.
fn confirm_restart() -> bool {
    eprintln!(
        "\n  Chrome shows a \"…started debugging this browser\" banner while chrome-use is attached."
    );
    eprintln!(
        "  To remove it, Chrome needs a one-time restart — your tabs and logins are restored \
         automatically (~2s)."
    );
    eprint!("  Restart Chrome now? [Y/n] ");
    let _ = io::stderr().flush();

    let mut input = String::new();
    if io::stdin().read_line(&mut input).is_err() {
        return false;
    }
    let a = input.trim().to_lowercase();
    a.is_empty() || a == "y" || a == "yes"
}

/// Poll until no Chromium-family browser is running, or time out. NEVER force
/// kills — a force kill would lose the user's session.
fn wait_until_gone(timeout: Duration) -> Result<(), String> {
    let deadline = Instant::now() + timeout;
    loop {
        if detect().is_none() {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(
                "Chrome did not fully quit (a page may be asking \"Leave site?\"). Not \
                 force-quitting, to avoid losing your tabs — close that page and retry."
                    .to_string(),
            );
        }
        std::thread::sleep(Duration::from_millis(400));
    }
}

// ───────────────────────── macOS ─────────────────────────

#[cfg(target_os = "macos")]
fn detect() -> Option<RunningChrome> {
    // (cmdline substring, .app display name) for Chromium-family browsers.
    const APPS: &[(&str, &str)] = &[
        (
            "Google Chrome.app/Contents/MacOS/Google Chrome",
            "Google Chrome",
        ),
        (
            "Google Chrome Canary.app/Contents/MacOS/Google Chrome Canary",
            "Google Chrome Canary",
        ),
        ("Chromium.app/Contents/MacOS/Chromium", "Chromium"),
        (
            "Brave Browser.app/Contents/MacOS/Brave Browser",
            "Brave Browser",
        ),
    ];
    let out = Command::new("ps")
        .args(["-axww", "-o", "command="])
        .output()
        .ok()?;
    scan_ps_macos(&String::from_utf8_lossy(&out.stdout), APPS)
}

/// Pure parser for `ps -axww -o command=` output (extracted for testing). Skips
/// helper processes (`--type=`), counts main browser instances, and reports
/// `has_flag = ANY instance carries [`SILENCE_FLAG`]` — the defensive check that
/// stops us restarting a browser that is already silenced.
#[cfg(target_os = "macos")]
fn scan_ps_macos(text: &str, apps: &[(&str, &str)]) -> Option<RunningChrome> {
    let mut first: Option<(String, PathBuf)> = None;
    let mut instances = 0usize;
    let mut any_flag = false;
    for line in text.lines() {
        if line.contains("--type=") {
            continue; // helper (renderer/gpu/utility) — not the browser
        }
        for (marker, app) in apps {
            if let Some(pos) = line.find(marker) {
                instances += 1;
                if line.contains(SILENCE_FLAG) {
                    any_flag = true;
                }
                if first.is_none() {
                    let end = pos + marker.len();
                    first = Some((app.to_string(), PathBuf::from(&line[..end])));
                }
                break; // at most one marker per line
            }
        }
    }
    let (app_name, exe) = first?;
    Some(RunningChrome {
        app_name,
        exe,
        pid: None,
        instances,
        has_flag: any_flag,
    })
}

#[cfg(target_os = "macos")]
fn graceful_quit(rc: &RunningChrome) -> Result<(), String> {
    // AppleScript `quit` ≈ Cmd-Q: Chrome saves its session before exiting.
    let script = format!("tell application \"{}\" to quit", rc.app_name);
    let _ = Command::new("osascript").args(["-e", &script]).status();
    wait_until_gone(QUIT_TIMEOUT)
}

#[cfg(target_os = "macos")]
fn relaunch(rc: &RunningChrome) -> Result<(), String> {
    // `open -a … --args` only passes flags on a cold start — guaranteed here
    // because graceful_quit already confirmed the browser fully exited. Uses
    // the default profile (all logins) since no --user-data-dir is given.
    let status = Command::new("open")
        .args(["-a", &rc.app_name, "--args", SILENCE_FLAG, RESTORE_FLAG])
        .status()
        .map_err(|e| format!("failed to relaunch {}: {e}", rc.app_name))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("`open -a {}` exited with failure", rc.app_name))
    }
}

// ───────────────────────── Linux ─────────────────────────

#[cfg(target_os = "linux")]
fn detect() -> Option<RunningChrome> {
    let out = Command::new("ps")
        .args(["-axww", "-o", "pid=,command="])
        .output()
        .ok()?;
    let text = String::from_utf8_lossy(&out.stdout);
    let mut first: Option<(String, PathBuf, Option<u32>)> = None;
    let mut instances = 0usize;
    let mut any_flag = false;
    for raw in text.lines() {
        let line = raw.trim_start();
        let (pid_str, cmd) = match line.split_once(char::is_whitespace) {
            Some(v) => v,
            None => continue,
        };
        let cmd = cmd.trim_start();
        if cmd.contains("--type=") {
            continue;
        }
        let argv0 = cmd.split_whitespace().next().unwrap_or("");
        let base = argv0.rsplit('/').next().unwrap_or(argv0);
        let is_chrome = matches!(
            base,
            "chrome"
                | "chromium"
                | "chromium-browser"
                | "google-chrome"
                | "google-chrome-stable"
                | "brave"
                | "brave-browser"
        );
        if !is_chrome {
            continue;
        }
        instances += 1;
        if cmd.contains(SILENCE_FLAG) {
            any_flag = true;
        }
        if first.is_none() {
            let pid = pid_str.trim().parse::<u32>().ok();
            let exe = pid
                .and_then(|p| std::fs::read_link(format!("/proc/{p}/exe")).ok())
                .unwrap_or_else(|| PathBuf::from(argv0));
            first = Some((base.to_string(), exe, pid));
        }
    }
    let (app_name, exe, pid) = first?;
    Some(RunningChrome {
        app_name,
        exe,
        pid,
        instances,
        has_flag: any_flag,
    })
}

#[cfg(target_os = "linux")]
fn graceful_quit(rc: &RunningChrome) -> Result<(), String> {
    // SIGTERM (never SIGKILL) lets Chrome shut down cleanly and save its session.
    if let Some(pid) = rc.pid {
        let _ = Command::new("kill")
            .args(["-TERM", &pid.to_string()])
            .status();
    }
    wait_until_gone(QUIT_TIMEOUT)
}

#[cfg(target_os = "linux")]
fn relaunch(rc: &RunningChrome) -> Result<(), String> {
    use std::process::Stdio;
    Command::new(&rc.exe)
        .args([SILENCE_FLAG, RESTORE_FLAG])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map(|_| ())
        .map_err(|e| format!("failed to relaunch {:?}: {e}", rc.exe))
}

// ───────────────────────── Windows ─────────────────────────

#[cfg(target_os = "windows")]
fn detect() -> Option<RunningChrome> {
    // ExecutablePath<TAB>PID<TAB>CommandLine for every chrome/brave process.
    let script = "Get-CimInstance Win32_Process -Filter \"Name='chrome.exe' OR \
                  Name='brave.exe'\" | ForEach-Object { $_.ExecutablePath + \"`t\" + \
                  [string]$_.ProcessId + \"`t\" + $_.CommandLine }";
    let out = Command::new("powershell")
        .args(["-NoProfile", "-NonInteractive", "-Command", script])
        .output()
        .ok()?;
    let text = String::from_utf8_lossy(&out.stdout);
    let mut first: Option<(PathBuf, Option<u32>)> = None;
    let mut instances = 0usize;
    let mut any_flag = false;
    for line in text.lines() {
        let mut parts = line.splitn(3, '\t');
        let exe = parts.next().unwrap_or("").trim();
        let pid = parts.next().unwrap_or("").trim();
        let cmd = parts.next().unwrap_or("");
        if exe.is_empty() || cmd.contains("--type=") {
            continue; // blank path or a helper process
        }
        instances += 1;
        if cmd.contains(SILENCE_FLAG) {
            any_flag = true;
        }
        if first.is_none() {
            first = Some((PathBuf::from(exe), pid.parse::<u32>().ok()));
        }
    }
    let (exe, pid) = first?;
    Some(RunningChrome {
        app_name: String::new(),
        exe,
        pid,
        instances,
        has_flag: any_flag,
    })
}

#[cfg(target_os = "windows")]
fn graceful_quit(rc: &RunningChrome) -> Result<(), String> {
    // `taskkill` WITHOUT `/F` posts WM_CLOSE so Chrome shuts down cleanly and
    // saves its session. `/F` (or PowerShell Stop-Process) would force-kill and
    // lose the tabs, so it is deliberately avoided.
    let img = rc
        .exe
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("chrome.exe");
    let _ = Command::new("taskkill").args(["/IM", img]).status();
    wait_until_gone(QUIT_TIMEOUT)
}

#[cfg(target_os = "windows")]
fn relaunch(rc: &RunningChrome) -> Result<(), String> {
    use std::process::Stdio;
    Command::new(&rc.exe)
        .args([SILENCE_FLAG, RESTORE_FLAG])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map(|_| ())
        .map_err(|e| format!("failed to relaunch {:?}: {e}", rc.exe))
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::*;

    const APPS: &[(&str, &str)] = &[
        (
            "Google Chrome.app/Contents/MacOS/Google Chrome",
            "Google Chrome",
        ),
        ("Chromium.app/Contents/MacOS/Chromium", "Chromium"),
    ];

    const MAIN: &str = "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome";

    #[test]
    fn none_when_no_chrome() {
        assert!(scan_ps_macos("/usr/bin/some-daemon --foo\n/bin/zsh", APPS).is_none());
    }

    #[test]
    fn single_flagged_instance() {
        let text = format!("{MAIN} {SILENCE_FLAG} {RESTORE_FLAG}\n");
        let rc = scan_ps_macos(&text, APPS).expect("should detect");
        assert_eq!(rc.instances, 1);
        assert!(rc.has_flag);
        assert_eq!(rc.app_name, "Google Chrome");
        assert_eq!(rc.exe, PathBuf::from(MAIN));
    }

    #[test]
    fn single_unflagged_instance() {
        let rc = scan_ps_macos(&format!("{MAIN}\n"), APPS).expect("should detect");
        assert_eq!(rc.instances, 1);
        assert!(!rc.has_flag);
    }

    /// The regression: a flagless instance is listed BEFORE the flagged real
    /// browser. `has_flag` must still be true (ANY instance), so we never quit a
    /// browser that is already silenced, and `instances` must be 2 (→ Ambiguous).
    #[test]
    fn flagless_before_flagged_is_still_has_flag() {
        let text =
            format!("{MAIN} --user-data-dir=/tmp/scratch\n{MAIN} {SILENCE_FLAG} {RESTORE_FLAG}\n");
        let rc = scan_ps_macos(&text, APPS).expect("should detect");
        assert_eq!(rc.instances, 2, "both main processes counted");
        assert!(rc.has_flag, "ANY instance with the flag ⇒ already silent");
    }

    #[test]
    fn two_unflagged_instances_are_ambiguous() {
        let text = format!("{MAIN} --user-data-dir=/tmp/a\n{MAIN} --user-data-dir=/tmp/b\n");
        let rc = scan_ps_macos(&text, APPS).expect("should detect");
        assert_eq!(rc.instances, 2);
        assert!(!rc.has_flag);
    }

    #[test]
    fn helper_processes_are_ignored() {
        // Renderer/GPU helpers carry --type= and must not count as instances.
        let text = format!(
            "/Applications/Google Chrome.app/Contents/Frameworks/Google Chrome Framework.framework/Versions/1/Helpers/Google Chrome Helper (Renderer).app/Contents/MacOS/Google Chrome Helper (Renderer) --type=renderer\n{MAIN}\n"
        );
        let rc = scan_ps_macos(&text, APPS).expect("should detect");
        assert_eq!(rc.instances, 1, "only the main process counts");
        assert!(!rc.has_flag);
    }
}
