//! Confirmation navigation over a synthetic CDP server; never starts Chrome.
#![cfg(unix)]
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

#[derive(Default)]
struct Browser {
    pages: HashMap<String, String>,
    calls: Vec<Value>,
    next: usize,
}
impl Browser {
    fn reply(&mut self, request: &Value) -> Value {
        self.calls.push(request.clone());
        let params = &request["params"];
        let target = request["sessionId"]
            .as_str()
            .unwrap_or("")
            .trim_start_matches("S-");
        let url = self
            .pages
            .get(target)
            .cloned()
            .unwrap_or_else(|| "about:blank".into());
        match request["method"].as_str().unwrap() {
            "Browser.getVersion" => {
                json!({"product":"Chrome/156","protocolVersion":"1.3","userAgent":"synthetic","revision":"1","jsVersion":"1"})
            }
            "Target.getTargets" => {
                json!({"targetInfos":self.pages.iter().map(|(id,url)|json!({"targetId":id,"type":"page","url":url,"title":"Synthetic","attached":true})).collect::<Vec<_>>()})
            }
            "Target.createTarget" => {
                self.next += 1;
                let target = format!("N{}", self.next);
                self.pages.insert(
                    target.clone(),
                    params["url"].as_str().unwrap_or("about:blank").into(),
                );
                json!({"targetId":target})
            }
            "Target.attachToTarget" => {
                json!({"sessionId":format!("S-{}",params["targetId"].as_str().unwrap())})
            }
            "Target.getTargetInfo" => {
                let id = params["targetId"]
                    .as_str()
                    .or_else(|| self.pages.contains_key(target).then_some(target))
                    .or_else(|| self.pages.keys().next().map(String::as_str))
                    .unwrap_or("browser");
                json!({"targetInfo":{"targetId":id,"type":"page","url":self.pages.get(id),"title":"Synthetic","attached":true}})
            }
            "Target.closeTarget" => {
                self.pages.remove(params["targetId"].as_str().unwrap_or(""));
                json!({"success":true})
            }
            "Page.getFrameTree" => {
                json!({"frameTree":{"frame":{"id":target,"loaderId":"L1","url":url,"securityOrigin":url::Url::parse(&url).map(|u|u.origin().ascii_serialization()).unwrap_or_else(|_|"null".into())}}})
            }
            "Page.createIsolatedWorld" => json!({"executionContextId":42}),
            "Page.navigate" => {
                assert!(
                    self.pages.contains_key(target),
                    "navigation must target a created page: {request}"
                );
                self.pages
                    .insert(target.into(), params["url"].as_str().unwrap().into());
                json!({"frameId":target})
            }
            "Runtime.evaluate" => {
                let expression = params["expression"].as_str().unwrap_or("");
                let value = if expression.trim() == "location.href" {
                    Some(url)
                } else if expression.contains("document.readyState") {
                    Some("complete".into())
                } else if expression.trim() == "document.title" {
                    Some("Synthetic".into())
                } else {
                    None
                };
                match value {
                    Some(value) => json!({"result":{"type":"string","value":value}}),
                    None => json!({"result":{"type":"undefined"}}),
                }
            }
            _ => json!({}),
        }
    }
}
fn fake_browser() -> (Arc<Mutex<Browser>>, String) {
    let state = Arc::new(Mutex::new(Browser::default()));
    let server_state = state.clone();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(async move {
                let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
                tx.send(format!(
                    "ws://{}/devtools/browser/synthetic",
                    listener.local_addr().unwrap()
                ))
                .unwrap();
                loop {
                    let (stream, _) = listener.accept().await.unwrap();
                    let state = server_state.clone();
                    tokio::spawn(async move {
                        let mut ws = tokio_tungstenite::accept_async(stream).await.unwrap();
                        while let Some(Ok(tokio_tungstenite::tungstenite::Message::Text(raw))) =
                            ws.next().await
                        {
                            let request: Value = serde_json::from_str(&raw).unwrap();
                            let result = state.lock().unwrap().reply(&request);
                            if ws
                                .send(tokio_tungstenite::tungstenite::Message::Text(
                                    json!({"id":request["id"],"result":result}).to_string(),
                                ))
                                .await
                                .is_err()
                            {
                                break;
                            }
                        }
                    });
                }
            });
    });
    (state, rx.recv().unwrap())
}
struct Daemon {
    child: Child,
    home: tempfile::TempDir,
    socket: tempfile::TempDir,
}
impl Daemon {
    fn start(endpoint: &str) -> Self {
        let home = tempfile::tempdir().unwrap();
        let socket = tempfile::tempdir_in("/tmp").unwrap();
        let child = Command::new(env!("CARGO_BIN_EXE_chrome-use"))
            .env("AGENT_BROWSER_DAEMON", "1")
            .env("AGENT_BROWSER_SESSION", "approve-nav")
            .env("HOME", home.path())
            .env("AGENT_BROWSER_SOCKET_DIR", socket.path())
            .env("CHROME_USE_RELAY_DIR", home.path().join("relay"))
            .env("AGENT_BROWSER_CDP", endpoint)
            .env("AGENT_BROWSER_CONFIRM_ACTIONS", "navigate")
            .env("AGENT_BROWSER_NO_AUTO_RECONNECT", "1")
            .env_remove("AGENT_BROWSER_AUTO_CONNECT")
            .env_remove("AGENT_BROWSER_PROVIDER")
            .env_remove("AGENT_BROWSER_FORCE_LAUNCH")
            .env("NO_COLOR", "1")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let daemon = Self {
            child,
            home,
            socket,
        };
        let start = Instant::now();
        while !daemon.socket.path().join("approve-nav.sock").exists() {
            assert!(start.elapsed() < Duration::from_secs(15));
            std::thread::sleep(Duration::from_millis(10));
        }
        daemon
    }
    fn send(&self, command: Value) -> Value {
        let mut stream = UnixStream::connect(self.socket.path().join("approve-nav.sock")).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(30)))
            .unwrap();
        writeln!(stream, "{command}").unwrap();
        let mut line = String::new();
        BufReader::new(stream).read_line(&mut line).unwrap();
        serde_json::from_str(&line).unwrap_or_else(|_| panic!("invalid daemon reply: {line}"))
    }
}
impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = &self.home;
    }
}

#[test]
fn a_cold_navigation_approval_creates_its_own_target_and_preserves_an_intervening_c() {
    for intervening in [false, true] {
        let (browser, endpoint) = fake_browser();
        let daemon = Daemon::start(&endpoint);
        let cmd = json!({"id":"first-nav","action":"navigate","url":"https://destination.test/path","waitUntil":"none","headers":{"X-Synthetic":"receipt"}});
        let first = daemon.send(cmd);
        assert_eq!(first["data"]["confirmation_required"], true, "{first}");
        assert!(
            browser.lock().unwrap().calls.is_empty(),
            "cold prompt must not launch or connect"
        );
        let protected = if intervening {
            let created = daemon.send(
                json!({"id":"create-c","action":"tab_new","url":"https://untouched.test/form"}),
            );
            assert_eq!(created["success"], true, "{created}");
            let tabs = daemon.send(json!({"id":"tabs-c","action":"tab_list"}));
            Some(
                tabs["data"]["tabs"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .find(|t| t["active"] == true)
                    .unwrap()["targetId"]
                    .as_str()
                    .unwrap()
                    .to_string(),
            )
        } else {
            None
        };
        let before = browser.lock().unwrap().calls.len();
        let result = daemon
            .send(json!({"id":"approve-nav","action":"confirm","confirmationId":"first-nav"}));
        assert_eq!(result["success"], true, "{result}");
        assert_eq!(result["data"]["confirmed"], true, "{result}");
        let state = browser.lock().unwrap();
        let calls = &state.calls[before..];
        let navigation = calls
            .iter()
            .find(|r| r["method"] == "Page.navigate")
            .expect("approved navigation sent");
        assert_eq!(navigation["params"]["url"], "https://destination.test/path");
        let target = navigation["sessionId"]
            .as_str()
            .unwrap()
            .trim_start_matches("S-");
        assert!(
            calls.iter().any(|r| r["method"] == "Target.createTarget"),
            "new own target is mandatory"
        );
        if let Some(c) = protected {
            assert_ne!(target, c);
            assert_eq!(state.pages[&c], "https://untouched.test/form");
            assert!(!calls.iter().any(|r| r["sessionId"] == format!("S-{c}")
                && matches!(r["method"].as_str(), Some("Page.navigate" | "Fetch.enable"))));
        }
    }
}

#[test]
fn a_blank_owned_target_can_be_approved_for_https_without_a_source_dom_origin() {
    let (browser, endpoint) = fake_browser();
    let daemon = Daemon::start(&endpoint);
    let created = daemon.send(json!({"id":"blank","action":"tab_new"}));
    assert_eq!(created["success"], true, "{created}");
    let first=daemon.send(json!({"id":"blank-nav","action":"navigate","url":"https://destination.test/blank","waitUntil":"none"}));
    assert_eq!(first["data"]["confirmation_required"], true, "{first}");
    let before = browser.lock().unwrap().calls.len();
    let result =
        daemon.send(json!({"id":"approve-blank","action":"confirm","confirmationId":"blank-nav"}));
    assert_eq!(result["success"], true, "{result}");
    assert_eq!(result["data"]["confirmed"], true, "{result}");
    assert!(browser.lock().unwrap().calls[before..]
        .iter()
        .any(|r| r["method"] == "Page.navigate"
            && r["params"]["url"] == "https://destination.test/blank"));
}
