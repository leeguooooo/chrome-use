//! CLI batch control flow over a synthetic daemon socket, never a browser.
use super::*;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixListener;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

#[test]
fn pending_confirmation_stops_batch_without_replaying_prior_steps() {
    const MODE: &str = "CHROME_USE_TEST_BATCH_GATE_MODE";
    if let Ok(mode) = std::env::var(MODE) {
        let flags = flags::parse_flags(&[
            "--session".into(),
            "batch-fixture".into(),
            if mode.starts_with("json") {
                "--json"
            } else {
                "--debug"
            }
            .into(),
        ]);
        run_batch(
            &flags,
            mode.ends_with("-bail"),
            vec![
                vec!["get".into(), "text".into(), "#receipt".into()],
                vec![
                    "fill".into(),
                    "#editor".into(),
                    "replacement".into(),
                    "--allow-clipboard-fallback".into(),
                ],
                vec!["click".into(), "#publish".into()],
            ],
        );
        return;
    }
    for mode in ["json-continue", "json-bail", "text-continue", "text-bail"] {
        let root = tempfile::tempdir_in("/tmp").unwrap();
        let config = root.path().join("config.json");
        std::fs::write(&config, "{}").unwrap();
        let listener = UnixListener::bind(root.path().join("batch-fixture.sock")).unwrap();
        listener.set_nonblocking(true).unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let stopped = stop.clone();
        let actions = Arc::new(Mutex::new(Vec::<String>::new()));
        let seen = actions.clone();
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
                // macOS can inherit the listener's O_NONBLOCK on accept.
                // Bound the connected read with its timeout, not a timing race.
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
                let data = if action == "fill" {
                    assert_eq!(command["value"], "replacement");
                    assert_eq!(command["allowClipboardFallback"], true);
                    json!({"confirmation_required":true,"confirmation_id":"native-gate","approval":"user","site":"fixture.test","category":"clipboard","description":"Use clipboard fallback"})
                } else {
                    json!({"text":"already executed"})
                };
                writeln!(
                    stream,
                    "{}",
                    json!({"success":true,"data":data,"error":null})
                )
                .unwrap();
            }
        });
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "batch_confirmation_tests::pending_confirmation_stops_batch_without_replaying_prior_steps", "--nocapture"])
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
        assert_eq!(output.status.code(), Some(3), "{mode}: {stdout}\n{stderr}");
        assert_eq!(*actions.lock().unwrap(), ["gettext", "fill"], "{mode}");
        if mode.starts_with("json") {
            let results: Value = stdout
                .lines()
                .find_map(|line| {
                    serde_json::from_str::<Value>(line)
                        .ok()
                        .filter(Value::is_array)
                })
                .unwrap();
            assert_eq!(results.as_array().unwrap().len(), 2);
            assert_eq!(results[0]["result"]["text"], "already executed");
            assert_eq!(results[1]["result"]["confirmation_id"], "native-gate");
            assert_eq!(results[1]["result"]["approval"], "user");
        } else {
            assert!(stdout.contains("already executed"), "{stdout}");
            assert!(stdout.contains("Ask the user"), "{stdout}");
            assert!(stdout.contains("confirm native-gate"), "{stdout}");
        }
    }
}
