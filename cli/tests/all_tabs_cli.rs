//! `tab list --all` and `--force` through the real daemon, the real CLI and
//! the stdio MCP server, against a fake browser. In relay mode the fake
//! stands in for the ab-connect relay: user tabs exist only in
//! `chrome.tabs.query` (the extension never attached them), another
//! session's agent tab is attached and recorded, and every method the daemon
//! sends is logged, so a test can show that listing attached, activated,
//! moved and updated nothing, and that `--force` attached without
//! activating. In direct-CDP mode the fake is a Chrome debugging endpoint
//! with the user's tabs in two windows. No Chrome runs, nothing is shown.
#![cfg(unix)]
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use std::io::Write;
use std::os::unix::net::UnixListener;
use std::path::PathBuf;
use std::process::{Child, Command, Output, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const BIN: &str = env!("CARGO_BIN_EXE_chrome-use");

#[derive(Clone, Debug)]
struct Tab {
    target: String,
    chrome_id: i64,
    window: i64,
    index: i64,
    active: bool,
    pinned: bool,
    title: String,
    url: String,
    /// The relay holds a debugger attachment for this session's use.
    attached: bool,
    /// Attached by another session (the relay holds it, not for us).
    other_session: bool,
    /// The extension created it (an agent tab).
    agent: bool,
}

#[derive(Default)]
struct Browser {
    relay: bool,
    tabs: Vec<Tab>,
    created: u32,
    /// Capabilities the extension announces.
    caps: Vec<String>,
    /// Every method received, with its params.
    log: Vec<(String, Value)>,
}

#[derive(Clone)]
struct Fake(Arc<Mutex<Browser>>);

fn tab(target: &str, chrome_id: i64, window: i64, index: i64, url: &str) -> Tab {
    Tab {
        target: target.to_string(),
        chrome_id,
        window,
        index,
        active: false,
        pinned: false,
        title: format!("Title of {target}"),
        url: url.to_string(),
        attached: false,
        other_session: false,
        agent: false,
    }
}

/// Methods that attach a tab, activate or focus anything, move, update,
/// reload or open/close a tab or window.
fn is_touch(method: &str, params: &Value) -> bool {
    match method {
        "Target.attachToTarget"
        | "Target.activateTarget"
        | "Target.createTarget"
        | "Target.closeTarget"
        | "Page.bringToFront"
        | "Page.reload"
        | "Page.navigate"
        | "Browser.setWindowBounds"
        | "ABExt.attachTabById"
        | "ABExt.adoptByUrl"
        | "ABExt.releaseTab" => true,
        "ABExt.call" => {
            let m = params["method"].as_str().unwrap_or("");
            !matches!(
                m,
                "query" | "get" | "getAll" | "getCurrent" | "getLastFocused"
            )
        }
        _ => false,
    }
}

/// Methods that activate, focus or move: never allowed without `--activate`.
fn is_activation(method: &str, params: &Value) -> bool {
    match method {
        "Target.activateTarget" | "Page.bringToFront" | "Browser.setWindowBounds" => true,
        "ABExt.call" => matches!(
            params["method"].as_str().unwrap_or(""),
            "update" | "move" | "group" | "ungroup"
        ),
        _ => false,
    }
}

impl Fake {
    fn start(relay: bool, tabs: Vec<Tab>, caps: &[&str]) -> (Self, String) {
        let fake = Fake(Arc::new(Mutex::new(Browser {
            relay,
            tabs,
            caps: caps.iter().map(|c| c.to_string()).collect(),
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

    fn mark(&self) -> usize {
        self.0.lock().unwrap().log.len()
    }

    fn since(&self, mark: usize) -> Vec<(String, Value)> {
        self.0.lock().unwrap().log[mark..].to_vec()
    }

    fn open(&self, target: &str) -> bool {
        self.0
            .lock()
            .unwrap()
            .tabs
            .iter()
            .any(|t| t.target == target)
    }

    fn chrome_tab_json(t: &Tab) -> Value {
        json!({"id": t.chrome_id, "windowId": t.window, "index": t.index, "active": t.active,
               "pinned": t.pinned, "incognito": false, "discarded": false, "audible": false,
               "groupId": -1, "title": t.title, "url": t.url})
    }

    fn session_of(relay: bool, t: &Tab) -> String {
        if relay {
            format!("cb-tab-{}", t.chrome_id)
        } else {
            format!("S-{}", t.target)
        }
    }

    fn tab_for_session<'a>(b: &'a Browser, session: Option<&str>) -> Option<&'a Tab> {
        let session = session?;
        b.tabs
            .iter()
            .find(|t| Self::session_of(b.relay, t) == session)
    }

    fn reply(&self, req: &Value) -> Result<Value, String> {
        let mut b = self.0.lock().unwrap();
        let method = req["method"].as_str().unwrap_or("").to_string();
        let params = req["params"].clone();
        b.log.push((method.clone(), params.clone()));
        let relay = b.relay;
        Ok(match method.as_str() {
            "ABRelay.getCapabilities" if relay => json!({"capabilities": b.caps}),
            "ABExt.call" if relay => {
                let ns = params["namespace"].as_str().unwrap_or("");
                let m = params["method"].as_str().unwrap_or("");
                match (ns, m) {
                    ("windows", "getAll") => json!({"result": [{"id": 1, "type": "normal"},
                                                               {"id": 2, "type": "normal"}]}),
                    ("tabs", "query") => {
                        let mut tabs = b.tabs.clone();
                        tabs.sort_by_key(|t| (t.window, t.index));
                        json!({"result": tabs.iter().map(Self::chrome_tab_json).collect::<Vec<_>>()})
                    }
                    ("tabs", "get") => {
                        let id = params["args"][0].as_i64().unwrap_or(-1);
                        match b.tabs.iter().find(|t| t.chrome_id == id) {
                            Some(t) => json!({"result": Self::chrome_tab_json(t)}),
                            None => return Err(format!("tabs.get: No tab with id: {id}.")),
                        }
                    }
                    _ => json!({"result": null}),
                }
            }
            "ABExt.state" if relay => json!({
                "version": "fake",
                "ownedTabs": b.tabs.iter().filter(|t| t.agent).map(|t| t.chrome_id).collect::<Vec<_>>(),
                "groups": [],
                "attachedTargets": b.tabs.iter().filter(|t| t.attached || t.other_session)
                    .map(|t| json!({"targetId": t.target, "tabId": t.chrome_id, "attached": true}))
                    .collect::<Vec<_>>(),
            }),
            "ABExt.attachTabById" if relay => {
                let id = params["chromeTabId"].as_i64().unwrap_or(-1);
                let Some(t) = b.tabs.iter_mut().find(|t| t.chrome_id == id) else {
                    return Err(format!("attachTabById: Chrome tab {id} does not exist"));
                };
                t.attached = true;
                json!({"attached": true, "chromeTabId": id, "targetId": t.target,
                       "url": t.url, "title": t.title, "agentPopup": false})
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
            "ABRelay.getAllTargets" if relay => {
                let list: Vec<Value> = b
                    .tabs
                    .iter()
                    .filter(|t| t.attached || t.other_session)
                    .map(|t| {
                        json!({"targetId": t.target, "type": "page", "title": t.title,
                                    "url": t.url, "attached": true})
                    })
                    .collect();
                json!({"targetInfos": list})
            }
            "Target.getTargets" => {
                // The relay scopes its list to what it holds for this
                // session; Chrome's own endpoint lists every tab.
                let list: Vec<Value> = b
                    .tabs
                    .iter()
                    .filter(|t| !relay || (t.attached && !t.other_session))
                    .map(|t| {
                        json!({"targetId": t.target, "type": "page", "title": t.title,
                                    "url": t.url, "attached": t.attached,
                                    "browserContextId": "C1"})
                    })
                    .collect();
                json!({"targetInfos": list})
            }
            "Browser.getWindowForTarget" if !relay => {
                let id = params["targetId"].as_str().unwrap_or("");
                match b.tabs.iter().find(|t| t.target == id) {
                    Some(t) => json!({"windowId": t.window,
                                      "bounds": {"left": 0, "top": 0, "width": 800, "height": 600,
                                                 "windowState": "normal"}}),
                    None => return Err("No target with given id".to_string()),
                }
            }
            "Target.createTarget" => {
                b.created += 1;
                let n = b.created;
                let id = format!("MINE{n}");
                let mut t = tab(
                    &id,
                    500 + n as i64,
                    1,
                    10 + n as i64,
                    params["url"].as_str().unwrap_or("about:blank"),
                );
                t.attached = true;
                t.agent = relay;
                b.tabs.push(t);
                json!({"targetId": id})
            }
            "Target.attachToTarget" => {
                let id = params["targetId"].as_str().unwrap_or("");
                let Some(t) = b.tabs.iter_mut().find(|t| t.target == id) else {
                    return Err(format!("No target with given id {id}"));
                };
                t.attached = true;
                // The relay now holds it for this session too.
                t.other_session = false;
                let t = t.clone();
                json!({"sessionId": Self::session_of(relay, &t)})
            }
            "Target.closeTarget" => {
                let id = params["targetId"].as_str().unwrap_or("").to_string();
                let existed = b.tabs.iter().any(|t| t.target == id);
                b.tabs.retain(|t| t.target != id);
                json!({"success": existed})
            }
            "ABExt.tabPresence" if relay => {
                let target = params["targetId"].as_str().unwrap_or("").to_string();
                let asked = params["tabId"].as_i64();
                match b.tabs.iter().find(|t| t.target == target) {
                    Some(t) => json!({"tabPresenceVersion": 1, "targetId": target,
                                      "tabId": t.chrome_id, "presence": "present", "url": t.url}),
                    None => json!({"tabPresenceVersion": 1, "targetId": target, "tabId": asked,
                                   "presence": if asked.is_some() { "absent" } else { "unknown" }}),
                }
            }
            "Target.getTargetInfo" => {
                let session = req.get("sessionId").and_then(Value::as_str);
                let id = params["targetId"]
                    .as_str()
                    .map(str::to_string)
                    .or_else(|| Self::tab_for_session(&b, session).map(|t| t.target.clone()));
                let t = id.and_then(|id| b.tabs.iter().find(|t| t.target == id).cloned());
                match t {
                    Some(t) => json!({"targetInfo": {"targetId": t.target, "type": "page",
                                      "title": t.title, "url": t.url, "attached": true}}),
                    None => return Err("No target".to_string()),
                }
            }
            "Runtime.evaluate" | "Runtime.callFunctionOn" => {
                let session = req.get("sessionId").and_then(Value::as_str);
                let t = Self::tab_for_session(&b, session).cloned();
                let expr = params["expression"]
                    .as_str()
                    .or_else(|| params["functionDeclaration"].as_str())
                    .unwrap_or("");
                match t {
                    Some(t) if expr.contains("location.href") => {
                        json!({"result": {"type": "string", "value": t.url}})
                    }
                    Some(t) if expr.contains("document.title") => {
                        json!({"result": {"type": "string", "value": t.title}})
                    }
                    _ => json!({"result": {"type": "number", "value": 1}}),
                }
            }
            "Page.getFrameTree" => {
                let session = req.get("sessionId").and_then(Value::as_str);
                let url = Self::tab_for_session(&b, session)
                    .map(|t| t.url.clone())
                    .unwrap_or_else(|| "about:blank".to_string());
                json!({"frameTree": {"frame": {"id": "F1", "loaderId": "L1", "url": url,
                       "securityOrigin": "", "mimeType": "text/html"}}})
            }
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
    home: tempfile::TempDir,
    sock: tempfile::TempDir,
    relay: tempfile::TempDir,
    config: PathBuf,
    session: String,
    cdp: String,
    /// Stands in for another session's live daemon socket.
    _other: Option<UnixListener>,
}

impl Daemon {
    fn start(session: &str, cdp: &str, relay_mode: bool) -> Self {
        let home = tempfile::tempdir().unwrap();
        let sock = tempfile::Builder::new()
            .prefix("cua")
            .tempdir_in("/tmp")
            .unwrap();
        let relay = tempfile::tempdir().unwrap();
        if relay_mode {
            std::fs::write(relay.path().join("relay-cdp-url"), cdp).unwrap();
        }
        let config = home.path().join("chrome-use.json");
        std::fs::write(&config, "{}").unwrap();
        let mut d = Daemon {
            child: Command::new("/usr/bin/true").spawn().unwrap(),
            home,
            sock,
            relay,
            config,
            session: session.to_string(),
            cdp: cdp.to_string(),
            _other: None,
        };
        let _ = d.child.wait();
        let mut c = Command::new(BIN);
        d.child = d
            .env(&mut c)
            .env("AGENT_BROWSER_DAEMON", "1")
            .env("AGENT_BROWSER_SESSION", session)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
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

    fn env<'a>(&self, c: &'a mut Command) -> &'a mut Command {
        c.env("AGENT_BROWSER_SOCKET_DIR", self.sock.path())
            .env("HOME", self.home.path())
            .env("CHROME_USE_RELAY_DIR", self.relay.path())
            .env("AGENT_BROWSER_CDP", &self.cdp)
            .env("AGENT_BROWSER_CONFIG", &self.config)
            .env("AGENT_BROWSER_NO_AUTO_OPEN", "1")
            .env("AGENT_BROWSER_NO_AUTO_RECONNECT", "1")
            .env_remove("AGENT_BROWSER_AUTO_CONNECT")
            .env_remove("AGENT_BROWSER_PROVIDER")
            .env_remove("CI")
            .env("NO_COLOR", "1")
    }

    /// Another session `name` recorded `targets` as created, and (when
    /// `live`) its daemon is listening.
    fn other_session(&mut self, name: &str, targets: &[&str], live: bool) {
        std::fs::write(
            self.sock
                .path()
                .join(format!("{name}.created-targets.json")),
            json!({"endpoint_sha256": "0", "target_ids": targets}).to_string(),
        )
        .unwrap();
        if live {
            self._other =
                Some(UnixListener::bind(self.sock.path().join(format!("{name}.sock"))).unwrap());
        }
    }

    /// The real CLI against this daemon.
    fn cli(&self, args: &[&str]) -> Output {
        let mut c = Command::new(BIN);
        self.env(&mut c)
            .env_remove("AGENT_BROWSER_SESSION")
            .args(["--session", &self.session])
            .args(args)
            .output()
            .expect("run chrome-use")
    }

    fn json(&self, args: &[&str]) -> Value {
        let mut all = vec!["--json"];
        all.extend_from_slice(args);
        let out = self.cli(&all);
        serde_json::from_slice(&out.stdout).unwrap_or_else(|e| {
            panic!(
                "{args:?}: {e}\nstdout: {}\nstderr: {}",
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            )
        })
    }

    fn ok(&self, args: &[&str]) -> Value {
        let v = self.json(args);
        assert_eq!(v["success"], true, "{args:?}: {v}");
        v
    }

    fn mcp(&self, calls: &[(&str, Value)]) -> Vec<Value> {
        let mut c = Command::new(BIN);
        let mut child = self
            .env(&mut c)
            .env_remove("AGENT_BROWSER_SESSION")
            .args(["mcp", "--tools", "all"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let mut input = child.stdin.take().unwrap();
        writeln!(input, "{}", json!({"jsonrpc":"2.0","id":0,"method":"initialize","params":{"protocolVersion":"2025-03-26","capabilities":{},"clientInfo":{"name":"all-tabs","version":"1"}}})).unwrap();
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
        drop(input);
        let output = child.wait_with_output().unwrap();
        let rows: Vec<Value> = String::from_utf8_lossy(&output.stdout)
            .lines()
            .filter_map(|s| serde_json::from_str(s).ok())
            .collect();
        (1..=calls.len())
            .map(|i| {
                rows.iter()
                    .find(|r| r["id"] == i)
                    .cloned()
                    .unwrap_or_else(|| panic!("no reply {i}: {rows:?}"))
            })
            .collect()
    }

    fn sock_path(&self) -> PathBuf {
        self.sock.path().join(format!("{}.sock", self.session))
    }
}

impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn rows(v: &Value) -> Vec<Value> {
    v["data"]["browserTabs"]
        .as_array()
        .unwrap_or_else(|| panic!("no browserTabs: {v}"))
        .clone()
}

fn handles(v: &Value) -> Vec<String> {
    rows(v)
        .iter()
        .map(|r| r["handle"].as_str().unwrap().to_string())
        .collect()
}

fn row<'a>(rows: &'a [Value], handle: &str) -> &'a Value {
    rows.iter()
        .find(|r| r["handle"] == handle)
        .unwrap_or_else(|| panic!("no row {handle}: {rows:?}"))
}

fn no_touch(log: &[(String, Value)], what: &str) {
    let touched: Vec<&(String, Value)> = log.iter().filter(|(m, p)| is_touch(m, p)).collect();
    assert!(touched.is_empty(), "{what} touched tabs: {touched:?}");
}

fn no_activation(log: &[(String, Value)], what: &str) {
    let hits: Vec<&(String, Value)> = log.iter().filter(|(m, p)| is_activation(m, p)).collect();
    assert!(hits.is_empty(), "{what} activated or moved: {hits:?}");
}

/// The user's Chrome over the relay: two windows of the user's own tabs and
/// another session's agent tab.
fn relay_profile() -> Vec<Tab> {
    let mut a = tab("UA", 101, 1, 0, "https://mail.example/inbox?session=tok123");
    a.active = true;
    a.pinned = true;
    let b = tab("UB", 102, 1, 1, "https://docs.example/report");
    let mut c = tab("UC", 201, 2, 0, "https://news.example/");
    c.active = true;
    let mut other = tab("OTHER1", 301, 2, 1, "https://shop.example/cart");
    other.other_session = true;
    other.agent = true;
    vec![a, b, c, other]
}

const CAPS_0534: &[&str] = &["call:call-v1", "state", "attachTabById", "releaseTab"];
const CAPS_0533: &[&str] = &["call:call-v1", "state", "attachTabById"];

#[test]
fn relay_listing_shows_every_tab_by_owner_and_touches_nothing() {
    let (fake, url) = Fake::start(true, relay_profile(), CAPS_0534);
    let mut d = Daemon::start("at-list-r", &url, true);
    d.other_session("alpha", &["OTHER1"], true);
    // Connect: the session opens its own first tab.
    d.ok(&["tab", "list"]);

    let mark = fake.mark();
    let v = d.ok(&["tab", "list", "--all"]);
    no_touch(&fake.since(mark), "tab list --all");
    let data = &v["data"];
    assert_eq!(data["readOnly"], true, "{v}");
    assert_eq!(data["total"], 5, "{v}");
    assert_eq!(data["omitted"], 0, "{v}");
    assert_eq!(data["windows"], 2, "{v}");
    // Own tab first, then session alpha, then the user's by window/index.
    let own = handles(&v)[0].clone();
    assert_eq!(
        handles(&v)[1..],
        [
            "chrome-tab:301",
            "chrome-tab:101",
            "chrome-tab:102",
            "chrome-tab:201"
        ],
        "{v}"
    );
    let rows = rows(&v);
    let mine = row(&rows, &own);
    assert_eq!(mine["owner"], json!({"kind": "self"}), "{mine}");
    assert_eq!(mine["ownership"], "created");
    assert_eq!(mine["tabId"], "t1", "{mine}");
    assert!(mine.get("needsForce").is_none());
    let other = row(&rows, "chrome-tab:301");
    assert_eq!(
        other["owner"],
        json!({"kind": "session", "session": "alpha", "live": true}),
        "{other}"
    );
    assert_eq!(other["ownerLabel"], "session alpha (live)");
    assert_eq!(other["actOn"], "--tab chrome-tab:301 --force");
    let mail = row(&rows, "chrome-tab:101");
    assert_eq!(mail["owner"], json!({"kind": "user"}));
    assert_eq!(mail["active"], true);
    assert_eq!(mail["pinned"], true);
    assert_eq!(mail["windowId"], 1);
    // The url as Chrome reports it, token included.
    assert_eq!(mail["url"], "https://mail.example/inbox?session=tok123");
    assert_eq!(mail["needsForce"], true);
    assert_eq!(row(&rows, "chrome-tab:102")["active"], false);
    assert!(data["forceHint"].as_str().unwrap().contains("--force"));

    // The text form says the same, grouped, with the rule up front.
    let mark = fake.mark();
    let out = d.cli(&["tab", "list", "--all"]);
    no_touch(&fake.since(mark), "tab list --all (text)");
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success(), "{text}");
    assert!(text.contains("5 tab(s) in 2 window(s)"), "{text}");
    assert!(text.contains("== this session =="), "{text}");
    assert!(
        text.contains("== session alpha (live) — act on one with --tab <handle> --force =="),
        "{text}"
    );
    assert!(
        text.contains("== the user — act on one with --tab <handle> --force =="),
        "{text}"
    );
    let at = |s: &str| {
        text.find(s)
            .unwrap_or_else(|| panic!("{s} missing: {text}"))
    };
    assert!(at("== this session") < at("== session alpha"));
    assert!(at("== session alpha") < at("== the user"));
    assert!(at("chrome-tab:101") < at("chrome-tab:201"), "{text}");

    // Bounded, and says what it left out.
    let v = d.ok(&["tab", "list", "--all", "--limit", "2"]);
    assert_eq!(v["data"]["shown"], 2, "{v}");
    assert_eq!(v["data"]["omitted"], 3, "{v}");
    assert!(v["data"]["leftOut"]
        .as_str()
        .unwrap()
        .contains("3 tab(s) not listed"));
}

#[test]
fn relay_force_acts_in_place_and_close_leaves_forced_tabs_open() {
    let (fake, url) = Fake::start(true, relay_profile(), CAPS_0534);
    let mut d = Daemon::start("at-force-r", &url, true);
    d.other_session("alpha", &["OTHER1"], true);
    d.ok(&["tab", "list"]);

    // Without --force: refused, naming the owner and the flag, nothing attached.
    let mark = fake.mark();
    let v = d.json(&["--tab", "chrome-tab:102", "eval", "location.href"]);
    assert_eq!(v["success"], false, "{v}");
    let e = v["error"].as_str().unwrap();
    assert!(e.contains("belongs to the user"), "{e}");
    assert!(e.contains("--tab chrome-tab:102 --force"), "{e}");
    let v = d.json(&["--tab", "chrome-tab:301", "eval", "location.href"]);
    let e = v["error"].as_str().unwrap();
    assert!(e.contains("session alpha (live)"), "{e}");
    let v = d.json(&["tab", "close", "chrome-tab:101"]);
    assert_eq!(v["success"], false, "{v}");
    let e = v["error"].as_str().unwrap();
    assert!(e.contains("belongs to the user"), "{e}");
    assert!(e.contains("tab close chrome-tab:101 --force"), "{e}");
    no_touch(&fake.since(mark), "refused commands");
    assert!(fake.open("UA"));

    // With --force: runs on that tab, in place, and says so.
    let mark = fake.mark();
    let v = d.ok(&[
        "--tab",
        "chrome-tab:102",
        "--force",
        "eval",
        "location.href",
    ]);
    assert_eq!(v["data"]["result"], "https://docs.example/report", "{v}");
    assert_eq!(v["data"]["forced"], true, "{v}");
    let forced = &v["data"]["forcedTab"];
    assert_eq!(forced["handle"], "chrome-tab:102", "{v}");
    assert_eq!(forced["url"], "https://docs.example/report");
    assert_eq!(forced["title"], "Title of UB");
    assert_eq!(forced["owner"], json!({"kind": "user"}));
    assert_eq!(forced["activated"], false);
    let log = fake.since(mark);
    no_activation(&log, "--tab --force");
    assert!(
        log.iter()
            .any(|(m, p)| m == "ABExt.attachTabById" && p["chromeTabId"] == 102),
        "{log:?}"
    );
    // Listed as this session's now, marked forced.
    let v = d.ok(&["tab", "list", "--all"]);
    let r = rows(&v);
    let ub = row(&r, "chrome-tab:102");
    assert_eq!(ub["owner"]["kind"], "self", "{ub}");
    assert_eq!(ub["forced"], true, "{ub}");

    // Another live session's tab: allowed with --force, with a warning.
    let mark = fake.mark();
    let v = d.ok(&[
        "--tab",
        "chrome-tab:301",
        "--force",
        "eval",
        "location.href",
    ]);
    assert_eq!(v["data"]["result"], "https://shop.example/cart", "{v}");
    let w = v["warning"].as_str().unwrap_or_default();
    assert!(w.contains("session alpha (live)"), "{v}");
    assert!(w.contains("running now"), "{v}");
    no_activation(&fake.since(mark), "--force on another session's tab");

    // `tab close <handle> --force` really closes the user's tab.
    let mark = fake.mark();
    let v = d.ok(&["tab", "close", "chrome-tab:101", "--force"]);
    assert_eq!(v["data"]["closed"], true, "{v}");
    assert_eq!(v["data"]["verifiedAbsent"], true, "{v}");
    assert_eq!(v["data"]["forced"], true, "{v}");
    assert_eq!(
        v["data"]["forcedTab"]["url"],
        "https://mail.example/inbox?session=tok123"
    );
    assert!(!fake.open("UA"), "the user's tab is still open");
    no_activation(&fake.since(mark), "tab close --force");

    // Plain session close: the session's own tab goes, forced tabs stay open
    // and are released, never closed.
    let mark = fake.mark();
    let v = d.ok(&["close"]);
    let left = v["data"]["forcedTabsLeftOpen"]
        .as_array()
        .unwrap_or_else(|| panic!("{v}"));
    let mut released: Vec<(String, String)> = left
        .iter()
        .map(|t| {
            (
                t["targetId"].as_str().unwrap().to_string(),
                t["release"].as_str().unwrap().to_string(),
            )
        })
        .collect();
    released.sort();
    assert_eq!(
        released,
        vec![
            (
                "OTHER1".to_string(),
                "not released: agent-owned".to_string()
            ),
            ("UB".to_string(), "released".to_string()),
        ],
        "{v}"
    );
    assert!(fake.open("UB"), "a forced user tab was closed by `close`");
    assert!(fake.open("OTHER1"), "another session's tab was closed");
    assert!(fake.open("UC"));
    let log = fake.since(mark);
    let closed: Vec<&Value> = log
        .iter()
        .filter(|(m, _)| m == "Target.closeTarget")
        .map(|(_, p)| &p["targetId"])
        .collect();
    assert!(
        closed
            .iter()
            .all(|t| t.as_str().unwrap().starts_with("MINE")),
        "{closed:?}"
    );
}

#[test]
fn an_0533_extension_keeps_forced_tabs_attached_and_says_so() {
    let (fake, url) = Fake::start(true, relay_profile(), CAPS_0533);
    let d = Daemon::start("at-old-r", &url, true);
    d.ok(&["tab", "list"]);
    d.ok(&[
        "--tab",
        "chrome-tab:201",
        "--force",
        "eval",
        "location.href",
    ]);
    let mark = fake.mark();
    let v = d.ok(&["close"]);
    let left = &v["data"]["forcedTabsLeftOpen"][0];
    assert_eq!(left["targetId"], "UC", "{v}");
    assert!(
        left["release"]
            .as_str()
            .unwrap()
            .contains("needs ab-connect 0.5.34"),
        "{v}"
    );
    assert!(fake.open("UC"));
    assert!(
        !fake
            .since(mark)
            .iter()
            .any(|(m, _)| m == "ABExt.releaseTab"),
        "sent a method 0.5.33 does not have"
    );
}

#[test]
fn direct_cdp_listing_is_observation_and_force_works_in_place() {
    let mut a = tab("UA", 0, 1, 0, "https://mail.example/inbox");
    a.attached = false;
    let b = tab("UB", 0, 1, 1, "https://docs.example/report");
    let c = tab("UC", 0, 2, 0, "https://news.example/");
    let (fake, url) = Fake::start(false, vec![a, b, c], &[]);
    let mut d = Daemon::start("at-cdp", &url, false);
    d.other_session("beta", &["UC"], false);
    d.ok(&["tab", "new", "https://mine.example/"]);

    let mark = fake.mark();
    let v = d.ok(&["tab", "list", "--all"]);
    no_touch(&fake.since(mark), "tab list --all over CDP");
    assert_eq!(v["data"]["total"], 4, "{v}");
    assert_eq!(v["data"]["source"], "cdp:Target.getTargets");
    let h = handles(&v);
    assert_eq!(h[0], "MINE1", "{v}");
    assert_eq!(h[1], "UC", "{v}");
    let r = rows(&v);
    assert_eq!(
        row(&r, "UC")["owner"],
        json!({"kind": "session", "session": "beta", "live": false})
    );
    assert_eq!(row(&r, "UC")["windowId"], 2);
    assert_eq!(row(&r, "UA")["owner"], json!({"kind": "user"}));
    assert_eq!(row(&r, "UA")["windowId"], 1);

    // The session's own `tab list` names the user's tab by `t<N>`.
    let list = d.ok(&["tab", "list"]);
    let ua_ref = list["data"]["tabs"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["targetId"] == "UA")
        .map(|t| t["tabId"].as_str().unwrap().to_string())
        .unwrap_or_else(|| panic!("{list}"));

    // Without --force: refused with the owner and the flag.
    let v = d.json(&["--tab", &ua_ref, "eval", "location.href"]);
    let e = v["error"].as_str().unwrap_or_else(|| panic!("{v}"));
    assert!(e.contains("belongs to the user"), "{e}");
    assert!(e.contains("--tab UA --force"), "{e}");
    let v = d.json(&["tab", "close", &ua_ref]);
    let e = v["error"].as_str().unwrap_or_else(|| panic!("{v}"));
    assert!(e.contains("belongs to the user"), "{e}");
    assert!(e.contains("--force"), "{e}");
    assert!(fake.open("UA"));

    // With --force, by targetId: runs there, nothing activated.
    let mark = fake.mark();
    let v = d.ok(&["--tab", "UB", "--force", "eval", "location.href"]);
    assert_eq!(v["data"]["result"], "https://docs.example/report", "{v}");
    assert_eq!(v["data"]["forcedTab"]["owner"]["kind"], "user");
    no_activation(&fake.since(mark), "--force over CDP");

    let v = d.ok(&["tab", "close", &ua_ref, "--force"]);
    assert_eq!(v["data"]["verifiedAbsent"], true, "{v}");
    assert!(!fake.open("UA"));

    let mark = fake.mark();
    let v = d.ok(&["close"]);
    let left = &v["data"]["forcedTabsLeftOpen"][0];
    assert_eq!(left["targetId"], "UB", "{v}");
    assert_eq!(left["release"], "detached", "{v}");
    assert!(fake.open("UB"), "a forced tab was closed by `close`");
    assert!(fake.open("UC"));
    let log = fake.since(mark);
    assert!(
        log.iter().any(|(m, _)| m == "Target.detachFromTarget"),
        "{log:?}"
    );
    assert!(
        !log.iter()
            .any(|(m, p)| m == "Target.closeTarget" && p["targetId"] != "MINE1"),
        "{log:?}"
    );
}

#[test]
fn mcp_lists_all_tabs_and_forces_only_on_request() {
    let (fake, url) = Fake::start(true, relay_profile(), CAPS_0534);
    let d = Daemon::start("at-mcp", &url, true);
    d.ok(&["tab", "list"]);
    let mark = fake.mark();
    let replies = d.mcp(&[
        (
            "chrome_use_tabs",
            json!({"action": "list", "all": true, "limit": 50}),
        ),
        (
            "chrome_use_eval",
            json!({"script": "location.href", "tabId": "chrome-tab:102"}),
        ),
    ]);
    no_touch(&fake.since(mark), "MCP list all + refused eval");
    let list = &replies[0]["result"]["structuredContent"]["response"]["data"];
    assert_eq!(list["total"], 5, "{}", replies[0]);
    // An agent tab no record names: another session, unnamed.
    assert_eq!(list["browserTabs"][1]["owner"]["kind"], "session", "{list}");
    assert_eq!(list["browserTabs"][2]["owner"]["kind"], "user", "{list}");
    let refused = replies[1].to_string();
    assert!(refused.contains("--force"), "{refused}");
    assert!(refused.contains("belongs to"), "{refused}");

    let mark = fake.mark();
    let replies = d.mcp(&[(
        "chrome_use_eval",
        json!({"script": "location.href", "tabId": "chrome-tab:102", "force": true}),
    )]);
    let data = &replies[0]["result"]["structuredContent"]["response"]["data"];
    assert_eq!(
        data["result"], "https://docs.example/report",
        "{}",
        replies[0]
    );
    assert_eq!(data["forced"], true, "{data}");
    assert_eq!(data["forcedTab"]["owner"]["kind"], "user");
    no_activation(&fake.since(mark), "MCP eval force");
}
