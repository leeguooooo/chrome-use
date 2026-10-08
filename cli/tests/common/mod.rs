//! A stub daemon for real-CLI tests: the built binary talks to a Unix socket
//! in a temp dir that answers every command with success and records what it
//! was sent, so a test can check the command on the wire with no browser.
#![cfg(unix)]
#![allow(dead_code)]
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixListener;
use std::process::{Command, Output};
use std::sync::{Arc, Mutex};

pub const BIN: &str = env!("CARGO_BIN_EXE_chrome-use");

pub struct Stub {
    home: tempfile::TempDir,
    sock: tempfile::TempDir,
    session: String,
    seen: Arc<Mutex<Vec<serde_json::Value>>>,
}

impl Stub {
    pub fn start(session: &str) -> Self {
        let home = tempfile::tempdir().unwrap();
        // A short socket dir: Unix socket paths are capped near 104 bytes.
        let sock = tempfile::Builder::new()
            .prefix("cu")
            .tempdir_in("/tmp")
            .unwrap();
        std::fs::write(
            sock.path().join(format!("{session}.version")),
            env!("CARGO_PKG_VERSION"),
        )
        .unwrap();
        let listener = UnixListener::bind(sock.path().join(format!("{session}.sock"))).unwrap();
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
        Stub {
            home,
            sock,
            session: session.to_string(),
            seen,
        }
    }

    /// The temp `$HOME` the CLI runs with.
    pub fn home(&self) -> &std::path::Path {
        self.home.path()
    }

    /// The socket dir, where per-session sidecars live.
    pub fn sock_dir(&self) -> &std::path::Path {
        self.sock.path()
    }

    pub fn session(&self) -> &str {
        &self.session
    }

    /// Run the CLI with `--session <session> --json` plus `args`.
    pub fn run(&self, args: &[&str]) -> Output {
        self.run_env(args, &[])
    }

    /// [`Stub::run`] with extra environment variables.
    pub fn run_env(&self, args: &[&str], envs: &[(&str, &str)]) -> Output {
        self.command(envs)
            .args(["--session", &self.session, "--json"])
            .args(args)
            .output()
            .expect("run chrome-use")
    }

    /// The CLI with this stub's environment and no arguments yet.
    pub fn command(&self, envs: &[(&str, &str)]) -> Command {
        let mut c = Command::new(BIN);
        c.env("AGENT_BROWSER_SOCKET_DIR", self.sock.path())
            .env("HOME", self.home.path())
            .env("USERPROFILE", self.home.path())
            .env("AGENT_BROWSER_NO_AUTO_RECONNECT", "1")
            .env_remove("AGENT_BROWSER_CDP")
            .env_remove("AGENT_BROWSER_PROVIDER")
            .env_remove("AGENT_BROWSER_SESSION")
            .env_remove("CHROME_USE_CHOOSEBROWSER_RULES_FILE")
            .env("NO_COLOR", "1");
        for (k, v) in envs {
            c.env(k, v);
        }
        c
    }

    /// Run and require success.
    pub fn ok(&self, args: &[&str]) -> Output {
        let out = self.run(args);
        assert!(
            out.status.success(),
            "{args:?} failed\nstdout: {}\nstderr: {}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        out
    }

    /// Commands with this `action` the daemon received, oldest first.
    pub fn sent(&self, action: &str) -> Vec<serde_json::Value> {
        self.seen
            .lock()
            .unwrap()
            .iter()
            .filter(|c| c["action"] == action)
            .cloned()
            .collect()
    }

    pub fn clear(&self) {
        self.seen.lock().unwrap().clear();
    }
}

pub fn text(out: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}
