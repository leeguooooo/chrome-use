mod chat;
mod color;
mod commands;
mod connect;
mod connection;
mod cookie_export;
mod doctor;
mod findurl;
mod flags;
mod friction;
mod install;
mod native;
mod output;
mod read;
mod silence;
mod site;
mod skills;
mod test_runner;
#[cfg(test)]
mod test_utils;
mod upgrade;
mod validation;

use serde_json::json;
use std::env;
use std::fs;
use std::process::exit;

#[cfg(windows)]
use windows_sys::Win32::Foundation::CloseHandle;
#[cfg(windows)]
use windows_sys::Win32::System::Threading::OpenProcess;

use commands::{gen_id, parse_command, ParseError};
use connection::{
    cleanup_stale_files, ensure_daemon, get_socket_dir, is_pid_alive, restart_all_daemons,
    send_command, walk_daemons, DaemonOptions,
};
use flags::{clean_args, parse_flags, Flags};
use install::run_install;
use output::{
    print_command_help, print_help, print_response_with_opts, print_version, OutputOptions,
};
use upgrade::run_upgrade;

fn serialize_json_value(value: &serde_json::Value) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| {
        r#"{"success":false,"error":"Failed to serialize JSON response"}"#.to_string()
    })
}

fn print_json_value(value: serde_json::Value) {
    println!("{}", serialize_json_value(&value));
}

fn print_json_error(message: impl AsRef<str>) {
    print_json_value(json!({
        "success": false,
        "error": message.as_ref(),
    }));
}

fn print_json_error_with_type(message: impl AsRef<str>, error_type: &str) {
    print_json_value(json!({
        "success": false,
        "error": message.as_ref(),
        "type": error_type,
    }));
}

fn should_send_hide_scrollbars_launch_option(
    cli_hide_scrollbars: bool,
    hide_scrollbars: bool,
) -> bool {
    cli_hide_scrollbars || !hide_scrollbars
}

fn apply_hide_scrollbars_launch_option(
    launch_cmd: &mut serde_json::Value,
    cli_hide_scrollbars: bool,
    hide_scrollbars: bool,
) {
    if should_send_hide_scrollbars_launch_option(cli_hide_scrollbars, hide_scrollbars) {
        launch_cmd["hideScrollbars"] = json!(hide_scrollbars);
    }
}

struct ParsedProxy {
    server: String,
    username: Option<String>,
    password: Option<String>,
}

fn parse_proxy(proxy_str: &str) -> ParsedProxy {
    let Some(protocol_end) = proxy_str.find("://") else {
        return ParsedProxy {
            server: proxy_str.to_string(),
            username: None,
            password: None,
        };
    };
    let protocol = &proxy_str[..protocol_end + 3];
    let rest = &proxy_str[protocol_end + 3..];

    let Some(at_pos) = rest.rfind('@') else {
        return ParsedProxy {
            server: proxy_str.to_string(),
            username: None,
            password: None,
        };
    };

    let creds = &rest[..at_pos];
    let server_part = &rest[at_pos + 1..];
    let server = format!("{}{}", protocol, server_part);

    let (username, password) = match creds.find(':') {
        Some(colon_pos) => {
            let u = &creds[..colon_pos];
            let p = &creds[colon_pos + 1..];
            (
                if u.is_empty() {
                    None
                } else {
                    Some(u.to_string())
                },
                if p.is_empty() {
                    None
                } else {
                    Some(p.to_string())
                },
            )
        }
        None => (
            if creds.is_empty() {
                None
            } else {
                Some(creds.to_string())
            },
            None,
        ),
    };

    ParsedProxy {
        server,
        username,
        password,
    }
}

fn run_profiles(json_mode: bool) {
    use crate::native::cdp::chrome::{find_chrome_user_data_dir, list_chrome_profiles};

    let user_data_dir = match find_chrome_user_data_dir() {
        Some(dir) => dir,
        None => {
            if json_mode {
                print_json_error("No Chrome user data directory found");
            } else {
                eprintln!("{}", color::red("No Chrome user data directory found"));
            }
            exit(1);
        }
    };

    let profiles = list_chrome_profiles(&user_data_dir);
    if profiles.is_empty() {
        if json_mode {
            print_json_value(json!({
                "success": true,
                "data": []
            }));
        } else {
            println!("No Chrome profiles found");
        }
        return;
    }

    if json_mode {
        let items: Vec<serde_json::Value> = profiles
            .iter()
            .map(|p| {
                json!({
                    "directory": p.directory,
                    "name": p.name
                })
            })
            .collect();
        print_json_value(json!({
            "success": true,
            "data": items
        }));
    } else {
        println!(
            "{} ({}):\n",
            color::bold("Chrome profiles"),
            user_data_dir.display()
        );
        for p in &profiles {
            println!(
                "  {}  {}",
                color::bold(&p.directory),
                color::dim(&format!("({})", p.name))
            );
        }
    }
}

fn run_cookies_export(args: &[String], flags: &Flags) {
    // Source profile comes from `--from <profile>`, falling back to the global
    // `--profile` (which the flag parser has already moved into flags.profile).
    let from = args
        .iter()
        .position(|a| a == "--from")
        .and_then(|i| args.get(i + 1))
        .map(|s| s.as_str())
        .or(flags.profile.as_deref());
    let profile = match from {
        Some(p) => p,
        None => {
            let msg = "cookies export needs a source profile: cookies export --from <profile> [--domain <d>]";
            if flags.json {
                print_json_error(msg);
            } else {
                eprintln!("{} {}", color::error_indicator(), msg);
            }
            exit(1);
        }
    };
    let domain = args
        .iter()
        .position(|a| a == "--domain")
        .and_then(|i| args.get(i + 1))
        .map(|s| s.as_str());

    match cookie_export::export_cookies(profile, domain) {
        Ok(cookies) => {
            if flags.json {
                print_json_value(json!({ "success": true, "data": cookies }));
            } else {
                // A JSON array ready for `cookies set --curl <file>`.
                println!(
                    "{}",
                    serde_json::to_string(&cookies).unwrap_or_else(|_| "[]".to_string())
                );
                eprintln!(
                    "{}",
                    color::dim(&format!(
                        "{} cookies exported from \"{}\"",
                        cookies.len(),
                        profile
                    ))
                );
            }
        }
        Err(e) => {
            if flags.json {
                print_json_error(&e);
            } else {
                eprintln!("{} {}", color::error_indicator(), e);
            }
            exit(1);
        }
    }
}

fn run_session(args: &[String], session: &str, json_mode: bool) {
    let subcommand = args.get(1).map(|s| s.as_str());

    match subcommand {
        Some("list") => {
            let sessions: Vec<String> = walk_daemons()
                .sessions
                .into_iter()
                .map(|s| s.name)
                .collect();
            // The extension relay drives the user's live Chrome but isn't always
            // registered as a launched daemon session — without surfacing it,
            // `session list` says "No active sessions" while open/tab work fine,
            // and agents misjudge the connection as down (issue #15).
            let relay_up = connect::relay_url().is_some();

            if json_mode {
                println!(
                    r#"{{"success":true,"data":{{"sessions":{},"relay":{}}}}}"#,
                    serde_json::to_string(&sessions).unwrap_or_default(),
                    relay_up
                );
            } else if sessions.is_empty() && !relay_up {
                println!("No active sessions");
            } else {
                println!("Active sessions:");
                for s in &sessions {
                    let marker = if s == session {
                        color::cyan("→")
                    } else {
                        " ".to_string()
                    };
                    println!("{} {}", marker, s);
                }
                if relay_up && !sessions.iter().any(|s| s == session) {
                    println!(
                        "{} {} {}",
                        color::cyan("→"),
                        session,
                        color::dim("(relay/extension → live Chrome)")
                    );
                }
            }
        }
        // Stop a specific session daemon (issue #48). Graceful: kill_stale_daemon
        // sends SIGTERM first, so the daemon's shutdown handler runs `close()` and
        // tidies the tabs IT created (its tab group) before exiting.
        Some("stop") => {
            let target = args.get(2).map(|s| s.as_str()).unwrap_or(session);
            connection::kill_stale_daemon(target);
            if json_mode {
                print_json_value(json!({ "success": true, "data": { "stopped": target } }));
            } else {
                println!(
                    "{} stopped session daemon: {}",
                    color::success_indicator(),
                    target
                );
            }
        }
        // Reclaim ALL session daemons now (issue #48) — for clearing the pile of
        // idle daemons left after a round of automation/debugging without waiting
        // for the idle timeout. Each is stopped gracefully (closes its own tabs);
        // they respawn clean on next use. The `__nm-host` relay is not a tracked
        // session daemon, so the extension/live-Chrome connection survives.
        Some("prune") => {
            let sessions: Vec<String> = walk_daemons()
                .sessions
                .into_iter()
                .map(|s| s.name)
                .collect();
            for s in &sessions {
                connection::kill_stale_daemon(s);
            }
            if json_mode {
                print_json_value(json!({ "success": true, "data": { "pruned": sessions } }));
            } else if sessions.is_empty() {
                println!("No session daemons to prune");
            } else {
                println!(
                    "{} pruned {} session daemon(s): {}",
                    color::success_indicator(),
                    sessions.len(),
                    sessions.join(", ")
                );
            }
        }
        None | Some(_) => {
            // Just show current session
            if json_mode {
                print_json_value(json!({
                    "success": true,
                    "data": {
                        "session": session,
                    },
                }));
            } else {
                println!("{}", session);
            }
        }
    }
}

/// `chrome-use daemon <restart|status>` — manage the per-session daemon workers
/// without resorting to `pgrep`/`kill`. `restart` clears corrupted or
/// cross-leaked daemon state (e.g. after a mid-session `chrome-use upgrade`
/// where stale tab handles bleed across sessions, issue #20) by killing every
/// session worker. The Chrome-launched `__nm-host` native-messaging bridge is
/// NOT a tracked session daemon, so the extension relay survives a restart —
/// the next command spins up a fresh, clean daemon against the same live Chrome.
fn run_daemon(args: &[String], json_mode: bool) {
    match args.get(1).map(|s| s.as_str()) {
        Some("restart") => {
            let stopped = restart_all_daemons();
            let relay_up = connect::relay_url().is_some();
            if json_mode {
                print_json_value(json!({
                    "success": true,
                    "data": { "stopped": stopped, "count": stopped.len(), "relay": relay_up },
                }));
            } else if stopped.is_empty() {
                println!("No session daemons running — nothing to restart.");
                if relay_up {
                    println!(
                        "{}",
                        color::dim("Extension relay still up; next command starts a fresh daemon.")
                    );
                }
            } else {
                for s in &stopped {
                    println!("{} Stopped daemon: {}", color::green("✓"), s);
                }
                println!(
                    "{}",
                    color::dim(if relay_up {
                        "Extension relay (__nm-host) left running; next command starts a fresh daemon."
                    } else {
                        "Next command starts a fresh daemon."
                    })
                );
            }
        }
        Some("status") | Some("list") => {
            let inventory = walk_daemons();
            let relay_up = connect::relay_url().is_some();
            if json_mode {
                let sessions: Vec<_> = inventory
                    .sessions
                    .iter()
                    .map(|s| json!({ "name": s.name, "pid": s.pid, "version": s.version }))
                    .collect();
                print_json_value(json!({
                    "success": true,
                    "data": { "sessions": sessions, "relay": relay_up },
                }));
            } else if inventory.sessions.is_empty() {
                println!("No session daemons running.");
                if relay_up {
                    println!("{}", color::dim("Extension relay (__nm-host): up"));
                }
            } else {
                println!("Session daemons:");
                for s in &inventory.sessions {
                    let ver = s
                        .version
                        .as_deref()
                        .map(|v| format!(" {}", color::dim(&format!("(v{})", v))))
                        .unwrap_or_default();
                    println!("  {} pid {}{}", s.name, s.pid, ver);
                }
                if relay_up {
                    println!("{}", color::dim("Extension relay (__nm-host): up"));
                }
            }
        }
        other => {
            eprintln!(
                "{} usage: chrome-use daemon <restart|status>",
                color::error_indicator()
            );
            if let Some(unknown) = other {
                eprintln!(
                    "{}",
                    color::dim(&format!("  unknown subcommand: {}", unknown))
                );
            }
            exit(2);
        }
    }
}

fn get_dashboard_pid_path() -> std::path::PathBuf {
    get_socket_dir().join("dashboard.pid")
}

fn run_dashboard_start(port: u16, json_mode: bool) {
    let pid_path = get_dashboard_pid_path();

    // Check if already running
    if let Ok(pid_str) = fs::read_to_string(&pid_path) {
        if let Ok(pid) = pid_str.trim().parse::<u32>() {
            if is_pid_alive(pid) {
                if json_mode {
                    print_json_value(json!({
                        "success": true,
                        "data": { "port": port, "pid": pid, "already_running": true },
                    }));
                } else {
                    println!("Dashboard already running at http://localhost:{}", port);
                }
                return;
            }
        }
        let _ = fs::remove_file(&pid_path);
    }

    let socket_dir = get_socket_dir();
    if !socket_dir.exists() {
        let _ = fs::create_dir_all(&socket_dir);
    }

    let exe_path = match env::current_exe() {
        Ok(p) => p.canonicalize().unwrap_or(p),
        Err(e) => {
            if json_mode {
                print_json_error(format!("Failed to get executable path: {}", e));
            } else {
                eprintln!(
                    "{} Failed to get executable path: {}",
                    color::error_indicator(),
                    e
                );
            }
            exit(1);
        }
    };

    let mut cmd = std::process::Command::new(&exe_path);
    cmd.env("AGENT_BROWSER_DASHBOARD", "1")
        .env("AGENT_BROWSER_DASHBOARD_PORT", port.to_string());

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

    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x00000200;
        const DETACHED_PROCESS: u32 = 0x00000008;
        cmd.creation_flags(CREATE_NEW_PROCESS_GROUP | DETACHED_PROCESS);
    }

    match cmd
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
    {
        Ok(child) => {
            let pid = child.id();
            let _ = fs::write(&pid_path, pid.to_string());

            if json_mode {
                print_json_value(json!({
                    "success": true,
                    "data": { "port": port, "pid": pid },
                }));
            } else {
                println!("Dashboard started at http://localhost:{}", port);
            }
        }
        Err(e) => {
            if json_mode {
                print_json_error(format!("Failed to start dashboard: {}", e));
            } else {
                eprintln!(
                    "{} Failed to start dashboard: {}",
                    color::error_indicator(),
                    e
                );
            }
            exit(1);
        }
    }
}

fn run_dashboard_stop(json_mode: bool) {
    let pid_path = get_dashboard_pid_path();

    let pid_str = match fs::read_to_string(&pid_path) {
        Ok(s) => s,
        Err(_) => {
            if json_mode {
                print_json_value(
                    json!({ "success": true, "data": { "stopped": false, "reason": "not running" } }),
                );
            } else {
                println!("Dashboard is not running");
            }
            return;
        }
    };

    let pid: u32 = match pid_str.trim().parse() {
        Ok(p) => p,
        Err(_) => {
            let _ = fs::remove_file(&pid_path);
            if json_mode {
                print_json_value(
                    json!({ "success": true, "data": { "stopped": false, "reason": "invalid pid" } }),
                );
            } else {
                println!("Dashboard is not running");
            }
            return;
        }
    };

    #[cfg(unix)]
    {
        unsafe {
            libc::kill(pid as i32, libc::SIGTERM);
        }
    }
    #[cfg(windows)]
    {
        unsafe {
            let handle = OpenProcess(1, 0, pid); // PROCESS_TERMINATE = 1
            if handle != 0 {
                windows_sys::Win32::System::Threading::TerminateProcess(handle, 0);
                CloseHandle(handle);
            }
        }
    }

    let _ = fs::remove_file(&pid_path);

    if json_mode {
        print_json_value(json!({ "success": true, "data": { "stopped": true } }));
    } else {
        println!("{} Dashboard stopped", color::green("✓"));
    }
}

fn run_close_all(flags: &Flags) {
    // walk_daemons auto-cleans stale .pid / .sock / .stream sidecar files and
    // separates out the standalone dashboard. We only want to send `close` to
    // real session daemons; the dashboard has its own `dashboard stop`.
    let inventory = walk_daemons();
    let sessions: Vec<(String, u32)> = inventory
        .sessions
        .iter()
        .map(|s| (s.name.clone(), s.pid))
        .collect();

    if sessions.is_empty() {
        if flags.json {
            print_json_value(json!({
                "success": true,
                "data": { "closed": 0, "sessions": [] },
            }));
        } else {
            println!("No active sessions");
        }
        return;
    }

    let mut closed: Vec<String> = Vec::new();
    let mut failed: Vec<(String, String)> = Vec::new();

    for (session, pid) in &sessions {
        let cmd = json!({ "id": gen_id(), "action": "close" });
        match send_command(cmd, session) {
            Ok(resp) if resp.success => closed.push(session.clone()),
            Ok(resp) => {
                let err = resp.error.unwrap_or_else(|| "Unknown error".to_string());
                failed.push((session.clone(), err));
            }
            Err(_) => {
                // Daemon is unreachable despite its process existing.
                // Force-kill the process and clean up stale files so future
                // sessions are not poisoned.
                #[cfg(unix)]
                unsafe {
                    libc::kill(*pid as i32, libc::SIGKILL);
                }
                #[cfg(windows)]
                unsafe {
                    let handle = OpenProcess(1, 0, *pid); // PROCESS_TERMINATE = 1
                    if handle != 0 {
                        windows_sys::Win32::System::Threading::TerminateProcess(handle, 1);
                        CloseHandle(handle);
                    }
                }
                cleanup_stale_files(session);
                closed.push(session.clone());
            }
        }
    }

    if flags.json {
        print_json_value(json!({
            "success": failed.is_empty(),
            "data": {
                "closed": closed.len(),
                "sessions": closed,
                "failed": failed.iter().map(|(s, e)| json!({"session": s, "error": e})).collect::<Vec<_>>(),
            },
        }));
    } else {
        for s in &closed {
            println!("{} Closed session: {}", color::green("✓"), s);
        }
        for (s, e) in &failed {
            eprintln!("{} Failed to close {}: {}", color::error_indicator(), s, e);
        }
        if closed.is_empty() && !failed.is_empty() {
            exit(1);
        }
    }

    if !failed.is_empty() {
        exit(1);
    }
}

fn main() {
    // Rust ignores SIGPIPE by default, causing println! to panic on broken pipes.
    // Reset to SIG_DFL so the OS terminates the process cleanly instead.
    #[cfg(unix)]
    unsafe {
        libc::signal(libc::SIGPIPE, libc::SIG_DFL);
    }

    // Prevent MSYS/Git Bash path translation from mangling arguments
    #[cfg(windows)]
    {
        env::set_var("MSYS_NO_PATHCONV", "1");
        env::set_var("MSYS2_ARG_CONV_EXCL", "*");
    }

    // Native-messaging host mode: Chrome launches `chrome-use __nm-host
    // <extension-origin> [...]` for the ab-connect extension. Must run before
    // ANY stdout write — stdout is the Chrome native-messaging channel.
    if env::args().nth(1).as_deref() == Some("__nm-host") {
        connect::run_nm_host();
        return;
    }

    // Hidden update-check worker, spawned detached by maybe_notify_update() to
    // refresh the cached latest version without blocking a real command.
    if env::args().nth(1).as_deref() == Some("__update-check") {
        upgrade::run_update_check();
        return;
    }

    // Non-blocking "update available" hint (stderr only; self-skips meta
    // commands, daemon mode, CI, and the opt-out env vars).
    upgrade::maybe_notify_update();

    // Native daemon mode: when AGENT_BROWSER_DAEMON is set, run as the daemon process
    if env::var("AGENT_BROWSER_DAEMON").is_ok() {
        // Ignore SIGPIPE so the daemon isn't killed when the parent drops
        // the piped stderr handle after confirming the daemon is ready.
        #[cfg(unix)]
        unsafe {
            libc::signal(libc::SIGPIPE, libc::SIG_IGN);
        }
        let session = env::var("AGENT_BROWSER_SESSION").unwrap_or_else(|_| "default".to_string());
        let rt = tokio::runtime::Runtime::new().expect("Failed to create tokio runtime");
        rt.block_on(native::daemon::run_daemon(&session));
        return;
    }

    // Standalone dashboard server mode
    if env::var("AGENT_BROWSER_DASHBOARD").is_ok() {
        let port: u16 = env::var("AGENT_BROWSER_DASHBOARD_PORT")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(4848);
        let rt = tokio::runtime::Runtime::new().expect("Failed to create tokio runtime");
        rt.block_on(native::stream::run_dashboard_server(port));
        return;
    }

    let args: Vec<String> = env::args().skip(1).collect();
    let mut flags = parse_flags(&args);
    let mut clean = clean_args(&args);

    // Loudly warn when launching a fresh browser with no profile: it gets a
    // temporary EMPTY profile (no cookies / no login). For logged-in sites the
    // user almost always wants --profile auto (their real Chrome profile).
    // Skipped under CI (force_launch is implicit there and login isn't expected).
    if flags.force_launch && flags.profile.is_none() && env::var("CI").is_err() {
        eprintln!(
            "⚠ --launch opens a fresh, isolated test profile (no cookies, no login, no \
             extensions). The window is labelled `chrome-use (<session>)` in Chrome's \
             profile menu so you can tell it apart from your real browser.\n  \
             • reuse your real Chrome (cookies/login/extensions): `--profile auto` \
             (or set AGENT_BROWSER_PROFILE=auto once)\n  \
             • load an unpacked extension into the test profile: \
             `--args \"--load-extension=<dir>\"`"
        );
    }

    let has_help = args.iter().any(|a| a == "--help" || a == "-h");
    let has_version = args.iter().any(|a| a == "--version" || a == "-V");

    if has_help {
        if let Some(cmd) = clean.first() {
            if print_command_help(cmd) {
                return;
            }
        }
        print_help();
        return;
    }

    if has_version {
        print_version();
        return;
    }

    if clean.is_empty() {
        print_help();
        return;
    }

    // Handle install separately
    if clean.first().map(|s| s.as_str()) == Some("install") {
        let with_deps = args.iter().any(|a| a == "--with-deps" || a == "-d");
        run_install(with_deps);
        return;
    }

    // Handle upgrade separately
    if clean.first().map(|s| s.as_str()) == Some("upgrade") {
        run_upgrade();
        return;
    }

    // Handle doctor separately (doesn't need daemon; spawns its own scratch
    // session for the live launch test).
    if clean.first().map(|s| s.as_str()) == Some("doctor") {
        let opts = doctor::DoctorOptions {
            offline: args.iter().any(|a| a == "--offline"),
            quick: args.iter().any(|a| a == "--quick"),
            fix: args.iter().any(|a| a == "--fix"),
            json: flags.json,
        };
        exit(doctor::run_doctor(opts));
    }

    // Handle dashboard subcommand
    if clean.first().map(|s| s.as_str()) == Some("dashboard") {
        match clean.get(1).map(|s| s.as_str()) {
            Some("start") | None => {
                let port = clean
                    .iter()
                    .position(|a| a == "--port")
                    .and_then(|i| clean.get(i + 1))
                    .and_then(|s| s.parse::<u16>().ok())
                    .unwrap_or(4848);
                run_dashboard_start(port, flags.json);
                return;
            }
            Some("stop") => {
                run_dashboard_stop(flags.json);
                return;
            }
            Some(unknown) => {
                eprintln!(
                    "{} Unknown dashboard subcommand: {}",
                    color::error_indicator(),
                    unknown
                );
                exit(1);
            }
        }
    }

    // Handle profiles command (doesn't need daemon)
    if clean.first().map(|s| s.as_str()) == Some("profiles") {
        run_profiles(flags.json);
        return;
    }

    // Handle `cookies export` (doesn't need daemon): decrypt an on-disk Chrome
    // profile's cookies and print them as JSON for `cookies set --curl`.
    if clean.first().map(|s| s.as_str()) == Some("cookies")
        && clean.get(1).map(|s| s.as_str()) == Some("export")
    {
        run_cookies_export(&clean, &flags);
        return;
    }

    // Handle `test <suite.yaml>`: run a browser test suite. It orchestrates by
    // re-invoking this binary per step, so it lives outside the normal dispatch.
    if clean.first().map(|s| s.as_str()) == Some("test") {
        let Some(suite) = clean.get(1) else {
            eprintln!(
                "{} usage: chrome-use test <suite.yaml> [--launch | --session <name>]",
                color::error_indicator()
            );
            exit(2);
        };
        exit(test_runner::run_test(suite, &flags));
    }

    // Handle `site`: site adapters — turn a website into a structured-data CLI by
    // running a per-command JS adapter inside your logged-in tab. `update`/`list`/
    // `info` are CLI-side (download/filesystem); `site <name>/<cmd> [args]` falls
    // through to the daemon dispatch below (navigate to the adapter's domain + eval).
    if clean.first().map(|s| s.as_str()) == Some("site") {
        // Auto-sync the adapter pack on first use and periodically (TTL, default
        // 7d) so adapters stay fresh without a manual `site update`. Skipped for an
        // explicit `update` (full sync below). Best-effort: offline → cached pack.
        // Disable with AGENT_BROWSER_SITES_NO_AUTO_UPDATE=1.
        if clean.get(1).map(|s| s.as_str()) != Some("update") && site::needs_refresh() {
            let rt = tokio::runtime::Runtime::new().expect("Failed to create tokio runtime");
            match rt.block_on(site::update()) {
                Ok(n) => {
                    eprintln!(
                        "{}",
                        color::dim(&format!("site: synced {n} adapters (auto)"))
                    )
                }
                Err(e) => eprintln!(
                    "{}",
                    color::dim(&format!(
                        "site: auto-sync skipped ({e}); using cached adapters"
                    ))
                ),
            }
        }
        match clean.get(1).map(|s| s.as_str()) {
            Some("update") => {
                let rt = tokio::runtime::Runtime::new().expect("Failed to create tokio runtime");
                match rt.block_on(site::update()) {
                    Ok(n) if flags.json => {
                        println!("{}", json!({ "success": true, "adapters": n }))
                    }
                    Ok(n) => println!(
                        "{} synced {} site adapters → ~/.chrome-use/sites (run `chrome-use site list`)",
                        color::success_indicator(),
                        n
                    ),
                    Err(e) => {
                        eprintln!("{} {}", color::error_indicator(), e);
                        exit(1);
                    }
                }
                return;
            }
            Some("list") => {
                match site::list_adapters() {
                    Ok(list) if flags.json => {
                        println!("{}", json!({ "success": true, "adapters": list }))
                    }
                    Ok(list) if list.is_empty() => {
                        println!("no site adapters installed — run `chrome-use site update`")
                    }
                    Ok(list) => {
                        for a in &list {
                            println!("{a}");
                        }
                        eprintln!(
                            "{}",
                            color::dim(&format!(
                                "{} adapters · run: chrome-use site <name>/<cmd> [args]",
                                list.len()
                            ))
                        );
                    }
                    Err(e) => {
                        eprintln!("{} {}", color::error_indicator(), e);
                        exit(1);
                    }
                }
                return;
            }
            Some("info") => {
                let spec = clean.get(2).cloned().unwrap_or_default();
                match site::load_adapter(&spec) {
                    Ok(a) => println!(
                        "{}",
                        serde_json::to_string_pretty(&a.meta).unwrap_or_default()
                    ),
                    Err(e) => {
                        eprintln!("{} {}", color::error_indicator(), e);
                        exit(1);
                    }
                }
                return;
            }
            // `site <name>/<cmd> [args]` → fall through to the daemon dispatch.
            Some(spec) if spec.contains('/') => {}
            _ => {
                eprintln!(
                    "{} usage: chrome-use site <name>/<cmd> [args] | site update | site list | \
                     site info <name>/<cmd>",
                    color::error_indicator()
                );
                exit(2);
            }
        }
    }

    // Handle skills command (doesn't need daemon)
    if clean.first().map(|s| s.as_str()) == Some("skills") {
        skills::run_skills(&clean, flags.json);
        return;
    }

    // Handle find-url (doesn't need daemon): search local bookmarks
    if matches!(
        clean.first().map(|s| s.as_str()),
        Some("find-url") | Some("findurl")
    ) {
        findurl::run_find_url(&clean, flags.json);
        return;
    }

    // `friction` (no daemon): aggregate the local friction log — what's been
    // painful to drive. Data for the next round of features.
    if clean.first().map(|s| s.as_str()) == Some("friction") {
        friction::run_friction(&clean[1..], flags.json);
        return;
    }

    // `browsers` (no daemon): list the connected Chrome profiles so an agent can
    // pin a session to one with `--browser <id|email>` (issue #60).
    if clean.first().map(|s| s.as_str()) == Some("browsers") {
        connect::run_browsers(flags.json);
        return;
    }

    // `reconnect` is a friendly alias for `extension connect` (issue #58): re-bind
    // the session to the running Chrome's relay without any reinstall. Rewrite it
    // into `extension connect …` (preserving any flags like --silent) and let the
    // block below handle it.
    if clean.first().map(|s| s.as_str()) == Some("reconnect") {
        let mut rebuilt = vec!["extension".to_string(), "connect".to_string()];
        rebuilt.extend(clean.into_iter().skip(1));
        clean = rebuilt;
    }

    // Handle extension: native-messaging host install/status, and
    // `extension connect` which attaches to the live relay (auto-discovers the
    // CDP url the host wrote) by rewriting into the normal `connect <url>` flow.
    // (`connect <port>` stays the plain CDP-attach command.)
    if clean.first().map(|s| s.as_str()) == Some("extension") {
        if clean.get(1).map(|s| s.as_str()) == Some("connect") {
            // Optionally silence Chrome's `chrome.debugger` "started debugging
            // this browser" banner by cold-relaunching the user's Chrome with
            // --silent-debugger-extension-api. Default (Auto) only restarts after
            // an interactive confirm; `--silent` forces it, `--keep-banner` skips.
            let silence_mode = if clean.iter().any(|a| a == "--keep-banner") {
                silence::SilenceMode::Off
            } else if clean.iter().any(|a| a == "--silent") {
                silence::SilenceMode::Force
            } else {
                silence::SilenceMode::Auto
            };
            if silence_mode != silence::SilenceMode::Off {
                match silence::ensure_banner_silenced(silence_mode) {
                    silence::SilenceOutcome::Restarted => {
                        // Chrome dropped the relay on quit; wait for ab-connect
                        // to respawn the native host and rewrite its CDP url.
                        eprint!(
                            "{} Chrome restarted; waiting for the extension relay to reconnect…",
                            color::success_indicator()
                        );
                        let _ = std::io::Write::flush(&mut std::io::stderr());
                        let deadline =
                            std::time::Instant::now() + std::time::Duration::from_secs(25);
                        while connect::relay_url().is_none() && std::time::Instant::now() < deadline
                        {
                            std::thread::sleep(std::time::Duration::from_millis(500));
                        }
                        eprintln!();
                    }
                    silence::SilenceOutcome::Failed(e) => {
                        eprintln!(
                            "{} could not silence the debugging banner: {e}",
                            color::warning_indicator()
                        );
                    }
                    silence::SilenceOutcome::Ambiguous(n) => {
                        eprintln!(
                            "{} {n} Chrome instances are running — not auto-restarting (quitting \
                             would close all of them). Quit the extra Chrome instances and retry, \
                             or launch Chrome with --silent-debugger-extension-api yourself.",
                            color::warning_indicator()
                        );
                    }
                    // AlreadySilent / NotRunning / Declined → proceed as before.
                    _ => {}
                }
            }
            // The relay may not be up the instant we ask: after a fresh host
            // install or an MV3 service-worker sleep, the extension reconnects on
            // its keepalive (~30s, allow up to ~45s) and only THEN writes its CDP
            // url. Failing instantly here is exactly what misled users into
            // quitting/restarting Chrome — verified locally that a running Chrome
            // picks the host back up on its own, no restart needed. So if the host
            // is registered, register-to-be-safe and poll for the relay to come up.
            if connect::relay_url().is_none() && crate::connect::host_installed() {
                crate::connect::ensure_host_installed();
                eprint!(
                    "{} extension relay reconnecting (the worker wakes ~every 30s)…",
                    color::success_indicator()
                );
                let _ = std::io::Write::flush(&mut std::io::stderr());
                let deadline = std::time::Instant::now() + std::time::Duration::from_secs(45);
                while connect::relay_url().is_none() && std::time::Instant::now() < deadline {
                    std::thread::sleep(std::time::Duration::from_millis(750));
                }
                eprintln!();
            }
            match connect::relay_url() {
                Some(url) => {
                    // The connect path reads `flags.cdp` (parsed from the original
                    // argv, which was `extension connect` → None), NOT `clean`.
                    // Without this the relay URL is dropped and we fall through to
                    // auto-connect, grabbing some other Chrome (stale :9222) or
                    // popping the remote-debug prompt. Point the daemon at the
                    // relay explicitly.
                    flags.cdp = Some(url.clone());
                    flags.auto_connect = false;
                    clean = vec!["connect".to_string(), url];
                }
                None if !crate::connect::host_installed() => {
                    // Host not set up → register it + open the Store page (one
                    // click). Never the dev-mode "Load unpacked" lecture.
                    crate::connect::ensure_host_installed();
                    crate::connect::open_url(crate::connect::STORE_URL);
                    eprintln!("{}", crate::connect::extension_not_installed_message());
                    exit(1);
                }
                None => {
                    // Extension IS set up — the worker just hasn't reconnected yet.
                    // Accurate guidance: retry, or reload ONLY the extension. NEVER
                    // "restart Chrome" (a running Chrome picks the host up on its
                    // own — verified) and never dev-mode "Load unpacked".
                    eprintln!(
                        "{} The chrome-use extension is installed, but its background worker \
                         hasn't reconnected to the native host yet (MV3 workers sleep and wake \
                         on a ~30s timer). This usually clears on its own within ~30–60s — just \
                         re-run this command. To force it immediately, reload ONLY the chrome-use \
                         extension at chrome://extensions (the ↻ reload icon). A full Chrome \
                         restart is NOT required.",
                        color::error_indicator()
                    );
                    exit(1);
                }
            }
        } else {
            connect::run_connect(&clean, flags.json);
            return;
        }
    }

    // `adopt <url|targetId>`: read a PRE-EXISTING tab (the user's own, or another
    // session's) WITHOUT opening a new one. Forces a fresh daemon and points it at
    // the relay (like `extension connect`); the AGENT_BROWSER_ADOPT env makes the
    // daemon's first connect ADOPT the matching tab instead of creating an
    // about:blank. Rewrites into `connect <relay-url>` BEFORE parse_command so the
    // daemon attaches to the user's real Chrome. Must run before parse_command.
    if clean.first().map(|s| s.as_str()) == Some("adopt") {
        match clean.get(1) {
            Some(spec) if !spec.trim().is_empty() => {
                std::env::set_var("AGENT_BROWSER_ADOPT", spec.trim());
                connection::kill_stale_daemon(&flags.session);
                // Same as `extension connect`: the worker may be mid-reconnect, so
                // wait for the relay to come up instead of failing instantly (which
                // misled users into restarting Chrome).
                if connect::relay_url().is_none() && crate::connect::host_installed() {
                    crate::connect::ensure_host_installed();
                    eprint!(
                        "{} extension relay reconnecting (the worker wakes ~every 30s)…",
                        color::success_indicator()
                    );
                    let _ = std::io::Write::flush(&mut std::io::stderr());
                    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(45);
                    while connect::relay_url().is_none() && std::time::Instant::now() < deadline {
                        std::thread::sleep(std::time::Duration::from_millis(750));
                    }
                    eprintln!();
                }
                match connect::relay_url() {
                    Some(url) => {
                        flags.cdp = Some(url.clone());
                        flags.auto_connect = false;
                        clean = vec!["connect".to_string(), url];
                    }
                    None if !crate::connect::host_installed() => {
                        crate::connect::ensure_host_installed();
                        crate::connect::open_url(crate::connect::STORE_URL);
                        eprintln!("{}", crate::connect::extension_not_installed_message());
                        exit(1);
                    }
                    None => {
                        eprintln!(
                            "{} The chrome-use extension is installed but its background worker \
                             hasn't reconnected to the native host yet (MV3 workers sleep, ~30s \
                             wake timer). Re-run this in a moment, or reload ONLY the chrome-use \
                             extension at chrome://extensions (↻). A Chrome restart is NOT needed.",
                            color::error_indicator()
                        );
                        exit(1);
                    }
                }
            }
            _ => {
                eprintln!(
                    "{} usage: chrome-use adopt <url-substring|targetId>  (reads an existing tab, no new tab)",
                    color::error_indicator()
                );
                exit(2);
            }
        }
    }

    // `--browser <id|email-substr>` (issue #60): pin this session to a specific
    // connected Chrome profile by resolving to that profile's stable relay
    // endpoint and connecting the daemon to it. Session-sticky: the per-session
    // daemon binds to this endpoint on its first connect and keeps it for life
    // (a different session can pick a different profile — no global state, so
    // concurrent agents don't fight). To switch a *running* session's profile,
    // start a fresh `--session` (or close it first).
    if let Some(sel) = flags.browser.clone() {
        match connect::relay_url_for_browser(&sel) {
            Ok(url) => {
                flags.cdp = Some(url);
                flags.auto_connect = false;
            }
            Err(msg) => {
                eprintln!("{} {msg}", color::error_indicator());
                exit(1);
            }
        }
    }

    // Handle session separately (doesn't need daemon)
    if clean.first().map(|s| s.as_str()) == Some("session") {
        run_session(&clean, &flags.session, flags.json);
        return;
    }

    // Handle daemon management (doesn't talk to a daemon — it manages them).
    if clean.first().map(|s| s.as_str()) == Some("daemon") {
        run_daemon(&clean, flags.json);
        return;
    }

    // `sessions` is a natural top-level guess for "list my sessions" (the skill
    // advertises sessions as a feature) — route it to the daemon inventory the
    // same way `daemon status` does (issue #29).
    if clean.first().map(|s| s.as_str()) == Some("sessions") {
        run_daemon(&["sessions".to_string(), "status".to_string()], flags.json);
        return;
    }

    // Handle close --all: close all active sessions
    if matches!(
        clean.first().map(|s| s.as_str()),
        Some("close") | Some("quit") | Some("exit")
    ) && clean.iter().any(|a| a == "--all")
    {
        run_close_all(&flags);
        return;
    }

    // Handle chat command
    if clean.first().map(|s| s.as_str()) == Some("chat") {
        let message = if clean.len() > 1 {
            Some(clean[1..].join(" "))
        } else {
            None
        };
        chat::run_chat(&flags, message);
        return;
    }

    let mut cmd = match parse_command(&clean, &flags) {
        Ok(c) => c,
        Err(e) => {
            if flags.json {
                let error_type = match &e {
                    ParseError::UnknownCommand { .. } => "unknown_command",
                    ParseError::UnknownSubcommand { .. } => "unknown_subcommand",
                    ParseError::MissingArguments { .. } => "missing_arguments",
                    ParseError::InvalidValue { .. } => "invalid_value",
                    ParseError::InvalidSessionName { .. } => "invalid_session_name",
                };
                print_json_error_with_type(e.format(), error_type);
            } else {
                eprintln!("{}", color::red(&e.format()));
            }
            exit(1);
        }
    };

    // Handle --password-stdin for auth save
    if cmd.get("action").and_then(|v| v.as_str()) == Some("auth_save") {
        if cmd.get("password").is_some() {
            eprintln!(
                "{} Passwords on the command line may be visible in process listings and shell history. Use --password-stdin instead.",
                color::warning_indicator()
            );
        }
        if cmd
            .get("passwordStdin")
            .and_then(|v| v.as_bool())
            .unwrap_or(false)
        {
            let mut pass = String::new();
            if std::io::stdin().read_line(&mut pass).is_err() || pass.is_empty() {
                eprintln!(
                    "{} Failed to read password from stdin",
                    color::error_indicator()
                );
                exit(1);
            }
            let pass = pass.trim_end_matches('\n').trim_end_matches('\r');
            if pass.is_empty() {
                eprintln!("{} Password from stdin is empty", color::error_indicator());
                exit(1);
            }
            cmd["password"] = json!(pass);
            cmd.as_object_mut().unwrap().remove("passwordStdin");
        }
    }

    // Validate session name before starting daemon
    if let Some(ref name) = flags.session_name {
        if !validation::is_valid_session_name(name) {
            let msg = validation::session_name_error(name);
            if flags.json {
                print_json_error_with_type(msg, "invalid_session_name");
            } else {
                eprintln!("{} {}", color::error_indicator(), msg);
            }
            exit(1);
        }
    }

    // Handle state management commands locally — these are pure file operations
    // that don't need a daemon, avoiding an unnecessary daemon startup that
    // would lack runtime config like session_name.
    if let Some(result) = native::state::dispatch_state_command(&cmd) {
        let action = cmd.get("action").and_then(|v| v.as_str());
        let resp = match result {
            Ok(data) => connection::Response {
                success: true,
                data: Some(data),
                error: None,
                warning: None,
            },
            Err(e) => connection::Response {
                success: false,
                data: None,
                error: Some(e),
                warning: None,
            },
        };
        let output_opts = OutputOptions::from_flags(&flags);
        output::print_response_with_opts(&resp, action, &output_opts);
        if !resp.success {
            exit(1);
        }
        return;
    }

    // Relay self-heal (the "用不了" fix). On the extension-relay path, a dropped
    // relay used to mean either a 2-minute hang (a stale daemon still bound to the
    // dead relay ws keeps sending into the void) or a hard error that forced the
    // user to run `chrome-use reconnect` by hand. Instead, when we're about to
    // drive the user's real Chrome and the relay is down (host installed but
    // `relay-cdp-url` gone), recover automatically: drop the stale daemon so it
    // can't reuse the dead binding, then wait (bounded, with progress) for the MV3
    // worker to republish the relay — the fresh daemon then connects clean. Opt
    // out with AGENT_BROWSER_NO_AUTO_RECONNECT. Skipped for --launch/--cdp.
    if flags.auto_connect
        && flags.cdp.is_none()
        && !flags.force_launch
        && std::env::var("AGENT_BROWSER_NO_AUTO_RECONNECT").is_err()
        && connect::host_installed()
        && connect::relay_url().is_none()
        // Don't disturb a session that already has a healthy daemon — e.g. one
        // driving a `--launch`ed browser (its follow-up commands omit --launch and
        // would otherwise trip this relay-down branch and get the daemon killed). A
        // daemon stuck on a dead relay fails this probe (hangs → times out) and is
        // healed; a live launched browser answers fast and is left alone.
        && !connection::probe_daemon_healthy(&flags.session, std::time::Duration::from_secs(3))
    {
        connection::kill_stale_daemon(&flags.session);
        connect::ensure_host_installed();
        // Reap any zombie native-messaging host (it already lost its Chrome port —
        // relay-cdp-url is gone — but the process can linger). Killing it makes the
        // extension worker's port disconnect fire immediately, so it reconnects and
        // republishes the relay in ~2s instead of waiting ~30s for the keepalive
        // alarm. Best-effort, unix-only; safe here because the relay is already down.
        #[cfg(unix)]
        {
            let _ = std::process::Command::new("pkill")
                .args(["-f", "__nm-host"])
                .output();
        }
        eprint!(
            "{} Chrome relay dropped — reconnecting…",
            color::success_indicator()
        );
        let _ = std::io::Write::flush(&mut std::io::stderr());
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(45);
        while connect::relay_url().is_none() && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(300));
        }
        eprintln!();
    }

    // Parse proxy URL to separate server from credentials for the daemon.
    let (proxy_server, proxy_username, proxy_password) = if let Some(ref proxy_str) = flags.proxy {
        let parsed = parse_proxy(proxy_str);
        (Some(parsed.server), parsed.username, parsed.password)
    } else {
        (None, None, None)
    };
    let daemon_opts = DaemonOptions {
        headed: flags.headed,
        debug: flags.debug,
        executable_path: flags.executable_path.as_deref(),
        extensions: &flags.extensions,
        init_scripts: &flags.init_scripts,
        enable: &flags.enable,
        args: flags.args.as_deref(),
        user_agent: flags.user_agent.as_deref(),
        proxy: proxy_server.as_deref(),
        proxy_bypass: flags.proxy_bypass.as_deref(),
        proxy_username: proxy_username.as_deref(),
        proxy_password: proxy_password.as_deref(),
        ignore_https_errors: flags.ignore_https_errors,
        allow_file_access: flags.allow_file_access,
        hide_scrollbars: flags.hide_scrollbars,
        profile: flags.profile.as_deref(),
        state: flags.state.as_deref(),
        provider: flags.provider.as_deref(),
        device: flags.device.as_deref(),
        session_name: flags.session_name.as_deref(),
        download_path: flags.download_path.as_deref(),
        allowed_domains: flags.allowed_domains.as_deref(),
        action_policy: flags.action_policy.as_deref(),
        confirm_actions: flags.confirm_actions.as_deref(),
        engine: flags.engine.as_deref(),
        auto_connect: flags.auto_connect,
        force_launch: flags.force_launch,
        idle_timeout: flags.idle_timeout.as_deref(),
        default_timeout: flags.default_timeout,
        cdp: flags.cdp.as_deref(),
        no_auto_dialog: flags.no_auto_dialog,
    };

    let daemon_result = match ensure_daemon(&flags.session, &daemon_opts) {
        Ok(result) => result,
        Err(e) => {
            if flags.json {
                print_json_error(e);
            } else {
                eprintln!("{} {}", color::error_indicator(), e);
            }
            exit(1);
        }
    };

    // Warn if launch-time options were explicitly passed via CLI but daemon was already running
    // Only warn about flags that were passed on the command line, not those set via environment
    // variables (since the daemon already uses the env vars when it starts).
    if daemon_result.already_running {
        let ignored_flags: Vec<&str> = [
            if flags.cli_executable_path {
                Some("--executable-path")
            } else {
                None
            },
            if flags.cli_extensions {
                Some("--extension")
            } else {
                None
            },
            if flags.cli_profile {
                Some("--profile")
            } else {
                None
            },
            if flags.cli_state {
                Some("--state")
            } else {
                None
            },
            if flags.cli_args { Some("--args") } else { None },
            if flags.cli_user_agent {
                Some("--user-agent")
            } else {
                None
            },
            if flags.cli_proxy {
                Some("--proxy")
            } else {
                None
            },
            if flags.cli_proxy_bypass {
                Some("--proxy-bypass")
            } else {
                None
            },
            flags.ignore_https_errors.then_some("--ignore-https-errors"),
            flags.cli_allow_file_access.then_some("--allow-file-access"),
            flags.cli_hide_scrollbars.then_some("--hide-scrollbars"),
            flags.cli_download_path.then_some("--download-path"),
            flags.cli_headed.then_some("--headed"),
        ]
        .into_iter()
        .flatten()
        .collect();

        if !ignored_flags.is_empty() && !flags.json {
            // Special case: --headed is irrelevant in CDP-attach mode
            // (your existing Chrome is always already visible). The
            // "chrome-use close + reopen" advice doesn't help because
            // the new daemon will attach right back to the same Chrome.
            // Don't suggest a useless workaround.
            if ignored_flags == ["--headed"] {
                eprintln!(
                    "{} --headed has no effect when attached to your running Chrome (it's already visible). \
                     Pass --launch to spawn a separate browser if you need to control headedness.",
                    color::warning_indicator(),
                );
            } else {
                eprintln!(
                    "{} {} ignored: daemon already running. Use 'chrome-use close' first to restart with new options.",
                    color::warning_indicator(),
                    ignored_flags.join(", ")
                );
            }
        }
    }

    // Validate mutually exclusive options
    if flags.cdp.is_some() && flags.provider.is_some() {
        let msg = "Cannot use --cdp and -p/--provider together";
        if flags.json {
            print_json_error(msg);
        } else {
            eprintln!("{} {}", color::error_indicator(), msg);
        }
        exit(1);
    }

    // Explicit --cdp or --provider disables auto-connect (they specify the connection)
    if flags.cdp.is_some() || flags.provider.is_some() {
        flags.auto_connect = false;
    }

    if flags.provider.is_some() && !flags.extensions.is_empty() {
        let msg = "Cannot use --extension with -p/--provider (extensions require local browser)";
        if flags.json {
            print_json_error(msg);
        } else {
            eprintln!("{} {}", color::error_indicator(), msg);
        }
        exit(1);
    }

    if flags.cdp.is_some() && !flags.extensions.is_empty() {
        let msg = "Cannot use --extension with --cdp (extensions require local browser)";
        if flags.json {
            print_json_error(msg);
        } else {
            eprintln!("{} {}", color::error_indicator(), msg);
        }
        exit(1);
    }

    // Auto-connect to existing browser.
    // Skip when the daemon was already running — it already holds the connection
    // from a previous auto-connect launch, so re-sending the launch command would
    // redundantly probe Chrome and may trigger repeated permission prompts (#962).
    if flags.auto_connect && !daemon_result.already_running {
        let mut launch_cmd = json!({
            "id": gen_id(),
            "action": "launch",
            "autoConnect": true
        });

        if flags.ignore_https_errors {
            launch_cmd["ignoreHTTPSErrors"] = json!(true);
        }

        if let Some(ref cs) = flags.color_scheme {
            launch_cmd["colorScheme"] = json!(cs);
        }

        if let Some(ref dp) = flags.download_path {
            launch_cmd["downloadPath"] = json!(dp);
        }

        let err = match send_command(launch_cmd, &flags.session) {
            Ok(resp) if resp.success => None,
            Ok(resp) => Some(
                resp.error
                    .unwrap_or_else(|| "Auto-connect failed".to_string()),
            ),
            Err(e) => Some(e.to_string()),
        };

        if let Some(msg) = err {
            if flags.json {
                print_json_error(msg);
            } else {
                eprintln!("{} {}", color::error_indicator(), msg);
            }
            exit(1);
        }
    }

    // Connect via CDP if --cdp flag is set
    // Accepts either a port number (e.g., "9222") or a full URL (e.g., "ws://..." or "wss://...")
    // Skip when daemon already running — it already holds the CDP connection.
    if let Some(ref cdp_value) = flags.cdp {
        // Validate CDP value eagerly (even when daemon is already running) so
        // the user gets an immediate error for bad input instead of a silent no-op.
        let launch_cmd = if cdp_value.starts_with("ws://")
            || cdp_value.starts_with("wss://")
            || cdp_value.starts_with("http://")
            || cdp_value.starts_with("https://")
        {
            // It's a URL - use cdpUrl field
            json!({
                "id": gen_id(),
                "action": "launch",
                "cdpUrl": cdp_value
            })
        } else {
            // It's a port number - validate and use cdpPort field
            let cdp_port: u16 = match cdp_value.parse::<u32>() {
                Ok(0) => {
                    let msg = "Invalid CDP port: port must be greater than 0".to_string();
                    if flags.json {
                        print_json_error(&msg);
                    } else {
                        eprintln!("{} {}", color::error_indicator(), msg);
                    }
                    exit(1);
                }
                Ok(p) if p > 65535 => {
                    let msg = format!(
                        "Invalid CDP port: {} is out of range (valid range: 1-65535)",
                        p
                    );
                    if flags.json {
                        print_json_error(&msg);
                    } else {
                        eprintln!("{} {}", color::error_indicator(), msg);
                    }
                    exit(1);
                }
                Ok(p) => p as u16,
                Err(_) => {
                    let msg = format!(
                        "Invalid CDP value: '{}' is not a valid port number or URL",
                        cdp_value
                    );
                    if flags.json {
                        print_json_error(&msg);
                    } else {
                        eprintln!("{} {}", color::error_indicator(), msg);
                    }
                    exit(1);
                }
            };
            json!({
                "id": gen_id(),
                "action": "launch",
                "cdpPort": cdp_port
            })
        };

        if !daemon_result.already_running {
            let mut launch_cmd = launch_cmd;

            if flags.ignore_https_errors {
                launch_cmd["ignoreHTTPSErrors"] = json!(true);
            }

            if let Some(ref cs) = flags.color_scheme {
                launch_cmd["colorScheme"] = json!(cs);
            }

            if let Some(ref dp) = flags.download_path {
                launch_cmd["downloadPath"] = json!(dp);
            }

            let err = match send_command(launch_cmd, &flags.session) {
                Ok(resp) if resp.success => None,
                Ok(resp) => Some(
                    resp.error
                        .unwrap_or_else(|| "CDP connection failed".to_string()),
                ),
                Err(e) => Some(e.to_string()),
            };

            if let Some(msg) = err {
                if flags.json {
                    print_json_error(msg);
                } else {
                    eprintln!("{} {}", color::error_indicator(), msg);
                }
                exit(1);
            }
        }
    }

    // Launch with cloud provider if -p flag is set
    // Skip when daemon already running — it already holds the provider connection.
    if let Some(ref provider) = flags.provider {
        if !daemon_result.already_running {
            let mut launch_cmd = json!({
                "id": gen_id(),
                "action": "launch",
                "provider": provider
            });

            if let Some(ref cs) = flags.color_scheme {
                launch_cmd["colorScheme"] = json!(cs);
            }

            let err = match send_command(launch_cmd, &flags.session) {
                Ok(resp) if resp.success => None,
                Ok(resp) => Some(
                    resp.error
                        .unwrap_or_else(|| "Provider connection failed".to_string()),
                ),
                Err(e) => Some(e.to_string()),
            };

            if let Some(msg) = err {
                if flags.json {
                    print_json_error(msg);
                } else {
                    eprintln!("{} {}", color::error_indicator(), msg);
                }
                exit(1);
            }
        }
    }

    // Launch headed browser or configure browser options (without CDP or provider)
    if (flags.headed
        || flags.cli_headed  // User explicitly set --headed (even if false)
        || flags.executable_path.is_some()
        || flags.profile.is_some()
        || flags.state.is_some()
        || flags.proxy.is_some()
        || flags.args.is_some()
        || flags.user_agent.is_some()
        || flags.allow_file_access
        || should_send_hide_scrollbars_launch_option(
            flags.cli_hide_scrollbars,
            flags.hide_scrollbars,
        )
        || flags.color_scheme.is_some()
        || flags.download_path.is_some()
        || flags.engine.is_some()
        || !flags.extensions.is_empty())
        && flags.cdp.is_none()
        && flags.provider.is_none()
        && (flags.force_launch || !flags.auto_connect)
    {
        // Launching a debug-port Chrome pops Chrome's "Allow remote debugging?"
        // consent modal (Chrome 136+). When the ab-connect relay is already up,
        // this is almost always unintended — the relay drives the user's real
        // Chrome with NO modal. Warn so the modal is self-explained and the
        // caller (often a stray --launch / --no-auto-connect) is fixable (#32).
        if !flags.json && connect::relay_url().is_some() {
            eprintln!(
                "{} launching a new Chrome with a debug port — this pops Chrome's \
                 \"Allow remote debugging?\" modal.\n  The ab-connect relay is up; \
                 drop --launch/--new (and don't pass --no-auto-connect) to drive your \
                 real Chrome with no modal.",
                color::warning_indicator()
            );
        }
        let mut launch_cmd = json!({
            "id": gen_id(),
            "action": "launch",
            "headless": !flags.headed
        });

        let cmd_obj = launch_cmd
            .as_object_mut()
            .expect("json! macro guarantees object type");

        // Add executable path if specified
        if let Some(ref exec_path) = flags.executable_path {
            cmd_obj.insert("executablePath".to_string(), json!(exec_path));
        }

        // Add profile path if specified
        if let Some(ref profile_path) = flags.profile {
            cmd_obj.insert("profile".to_string(), json!(profile_path));
        }

        // Add state path if specified
        if let Some(ref state_path) = flags.state {
            cmd_obj.insert("storageState".to_string(), json!(state_path));
        }

        if let Some(ref proxy_str) = flags.proxy {
            let parsed = parse_proxy(proxy_str);
            let mut proxy_obj = json!({ "server": parsed.server });
            if let Some(ref username) = parsed.username {
                proxy_obj["username"] = json!(username);
            }
            if let Some(ref password) = parsed.password {
                proxy_obj["password"] = json!(password);
            }
            if let Some(ref bypass) = flags.proxy_bypass {
                proxy_obj["bypass"] = json!(bypass);
            }
            cmd_obj.insert("proxy".to_string(), proxy_obj);
        }

        if let Some(ref ua) = flags.user_agent {
            cmd_obj.insert("userAgent".to_string(), json!(ua));
        }

        if let Some(ref a) = flags.args {
            // Parse args (comma or newline separated)
            let args_vec: Vec<String> = a
                .split(&[',', '\n'][..])
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect();
            cmd_obj.insert("args".to_string(), json!(args_vec));
        }

        if !flags.extensions.is_empty() {
            cmd_obj.insert("extensions".to_string(), json!(&flags.extensions));
        }

        if flags.ignore_https_errors {
            launch_cmd["ignoreHTTPSErrors"] = json!(true);
        }

        if flags.allow_file_access {
            launch_cmd["allowFileAccess"] = json!(true);
        }

        apply_hide_scrollbars_launch_option(
            &mut launch_cmd,
            flags.cli_hide_scrollbars,
            flags.hide_scrollbars,
        );

        if let Some(ref cs) = flags.color_scheme {
            launch_cmd["colorScheme"] = json!(cs);
        }

        if let Some(ref dp) = flags.download_path {
            launch_cmd["downloadPath"] = json!(dp);
        }

        if let Some(ref domains) = flags.allowed_domains {
            launch_cmd["allowedDomains"] = json!(domains);
        }

        if let Some(ref engine) = flags.engine {
            launch_cmd["engine"] = json!(engine);
        }

        match send_command(launch_cmd, &flags.session) {
            Ok(resp) if !resp.success => {
                // Launch command failed (e.g., invalid state file, profile error)
                let error_msg = resp
                    .error
                    .unwrap_or_else(|| "Browser launch failed".to_string());
                if flags.json {
                    print_json_error(error_msg);
                } else {
                    eprintln!("{} {}", color::error_indicator(), error_msg);
                }
                exit(1);
            }
            Err(e) => {
                if flags.json {
                    print_json_error(e);
                } else {
                    eprintln!(
                        "{} Could not configure browser: {}",
                        color::error_indicator(),
                        e
                    );
                }
                exit(1);
            }
            Ok(_) => {
                // Launch succeeded
            }
        }
    }

    // Handle batch command: from args or stdin
    if cmd.get("action").and_then(|v| v.as_str()) == Some("batch") {
        let bail = cmd.get("bail").and_then(|v| v.as_bool()).unwrap_or(false);
        let arg_commands = cmd.get("commands").and_then(|v| v.as_array()).map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str())
                .map(commands::shell_words_split)
                .collect::<Vec<Vec<String>>>()
        });
        run_batch(&flags, bail, arg_commands);
        return;
    }

    let output_opts = OutputOptions::from_flags(&flags);

    match send_command(cmd.clone(), &flags.session) {
        Ok(resp) => {
            let success = resp.success;
            // Handle interactive confirmation
            if flags.confirm_interactive {
                if let Some(data) = &resp.data {
                    if data
                        .get("confirmation_required")
                        .and_then(|v| v.as_bool())
                        .unwrap_or(false)
                    {
                        let desc = data
                            .get("description")
                            .and_then(|v| v.as_str())
                            .unwrap_or("unknown action");
                        let category = data.get("category").and_then(|v| v.as_str()).unwrap_or("");
                        let cid = data
                            .get("confirmation_id")
                            .and_then(|v| v.as_str())
                            .unwrap_or("");

                        eprintln!("[chrome-use] Action requires confirmation:");
                        eprintln!("  {}: {}", category, desc);
                        eprint!("  Allow? [y/N]: ");

                        let mut input = String::new();
                        let approved = if std::io::IsTerminal::is_terminal(&std::io::stdin()) {
                            std::io::stdin().read_line(&mut input).is_ok()
                                && matches!(input.trim().to_lowercase().as_str(), "y" | "yes")
                        } else {
                            false
                        };

                        let confirm_cmd = if approved {
                            json!({ "id": gen_id(), "action": "confirm", "confirmationId": cid })
                        } else {
                            json!({ "id": gen_id(), "action": "deny", "confirmationId": cid })
                        };

                        match send_command(confirm_cmd, &flags.session) {
                            Ok(r) => {
                                if !approved {
                                    eprintln!("{} Action denied", color::error_indicator());
                                    exit(1);
                                }
                                print_response_with_opts(&r, None, &output_opts);
                            }
                            Err(e) => {
                                eprintln!("{} {}", color::error_indicator(), e);
                                exit(1);
                            }
                        }
                        return;
                    }
                }
            }
            // Extract action for context-specific output handling
            let action = cmd.get("action").and_then(|v| v.as_str());
            print_response_with_opts(&resp, action, &output_opts);
            // `expect` is an assertion: map to a 3-way exit code so it composes in
            // shells/CI — 0 pass, 1 condition false, 2 un-evaluable (transport
            // error: no browser / bad grammar). Must run before the generic
            // `!success → exit(1)` below.
            if action == Some("expect") {
                if !success {
                    exit(2);
                }
                let pass = resp
                    .data
                    .as_ref()
                    .and_then(|d| d.get("pass"))
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                exit(if pass { 0 } else { 1 });
            }
            if !success {
                exit(1);
            }
        }
        Err(e) => {
            if flags.json {
                print_json_error(e);
            } else {
                eprintln!("{} {}", color::error_indicator(), e);
            }
            exit(1);
        }
    }
}

fn run_batch(flags: &Flags, bail: bool, arg_commands: Option<Vec<Vec<String>>>) {
    let commands: Vec<Vec<String>> = if let Some(cmds) = arg_commands {
        cmds
    } else {
        use std::io::Read as _;

        let mut input = String::new();
        if let Err(e) = std::io::stdin().read_to_string(&mut input) {
            if flags.json {
                print_json_error(format!("Failed to read stdin: {}", e));
            } else {
                eprintln!("{} Failed to read stdin: {}", color::error_indicator(), e);
            }
            exit(1);
        }

        match serde_json::from_str(&input) {
            Ok(c) => c,
            Err(e) => {
                if flags.json {
                    print_json_error(format!(
                        "Invalid JSON input: {}. Expected an array of string arrays, e.g. [[\"open\", \"https://example.com\"], [\"snapshot\"]]",
                        e
                    ));
                } else {
                    eprintln!(
                        "{} Invalid JSON input: {}. Expected an array of string arrays.",
                        color::error_indicator(),
                        e
                    );
                }
                exit(1);
            }
        }
    };

    if commands.is_empty() {
        if flags.json {
            println!("[]");
        }
        return;
    }

    let output_opts = OutputOptions::from_flags(flags);

    let mut results: Vec<serde_json::Value> = Vec::new();
    let mut had_error = false;

    for (i, cmd_args) in commands.iter().enumerate() {
        if cmd_args.is_empty() {
            continue;
        }

        let parsed = match parse_command(cmd_args, flags) {
            Ok(c) => c,
            Err(e) => {
                had_error = true;
                if flags.json {
                    results.push(json!({
                        "command": cmd_args,
                        "success": false,
                        "error": e.format(),
                    }));
                    if bail {
                        break;
                    }
                } else {
                    eprintln!(
                        "{} Command {}: {}",
                        color::error_indicator(),
                        i + 1,
                        e.format()
                    );
                    if bail {
                        exit(1);
                    }
                }
                continue;
            }
        };

        let action = parsed
            .get("action")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());

        match send_command(parsed, &flags.session) {
            Ok(resp) => {
                if flags.json {
                    results.push(json!({
                        "command": cmd_args,
                        "success": resp.success,
                        "result": resp.data,
                        "error": resp.error,
                    }));
                } else {
                    if i > 0 {
                        println!();
                    }
                    print_response_with_opts(&resp, action.as_deref(), &output_opts);
                }
                if !resp.success {
                    had_error = true;
                    if bail {
                        if !flags.json {
                            exit(1);
                        }
                        break;
                    }
                }
            }
            Err(e) => {
                had_error = true;
                if flags.json {
                    results.push(json!({
                        "command": cmd_args,
                        "success": false,
                        "error": e.to_string(),
                    }));
                    if bail {
                        break;
                    }
                } else {
                    eprintln!("{} Command {}: {}", color::error_indicator(), i + 1, e);
                    if bail {
                        exit(1);
                    }
                }
            }
        }
    }

    if flags.json {
        println!(
            "{}",
            serde_json::to_string(&results).unwrap_or_else(|_| "[]".to_string())
        );
    }

    if had_error {
        exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_proxy_simple() {
        let result = parse_proxy("http://proxy.com:8080");
        assert_eq!(result.server, "http://proxy.com:8080");
        assert!(result.username.is_none());
        assert!(result.password.is_none());
    }

    #[test]
    fn test_parse_proxy_with_auth() {
        let result = parse_proxy("http://user:pass@proxy.com:8080");
        assert_eq!(result.server, "http://proxy.com:8080");
        assert_eq!(result.username.as_deref(), Some("user"));
        assert_eq!(result.password.as_deref(), Some("pass"));
    }

    #[test]
    fn test_parse_proxy_username_only() {
        let result = parse_proxy("http://user@proxy.com:8080");
        assert_eq!(result.server, "http://proxy.com:8080");
        assert_eq!(result.username.as_deref(), Some("user"));
        assert!(result.password.is_none());
    }

    #[test]
    fn test_parse_proxy_no_protocol() {
        let result = parse_proxy("proxy.com:8080");
        assert_eq!(result.server, "proxy.com:8080");
        assert!(result.username.is_none());
    }

    #[test]
    fn test_parse_proxy_socks5() {
        let result = parse_proxy("socks5://proxy.com:1080");
        assert_eq!(result.server, "socks5://proxy.com:1080");
        assert!(result.username.is_none());
    }

    #[test]
    fn test_parse_proxy_socks5_with_auth() {
        let result = parse_proxy("socks5://admin:secret@proxy.com:1080");
        assert_eq!(result.server, "socks5://proxy.com:1080");
        assert_eq!(result.username.as_deref(), Some("admin"));
        assert_eq!(result.password.as_deref(), Some("secret"));
    }

    #[test]
    fn test_parse_proxy_complex_password() {
        let result = parse_proxy("http://user:p@ss:w0rd@proxy.com:8080");
        assert_eq!(result.server, "http://proxy.com:8080");
        assert_eq!(result.username.as_deref(), Some("user"));
        assert_eq!(result.password.as_deref(), Some("p@ss:w0rd"));
    }

    #[test]
    fn test_serialize_json_value_escapes_control_characters() {
        let payload = serialize_json_value(&json!({
            "success": false,
            "error": "Daemon process exited during startup:\nline \"quoted\"\u{001b}[2mansi\u{001b}[22m",
        }));

        let parsed: serde_json::Value = serde_json::from_str(&payload).unwrap();
        assert_eq!(parsed["success"], false);
        assert_eq!(
            parsed["error"],
            "Daemon process exited during startup:\nline \"quoted\"\u{001b}[2mansi\u{001b}[22m"
        );
    }

    #[test]
    fn test_hide_scrollbars_launch_option_serialization() {
        assert!(!should_send_hide_scrollbars_launch_option(false, true));
        assert!(should_send_hide_scrollbars_launch_option(false, false));
        assert!(should_send_hide_scrollbars_launch_option(true, true));

        let mut default_cmd = json!({ "action": "launch" });
        apply_hide_scrollbars_launch_option(&mut default_cmd, false, true);
        assert!(default_cmd.get("hideScrollbars").is_none());

        let mut config_false_cmd = json!({ "action": "launch" });
        apply_hide_scrollbars_launch_option(&mut config_false_cmd, false, false);
        assert_eq!(config_false_cmd["hideScrollbars"], false);

        let mut cli_true_cmd = json!({ "action": "launch" });
        apply_hide_scrollbars_launch_option(&mut cli_true_cmd, true, true);
        assert_eq!(cli_true_cmd["hideScrollbars"], true);
    }
}
