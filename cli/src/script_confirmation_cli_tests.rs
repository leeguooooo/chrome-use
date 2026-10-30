//! CLI script approval control flow over a synthetic daemon socket, never a browser.
use super::*;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixListener;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

#[test]
fn script_pending_confirmation_preserves_trace_and_cli_exit_codes() {
    const MODE: &str = "CHROME_USE_TEST_SCRIPT_GATE_MODE";
    if let Ok(mode) = std::env::var(MODE) {
        let flags = flags::parse_flags(&[
            "--session".into(),
            "script-fixture".into(),
            if mode.starts_with("json") {
                "--json"
            } else {
                "--debug"
            }
            .into(),
        ]);
        dispatch_script(
            &flags,
            json!({"id":"script","action":"script","program":[]}),
        );
        return;
    }
    for mode in [
        "json-pending",
        "text-pending",
        "json-ok",
        "json-failed",
        "json-invalid",
    ] {
        let root = tempfile::tempdir_in("/tmp").unwrap();
        let config = root.path().join("config.json");
        std::fs::write(&config, "{}").unwrap();
        let listener = UnixListener::bind(root.path().join("script-fixture.sock")).unwrap();
        listener.set_nonblocking(true).unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let stopped = stop.clone();
        let actions = Arc::new(Mutex::new(Vec::<String>::new()));
        let seen = actions.clone();
        let response_mode = mode.to_string();
        let server = std::thread::spawn(move || {
            while !stopped.load(Ordering::Acquire) {
                let (mut stream, _) = match listener.accept() {
                    Ok(connection) => connection,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(std::time::Duration::from_millis(1));
                        continue;
                    }
                    Err(error) => panic!("fake daemon accept failed: {error}"),
                };
                stream.set_nonblocking(false).unwrap();
                stream
                    .set_read_timeout(Some(std::time::Duration::from_secs(5)))
                    .unwrap();
                let mut raw = String::new();
                BufReader::new(stream.try_clone().unwrap())
                    .read_line(&mut raw)
                    .unwrap();
                let command: Value = serde_json::from_str(&raw).unwrap();
                let action = command["action"].as_str().unwrap().to_string();
                seen.lock().unwrap().push(action.clone());
                assert_eq!(action, "script");
                let data = if response_mode.ends_with("-pending") {
                    json!({"ok":false,"return":null,"confirmation_required":true,"confirmation_id":"native-gate","approval":"user","site":"fixture.test","category":"clipboard","description":"Use clipboard fallback","steps":[{"op":"evaluate","ok":true},{"op":"fill","ok":false,"confirmation_required":true}]})
                } else {
                    json!({"ok":response_mode == "json-ok","return":"ordinary result","logs":[]})
                };
                writeln!(
                    stream,
                    "{}",
                    json!({"success":response_mode != "json-invalid","data":data,"error":null})
                )
                .unwrap();
            }
        });
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "script_confirmation_cli_tests::script_pending_confirmation_preserves_trace_and_cli_exit_codes", "--nocapture"])
            .current_dir(root.path())
            .env(MODE, mode)
            .env("AGENT_BROWSER_CONFIG", config)
            .env("AGENT_BROWSER_SOCKET_DIR", root.path())
            .env("NO_COLOR", "1")
            .output().unwrap();
        stop.store(true, Ordering::Release);
        server.join().unwrap();
        let stdout = String::from_utf8(output.stdout).unwrap();
        let stderr = String::from_utf8(output.stderr).unwrap();
        let expected = if mode.ends_with("-pending") {
            3
        } else if mode == "json-ok" {
            0
        } else if mode == "json-invalid" {
            2
        } else {
            1
        };
        assert_eq!(
            output.status.code(),
            Some(expected),
            "{mode}: {stdout}\n{stderr}"
        );
        assert_eq!(*actions.lock().unwrap(), ["script"], "{mode}");
        if mode.starts_with("json") {
            let result: Value = stdout
                .lines()
                .find_map(|line| {
                    serde_json::from_str::<Value>(line)
                        .ok()
                        .filter(Value::is_object)
                })
                .unwrap();
            if mode.ends_with("-pending") {
                assert_eq!(result["steps"].as_array().unwrap().len(), 2);
                assert_eq!(result["steps"][0]["ok"], true);
                assert_eq!(result["confirmation_id"], "native-gate");
                assert_eq!(result["approval"], "user");
            } else {
                assert_eq!(
                    result,
                    json!({"ok":mode == "json-ok","return":"ordinary result","logs":[]})
                );
            }
        } else {
            assert!(stdout.contains("Ask the user"), "{stdout}");
            assert!(stdout.contains("confirm native-gate"), "{stdout}");
        }
    }
}
