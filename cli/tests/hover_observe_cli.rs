//! `hover --observe` (#501) through the real daemon and the real CLI. The
//! browser is a fake CDP endpoint serving one page: a "Menu" button whose
//! two menu links appear once the pointer has moved onto the button, the way
//! a hover menu does. It counts every pointer move sent to it, can deny the
//! capture that follows the hover, and records which element a press lands
//! on. No Chrome runs.
//!
//! What the tests pin down:
//! - the observation lists the menu links the hover revealed, and those refs
//!   are real: clicking one presses that link, not the other;
//! - when the post-hover capture is denied, the hover was sent exactly once,
//!   onto the button, and the reply never says "no change".
#![cfg(unix)]
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::process::{Child, Command, Output, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const BIN: &str = env!("CARGO_BIN_EXE_chrome-use");

/// (backend node id, role, name, box: x, y, w, h) on the page.
const MENU: (i64, &str, &str, f64, f64, f64, f64) = (55, "button", "Menu", 10.0, 10.0, 100.0, 30.0);
const ITEMS: &[(i64, &str, &str, f64, f64, f64, f64)] = &[
    (61, "link", "Item A", 10.0, 50.0, 100.0, 20.0),
    (62, "link", "Item B", 10.0, 80.0, 100.0, 20.0),
];

#[derive(Default)]
struct Page {
    created: bool,
    /// The menu links exist once the pointer has been on the button.
    menu_open: bool,
    /// Every `mouseMoved` the page received, as (x, y).
    moves: Vec<(f64, f64)>,
    /// What each `mousePressed` landed on (the name of the element whose box
    /// holds the point, or "nothing").
    presses: Vec<String>,
    /// Deny the next full accessibility read after the menu opened.
    deny_capture_after_open: bool,
    denials: u32,
}

#[derive(Clone)]
struct Fake(Arc<Mutex<Page>>);

fn inside(x: f64, y: f64, b: (f64, f64, f64, f64)) -> bool {
    x >= b.0 && x <= b.0 + b.2 && y >= b.1 && y <= b.1 + b.3
}

impl Fake {
    fn start() -> (Self, String) {
        let fake = Fake(Arc::new(Mutex::new(Page::default())));
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

    fn page<R>(&self, f: impl FnOnce(&mut Page) -> R) -> R {
        f(&mut self.0.lock().unwrap())
    }

    fn element_box(bid: i64) -> Option<(f64, f64, f64, f64)> {
        std::iter::once(&MENU)
            .chain(ITEMS.iter())
            .find(|n| n.0 == bid)
            .map(|n| (n.3, n.4, n.5, n.6))
    }

    fn name_at(p: &Page, x: f64, y: f64) -> String {
        let mut all = vec![MENU];
        if p.menu_open {
            all.extend_from_slice(ITEMS);
        }
        all.iter()
            .find(|n| inside(x, y, (n.3, n.4, n.5, n.6)))
            .map(|n| n.2.to_string())
            .unwrap_or_else(|| "nothing".into())
    }

    fn ax_node(bid: i64, role: &str, name: &str) -> Value {
        json!({"nodeId": bid.to_string(), "ignored": false,
               "role": {"type": "role", "value": role},
               "name": {"type": "computedString", "value": name},
               "backendDOMNodeId": bid, "parentId": "1", "childIds": []})
    }

    fn reply(&self, req: &Value) -> Result<Value, String> {
        let mut p = self.0.lock().unwrap();
        let method = req["method"].as_str().unwrap_or("").to_string();
        let params = &req["params"];
        Ok(match method.as_str() {
            "Target.getTargets" => {
                let infos: Vec<Value> = p
                    .created
                    .then(|| {
                        json!({"targetId": "T1", "type": "page", "title": "Menu",
                               "url": "https://menu.test/", "attached": true,
                               "browserContextId": "C1"})
                    })
                    .into_iter()
                    .collect();
                json!({"targetInfos": infos})
            }
            "Target.createTarget" => {
                p.created = true;
                json!({"targetId": "T1"})
            }
            "Target.attachToTarget" => json!({"sessionId": "S-T1"}),
            "Target.getTargetInfo" => json!({"targetInfo": {"targetId": "T1", "type": "page",
                "title": "Menu", "url": "https://menu.test/", "attached": true}}),
            "Browser.getVersion" => json!({"protocolVersion": "1.3", "product": "Chrome/1",
                "revision": "1", "userAgent": "fake", "jsVersion": "1"}),
            "Page.getFrameTree" => json!({"frameTree": {"frame": {
                "id": "T1", "loaderId": "L1", "url": "https://menu.test/",
                "securityOrigin": "https://menu.test", "mimeType": "text/html"}}}),
            "Accessibility.getFullAXTree" => {
                if p.deny_capture_after_open && p.menu_open {
                    p.deny_capture_after_open = false;
                    p.denials += 1;
                    return Err("debugger_access_denied: Chrome blocked debugger access \
                                (fixture)"
                        .into());
                }
                let mut list = vec![MENU];
                if p.menu_open {
                    list.extend_from_slice(ITEMS);
                }
                let mut nodes = vec![json!({"nodeId": "1", "ignored": false,
                    "role": {"type": "role", "value": "RootWebArea"},
                    "name": {"type": "computedString", "value": "Menu"},
                    "backendDOMNodeId": 1,
                    "childIds": list.iter().map(|n| n.0.to_string()).collect::<Vec<_>>()})];
                nodes.extend(list.iter().map(|n| Self::ax_node(n.0, n.1, n.2)));
                json!({"nodes": nodes})
            }
            "Accessibility.getPartialAXTree" => {
                let bid = params["backendNodeId"].as_i64().unwrap_or(0);
                match std::iter::once(&MENU)
                    .chain(ITEMS.iter())
                    .find(|n| n.0 == bid)
                {
                    Some(n) => json!({"nodes": [Self::ax_node(n.0, n.1, n.2)]}),
                    None => return Err("No node with given id found".into()),
                }
            }
            "DOM.resolveNode" => {
                let bid = params["backendNodeId"].as_i64().unwrap_or(0);
                json!({"object": {"type": "object", "subtype": "node",
                                  "objectId": format!("obj-{bid}")}})
            }
            "DOM.describeNode" => {
                let bid = params["backendNodeId"].as_i64().unwrap_or(1);
                json!({"node": {"nodeId": 1, "backendNodeId": bid, "nodeType": 1,
                                "nodeName": "BUTTON", "localName": "button", "nodeValue": ""}})
            }
            "DOM.getBoxModel" => {
                let bid = params["backendNodeId"].as_i64().unwrap_or_else(|| {
                    params["objectId"]
                        .as_str()
                        .and_then(|o| o.strip_prefix("obj-"))
                        .and_then(|b| b.parse().ok())
                        .unwrap_or(0)
                });
                let Some((x, y, w, h)) = Self::element_box(bid) else {
                    return Err("Could not compute box model.".into());
                };
                let q = json!([x, y, x + w, y, x + w, y + h, x, y + h]);
                json!({"model": {"content": q, "padding": q, "border": q, "margin": q,
                                 "width": w as i64, "height": h as i64}})
            }
            "Input.dispatchMouseEvent" => {
                let (x, y) = (
                    params["x"].as_f64().unwrap_or(-1.0),
                    params["y"].as_f64().unwrap_or(-1.0),
                );
                match params["type"].as_str().unwrap_or("") {
                    "mouseMoved" => {
                        p.moves.push((x, y));
                        if inside(x, y, (MENU.3, MENU.4, MENU.5, MENU.6)) {
                            p.menu_open = true;
                        }
                    }
                    "mousePressed" => {
                        let name = Self::name_at(&p, x, y);
                        p.presses.push(name);
                    }
                    _ => {}
                }
                json!({})
            }
            "Runtime.callFunctionOn" => {
                let f = params["functionDeclaration"].as_str().unwrap_or("");
                let obj = params["objectId"].as_str().unwrap_or("");
                // A hover that hit-tests in the page before moving (#500)
                // gets the button's centre; one that records the events it
                // produced is told whether the pointer reached the button.
                if f.contains("transformed-frame") {
                    let bid = obj
                        .strip_prefix("obj-")
                        .and_then(|b| b.parse().ok())
                        .unwrap_or(0);
                    match Self::element_box(bid) {
                        Some((x, y, w, h)) => json!({"result": {"type": "object", "value":
                            {"points": [{"gx": x + w / 2.0, "gy": y + h / 2.0}]}}}),
                        None => json!({"result": {"type": "object", "value": {"error": "no-box"}}}),
                    }
                } else if f.contains("rec.events") {
                    json!({"result": {"type": "object", "objectId": "rec-1"}})
                } else if f.contains("ancestorMode") {
                    let on_menu = p
                        .moves
                        .iter()
                        .any(|m| inside(m.0, m.1, (MENU.3, MENU.4, MENU.5, MENU.6)));
                    json!({"result": {"type": "object",
                        "value": {"hovered": on_menu, "under": "<button>"}}})
                } else {
                    json!({"result": {"type": "undefined"}})
                }
            }
            "Runtime.evaluate" => {
                let e = params["expression"].as_str().unwrap_or("");
                if e.trim() == "location.href" {
                    json!({"result": {"type": "string", "value": "https://menu.test/"}})
                } else {
                    json!({"result": {"type": "undefined"}})
                }
            }
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
    session: String,
    cdp: String,
}

impl Daemon {
    fn start(session: &str, cdp: &str) -> Self {
        let home = tempfile::tempdir().unwrap();
        let sock = tempfile::Builder::new()
            .prefix("cuh")
            .tempdir_in("/tmp")
            .unwrap();
        let relay = tempfile::tempdir().unwrap();
        let child = Command::new(BIN)
            .env("AGENT_BROWSER_DAEMON", "1")
            .env("AGENT_BROWSER_SESSION", session)
            .env("AGENT_BROWSER_SOCKET_DIR", sock.path())
            .env("HOME", home.path())
            .env("CHROME_USE_RELAY_DIR", relay.path())
            .env("AGENT_BROWSER_CDP", cdp)
            .env("AGENT_BROWSER_NO_AUTO_RECONNECT", "1")
            .env("AGENT_BROWSER_HUMANIZE", "off")
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

    fn sock_path(&self) -> PathBuf {
        self.sock.path().join(format!("{}.sock", self.session))
    }

    fn send(&self, cmd: Value) -> Value {
        let mut s = UnixStream::connect(self.sock_path()).unwrap();
        s.set_read_timeout(Some(Duration::from_secs(90))).unwrap();
        writeln!(s, "{cmd}").unwrap();
        let mut line = String::new();
        BufReader::new(&s).read_line(&mut line).unwrap();
        serde_json::from_str(&line).unwrap_or_else(|e| panic!("{e}: {line:?}"))
    }

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
            .args(["--session", &self.session])
            .args(args)
            .output()
            .expect("run chrome-use")
    }

    /// Connect and take the snapshot whose refs the hover uses.
    fn snapshot(&self) -> String {
        let r = self.send(json!({"id": "l", "action": "tab_list"}));
        assert_eq!(r["success"], true, "fake browser did not connect: {r}");
        let r = self.send(json!({"id": "s", "action": "snapshot", "interactive": true}));
        assert_eq!(r["success"], true, "{r}");
        r["data"]["snapshot"].as_str().unwrap_or("").to_string()
    }
}

impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn text(out: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

/// The ref printed for `name` in a tree or an observation.
fn ref_for(tree: &str, name: &str) -> String {
    let line = tree
        .lines()
        .find(|l| l.contains(&format!("\"{name}\"")) && l.contains("ref="))
        .unwrap_or_else(|| panic!("no {name} in {tree}"));
    let at = line.find("ref=").expect("a ref") + 4;
    line[at..]
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric())
        .collect()
}

/// `hover --observe` returns the menu links the hover revealed, and the refs
/// it prints are live: clicking "Item B" presses Item B.
#[test]
fn hover_observe_reveals_the_menu_and_its_refs_click_the_right_item() {
    let (fake, cdp) = Fake::start();
    let d = Daemon::start("hover-observe-menu", &cdp);
    let snap = d.snapshot();
    let menu = ref_for(&snap, "Menu");
    assert!(!snap.contains("Item B"), "{snap}");

    let out = d.cli(&["hover", &format!("@{menu}"), "--observe"]);
    let all = text(&out);
    assert!(out.status.success(), "{all}");
    assert!(!all.contains("not supported for `hover`"), "{all}");
    assert!(all.contains("Item A") && all.contains("Item B"), "{all}");
    assert!(
        fake.page(|p| p
            .moves
            .iter()
            .any(|m| inside(m.0, m.1, (10.0, 10.0, 100.0, 30.0)))),
        "the hover never reached the button: {all}"
    );

    let item_b = ref_for(&all, "Item B");
    let out = d.cli(&["click", &format!("@{item_b}")]);
    let all2 = text(&out);
    assert!(out.status.success(), "{all2}");
    assert_eq!(fake.page(|p| p.presses.clone()), vec!["Item B"], "{all2}");
}

/// The capture after the hover is denied. The hover was sent once, onto the
/// button; the reply says the observation is incomplete and never claims
/// the hover changed nothing.
#[test]
fn a_denied_capture_after_hover_sends_the_input_once_and_never_says_no_change() {
    let (fake, cdp) = Fake::start();
    let d = Daemon::start("hover-observe-deny", &cdp);
    let snap = d.snapshot();
    let menu = ref_for(&snap, "Menu");
    fake.page(|p| p.deny_capture_after_open = true);

    let out = d.cli(&["--json", "hover", &format!("@{menu}"), "--observe"]);
    let all = text(&out);
    assert_eq!(
        fake.page(|p| p.denials),
        1,
        "the capture was not denied: {all}"
    );
    let on_menu: Vec<(f64, f64)> = fake.page(|p| {
        p.moves
            .iter()
            .copied()
            .filter(|m| inside(m.0, m.1, (10.0, 10.0, 100.0, 30.0)))
            .collect()
    });
    let all_moves = fake.page(|p| p.moves.clone());
    // One hover gesture: every move landed on the button, and the gesture
    // was not replayed after the denial.
    assert!(!on_menu.is_empty(), "{all}");
    assert_eq!(
        on_menu.len(),
        all_moves.len(),
        "a move went elsewhere: {all_moves:?}"
    );
    assert!(all_moves.len() <= 2, "the hover was resent: {all_moves:?}");
    let v: Value = serde_json::from_str(String::from_utf8_lossy(&out.stdout).trim())
        .unwrap_or_else(|e| panic!("{e}: {all}"));
    let observed = &v["data"]["observed"];
    assert_ne!(observed["status"], "complete", "{v}");
    assert_ne!(
        observed["changed"], false,
        "a failed capture must not read as no change: {v}"
    );
    assert!(!all.contains("no change"), "{all}");
}
