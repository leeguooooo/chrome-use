//! A session bound to a Chrome profile (`--browser <profile>`) across a relay
//! host restart (#484), through the real daemon process and its socket.
//!
//! The relay is a fake CDP endpoint that can restart the way the native host
//! does: the old port stops accepting connections and the same browser (same
//! tabs, same target ids) comes back on a new port. The per-profile relay
//! records are real files in a private relay dir, and they lag the restart
//! the way a killed host's records do: the profile's record keeps naming the
//! dead port until the test writes the new one. No Chrome runs.
#![cfg(unix)]
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const BIN: &str = env!("CARGO_BIN_EXE_chrome-use");

const PROFILE: &str = "P1";

#[derive(Default)]
struct Browser {
    /// (target id, url), in creation order.
    targets: Vec<(String, String)>,
    /// List targets newest first, the way a reconnect may enumerate them.
    reversed: bool,
    /// Bumped to drop every open connection.
    generation: u64,
    created: u32,
}

/// One browser, served on one port at a time.
#[derive(Clone)]
struct Fake {
    browser: Arc<Mutex<Browser>>,
    /// Set to stop the current port's listener (its port then refuses).
    retired: Arc<Mutex<Arc<AtomicBool>>>,
}

impl Fake {
    fn start() -> (Self, String) {
        let fake = Fake {
            browser: Arc::new(Mutex::new(Browser::default())),
            retired: Arc::new(Mutex::new(Arc::new(AtomicBool::new(false)))),
        };
        let url = fake.listen();
        (fake, url)
    }

    /// Serve the browser on a new port until this listener is retired.
    fn listen(&self) -> String {
        let retired = Arc::new(AtomicBool::new(false));
        *self.retired.lock().unwrap() = retired.clone();
        let shared = self.clone();
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let rt = tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .enable_all()
                .build()
                .unwrap();
            rt.block_on(async move {
                let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
                let port = listener.local_addr().unwrap().port();
                tx.send(format!(
                    "ws://127.0.0.1:{port}/devtools/browser/relay-{port}"
                ))
                .unwrap();
                while !retired.load(Ordering::SeqCst) {
                    let accepted =
                        tokio::time::timeout(Duration::from_millis(50), listener.accept()).await;
                    let Ok(Ok((stream, _))) = accepted else {
                        continue;
                    };
                    let shared = shared.clone();
                    let generation = shared.browser.lock().unwrap().generation;
                    tokio::spawn(async move { serve(shared, stream, generation).await });
                }
                // The listener is dropped here: the port refuses from now on.
            });
        });
        rx.recv().unwrap()
    }

    /// The relay host restarts: every open connection drops, the old port
    /// refuses, and the same browser is served on a new port.
    fn restart(&self, reversed: bool) -> String {
        self.retired.lock().unwrap().store(true, Ordering::SeqCst);
        {
            let mut b = self.browser.lock().unwrap();
            b.generation += 1;
            b.reversed = reversed;
        }
        // Let every open connection notice and close, and the old listener stop.
        std::thread::sleep(Duration::from_millis(300));
        self.listen()
    }

    fn remove(&self, target: &str) {
        self.browser
            .lock()
            .unwrap()
            .targets
            .retain(|(t, _)| t != target);
    }

    fn reply(&self, req: &Value) -> Value {
        let mut b = self.browser.lock().unwrap();
        let method = req["method"].as_str().unwrap_or("");
        let params = &req["params"];
        match method {
            // The profile has a window open (#486 checks before creating a tab).
            "ABExt.call" => json!({"result": [{"id": 1, "type": "normal"}]}),
            "Target.getTargets" => {
                let mut list: Vec<Value> = b
                    .targets
                    .iter()
                    .map(|(id, url)| {
                        json!({"targetId": id, "type": "page", "title": id, "url": url,
                               "attached": true, "browserContextId": "C1"})
                    })
                    .collect();
                if b.reversed {
                    list.reverse();
                }
                json!({"targetInfos": list})
            }
            "Target.createTarget" => {
                b.created += 1;
                let id = format!("T{}", b.created);
                let url = params["url"].as_str().unwrap_or("about:blank").to_string();
                b.targets.push((id.clone(), url));
                json!({"targetId": id})
            }
            "Target.attachToTarget" => {
                json!({"sessionId": format!("S-{}", params["targetId"].as_str().unwrap_or(""))})
            }
            "Target.closeTarget" => {
                let id = params["targetId"].as_str().unwrap_or("").to_string();
                b.targets.retain(|(t, _)| *t != id);
                json!({"success": true})
            }
            "Target.getTargetInfo" => {
                let id = params["targetId"].as_str().unwrap_or("");
                let url = b
                    .targets
                    .iter()
                    .find(|(t, _)| t == id)
                    .map(|(_, u)| u.clone())
                    .unwrap_or_default();
                json!({"targetInfo": {"targetId": id, "type": "page", "title": id, "url": url,
                                      "attached": true}})
            }
            // One button, so `snapshot -i` hands out a ref.
            "Accessibility.getFullAXTree" => json!({"nodes": [
                {"nodeId": "1", "role": {"type": "role", "value": "RootWebArea"},
                 "name": {"type": "computedString", "value": "fixture"},
                 "childIds": ["2"], "backendDOMNodeId": 1, "ignored": false},
                {"nodeId": "2", "role": {"type": "role", "value": "button"},
                 "name": {"type": "computedString", "value": "Go"},
                 "childIds": [], "backendDOMNodeId": 7, "ignored": false}
            ]}),
            "Runtime.evaluate" | "Runtime.callFunctionOn" => {
                json!({"result": {"type": "number", "value": 1}})
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
}

async fn serve(fake: Fake, stream: tokio::net::TcpStream, generation: u64) {
    let Ok(mut ws) = tokio_tungstenite::accept_async(stream).await else {
        return;
    };
    let current = |fake: &Fake| fake.browser.lock().unwrap().generation == generation;
    loop {
        if !current(&fake) {
            return; // dropping the socket is the dead connection
        }
        let next = tokio::time::timeout(Duration::from_millis(50), ws.next()).await;
        let msg = match next {
            Err(_) => continue,
            Ok(Some(Ok(m))) => m,
            Ok(_) => return,
        };
        let Ok(text) = msg.into_text() else { continue };
        let Ok(req) = serde_json::from_str::<Value>(&text) else {
            continue;
        };
        if !current(&fake) {
            return;
        }
        let mut reply = json!({"id": req["id"], "result": fake.reply(&req)});
        if let Some(s) = req.get("sessionId") {
            reply["sessionId"] = s.clone();
        }
        if ws
            .send(tokio_tungstenite::tungstenite::Message::Text(
                reply.to_string(),
            ))
            .await
            .is_err()
        {
            return;
        }
    }
}

/// The per-profile relay records the native host writes.
fn write_profile_endpoint(relay: &Path, profile: &str, ws: &str) {
    std::fs::write(
        relay.join(format!("relay-ext-profile-{profile}")),
        json!({"id": profile, "email": format!("{}@example.test", profile.to_lowercase())})
            .to_string(),
    )
    .unwrap();
    std::fs::write(relay.join(format!("relay-cdp-url-{profile}")), ws).unwrap();
}

fn remove_profile_endpoint(relay: &Path, profile: &str) {
    let _ = std::fs::remove_file(relay.join(format!("relay-ext-profile-{profile}")));
    let _ = std::fs::remove_file(relay.join(format!("relay-cdp-url-{profile}")));
}

/// The new host writes its endpoint record `after` the restart.
fn write_profile_endpoint_later(relay: &Path, ws: &str, after: Duration) {
    let relay = relay.to_path_buf();
    let ws = ws.to_string();
    std::thread::spawn(move || {
        std::thread::sleep(after);
        write_profile_endpoint(&relay, PROFILE, &ws);
    });
}

struct Daemon {
    child: Child,
    _home: tempfile::TempDir,
    sock: tempfile::TempDir,
    relay: tempfile::TempDir,
    session: String,
}

impl Daemon {
    /// A daemon the way the CLI spawns one for `--browser P1`: bound to the
    /// profile (its `relay-profile` record) and started on its endpoint.
    fn start_bound(session: &str, ws: &str, revive_secs: u64) -> Self {
        let home = tempfile::tempdir().unwrap();
        let sock = tempfile::Builder::new()
            .prefix("cub")
            .tempdir_in("/tmp")
            .unwrap();
        let relay = tempfile::tempdir().unwrap();
        write_profile_endpoint(relay.path(), PROFILE, ws);
        std::fs::write(
            sock.path().join(format!("{session}.relay-profile")),
            PROFILE,
        )
        .unwrap();
        std::fs::write(sock.path().join(format!("{session}.profile")), ws).unwrap();
        let child = Command::new(BIN)
            .env("AGENT_BROWSER_DAEMON", "1")
            .env("AGENT_BROWSER_SESSION", session)
            .env("AGENT_BROWSER_SOCKET_DIR", sock.path())
            .env("HOME", home.path())
            .env("CHROME_USE_RELAY_DIR", relay.path())
            .env("AGENT_BROWSER_CDP", ws)
            .env("AGENT_BROWSER_RELAY_REVIVE_SECS", revive_secs.to_string())
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
        std::thread::sleep(Duration::from_millis(300));
        d
    }

    fn sock_path(&self) -> PathBuf {
        self.sock.path().join(format!("{}.sock", self.session))
    }

    fn relay(&self) -> &Path {
        self.relay.path()
    }

    fn send(&self, cmd: Value) -> Value {
        let mut s = UnixStream::connect(self.sock_path())
            .unwrap_or_else(|e| panic!("{}: no daemon for {cmd}: {e}", self.session));
        s.set_read_timeout(Some(Duration::from_secs(90))).unwrap();
        writeln!(s, "{cmd}").unwrap();
        let mut line = String::new();
        BufReader::new(&s).read_line(&mut line).unwrap();
        serde_json::from_str(&line).unwrap_or_else(|e| panic!("{e}: {line:?}"))
    }

    /// What the CLI sends before every `--browser P1` command: a launch on
    /// the endpoint it resolved for the profile.
    fn launch(&self, ws: &str) -> Value {
        self.send(json!({"id": "launch", "action": "launch", "cdpUrl": ws}))
    }

    /// `tab_list`: (tab id, target id, label).
    fn tabs(&self) -> Vec<(String, String, Option<String>)> {
        let r = self.send(json!({"id": "l", "action": "tab_list"}));
        assert_eq!(r["success"], true, "{r}");
        r["data"]["tabs"]
            .as_array()
            .unwrap_or_else(|| panic!("no tabs: {r}"))
            .iter()
            .map(|t| {
                (
                    t["tabId"].as_str().unwrap().to_string(),
                    t["targetId"].as_str().unwrap().to_string(),
                    t["label"].as_str().map(str::to_string),
                )
            })
            .collect()
    }
}

impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn tab_of(tabs: &[(String, String, Option<String>)], target: &str) -> Option<String> {
    tabs.iter()
        .find(|(_, t, _)| t == target)
        .map(|(id, _, _)| id.clone())
}

fn assert_same_ids(d: &Daemon) {
    let tabs = d.tabs();
    assert_eq!(tab_of(&tabs, "T1").as_deref(), Some("t1"), "{tabs:?}");
    assert_eq!(tab_of(&tabs, "T2").as_deref(), Some("t2"), "{tabs:?}");
    assert_eq!(tab_of(&tabs, "T3").as_deref(), Some("t3"), "{tabs:?}");
}

/// Bound session on `ws` with three tabs, t2 (`docs`) the driven one, and a
/// snapshot taken on it. Returns the button's ref.
fn bound_session_with_refs(d: &Daemon, ws: &str) -> String {
    let r = d.launch(ws);
    assert_eq!(r["success"], true, "first launch: {r}");
    let r = d.send(json!({"id": "a", "action": "tab_list"}));
    assert_eq!(r["success"], true, "fake relay did not connect: {r}");
    if d.tabs().is_empty() {
        let r = d.send(json!({"id": "a0", "action": "tab_new"}));
        assert_eq!(r["success"], true, "{r}");
    }
    let r = d.send(json!({"id": "b", "action": "tab_new", "label": "docs"}));
    assert_eq!(r["success"], true, "{r}");
    let r = d.send(json!({"id": "c", "action": "tab_new"}));
    assert_eq!(r["success"], true, "{r}");
    let r = d.send(json!({"id": "d", "action": "tab_switch", "tabId": "t2"}));
    assert_eq!(r["success"], true, "{r}");
    assert_same_ids(d);
    let r = d.send(json!({"id": "s", "action": "snapshot", "interactive": true}));
    assert_eq!(r["success"], true, "{r}");
    let refs = r["data"]["refs"]
        .as_object()
        .unwrap_or_else(|| panic!("no refs: {r}"));
    let button = refs
        .iter()
        .find(|(_, v)| v["role"] == "button")
        .map(|(k, _)| k.clone())
        .unwrap_or_else(|| panic!("the fake's button got no ref: {r}"));
    // The ref resolves before the restart.
    assert!(!lost_ref(&ref_probe(d, &button)), "{button} unknown before");
    button
}

/// A command that resolves `@ref` through the session's ref map first.
fn ref_probe(d: &Daemon, r: &str) -> Value {
    d.send(json!({"id": "p", "action": "actions", "selector": format!("@{r}")}))
}

fn lost_ref(r: &Value) -> bool {
    r["error"]
        .as_str()
        .is_some_and(|e| e.contains("Unknown ref") || e.contains("NO snapshot refs"))
}

fn refused_for_lost_tab(r: &Value) -> bool {
    r["success"] == false
        && r["error"]
            .as_str()
            .is_some_and(|e| e.contains("Refusing to run `"))
}

/// #484 as found: the relay host restarts, the CLI resolves `--browser P1` to
/// the endpoint the profile's record still names (the dead one), and the new
/// host writes its own a moment later. The first command must not fail, and
/// the session keeps its tab ids and refs.
#[test]
fn first_browser_command_after_a_relay_restart_waits_out_the_stale_endpoint() {
    let (fake, ws1) = Fake::start();
    let d = Daemon::start_bound("rb-stale", &ws1, 20);
    let button = bound_session_with_refs(&d, &ws1);

    let ws2 = fake.restart(true);
    write_profile_endpoint_later(d.relay(), &ws2, Duration::from_millis(1500));
    let started = Instant::now();
    let r = d.launch(&ws1);
    assert_eq!(r["success"], true, "first command after restart: {r}");
    assert!(
        started.elapsed() >= Duration::from_millis(1000),
        "connected before the new endpoint existed: {r}"
    );

    let r = d.send(json!({"id": "x", "action": "tab_list"}));
    assert_eq!(r["success"], true, "{r}");
    let warning = r["warning"].as_str().unwrap_or("");
    assert!(warning.contains("re-established"), "{r}");
    assert!(warning.contains("keep the same ids"), "{r}");
    assert!(!warning.contains("could not be found"), "{r}");
    assert_same_ids(&d);
    let r = ref_probe(&d, &button);
    assert!(!lost_ref(&r), "refs lost across the restart: {r}");

    // The next `--browser P1` resolves the new endpoint: a reuse, not
    // another reconnect.
    let r = d.launch(&ws2);
    assert_eq!(r["success"], true, "{r}");
    assert_eq!(r["data"]["reused"], true, "{r}");
    let r = ref_probe(&d, &button);
    assert!(!lost_ref(&r), "{r}");
}

/// The other order from the issue: the new host's record is already written,
/// so the CLI asks for the new endpoint. The daemon replaces the connection
/// on that path, and it carries the ids the same way, failing closed for a
/// tab that is not found again: the driven tab's id and label are refused,
/// and nothing runs in a tab the agent did not choose.
#[test]
fn browser_launch_on_the_new_endpoint_carries_ids_and_fails_closed() {
    let (fake, ws1) = Fake::start();
    let d = Daemon::start_bound("rb-new", &ws1, 20);
    bound_session_with_refs(&d, &ws1);

    fake.remove("T2");
    let ws2 = fake.restart(true);
    write_profile_endpoint(d.relay(), PROFILE, &ws2);
    let r = d.launch(&ws2);
    assert_eq!(r["success"], true, "{r}");

    let r = d.send(json!({"id": "x", "action": "tab_list"}));
    assert_eq!(r["success"], true, "{r}");
    let warning = r["warning"].as_str().unwrap_or("");
    assert!(warning.contains("Tabs t2 could not be found"), "{r}");
    let tabs = d.tabs();
    assert_eq!(tab_of(&tabs, "T1").as_deref(), Some("t1"), "{tabs:?}");
    assert_eq!(tab_of(&tabs, "T3").as_deref(), Some("t3"), "{tabs:?}");
    assert_eq!(tabs.len(), 2, "{tabs:?}");

    let r = d.send(json!({"id": "c1", "action": "click", "selector": "#submit"}));
    assert!(refused_for_lost_tab(&r), "{r}");
    let r = d.send(json!({"id": "l1", "action": "tab_new", "label": "docs"}));
    assert_eq!(r["success"], false, "{r}");
    assert!(
        r["error"]
            .as_str()
            .unwrap_or("")
            .contains("still belongs to t2"),
        "{r}"
    );
}

/// The same restart with no `--browser` on the next command (relay-C4): the
/// daemon finds the connection dead and reconnects to the bound profile, and
/// the stale record is waited out there too.
#[test]
fn plain_command_after_a_relay_restart_waits_out_the_stale_endpoint() {
    let (fake, ws1) = Fake::start();
    let d = Daemon::start_bound("rb-plain", &ws1, 20);
    let button = bound_session_with_refs(&d, &ws1);

    let ws2 = fake.restart(false);
    write_profile_endpoint_later(d.relay(), &ws2, Duration::from_millis(1500));
    let r = d.send(json!({"id": "x", "action": "tab_list"}));
    assert_eq!(r["success"], true, "{r}");
    let warning = r["warning"].as_str().unwrap_or("");
    assert!(warning.contains("keep the same ids"), "{r}");
    assert_same_ids(&d);
    let r = ref_probe(&d, &button);
    assert!(!lost_ref(&r), "{r}");
}

/// The profile does not come back within the wait, while another profile's
/// relay is live on the port the browser is on now. The reconnect fails and
/// says so; it never connects to the other profile. The ids are kept, and
/// once the bound profile is back the next launch binds them.
#[test]
fn a_profile_that_does_not_come_back_fails_closed_and_keeps_its_ids() {
    let (fake, ws1) = Fake::start();
    let d = Daemon::start_bound("rb-gone", &ws1, 2);
    let button = bound_session_with_refs(&d, &ws1);

    let ws2 = fake.restart(true);
    write_profile_endpoint(d.relay(), "P2", &ws2);
    let r = d.launch(&ws1);
    assert_eq!(r["success"], false, "{r}");
    let error = r["error"].as_str().unwrap_or("");
    assert!(
        error.contains("could not reconnect to Chrome profile P1"),
        "{r}"
    );
    assert!(!error.contains("still driving"), "{r}");

    // A command without `--browser` is held to the profile too.
    let r = d.send(json!({"id": "y", "action": "tab_list"}));
    assert_eq!(r["success"], false, "{r}");

    remove_profile_endpoint(d.relay(), "P2");
    write_profile_endpoint(d.relay(), PROFILE, &ws2);
    let r = d.launch(&ws2);
    assert_eq!(r["success"], true, "{r}");
    let r = d.send(json!({"id": "z", "action": "tab_list"}));
    assert_eq!(r["success"], true, "{r}");
    assert!(
        r["warning"]
            .as_str()
            .unwrap_or("")
            .contains("keep the same ids"),
        "{r}"
    );
    assert_same_ids(&d);
    let r = ref_probe(&d, &button);
    assert!(!lost_ref(&r), "{r}");
}
