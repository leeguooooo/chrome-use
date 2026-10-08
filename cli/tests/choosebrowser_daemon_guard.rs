//! The daemon-side ChooseBrowser guard, through the real daemon process and
//! its socket — raw commands, no CLI in between, so nothing the CLI adds (or
//! checks) is involved. The "browser" is a fake CDP endpoint; no Chrome runs.
//!
//! macOS-only, like the rule lookup's Chrome data root.
#![cfg(target_os = "macos")]
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

const BIN: &str = env!("CARGO_BIN_EXE_chrome-use");

const RULES: &str = r#"{"version":2,"rules":[
  {"ruleId":"r-one","match":{"domain":"one.example"},
   "action":{"bundleIdentifier":"com.google.Chrome::profile::111"}},
  {"ruleId":"r-two","match":{"domain":"two.example"},
   "action":{"bundleIdentifier":"com.google.Chrome::profile::222"}},
  {"ruleId":"r-twin","match":{"domain":"twin.example"},
   "action":{"bundleIdentifier":"com.google.Chrome::profile::555"}}
]}"#;

const LOCAL_STATE: &str = r#"{"profile":{"info_cache":{
  "Profile 1":{"gaia_id":"111","user_name":"one@x.test","name":"One"},
  "Profile 2":{"gaia_id":"222","user_name":"two@x.test","name":"Two"},
  "Profile 3":{"gaia_id":"555","user_name":"twin@x.test","name":"Twin A"},
  "Profile 4":{"gaia_id":"555","user_name":"twin@x.test","name":"Twin B"}
}}}"#;

/// A CDP endpoint that answers every call with a result carrying whatever
/// fields the daemon's connect path reads, so `connect_cdp` succeeds.
fn fake_cdp() -> String {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async move {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let port = listener.local_addr().unwrap().port();
            tx.send(format!("ws://127.0.0.1:{port}/devtools/browser/fake"))
                .unwrap();
            loop {
                let Ok((stream, _)) = listener.accept().await else {
                    continue;
                };
                tokio::spawn(async move {
                    let Ok(mut ws) = tokio_tungstenite::accept_async(stream).await else {
                        return;
                    };
                    while let Some(Ok(msg)) = ws.next().await {
                        let Ok(text) = msg.into_text() else { continue };
                        let Ok(req) = serde_json::from_str::<Value>(&text) else {
                            continue;
                        };
                        let mut reply = json!({"id": req["id"], "result": result_for(&req)});
                        if let Some(s) = req.get("sessionId") {
                            reply["sessionId"] = s.clone();
                        }
                        if ws
                            .send(tokio_tungstenite::tungstenite::Message::Text(
                                reply.to_string().into(),
                            ))
                            .await
                            .is_err()
                        {
                            return;
                        }
                    }
                });
            }
        });
    });
    rx.recv().unwrap()
}

fn result_for(req: &Value) -> Value {
    let page = json!({"targetId": "T1", "type": "page", "title": "", "url": "about:blank",
                      "attached": false, "browserContextId": "C1"});
    match req["method"].as_str().unwrap_or("") {
        "Target.getTargets" => json!({"targetInfos": [page]}),
        "Target.attachToTarget" => json!({"sessionId": "S1"}),
        "Target.createTarget" => json!({"targetId": "T1"}),
        "Runtime.evaluate" | "Runtime.callFunctionOn" => {
            json!({"result": {"type": "undefined"}})
        }
        "Page.navigate" => json!({"frameId": "F1", "loaderId": "L1"}),
        "Page.getFrameTree" => json!({"frameTree": {"frame": {
            "id": "F1", "loaderId": "L1", "url": "about:blank",
            "securityOrigin": "", "mimeType": "text/html"}}}),
        "Browser.getVersion" => json!({"protocolVersion": "1.3", "product": "Chrome/1",
            "revision": "1", "userAgent": "fake", "jsVersion": "1"}),
        _ => json!({}),
    }
}

struct Daemon {
    child: Child,
    _home: tempfile::TempDir,
    sock: tempfile::TempDir,
    relay: tempfile::TempDir,
    session: String,
}

impl Daemon {
    fn start(session: &str, cdp: &str) -> Self {
        let home = tempfile::tempdir().unwrap();
        let sock = tempfile::Builder::new()
            .prefix("cud")
            .tempdir_in("/tmp")
            .unwrap();
        let relay = tempfile::tempdir().unwrap();
        let root = chrome_root(home.path());
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("Local State"), LOCAL_STATE).unwrap();
        let rules = home.path().join("rules.json");
        std::fs::write(&rules, RULES).unwrap();
        let child = Command::new(BIN)
            .env("AGENT_BROWSER_DAEMON", "1")
            .env("AGENT_BROWSER_SESSION", session)
            .env("AGENT_BROWSER_SOCKET_DIR", sock.path())
            .env("HOME", home.path())
            .env("CHROME_USE_CHOOSEBROWSER_RULES_FILE", &rules)
            .env("CHROME_USE_RELAY_DIR", relay.path())
            .env("AGENT_BROWSER_CDP", cdp)
            .env("AGENT_BROWSER_NO_AUTO_RECONNECT", "1")
            .env_remove("AGENT_BROWSER_AUTO_CONNECT")
            .env_remove("AGENT_BROWSER_PROVIDER")
            .env("NO_COLOR", "1")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let d = Daemon {
            child,
            _home: home,
            sock,
            relay,
            session: session.to_string(),
        };
        let started = Instant::now();
        while !d.sock_path().exists() {
            assert!(
                started.elapsed() < Duration::from_secs(20),
                "daemon never listened"
            );
            std::thread::sleep(Duration::from_millis(50));
        }
        // Startup clears per-session sidecars; let it finish first.
        std::thread::sleep(Duration::from_millis(300));
        d
    }

    fn sock_path(&self) -> PathBuf {
        self.sock.path().join(format!("{}.sock", self.session))
    }

    fn root(&self) -> PathBuf {
        chrome_root(self._home.path())
    }

    /// The session record the CLI writes on first attach.
    fn bind_to(&self, dir: &str, label: &str) {
        std::fs::write(
            self.sock
                .path()
                .join(format!("{}.browser-profile", self.session)),
            json!({"id": "relay-x", "root": self.root().display().to_string(),
                   "dir": dir, "label": label})
            .to_string(),
        )
        .unwrap();
    }

    /// Publish `ws` as a relay endpoint, the way the native host does.
    fn publish_relay(&self, ws: &str) {
        std::fs::write(self.relay.path().join("relay-cdp-url-abc"), ws).unwrap();
    }

    fn send(&self, cmd: Value) -> Value {
        let mut s = UnixStream::connect(self.sock_path()).unwrap();
        s.set_read_timeout(Some(Duration::from_secs(60))).unwrap();
        writeln!(s, "{cmd}").unwrap();
        let mut line = String::new();
        BufReader::new(&s).read_line(&mut line).unwrap();
        serde_json::from_str(&line).unwrap_or_else(|e| panic!("{e}: {line:?}"))
    }
}

impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn chrome_root(home: &Path) -> PathBuf {
    home.join("Library/Application Support/Google/Chrome")
}

fn refused_by_rule(resp: &Value) -> bool {
    resp["success"] == false
        && resp["error"]
            .as_str()
            .is_some_and(|e| e.contains("ChooseBrowser rule"))
}

fn navigate(url: &str) -> Value {
    json!({"id": "n", "action": "navigate", "url": url, "waitUntil": "none"})
}

/// One command's explicit skip must not leak into the next: a raw command
/// without `_cbSkip` is checked even right after one that skipped.
#[test]
fn a_skip_does_not_outlive_the_command_that_asked_for_it() {
    // No browser is reachable here: the guard runs before any launch.
    let d = Daemon::start("cbd-leak", "ws://127.0.0.1:9/none");
    d.bind_to("Profile 1", "One (Profile 1)");
    let r = d.send(json!({"id": "a", "action": "state_list", "_cbSkip": true}));
    assert_ne!(r["success"], Value::Null, "{r}");
    let r = d.send(navigate("https://two.example/"));
    assert!(refused_by_rule(&r), "inherited a skip: {r}");
    assert!(
        r["error"].as_str().unwrap().contains("is bound to One"),
        "{r}"
    );
    // And the explicit skip itself still bypasses (it fails later, on the
    // unreachable browser — not on the rule).
    let r = d.send(
        json!({"id": "b", "action": "navigate", "url": "https://two.example/",
                          "waitUntil": "none", "_cbSkip": true}),
    );
    assert!(!refused_by_rule(&r), "{r}");
}

/// A script's own choice reaches its steps: skipped when the script says so,
/// checked when it does not.
#[test]
fn script_steps_inherit_the_scripts_choice() {
    // A script needs a browser before its steps run: the fake endpoint, not
    // published as a relay, so the session's identity is its record.
    let d = Daemon::start("cbd-script", &fake_cdp());
    d.bind_to("Profile 1", "One (Profile 1)");
    let program = json!([{"do": "navigate", "url": "https://two.example/", "waitUntil": "none"}]);
    let checked = d.send(json!({"id": "s1", "action": "script", "program": program}));
    assert!(
        checked.to_string().contains("ChooseBrowser rule (r-two)"),
        "{checked}"
    );
    let skipped = d.send(json!({"id": "s2", "action": "script", "program": program,
                                "_cbSkip": true}));
    assert!(
        !skipped.to_string().contains("ChooseBrowser rule"),
        "{skipped}"
    );
    // The skip ended with that script: the next one is checked again.
    let again = d.send(json!({"id": "s3", "action": "script", "program": program}));
    assert!(
        again.to_string().contains("ChooseBrowser rule (r-two)"),
        "{again}"
    );
}

/// A session on a known relay endpoint whose profile cannot be identified
/// (no relay row, no record) is refused, not treated as off the relay — even
/// for an ambiguous rule. An explicit skip still bypasses.
#[test]
fn a_relay_session_with_unknown_identity_is_refused() {
    let cdp = fake_cdp();
    let d = Daemon::start("cbd-unknown", &cdp);
    d.publish_relay(&cdp);
    // Connect the daemon's browser to the fake endpoint.
    let r = d.send(json!({"id": "t", "action": "tab_list", "_cbSkip": true}));
    assert_eq!(r["success"], true, "fake browser did not connect: {r}");

    let r = d.send(navigate("https://two.example/"));
    assert!(refused_by_rule(&r), "{r}");
    assert!(
        r["error"]
            .as_str()
            .unwrap()
            .contains("cannot tell which profile"),
        "{r}"
    );
    let r = d.send(navigate("https://twin.example/"));
    assert!(refused_by_rule(&r), "{r}");
    assert!(
        r["error"].as_str().unwrap().contains("matches 2 profiles"),
        "{r}"
    );

    let mut skip = navigate("https://two.example/");
    skip["_cbSkip"] = json!(true);
    let r = d.send(skip);
    assert!(!refused_by_rule(&r), "{r}");

    // The same endpoint, not published as a relay: a plain --cdp browser,
    // which no rule is about.
    std::fs::remove_file(d.relay.path().join("relay-cdp-url-abc")).unwrap();
    let r = d.send(navigate("https://two.example/"));
    assert!(!refused_by_rule(&r), "{r}");
}
