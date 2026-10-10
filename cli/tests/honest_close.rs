//! `close`, the last-tab `tab close` and `tab close` of one tab read the
//! session's tabs back before calling them closed, through the real daemon
//! process (and the real CLI). The browser is a fake CDP endpoint that can lie
//! about a close (acknowledge it and keep the tab), refuse it, hang, or fail
//! to report a tab. In relay mode it stands in for the ab-connect relay:
//! sessions are `cb-tab-<id>`, `ABExt.tabPresence` answers the 0.5.33
//! contract (or, as an older extension, not at all), and its
//! `Target.getTargets` drops a tab it was asked to close even when the tab is
//! still open, the way the relay's cached list can. No Chrome runs.
#![cfg(unix)]
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use std::collections::HashSet;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::process::{Child, Command, Output, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const BIN: &str = env!("CARGO_BIN_EXE_chrome-use");

/// The product's whole close-and-verify deadline (`CLOSE_TOTAL_BUDGET`).
const CLOSE_TOTAL_BUDGET: Duration = Duration::from_secs(15);

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
    /// `ABExt.tabPresence` cannot read these (a chrome.tabs API error):
    /// the extension answers `unknown`.
    api_error: HashSet<String>,
    /// The extension predates `ABExt.tabPresence` (ab-connect 0.5.32).
    old_extension: bool,
    /// A malformed answer in place of every `absent` (fields to overwrite).
    malformed_absence: Option<Value>,
    /// Methods never answered.
    hang: HashSet<String>,
    /// Targets the relay's cached list no longer shows (still open).
    unlisted: HashSet<String>,
    /// Every `Target.closeTarget` received.
    close_requests: Vec<String>,
    /// CDP events sent before the next reply.
    events: Vec<Value>,
    /// Every `ABExt.call`, as `namespace.method` (#517).
    abext_calls: Vec<String>,
    /// WebSocket connections accepted (#517).
    connections: u32,
    /// `Target.createTarget` calls since the counts were last reset (#517).
    created_since_reset: u32,
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

    fn fail_presence(&self, target: &str) {
        self.0.lock().unwrap().api_error.insert(target.to_string());
    }

    fn hang(&self, method: &str) {
        self.0.lock().unwrap().hang.insert(method.to_string());
    }

    fn hangs(&self, method: &str) -> bool {
        self.0.lock().unwrap().hang.contains(method)
    }

    /// Chrome replaces the tab holding `target` (a discard or prerender
    /// swap): the same target, under a new Chrome tab id; the old id is gone.
    fn replace_tab(&self, target: &str) -> i64 {
        let mut b = self.0.lock().unwrap();
        let entry = b.tabs.iter_mut().find(|(t, _, _)| t == target).unwrap();
        entry.1 += 1000;
        entry.1
    }

    /// The user closes a tab, outside the session: Chrome reports it
    /// destroyed.
    fn remove(&self, target: &str) {
        let mut b = self.0.lock().unwrap();
        b.tabs.retain(|(t, _, _)| t != target);
        b.events.push(json!({"method": "Target.targetDestroyed",
                             "params": {"targetId": target}}));
    }

    fn take_events(&self) -> Vec<Value> {
        std::mem::take(&mut self.0.lock().unwrap().events)
    }

    fn behave(&self) {
        let mut b = self.0.lock().unwrap();
        b.lie.clear();
        b.refuse.clear();
        b.api_error.clear();
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
        if method == "ABExt.call" {
            let call = format!(
                "{}.{}",
                params["namespace"].as_str().unwrap_or(""),
                params["method"].as_str().unwrap_or("")
            );
            b.abext_calls.push(call);
        }
        Ok(match method {
            // The profile has a window (#486 checks before a first tab).
            "ABExt.call" if params["namespace"] == "windows" && params["method"] == "getAll" => {
                json!({"result": [{"id": 1, "type": "normal"}]})
            }
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
                b.created_since_reset += 1;
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
            // ab-connect 0.5.33's contract: listed in Chrome's registry means
            // present (at its current tab); absent only when the target is
            // gone AND the exact tab id given is gone; an API error is unknown.
            "ABExt.tabPresence" if b.relay => {
                if b.old_extension {
                    // What 0.5.32 really answers: an unknown method falls
                    // through to its debugger path, which reads like a lost
                    // tab (seen live; the command layer must not take it as
                    // one).
                    let target = params["targetId"].as_str().unwrap_or("");
                    return Err(format!(
                        "no attached tab for targetId {target} (ABExt.tabPresence)"
                    ));
                }
                let target = params["targetId"].as_str().unwrap_or("").to_string();
                let asked = params["tabId"].as_i64();
                if b.api_error.contains(&target) {
                    json!({"tabPresenceVersion": 1, "targetId": target, "tabId": asked,
                           "presence": "unknown",
                           "error": "Tabs cannot be edited right now (user may be dragging a tab)."})
                } else if let Some((_, tab, url)) = b.tabs.iter().find(|(t, _, _)| *t == target) {
                    json!({"tabPresenceVersion": 1, "targetId": target, "tabId": tab,
                           "presence": "present", "url": url})
                } else if let Some(tab) = asked {
                    let presence = if b.tabs.iter().any(|(_, id, _)| *id == tab) {
                        "unknown"
                    } else {
                        "absent"
                    };
                    let mut reply = json!({"tabPresenceVersion": 1, "targetId": target,
                                           "tabId": tab, "presence": presence});
                    if let (Some(fields), "absent") = (b.malformed_absence.as_ref(), presence) {
                        for (k, v) in fields.as_object().unwrap() {
                            reply[k] = v.clone();
                        }
                    }
                    reply
                } else {
                    json!({"tabPresenceVersion": 1, "targetId": target, "tabId": null,
                           "presence": "unknown", "error": "no tab id to confirm"})
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
    fake.0.lock().unwrap().connections += 1;
    while let Some(Ok(msg)) = ws.next().await {
        let Ok(text) = msg.into_text() else { continue };
        let Ok(req) = serde_json::from_str::<Value>(&text) else {
            continue;
        };
        if fake.hangs(req["method"].as_str().unwrap_or("")) {
            continue; // never answered
        }
        for event in fake.take_events() {
            let frame = tokio_tungstenite::tungstenite::Message::Text(event.to_string());
            if ws.send(frame).await.is_err() {
                return;
            }
        }
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
    home: tempfile::TempDir,
    sock: tempfile::TempDir,
    relay: tempfile::TempDir,
    session: String,
    cdp: String,
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
            home,
            sock,
            relay,
            session: session.to_string(),
            cdp: cdp.to_string(),
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

    /// The real CLI against this daemon, with `--json`.
    fn cli(&self, args: &[&str]) -> Output {
        Command::new(BIN)
            .env("AGENT_BROWSER_SOCKET_DIR", self.sock.path())
            .env("HOME", self.home.path())
            .env("CHROME_USE_RELAY_DIR", self.relay.path())
            .env("AGENT_BROWSER_CDP", &self.cdp)
            .env("AGENT_BROWSER_NO_AUTO_RECONNECT", "1")
            .env_remove("AGENT_BROWSER_AUTO_CONNECT")
            .env_remove("AGENT_BROWSER_PROVIDER")
            .env_remove("AGENT_BROWSER_SESSION")
            .env("NO_COLOR", "1")
            .args(["--json", "--session", &self.session])
            .args(args)
            .output()
            .expect("run chrome-use")
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

fn record(d: &Daemon) -> String {
    std::fs::read_to_string(d.record_path()).expect("ownership record kept")
}

/// A relay session with two tabs of its own: (t1 target, t2 target).
fn two_relay_tabs(fake: &Fake, d: &Daemon) -> (String, String) {
    let _ = fake;
    one_tab(d);
    let r = d.send(json!({"id": "n", "action": "tab_new", "label": "docs"}));
    assert_eq!(r["success"], true, "{r}");
    let tabs = d.tabs();
    let of = |id: &str| tabs.iter().find(|(t, _)| t == id).unwrap().1.clone();
    (of("t1"), of("t2"))
}

/// #496 review: an API error reading the tab is not a missing tab. The tab
/// really was closed here, but the extension cannot confirm it: close stays
/// incomplete, the ownership record keeps the tab, and the daemon stays.
#[test]
fn an_extension_api_error_is_unverified_not_closed() {
    let (fake, url) = Fake::start(true);
    let mut d = Daemon::start("hc-apierr", &url, true);
    let (_, t2) = two_relay_tabs(&fake, &d);
    fake.fail_presence(&t2);
    let r = d.send(json!({"id": "x", "action": "close"}));
    assert_eq!(r["success"], false, "{r}");
    let error = r["error"].as_str().unwrap_or_default();
    assert!(error.contains("close incomplete"), "{error}");
    assert!(
        error.contains("unverified") && error.contains(&t2),
        "{error}"
    );
    assert!(error.contains("dragging a tab"), "{error}");
    assert!(d.alive(), "daemon exited after an unverified close");
    assert!(d.sock_path().exists());
    assert!(record(&d).contains(&t2), "ownership of {t2} dropped");
    // Once the extension can read it again, the retry confirms it gone.
    fake.behave();
    let r = d.send(json!({"id": "y", "action": "close"}));
    assert_eq!(r["success"], true, "{r}");
    assert_eq!(closed_targets(&r), vec![t2.clone()], "{r}");
    assert!(d.wait_exit());
}

/// An extension without the `ABExt.tabPresence` contract (0.5.32) can never
/// have a tab counted as closed: unverified, with the update named.
#[test]
fn an_extension_without_tab_presence_is_unverified() {
    let (fake, url) = Fake::start(true);
    fake.0.lock().unwrap().old_extension = true;
    let mut d = Daemon::start("hc-oldext", &url, true);
    let (t1, t2) = two_relay_tabs(&fake, &d);
    let r = d.send(json!({"id": "x", "action": "close"}));
    assert_eq!(r["success"], false, "{r}");
    let error = r["error"].as_str().unwrap_or_default();
    assert!(
        error.contains("2 of the 2 tab(s) could not be confirmed closed"),
        "{error}"
    );
    assert!(error.contains("update it to 0.5.33 or newer"), "{error}");
    // Not mistaken for a lost tab: no stale-session retry closed them again.
    assert!(error.starts_with("close incomplete"), "{error}");
    assert!(!error.contains("was driving is gone"), "{error}");
    assert_eq!(
        fake.close_requests().len(),
        2,
        "{:?}",
        fake.close_requests()
    );
    assert!(d.alive());
    let kept = record(&d);
    assert!(kept.contains(&t1) && kept.contains(&t2), "{kept}");
}

/// The upgrade path: a close under 0.5.32 closes the tabs but cannot confirm
/// it, so it keeps the session and the record. Once the extension is
/// upgraded, `close` on the same session confirms them gone (by the exact tab
/// ids the session recorded) and clears the record, without `--force`.
#[test]
fn close_after_upgrading_the_extension_completes_without_force() {
    let (fake, url) = Fake::start(true);
    fake.0.lock().unwrap().old_extension = true;
    let mut d = Daemon::start("hc-upgrade", &url, true);
    let (t1, t2) = two_relay_tabs(&fake, &d);
    let r = d.send(json!({"id": "x", "action": "close"}));
    assert_eq!(r["success"], false, "{r}");
    assert!(fake.open_targets().is_empty(), "the tabs were closed");
    let kept = record(&d);
    assert!(kept.contains(&t1) && kept.contains(&t2), "{kept}");
    // ab-connect 0.5.33 is installed.
    fake.0.lock().unwrap().old_extension = false;
    let r = d.send(json!({"id": "y", "action": "close"}));
    assert_eq!(r["success"], true, "{r}");
    assert_eq!(r["data"]["verifiedAbsent"], true, "{r}");
    let mut want = vec![t1, t2];
    want.sort();
    assert_eq!(closed_targets(&r), want, "{r}");
    assert!(d.wait_exit());
    assert!(!d.record_path().exists(), "record left behind");
}

/// The same with `tab close` of one of several tabs.
#[test]
fn tab_close_with_an_extension_without_tab_presence_keeps_the_tab() {
    let (fake, url) = Fake::start(true);
    let mut d = Daemon::start("hc-oldext-tab", &url, true);
    let (_, t2) = two_relay_tabs(&fake, &d);
    fake.0.lock().unwrap().old_extension = true;
    let r = d.send(json!({"id": "c", "action": "tab_close", "tabId": "t2"}));
    assert_eq!(r["success"], false, "{r}");
    let error = r["error"].as_str().unwrap_or_default();
    assert!(error.contains("tab t2 was not closed"), "{error}");
    assert!(error.contains("0.5.33"), "{error}");
    assert_eq!(fake.close_requests(), vec![t2.clone()]);
    assert!(d.alive());
    assert!(record(&d).contains(&t2));
    assert!(d.tabs().iter().any(|(_, t)| *t == t2));
}

/// #496 review: the same target under a new Chrome tab id (a replacement),
/// with a close acknowledged and the tab still open. The recorded tab id no
/// longer exists, which must not read as the tab being gone.
#[test]
fn a_replaced_tab_whose_close_was_only_acknowledged_is_still_open() {
    let (fake, url) = Fake::start(true);
    let mut d = Daemon::start("hc-replaced", &url, true);
    let (_, t2) = two_relay_tabs(&fake, &d);
    let new_tab = fake.replace_tab(&t2);
    fake.lie_about(&t2);
    let r = d.send(json!({"id": "x", "action": "close"}));
    assert_eq!(r["success"], false, "{r}");
    assert_ne!(r["data"]["verifiedAbsent"], true, "{r}");
    let error = r["error"].as_str().unwrap_or_default();
    assert!(
        error.contains("still open") && error.contains(&t2),
        "{error}"
    );
    assert!(fake.open_targets().contains(&t2));
    assert!(d.alive());
    assert!(record(&d).contains(&t2));
    // The tab the target lives in now is the one closed on the retry.
    fake.behave();
    let r = d.send(json!({"id": "y", "action": "close"}));
    assert_eq!(r["success"], true, "{r}");
    assert_eq!(closed_targets(&r), vec![t2.clone()], "{r}");
    assert!(
        !fake.open_targets().contains(&t2),
        "tab {new_tab} still open"
    );
    assert!(d.wait_exit());
}

/// #496 review: one deadline for the whole close. Sixteen tabs whose
/// presence reads and closes never answer: `close` still answers within
/// the budget (far under the CLI's 45 s), every tab unverified, every
/// ownership kept, the daemon still up.
#[test]
fn close_of_many_hanging_tabs_stays_within_one_budget() {
    let (fake, url) = Fake::start(true);
    let mut d = Daemon::start("hc-hang", &url, true);
    one_tab(&d);
    for i in 0..15 {
        let r = d.send(json!({"id": format!("n{i}"), "action": "tab_new"}));
        assert_eq!(r["success"], true, "{r}");
    }
    let tabs = d.tabs();
    assert_eq!(tabs.len(), 16, "{tabs:?}");
    fake.hang("ABExt.tabPresence");
    fake.hang("Target.closeTarget");
    let started = Instant::now();
    let r = d.send(json!({"id": "x", "action": "close"}));
    let took = started.elapsed();
    assert_eq!(r["success"], false, "{r}");
    assert!(
        took < CLOSE_TOTAL_BUDGET + Duration::from_secs(5),
        "close took {took:?}, over its {CLOSE_TOTAL_BUDGET:?} budget"
    );
    let error = r["error"].as_str().unwrap_or_default();
    assert!(
        error.contains("16 of the 16 tab(s) could not be confirmed closed"),
        "{error}"
    );
    assert!(d.alive());
    let kept = record(&d);
    for (_, target) in &tabs {
        assert!(kept.contains(target), "{target} dropped: {kept}");
    }
    eprintln!("close of 16 hanging tabs answered in {took:?}");
}

/// #496 review: `tab close` of one of several tabs is read back too. Two
/// tabs of the session's own and one of the user's; the session's second tab
/// acknowledges its close and stays open. The real CLI reports the failure,
/// the tab keeps its close right, and a later `close` closes it. The user's
/// tab is never asked to close and stays open throughout.
fn multi_tab_case(relay: bool) {
    let (fake, url) = Fake::start(relay);
    fake.0
        .lock()
        .unwrap()
        .tabs
        .push(("USER".to_string(), 7, "https://example.com/mine".into()));
    let mut d = Daemon::start(&format!("hc-multi-{}", relay as u8), &url, relay);
    for _ in 0..2 {
        let out = d.cli(&["tab", "new"]);
        assert!(out.status.success(), "{}", text(&out));
    }
    let out = d.cli(&["tab", "list"]);
    let list: Value = serde_json::from_slice(&out.stdout).unwrap();
    let created: Vec<(String, String)> = list["data"]["tabs"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|t| t["ownership"] == "created" || (!relay && t["targetId"] != "USER"))
        .map(|t| {
            (
                t["tabId"].as_str().unwrap().to_string(),
                t["targetId"].as_str().unwrap().to_string(),
            )
        })
        .collect();
    assert!(created.len() >= 2, "{list}");
    assert!(created.iter().all(|(_, t)| t != "USER"), "{list}");
    let (stuck_ref, stuck) = created.last().unwrap().clone();
    fake.lie_about(&stuck);

    let out = d.cli(&["tab", "close", &stuck_ref]);
    assert!(
        !out.status.success(),
        "tab close reported success: {}",
        text(&out)
    );
    let said = text(&out);
    assert!(
        said.contains("was not closed") && said.contains(&stuck),
        "{said}"
    );
    assert!(said.contains("keeps its close right"), "{said}");
    assert!(fake.open_targets().contains(&stuck));
    assert!(record(&d).contains(&stuck), "close right lost");
    let out = d.cli(&["tab", "list"]);
    let list: Value = serde_json::from_slice(&out.stdout).unwrap();
    let row = list["data"]["tabs"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["targetId"] == stuck.as_str())
        .unwrap_or_else(|| panic!("{stuck} left the tab list: {list}"))
        .clone();
    if relay {
        assert_eq!(row["ownership"], "created", "{row}");
    }

    fake.behave();
    let out = d.cli(&["close"]);
    assert!(out.status.success(), "{}", text(&out));
    let r: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(r["data"]["verifiedAbsent"], true, "{r}");
    assert!(closed_targets(&r).contains(&stuck), "{r}");
    assert!(d.wait_exit());
    assert!(fake.open_targets().contains(&"USER".to_string()));
    assert!(!fake.close_requests().contains(&"USER".to_string()));
}

#[test]
fn tab_close_of_a_tab_left_open_keeps_its_right_and_close_recovers_it() {
    multi_tab_case(false);
}

#[test]
fn tab_close_of_a_tab_left_open_keeps_its_right_over_the_relay() {
    multi_tab_case(true);
}

/// #496 review: an omitted `tabId` resolves the active tab strictly. Over the
/// relay, the pinned tab is destroyed (the pin stays as a tombstone) and the
/// session's other tab is all `pages` holds: `tab close` refuses instead of
/// taking the last-tab branch and ending the session with that other tab.
#[test]
fn tab_close_without_a_tab_refuses_when_the_pinned_tab_is_gone() {
    let (fake, url) = Fake::start(true);
    let mut d = Daemon::start("hc-dangling", &url, true);
    let t1 = one_tab(&d);
    let r = d.send(json!({"id": "n", "action": "tab_new", "label": "docs"}));
    assert_eq!(r["success"], true, "{r}");
    let t2 = d.tabs().into_iter().find(|(id, _)| id == "t2").unwrap().1;
    // The user closes the pinned tab; the session has only t1 left.
    fake.remove(&t2);
    // The daemon drains Chrome's events at the start of a command.
    let mut last = Value::Null;
    let gone = (0..5).any(|i| {
        last = d.send(json!({"id": format!("l{i}"), "action": "tab_list"}));
        let listed: Vec<&str> = last["data"]["tabs"]
            .as_array()
            .map(|tabs| tabs.iter().filter_map(|t| t["targetId"].as_str()).collect())
            .unwrap_or_default();
        !listed.contains(&t2.as_str())
    });
    assert!(gone, "{t2} still tracked: {last}");
    let r = d.send(json!({"id": "c", "action": "tab_close"}));
    assert_eq!(r["success"], false, "{r}");
    assert_ne!(r["data"]["sessionClosed"], true, "{r}");
    assert!(fake.open_targets().contains(&t1), "t1 was closed");
    assert!(
        !fake.close_requests().contains(&t1),
        "{:?}",
        fake.close_requests()
    );
    assert!(d.alive());
    assert!(record(&d).contains(&t1));
}

/// A `tabId` that is not a string is refused, never read as omitted (which
/// would close the session's last tab).
#[test]
fn tab_close_refuses_a_tab_id_that_is_not_a_string() {
    let (fake, url) = Fake::start(false);
    let mut d = Daemon::start("hc-rawid", &url, false);
    let t1 = one_tab(&d);
    for raw in [json!(1), json!(true), json!({"tab": "t1"}), json!(["t1"])] {
        let r = d.send(json!({"id": "c", "action": "tab_close", "tabId": raw}));
        assert_eq!(r["success"], false, "tabId {raw}: {r}");
        assert!(
            r["error"]
                .as_str()
                .is_some_and(|e| e.contains("tabId must be a tab id or label string")),
            "{r}"
        );
    }
    assert!(fake.open_targets().contains(&t1));
    assert!(
        fake.close_requests().is_empty(),
        "{:?}",
        fake.close_requests()
    );
    assert!(d.alive());
    assert!(record(&d).contains(&t1));
}

fn text(out: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

/// #496 review: an `absent` that is malformed (no tab id, a negative one,
/// another tab's, or a contract version this CLI does not know) is not proof.
/// The tabs really are closed here, but every close stays unverified: the
/// ownership record keeps them and the daemon stays up.
#[test]
fn a_malformed_absence_reply_keeps_the_rights() {
    for (i, fields) in [
        json!({"tabId": null}),
        json!({"tabId": -1}),
        json!({"tabId": 1}),
        json!({"tabPresenceVersion": 2}),
    ]
    .into_iter()
    .enumerate()
    {
        let (fake, url) = Fake::start(true);
        fake.0.lock().unwrap().malformed_absence = Some(fields.clone());
        let mut d = Daemon::start(&format!("hc-malformed-{i}"), &url, true);
        let (t1, t2) = two_relay_tabs(&fake, &d);
        let r = d.send(json!({"id": "x", "action": "close"}));
        assert_eq!(r["success"], false, "{fields}: {r}");
        let error = r["error"].as_str().unwrap_or_default();
        assert!(
            error.contains("2 of the 2 tab(s) could not be confirmed closed"),
            "{fields}: {error}"
        );
        assert!(fake.open_targets().is_empty(), "the fake closed them");
        let kept = record(&d);
        assert!(kept.contains(&t1) && kept.contains(&t2), "{fields}: {kept}");
        assert!(d.alive(), "{fields}: daemon exited");
        assert!(d.sock_path().exists());
    }
}

// ---------------------------------------------------------------------------
// #517: `close` with no browser connection creates nothing.
//
// Here the real CLI starts the daemon itself, the way an agent's commands do:
// nothing is started by the test. The fake is the extension relay (its url in
// the private relay dir) or, with `relay: false`, the endpoint named by a
// config file (AGENT_BROWSER_CONFIG). The host manifest is absent from the
// private HOME and AGENT_BROWSER_NO_AUTO_OPEN is set, so no Web Store page is
// opened and no Chrome is launched on the machine running the tests.
// ---------------------------------------------------------------------------

struct Cli {
    home: tempfile::TempDir,
    sock: tempfile::TempDir,
    relay: tempfile::TempDir,
    config: PathBuf,
    session: String,
}

impl Cli {
    fn new(session: &str, endpoint: &str, relay: bool) -> Self {
        let home = tempfile::tempdir().unwrap();
        let sock = tempfile::Builder::new()
            .prefix("cun")
            .tempdir_in("/tmp")
            .unwrap();
        let relay_dir = tempfile::tempdir().unwrap();
        let config = home.path().join("cu-test-config.json");
        if relay {
            std::fs::write(relay_dir.path().join("relay-cdp-url"), endpoint).unwrap();
            std::fs::write(&config, "{}").unwrap();
        } else {
            std::fs::write(&config, json!({ "cdp": endpoint }).to_string()).unwrap();
        }
        Cli {
            home,
            sock,
            relay: relay_dir,
            config,
            session: session.to_string(),
        }
    }

    fn command(&self, extra_env: &[(&str, &str)]) -> Command {
        let mut c = Command::new(BIN);
        c.env("HOME", self.home.path())
            .env_remove("XDG_CONFIG_HOME")
            .env("AGENT_BROWSER_SOCKET_DIR", self.sock.path())
            .env("CHROME_USE_RELAY_DIR", self.relay.path())
            .env("AGENT_BROWSER_CONFIG", &self.config)
            .env("AGENT_BROWSER_NO_AUTO_OPEN", "1")
            .env("AGENT_BROWSER_NO_AUTO_RECONNECT", "1")
            .env_remove("AGENT_BROWSER_CDP")
            .env_remove("AGENT_BROWSER_AUTO_CONNECT")
            .env_remove("AGENT_BROWSER_NO_AUTO_CONNECT")
            .env_remove("AGENT_BROWSER_PROVIDER")
            .env_remove("AGENT_BROWSER_SESSION")
            .env_remove("AGENT_BROWSER_IDLE_TIMEOUT_MS")
            .env("NO_COLOR", "1")
            .stdin(Stdio::null());
        for (k, v) in extra_env {
            c.env(k, v);
        }
        c
    }

    /// The real CLI, `--json`, starting the daemon when there is none.
    fn run(&self, args: &[&str], extra_env: &[(&str, &str)]) -> Value {
        let out = self
            .command(extra_env)
            .args(["--json", "--session", &self.session])
            .args(args)
            .output()
            .expect("run chrome-use");
        serde_json::from_slice(&out.stdout)
            .unwrap_or_else(|e| panic!("{args:?}: {e}: {}", text(&out)))
    }

    fn sock_path(&self) -> PathBuf {
        self.sock.path().join(format!("{}.sock", self.session))
    }

    fn record_path(&self) -> PathBuf {
        self.sock
            .path()
            .join(format!("{}.created-targets.json", self.session))
    }

    /// The pid of the daemon the CLI started for this session, if any.
    fn daemon_pid(&self) -> Option<i32> {
        std::fs::read_to_string(self.sock.path().join(format!("{}.pid", self.session)))
            .ok()?
            .trim()
            .parse()
            .ok()
    }

    fn daemon_alive(&self) -> bool {
        self.daemon_pid().is_some_and(|pid| {
            Command::new("kill")
                .args(["-0", &pid.to_string()])
                .stderr(Stdio::null())
                .status()
                .is_ok_and(|s| s.success())
        })
    }

    /// The session's daemon is gone: no socket, no live process.
    fn wait_no_daemon(&self, budget: Duration) -> bool {
        let started = Instant::now();
        while started.elapsed() < budget {
            if !self.sock_path().exists() && !self.daemon_alive() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        false
    }
}

impl Drop for Cli {
    /// Stop the daemon this test's CLI started, and only that one.
    fn drop(&mut self) {
        if self.daemon_alive() {
            if let Some(pid) = self.daemon_pid() {
                let _ = Command::new("kill").arg(pid.to_string()).status();
            }
        }
    }
}

/// What a command did to the browser: tabs created by CDP, `tabs.create` and
/// `windows.create` over the extension, and connections opened.
#[derive(Debug, PartialEq)]
struct Touched {
    create_target: u32,
    tabs_create: usize,
    windows_create: usize,
    connections: u32,
}

impl Fake {
    fn touched(&self) -> Touched {
        let b = self.0.lock().unwrap();
        Touched {
            create_target: b.created_since_reset,
            tabs_create: b.abext_calls.iter().filter(|c| *c == "tabs.create").count(),
            windows_create: b
                .abext_calls
                .iter()
                .filter(|c| *c == "windows.create")
                .count(),
            connections: b.connections,
        }
    }

    fn reset_counts(&self) {
        let mut b = self.0.lock().unwrap();
        b.created_since_reset = 0;
        b.abext_calls.clear();
        b.connections = 0;
        b.close_requests.clear();
    }
}

const NOTHING: Touched = Touched {
    create_target: 0,
    tabs_create: 0,
    windows_create: 0,
    connections: 0,
};

fn assert_nothing_to_close(r: &Value) {
    assert_eq!(r["success"], true, "{r}");
    assert_eq!(r["data"]["closed"], true, "{r}");
    assert_eq!(r["data"]["tabsClosed"], json!([]), "{r}");
    assert_eq!(r["data"]["nothingToClose"], true, "{r}");
}

/// The second `close` of a session, after the first one closed its tab and
/// ended the daemon: the CLI starts a new daemon for it, and that close must
/// not connect, let alone open a tab to close it again.
fn second_close_case(relay: bool) {
    let (fake, url) = Fake::start(relay);
    let cli = Cli::new(&format!("hc517-second-{}", relay as u8), &url, relay);
    let r = cli.run(&["tab", "list"], &[]);
    assert_eq!(r["success"], true, "{r}");
    let t1 = fake.open_targets();
    assert_eq!(t1.len(), 1, "the first command opens the session's tab");
    let r = cli.run(&["close"], &[]);
    assert_eq!(r["success"], true, "{r}");
    assert_eq!(closed_targets(&r), t1, "{r}");
    assert_ne!(r["data"]["nothingToClose"], true, "{r}");
    assert!(
        cli.wait_no_daemon(Duration::from_secs(30)),
        "first close kept the daemon"
    );
    assert!(
        !cli.record_path().exists(),
        "record left after a complete close"
    );

    fake.reset_counts();
    let r = cli.run(&["close"], &[]);
    assert_nothing_to_close(&r);
    assert_eq!(
        fake.touched(),
        NOTHING,
        "the second close touched the browser"
    );
    assert!(fake.open_targets().is_empty(), "{:?}", fake.open_targets());
    assert!(
        fake.close_requests().is_empty(),
        "{:?}",
        fake.close_requests()
    );
    assert!(cli.wait_no_daemon(Duration::from_secs(30)));

    // The plain-text reply says the same, without claiming a closed browser.
    let out = cli
        .command(&[])
        .args(["--session", &cli.session, "close"])
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", text(&out));
    assert!(text(&out).contains("Nothing to close"), "{}", text(&out));
    assert_eq!(
        fake.touched(),
        NOTHING,
        "the third close touched the browser"
    );
}

#[test]
fn second_close_creates_nothing_over_the_relay() {
    second_close_case(true);
}

#[test]
fn second_close_creates_nothing_on_a_configured_endpoint() {
    second_close_case(false);
}

/// A session name that was never opened.
fn never_opened_case(relay: bool) {
    let (fake, url) = Fake::start(relay);
    fake.0
        .lock()
        .unwrap()
        .tabs
        .push(("USER".to_string(), 7, "https://example.com/mine".into()));
    let cli = Cli::new(&format!("hc517-never-{}", relay as u8), &url, relay);
    let r = cli.run(&["close"], &[]);
    assert_nothing_to_close(&r);
    assert_eq!(
        fake.touched(),
        NOTHING,
        "close of a never-opened session touched the browser"
    );
    assert_eq!(fake.open_targets(), vec!["USER".to_string()]);
    assert!(fake.close_requests().is_empty());
    assert!(cli.wait_no_daemon(Duration::from_secs(30)));
}

#[test]
fn close_of_a_never_opened_session_creates_nothing_over_the_relay() {
    never_opened_case(true);
}

#[test]
fn close_of_a_never_opened_session_creates_nothing_on_a_configured_endpoint() {
    never_opened_case(false);
}

/// Held rights with no daemon: the session's daemon exited on its idle
/// timeout, which leaves the user's Chrome tabs open and their ownership
/// saved. `close` closes exactly those, verified, over a new connection, and
/// creates nothing. `bound`: the session is pinned to relay profile P1 while
/// another profile (P2) is the relay's last-connected default; the close goes
/// to P1 only.
fn held_rights_case(bound: bool) {
    let (fake, url) = Fake::start(true);
    fake.0
        .lock()
        .unwrap()
        .tabs
        .push(("USER".to_string(), 7, "https://example.com/mine".into()));
    let cli = Cli::new(&format!("hc517-held-{}", bound as u8), &url, true);
    let other = if bound {
        let (other, other_url) = Fake::start(true);
        let dir = cli.relay.path();
        std::fs::write(
            dir.join("relay-ext-profile-P1"),
            r#"{"id": "P1", "email": "p1@example.test"}"#,
        )
        .unwrap();
        std::fs::write(dir.join("relay-cdp-url-P1"), &url).unwrap();
        std::fs::write(
            dir.join("relay-ext-profile-P2"),
            r#"{"id": "P2", "email": "p2@example.test"}"#,
        )
        .unwrap();
        std::fs::write(dir.join("relay-cdp-url-P2"), &other_url).unwrap();
        // The generic endpoint is P2's: the host that connected last.
        std::fs::write(dir.join("relay-cdp-url"), &other_url).unwrap();
        std::fs::write(
            cli.sock
                .path()
                .join(format!("{}.relay-profile", cli.session)),
            "P1",
        )
        .unwrap();
        Some(other)
    } else {
        None
    };

    let idle = [("AGENT_BROWSER_IDLE_TIMEOUT_MS", "1500")];
    let r = cli.run(&["tab", "list"], &idle);
    assert_eq!(r["success"], true, "{r}");
    // The first `tab new` may take over the blank first tab; the second opens
    // another.
    for _ in 0..2 {
        let r = cli.run(&["tab", "new"], &idle);
        assert_eq!(r["success"], true, "{r}");
    }
    let mut owned: Vec<String> = fake
        .open_targets()
        .into_iter()
        .filter(|t| t != "USER")
        .collect();
    owned.sort();
    assert!(owned.len() >= 2, "{owned:?}");
    if let Some(other) = &other {
        assert!(other.open_targets().is_empty(), "the session left P1");
    }
    // The idle timeout ends the daemon; the tabs and their record stay.
    assert!(
        cli.wait_no_daemon(Duration::from_secs(30)),
        "daemon did not idle out"
    );
    let saved = std::fs::read_to_string(cli.record_path()).expect("ownership saved");
    assert!(owned.iter().all(|t| saved.contains(t)), "{saved}");

    fake.reset_counts();
    if let Some(other) = &other {
        other.reset_counts();
    }
    let r = cli.run(&["close"], &[]);
    assert_eq!(r["success"], true, "{r}");
    assert_eq!(r["data"]["verifiedAbsent"], true, "{r}");
    assert_ne!(r["data"]["nothingToClose"], true, "{r}");
    assert_eq!(closed_targets(&r), owned, "{r}");
    let touched = fake.touched();
    assert_eq!(
        (
            touched.create_target,
            touched.tabs_create,
            touched.windows_create
        ),
        (0, 0, 0),
        "close created something: {touched:?}"
    );
    assert_eq!(fake.open_targets(), vec!["USER".to_string()]);
    assert!(!fake.close_requests().contains(&"USER".to_string()));
    if let Some(other) = &other {
        assert_eq!(other.touched(), NOTHING, "close went to another profile");
    }
    assert!(
        !cli.record_path().exists(),
        "record left after a complete close"
    );
    assert!(cli.wait_no_daemon(Duration::from_secs(30)));
}

#[test]
fn close_with_saved_rights_and_no_daemon_closes_them_and_creates_nothing() {
    held_rights_case(false);
}

#[test]
fn close_with_saved_rights_closes_them_in_the_bound_profile_only() {
    held_rights_case(true);
}

/// `close incomplete` with no daemon: a saved tab whose close is refused
/// keeps its record (and the daemon, which holds the retry), and nothing is
/// created. The retry once Chrome behaves closes it.
#[test]
fn incomplete_close_of_saved_rights_keeps_them() {
    let (fake, url) = Fake::start(true);
    let cli = Cli::new("hc517-incomplete", &url, true);
    let idle = [("AGENT_BROWSER_IDLE_TIMEOUT_MS", "1500")];
    let r = cli.run(&["tab", "list"], &idle);
    assert_eq!(r["success"], true, "{r}");
    let owned = fake.open_targets();
    assert_eq!(owned.len(), 1);
    assert!(
        cli.wait_no_daemon(Duration::from_secs(30)),
        "daemon did not idle out"
    );
    fake.refuse(&owned[0]);
    fake.reset_counts();
    let r = cli.run(&["close"], &[]);
    assert_eq!(r["success"], false, "{r}");
    let error = r["error"].as_str().unwrap_or_default();
    assert!(error.contains("close incomplete"), "{error}");
    assert_eq!(fake.touched().create_target, 0);
    assert_eq!(fake.open_targets(), owned);
    let saved = std::fs::read_to_string(cli.record_path()).expect("ownership kept");
    assert!(saved.contains(&owned[0]), "{saved}");
    fake.behave();
    let r = cli.run(&["close"], &[]);
    assert_eq!(r["success"], true, "{r}");
    assert_eq!(closed_targets(&r), owned, "{r}");
    assert_eq!(fake.touched().create_target, 0);
    assert!(fake.open_targets().is_empty());
    assert!(cli.wait_no_daemon(Duration::from_secs(30)));
}
