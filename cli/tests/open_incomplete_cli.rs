//! An `open` whose page never finishes loading (#502), through the real
//! daemon, the real CLI and the stdio MCP server, on a fake extension relay.
//! The fake answers `Page.navigate` and never sends `load`, so the 25s wait
//! runs out. Then (option C):
//! - the page is clearly usable but nothing ties it to this navigation (the
//!   relay's synthetic loader): success with `commit: "unverified"` and a
//!   warning naming the tab's real URL, also when that is another
//!   navigation's page;
//! - this navigation's own loader is the frame's document and the page is
//!   ready: success as before, with no `commit` field;
//! - anything else: `navigation_incomplete:`, which quotes "Timeout waiting
//!   for Page.loadEventFired" and here a URL with denial or connection
//!   words. Whatever it quotes, it keeps its own code with `retryable:
//!   false`.
//!
//! In every case it runs no recovery and never repeats the navigation. No
//! Chrome runs.
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
    /// Where the tab ends up after the next `Page.navigate`, when not the
    /// requested URL (another navigation committed meanwhile, or blank).
    land: Option<String>,
    /// What the readiness probe reports besides the URL.
    ready_state: String,
    text_chars: u64,
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
            ready_state: "loading".into(),
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
    /// (committed). And what the tab then shows: where it lands (`None`: the
    /// requested URL), its readyState and how much rendered text its body
    /// has.
    fn next(
        &self,
        nav_loader: &str,
        frame_loader: &str,
        land: Option<&str>,
        ready_state: &str,
        text_chars: u64,
    ) {
        let mut p = self.0.lock().unwrap();
        p.nav_loader = nav_loader.into();
        p.frame_loader = frame_loader.into();
        p.land = land.map(String::from);
        p.ready_state = ready_state.into();
        p.text_chars = text_chars;
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
                p.url = match &p.land {
                    Some(land) => land.clone(),
                    None => params["url"].as_str().unwrap_or("").to_string(),
                };
                json!({"frameId": "T1", "loaderId": p.nav_loader})
            }
            "Page.getFrameTree" => json!({"frameTree": {"frame": {
                "id": "T1", "loaderId": p.frame_loader, "url": p.url,
                "securityOrigin": "https://slow.test", "mimeType": "text/html"}}}),
            "Runtime.evaluate" => {
                let e = params["expression"].as_str().unwrap_or("");
                if e.contains("getEntriesByType('resource')") {
                    // The readiness probe: what the document shows.
                    json!({"result": {"type": "object", "value": {
                        "readyState": p.ready_state, "url": p.url,
                        "pending": ["image https://slow.test/held.png"], "pendingTotal": 1,
                        "hasBody": true, "textChars": p.text_chars,
                        "visibleElements": 0}}})
                } else if e.trim() == "location.href" {
                    json!({"result": {"type": "string", "value": p.url}})
                } else if e.contains("readyState") {
                    json!({"result": {"type": "string", "value": p.ready_state}})
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

/// How the next navigation answers and what the tab then shows.
struct Case {
    label: &'static str,
    nav_loader: &'static str,
    frame_loader: &'static str,
    url: String,
    /// Where the tab lands instead of `url`, if anywhere.
    land: Option<&'static str>,
    ready_state: &'static str,
    text_chars: u64,
}

/// Pages that are not clearly usable: (case, the expected code). The URLs
/// read as a denial, a lost connection or a timeout.
fn unusable_cases() -> Vec<(Case, &'static str)> {
    vec![
        // Over the relay: `tabs.update`'s synthetic loader binds nothing, so
        // the commit is unknown, and the document is still loading with no
        // content.
        (
            Case {
                label: "relay-loading",
                nav_loader: SYNTHETIC_LOADER,
                frame_loader: "L-real",
                url: "https://slow.test/debugger_access_denied:/blocked?timed-out".to_string(),
                land: None,
                ready_state: "loading",
                text_chars: 0,
            },
            "navigation_commit_unknown",
        ),
        // Over the relay, and the tab shows about:blank, "complete".
        (
            Case {
                label: "relay-blank",
                nav_loader: SYNTHETIC_LOADER,
                frame_loader: "L-real",
                url: "https://slow.test/blank/target closed".to_string(),
                land: Some("about:blank"),
                ready_state: "complete",
                text_chars: 0,
            },
            "navigation_commit_unknown",
        ),
        // Over the relay, parsed but an empty shell.
        (
            Case {
                label: "relay-empty-shell",
                nav_loader: SYNTHETIC_LOADER,
                frame_loader: "L-real",
                url: "https://slow.test/shell".to_string(),
                land: None,
                ready_state: "interactive",
                text_chars: 0,
            },
            "navigation_commit_unknown",
        ),
        // A real loader the frame shows: committed, still loading. The URL
        // reads as a lost connection (and a stale target).
        (
            Case {
                label: "committed-loading",
                nav_loader: "L2",
                frame_loader: "L2",
                url: "https://slow.test/connection-refused/failed to connect/target closed"
                    .to_string(),
                land: None,
                ready_state: "loading",
                text_chars: 0,
            },
            "navigation_incomplete",
        ),
    ]
}

/// What a usable page reports: (case, expected `commit`, words the warning
/// must carry, words it must not).
fn usable_cases() -> Vec<(Case, Option<&'static str>, Vec<String>, Vec<&'static str>)> {
    let a = "https://slow.test/slow.html?debugger_access_denied:timed-out".to_string();
    vec![
        // (a) Over the relay, the page is usable: success, unverified.
        (
            Case {
                label: "relay-usable",
                nav_loader: SYNTHETIC_LOADER,
                frame_loader: "L-real",
                url: a.clone(),
                land: None,
                ready_state: "interactive",
                text_chars: 120,
            },
            Some("unverified"),
            vec![
                "`load` had not arrived".to_string(),
                "could not be confirmed that the page in the tab came from this request"
                    .to_string(),
                format!("The tab is on {a};"),
                "readyState is \"interactive\"".to_string(),
                "image https://slow.test/held.png".to_string(),
                "may still be loading, or their record is missing".to_string(),
            ],
            vec!["this navigation committed", "navigation_incomplete"],
        ),
        // (d) Another navigation B committed while A was pending, and B's
        // page is usable: success, unverified, B's real URL named as not the
        // requested one, never "committed" for A.
        (
            Case {
                label: "relay-other-navigation",
                nav_loader: SYNTHETIC_LOADER,
                frame_loader: "L-B",
                url: "https://slow.test/a".to_string(),
                land: Some("https://other.test/b"),
                ready_state: "complete",
                text_chars: 40,
            },
            Some("unverified"),
            vec![
                "The tab is on https://other.test/b, not the requested https://slow.test/a"
                    .to_string(),
                "commit: unverified".to_string(),
            ],
            vec!["this navigation committed", "This navigation committed"],
        ),
        // (c) This navigation's own loader is the frame's document: proven,
        // so success as before, with no `commit` field.
        (
            Case {
                label: "own-loader",
                nav_loader: "L3",
                frame_loader: "L3",
                url: "https://slow.test/own".to_string(),
                land: None,
                ready_state: "interactive",
                text_chars: 0,
            },
            None,
            vec!["this navigation committed and its DOM is ready (interactive)".to_string()],
            vec!["unverified"],
        ),
    ]
}

fn arm(fake: &Fake, c: &Case) {
    fake.next(
        c.nav_loader,
        c.frame_loader,
        c.land,
        c.ready_state,
        c.text_chars,
    );
}

/// Exactly one navigation, with no tab hidden and no tab opened (no #373
/// recovery, no replay).
fn assert_one_navigation(label: &str, fake: &Fake, before: (u32, u32, u32)) {
    let after = fake.counts();
    assert_eq!(
        (after.0 - before.0, after.1 - before.1, after.2 - before.2),
        (1, 0, 0),
        "{label}: one Page.navigate, no tab opened or hidden; log: {}",
        fake.log()
    );
}

/// What every unusable case must show: its own code, `retryable: false`,
/// the no-repeat instruction, the timeout it quotes, the real URL and
/// readyState.
fn assert_unfinished_open(label: &str, response: &Value, c: &Case, code: &str, fake: &Fake) {
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
    assert!(
        e.contains(&format!("readyState \"{}\"", c.ready_state)),
        "{label}: {e}"
    );
    assert!(e.contains(c.land.unwrap_or(&c.url)), "{label}: {e}");
    assert!(!e.contains("closed the menu"), "{label}: {e}");
}

/// What every usable case must show: success, the real URL, the expected
/// `commit` field and warning.
fn assert_usable_open(
    label: &str,
    response: &Value,
    c: &Case,
    commit: Option<&str>,
    has: &[String],
    lacks: &[&str],
) {
    assert_eq!(response["success"], true, "{label}: {response}");
    let data = &response["data"];
    assert_eq!(
        data["url"].as_str(),
        Some(c.land.unwrap_or(&c.url)),
        "{label}: {response}"
    );
    assert_eq!(data["commit"].as_str(), commit, "{label}: {response}");
    let w = data["warning"].as_str().unwrap_or("");
    for s in has {
        assert!(w.contains(s.as_str()), "{label}: missing {s:?} in {w}");
    }
    for s in lacks {
        assert!(!w.contains(s), "{label}: unexpected {s:?} in {w}");
    }
}

fn session(kind: &str) -> String {
    format!("open-inc-{kind}-{}", std::process::id())
}

#[test]
fn an_unusable_unfinished_open_keeps_its_code_and_is_never_repeated_through_the_cli() {
    let (fake, cdp) = Fake::start();
    let d = Daemon::start(&session("cli-err"), &cdp);
    for (c, code) in unusable_cases() {
        arm(&fake, &c);
        let before = fake.counts();
        let (v, out) = d.cli_open(&c.url);
        let label = format!("cli {}", c.label);
        assert!(!out.status.success(), "{label}: {}", text(&out));
        assert_unfinished_open(&label, &v, &c, code, &fake);
        assert_one_navigation(&label, &fake, before);
    }
}

#[test]
fn an_unusable_unfinished_open_keeps_its_code_and_is_never_repeated_through_stdio_mcp() {
    let (fake, cdp) = Fake::start();
    let d = Daemon::start(&session("mcp-err"), &cdp);
    for (c, code) in unusable_cases() {
        arm(&fake, &c);
        let before = fake.counts();
        let r = d.mcp_open(&c.url);
        let label = format!("mcp {}", c.label);
        assert_eq!(r["result"]["isError"], true, "{label}: {r}");
        let response = &r["result"]["structuredContent"]["response"];
        assert_unfinished_open(&label, response, &c, code, &fake);
        assert_one_navigation(&label, &fake, before);
    }
}

#[test]
fn a_usable_unfinished_open_succeeds_with_what_is_known_through_the_cli() {
    let (fake, cdp) = Fake::start();
    let d = Daemon::start(&session("cli-ok"), &cdp);
    for (c, commit, has, lacks) in usable_cases() {
        arm(&fake, &c);
        let before = fake.counts();
        let (v, out) = d.cli_open(&c.url);
        let label = format!("cli {}", c.label);
        assert!(out.status.success(), "{label}: {}", text(&out));
        assert_usable_open(&label, &v, &c, commit, &has, &lacks);
        assert_one_navigation(&label, &fake, before);
    }
}

#[test]
fn a_usable_unfinished_open_succeeds_with_what_is_known_through_stdio_mcp() {
    let (fake, cdp) = Fake::start();
    let d = Daemon::start(&session("mcp-ok"), &cdp);
    for (c, commit, has, lacks) in usable_cases() {
        arm(&fake, &c);
        let before = fake.counts();
        let r = d.mcp_open(&c.url);
        let label = format!("mcp {}", c.label);
        assert_ne!(r["result"]["isError"], true, "{label}: {r}");
        let response = &r["result"]["structuredContent"]["response"];
        assert_usable_open(&label, response, &c, commit, &has, &lacks);
        assert_one_navigation(&label, &fake, before);
    }
}
