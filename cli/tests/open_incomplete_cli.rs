//! An `open` whose page never finishes loading (#502), through the real
//! daemon, the real CLI and the stdio MCP server, on a fake extension relay.
//! The fake answers `Page.navigate` and never sends `load`, so the 25s wait
//! runs out and the CLI reports `navigation_incomplete:`. That message
//! quotes "Timeout waiting for Page.loadEventFired", and here its URL also
//! carries denial or connection words. Whatever it quotes, it must keep its
//! own code with `retryable: false`, run no recovery and never repeat the
//! navigation. No Chrome runs.
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

/// The extension's answer to `Page.navigate` served by `chrome.tabs.update`.
const SYNTHETIC_LOADER: &str = "browser-level-navigation";

#[derive(Default)]
struct Page {
    /// The loader id `Page.navigate` answers with.
    nav_loader: String,
    /// The main frame's document (`Page.getFrameTree`) once navigated.
    frame_loader: String,
    /// Where the tab is.
    url: String,
    created: bool,
    navigates: u32,
    creates: u32,
    tab_updates: u32,
    log: Vec<String>,
}

#[derive(Clone)]
struct Fake(Arc<Mutex<Page>>);

impl Fake {
    fn start() -> (Self, String) {
        let fake = Fake(Arc::new(Mutex::new(Page {
            frame_loader: "L1".into(),
            url: "about:blank".into(),
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

    /// How the next navigation answers: the relay's synthetic loader (no
    /// commit can be bound), or a real loader the frame then shows
    /// (committed, but the document is still loading).
    fn next_navigation(&self, nav_loader: &str, frame_loader: &str) {
        let mut p = self.0.lock().unwrap();
        p.nav_loader = nav_loader.into();
        p.frame_loader = frame_loader.into();
    }

    /// `(Page.navigate, Target.createTarget, tabs.update)` calls so far.
    fn counts(&self) -> (u32, u32, u32) {
        let p = self.0.lock().unwrap();
        (p.navigates, p.creates, p.tab_updates)
    }

    fn log(&self) -> String {
        self.0.lock().unwrap().log.join(" ")
    }

    fn reply(&self, req: &Value) -> Result<Value, String> {
        let mut p = self.0.lock().unwrap();
        let method = req["method"].as_str().unwrap_or("").to_string();
        let params = &req["params"];
        p.log.push(method.clone());
        Ok(match method.as_str() {
            "Target.getTargets" => {
                let infos: Vec<Value> = p
                    .created
                    .then(|| {
                        json!({"targetId": "T1", "type": "page", "title": "Slow",
                               "url": p.url, "attached": true, "browserContextId": "C1"})
                    })
                    .into_iter()
                    .collect();
                json!({"targetInfos": infos})
            }
            "Target.createTarget" => {
                p.creates += 1;
                p.created = true;
                json!({"targetId": "T1"})
            }
            "Target.closeTarget" => json!({"success": true}),
            "ABExt.inspectTab" => json!({"chromeTabId": 11, "windowId": 1, "active": true,
                                         "url": p.url, "title": "Slow"}),
            "ABExt.state" => json!({"ownedTabs": [11]}),
            "ABExt.call" => {
                let call = format!(
                    "{}.{}",
                    params["namespace"].as_str().unwrap_or(""),
                    params["method"].as_str().unwrap_or("")
                );
                match call.as_str() {
                    "windows.get" => json!({"result": {"state": "normal", "focused": true}}),
                    "windows.getAll" => json!({"result": [{"id": 1, "type": "normal"}]}),
                    "tabs.query" => json!({"result": [{"id": 11, "index": 0, "active": true,
                                                       "windowId": 1}]}),
                    "tabs.get" => json!({"result": {"id": 11, "groupId": -1}}),
                    "tabs.update" => {
                        p.tab_updates += 1;
                        json!({"result": {}})
                    }
                    _ => json!({"result": null}),
                }
            }
            "Target.attachToTarget" => json!({"sessionId": "S-T1"}),
            "Target.getTargetInfo" => json!({"targetInfo": {"targetId": "T1", "type": "page",
                "title": "Slow", "url": p.url, "attached": true}}),
            "Browser.getVersion" => json!({"protocolVersion": "1.3", "product": "Chrome/1",
                "revision": "1", "userAgent": "fake", "jsVersion": "1"}),
            // The navigation starts; its `load` never comes.
            "Page.navigate" => {
                p.navigates += 1;
                p.url = params["url"].as_str().unwrap_or("").to_string();
                json!({"frameId": "T1", "loaderId": p.nav_loader})
            }
            "Page.getFrameTree" => json!({"frameTree": {"frame": {
                "id": "T1", "loaderId": p.frame_loader, "url": p.url,
                "securityOrigin": "https://slow.test", "mimeType": "text/html"}}}),
            "Runtime.evaluate" => {
                let e = params["expression"].as_str().unwrap_or("");
                if e.contains("getEntriesByType('resource')") {
                    // The readiness probe: the document is still loading.
                    json!({"result": {"type": "object", "value": {
                        "readyState": "loading", "url": p.url,
                        "pending": [], "pendingTotal": 0}}})
                } else if e.trim() == "location.href" {
                    json!({"result": {"type": "string", "value": p.url}})
                } else if e.contains("readyState") {
                    json!({"result": {"type": "string", "value": "loading"}})
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

/// A daemon whose extension relay is the fake.
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
            .prefix("cuo")
            .tempdir_in("/tmp")
            .unwrap();
        let relay = tempfile::tempdir().unwrap();
        std::fs::write(relay.path().join("relay-cdp-url"), cdp).unwrap();
        let child = Command::new(BIN)
            .env("AGENT_BROWSER_DAEMON", "1")
            .env("AGENT_BROWSER_SESSION", session)
            .env("AGENT_BROWSER_SOCKET_DIR", sock.path())
            .env("HOME", home.path())
            .env("CHROME_USE_RELAY_DIR", relay.path())
            .env("AGENT_BROWSER_CDP", cdp)
            .env("AGENT_BROWSER_NO_AUTO_RECONNECT", "1")
            .env("AGENT_BROWSER_NO_AUTO_CONNECT", "1")
            .env("AGENT_BROWSER_NO_AUTO_OPEN", "1")
            .env_remove("AGENT_BROWSER_AUTO_CONNECT")
            .env_remove("AGENT_BROWSER_PROVIDER")
            .env_remove("AGENT_BROWSER_DEFAULT_TIMEOUT")
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
        // Connect, so the session owns its tab before the open.
        let r = d.send(json!({"id": "l", "action": "tab_list"}));
        assert_eq!(r["success"], true, "fake relay did not connect: {r}");
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

    /// The CLI (or the MCP server it runs) with this daemon's environment.
    /// Never auto-connect, and never open the extension's store page on the
    /// machine's screen. Without auto-connect the CLI would launch a Chrome
    /// of its own, so a config file names the fake as its `--cdp` endpoint
    /// (the MCP server's child CLI reads the same file).
    fn command(&self) -> Command {
        let config = self.home.path().join("cdp-config.json");
        std::fs::write(&config, json!({ "cdp": self.cdp }).to_string()).unwrap();
        let mut c = Command::new(BIN);
        c.env("AGENT_BROWSER_CONFIG", &config)
            .env("AGENT_BROWSER_SOCKET_DIR", self.sock.path())
            .env("HOME", self.home.path())
            .env("CHROME_USE_RELAY_DIR", self.relay.path())
            .env("AGENT_BROWSER_CDP", &self.cdp)
            .env("AGENT_BROWSER_NO_AUTO_RECONNECT", "1")
            .env("AGENT_BROWSER_NO_AUTO_CONNECT", "1")
            .env("AGENT_BROWSER_NO_AUTO_OPEN", "1")
            .env_remove("AGENT_BROWSER_AUTO_CONNECT")
            .env_remove("AGENT_BROWSER_PROVIDER")
            .env_remove("AGENT_BROWSER_SESSION")
            .env_remove("CHROME_USE_CHOOSEBROWSER_RULES_FILE")
            .env_remove("CI")
            .env("NO_COLOR", "1");
        c
    }

    fn cli_open(&self, url: &str) -> (Value, Output) {
        let out = self
            .command()
            .args(["--session", &self.session, "--json", "open", url])
            .output()
            .expect("run chrome-use");
        let all = text(&out);
        let v: Value = serde_json::from_str(all.trim().lines().next().unwrap_or(""))
            .unwrap_or_else(|_| json!({"raw": all}));
        (v, out)
    }

    /// `chrome_use_open` through the stdio MCP server; the tool call's reply.
    fn mcp_open(&self, url: &str) -> Value {
        let mut child = self
            .command()
            .args(["mcp", "--tools", "all"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let mut input = child.stdin.take().unwrap();
        for request in [
            json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-03-26","capabilities":{},"clientInfo":{"name":"open-incomplete","version":"1"}}}),
            json!({"jsonrpc":"2.0","method":"notifications/initialized"}),
            json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"chrome_use_open",
                "arguments":{"url": url, "session": self.session, "timeoutMs": 120000}}}),
        ] {
            writeln!(input, "{request}").unwrap();
        }
        drop(input);
        let output = child.wait_with_output().unwrap();
        String::from_utf8_lossy(&output.stdout)
            .lines()
            .filter_map(|s| serde_json::from_str::<Value>(s).ok())
            .find(|r| r["id"] == 2)
            .unwrap_or_else(|| {
                panic!(
                    "no tools/call reply: {}",
                    String::from_utf8_lossy(&output.stderr)
                )
            })
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

/// The cases: (how the navigation answers, a URL whose words read as a
/// denial, a lost connection or a timeout, the expected code).
fn cases() -> Vec<(&'static str, &'static str, String, &'static str)> {
    vec![
        // Over the relay: `tabs.update`'s synthetic loader binds nothing,
        // so the commit is unknown. The URL reads as a denial.
        (
            SYNTHETIC_LOADER,
            "L-real",
            "https://slow.test/debugger_access_denied:/blocked?timed-out".to_string(),
            "navigation_commit_unknown",
        ),
        // A real loader the frame shows: committed, still loading. The URL
        // reads as a lost connection (and a stale target).
        (
            "L2",
            "L2",
            "https://slow.test/connection-refused/failed to connect/target closed".to_string(),
            "navigation_incomplete",
        ),
    ]
}

/// What every case must show: its own code, `retryable: false`, the
/// no-repeat instruction, the timeout it quotes, and exactly one navigation
/// with no tab hidden and no tab opened (no #373 recovery, no replay).
fn assert_unfinished_open(
    label: &str,
    response: &Value,
    code: &str,
    fake: &Fake,
    before: (u32, u32, u32),
) {
    assert_eq!(response["success"], false, "{label}: {response}");
    assert_eq!(
        response["code"],
        code,
        "{label}: {response}; log: {}",
        fake.log()
    );
    assert_eq!(response["retryable"], false, "{label}: {response}");
    let e = response["error"].as_str().unwrap_or("");
    assert!(e.starts_with("navigation_incomplete:"), "{label}: {e}");
    assert!(
        e.contains("Timeout waiting for Page.loadEventFired"),
        "{label}: {e}"
    );
    assert!(e.contains("Do not repeat the open"), "{label}: {e}");
    assert!(!e.contains("closed the menu"), "{label}: {e}");
    let after = fake.counts();
    assert_eq!(
        (after.0 - before.0, after.1 - before.1, after.2 - before.2),
        (1, 0, 0),
        "{label}: one Page.navigate, no tab opened or hidden; log: {}",
        fake.log()
    );
}

#[test]
fn an_unfinished_open_keeps_its_code_and_is_never_repeated_through_the_cli() {
    let (fake, cdp) = Fake::start();
    let d = Daemon::start(&format!("open-inc-cli-{}", std::process::id()), &cdp);
    for (nav_loader, frame_loader, url, code) in cases() {
        fake.next_navigation(nav_loader, frame_loader);
        let before = fake.counts();
        let (v, out) = d.cli_open(&url);
        assert!(!out.status.success(), "{code}: {}", text(&out));
        assert_unfinished_open(&format!("cli {code}"), &v, code, &fake, before);
    }
}

#[test]
fn an_unfinished_open_keeps_its_code_and_is_never_repeated_through_stdio_mcp() {
    let (fake, cdp) = Fake::start();
    let d = Daemon::start(&format!("open-inc-mcp-{}", std::process::id()), &cdp);
    for (nav_loader, frame_loader, url, code) in cases() {
        fake.next_navigation(nav_loader, frame_loader);
        let before = fake.counts();
        let r = d.mcp_open(&url);
        assert_eq!(r["result"]["isError"], true, "{code}: {r}");
        let response = &r["result"]["structuredContent"]["response"];
        assert_unfinished_open(&format!("mcp {code}"), response, code, &fake, before);
    }
}
