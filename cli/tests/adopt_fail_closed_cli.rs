//! `adopt <url>` that finds no tab fails closed (#507), through the real CLI
//! and a daemon the CLI starts itself, against a fake extension relay.
//!
//! The fake models what the relay and ab-connect do with the user's tabs:
//! they exist in `chrome.tabs` only (the extension never attached them),
//! `Target.getTargets` is scoped to the tab group a client announced, an
//! attach tags the tab into the attaching client's group, and
//! `ABExt.adoptByUrl` resolves a spec against `chrome.tabs`. Every method is
//! logged with the session it was sent to, so a test can show that no user
//! tab was attached, evaluated, activated or navigated. No Chrome runs and
//! nothing is shown.
#![cfg(unix)]
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use std::path::PathBuf;
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};
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
    title: String,
    url: String,
    /// The relay holds a debugger attachment for it.
    held: bool,
    /// The relay's group tag (the owning session's group).
    group: Option<String>,
    /// The user's own tab (never to be touched).
    user: bool,
}

#[derive(Default)]
struct Browser {
    tabs: Vec<Tab>,
    created: u32,
    /// Connection id -> announced group.
    groups: std::collections::HashMap<u64, String>,
    /// (connection, method, params, sessionId) for every request.
    log: Vec<(u64, String, Value, Option<String>)>,
}

#[derive(Clone)]
struct Fake(Arc<Mutex<Browser>>);

static NEXT_CONN: AtomicU64 = AtomicU64::new(1);

fn user_tab(target: &str, chrome_id: i64, window: i64, index: i64, url: &str) -> Tab {
    Tab {
        target: target.to_string(),
        chrome_id,
        window,
        index,
        active: false,
        title: format!("Title of {target}"),
        url: url.to_string(),
        held: false,
        group: None,
        user: true,
    }
}

/// The user's Chrome: several of their own tabs in two windows, one of them
/// focused, none attached.
fn user_tabs() -> Vec<Tab> {
    let mut a = user_tab("UA", 101, 1, 0, "https://mail.example/inbox");
    a.active = true;
    let b = user_tab("UB", 102, 1, 1, "https://bank.example/account");
    let mut c = user_tab("UC", 201, 2, 0, "https://news.example/");
    c.active = true;
    let d = user_tab("UD", 202, 2, 1, "https://social.example/feed");
    vec![a, b, c, d]
}

fn session_of(t: &Tab) -> String {
    format!("cb-tab-{}", t.chrome_id)
}

fn chrome_tab_json(t: &Tab) -> Value {
    json!({"id": t.chrome_id, "windowId": t.window, "index": t.index, "active": t.active,
           "pinned": false, "incognito": false, "discarded": false, "audible": false,
           "groupId": -1, "title": t.title, "url": t.url})
}

fn target_json(t: &Tab) -> Value {
    json!({"targetId": t.target, "type": "page", "title": t.title, "url": t.url,
           "attached": true, "browserContextId": "C1"})
}

impl Fake {
    fn start(tabs: Vec<Tab>) -> (Self, String) {
        let fake = Fake(Arc::new(Mutex::new(Browser {
            tabs,
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
                    let conn = NEXT_CONN.fetch_add(1, Ordering::SeqCst);
                    tokio::spawn(async move { serve(shared, conn, stream).await });
                }
            });
        });
        let url = rx.recv().unwrap();
        (fake, url)
    }

    fn mark(&self) -> usize {
        self.0.lock().unwrap().log.len()
    }

    fn since(&self, mark: usize) -> Vec<(u64, String, Value, Option<String>)> {
        self.0.lock().unwrap().log[mark..].to_vec()
    }

    fn created(&self) -> u32 {
        self.0.lock().unwrap().created
    }

    fn reply(&self, conn: u64, req: &Value) -> Result<Value, String> {
        let mut b = self.0.lock().unwrap();
        let method = req["method"].as_str().unwrap_or("").to_string();
        let params = req["params"].clone();
        let session = req
            .get("sessionId")
            .and_then(Value::as_str)
            .map(str::to_string);
        b.log
            .push((conn, method.clone(), params.clone(), session.clone()));
        let group = b.groups.get(&conn).cloned();
        let tab_for_session = |b: &Browser| {
            session
                .as_deref()
                .and_then(|s| b.tabs.iter().find(|t| session_of(t) == s).cloned())
        };
        Ok(match method.as_str() {
            "ABRelay.setGroup" => {
                if let Some(g) = params["group"].as_str().filter(|g| !g.is_empty()) {
                    b.groups.insert(conn, g.to_string());
                }
                json!({})
            }
            "ABRelay.getCapabilities" => {
                json!({"capabilities": ["call:call-v1", "state", "attachTabById", "releaseTab"]})
            }
            "Target.setDiscoverTargets" | "Target.setAutoAttach" => json!({}),
            "Target.getTargets" => {
                // Scoped to the client's announced group, like the relay.
                let list: Vec<Value> = b
                    .tabs
                    .iter()
                    .filter(|t| t.held)
                    .filter(|t| match &group {
                        Some(g) => t.group.as_deref() == Some(g.as_str()),
                        None => true,
                    })
                    .map(target_json)
                    .collect();
                json!({"targetInfos": list})
            }
            "ABRelay.getAllTargets" => {
                let list: Vec<Value> = b.tabs.iter().filter(|t| t.held).map(target_json).collect();
                json!({"targetInfos": list})
            }
            "ABExt.attachedTargets" => json!({"targets": b.tabs.iter().filter(|t| t.held)
                .map(|t| json!({"targetId": t.target, "tabId": t.chrome_id, "attached": true}))
                .collect::<Vec<_>>()}),
            "ABExt.state" => json!({
                "version": "fake",
                "ownedTabs": b.tabs.iter().filter(|t| !t.user).map(|t| t.chrome_id).collect::<Vec<_>>(),
                "groups": [],
                "attachedTargets": b.tabs.iter().filter(|t| t.held)
                    .map(|t| json!({"targetId": t.target, "tabId": t.chrome_id, "attached": true}))
                    .collect::<Vec<_>>(),
            }),
            "ABExt.call" => {
                let ns = params["namespace"].as_str().unwrap_or("");
                let m = params["method"].as_str().unwrap_or("");
                match (ns, m) {
                    ("windows", "getAll") => json!({"result": [{"id": 1, "type": "normal"},
                                                               {"id": 2, "type": "normal"}]}),
                    ("tabs", "query") => {
                        let mut tabs = b.tabs.clone();
                        tabs.sort_by_key(|t| (t.window, t.index));
                        json!({"result": tabs.iter().map(chrome_tab_json).collect::<Vec<_>>()})
                    }
                    ("tabs", "get") => {
                        let id = params["args"][0].as_i64().unwrap_or(-1);
                        match b.tabs.iter().find(|t| t.chrome_id == id) {
                            Some(t) => json!({"result": chrome_tab_json(t)}),
                            None => return Err(format!("tabs.get: No tab with id: {id}.")),
                        }
                    }
                    _ => json!({"result": null}),
                }
            }
            "ABExt.adoptByUrl" => {
                // ab-connect: resolve against chrome.tabs, attach only a match.
                let spec = params["spec"].as_str().unwrap_or("").trim().to_lowercase();
                let found = b.tabs.iter_mut().find(|t| {
                    t.target.to_lowercase() == spec
                        || (!spec.is_empty() && t.url.to_lowercase().contains(&spec))
                });
                match found {
                    Some(t) => {
                        t.held = true;
                        json!({"targetId": t.target, "url": t.url, "title": t.title})
                    }
                    None => json!({
                        "targetId": null,
                        "candidates": b.tabs.iter()
                            .map(|t| json!({"url": t.url, "title": t.title}))
                            .collect::<Vec<_>>(),
                    }),
                }
            }
            "ABExt.attachTabById" => {
                let id = params["chromeTabId"].as_i64().unwrap_or(-1);
                let Some(t) = b.tabs.iter_mut().find(|t| t.chrome_id == id) else {
                    return Err(format!("attachTabById: Chrome tab {id} does not exist"));
                };
                t.held = true;
                json!({"attached": true, "chromeTabId": id, "targetId": t.target,
                       "url": t.url, "title": t.title, "agentPopup": false})
            }
            "ABExt.releaseTab" => {
                let target = params["targetId"].as_str().unwrap_or("");
                match b.tabs.iter_mut().find(|t| t.target == target) {
                    Some(t) if !t.user => json!({"released": false, "reason": "agent-owned"}),
                    Some(t) => {
                        t.held = false;
                        t.group = None;
                        json!({"released": true, "tabId": t.chrome_id})
                    }
                    None => json!({"released": false, "reason": "not-attached"}),
                }
            }
            "ABExt.tabPresence" => {
                let target = params["targetId"].as_str().unwrap_or("").to_string();
                let asked = params["tabId"].as_i64();
                match b.tabs.iter().find(|t| t.target == target) {
                    Some(t) => json!({"tabPresenceVersion": 1, "targetId": target,
                                      "tabId": t.chrome_id, "presence": "present", "url": t.url}),
                    None => json!({"tabPresenceVersion": 1, "targetId": target, "tabId": asked,
                                   "presence": if asked.is_some() { "absent" } else { "unknown" }}),
                }
            }
            "Target.createTarget" => {
                b.created += 1;
                let n = b.created;
                let id = format!("MINE{n}");
                let tag = params["agentGroup"]
                    .as_str()
                    .map(str::to_string)
                    .or(group.clone());
                b.tabs.push(Tab {
                    target: id.clone(),
                    chrome_id: 500 + n as i64,
                    window: 9,
                    index: n as i64,
                    active: false,
                    title: String::new(),
                    url: params["url"].as_str().unwrap_or("about:blank").to_string(),
                    held: true,
                    group: tag,
                    user: false,
                });
                json!({"targetId": id})
            }
            "Target.attachToTarget" => {
                let id = params["targetId"].as_str().unwrap_or("");
                let Some(t) = b.tabs.iter_mut().find(|t| t.target == id && t.held) else {
                    return Err(format!("No such target {id}"));
                };
                // The relay tags an attached tab into the attacher's group.
                if let Some(g) = group.clone() {
                    t.group = Some(g);
                }
                json!({"sessionId": session_of(t)})
            }
            "Target.closeTarget" => {
                let id = params["targetId"].as_str().unwrap_or("").to_string();
                let existed = b.tabs.iter().any(|t| t.target == id);
                b.tabs.retain(|t| t.target != id);
                json!({"success": existed})
            }
            "Target.getTargetInfo" => {
                let t = match params["targetId"].as_str() {
                    Some(id) => b.tabs.iter().find(|t| t.target == id).cloned(),
                    None => tab_for_session(&b),
                };
                match t {
                    Some(t) => json!({"targetInfo": target_json(&t)}),
                    None => return Err("No target".to_string()),
                }
            }
            "Page.navigate" => {
                let url = params["url"].as_str().unwrap_or("").to_string();
                if let Some(s) = session.as_deref() {
                    if let Some(t) = b.tabs.iter_mut().find(|t| session_of(t) == s) {
                        t.url = url;
                    }
                }
                json!({"frameId": "F1", "loaderId": "L2"})
            }
            "Runtime.evaluate" | "Runtime.callFunctionOn" => {
                let expr = params["expression"]
                    .as_str()
                    .or_else(|| params["functionDeclaration"].as_str())
                    .unwrap_or("");
                match tab_for_session(&b) {
                    Some(t) if expr.contains("location.href") => {
                        json!({"result": {"type": "string", "value": t.url}})
                    }
                    Some(t) if expr.contains("document.title") => {
                        json!({"result": {"type": "string", "value": t.title}})
                    }
                    Some(_) if expr.contains("readyState") => {
                        json!({"result": {"type": "string", "value": "complete"}})
                    }
                    _ => json!({"result": {"type": "number", "value": 1}}),
                }
            }
            "Page.getFrameTree" => {
                let url = tab_for_session(&b)
                    .map(|t| t.url)
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

async fn serve(fake: Fake, conn: u64, stream: tokio::net::TcpStream) {
    let Ok(mut ws) = tokio_tungstenite::accept_async(stream).await else {
        return;
    };
    while let Some(Ok(msg)) = ws.next().await {
        let Ok(text) = msg.into_text() else { continue };
        let Ok(req) = serde_json::from_str::<Value>(&text) else {
            continue;
        };
        let mut reply = match fake.reply(conn, &req) {
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
        // A navigation loads at once.
        if req["method"] == "Page.navigate" {
            if let Some(s) = req.get("sessionId") {
                for event in [
                    "Page.frameNavigated",
                    "Page.domContentEventFired",
                    "Page.loadEventFired",
                ] {
                    let params = if event == "Page.frameNavigated" {
                        json!({"frame": {"id": "F1", "loaderId": "L2",
                               "url": req["params"]["url"], "securityOrigin": "",
                               "mimeType": "text/html"}})
                    } else {
                        json!({"timestamp": 1.0})
                    };
                    let msg = json!({"method": event, "params": params, "sessionId": s});
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
    }
}

/// The CLI against the fake relay, with a private home, socket dir and relay
/// dir. The CLI starts (and `adopt` restarts) the session's daemon itself.
struct Env {
    home: tempfile::TempDir,
    sock: tempfile::TempDir,
    relay: tempfile::TempDir,
    config: PathBuf,
    session: String,
}

impl Env {
    fn new(session: &str, relay_url: &str) -> Self {
        let home = tempfile::tempdir().unwrap();
        let sock = tempfile::Builder::new()
            .prefix("cua")
            .tempdir_in("/tmp")
            .unwrap();
        let relay = tempfile::tempdir().unwrap();
        std::fs::write(relay.path().join("relay-cdp-url"), relay_url).unwrap();
        let config = home.path().join("chrome-use.json");
        std::fs::write(&config, "{}").unwrap();
        Env {
            home,
            sock,
            relay,
            config,
            session: session.to_string(),
        }
    }

    fn cli(&self, args: &[&str]) -> Output {
        Command::new(BIN)
            .env("AGENT_BROWSER_SOCKET_DIR", self.sock.path())
            .env("HOME", self.home.path())
            .env("CHROME_USE_RELAY_DIR", self.relay.path())
            .env("AGENT_BROWSER_CONFIG", &self.config)
            .env("AGENT_BROWSER_NO_AUTO_OPEN", "1")
            .env("AGENT_BROWSER_NO_AUTO_RECONNECT", "1")
            .env("AGENT_BROWSER_RELAY_REVIVE_SECS", "0")
            .env("AGENT_BROWSER_NO_UPDATE_CHECK", "1")
            .env_remove("AGENT_BROWSER_CDP")
            .env_remove("AGENT_BROWSER_AUTO_CONNECT")
            .env_remove("AGENT_BROWSER_PROVIDER")
            .env_remove("AGENT_BROWSER_SESSION")
            .env_remove("AGENT_BROWSER_ADOPT")
            .env_remove("CI")
            .env("NO_COLOR", "1")
            .args(["--session", &self.session, "--json"])
            .args(args)
            .output()
            .expect("run chrome-use")
    }

    fn json(&self, args: &[&str]) -> (Value, String) {
        let out = self.cli(args);
        let stdout = String::from_utf8_lossy(&out.stdout).to_string();
        let stderr = String::from_utf8_lossy(&out.stderr).to_string();
        let v = stdout
            .lines()
            .rev()
            .find_map(|l| serde_json::from_str::<Value>(l).ok())
            .unwrap_or_else(|| json!({"success": out.status.success(), "raw": stdout}));
        (v, format!("{args:?}\nstdout: {stdout}\nstderr: {stderr}"))
    }

    fn daemon_pid(&self) -> Option<i32> {
        std::fs::read_to_string(self.sock.path().join(format!("{}.pid", self.session)))
            .ok()?
            .trim()
            .parse()
            .ok()
    }
}

impl Drop for Env {
    fn drop(&mut self) {
        if let Some(pid) = self.daemon_pid() {
            let _ = Command::new("kill").arg(pid.to_string()).status();
            let started = Instant::now();
            while started.elapsed() < Duration::from_secs(3)
                && Command::new("kill")
                    .args(["-0", &pid.to_string()])
                    .status()
                    .is_ok_and(|s| s.success())
            {
                std::thread::sleep(Duration::from_millis(50));
            }
            let _ = Command::new("kill").args(["-9", &pid.to_string()]).status();
        }
    }
}

/// Requests that touched one of the user's tabs: attached, evaluated in,
/// navigated, activated or closed it.
fn user_touches(
    fake: &Fake,
    log: &[(u64, String, Value, Option<String>)],
) -> Vec<(String, Value, Option<String>)> {
    let b = fake.0.lock().unwrap();
    let user_targets: Vec<String> = b
        .tabs
        .iter()
        .filter(|t| t.user)
        .map(|t| t.target.clone())
        .collect();
    let user_sessions: Vec<String> = b.tabs.iter().filter(|t| t.user).map(session_of).collect();
    let user_chrome_ids: Vec<i64> = b
        .tabs
        .iter()
        .filter(|t| t.user)
        .map(|t| t.chrome_id)
        .collect();
    log.iter()
        .filter(|(_, method, params, session)| {
            if session
                .as_deref()
                .is_some_and(|s| user_sessions.iter().any(|u| u == s))
            {
                return true;
            }
            let target = params["targetId"].as_str().unwrap_or("");
            let on_user_target = user_targets.iter().any(|u| u == target);
            match method.as_str() {
                "Target.attachToTarget"
                | "Target.activateTarget"
                | "Target.closeTarget"
                | "ABExt.releaseTab" => on_user_target,
                "ABExt.attachTabById" => params["chromeTabId"]
                    .as_i64()
                    .is_some_and(|id| user_chrome_ids.contains(&id)),
                "ABExt.call" => {
                    let m = params["method"].as_str().unwrap_or("");
                    !matches!(
                        m,
                        "query" | "get" | "getAll" | "getCurrent" | "getLastFocused"
                    )
                }
                _ => false,
            }
        })
        .map(|(_, m, p, s)| (m.clone(), p.clone(), s.clone()))
        .collect()
}

fn is_no_current_tab(v: &Value) -> bool {
    let e = v["error"].as_str().unwrap_or("").to_lowercase();
    e.contains("no current tab")
}

/// #507: a failed `adopt` leaves the session with no current tab. Reads and
/// clicks are refused, nothing of the user's is attached or evaluated, and
/// `tab new` still opens a fresh tab of the session's own.
#[test]
fn adopt_with_no_match_fails_closed_and_tab_new_still_opens_a_tab() {
    let (fake, url) = Fake::start(user_tabs());
    let env = Env::new("ad507", &url);
    let mark = fake.mark();

    let (v, ctx) = env.json(&["adopt", "https://creator.nomatch.example/publish?source=x"]);
    assert_eq!(v["success"], false, "adopt with no match succeeded: {ctx}");
    let err = v["error"].as_str().unwrap_or("");
    assert!(
        err.contains("no open tab matching"),
        "adopt must say nothing matched: {ctx}"
    );
    assert!(
        err.to_lowercase().contains("no current tab"),
        "adopt must say the session has no current tab: {ctx}"
    );

    let (v, ctx) = env.json(&["eval", "location.href"]);
    assert_eq!(v["success"], false, "eval ran after a failed adopt: {ctx}");
    assert!(is_no_current_tab(&v), "eval must say no current tab: {ctx}");

    let (v, ctx) = env.json(&["get", "url"]);
    assert_eq!(
        v["success"], false,
        "get url ran after a failed adopt: {ctx}"
    );
    assert!(
        is_no_current_tab(&v),
        "get url must say no current tab: {ctx}"
    );

    let (v, ctx) = env.json(&["click", "#submit"]);
    assert_eq!(v["success"], false, "click ran after a failed adopt: {ctx}");
    assert!(
        is_no_current_tab(&v),
        "click must say no current tab: {ctx}"
    );

    // Nothing so far created a tab, or attached, evaluated in or activated
    // any of the user's.
    let log = fake.since(mark);
    let touched = user_touches(&fake, &log);
    assert!(touched.is_empty(), "user tabs were touched: {touched:?}");
    assert_eq!(fake.created(), 0, "a tab was created before `tab new`");
    let evals: Vec<_> = log
        .iter()
        .filter(|(_, m, _, _)| m.starts_with("Runtime."))
        .collect();
    assert!(evals.is_empty(), "something was evaluated: {evals:?}");

    // `tab new` opens a fresh tab of the session's own, never a user tab.
    let (v, ctx) = env.json(&["tab", "new", "https://fresh.example/start", "--activate"]);
    assert_eq!(v["success"], true, "tab new failed: {ctx}");
    assert!(fake.created() >= 1, "tab new created no tab: {ctx}");
    let (v, ctx) = env.json(&["eval", "location.href"]);
    assert_eq!(v["success"], true, "eval in the new tab failed: {ctx}");
    assert_eq!(
        v["data"]["result"], "https://fresh.example/start",
        "eval did not run in the new tab: {ctx}"
    );

    let touched = user_touches(&fake, &fake.since(mark));
    assert!(touched.is_empty(), "user tabs were touched: {touched:?}");
}

/// The same with the adopt retried and a different command in between: the
/// failed directive is not replayed on later connects, and a second failed
/// adopt in a session that already has its own tab fails closed too.
#[test]
fn failed_adopt_after_a_working_session_drops_the_current_tab() {
    let (fake, url) = Fake::start(user_tabs());
    let env = Env::new("ad507b", &url);

    let (v, ctx) = env.json(&["open", "https://mine.example/"]);
    assert_eq!(v["success"], true, "open failed: {ctx}");
    let mark = fake.mark();

    let (v, ctx) = env.json(&["adopt", "nothing-like-this.example"]);
    assert_eq!(v["success"], false, "adopt with no match succeeded: {ctx}");

    for args in [
        &["eval", "location.href"][..],
        &["snapshot"][..],
        &["click", "#x"][..],
    ] {
        let (v, ctx) = env.json(args);
        assert_eq!(
            v["success"], false,
            "{args:?} ran after a failed adopt: {ctx}"
        );
        assert!(
            is_no_current_tab(&v),
            "{args:?} must say no current tab: {ctx}"
        );
    }
    let touched = user_touches(&fake, &fake.since(mark));
    assert!(touched.is_empty(), "user tabs were touched: {touched:?}");

    // A matching adopt afterwards works and is the session's current tab.
    let (v, ctx) = env.json(&["adopt", "news.example"]);
    assert_eq!(v["success"], true, "a matching adopt failed: {ctx}");
    let (v, ctx) = env.json(&["eval", "location.href"]);
    assert_eq!(v["success"], true, "{ctx}");
    assert_eq!(v["data"]["result"], "https://news.example/", "{ctx}");
}

/// `tab select` of a tab the session does not own (a user tab's handle) is
/// refused without `--force`, and leaves the current tab where it was.
#[test]
fn tab_select_of_a_user_tab_fails_closed() {
    let (fake, url) = Fake::start(user_tabs());
    let env = Env::new("ad507c", &url);
    let (v, ctx) = env.json(&["open", "https://mine.example/"]);
    assert_eq!(v["success"], true, "open failed: {ctx}");
    let mark = fake.mark();

    for handle in ["chrome-tab:102", "UB", "bank.example"] {
        let (v, ctx) = env.json(&["tab", "select", handle]);
        assert_eq!(v["success"], false, "tab select {handle} succeeded: {ctx}");
    }
    let (v, ctx) = env.json(&["eval", "location.href"]);
    assert_eq!(v["success"], true, "{ctx}");
    assert_eq!(v["data"]["result"], "https://mine.example/", "{ctx}");
    let touched = user_touches(&fake, &fake.since(mark));
    assert!(touched.is_empty(), "user tabs were touched: {touched:?}");
}
