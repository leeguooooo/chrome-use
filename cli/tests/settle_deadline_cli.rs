//! The settle wait's report at its deadline (#505), through the real daemon
//! and the real CLI against a fake CDP page. `click --observe` on "Send"
//! starts a request (`Network.requestWillBeSent`) during the wait and
//! completes it (`Network.loadingFinished`) before the deadline; the page
//! never answers the settle's quiet check. No Chrome runs.
//!
//! The reply must not name the finished request, must not call the page
//! quiet, and the click must have been sent once.
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
/// The "Send" button: backend id and box.
const SEND: (i64, f64, f64, f64, f64) = (55, 10.0, 10.0, 100.0, 30.0);
/// How long after the click the request finishes; below the 2s ceiling.
const REQUEST_MS: u64 = 1200;

#[derive(Default)]
struct Page {
    created: bool,
    presses: u32,
}

#[derive(Clone)]
struct Fake {
    page: Arc<Mutex<Page>>,
    /// Events to push to the client (serialized CDP messages).
    out: Arc<Mutex<Option<tokio::sync::mpsc::UnboundedSender<String>>>>,
}

fn inside(x: f64, y: f64) -> bool {
    x >= SEND.1 && x <= SEND.1 + SEND.3 && y >= SEND.2 && y <= SEND.2 + SEND.4
}

impl Fake {
    fn start() -> (Self, String) {
        let fake = Fake {
            page: Arc::new(Mutex::new(Page::default())),
            out: Arc::new(Mutex::new(None)),
        };
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

    fn presses(&self) -> u32 {
        self.page.lock().unwrap().presses
    }

    /// Push an event on the session after `delay`.
    fn push_later(&self, delay: Duration, event: Value) {
        let out = self.out.lock().unwrap().clone();
        if let Some(out) = out {
            tokio::spawn(async move {
                tokio::time::sleep(delay).await;
                let _ = out.send(event.to_string());
            });
        }
    }

    fn ax(bid: i64, role: &str, name: &str) -> Value {
        json!({"nodeId": bid.to_string(), "ignored": false,
               "role": {"type": "role", "value": role},
               "name": {"type": "computedString", "value": name},
               "backendDOMNodeId": bid, "parentId": "1", "childIds": []})
    }

    /// `None` means: never answer this request.
    fn reply(&self, req: &Value) -> Option<Result<Value, String>> {
        let method = req["method"].as_str().unwrap_or("").to_string();
        let params = &req["params"];
        let mut p = self.page.lock().unwrap();
        Some(Ok(match method.as_str() {
            "Target.getTargets" => {
                let infos: Vec<Value> = p
                    .created
                    .then(|| {
                        json!({"targetId": "T1", "type": "page", "title": "Send",
                               "url": "https://send.test/", "attached": true,
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
                "title": "Send", "url": "https://send.test/", "attached": true}}),
            "Browser.getVersion" => json!({"protocolVersion": "1.3", "product": "Chrome/1",
                "revision": "1", "userAgent": "fake", "jsVersion": "1"}),
            "Page.getFrameTree" => json!({"frameTree": {"frame": {
                "id": "T1", "loaderId": "L1", "url": "https://send.test/",
                "securityOrigin": "https://send.test", "mimeType": "text/html"}}}),
            "Accessibility.getFullAXTree" => json!({"nodes": [
                {"nodeId": "1", "ignored": false,
                 "role": {"type": "role", "value": "RootWebArea"},
                 "name": {"type": "computedString", "value": "Send"},
                 "backendDOMNodeId": 1, "childIds": ["55"]},
                Self::ax(SEND.0, "button", "Send")]}),
            "Accessibility.getPartialAXTree" => {
                if params["backendNodeId"].as_i64() == Some(SEND.0) {
                    json!({"nodes": [Self::ax(SEND.0, "button", "Send")]})
                } else {
                    return Some(Err("No node with given id found".into()));
                }
            }
            "DOM.resolveNode" => json!({"object": {"type": "object", "subtype": "node",
                "objectId": format!("obj-{}", params["backendNodeId"].as_i64().unwrap_or(0))}}),
            "DOM.describeNode" => json!({"node": {"nodeId": 1, "backendNodeId": SEND.0,
                "nodeType": 1, "nodeName": "BUTTON", "localName": "button", "nodeValue": ""}}),
            "DOM.getBoxModel" => {
                let (x, y, w, h) = (SEND.1, SEND.2, SEND.3, SEND.4);
                let q = json!([x, y, x + w, y, x + w, y + h, x, y + h]);
                json!({"model": {"content": q, "padding": q, "border": q, "margin": q,
                                 "width": w as i64, "height": h as i64}})
            }
            "Input.dispatchMouseEvent" => {
                let (x, y) = (
                    params["x"].as_f64().unwrap_or(-1.0),
                    params["y"].as_f64().unwrap_or(-1.0),
                );
                if params["type"] == "mousePressed" && inside(x, y) {
                    p.presses += 1;
                    drop(p);
                    // The click starts a request; it finishes before the
                    // settle's 2s ceiling.
                    self.push_later(
                        Duration::from_millis(5),
                        json!({"method": "Network.requestWillBeSent", "sessionId": "S-T1",
                               "params": {"requestId": "R1", "request": {"method": "POST",
                               "url": "https://send.test/api/save?token=secret"},
                               "wallTime": 0, "timestamp": 0}}),
                    );
                    self.push_later(
                        Duration::from_millis(REQUEST_MS),
                        json!({"method": "Network.loadingFinished", "sessionId": "S-T1",
                               "params": {"requestId": "R1"}}),
                    );
                    return Some(Ok(json!({})));
                }
                json!({})
            }
            "Runtime.evaluate" => {
                let e = params["expression"].as_str().unwrap_or("");
                if e.contains("const QUIET") {
                    // The page never answers the settle's quiet check.
                    return None;
                }
                if e.trim() == "location.href" {
                    json!({"result": {"type": "string", "value": "https://send.test/"}})
                } else {
                    json!({"result": {"type": "undefined"}})
                }
            }
            "Runtime.callFunctionOn" => {
                let f = params["functionDeclaration"].as_str().unwrap_or("");
                if f.contains("const QUIET") {
                    return None;
                }
                json!({"result": {"type": "undefined"}})
            }
            _ => json!({}),
        }))
    }
}

async fn serve(fake: Fake, stream: tokio::net::TcpStream) {
    let Ok(ws) = tokio_tungstenite::accept_async(stream).await else {
        return;
    };
    let (mut sink, mut source) = ws.split();
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<String>();
    *fake.out.lock().unwrap() = Some(tx.clone());
    loop {
        tokio::select! {
            msg = source.next() => {
                let Some(Ok(msg)) = msg else { return };
                let Ok(text) = msg.into_text() else { continue };
                let Ok(req) = serde_json::from_str::<Value>(&text) else { continue };
                let Some(result) = fake.reply(&req) else { continue };
                let mut reply = match result {
                    Ok(r) => json!({"id": req["id"], "result": r}),
                    Err(m) => json!({"id": req["id"], "error": {"code": -32000, "message": m}}),
                };
                if let Some(s) = req.get("sessionId") {
                    reply["sessionId"] = s.clone();
                }
                let _ = tx.send(reply.to_string());
            }
            out = rx.recv() => {
                let Some(out) = out else { return };
                if sink
                    .send(tokio_tungstenite::tungstenite::Message::Text(out))
                    .await
                    .is_err()
                {
                    return;
                }
            }
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
            .prefix("cud")
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
            .env("AGENT_BROWSER_NO_AUTO_OPEN", "1")
            .env("AGENT_BROWSER_HUMANIZE", "off")
            .env_remove("AGENT_BROWSER_AUTO_CONNECT")
            .env_remove("AGENT_BROWSER_PROVIDER")
            .env_remove("AGENT_BROWSER_SETTLE_MS")
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
            .env("AGENT_BROWSER_NO_AUTO_OPEN", "1")
            .env_remove("AGENT_BROWSER_AUTO_CONNECT")
            .env_remove("AGENT_BROWSER_PROVIDER")
            .env_remove("AGENT_BROWSER_SESSION")
            .env("NO_COLOR", "1")
            .args(["--session", &self.session])
            .args(args)
            .output()
            .expect("run chrome-use")
    }
}

impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[test]
fn a_request_finished_before_the_deadline_is_not_reported_and_the_page_is_not_called_quiet() {
    let (fake, cdp) = Fake::start();
    let d = Daemon::start("settle-deadline", &cdp);
    let r = d.send(json!({"id": "l", "action": "tab_list"}));
    assert_eq!(r["success"], true, "fake browser did not connect: {r}");
    let r = d.send(json!({"id": "s", "action": "snapshot", "interactive": true}));
    let snap = r["data"]["snapshot"].as_str().unwrap_or("").to_string();
    let at = snap.find("ref=").expect("a ref") + 4;
    let send_ref: String = snap[at..]
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric())
        .collect();

    let out = d.cli(&[
        "--json",
        "click",
        &format!("@{send_ref}"),
        "--observe",
        "--settle-ms",
        "2000",
    ]);
    let text = String::from_utf8_lossy(&out.stdout).to_string();
    let v: Value = serde_json::from_str(text.trim())
        .unwrap_or_else(|e| panic!("{e}: {text} {}", String::from_utf8_lossy(&out.stderr)));
    let settle = &v["data"]["observed"]["settle"];
    assert!(settle.is_object(), "{v}");
    // The request finished before the deadline: it must not be reported as
    // active, and its query must never appear.
    assert_eq!(settle["pendingRequests"], json!([]), "{v}");
    assert!(!text.contains("token=secret"), "{text}");
    assert!(!text.contains("api/save"), "{text}");
    // The page never answered the quiet check: not quiet.
    assert_eq!(settle["quiet"], false, "{v}");
    let pending = settle["pending"].as_array().cloned().unwrap_or_default();
    assert!(
        pending.iter().any(|p| p == "probe" || p == "unknown"),
        "expected probe/unknown, got {pending:?}: {v}"
    );
    // One click, never resent.
    assert_eq!(fake.presses(), 1, "{v}");
}
