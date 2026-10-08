//! `screenshot` argument placement, through the real binary.
//!
//! `screenshot <path> --selector <sel>` used to send the path as the selector
//! and fail with "Element not found: <path>". The parse is unit-tested in
//! `commands.rs`; this checks the command the CLI actually puts on the wire,
//! against a stub daemon on a temp socket, so no browser is involved.
#![cfg(unix)]
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixListener;
use std::process::Command;
use std::sync::{Arc, Mutex};

const BIN: &str = env!("CARGO_BIN_EXE_chrome-use");

/// Start a stub daemon for `session` under `dir` that answers every command
/// with success and records what it was sent.
fn stub_daemon(dir: &std::path::Path, session: &str) -> Arc<Mutex<Vec<serde_json::Value>>> {
    std::fs::write(
        dir.join(format!("{session}.version")),
        env!("CARGO_PKG_VERSION"),
    )
    .unwrap();
    let listener = UnixListener::bind(dir.join(format!("{session}.sock"))).unwrap();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let log = seen.clone();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            let mut line = String::new();
            if BufReader::new(&stream).read_line(&mut line).is_err() {
                continue;
            }
            let cmd: serde_json::Value = serde_json::from_str(&line).unwrap_or_default();
            let reply = serde_json::json!({
                "id": cmd.get("id").cloned().unwrap_or_default(),
                "success": true,
                "data": { "path": cmd.get("path").cloned().unwrap_or_default() },
            });
            log.lock().unwrap().push(cmd);
            let _ = stream.write_all(format!("{reply}\n").as_bytes());
        }
    });
    seen
}

fn run(dir: &std::path::Path, home: &std::path::Path, session: &str, args: &[&str]) -> String {
    let out = Command::new(BIN)
        .args(["--session", session, "--json"])
        .args(args)
        .env("AGENT_BROWSER_SOCKET_DIR", dir)
        .env("HOME", home)
        .env("USERPROFILE", home)
        .env("AGENT_BROWSER_NO_AUTO_RECONNECT", "1")
        .env_remove("AGENT_BROWSER_CDP")
        .env_remove("AGENT_BROWSER_PROVIDER")
        .env("NO_COLOR", "1")
        .output()
        .expect("run chrome-use");
    assert!(
        out.status.success(),
        "{args:?} failed\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

#[test]
fn screenshot_path_and_selector_reach_the_daemon_in_any_order() {
    let tmp = tempfile::tempdir().unwrap();
    // A short socket dir: Unix socket paths are capped near 104 bytes.
    let sock = tempfile::Builder::new()
        .prefix("cu")
        .tempdir_in("/tmp")
        .unwrap();
    let session = "shot";
    let seen = stub_daemon(sock.path(), session);
    let orders: [&[&str]; 4] = [
        &["screenshot", "/tmp/cu-shot/out.png", "--selector", "main"],
        &["screenshot", "--selector", "main", "/tmp/cu-shot/out.png"],
        &["screenshot", "main", "/tmp/cu-shot/out.png"],
        &["screenshot", "/tmp/cu-shot/out.png", "main"],
    ];
    for args in orders {
        run(sock.path(), tmp.path(), session, args);
        let cmd = seen
            .lock()
            .unwrap()
            .iter()
            .rev()
            .find(|c| c["action"] == "screenshot")
            .cloned()
            .unwrap_or_else(|| panic!("{args:?}: no screenshot command reached the daemon"));
        assert_eq!(cmd["selector"], "main", "{args:?}: {cmd}");
        assert_eq!(cmd["path"], "/tmp/cu-shot/out.png", "{args:?}: {cmd}");
        seen.lock().unwrap().clear();
    }
}

#[test]
fn an_unknown_screenshot_option_is_refused_before_the_daemon() {
    let tmp = tempfile::tempdir().unwrap();
    let sock = tempfile::Builder::new()
        .prefix("cu")
        .tempdir_in("/tmp")
        .unwrap();
    let session = "shot2";
    let seen = stub_daemon(sock.path(), session);
    let out = Command::new(BIN)
        .args([
            "--session",
            session,
            "--json",
            "screenshot",
            "out.png",
            "--sel",
            "main",
        ])
        .env("AGENT_BROWSER_SOCKET_DIR", sock.path())
        .env("HOME", tmp.path())
        .env("AGENT_BROWSER_NO_AUTO_RECONNECT", "1")
        .env("NO_COLOR", "1")
        .output()
        .unwrap();
    assert!(!out.status.success());
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(text.contains("--sel"), "{text}");
    assert!(
        !seen
            .lock()
            .unwrap()
            .iter()
            .any(|c| c["action"] == "screenshot"),
        "a refused command must not be sent"
    );
}
