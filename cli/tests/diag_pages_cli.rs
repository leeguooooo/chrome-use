//! `diag pages` (#521) through the real CLI and the stdio MCP server, against
//! a fake browser. In relay mode the fake stands in for the ab-connect relay
//! (with a phantom record of a closed tab and a worker record); in CDP mode
//! it is a Chrome debugging endpoint. Every method the CLI sends is logged,
//! so a test can show that the default run attached nothing, that
//! `--measure` without `--force` touched only pages already attached, and
//! that `--force` attached, measured and released without activating.
//! No daemon and no Chrome run; nothing is shown.
#![cfg(unix)]
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use std::io::Write;
use std::os::unix::net::UnixListener;
use std::path::PathBuf;
use std::process::{Command, Output, Stdio};
use std::sync::{Arc, Mutex};

const BIN: &str = env!("CARGO_BIN_EXE_chrome-use");
const MB: f64 = 1024.0 * 1024.0;

fn bundled_version() -> String {
    let manifest: Value =
        serde_json::from_str(include_str!("../../extensions/ab-connect/manifest.json")).unwrap();
    manifest["version"].as_str().unwrap().to_string()
}

#[derive(Clone, Debug)]
struct Tab {
    target: String,
    chrome_id: i64,
    window: i64,
    index: i64,
    active: bool,
    discarded: bool,
    title: String,
    url: String,
    /// The extension's debugger is on it (relay) / Chrome says attached (CDP).
    attached: bool,
    agent: bool,
    /// heap, nodes, listeners now, and what each getMetrics adds.
    metrics: (f64, f64, f64),
    growth: (f64, f64, f64),
}

fn tab(target: &str, chrome_id: i64, window: i64, index: i64, url: &str) -> Tab {
    Tab {
        target: target.to_string(),
        chrome_id,
        window,
        index,
        active: index == 0,
        discarded: false,
        title: format!("Title of {target}"),
        url: url.to_string(),
        attached: false,
        agent: false,
        metrics: (8.0 * MB, 1500.0, 200.0),
        growth: (0.0, 0.0, 0.0),
    }
}

#[derive(Default)]
struct Browser {
    relay: bool,
    version: String,
    tabs: Vec<Tab>,
    caps: Vec<String>,
    /// Relay records of tabs that closed long ago (#519).
    phantoms: Vec<String>,
    /// A worker the relay (or Chrome) lists.
    worker: Option<(String, String)>,
    /// A dedicated worker auto-attach finds under a CDP page session.
    dedicated_worker: bool,
    log: Vec<(String, Value, Option<String>)>,
    /// Commands sent to a session whose tab was not attached: on the real
    /// extension each would silently attach the tab.
    implicit_attaches: Vec<String>,
}

#[derive(Clone)]
struct Fake(Arc<Mutex<Browser>>);

fn is_attach(method: &str) -> bool {
    matches!(method, "Target.attachToTarget" | "ABExt.attachTabById")
}

fn is_activation(method: &str, params: &Value) -> bool {
    match method {
        "Target.activateTarget" | "Page.bringToFront" | "Browser.setWindowBounds" => true,
        "ABExt.call" => matches!(
            params["method"].as_str().unwrap_or(""),
            "update" | "move" | "group" | "ungroup" | "reload" | "discard"
        ),
        _ => false,
    }
}

impl Fake {
    fn start(b: Browser) -> (Self, String) {
        let fake = Fake(Arc::new(Mutex::new(b)));
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
        (fake, rx.recv().unwrap())
    }

    fn log(&self) -> Vec<(String, Value, Option<String>)> {
        self.0.lock().unwrap().log.clone()
    }

    fn methods(&self) -> Vec<String> {
        self.log().into_iter().map(|(m, _, _)| m).collect()
    }

    fn tab(&self, target: &str) -> Tab {
        self.0
            .lock()
            .unwrap()
            .tabs
            .iter()
            .find(|t| t.target == target)
            .cloned()
            .unwrap()
    }

    fn implicit_attaches(&self) -> Vec<String> {
        self.0.lock().unwrap().implicit_attaches.clone()
    }

    fn chrome_tab_json(t: &Tab) -> Value {
        json!({"id": t.chrome_id, "windowId": t.window, "index": t.index, "active": t.active,
               "pinned": false, "incognito": false, "discarded": t.discarded, "audible": false,
               "groupId": -1, "title": t.title, "url": t.url})
    }

    /// The tab a page session names, and whether it is attached.
    fn session_tab(b: &Browser, session: &str) -> Option<usize> {
        b.tabs.iter().position(|t| {
            if b.relay {
                format!("cb-tab-{}", t.chrome_id) == session
            } else {
                format!("S-{}", t.target) == session
            }
        })
    }

    fn reply(&self, req: &Value) -> (Result<Value, String>, Vec<Value>) {
        let mut b = self.0.lock().unwrap();
        let method = req["method"].as_str().unwrap_or("").to_string();
        let params = req["params"].clone();
        let session = req
            .get("sessionId")
            .and_then(Value::as_str)
            .map(str::to_string);
        b.log
            .push((method.clone(), params.clone(), session.clone()));
        let relay = b.relay;
        let mut events = Vec::new();
        let result = (|| -> Result<Value, String> {
            if let Some(sid) = session.as_deref() {
                if sid.starts_with("W-") || sid.starts_with("DW-") {
                    return match method.as_str() {
                        "Runtime.getHeapUsage" => {
                            Ok(json!({"usedSize": 3.0 * MB, "totalSize": 4.0 * MB}))
                        }
                        _ => Ok(json!({})),
                    };
                }
                let Some(i) = Self::session_tab(&b, sid) else {
                    return Err(format!("unknown sessionId {sid}"));
                };
                if !b.tabs[i].attached {
                    let t = b.tabs[i].target.clone();
                    b.implicit_attaches.push(format!("{method} on {t}"));
                }
                return Ok(match method.as_str() {
                    "Performance.getMetrics" => {
                        let t = &mut b.tabs[i];
                        let (h, n, l) = t.metrics;
                        t.metrics = (h + t.growth.0, n + t.growth.1, l + t.growth.2);
                        json!({"metrics": [
                            {"name": "Timestamp", "value": 1.0},
                            {"name": "Documents", "value": 2.0},
                            {"name": "Nodes", "value": n},
                            {"name": "JSEventListeners", "value": l},
                            {"name": "JSHeapUsedSize", "value": h},
                            {"name": "JSHeapTotalSize", "value": h * 1.5},
                        ]})
                    }
                    "Target.setAutoAttach" if b.dedicated_worker => {
                        events.push(json!({"method": "Target.attachedToTarget", "sessionId": sid,
                            "params": {"sessionId": format!("DW-{}", b.tabs[i].target),
                                "targetInfo": {"targetId": format!("DWT-{}", b.tabs[i].target),
                                    "type": "worker", "title": "", "url": "https://docs.example/dw.js",
                                    "attached": true}, "waitingForDebugger": false}}));
                        json!({})
                    }
                    _ => json!({}),
                });
            }
            Ok(match method.as_str() {
                "ABRelay.getCapabilities" if relay => json!({"capabilities": b.caps}),
                "ABExt.state" if relay => json!({
                    "version": b.version,
                    "connected": true,
                    "ownedTabs": b.tabs.iter().filter(|t| t.agent).map(|t| t.chrome_id).collect::<Vec<_>>(),
                    "groups": [],
                    "attachedTargets": b.tabs.iter().filter(|t| t.attached)
                        .map(|t| json!({"targetId": t.target, "tabId": t.chrome_id, "attached": true}))
                        .collect::<Vec<_>>(),
                }),
                "ABExt.attachedTargets" if relay => json!({
                    "targets": b.tabs.iter().filter(|t| t.attached)
                        .map(|t| json!({"targetId": t.target, "tabId": t.chrome_id}))
                        .collect::<Vec<_>>(),
                }),
                "ABExt.tabPresence" if relay => {
                    let target = params["targetId"].as_str().unwrap_or("").to_string();
                    match b.tabs.iter().find(|t| t.target == target) {
                        Some(t) => json!({"tabPresenceVersion": 1, "targetId": target,
                                          "tabId": t.chrome_id, "presence": "present"}),
                        None => json!({"tabPresenceVersion": 1, "targetId": target,
                                       "presence": "unknown", "listed": false}),
                    }
                }
                "ABExt.call" if relay => {
                    let ns = params["namespace"].as_str().unwrap_or("");
                    let m = params["method"].as_str().unwrap_or("");
                    match (ns, m) {
                        ("windows", "getAll") => json!({"result": [
                            {"id": 1, "type": "normal", "state": "normal"},
                            {"id": 2, "type": "normal", "state": "minimized"}]}),
                        ("tabs", "query") => {
                            let mut tabs = b.tabs.clone();
                            tabs.sort_by_key(|t| (t.window, t.index));
                            json!({"result": tabs.iter().map(Self::chrome_tab_json).collect::<Vec<_>>()})
                        }
                        _ => json!({"result": null}),
                    }
                }
                "ABExt.attachTabById" if relay => {
                    let id = params["chromeTabId"].as_i64().unwrap_or(-1);
                    let Some(t) = b.tabs.iter_mut().find(|t| t.chrome_id == id) else {
                        return Err(format!("attachTabById: Chrome tab {id} does not exist"));
                    };
                    t.attached = true;
                    json!({"attached": true, "chromeTabId": id, "targetId": t.target,
                           "url": t.url, "title": t.title})
                }
                "ABExt.releaseTab" if relay => {
                    if !b.caps.iter().any(|c| c == "releaseTab") {
                        return Err("'ABExt.releaseTab' wasn't found".to_string());
                    }
                    let target = params["targetId"].as_str().unwrap_or("");
                    match b.tabs.iter_mut().find(|t| t.target == target) {
                        Some(t) if t.agent => json!({"released": false, "reason": "agent-owned"}),
                        Some(t) => {
                            t.attached = false;
                            json!({"released": true, "tabId": t.chrome_id})
                        }
                        None => json!({"released": false, "reason": "not-attached"}),
                    }
                }
                "Target.getTargets" => {
                    let mut list: Vec<Value> = b
                        .tabs
                        .iter()
                        .filter(|t| !relay || t.attached)
                        .map(|t| {
                            json!({"targetId": t.target, "type": "page", "title": t.title,
                                        "url": t.url, "attached": t.attached})
                        })
                        .collect();
                    if relay {
                        for p in &b.phantoms {
                            list.push(
                                json!({"targetId": p, "type": "page", "title": "", "url": "",
                                             "attached": true}),
                            );
                        }
                    }
                    if let Some((id, url)) = &b.worker {
                        list.push(
                            json!({"targetId": id, "type": "service_worker", "title": "",
                                         "url": url, "attached": false}),
                        );
                    }
                    json!({"targetInfos": list})
                }
                "Browser.getWindowForTarget" if !relay => {
                    let id = params["targetId"].as_str().unwrap_or("");
                    match b.tabs.iter().find(|t| t.target == id) {
                        Some(t) => json!({"windowId": t.window}),
                        None => return Err("No target with given id".to_string()),
                    }
                }
                "Target.attachToTarget" => {
                    let id = params["targetId"].as_str().unwrap_or("");
                    if b.worker.as_ref().is_some_and(|(w, _)| w == id) {
                        return Ok(json!({"sessionId": format!("W-{id}")}));
                    }
                    if relay {
                        return Err(format!("No such target {id}"));
                    }
                    let Some(t) = b.tabs.iter_mut().find(|t| t.target == id) else {
                        return Err(format!("No target with given id {id}"));
                    };
                    t.attached = true;
                    json!({"sessionId": format!("S-{id}")})
                }
                "Target.detachFromTarget" if !relay => {
                    let sid = params["sessionId"].as_str().unwrap_or("").to_string();
                    if let Some(i) = Self::session_tab(&b, &sid) {
                        b.tabs[i].attached = false;
                    }
                    json!({})
                }
                _ => json!({}),
            })
        })();
        (result, events)
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
        let (result, events) = fake.reply(&req);
        let mut reply = match result {
            Ok(result) => json!({"id": req["id"], "result": result}),
            Err(message) => json!({"id": req["id"], "error": {"code": -32000, "message": message}}),
        };
        if let Some(s) = req.get("sessionId") {
            reply["sessionId"] = s.clone();
        }
        for msg in events.into_iter().chain([reply]) {
            if ws
                .send(tokio_tungstenite::tungstenite::Message::Text(
                    msg.to_string(),
                ))
                .await
                .is_err()
            {
                return;
            }
        }
    }
}

struct Env {
    home: tempfile::TempDir,
    sock: tempfile::TempDir,
    relay: tempfile::TempDir,
    config: PathBuf,
    session: String,
    _others: Vec<UnixListener>,
}

impl Env {
    fn new(session: &str, relay_url: Option<&str>) -> Self {
        let home = tempfile::tempdir().unwrap();
        let sock = tempfile::Builder::new()
            .prefix("cud")
            .tempdir_in("/tmp")
            .unwrap();
        let relay = tempfile::tempdir().unwrap();
        if let Some(url) = relay_url {
            std::fs::write(relay.path().join("relay-cdp-url"), url).unwrap();
        }
        let config = home.path().join("chrome-use.json");
        std::fs::write(&config, "{}").unwrap();
        Env {
            home,
            sock,
            relay,
            config,
            session: session.to_string(),
            _others: Vec::new(),
        }
    }

    /// Session `name` recorded `targets` as created; when `live`, its daemon
    /// socket answers.
    fn session_record(&mut self, name: &str, targets: &[&str], live: bool) {
        std::fs::write(
            self.sock
                .path()
                .join(format!("{name}.created-targets.json")),
            json!({"endpoint_sha256": "0", "target_ids": targets}).to_string(),
        )
        .unwrap();
        if live {
            self._others
                .push(UnixListener::bind(self.sock.path().join(format!("{name}.sock"))).unwrap());
        }
    }

    fn env<'a>(&self, c: &'a mut Command) -> &'a mut Command {
        c.env("AGENT_BROWSER_SOCKET_DIR", self.sock.path())
            .env("HOME", self.home.path())
            .env("CHROME_USE_RELAY_DIR", self.relay.path())
            .env("AGENT_BROWSER_CONFIG", &self.config)
            .env("AGENT_BROWSER_NO_AUTO_OPEN", "1")
            .env("AGENT_BROWSER_NO_AUTO_RECONNECT", "1")
            .env_remove("AGENT_BROWSER_CDP")
            .env_remove("AGENT_BROWSER_SESSION")
            .env_remove("AGENT_BROWSER_AUTO_CONNECT")
            .env_remove("AGENT_BROWSER_PROVIDER")
            .env_remove("AGENT_BROWSER_PROFILE")
            .env_remove("CI")
            .env("NO_COLOR", "1")
    }

    fn cli(&self, args: &[&str]) -> Output {
        let mut c = Command::new(BIN);
        self.env(&mut c)
            .args(["--session", &self.session])
            .args(args)
            .output()
            .expect("run chrome-use")
    }

    /// `--json diag pages <args>`: the document and the exit code.
    fn diag(&self, args: &[&str]) -> (Value, i32) {
        let mut all = vec!["--json", "diag", "pages"];
        all.extend_from_slice(args);
        let out = self.cli(&all);
        let doc = serde_json::from_slice(&out.stdout).unwrap_or_else(|e| {
            panic!(
                "{args:?}: {e}\nstdout: {}\nstderr: {}",
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            )
        });
        (doc, out.status.code().unwrap_or(-1))
    }

    fn mcp(&self, calls: &[(&str, Value)]) -> Vec<Value> {
        let mut c = Command::new(BIN);
        let mut child = self
            .env(&mut c)
            .args(["mcp", "--tools", "all"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let mut input = child.stdin.take().unwrap();
        writeln!(input, "{}", json!({"jsonrpc":"2.0","id":0,"method":"initialize","params":{"protocolVersion":"2025-03-26","capabilities":{},"clientInfo":{"name":"diag","version":"1"}}})).unwrap();
        writeln!(
            input,
            "{}",
            json!({"jsonrpc":"2.0","method":"notifications/initialized"})
        )
        .unwrap();
        for (i, (name, mut arguments)) in calls.iter().cloned().enumerate() {
            arguments["session"] = json!(self.session);
            writeln!(
                input,
                "{}",
                json!({"jsonrpc":"2.0","id": i + 1,"method":"tools/call",
                "params":{"name": name, "arguments": arguments}})
            )
            .unwrap();
        }
        writeln!(
            input,
            "{}",
            json!({"jsonrpc":"2.0","id": 99,"method":"tools/list"})
        )
        .unwrap();
        drop(input);
        let output = child.wait_with_output().unwrap();
        String::from_utf8_lossy(&output.stdout)
            .lines()
            .filter_map(|s| serde_json::from_str(s).ok())
            .collect()
    }
}

fn page<'a>(doc: &'a Value, handle: &str) -> &'a Value {
    doc["pages"]
        .as_array()
        .unwrap_or_else(|| panic!("no pages: {doc}"))
        .iter()
        .find(|p| p["handle"] == handle)
        .unwrap_or_else(|| panic!("no page {handle}: {doc}"))
}

fn no_activation(fake: &Fake, what: &str) {
    let hits: Vec<_> = fake
        .log()
        .into_iter()
        .filter(|(m, p, _)| is_activation(m, p))
        .collect();
    assert!(hits.is_empty(), "{what} activated or moved: {hits:?}");
}

/// The user's Chrome over the relay: this session's agent tab, another live
/// session's tab, three of the user's (one discarded, one in a minimized
/// window), a phantom record and a service worker.
fn relay_profile() -> Browser {
    let mut mine = tab("MINE", 50, 1, 5, "https://app.example/");
    mine.attached = true;
    mine.agent = true;
    mine.active = false;
    let mut other = tab("OTHER", 60, 1, 6, "https://shop.example/cart");
    other.attached = true;
    other.agent = true;
    other.active = false;
    let mut mail = tab(
        "UMAIL",
        101,
        1,
        0,
        "https://mail.example/inbox?session=tok123",
    );
    mail.active = true;
    let docs = tab("UDOCS", 102, 1, 1, "https://docs.example/report");
    let mut news = tab("UNEWS", 201, 2, 0, "https://news.example/");
    news.discarded = true;
    Browser {
        relay: true,
        version: bundled_version(),
        tabs: vec![mine, other, mail, docs, news],
        caps: ["call:call-v1", "state", "attachTabById", "releaseTab"]
            .iter()
            .map(|s| s.to_string())
            .collect(),
        phantoms: vec!["DEAD1".to_string()],
        worker: Some(("SW1".to_string(), "https://app.example/sw.js".to_string())),
        ..Default::default()
    }
}

fn relay_env(url: &str) -> Env {
    let mut env = Env::new("s1", Some(url));
    env.session_record("s1", &["MINE"], false);
    env.session_record("alpha", &["OTHER"], true);
    env
}

#[test]
fn default_run_lists_every_page_and_attaches_nothing() {
    let (fake, url) = Fake::start(relay_profile());
    let env = relay_env(&url);
    let (doc, code) = env.diag(&[]);
    assert_eq!(code, 0, "{doc}");
    assert_eq!(doc["schema"], 1);
    assert_eq!(doc["success"], true);
    assert_eq!(doc["readOnly"], true);
    assert_eq!(doc["connection"]["transport"], "relay");
    assert_eq!(doc["connection"]["state"], "connected");
    assert_eq!(doc["extension"]["version"], bundled_version());
    assert_eq!(doc["extension"]["verdict"], "current");
    assert_eq!(doc["staleRelayRecords"], 1, "{doc}");
    assert_eq!(doc["relayRecords"]["records"], 3, "{doc}");
    assert_eq!(doc["counts"]["pages"], 5);
    assert_eq!(doc["counts"]["windows"], 2);
    assert_eq!(doc["counts"]["self"], 1);
    assert_eq!(doc["counts"]["otherSessions"], 1);
    assert_eq!(doc["counts"]["user"], 3);
    // This session first, then session alpha, then the user's.
    let order: Vec<&str> = doc["pages"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["handle"].as_str().unwrap())
        .collect();
    assert_eq!(
        order,
        [
            "chrome-tab:50",
            "chrome-tab:60",
            "chrome-tab:101",
            "chrome-tab:102",
            "chrome-tab:201"
        ]
    );
    assert_eq!(
        page(&doc, "chrome-tab:50")["owner"],
        json!({"kind": "self"})
    );
    assert_eq!(
        page(&doc, "chrome-tab:60")["owner"],
        json!({"kind": "session", "session": "alpha", "live": true})
    );
    let mail = page(&doc, "chrome-tab:101");
    assert_eq!(mail["owner"], json!({"kind": "user"}));
    assert_eq!(mail["url"], "https://mail.example/inbox?session=tok123");
    assert_eq!(mail["active"], true);
    assert_eq!(mail["visible"], true);
    assert_eq!(mail["attached"], false);
    assert_eq!(mail["windowId"], 1);
    assert_eq!(mail["rendererPid"], Value::Null);
    assert_eq!(mail["metrics"], Value::Null);
    assert_eq!(mail["measure"]["status"], "not_requested");
    // Active in a minimized window is not visible; discarded is said.
    let news = page(&doc, "chrome-tab:201");
    assert_eq!(news["discarded"], true);
    assert_eq!(news["visible"], false);
    assert_eq!(page(&doc, "chrome-tab:50")["targetId"], "MINE");
    assert_eq!(doc["workers"]["total"], 1);
    assert_eq!(doc["workers"]["sites"][0]["site"], "https://app.example");
    assert_eq!(doc["workers"]["sites"][0]["usedSize"], Value::Null);

    // Zero attach calls and nothing sent to any page or worker session.
    let log = fake.log();
    assert!(
        !log.iter().any(|(m, _, _)| is_attach(m)),
        "attached: {:?}",
        fake.methods()
    );
    assert!(
        log.iter().all(|(_, _, s)| s.is_none()),
        "a page session was used: {log:?}"
    );
    assert!(!log.iter().any(|(m, _, _)| m == "ABExt.releaseTab"));
    no_activation(&fake, "diag pages");

    // The text form says the same.
    let out = env.cli(&["diag", "pages"]);
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success(), "{text}");
    assert!(text.contains("5 page(s) in 2 window(s)"), "{text}");
    assert!(text.contains("stale relay records: 1"), "{text}");
    assert!(text.contains("nothing was attached"), "{text}");
    assert!(!fake.log().iter().any(|(m, _, _)| is_attach(m)));

    // Bounded: the limit says what it left out.
    let (doc, code) = env.diag(&["--limit", "2"]);
    assert_eq!(code, 0);
    assert_eq!(doc["counts"]["shown"], 2);
    assert_eq!(doc["omitted"]["pages"], 3);
    assert!(doc["omitted"]["note"]
        .as_str()
        .unwrap()
        .contains("3 page(s) not listed"));
}

#[test]
fn measure_without_force_attaches_nothing_foreign() {
    let (fake, url) = Fake::start(relay_profile());
    let env = relay_env(&url);
    let (doc, code) = env.diag(&["--measure"]);
    assert_eq!(code, 0, "{doc}");
    assert_eq!(doc["readOnly"], true);
    let mine = page(&doc, "chrome-tab:50");
    assert_eq!(mine["measure"]["status"], "measured", "{mine}");
    assert_eq!(mine["measure"]["via"], "existing_attachment");
    assert_eq!(mine["metrics"]["Nodes"], 1500);
    assert_eq!(mine["metrics"]["JSEventListeners"], 200);
    assert_eq!(mine["metrics"]["JSHeapUsedSize"], (8.0 * MB) as i64);
    assert_eq!(mine["metrics"]["Documents"], 2);
    let other = page(&doc, "chrome-tab:60");
    assert_eq!(
        other["measure"]["reason"], "held_by_other_session",
        "{other}"
    );
    assert_eq!(other["metrics"], Value::Null);
    assert_eq!(
        page(&doc, "chrome-tab:102")["measure"]["reason"],
        "needs_force"
    );
    assert_eq!(
        page(&doc, "chrome-tab:201")["measure"]["reason"],
        "discarded"
    );
    assert_eq!(doc["counts"]["measured"], 1);
    assert_eq!(doc["counts"]["skipped"], 4);
    assert_eq!(doc["workers"]["sites"][0]["usedSize"], Value::Null);

    let log = fake.log();
    assert!(
        !log.iter()
            .any(|(m, _, _)| is_attach(m) || m == "ABExt.releaseTab"),
        "{:?}",
        fake.methods()
    );
    // Only this session's attached tab got page commands.
    let sessions: std::collections::HashSet<String> =
        log.iter().filter_map(|(_, _, s)| s.clone()).collect();
    assert_eq!(
        sessions,
        ["cb-tab-50".to_string()].into_iter().collect(),
        "{log:?}"
    );
    assert!(fake.implicit_attaches().is_empty());
    no_activation(&fake, "diag pages --measure");
}

#[test]
fn measure_with_force_attaches_measures_and_releases_without_activating() {
    let (fake, url) = Fake::start(relay_profile());
    let env = relay_env(&url);
    let (doc, code) = env.diag(&["--measure", "--force"]);
    assert_eq!(code, 0, "{doc}");
    assert_eq!(doc["readOnly"], false);
    for h in ["chrome-tab:101", "chrome-tab:102"] {
        let p = page(&doc, h);
        assert_eq!(p["measure"]["status"], "measured", "{p}");
        assert_eq!(p["measure"]["via"], "temporary_attach");
        assert_eq!(p["measure"]["forced"], true);
    }
    // Another session's attached tab: measured as it is, not attached again.
    let other = page(&doc, "chrome-tab:60");
    assert_eq!(other["measure"]["via"], "existing_attachment", "{other}");
    // A discarded tab is never attached (it would reload).
    assert_eq!(
        page(&doc, "chrome-tab:201")["measure"]["reason"],
        "discarded"
    );
    let forced = doc["forcedAttaches"].as_array().unwrap();
    assert_eq!(forced.len(), 2, "{doc}");
    for f in forced {
        assert_eq!(f["release"], "released", "{f}");
        assert_eq!(f["activated"], false);
    }
    assert_eq!(doc["workers"]["sites"][0]["usedSize"], (3.0 * MB) as i64);

    // Per tab: attach, then measure, then release; nothing else attached.
    let log = fake.log();
    for (chrome, target) in [(101, "UMAIL"), (102, "UDOCS")] {
        let at = |pred: &dyn Fn(&(String, Value, Option<String>)) -> bool| {
            log.iter()
                .position(pred)
                .unwrap_or_else(|| panic!("{target}: {log:?}"))
        };
        let attach = at(&|(m, p, _)| m == "ABExt.attachTabById" && p["chromeTabId"] == chrome);
        let session = format!("cb-tab-{chrome}");
        let metrics = at(&|(m, _, s)| {
            m == "Performance.getMetrics" && s.as_deref() == Some(session.as_str())
        });
        let release = at(&|(m, p, _)| m == "ABExt.releaseTab" && p["targetId"] == target);
        assert!(attach < metrics && metrics < release, "{target}: {log:?}");
        assert!(!fake.tab(target).attached, "{target} left attached");
    }
    assert!(!log.iter().any(|(m, p, _)| m == "ABExt.attachTabById"
        && (p["chromeTabId"] == 201 || p["chromeTabId"] == 60)));
    assert!(
        fake.tab("OTHER").attached,
        "another session's tab was released"
    );
    assert!(
        fake.implicit_attaches().is_empty(),
        "{:?}",
        fake.implicit_attaches()
    );
    no_activation(&fake, "diag pages --measure --force");
}

#[test]
fn watch_classifies_a_listener_leak_and_a_heap_leak() {
    let mut b = relay_profile();
    for t in b.tabs.iter_mut() {
        match t.target.as_str() {
            // 400 listeners a sample on the same nodes.
            "UMAIL" => t.growth = (0.0, 0.0, 400.0),
            // 6 MiB a sample: far over 1.5 MiB/min at any interval here.
            "UDOCS" => t.growth = (6.0 * MB, 0.0, 0.0),
            "MINE" => t.growth = (0.0, 3000.0, 0.0),
            _ => {}
        }
    }
    let (fake, url) = Fake::start(b);
    let env = relay_env(&url);
    let (doc, code) = env.diag(&["--watch", "1", "--force"]);
    assert_eq!(code, 0, "{doc}");
    assert_eq!(doc["options"]["measure"], true);
    assert_eq!(doc["watch"]["intervalSeconds"], 1);
    let mail = page(&doc, "chrome-tab:101");
    assert_eq!(mail["leakClass"], "listeners_growing_nodes_flat", "{mail}");
    assert_eq!(mail["delta"]["JSEventListeners"], 400);
    assert_eq!(mail["metricsBefore"]["JSEventListeners"], 200);
    let docs = page(&doc, "chrome-tab:102");
    assert_eq!(docs["leakClass"], "heap_growing", "{docs}");
    assert!(docs["delta"]["heapBytesPerMinute"].as_f64().unwrap() >= 1.5 * MB);
    assert_eq!(page(&doc, "chrome-tab:50")["leakClass"], "nodes_climbing");
    assert_eq!(page(&doc, "chrome-tab:60")["leakClass"], "none");
    assert_eq!(page(&doc, "chrome-tab:201")["leakClass"], Value::Null);
    assert!(
        page(&doc, "chrome-tab:101")["delta"]["seconds"]
            .as_f64()
            .unwrap()
            >= 1.0
    );
    // Each forced tab was attached and released once per sample.
    assert_eq!(doc["forcedAttaches"].as_array().unwrap().len(), 4, "{doc}");
    let attaches = fake
        .methods()
        .iter()
        .filter(|m| *m == "ABExt.attachTabById")
        .count();
    let releases = fake
        .methods()
        .iter()
        .filter(|m| *m == "ABExt.releaseTab")
        .count();
    assert_eq!((attaches, releases), (4, 4));
    assert!(!fake.tab("UMAIL").attached && !fake.tab("UDOCS").attached);
    no_activation(&fake, "diag pages --watch");

    let out = env.cli(&["diag", "pages", "--watch", "1", "--force"]);
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("listeners_growing_nodes_flat"), "{text}");
    assert!(text.contains("heap_growing"), "{text}");
}

#[test]
fn exit_three_when_another_live_session_holds_every_page() {
    let mut b = relay_profile();
    b.tabs.retain(|t| t.target == "OTHER");
    b.worker = None;
    let (fake, url) = Fake::start(b);
    let env = relay_env(&url);
    let (doc, code) = env.diag(&["--measure"]);
    assert_eq!(code, 3, "{doc}");
    assert_eq!(doc["exitCode"], 3);
    assert_eq!(doc["success"], false);
    assert_eq!(doc["exitReason"], "held_by_other_session");
    assert_eq!(
        doc["pages"][0]["measure"]["reason"],
        "held_by_other_session"
    );
    assert!(fake.log().iter().all(|(_, _, s)| s.is_none()));
    // A watch stops at the first sample.
    let (_, code) = env.diag(&["--watch", "1"]);
    assert_eq!(code, 3);
    // Listing is fine; --force measures it as it is.
    assert_eq!(env.diag(&[]).1, 0);
    let (doc, code) = env.diag(&["--measure", "--force"]);
    assert_eq!(code, 0, "{doc}");
    assert_eq!(doc["pages"][0]["measure"]["status"], "measured");
    assert!(!fake.methods().iter().any(|m| is_attach(m)));
    // The text form exits 3 as well.
    let out = env.cli(&["diag", "pages", "--measure"]);
    assert_eq!(out.status.code(), Some(3));
}

#[test]
fn exit_two_when_nothing_is_connected() {
    let env = Env::new("s1", None);
    let (doc, code) = env.diag(&[]);
    assert_eq!(code, 2, "{doc}");
    assert_eq!(doc["schema"], 1);
    assert_eq!(doc["exitReason"], "not_connected");
    assert_eq!(doc["connection"]["state"], "not_connected");
    assert_eq!(doc["pages"], json!([]));
    let out = env.cli(&["diag", "pages", "--measure"]);
    assert_eq!(out.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&out.stderr).contains("relay is not running"));

    // A relay url nobody answers on.
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let env = Env::new("s1", Some(&format!("ws://127.0.0.1:{port}/gone")));
    let (doc, code) = env.diag(&["--measure"]);
    assert_eq!(code, 2, "{doc}");
    assert_eq!(doc["connection"]["transport"], "relay");
    // A --cdp endpoint nobody answers on.
    let env = Env::new("s1", None);
    let (doc, code) = env.diag(&[
        "--cdp",
        &format!("ws://127.0.0.1:{port}/devtools/browser/x"),
    ]);
    assert_eq!(code, 2, "{doc}");

    // Usage errors are 1, never 0/2/3.
    let env = Env::new("s1", None);
    assert_eq!(
        env.cli(&["diag", "pages", "--force"]).status.code(),
        Some(1)
    );
    assert_eq!(env.cli(&["diag", "nope"]).status.code(), Some(1));
}

#[test]
fn an_extension_behind_the_published_one_says_how_to_update() {
    let mut b = relay_profile();
    b.version = "0.5.20".to_string();
    let (_fake, url) = Fake::start(b);
    let env = relay_env(&url);
    // The published version, as the twelve-hour cache holds it (no network).
    std::fs::write(
        env.relay.path().join("store-ext-version"),
        bundled_version(),
    )
    .unwrap();
    let (doc, code) = env.diag(&[]);
    assert_eq!(code, 0, "{doc}");
    let ext = &doc["extension"];
    assert_eq!(ext["version"], "0.5.20");
    assert_eq!(ext["published"], bundled_version());
    assert_eq!(ext["verdict"], "behind_published", "{ext}");
    assert_eq!(ext["behindPublished"], true);
    assert!(
        ext["hint"]
            .as_str()
            .unwrap()
            .contains("chrome://extensions"),
        "{ext}"
    );
}

#[test]
fn an_old_extension_is_never_left_holding_a_forced_tab() {
    let mut b = relay_profile();
    b.caps.retain(|c| c != "releaseTab");
    let (fake, url) = Fake::start(b);
    let env = relay_env(&url);
    let (doc, code) = env.diag(&["--measure", "--force"]);
    assert_eq!(code, 0, "{doc}");
    assert_eq!(
        page(&doc, "chrome-tab:102")["measure"]["reason"],
        "release_unsupported"
    );
    assert!(!fake.methods().iter().any(|m| m == "ABExt.attachTabById"));
}

fn cdp_profile() -> Browser {
    let mut mine = tab("MINE", 0, 1, 0, "https://app.example/");
    mine.attached = false;
    let user = tab("UDOCS", 0, 1, 1, "https://docs.example/report");
    let mut other = tab("OTHER", 0, 2, 0, "https://shop.example/");
    other.attached = true;
    Browser {
        relay: false,
        tabs: vec![mine, user, other],
        worker: Some(("SW1".to_string(), "https://docs.example/sw.js".to_string())),
        dedicated_worker: true,
        ..Default::default()
    }
}

#[test]
fn direct_cdp_measures_on_private_sessions_and_detaches_them() {
    let (fake, url) = Fake::start(cdp_profile());
    let mut env = Env::new("c1", None);
    env.session_record("c1", &["MINE"], false);
    env.session_record("beta", &["OTHER"], true);

    let (doc, code) = env.diag(&["--cdp", &url]);
    assert_eq!(code, 0, "{doc}");
    assert_eq!(doc["connection"]["transport"], "cdp");
    assert_eq!(doc["extension"]["applies"], false);
    assert_eq!(doc["staleRelayRecords"], Value::Null);
    assert_eq!(page(&doc, "MINE")["owner"]["kind"], "self");
    assert_eq!(page(&doc, "UDOCS")["windowId"], 1);
    assert_eq!(page(&doc, "UDOCS")["active"], Value::Null);
    assert!(
        !fake.methods().iter().any(|m| is_attach(m)),
        "{:?}",
        fake.methods()
    );

    // --measure: own page on a private session; the user's needs --force;
    // another live session's is left alone.
    let (doc, code) = env.diag(&["--cdp", &url, "--measure"]);
    assert_eq!(code, 0, "{doc}");
    assert_eq!(page(&doc, "MINE")["measure"]["via"], "cdp_session");
    assert_eq!(page(&doc, "UDOCS")["measure"]["reason"], "needs_force");
    assert_eq!(
        page(&doc, "OTHER")["measure"]["reason"],
        "held_by_other_session"
    );
    let attached: Vec<String> = fake
        .log()
        .into_iter()
        .filter(|(m, _, _)| m == "Target.attachToTarget")
        .map(|(_, p, _)| p["targetId"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(attached, ["MINE"], "foreign attach without --force");
    // Its dedicated worker came with it.
    assert_eq!(doc["workers"]["sites"][0]["site"], "https://docs.example");

    let (doc, code) = env.diag(&["--cdp", &url, "--measure", "--force"]);
    assert_eq!(code, 0, "{doc}");
    let udocs = page(&doc, "UDOCS");
    assert_eq!(udocs["measure"]["status"], "measured", "{udocs}");
    assert_eq!(udocs["measure"]["forced"], true);
    let f = doc["forcedAttaches"]
        .as_array()
        .unwrap()
        .iter()
        .find(|f| f["handle"] == "UDOCS")
        .unwrap_or_else(|| panic!("{doc}"));
    assert_eq!(f["release"], "detached");
    // Every private page session was detached, and none is left attached.
    let log = fake.log();
    let attaches = log
        .iter()
        .filter(|(m, p, _)| m == "Target.attachToTarget" && p["targetId"] != "SW1")
        .count();
    let detaches = log
        .iter()
        .filter(|(m, p, _)| {
            m == "Target.detachFromTarget"
                && p["sessionId"].as_str().unwrap_or("").starts_with("S-")
        })
        .count();
    assert_eq!(attaches, detaches, "{log:?}");
    assert!(!fake.tab("UDOCS").attached && !fake.tab("MINE").attached);
    let sites = doc["workers"]["sites"].as_array().unwrap();
    assert_eq!(sites[0]["site"], "https://docs.example", "{doc}");
    assert!(sites[0]["measured"].as_u64().unwrap() >= 2, "{doc}");
    no_activation(&fake, "diag pages over CDP");
}

#[test]
fn mcp_runs_diag_pages() {
    let (fake, url) = Fake::start(relay_profile());
    let env = relay_env(&url);
    let replies = env.mcp(&[
        ("chrome_use_diag_pages", json!({})),
        ("chrome_use_diag_pages", json!({"measure": true})),
    ]);
    let by_id = |id: i64| {
        replies
            .iter()
            .find(|r| r["id"] == id)
            .unwrap_or_else(|| panic!("no reply {id}: {replies:?}"))
    };
    for id in [1, 2] {
        let r = by_id(id);
        assert_eq!(r["result"]["isError"], false, "{r}");
        let doc = &r["result"]["structuredContent"]["response"];
        assert_eq!(doc["schema"], 1, "{r}");
        assert_eq!(r["result"]["structuredContent"]["exitCode"], 0);
    }
    assert_eq!(
        by_id(2)["result"]["structuredContent"]["response"]["counts"]["measured"],
        1
    );
    let tools = by_id(99)["result"]["tools"].as_array().unwrap();
    assert!(tools.iter().any(|t| t["name"] == "chrome_use_diag_pages"));
    assert!(!fake.methods().iter().any(|m| is_attach(m)));
    no_activation(&fake, "MCP diag pages");
}
