//! `close` and the last-tab `tab close` read the session's tabs back before
//! calling them closed, through the real daemon process. The browser is a fake
//! CDP endpoint that can lie about a close (acknowledge it and keep the tab) or
//! refuse it. In relay mode it stands in for the ab-connect relay: sessions are
//! `cb-tab-<id>`, `ABExt.inspectTab` answers from the tab record, and its
//! `Target.getTargets` drops a tab it was asked to close even when the tab is
//! still open, the way the relay's cached list can. No Chrome runs.
#![cfg(unix)]
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use std::collections::HashSet;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const BIN: &str = env!("CARGO_BIN_EXE_chrome-use");

#[derive(Default)]
struct Browser {
    relay: bool,
    /// Open tabs: (target id, Chrome tab id, url).
    tabs: Vec<(String, i64, String)>,
    created: u32,
    /// A close of one of these is acknowledged, and the tab stays open.
    lie: HashSet<String>,
    /// A close of one of these is refused (`success: false`).
    refuse: HashSet<String>,
    /// Targets the relay's cached list no longer shows (still open).
    unlisted: HashSet<String>,
    /// Every `Target.closeTarget` received.
    close_requests: Vec<String>,
}

#[derive(Clone)]
struct Fake(Arc<Mutex<Browser>>);

impl Fake {
    fn start(relay: bool) -> (Self, String) {
        let fake = Fake(Arc::new(Mutex::new(Browser {
            relay,
            ..Default::default()
        })));
        let shared = fake.clone();
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
                tx.send(format!("ws://127.0.0.1:{port}/devtools/browser/fake"))
                    .unwrap();
                loop {
                    let Ok((stream, _)) = listener.accept().await else {
                        continue;
                    };
                    let shared = shared.clone();
                    tokio::spawn(async move { serve(shared, stream).await });
                }
            });
        });
        let url = rx.recv().unwrap();
        (fake, url)
    }

    fn open_targets(&self) -> Vec<String> {
        self.0
            .lock()
            .unwrap()
            .tabs
            .iter()
            .map(|(t, _, _)| t.clone())
            .collect()
    }

    fn lie_about(&self, target: &str) {
        self.0.lock().unwrap().lie.insert(target.to_string());
    }

    fn refuse(&self, target: &str) {
        self.0.lock().unwrap().refuse.insert(target.to_string());
    }

    fn behave(&self) {
        let mut b = self.0.lock().unwrap();
        b.lie.clear();
        b.refuse.clear();
    }

    fn close_requests(&self) -> Vec<String> {
        self.0.lock().unwrap().close_requests.clone()
    }

    fn session_of(relay: bool, target: &str, tab: i64) -> String {
        if relay {
            format!("cb-tab-{tab}")
        } else {
            format!("S-{target}")
        }
    }

    fn reply(&self, req: &Value) -> Result<Value, String> {
        let mut b = self.0.lock().unwrap();
        let method = req["method"].as_str().unwrap_or("");
        let params = &req["params"];
        Ok(match method {
            "Target.getTargets" => {
                let list: Vec<Value> = b
                    .tabs
                    .iter()
                    .filter(|(t, _, _)| !b.unlisted.contains(t))
                    .map(|(id, _, url)| {
                        json!({"targetId": id, "type": "page", "title": id, "url": url,
                               "attached": true, "browserContextId": "C1"})
                    })
                    .collect();
                json!({"targetInfos": list})
            }
            "Target.createTarget" => {
                b.created += 1;
                let id = format!("T{}", b.created);
                let tab = 500 + b.created as i64;
                let url = params["url"].as_str().unwrap_or("about:blank").to_string();
                b.tabs.push((id.clone(), tab, url));
                json!({"targetId": id})
            }
            "Target.attachToTarget" => {
                let id = params["targetId"].as_str().unwrap_or("");
                let tab = b
                    .tabs
                    .iter()
                    .find(|(t, _, _)| t == id)
                    .map(|(_, tab, _)| *tab)
                    .unwrap_or(0);
                json!({"sessionId": Self::session_of(b.relay, id, tab)})
            }
            "Target.closeTarget" => {
                let id = params["targetId"].as_str().unwrap_or("").to_string();
                b.close_requests.push(id.clone());
                if b.refuse.contains(&id) {
                    json!({"success": false})
                } else if b.lie.contains(&id) {
                    // Acknowledged and still open. The relay's cached list
                    // drops it anyway; Chrome's own list (direct CDP) does not.
                    if b.relay {
                        b.unlisted.insert(id);
                    }
                    json!({"success": true})
                } else {
                    let existed = b.tabs.iter().any(|(t, _, _)| *t == id);
                    b.tabs.retain(|(t, _, _)| *t != id);
                    json!({"success": existed})
                }
            }
            "ABExt.inspectTab" if b.relay => {
                let session = params["sessionId"].as_str().unwrap_or("");
                let target = params["targetId"].as_str().unwrap_or("");
                let wanted: Option<i64> =
                    session.strip_prefix("cb-tab-").and_then(|n| n.parse().ok());
                let found = b.tabs.iter().find(|(t, tab, _)| {
                    Some(*tab) == wanted || (!target.is_empty() && t == target)
                });
                match (found, wanted) {
                    (Some((t, tab, url)), _) => {
                        json!({"chromeTabId": tab, "targetId": t, "url": url})
                    }
                    (None, Some(tab)) => {
                        return Err(format!("inspectTab: Chrome tab {tab} no longer exists"))
                    }
                    (None, None) => {
                        return Err(
                            "inspectTab: no tab matches the requested session or target".into()
                        )
                    }
                }
            }
            "Target.getTargetInfo" => {
                let id = params["targetId"].as_str().unwrap_or("");
                let url = b
                    .tabs
                    .iter()
                    .find(|(t, _, _)| t == id)
                    .map(|(_, _, u)| u.clone())
                    .unwrap_or_default();
                json!({"targetInfo": {"targetId": id, "type": "page", "title": id, "url": url,
                                      "attached": true}})
            }
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
        })
    }
}

async fn serve(fake: Fake, stream: tokio::net::TcpStream) {
    let Ok(mut ws) = tokio_tungstenite::accept_async(stream).await else {
        return;
    };
    while let Some(Ok(msg)) = ws.next().await {
        let Ok(text) = msg.into_text() else { continue };
        let Ok(req) = serde_json::from_str::<Value>(&text) else {
            continue;
        };
        let mut reply = match fake.reply(&req) {
            Ok(result) => json!({"id": req["id"], "result": result}),
            Err(message) => json!({"id": req["id"], "error": {"code": -32000, "message": message}}),
        };
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

struct Daemon {
    child: Child,
    _home: tempfile::TempDir,
    sock: tempfile::TempDir,
    _relay: tempfile::TempDir,
    session: String,
}

impl Daemon {
    fn start(session: &str, cdp: &str, relay_mode: bool) -> Self {
        let home = tempfile::tempdir().unwrap();
        let sock = tempfile::Builder::new()
            .prefix("cuc")
            .tempdir_in("/tmp")
            .unwrap();
        let relay = tempfile::tempdir().unwrap();
        if relay_mode {
            // The endpoint is the relay's: the daemon treats it as one.
            std::fs::write(relay.path().join("relay-cdp-url"), cdp).unwrap();
        }
        let child = Command::new(BIN)
            .env("AGENT_BROWSER_DAEMON", "1")
            .env("AGENT_BROWSER_SESSION", session)
            .env("AGENT_BROWSER_SOCKET_DIR", sock.path())
            .env("HOME", home.path())
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
            _relay: relay,
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

    fn record_path(&self) -> PathBuf {
        self.sock
            .path()
            .join(format!("{}.created-targets.json", self.session))
    }

    fn send(&self, cmd: Value) -> Value {
        let mut s = UnixStream::connect(self.sock_path())
            .unwrap_or_else(|e| panic!("{}: no daemon for {cmd}: {e}", self.session));
        s.set_read_timeout(Some(Duration::from_secs(60))).unwrap();
        writeln!(s, "{cmd}").unwrap();
        let mut line = String::new();
        BufReader::new(&s).read_line(&mut line).unwrap();
        serde_json::from_str(&line).unwrap_or_else(|e| panic!("{e}: {line:?}"))
    }

    /// The daemon exits after a completed close.
    /// The session ended: the daemon unlinked its socket (what it does as soon
    /// as a close completes) and then exited. The exit gets a generous budget
    /// because a loaded build host can take seconds to reap it.
    fn wait_exit(&mut self) -> bool {
        let started = Instant::now();
        while started.elapsed() < Duration::from_secs(30) {
            if !self.sock_path().exists() {
                if let Ok(Some(_)) = self.child.try_wait() {
                    return true;
                }
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        eprintln!(
            "socket present: {}, process exited: {:?}",
            self.sock_path().exists(),
            self.child.try_wait()
        );
        false
    }

    fn alive(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(None))
    }

    /// The session's tabs: (tab id, target id).
    fn tabs(&self) -> Vec<(String, String)> {
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

fn closed_targets(r: &Value) -> Vec<String> {
    let mut ids: Vec<String> = r["data"]["tabsClosed"]
        .as_array()
        .unwrap_or_else(|| panic!("no tabsClosed: {r}"))
        .iter()
        .map(|t| t["targetId"].as_str().unwrap().to_string())
        .collect();
    ids.sort();
    ids
}

/// A session with one tab of its own (T1), the way the first command opens it.
fn one_tab(d: &Daemon) -> String {
    let tabs = d.tabs();
    assert_eq!(tabs.len(), 1, "{tabs:?}");
    tabs[0].1.clone()
}

fn last_tab_case(relay: bool) {
    let (fake, url) = Fake::start(relay);
    let mut d = Daemon::start(if relay { "hc-last-r" } else { "hc-last" }, &url, relay);
    let target = one_tab(&d);
    let r = d.send(json!({"id": "c", "action": "tab_close", "tabId": "t1"}));
    assert_eq!(r["success"], true, "last-tab close was refused: {r}");
    assert_eq!(r["data"]["sessionClosed"], true, "{r}");
    assert_eq!(r["data"]["tabId"], "t1", "{r}");
    assert_eq!(r["data"]["verifiedAbsent"], true, "{r}");
    assert_eq!(closed_targets(&r), vec![target.clone()], "{r}");
    assert!(!fake.open_targets().contains(&target), "tab still open");
    assert!(d.wait_exit(), "the session did not end");
    assert!(!d.record_path().exists(), "ownership record left behind");
}

#[test]
fn last_tab_close_ends_the_session_in_one_round() {
    last_tab_case(false);
}

#[test]
fn last_tab_close_ends_the_session_over_the_relay() {
    last_tab_case(true);
}

fn verified_close_case(relay: bool) {
    let (fake, url) = Fake::start(relay);
    let mut d = Daemon::start(if relay { "hc-ok-r" } else { "hc-ok" }, &url, relay);
    let first = one_tab(&d);
    let r = d.send(json!({"id": "n", "action": "tab_new", "label": "docs"}));
    assert_eq!(r["success"], true, "{r}");
    let tabs = d.tabs();
    assert_eq!(tabs.len(), 2, "{tabs:?}");
    let r = d.send(json!({"id": "x", "action": "close"}));
    assert_eq!(r["success"], true, "{r}");
    assert_eq!(r["data"]["verifiedAbsent"], true, "{r}");
    assert_eq!(
        r["data"]["verifiedBy"],
        if relay {
            "extension-tabs"
        } else {
            "cdp-targets"
        },
        "{r}"
    );
    let mut want: Vec<String> = tabs.iter().map(|(_, t)| t.clone()).collect();
    want.sort();
    assert_eq!(closed_targets(&r), want, "{r}");
    assert!(want.contains(&first));
    assert!(fake.open_targets().is_empty(), "{:?}", fake.open_targets());
    assert!(d.wait_exit(), "daemon kept running after a complete close");
}

#[test]
fn successful_close_reports_verified_absent() {
    verified_close_case(false);
}

#[test]
fn successful_close_reports_verified_absent_over_the_relay() {
    verified_close_case(true);
}

fn close_that_leaves_a_tab_open_fails(relay: bool, refuse: bool) {
    let (fake, url) = Fake::start(relay);
    let name = format!("hc-open-{}{}", relay as u8, refuse as u8);
    let mut d = Daemon::start(&name, &url, relay);
    one_tab(&d);
    let r = d.send(json!({"id": "n", "action": "tab_new", "label": "docs"}));
    assert_eq!(r["success"], true, "{r}");
    let tabs = d.tabs();
    let stuck = tabs.iter().find(|(id, _)| id == "t2").unwrap().1.clone();
    if refuse {
        fake.refuse(&stuck);
    } else {
        // The close is acknowledged, the tab stays, and (over the relay) the
        // relay's cached target list stops showing it.
        fake.lie_about(&stuck);
    }

    let r = d.send(json!({"id": "x", "action": "close"}));
    assert_eq!(r["success"], false, "a tab is still open, close said: {r}");
    let error = r["error"].as_str().unwrap_or_default();
    assert!(error.contains("close incomplete"), "{error}");
    assert!(error.contains("still open"), "{error}");
    assert!(error.contains("t2") && error.contains(&stuck), "{error}");
    assert!(error.contains("retry `chrome-use close`"), "{error}");
    assert!(fake.open_targets().contains(&stuck));

    // The recovery basis is kept: the daemon, the tab, and its ownership.
    assert!(d.alive(), "daemon exited after an incomplete close");
    let record = std::fs::read_to_string(d.record_path()).expect("ownership record kept");
    assert!(record.contains(&stuck), "{record}");
    let tabs = d.tabs();
    assert_eq!(tabs, vec![("t2".to_string(), stuck.clone())], "{tabs:?}");

    // Once Chrome behaves, the retry closes it and the session ends.
    fake.behave();
    let r = d.send(json!({"id": "y", "action": "close"}));
    assert_eq!(r["success"], true, "{r}");
    assert_eq!(r["data"]["verifiedAbsent"], true, "{r}");
    assert_eq!(closed_targets(&r), vec![stuck.clone()], "{r}");
    assert!(fake.open_targets().is_empty());
    assert!(d.wait_exit());
}

#[test]
fn close_that_leaves_a_tab_open_reports_it_and_keeps_ownership() {
    close_that_leaves_a_tab_open_fails(false, false);
}

#[test]
fn close_refused_by_chrome_reports_it_and_keeps_ownership() {
    close_that_leaves_a_tab_open_fails(false, true);
}

#[test]
fn relay_list_dropping_an_open_tab_is_not_taken_as_closed() {
    close_that_leaves_a_tab_open_fails(true, false);
}

#[test]
fn last_tab_close_of_a_tab_left_open_keeps_the_session() {
    let (fake, url) = Fake::start(false);
    let mut d = Daemon::start("hc-last-stuck", &url, false);
    let target = one_tab(&d);
    fake.lie_about(&target);
    let r = d.send(json!({"id": "c", "action": "tab_close"}));
    assert_eq!(r["success"], false, "{r}");
    assert!(
        r["error"]
            .as_str()
            .is_some_and(|e| e.contains("close incomplete") && e.contains(&target)),
        "{r}"
    );
    assert!(
        d.alive(),
        "daemon exited after an incomplete last-tab close"
    );
    assert!(std::fs::read_to_string(d.record_path())
        .unwrap()
        .contains(&target));
}

#[test]
fn tab_close_never_closes_a_tab_the_session_did_not_create() {
    let (fake, url) = Fake::start(false);
    // The user's tab exists before the session connects.
    {
        let mut b = fake.0.lock().unwrap();
        b.tabs
            .push(("USER".to_string(), 1, "https://example.com/".into()));
    }
    let d = Daemon::start("hc-foreign", &url, false);
    let r = d.send(json!({"id": "a", "action": "tab_adopt", "spec": "USER"}));
    assert_eq!(r["success"], true, "adopting the user's tab failed: {r}");
    let tabs = d.tabs();
    let user = tabs.iter().find(|(_, t)| t == "USER").unwrap().0.clone();
    let r = d.send(json!({"id": "c", "action": "tab_close", "tabId": user}));
    assert_eq!(r["success"], false, "{r}");
    let error = r["error"].as_str().unwrap_or_default();
    assert!(error.contains("did not create it"), "{error}");
    assert!(error.contains("chrome-use close"), "{error}");
    let r = d.send(json!({"id": "x", "action": "close"}));
    assert_eq!(r["success"], true, "{r}");
    assert!(fake.open_targets().contains(&"USER".to_string()));
    assert!(!fake.close_requests().contains(&"USER".to_string()));
}
