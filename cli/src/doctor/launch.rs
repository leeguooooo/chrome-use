//! Live launch test: spawn a scratch daemon session, launch headless
//! Chrome, navigate to `about:blank`, then close. Skipped under `--quick`.
//! When the extension relay is up it probes the relay instead and launches
//! nothing: that is the path chrome-use takes, and doctor must not start a
//! browser the user did not ask for.
//!
//! A `LaunchGuard` Drop impl ensures the scratch session is closed and its
//! sidecar files cleaned even on panic or early return.

use std::env;
use std::time::{Duration, Instant, SystemTime};

use serde_json::{json, Value};

use super::helpers::new_id;
use super::{Check, Status};
use crate::connection::{cleanup_stale_files, ensure_daemon, send_command, DaemonOptions};

pub(super) fn check(checks: &mut Vec<Check>) {
    let category = "Launch test";

    if env::var("AGENT_BROWSER_PROVIDER").is_ok() {
        checks.push(Check::new(
            "launch.skipped.provider",
            category,
            Status::Info,
            "Skipped (AGENT_BROWSER_PROVIDER is set; would consume cloud quota)",
        ));
        return;
    }
    if env::var("AGENT_BROWSER_CDP").is_ok() {
        checks.push(Check::new(
            "launch.skipped.cdp",
            category,
            Status::Info,
            "Skipped (AGENT_BROWSER_CDP is set; would attach to a real browser)",
        ));
        return;
    }

    // With the extension relay up, chrome-use drives the user's own Chrome
    // and never launches one, so a launch test would exercise a path the user
    // does not take, and start a browser they never asked for (54 doctor
    // launches with the relay up in one user's connect-mode.log). Check the
    // path they do take instead: does the relay answer?
    if crate::connect::relay_url().is_some() {
        let (health, profiles) = crate::connect::relay_health_and_profiles();
        relay_checks(checks, &health);
        for warning in crate::connect::duplicate_extension_warnings(&profiles) {
            checks.push(Check::new(
                "relay.duplicate",
                "Launch test",
                Status::Warn,
                warning,
            ));
        }
        return;
    }

    // Short on purpose. A unix socket path is capped at 103 bytes, and the
    // whole config directory sits in front of the session name — under a deep
    // HOME (a sandbox, a CI workspace, a container mount) a long name is what
    // pushes it over, and the failure blames a name the user never chose
    // (issue #259). Base-36 pid and millis keep it unique and readable while
    // costing ~14 bytes instead of ~28.
    let millis = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    let session = format!(
        "doctor-{}-{}",
        to_base36(std::process::id() as u128),
        to_base36(millis)
    );

    // Armed after `ensure_daemon` succeeds so we don't send a stray `close`
    // or delete sidecar files for a daemon that never started. On every early
    // return past the `Some(...)` assignment below, Drop runs one close and
    // one `cleanup_stale_files`.
    let mut _guard: Option<LaunchGuard> = None;

    let opts = DaemonOptions {
        headed: false,
        debug: false,
        executable_path: None,
        extensions: &[],
        init_scripts: &[],
        enable: &[],
        args: None,
        user_agent: None,
        proxy: None,
        proxy_bypass: None,
        proxy_username: None,
        proxy_password: None,
        ignore_https_errors: false,
        allow_file_access: false,
        hide_scrollbars: true,
        profile: None,
        state: None,
        provider: None,
        device: None,
        session_name: None,
        download_path: None,
        allowed_domains: None,
        action_policy: None,
        confirm_actions: None,
        engine: None,
        auto_connect: false,
        force_launch: false,
        idle_timeout: None,
        default_timeout: None,
        cdp: None,
        no_auto_dialog: false,
    };

    // `"headless": true` below is ignored unless the daemon has
    // AGENT_BROWSER_ALLOW_HEADLESS (launches are headed by default for
    // stealth), so this check used to open a visible Chrome window on the
    // user's screen. Force the scratch daemon's environment to a windowless
    // launch, overriding whatever the shell set (see `isolate_launch_env`).
    // The daemon inherits this process's environment; every other check has
    // finished by now, so nothing reads it concurrently.
    apply_isolated_launch_env();

    let started = Instant::now();
    if let Err(e) = ensure_daemon(&session, &opts) {
        checks.push(
            Check::new(
                "launch.daemon",
                category,
                Status::Fail,
                format!("Could not start daemon: {}", e),
            )
            .with_fix("check Chrome install and re-run with --debug"),
        );
        return;
    }
    _guard = Some(LaunchGuard {
        session: session.clone(),
    });

    let launch_cmd = json!({
        "id": new_id(),
        "action": "launch",
        "headless": true,
    });
    let headless = match send_command(launch_cmd, &session) {
        Ok(resp) if resp.success => resp
            .data
            .as_ref()
            .and_then(|d| d.get("headless"))
            .and_then(Value::as_bool),
        Ok(resp) => {
            launch_failed(checks, resp.error.unwrap_or_else(|| "unknown error".into()));
            return;
        }
        Err(e) => {
            launch_failed(checks, e);
            return;
        }
    };
    // Verify the effect, not the request: the browser must have been spawned
    // with `--headless` (its real argv, reported by the daemon). A visible
    // window here is the bug this check used to be; the guard closes it.
    if headless != Some(true) {
        checks.push(
            Check::new(
                "launch.headless",
                category,
                Status::Fail,
                match headless {
                    Some(false) => {
                        "The test browser was started WITH a window (no --headless in its \
                                    arguments); it was closed at once"
                            .to_string()
                    }
                    _ => "Could not confirm the test browser was started headless; it was closed \
                          at once"
                        .to_string(),
                },
            )
            .with_fix("chrome-use report --note \"doctor launch was not headless\""),
        );
        return;
    }

    let open_cmd = json!({
        "id": new_id(),
        "action": "navigate",
        "url": "about:blank",
    });
    if let Err(e) = send_json(open_cmd, &session) {
        checks.push(
            Check::new(
                "launch.navigate",
                category,
                Status::Fail,
                format!("Navigation to about:blank failed: {}", e),
            )
            .with_fix("re-run with --debug for full launch logs"),
        );
        return;
    }

    // Close + stale-file cleanup happen exactly once via LaunchGuard::drop at
    // end of scope; no explicit close here.
    let elapsed = started.elapsed();
    let secs = elapsed.as_secs_f64();
    if elapsed > Duration::from_secs(5) {
        checks.push(Check::new(
            "launch.elapsed",
            category,
            Status::Warn,
            format!(
                "Headless launch + about:blank in {:.2}s (slow; expected < 5s)",
                secs
            ),
        ));
    } else {
        checks.push(Check::new(
            "launch.elapsed",
            category,
            Status::Pass,
            format!("Headless launch + about:blank in {:.2}s", secs),
        ));
    }
}

fn relay_checks(checks: &mut Vec<Check>, health: &crate::connect::RelayHealth) {
    let mut transport = Check::new(
        "launch.relay",
        "Launch test",
        if health.transport_responsive {
            Status::Pass
        } else {
            Status::Warn
        },
        if health.transport_responsive {
            "relay transport responsive (no browser launched)"
        } else {
            "relay transport did not answer (no browser launched)"
        },
    );
    if transport.status == Status::Warn {
        transport = transport
            .with_fix("chrome-use extension connect   # or reload the ab-connect extension");
    }
    checks.push(transport);
    if let Some(notice) = health.host_notice() {
        checks.push(Check::new(
            "relay.hostDiagnostic",
            "Launch test",
            Status::Info,
            notice,
        ));
    }
    let mut debugger = Check::new(
        "relay.debugger",
        "Launch test",
        if health.debugger_warns() {
            Status::Warn
        } else if health.debugger_answered() {
            Status::Pass
        } else {
            Status::Info
        },
        health.debugger.clone(),
    );
    if debugger.status == Status::Warn {
        debugger = debugger
            .with_fix("reload the chrome-use extension at chrome://extensions (or restart Chrome)");
    }
    checks.push(debugger);
}

fn launch_failed(checks: &mut Vec<Check>, e: String) {
    checks.push(
        Check::new(
            "launch.launch",
            "Launch test",
            Status::Fail,
            format!("Browser launch failed: {}", e),
        )
        .with_fix("chrome-use install   # or check --debug output"),
    );
}

fn send_json(cmd: Value, session: &str) -> Result<(), String> {
    match send_command(cmd, session) {
        Ok(resp) => {
            if resp.success {
                Ok(())
            } else {
                Err(resp.error.unwrap_or_else(|| "unknown error".to_string()))
            }
        }
        Err(e) => Err(e),
    }
}

/// Best-effort cleanup when the launch test panics or returns early.
struct LaunchGuard {
    session: String,
}

impl Drop for LaunchGuard {
    fn drop(&mut self) {
        let close_cmd = json!({ "id": new_id(), "action": "close" });
        let _ = send_command(close_cmd, &self.session);
        cleanup_stale_files(&self.session);
    }
}

/// The environment the scratch daemon needs for a launch with no window:
/// `Some(v)` to set, `None` to remove. Inherited values must not win:
/// `AGENT_BROWSER_ALLOW_HEADLESS=0`, `AGENT_BROWSER_HEADED=1` or any
/// `AGENT_BROWSER_EXTENSIONS` (extensions force a headed launch) would each
/// put a visible window on the user's screen.
fn isolate_launch_env() -> [(&'static str, Option<&'static str>); 3] {
    [
        ("AGENT_BROWSER_ALLOW_HEADLESS", Some("1")),
        ("AGENT_BROWSER_HEADED", None),
        ("AGENT_BROWSER_EXTENSIONS", None),
    ]
}

fn apply_isolated_launch_env() {
    for (key, value) in isolate_launch_env() {
        match value {
            Some(v) => env::set_var(key, v),
            None => env::remove_var(key),
        }
    }
}

/// Lowercase base-36, for keeping generated session names short.
fn to_base36(mut n: u128) -> String {
    const DIGITS: &[u8] = b"0123456789abcdefghijklmnopqrstuvwxyz";
    if n == 0 {
        return "0".to_string();
    }
    let mut out = Vec::new();
    while n > 0 {
        out.push(DIGITS[(n % 36) as usize]);
        n /= 36;
    }
    out.reverse();
    String::from_utf8(out).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::native::cdp::chrome::{launch_args_for_test, launches_headless, LaunchOptions};
    use crate::test_utils::EnvGuard;

    #[test]
    fn passive_relay_checks_classify_observations_and_restore_transport_fix() {
        for (extension_health, expected) in [
            (None, Status::Info),
            (Some(json!({"answered":0,"timedOut":0})), Status::Info),
            (Some(json!({"answered":4,"timedOut":0})), Status::Pass),
            (Some(json!({"answered":4,"timedOut":2})), Status::Warn),
        ] {
            for responsive in [false, true] {
                let mut checks = vec![];
                relay_checks(
                    &mut checks,
                    &crate::connect::RelayHealth {
                        transport_responsive: responsive,
                        extension_health: extension_health.clone(),
                        ..Default::default()
                    },
                );
                let transport = checks.iter().find(|c| c.id == "launch.relay").unwrap();
                assert_eq!(
                    transport.status,
                    if responsive {
                        Status::Pass
                    } else {
                        Status::Warn
                    }
                );
                assert_eq!(
                    transport.fix.as_deref(),
                    if responsive {
                        None
                    } else {
                        Some("chrome-use extension connect   # or reload the ab-connect extension")
                    }
                );
                let debugger = checks.iter().find(|c| c.id == "relay.debugger").unwrap();
                assert_eq!(debugger.status, expected);
                assert_eq!(debugger.fix.is_some(), expected == Status::Warn);
                assert_eq!(
                    checks.iter().any(|c| c.id == "relay.hostDiagnostic"),
                    responsive
                );
            }
        }
    }

    /// The launch options the scratch daemon builds for doctor's
    /// `{"action":"launch","headless":true}` (no extensions in the command),
    /// plus any extensions the environment would still contribute.
    fn doctor_launch_options() -> LaunchOptions {
        LaunchOptions {
            headless: true,
            extensions: env::var("AGENT_BROWSER_EXTENSIONS")
                .ok()
                .map(|v| v.split(',').map(str::to_string).collect()),
            ..Default::default()
        }
    }

    #[test]
    fn doctor_launch_is_headless_whatever_the_shell_inherited() {
        let vars = [
            "AGENT_BROWSER_ALLOW_HEADLESS",
            "AGENT_BROWSER_HEADED",
            "AGENT_BROWSER_EXTENSIONS",
        ];
        let inherited: &[&[(&str, &str)]] = &[
            &[],
            &[("AGENT_BROWSER_ALLOW_HEADLESS", "0")],
            &[("AGENT_BROWSER_ALLOW_HEADLESS", "false")],
            &[("AGENT_BROWSER_HEADED", "1")],
            &[("AGENT_BROWSER_EXTENSIONS", "/tmp/some-extension")],
            &[
                ("AGENT_BROWSER_ALLOW_HEADLESS", "0"),
                ("AGENT_BROWSER_HEADED", "true"),
                ("AGENT_BROWSER_EXTENSIONS", "/tmp/a,/tmp/b"),
            ],
        ];
        for case in inherited {
            let guard = EnvGuard::new(&vars);
            for v in vars {
                guard.remove(v);
            }
            for (k, v) in *case {
                guard.set(k, v);
            }
            apply_isolated_launch_env();
            let opts = doctor_launch_options();
            let args = launch_args_for_test(&opts);
            assert!(
                args.iter().any(|a| a == "--headless=new"),
                "{case:?}: doctor's launch must carry --headless=new, got {args:?}"
            );
            assert!(launches_headless(&opts), "{case:?}");
            assert!(
                env::var_os("AGENT_BROWSER_EXTENSIONS").is_none(),
                "{case:?}"
            );
            assert!(env::var_os("AGENT_BROWSER_HEADED").is_none(), "{case:?}");
            drop(guard);
        }
    }
}
