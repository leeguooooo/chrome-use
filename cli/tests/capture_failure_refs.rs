//! An observed action whose post-action capture is denied
//! (`debugger_access_denied`), through the real daemon and the real CLI
//! `batch`. The browser is a fake CDP endpoint serving one form page; it can
//! deny the first capture after a fill, and can move the page to a new
//! document (a new loaderId) at that moment. No Chrome runs.
//!
//! Before the fix the failed capture wiped the session's refs, so the next
//! batch step failed with "Unknown ref" and the agent repeated the fill.
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

/// (backend node id, role, name) of the form's controls.
const NODES: &[(i64, &str, &str)] = &[
    (55, "textbox", "Email"),
    (56, "textbox", "Name"),
    (57, "button", "Reserve"),
];

#[derive(Default)]
struct Page {
    /// The top frame's document. A new value is a navigation.
    loader: String,
    /// Text typed into the page (`Input.insertText`), in order: one per fill
    /// that actually ran.
    typed: Vec<String>,
    /// Deny the next full accessibility read that follows a fill.
    deny_capture_after_fill: bool,
    /// When that denial fires, also move the page to a new document.
    navigate_on_denial: bool,
    denials: u32,
    /// After that denial, keep the tab blocked: every later page command is
    /// denied too, as while another extension's frame stays on the page.
    stay_blocked: bool,
    blocked: bool,
    /// The session opened its tab (the one page).
    created: bool,
    /// Every method called, for diagnostics.
    log: Vec<String>,
}

#[derive(Clone)]
struct Fake(Arc<Mutex<Page>>);

impl Fake {
    fn start() -> (Self, String) {
        let fake = Fake(Arc::new(Mutex::new(Page {
            loader: "L1".into(),
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

    fn typed(&self) -> Vec<String> {
        self.0.lock().unwrap().typed.clone()
    }

    fn arm_blocked(&self) {
        self.arm(false);
        self.0.lock().unwrap().stay_blocked = true;
    }

    fn arm(&self, navigate: bool) {
        let mut p = self.0.lock().unwrap();
        p.deny_capture_after_fill = true;
        p.navigate_on_denial = navigate;
        p.typed.clear();
        p.denials = 0;
    }

    fn denials(&self) -> u32 {
        self.0.lock().unwrap().denials
    }

    fn ax_node(bid: i64, role: &str, name: &str) -> Value {
        json!({"nodeId": bid.to_string(), "ignored": false,
               "role": {"type": "role", "value": role},
               "name": {"type": "computedString", "value": name},
               "backendDOMNodeId": bid, "parentId": "1", "childIds": []})
    }

    /// `Ok(result)` or `Err(message)` for a CDP error reply.
    fn reply(&self, req: &Value) -> Result<Value, String> {
        let mut p = self.0.lock().unwrap();
        let method = req["method"].as_str().unwrap_or("").to_string();
        let params = &req["params"];
        p.log.push(method.clone());
        if p.blocked && req.get("sessionId").is_some() {
            return Err("Cannot access a chrome-extension:// URL of different extension".into());
        }
        Ok(match method.as_str() {
            "Target.getTargets" => {
                let infos: Vec<Value> = (p.created)
                    .then(|| {
                        json!({"targetId": "T1", "type": "page", "title": "Form",
                                    "url": "https://form.test/", "attached": true,
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
                "title": "Form", "url": "https://form.test/", "attached": true}}),
            "Browser.getVersion" => json!({"protocolVersion": "1.3", "product": "Chrome/1",
                "revision": "1", "userAgent": "fake", "jsVersion": "1"}),
            "Page.getFrameTree" => json!({"frameTree": {"frame": {
                "id": "T1", "loaderId": p.loader, "url": "https://form.test/",
                "securityOrigin": "https://form.test", "mimeType": "text/html"}}}),
            "Accessibility.getFullAXTree" => {
                if p.deny_capture_after_fill && !p.typed.is_empty() {
                    p.deny_capture_after_fill = false;
                    p.denials += 1;
                    if p.navigate_on_denial {
                        p.loader = "L2".into();
                    }
                    p.blocked = p.stay_blocked;
                    return Err("debugger_access_denied: Chrome blocked debugger access \
                                (fixture)"
                        .into());
                }
                let mut nodes = vec![json!({"nodeId": "1", "ignored": false,
                    "role": {"type": "role", "value": "RootWebArea"},
                    "name": {"type": "computedString", "value": "Form"},
                    "backendDOMNodeId": 1,
                    "childIds": NODES.iter().map(|n| n.0.to_string()).collect::<Vec<_>>()})];
                nodes.extend(NODES.iter().map(|(b, r, n)| Self::ax_node(*b, r, n)));
                json!({"nodes": nodes})
            }
            "Accessibility.getPartialAXTree" => {
                let bid = params["backendNodeId"].as_i64().unwrap_or(0);
                match NODES.iter().find(|n| n.0 == bid) {
                    Some((b, r, n)) => json!({"nodes": [Self::ax_node(*b, r, n)]}),
                    None => return Err("No node with given id found".into()),
                }
            }
            "DOM.resolveNode" => {
                let bid = params["backendNodeId"].as_i64().unwrap_or(0);
                json!({"object": {"type": "object", "subtype": "node",
                                  "objectId": format!("obj-{bid}")}})
            }
            "DOM.describeNode" => json!({"node": {"nodeId": 1, "backendNodeId": 1,
                "nodeType": 1, "nodeName": "INPUT", "localName": "input", "nodeValue": ""}}),
            "Input.insertText" => {
                let text = params["text"].as_str().unwrap_or("").to_string();
                p.typed.push(text);
                json!({})
            }
            "Runtime.callFunctionOn" => {
                let f = params["functionDeclaration"].as_str().unwrap_or("");
                if f.contains("input-trusted") {
                    json!({"result": {"type": "string", "value": "input-trusted"}})
                } else {
                    // A read-back of the field: the last value typed.
                    let value = p.typed.last().cloned().unwrap_or_default();
                    json!({"result": {"type": "object", "value": {"ok": true, "value": value}}})
                }
            }
            "Runtime.evaluate" => {
                let e = params["expression"].as_str().unwrap_or("");
                if e.trim() == "location.href" {
                    json!({"result": {"type": "string", "value": "https://form.test/"}})
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
            Err(message) => {
                json!({"id": req["id"], "error": {"code": -32000, "message": message}})
            }
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
            .prefix("cuf")
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
            .args(["--session", &self.session, "--json"])
            .args(args)
            .output()
            .expect("run chrome-use")
    }

    /// Connect and take the snapshot whose refs the batch uses.
    fn snapshot(&self) -> String {
        let r = self.send(json!({"id": "l", "action": "tab_list"}));
        assert_eq!(r["success"], true, "fake browser did not connect: {r}");
        let r = self.send(json!({"id": "s", "action": "snapshot", "interactive": true}));
        assert_eq!(r["success"], true, "{r}");
        let text = r["data"]["snapshot"].as_str().unwrap_or("").to_string();
        assert!(text.contains("Email"), "{r}");
        text
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

/// The ref the snapshot printed for `name`.
fn ref_for(snapshot: &str, name: &str) -> String {
    let line = snapshot
        .lines()
        .find(|l| l.contains(&format!("\"{name}\"")))
        .unwrap_or_else(|| panic!("no {name} in {snapshot}"));
    let at = line.find("ref=").expect("a ref") + 4;
    line[at..]
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric())
        .collect()
}

/// The capture after `fill --observe` is denied. The fill ran (once), the
/// reply says the observation is unavailable and the refs are kept
/// unverified, and the next batch step resolves its ref after the live check
/// and fills — without the first fill ever being sent again.
#[test]
fn a_denied_after_capture_keeps_refs_for_the_next_batch_step() {
    let (fake, cdp) = Fake::start();
    let d = Daemon::start("capfail-keep", &cdp);
    let snap = d.snapshot();
    let email = ref_for(&snap, "Email");
    let name = ref_for(&snap, "Name");

    fake.arm(false);
    let out = d.cli(&[
        "batch",
        &format!("fill @{email} me@example.test --observe"),
        &format!("fill @{name} Ada"),
    ]);
    let all = text(&out);
    assert_eq!(fake.denials(), 1, "the capture was not denied: {all}");
    assert!(out.status.success(), "{all}");
    assert!(!all.contains("Unknown ref"), "{all}");
    assert!(all.contains("kept-unverified"), "{all}");
    assert!(all.contains("debugger_access_denied"), "{all}");
    // The capture is read again only after a recovery, and there is none
    // off the extension relay: no second read, and no second fill.
    assert!(
        all.contains(r#""recovery":"no automatic recovery: not on the extension relay"#),
        "{all}"
    );
    // Each fill ran exactly once: the denied observation repeated nothing.
    assert_eq!(fake.typed(), vec!["me@example.test", "Ada"], "{all}");
}

/// The same denial, but the page moved to a new document at that moment (a
/// navigation the agent never saw). The kept ref must not be acted on: the
/// next step is refused, nothing is re-anchored by role or name, and the
/// first fill is still not repeated.
#[test]
fn a_kept_ref_is_refused_after_the_document_changed() {
    let (fake, cdp) = Fake::start();
    let d = Daemon::start("capfail-nav", &cdp);
    let snap = d.snapshot();
    let email = ref_for(&snap, "Email");
    let name = ref_for(&snap, "Name");

    fake.arm(true);
    let out = d.cli(&[
        "batch",
        &format!("fill @{email} me@example.test --observe"),
        &format!("fill @{name} Ada"),
    ]);
    let all = text(&out);
    assert_eq!(fake.denials(), 1, "{all}");
    assert!(all.contains("navigated to a new document"), "{all}");
    assert!(all.contains("Nothing was acted on"), "{all}");
    assert_eq!(fake.typed(), vec!["me@example.test"], "{all}");

    // A fresh snapshot makes the refs ordinary again.
    let snap = d.snapshot();
    let name = ref_for(&snap, "Name");
    let r = d.send(
        json!({"id": "f", "action": "fill", "selector": format!("@{name}"),
                          "value": "Ada"}),
    );
    assert_eq!(r["success"], true, "{r}");
    assert_eq!(fake.typed(), vec!["me@example.test", "Ada"]);
}

/// The tab stays blocked after the denied capture, and there is no way to
/// clear it here (off the extension relay). The kept ref cannot be checked,
/// so the next step is refused before it does anything, in words that do not
/// suggest the refused step may have run; the first fill is not repeated.
#[test]
fn a_kept_ref_on_a_tab_that_stays_blocked_is_refused_before_acting() {
    let (fake, cdp) = Fake::start();
    let d = Daemon::start("capfail-blocked", &cdp);
    let snap = d.snapshot();
    let email = ref_for(&snap, "Email");
    let name = ref_for(&snap, "Name");

    fake.arm_blocked();
    let out = d.cli(&[
        "batch",
        &format!("fill @{email} me@example.test --observe"),
        &format!("fill @{name} Ada"),
    ]);
    let all = text(&out);
    assert_eq!(fake.denials(), 1, "{all}");
    assert!(all.contains("could not confirm"), "{all}");
    assert!(all.contains("Nothing was acted on"), "{all}");
    assert!(!all.contains("may already have run"), "{all}");
    assert_eq!(fake.typed(), vec!["me@example.test"], "{all}");
}
