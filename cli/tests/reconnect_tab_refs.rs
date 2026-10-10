//! Tab refs across a dropped browser connection (#473), through the real
//! daemon process: raw commands on its socket, and the real CLI for `batch`
//! and `script`. The browser is a fake CDP endpoint that can go down, refuse
//! connections, come back listing its tabs in another order, and lose or
//! regain a tab. No Chrome runs.
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

#[derive(Default)]
struct Browser {
    /// (target id, url), in creation order.
    targets: Vec<(String, String)>,
    /// List targets newest first, the way a reconnect may enumerate them.
    reversed: bool,
    /// When false, connections are dropped before the handshake.
    up: bool,
    /// Bumped to drop every open connection.
    generation: u64,
    created: u32,
    /// Every page-level `Runtime.evaluate` expression, with the session it
    /// was sent to.
    evaluated: Vec<(String, String)>,
    /// The listener serving now; an older one stops and its port refuses.
    listener: u64,
    /// What `windows.getAll` answers over the relay (#486); `None` is one
    /// normal window.
    windows: Option<Value>,
    /// `Page.enable` fails, so a new tab cannot be set up.
    page_enable_fails: bool,
    /// `Page.enable` never answers.
    page_enable_hangs: bool,
    /// How `Target.closeTarget` behaves.
    close: CloseMode,
    /// `ABExt.tabPresence` gets no versioned answer, like ab-connect 0.5.32.
    old_extension: bool,
    /// `ABExt.tabPresence` fails for a target whose close went through.
    presence_fails_after_close: bool,
    /// Targets a `Target.closeTarget` removed.
    closed: Vec<String>,
    /// Every `ABExt.call` the daemon made, as `namespace.method`.
    calls: Vec<String>,
    /// `Target.attachToTarget` calls.
    attached: u32,
}

#[derive(Default, Clone, Copy, PartialEq, Debug)]
enum CloseMode {
    #[default]
    Closes,
    /// An error reply; the tab stays.
    Refused,
    /// No reply at all; the tab stays.
    Hangs,
    /// `{success: true}`, but the tab stays.
    AckButStays,
}

#[derive(Clone)]
struct Fake(Arc<Mutex<Browser>>);

impl Fake {
    fn start() -> (Self, String) {
        let fake = Fake(Arc::new(Mutex::new(Browser {
            up: true,
            ..Default::default()
        })));
        let url = fake.listen();
        (fake, url)
    }

    /// Serve on a new port until another listener replaces this one.
    fn listen(&self) -> String {
        let shared = self.clone();
        let mine = shared.0.lock().unwrap().listener;
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
                while shared.0.lock().unwrap().listener == mine {
                    let accepted =
                        tokio::time::timeout(Duration::from_millis(50), listener.accept()).await;
                    let Ok(Ok((stream, _))) = accepted else {
                        continue;
                    };
                    let shared = shared.clone();
                    let generation = {
                        let b = shared.0.lock().unwrap();
                        if !b.up {
                            drop(stream);
                            continue;
                        }
                        b.generation
                    };
                    tokio::spawn(async move { serve(shared, stream, generation).await });
                }
                // Dropping the listener closes the port: it refuses from now on.
            });
        });
        rx.recv().unwrap()
    }

    /// The relay host restarts: every connection drops, the old port refuses,
    /// and the same browser is served on a new port.
    fn restart_on_new_port(&self) -> String {
        {
            let mut b = self.0.lock().unwrap();
            b.listener += 1;
            b.generation += 1;
            b.up = true;
        }
        std::thread::sleep(Duration::from_millis(300));
        self.listen()
    }

    /// The connection dies and new ones are refused.
    fn go_down(&self) {
        {
            let mut b = self.0.lock().unwrap();
            b.up = false;
            b.generation += 1;
        }
        // Let every open connection notice and close.
        std::thread::sleep(Duration::from_millis(300));
    }

    fn come_back(&self, reversed: bool) {
        let mut b = self.0.lock().unwrap();
        b.up = true;
        b.reversed = reversed;
    }

    fn remove(&self, target: &str) {
        self.0.lock().unwrap().targets.retain(|(t, _)| t != target);
    }

    fn restore(&self, target: &str, url: &str) {
        self.0
            .lock()
            .unwrap()
            .targets
            .push((target.to_string(), url.to_string()));
    }

    /// Sessions that evaluated `location.href` since the last call.
    fn take_url_reads(&self) -> Vec<String> {
        let mut b = self.0.lock().unwrap();
        let reads = b
            .evaluated
            .iter()
            .filter(|(_, e)| e.contains("location.href"))
            .map(|(s, _)| s.clone())
            .collect();
        b.evaluated.clear();
        reads
    }

    fn reply(&self, req: &Value) -> Value {
        let mut b = self.0.lock().unwrap();
        let method = req["method"].as_str().unwrap_or("");
        let params = &req["params"];
        if method == "Runtime.evaluate" {
            let session = req["sessionId"].as_str().unwrap_or("").to_string();
            let expression = params["expression"].as_str().unwrap_or("").to_string();
            b.evaluated.push((session, expression));
        }
        match method {
            "ABExt.call" => {
                b.calls.push(format!(
                    "{}.{}",
                    params["namespace"].as_str().unwrap_or(""),
                    params["method"].as_str().unwrap_or("")
                ));
                let windows = b
                    .windows
                    .clone()
                    .unwrap_or_else(|| json!([{"id": 1, "type": "normal"}]));
                json!({ "result": windows })
            }
            "Page.enable" if b.page_enable_hangs => json!({"__hang": true}),
            "Page.enable" if b.page_enable_fails => {
                json!({"__error": "Page.enable: renderer did not answer"})
            }
            "Target.getTargets" => {
                let mut list: Vec<Value> = b
                    .targets
                    .iter()
                    .map(|(id, url)| {
                        json!({"targetId": id, "type": "page", "title": id, "url": url,
                               "attached": true, "browserContextId": "C1"})
                    })
                    .collect();
                if b.reversed {
                    list.reverse();
                }
                json!({"targetInfos": list})
            }
            "Target.createTarget" => {
                b.created += 1;
                let id = format!("T{}", b.created);
                let url = params["url"].as_str().unwrap_or("about:blank").to_string();
                b.targets.push((id.clone(), url));
                json!({"targetId": id})
            }
            "Target.attachToTarget" => {
                b.attached += 1;
                json!({"sessionId": format!("S-{}", params["targetId"].as_str().unwrap_or(""))})
            }
            "Target.closeTarget" => match b.close {
                CloseMode::Closes => {
                    let id = params["targetId"].as_str().unwrap_or("").to_string();
                    b.targets.retain(|(t, _)| *t != id);
                    b.closed.push(id);
                    json!({"success": true})
                }
                CloseMode::Refused => json!({"__error": "Target.closeTarget: refused"}),
                CloseMode::Hangs => json!({"__hang": true}),
                CloseMode::AckButStays => json!({"success": true}),
            },
            // The extension's `ABExt.tabPresence` (ab-connect 0.5.33), which
            // `close` reads tabs back with over a relay endpoint. Target `T<n>`
            // is Chrome tab `<n>`: listed means present; absent only when the
            // target is unlisted and the exact tab id given is gone.
            "ABExt.tabPresence" if b.old_extension => json!({}),
            "ABExt.tabPresence"
                if b.presence_fails_after_close
                    && b.closed.iter().any(|t| params["targetId"] == t.as_str()) =>
            {
                json!({"__error": "tabPresence: chrome.debugger.getTargets failed"})
            }
            "ABExt.tabPresence" => {
                let target = params["targetId"].as_str().unwrap_or("").to_string();
                let tab_of = |t: &str| t.strip_prefix('T').and_then(|n| n.parse::<i64>().ok());
                let listed = b.targets.iter().any(|(t, _)| *t == target);
                let asked = params["tabId"].as_i64();
                let (presence, tab) = if listed {
                    ("present", tab_of(&target))
                } else if let Some(tab) = asked {
                    if b.targets.iter().any(|(t, _)| tab_of(t) == Some(tab)) {
                        ("unknown", Some(tab))
                    } else {
                        ("absent", Some(tab))
                    }
                } else {
                    ("unknown", None)
                };
                json!({"tabPresenceVersion": 1, "targetId": target, "tabId": tab,
                       "presence": presence})
            }
            "Target.getTargetInfo" => {
                let id = params["targetId"].as_str().unwrap_or("");
                let url = b
                    .targets
                    .iter()
                    .find(|(t, _)| t == id)
                    .map(|(_, u)| u.clone())
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
        }
    }
}

async fn serve(fake: Fake, stream: tokio::net::TcpStream, generation: u64) {
    let Ok(mut ws) = tokio_tungstenite::accept_async(stream).await else {
        return;
    };
    loop {
        if fake.0.lock().unwrap().generation != generation {
            return; // dropping the socket is the dead connection
        }
        let next = tokio::time::timeout(Duration::from_millis(50), ws.next()).await;
        let msg = match next {
            Err(_) => continue,
            Ok(Some(Ok(m))) => m,
            Ok(_) => return,
        };
        let Ok(text) = msg.into_text() else { continue };
        let Ok(req) = serde_json::from_str::<Value>(&text) else {
            continue;
        };
        if fake.0.lock().unwrap().generation != generation {
            return;
        }
        let result = fake.reply(&req);
        if result.get("__hang").is_some() {
            continue;
        }
        let mut reply = match result.get("__error") {
            Some(e) => json!({"id": req["id"], "error": {"code": -32000, "message": e}}),
            None => json!({"id": req["id"], "result": result}),
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
            .prefix("cur")
            .tempdir_in("/tmp")
            .unwrap();
        let relay = tempfile::tempdir().unwrap();
        let child = Self::spawn(session, cdp, &home, &sock, &relay);
        let d = Daemon {
            child,
            home,
            sock,
            relay,
            session: session.to_string(),
            cdp: cdp.to_string(),
        };
        d.wait_listening();
        d
    }

    /// Kill this daemon the way a client stops a stuck one, and start a
    /// fresh one for the same session.
    fn replace(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_file(self.sock_path());
        self.child = Self::spawn(
            &self.session,
            &self.cdp,
            &self.home,
            &self.sock,
            &self.relay,
        );
        self.wait_listening();
    }

    fn spawn(
        session: &str,
        cdp: &str,
        home: &tempfile::TempDir,
        sock: &tempfile::TempDir,
        relay: &tempfile::TempDir,
    ) -> Child {
        Command::new(BIN)
            .env("AGENT_BROWSER_DAEMON", "1")
            .env("AGENT_BROWSER_SESSION", session)
            .env("AGENT_BROWSER_SOCKET_DIR", sock.path())
            .env("HOME", home.path())
            .env("CHROME_USE_RELAY_DIR", relay.path())
            .env("AGENT_BROWSER_CDP", cdp)
            .env("AGENT_BROWSER_NO_AUTO_RECONNECT", "1")
            .env("AGENT_BROWSER_RELAY_REVIVE_SECS", "3")
            .env_remove("AGENT_BROWSER_AUTO_CONNECT")
            .env_remove("AGENT_BROWSER_PROVIDER")
            .env("NO_COLOR", "1")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap()
    }

    fn wait_listening(&self) {
        let started = Instant::now();
        while !self.sock_path().exists() {
            assert!(
                started.elapsed() < Duration::from_secs(20),
                "daemon never listened"
            );
            std::thread::sleep(Duration::from_millis(50));
        }
        std::thread::sleep(Duration::from_millis(300));
    }

    fn sock_path(&self) -> PathBuf {
        self.sock.path().join(format!("{}.sock", self.session))
    }

    fn send(&self, cmd: Value) -> Value {
        let mut s = UnixStream::connect(self.sock_path())
            .unwrap_or_else(|e| panic!("{}: no daemon for {cmd}: {e}", self.session));
        s.set_read_timeout(Some(Duration::from_secs(90))).unwrap();
        writeln!(s, "{cmd}").unwrap();
        let mut line = String::new();
        BufReader::new(&s).read_line(&mut line).unwrap();
        serde_json::from_str(&line).unwrap_or_else(|e| panic!("{e}: {line:?}"))
    }

    /// The real CLI against this daemon, with `--json`.
    fn cli(&self, args: &[&str]) -> Output {
        let mut all = vec!["--json"];
        all.extend_from_slice(args);
        self.cli_plain(&all)
    }

    /// The real CLI against this daemon, as typed. (`script --json` prints
    /// only the data of a refused script, so its error is read without it.)
    fn cli_plain(&self, args: &[&str]) -> Output {
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

    /// `tab_list`: tab id -> (target id, label).
    fn tabs(&self) -> Vec<(String, String, Option<String>)> {
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
                    t["label"].as_str().map(str::to_string),
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

fn text(out: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

fn refused_for_lost_tab(r: &Value) -> bool {
    r["success"] == false
        && r["error"]
            .as_str()
            .is_some_and(|e| e.contains("Refusing to run `"))
}

fn tab_of(tabs: &[(String, String, Option<String>)], target: &str) -> Option<String> {
    tabs.iter()
        .find(|(_, t, _)| t == target)
        .map(|(id, _, _)| id.clone())
}

/// Three session tabs, t2 (`docs`) the driven one: t1=T1, t2=T2, t3=T3.
fn three_tabs(d: &Daemon) {
    let r = d.send(json!({"id": "a", "action": "tab_list"}));
    assert_eq!(r["success"], true, "fake browser did not connect: {r}");
    let r = d.send(json!({"id": "b", "action": "tab_new", "label": "docs"}));
    assert_eq!(r["success"], true, "{r}");
    let r = d.send(json!({"id": "c", "action": "tab_new"}));
    assert_eq!(r["success"], true, "{r}");
    let r = d.send(json!({"id": "d", "action": "tab_switch", "tabId": "t2"}));
    assert_eq!(r["success"], true, "{r}");
    let tabs = d.tabs();
    assert_eq!(tab_of(&tabs, "T1").as_deref(), Some("t1"), "{tabs:?}");
    assert_eq!(tab_of(&tabs, "T2").as_deref(), Some("t2"), "{tabs:?}");
    assert_eq!(tab_of(&tabs, "T3").as_deref(), Some("t3"), "{tabs:?}");
}

/// The first reconnect fails; the second finds the tabs listed in another
/// order and the driven tab gone. The refs carried from the dead connection
/// must still be the ones bound: ids by Chrome tab, the lost id and its label
/// refused, and the session refusing to act in a tab it never chose.
#[test]
fn refs_survive_a_failed_reconnect_and_bind_on_the_next() {
    let (fake, cdp) = Fake::start();
    let d = Daemon::start("rc-fail", &cdp);
    three_tabs(&d);

    fake.go_down();
    let r = d.send(json!({"id": "x1", "action": "tab_list"}));
    assert_eq!(r["success"], false, "reconnect should have failed: {r}");
    assert!(
        r["error"].as_str().unwrap().contains("Auto-launch failed"),
        "{r}"
    );

    fake.remove("T2");
    fake.come_back(true);
    let r = d.send(json!({"id": "x2", "action": "tab_list"}));
    assert_eq!(r["success"], true, "{r}");
    let warning = r["warning"].as_str().unwrap_or("");
    assert!(warning.contains("re-established"), "{r}");
    assert!(warning.contains("Tabs t2 could not be found"), "{r}");
    assert!(!warning.contains("previous tabs are gone"), "{r}");
    let tabs = d.tabs();
    assert_eq!(tab_of(&tabs, "T1").as_deref(), Some("t1"), "{tabs:?}");
    assert_eq!(tab_of(&tabs, "T3").as_deref(), Some("t3"), "{tabs:?}");
    assert_eq!(tabs.len(), 2, "{tabs:?}");

    // The driven tab is gone: nothing acts on whatever is active now.
    let r = d.send(json!({"id": "c1", "action": "click", "selector": "#submit"}));
    assert!(refused_for_lost_tab(&r), "{r}");
    // A `tabId` the routing would not honour selects nothing.
    for bad in [
        json!(1),
        json!(true),
        json!({"id": "t1"}),
        json!(""),
        json!("t2"),
        json!("t9"),
    ] {
        let r = d.send(json!({"id": "c2", "action": "click", "selector": "#submit",
                              "tabId": bad}));
        assert!(refused_for_lost_tab(&r), "tabId {bad}: {r}");
        // ...and leaves the refusal in place.
        let r = d.send(json!({"id": "c3", "action": "url"}));
        assert!(refused_for_lost_tab(&r), "after tabId {bad}: {r}");
    }
    // Closing "the current tab" would close a tab nobody chose.
    let r = d.send(json!({"id": "c4", "action": "tab_close"}));
    assert!(refused_for_lost_tab(&r), "{r}");
    // A switch to a tab that does not exist fails and changes nothing.
    let r = d.send(json!({"id": "c5", "action": "tab_switch", "tabId": "t2"}));
    assert_eq!(r["success"], false, "{r}");
    let r = d.send(json!({"id": "c6", "action": "url"}));
    assert!(refused_for_lost_tab(&r), "{r}");

    // The lost tab's ids and label are refused, never reassigned.
    let r = d.send(json!({"id": "l1", "action": "tab_switch", "tabId": "docs"}));
    assert!(refused_for_lost_tab(&r), "{r}");
    assert!(
        r["error"]
            .as_str()
            .unwrap_or("")
            .contains("could not be re-identified"),
        "{r}"
    );
    let r = d.send(json!({"id": "l2", "action": "tab_new", "label": "docs"}));
    assert_eq!(r["success"], false, "{r}");
    assert!(
        r["error"].as_str().unwrap().contains("still belongs to t2"),
        "{r}"
    );
    // Failed label attempts did not end the refusal either.
    let r = d.send(json!({"id": "l4", "action": "url"}));
    assert!(refused_for_lost_tab(&r), "{r}");

    // The old tab comes back: it is t2 and `docs` again, and only it is.
    fake.restore("T2", "about:blank");
    let tabs = d.tabs();
    assert_eq!(tab_of(&tabs, "T2").as_deref(), Some("t2"), "{tabs:?}");
    let docs: Vec<_> = tabs
        .iter()
        .filter(|(_, _, l)| l.as_deref() == Some("docs"))
        .collect();
    assert_eq!(docs.len(), 1, "{tabs:?}");
    assert_eq!(docs[0].1, "T2", "{tabs:?}");

    // Choosing a tab ends the refusal.
    let r = d.send(json!({"id": "s1", "action": "tab_switch", "tabId": "t3"}));
    assert_eq!(r["success"], true, "{r}");
    let r = d.send(json!({"id": "s2", "action": "url"}));
    assert!(!refused_for_lost_tab(&r), "{r}");
}

/// A lost tab's label stays reserved through `tab adopt` of another tab, and
/// when the old tab is adopted back it gets its id and label again.
#[test]
fn adopting_the_lost_tab_back_restores_its_id_and_label() {
    let (fake, cdp) = Fake::start();
    let d = Daemon::start("rc-adopt", &cdp);
    three_tabs(&d);
    fake.go_down();
    fake.remove("T2");
    fake.come_back(false);
    let r = d.send(json!({"id": "a", "action": "tab_list"}));
    assert_eq!(r["success"], true, "{r}");
    fake.restore("T2", "about:blank");
    let r = d.send(json!({"id": "b", "action": "tab_adopt", "spec": "T2"}));
    assert_eq!(r["success"], true, "{r}");
    let tabs = d.tabs();
    let t2: Vec<_> = tabs.iter().filter(|(_, t, _)| t == "T2").collect();
    assert_eq!(t2.len(), 1, "{tabs:?}");
    assert_eq!(t2[0].0, "t2", "{tabs:?}");
    assert_eq!(t2[0].2.as_deref(), Some("docs"), "{tabs:?}");
}

/// `batch --tab` and `script --tab` choose their tab before their steps run,
/// so the steps are not refused; without `--tab` they are.
#[test]
fn batch_and_script_with_tab_run_their_steps_in_that_tab() {
    let (fake, cdp) = Fake::start();
    let d = Daemon::start("rc-steps", &cdp);
    three_tabs(&d);

    // Lose the driven t2.
    fake.go_down();
    fake.remove("T2");
    fake.come_back(true);
    // Reconnect first, so the reads counted below are the steps' own.
    let r = d.send(json!({"id": "r", "action": "tab_list"}));
    assert_eq!(r["success"], true, "{r}");
    fake.take_url_reads();
    let out = d.cli(&["batch", "get url"]);
    assert!(text(&out).contains("Refusing to run"), "{}", text(&out));
    assert!(
        fake.take_url_reads().is_empty(),
        "a refused step reached a tab"
    );
    let out = d.cli(&["batch", "--tab", "t1", "get url", "get url"]);
    assert!(out.status.success(), "{}", text(&out));
    assert!(!text(&out).contains("Refusing"), "{}", text(&out));
    // Both steps read the url of T1, the tab t1 names, and of no other tab.
    assert_only_session(&fake, "S-T1");

    // Lose the driven t1 now.
    fake.go_down();
    fake.remove("T1");
    fake.come_back(false);
    let r = d.send(json!({"id": "r2", "action": "tab_list"}));
    assert_eq!(r["success"], true, "{r}");
    fake.take_url_reads();
    let program = d.home.path().join("p.json");
    std::fs::write(&program, r#"[{"do": "url"}, {"do": "url"}]"#).unwrap();
    let program = program.to_str().unwrap();
    let out = d.cli_plain(&["script", program]);
    assert!(
        text(&out).contains("Refusing to run"),
        "{:?} {}",
        out.status,
        text(&out)
    );
    assert!(
        fake.take_url_reads().is_empty(),
        "a refused script reached a tab"
    );
    let out = d.cli_plain(&["script", "--tab", "t3", program]);
    assert!(out.status.success(), "{}", text(&out));
    assert!(!text(&out).contains("Refusing"), "{}", text(&out));
    assert_only_session(&fake, "S-T3");
}

/// The daemon is stopped while its reconnect is failing (a client gives up on
/// a stuck daemon). The next daemon must bind the same refs, not number the
/// tabs afresh in the order the browser lists them.
#[test]
fn refs_survive_the_daemon_being_replaced_mid_reconnect() {
    let (fake, cdp) = Fake::start();
    let mut d = Daemon::start("rc-replaced", &cdp);
    three_tabs(&d);

    fake.go_down();
    let r = d.send(json!({"id": "x1", "action": "tab_list"}));
    assert_eq!(r["success"], false, "reconnect should have failed: {r}");
    d.replace();

    fake.remove("T2");
    fake.come_back(true);
    let r = d.send(json!({"id": "x2", "action": "tab_list"}));
    assert_eq!(r["success"], true, "{r}");
    let warning = r["warning"].as_str().unwrap_or("");
    assert!(warning.contains("Tabs t2 could not be found"), "{r}");
    let tabs = d.tabs();
    assert_eq!(tab_of(&tabs, "T1").as_deref(), Some("t1"), "{tabs:?}");
    assert_eq!(tab_of(&tabs, "T3").as_deref(), Some("t3"), "{tabs:?}");
    let r = d.send(json!({"id": "c1", "action": "click", "selector": "#submit"}));
    assert!(refused_for_lost_tab(&r), "{r}");
    let r = d.send(json!({"id": "l1", "action": "tab_new", "label": "docs"}));
    assert!(
        r["error"]
            .as_str()
            .unwrap_or("")
            .contains("still belongs to t2"),
        "{r}"
    );
}

fn record_path(d: &Daemon) -> PathBuf {
    d.sock
        .path()
        .join(format!("{}.carried-tabs.json", d.session))
}

fn held(r: &Value, needle: &str) -> bool {
    r["success"] == false
        && r["error"]
            .as_str()
            .is_some_and(|e| e.starts_with("Refusing to run") && e.contains(needle))
}

/// A record the next daemon cannot read, whether half-written or unreadable,
/// is not "no record": commands are held, nothing claims the tabs were bound
/// again, and only `close` ends the hold.
#[test]
fn an_unreadable_record_holds_instead_of_numbering_afresh() {
    use std::os::unix::fs::PermissionsExt;
    for (name, damage) in [("half", 0u8), ("locked", 1u8)] {
        let (fake, cdp) = Fake::start();
        let mut d = Daemon::start(&format!("rc-{name}"), &cdp);
        three_tabs(&d);
        fake.go_down();
        let r = d.send(json!({"id": "x", "action": "tab_list"}));
        assert_eq!(r["success"], false, "{r}");
        let path = record_path(&d);
        let full = std::fs::read_to_string(&path).expect("record written");
        if damage == 0 {
            std::fs::write(&path, &full[..full.len() / 2]).unwrap();
        } else {
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o000)).unwrap();
        }
        d.replace();
        fake.come_back(true);

        let r = d.send(json!({"id": "l", "action": "tab_list"}));
        assert_eq!(r["success"], true, "{name}: {r}");
        let warning = r["warning"].as_str().unwrap_or("");
        assert!(warning.contains("could not be read back"), "{name}: {r}");
        assert!(!warning.contains("re-established"), "{name}: {r}");
        fake.take_url_reads();
        for cmd in [
            json!({"id": "a", "action": "url"}),
            json!({"id": "b", "action": "click", "selector": "#submit", "tabId": "t1"}),
            json!({"id": "c", "action": "tab_switch", "tabId": "t1"}),
            json!({"id": "d", "action": "tab_new"}),
        ] {
            let r = d.send(cmd.clone());
            assert!(held(&r, "could not be read back"), "{name} {cmd}: {r}");
        }
        assert!(
            fake.take_url_reads().is_empty(),
            "{name}: a held command reached a tab"
        );
        if damage == 1 {
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        }
        let r = d.send(json!({"id": "z", "action": "close"}));
        assert_eq!(r["success"], true, "{name}: {r}");
        assert!(!path.exists(), "{name}: close left the record");
        // `close` may end the daemon; the next one must not hold either.
        d.replace();
        let r = d.send(json!({"id": "y", "action": "url"}));
        assert!(!held(&r, ""), "{name}: still held after close: {r}");
    }
}

/// A record that cannot be written stops the reconnect before the old state
/// is torn down: the next attempt still binds the same ids.
#[test]
fn a_record_that_cannot_be_written_changes_nothing() {
    let (fake, cdp) = Fake::start();
    let d = Daemon::start("rc-nowrite", &cdp);
    three_tabs(&d);
    let path = record_path(&d);
    std::fs::create_dir(&path).unwrap();
    fake.go_down();
    let r = d.send(json!({"id": "x1", "action": "tab_list"}));
    assert_eq!(r["success"], false, "{r}");
    assert!(
        r["error"]
            .as_str()
            .unwrap()
            .contains("could not be recorded"),
        "{r}"
    );
    std::fs::remove_dir(&path).unwrap();
    let leftovers: Vec<_> = std::fs::read_dir(d.sock.path())
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().to_string())
        .filter(|n| n.contains("carried-tabs"))
        .collect();
    assert!(leftovers.is_empty(), "temp files left: {leftovers:?}");

    fake.remove("T2");
    fake.come_back(true);
    let r = d.send(json!({"id": "x2", "action": "tab_list"}));
    assert_eq!(r["success"], true, "{r}");
    assert!(
        r["warning"]
            .as_str()
            .unwrap_or("")
            .contains("Tabs t2 could not be found"),
        "{r}"
    );
    let tabs = d.tabs();
    assert_eq!(tab_of(&tabs, "T1").as_deref(), Some("t1"), "{tabs:?}");
    assert_eq!(tab_of(&tabs, "T3").as_deref(), Some("t3"), "{tabs:?}");
    assert!(!path.exists());
}

/// The record was consumed but cannot be removed: commands are held, the
/// reply says so, and the hold ends only once the removal succeeds.
#[test]
fn a_record_that_cannot_be_removed_holds_commands() {
    use std::os::unix::fs::PermissionsExt;
    let (fake, cdp) = Fake::start();
    let d = Daemon::start("rc-norm", &cdp);
    three_tabs(&d);
    fake.go_down();
    let r = d.send(json!({"id": "x1", "action": "tab_list"}));
    assert_eq!(r["success"], false, "{r}");
    let path = record_path(&d);
    assert!(path.exists());
    fake.come_back(true);
    let dir = d.sock.path();
    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o555)).unwrap();
    let r = d.send(json!({"id": "x2", "action": "tab_list"}));
    fake.take_url_reads();
    let r2 = d.send(json!({"id": "x3", "action": "url"}));
    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o755)).unwrap();
    assert_eq!(r["success"], true, "{r}");
    let warning = r["warning"].as_str().unwrap_or("");
    assert!(warning.contains("re-established"), "{r}");
    assert!(warning.contains("could not remove"), "{r}");
    assert!(held(&r2, "could not remove"), "{r2}");
    assert!(path.exists(), "the record is still there");
    assert!(
        fake.take_url_reads().is_empty(),
        "a held command reached a tab"
    );

    let r = d.send(json!({"id": "x4", "action": "url"}));
    assert_eq!(r["success"], true, "{r}");
    assert!(!path.exists(), "removal was not retried");
    assert_only_session(&fake, "S-T2");
}

/// `session stop` ends the session: its carried tab ids do not come back on
/// the next daemon.
#[test]
fn a_stopped_sessions_record_does_not_come_back() {
    let (fake, cdp) = Fake::start();
    let mut d = Daemon::start("rc-stop", &cdp);
    three_tabs(&d);
    fake.go_down();
    let r = d.send(json!({"id": "x1", "action": "tab_list"}));
    assert_eq!(r["success"], false, "{r}");
    let path = record_path(&d);
    assert!(path.exists());
    let session = d.session.clone();
    let out = d.cli(&["session", "stop", &session, "--force"]);
    assert!(out.status.success(), "{}", text(&out));
    assert!(!path.exists(), "stop left the record: {}", text(&out));

    fake.remove("T2");
    fake.come_back(true);
    d.replace();
    let r = d.send(json!({"id": "x2", "action": "tab_list"}));
    assert_eq!(r["success"], true, "{r}");
    let warning = r["warning"].as_str().unwrap_or("");
    assert!(!warning.contains("re-established"), "{r}");
    assert!(!warning.contains("could not be found"), "{r}");
    let r = d.send(json!({"id": "x3", "action": "url"}));
    assert!(!refused_for_lost_tab(&r), "{r}");
}

/// The page reads since the last check all went to `session`, and there was
/// at least one: a wrong tab that happens to answer cannot pass.
fn assert_only_session(fake: &Fake, session: &str) {
    let reads = fake.take_url_reads();
    assert!(!reads.is_empty(), "no page read reached any tab");
    assert!(
        reads.iter().all(|s| s == session),
        "reads went to {reads:?}, expected only {session}"
    );
}

/// A record that cannot be read is evidence: a connection opened while the
/// session is held numbers its tabs only provisionally, so when it dies its
/// tabs must not be written over that record. After the daemon is replaced
/// the session is still held, the record is byte for byte the same, and no
/// tab action reaches the browser.
#[test]
fn a_held_session_never_overwrites_the_unreadable_record() {
    let (fake, cdp) = Fake::start();
    let mut d = Daemon::start("rc-evidence", &cdp);
    let path = record_path(&d);
    let corrupt = br#"{"why": "its browser connection was dead", "snaps"#;
    std::fs::write(&path, corrupt).unwrap();

    let r = d.send(json!({"id": "l1", "action": "tab_list"}));
    assert_eq!(r["success"], true, "{r}");
    assert!(
        r["warning"]
            .as_str()
            .unwrap_or("")
            .contains("could not be read back"),
        "{r}"
    );

    fake.go_down();
    let r = d.send(json!({"id": "u1", "action": "url"}));
    assert_eq!(r["success"], false, "{r}");
    assert_eq!(std::fs::read(&path).unwrap(), corrupt, "record overwritten");

    d.replace();
    fake.come_back(true);
    let r = d.send(json!({"id": "l2", "action": "tab_list"}));
    assert_eq!(r["success"], true, "{r}");
    assert!(
        r["warning"]
            .as_str()
            .unwrap_or("")
            .contains("could not be read back"),
        "{r}"
    );
    fake.take_url_reads();
    for cmd in [
        json!({"id": "a", "action": "url"}),
        json!({"id": "b", "action": "click", "selector": "#submit", "tabId": "t2"}),
        json!({"id": "c", "action": "tab_switch", "tabId": "t1"}),
        json!({"id": "d", "action": "tab_close", "tabId": "t1"}),
    ] {
        let r = d.send(cmd.clone());
        assert!(held(&r, "could not be read back"), "{cmd}: {r}");
    }
    assert!(
        fake.take_url_reads().is_empty(),
        "a held command reached a tab"
    );
    assert_eq!(std::fs::read(&path).unwrap(), corrupt, "record changed");
}

/// The targets the browser still has.
fn open_targets(fake: &Fake) -> Vec<String> {
    fake.0
        .lock()
        .unwrap()
        .targets
        .iter()
        .map(|(t, _)| t.clone())
        .collect()
}

/// The target ids `tab list` marks as created by this session.
fn created_by_session(d: &Daemon) -> Vec<String> {
    let r = d.send(json!({"id": "o", "action": "tab_list"}));
    assert_eq!(r["success"], true, "{r}");
    let created: Vec<String> = r["data"]["tabs"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|t| t["ownership"] == "created")
        .map(|t| t["targetId"].as_str().unwrap().to_string())
        .collect();
    assert!(created.len() >= 2, "{r}");
    created
}

fn created_record(d: &Daemon) -> PathBuf {
    d.sock
        .path()
        .join(format!("{}.created-targets.json", d.session))
}

/// Bind the session to relay profile P1 served at the fake's address.
fn bind_to_profile(d: &Daemon, cdp: &str) {
    std::fs::write(
        d.relay.path().join("relay-ext-profile-P1"),
        r#"{"id": "P1", "email": "p1@example.test"}"#,
    )
    .unwrap();
    std::fs::write(d.relay.path().join("relay-cdp-url-P1"), cdp).unwrap();
    std::fs::write(
        d.sock.path().join(format!("{}.relay-profile", d.session)),
        "P1",
    )
    .unwrap();
}

/// #485: the connection dies while the session is idle and the next command
/// is `close`. It must close the session's tabs over a new connection, not
/// report `closed` after sending its closes into the dead one. Both for a
/// session bound to a relay profile and for one on a plain endpoint.
#[test]
fn close_after_an_idle_restart_closes_the_tabs() {
    for bound in [false, true] {
        let (fake, cdp) = Fake::start();
        let d = Daemon::start(&format!("rc-close-{bound}"), &cdp);
        if bound {
            bind_to_profile(&d, &cdp);
        }
        three_tabs(&d);
        let created = created_by_session(&d);

        fake.go_down();
        fake.come_back(true);
        let r = d.send(json!({"id": "z", "action": "close"}));
        assert_eq!(r["success"], true, "bound={bound}: {r}");
        assert_eq!(r["data"]["closed"], true, "bound={bound}: {r}");
        let open = open_targets(&fake);
        for t in &created {
            assert!(
                !open.contains(t),
                "bound={bound}: {t} left open after close: {open:?}"
            );
        }
        assert!(
            !created_record(&d).exists(),
            "bound={bound}: close left the ownership record"
        );
    }
}

/// The browser is still unreachable when `close` runs: it says the close is
/// incomplete, closes nothing, keeps the ownership record and the daemon, and
/// a `close` once the browser is back closes the tabs.
#[test]
fn close_while_the_browser_is_unreachable_never_reports_closed() {
    let (fake, cdp) = Fake::start();
    let d = Daemon::start("rc-close-down", &cdp);
    three_tabs(&d);
    let created = created_by_session(&d);

    fake.go_down();
    let r = d.send(json!({"id": "z1", "action": "close"}));
    assert_eq!(r["success"], false, "{r}");
    assert!(
        r["error"]
            .as_str()
            .unwrap_or("")
            .contains("close incomplete"),
        "{r}"
    );
    assert!(r["data"]["closed"] != true, "{r}");
    let open = open_targets(&fake);
    for t in &created {
        assert!(open.contains(t), "{t} gone while the browser was down");
    }
    assert!(created_record(&d).exists(), "ownership record dropped");
    // The daemon that knows the tabs stays for the retry. (A daemon exits
    // shortly after a `close`; give it time to, so this does not race it.)
    std::thread::sleep(Duration::from_millis(1000));
    assert!(
        d.sock_path().exists(),
        "an incomplete close ended the daemon"
    );

    fake.come_back(false);
    let r = d.send(json!({"id": "z2", "action": "close"}));
    assert_eq!(r["success"], true, "{r}");
    assert_eq!(r["data"]["closed"], true, "{r}");
    let open = open_targets(&fake);
    for t in &created {
        assert!(!open.contains(t), "{t} left open: {open:?}");
    }
}

/// Relay profile P1's records name `ws` (what its native host writes).
fn publish_profile_endpoint(d: &Daemon, ws: &str) {
    std::fs::write(
        d.relay.path().join("relay-ext-profile-P1"),
        r#"{"id": "P1", "email": "p1@example.test"}"#,
    )
    .unwrap();
    std::fs::write(d.relay.path().join("relay-cdp-url-P1"), ws).unwrap();
}

/// #485 as found: a session on a relay profile it was never pinned to (it
/// auto-connected to the only connected profile), the relay host restarts
/// onto a new port, and `close` comes next. Its tabs are closed through the
/// profile's new endpoint: while the killed host's record still names the old
/// port (the new one is written a moment later), and when the new record is
/// already there.
#[test]
fn close_after_a_relay_restart_closes_the_tabs_on_the_new_port() {
    for record_late in [true, false] {
        let (fake, cdp) = Fake::start();
        let d = Daemon::start(&format!("rc-close-port-{record_late}"), &cdp);
        publish_profile_endpoint(&d, &cdp);
        three_tabs(&d);
        let created = created_by_session(&d);

        let new_ws = fake.restart_on_new_port();
        if record_late {
            let relay = d.relay.path().to_path_buf();
            let ws = new_ws.clone();
            std::thread::spawn(move || {
                std::thread::sleep(Duration::from_millis(1500));
                std::fs::write(relay.join("relay-cdp-url-P1"), ws).unwrap();
            });
        } else {
            publish_profile_endpoint(&d, &new_ws);
        }
        let r = d.send(json!({"id": "z", "action": "close"}));
        assert_eq!(r["success"], true, "late={record_late}: {r}");
        assert_eq!(r["data"]["closed"], true, "late={record_late}: {r}");
        let open = open_targets(&fake);
        for t in &created {
            assert!(
                !open.contains(t),
                "late={record_late}: {t} left open after close: {open:?}"
            );
        }
        assert!(
            !created_record(&d).exists(),
            "late={record_late}: close left the ownership record"
        );
    }
}

/// Another profile's relay is the only one live while this session's profile
/// stays down: `close` must not touch it, and reports the close incomplete.
#[test]
fn close_never_closes_through_another_profile() {
    let (fake, cdp) = Fake::start();
    let d = Daemon::start("rc-close-other", &cdp);
    publish_profile_endpoint(&d, &cdp);
    three_tabs(&d);
    let created = created_by_session(&d);

    // The same browser comes back, but published as profile P2's relay.
    let new_ws = fake.restart_on_new_port();
    std::fs::remove_file(d.relay.path().join("relay-cdp-url-P1")).unwrap();
    std::fs::remove_file(d.relay.path().join("relay-ext-profile-P1")).unwrap();
    std::fs::write(
        d.relay.path().join("relay-ext-profile-P2"),
        r#"{"id": "P2", "email": "p2@example.test"}"#,
    )
    .unwrap();
    std::fs::write(d.relay.path().join("relay-cdp-url-P2"), &new_ws).unwrap();
    let r = d.send(json!({"id": "z", "action": "close"}));
    assert_eq!(r["success"], false, "{r}");
    assert!(
        r["error"]
            .as_str()
            .unwrap_or("")
            .contains("close incomplete"),
        "{r}"
    );
    let open = open_targets(&fake);
    for t in &created {
        assert!(open.contains(t), "{t} closed through another profile");
    }
    assert!(created_record(&d).exists(), "ownership record dropped");
}

/// The `--json` error of a CLI run, with its code.
fn cli_error(out: &Output) -> (String, String) {
    let all = text(out);
    let v: Value = all
        .lines()
        .find_map(|l| serde_json::from_str::<Value>(l).ok())
        .unwrap_or_else(|| panic!("no JSON in: {all}"));
    (
        v["error"].as_str().unwrap_or("").to_string(),
        v["code"].as_str().unwrap_or("").to_string(),
    )
}

fn created_and_attached(fake: &Fake) -> (u32, u32) {
    let b = fake.0.lock().unwrap();
    (b.created, b.attached)
}

/// #486: the session's relay profile has no window open. Connecting must not
/// create a tab there (Chrome would open a window for it, in front of the
/// user): `open` and `extension call` say `profile not open` with the fix and
/// leave nothing behind. Once a window is open the same session works.
#[test]
fn a_profile_without_a_window_is_refused_and_nothing_is_created() {
    let (fake, cdp) = Fake::start();
    let d = Daemon::start("rc-no-window", &cdp);
    publish_profile_endpoint(&d, &cdp);
    fake.0.lock().unwrap().windows = Some(json!([]));

    for args in [
        &["open", "https://example.test/"][..],
        &["extension", "call", "tabs.query", "{}"][..],
    ] {
        let started = Instant::now();
        let out = d.cli(args);
        let (error, code) = cli_error(&out);
        assert!(!out.status.success(), "{args:?}: {error}");
        assert!(error.starts_with("profile not open:"), "{args:?}: {error}");
        assert!(
            error.contains("Open a window in that profile first"),
            "{args:?}: {error}"
        );
        assert_eq!(code, "profile_not_open", "{args:?}");
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "{args:?} waited"
        );
    }
    assert_eq!(created_and_attached(&fake), (0, 0));
    assert!(fake.0.lock().unwrap().targets.is_empty());
    assert!(fake
        .0
        .lock()
        .unwrap()
        .calls
        .iter()
        .any(|c| c == "windows.getAll"));
    assert!(
        !created_record(&d).exists(),
        "an ownership record was written"
    );

    fake.0.lock().unwrap().windows = None;
    let r = d.send(json!({"id": "a", "action": "tab_list"}));
    assert_eq!(r["success"], true, "{r}");
    assert_eq!(created_and_attached(&fake), (1, 1), "{r}");
}

/// An answer that does not show a real normal window is not "open", and it is
/// not "no window" either: `profile window unavailable`, with nothing created
/// or attached, through the real CLI.
#[test]
fn a_window_list_that_cannot_be_trusted_creates_nothing() {
    let (fake, cdp) = Fake::start();
    let d = Daemon::start("rc-bad-windows", &cdp);
    publish_profile_endpoint(&d, &cdp);
    for bad in [
        json!(null),
        json!({}),
        json!([null]),
        json!([{}]),
        json!([{"id": 1}]),
        json!([{"type": "normal"}]),
        json!([{"id": "1", "type": "normal"}]),
        json!([{"id": 1, "type": "popup"}]),
        json!([{"id": 1, "type": "normal"}, {"id": 2, "type": "app"}]),
        json!([{"id": 1, "type": "normal"}, {"id": 1, "type": "normal"}]),
    ] {
        fake.0.lock().unwrap().windows = Some(bad.clone());
        let out = d.cli(&["open", "https://example.test/"]);
        let (error, code) = cli_error(&out);
        assert!(!out.status.success(), "{bad}: {error}");
        assert!(
            error.starts_with("profile window unavailable:"),
            "{bad}: {error}"
        );
        assert!(error.contains("nothing was opened"), "{bad}: {error}");
        assert_eq!(code, "profile_window_unavailable", "{bad}");
        assert_eq!(created_and_attached(&fake), (0, 0), "{bad}");
    }
    assert!(!created_record(&d).exists());
}

/// A first tab that cannot be set up is closed, and reported gone only once
/// an authoritative read-back says so: Chrome's target list on a direct
/// connection, the versioned `ABExt.tabPresence` on the relay.
#[test]
fn a_first_tab_that_cannot_be_set_up_is_closed_and_read_back() {
    for relay in [false, true] {
        let (fake, cdp) = Fake::start();
        let d = Daemon::start(&format!("rc-setup-fails-{relay}"), &cdp);
        if relay {
            publish_profile_endpoint(&d, &cdp);
        }
        {
            let mut b = fake.0.lock().unwrap();
            b.page_enable_fails = true;
        }
        let r = d.send(json!({"id": "a", "action": "tab_list"}));
        let error = r["error"].as_str().unwrap_or("");
        assert_eq!(r["success"], false, "relay={relay}: {r}");
        assert!(
            error.starts_with("first tab setup failed:"),
            "relay={relay}: {r}"
        );
        assert!(
            error.contains("Chrome confirms it is gone"),
            "relay={relay}: {r}"
        );
        assert!(open_targets(&fake).is_empty(), "relay={relay}");
        let record = std::fs::read_to_string(created_record(&d)).unwrap_or_default();
        assert!(!record.contains("T1"), "relay={relay}: {record}");

        fake.0.lock().unwrap().page_enable_fails = false;
        let r = d.send(json!({"id": "b", "action": "tab_list"}));
        assert_eq!(r["success"], true, "relay={relay}: {r}");
        assert_eq!(open_targets(&fake), vec!["T2".to_string()], "relay={relay}");
    }
}

/// Setup fails, then the close is refused, never answered, or acknowledged
/// while the tab stays, or (relay) gone but not provable, as with
/// ab-connect 0.5.32. The error must not claim cleanup; the tab keeps its
/// delete right on disk; the next command adopts that tab instead of opening
/// another; and a later `close` removes it.
#[test]
fn a_first_tab_whose_close_is_not_confirmed_keeps_its_delete_right() {
    let cases = [
        (CloseMode::Refused, true),
        (CloseMode::Hangs, true),
        (CloseMode::AckButStays, true),
        (CloseMode::Closes, false),
    ];
    for (i, (mode, contract)) in cases.into_iter().enumerate() {
        let label = format!("{mode:?}-contract={contract}");
        let (fake, cdp) = Fake::start();
        let d = Daemon::start(&format!("rc-unconfirmed-{i}"), &cdp);
        publish_profile_endpoint(&d, &cdp);
        {
            let mut b = fake.0.lock().unwrap();
            b.page_enable_fails = true;
            b.close = mode;
            b.old_extension = !contract;
        }
        let started = Instant::now();
        let out = d.cli(&["open", "https://example.test/"]);
        let (error, code) = cli_error(&out);
        assert!(
            started.elapsed() < Duration::from_secs(40),
            "{label}: too slow"
        );
        assert_eq!(code, "first_tab_cleanup_incomplete", "{label}: {error}");
        assert!(error.contains("target T1"), "{label}: {error}");
        assert!(error.contains("Do not rerun `open`"), "{label}: {error}");
        assert!(!error.contains("nothing is left"), "{label}: {error}");
        let still = mode != CloseMode::Closes;
        assert_eq!(
            open_targets(&fake).contains(&"T1".to_string()),
            still,
            "{label}"
        );
        let record = std::fs::read_to_string(created_record(&d)).unwrap_or_default();
        assert!(
            record.contains("T1"),
            "{label}: delete right lost: {record}"
        );
        assert_eq!(fake.0.lock().unwrap().created, 1, "{label}");

        {
            let mut b = fake.0.lock().unwrap();
            b.page_enable_fails = false;
            b.close = CloseMode::Closes;
        }
        if still {
            // The next command takes the leftover tab back, opening nothing.
            let r = d.send(json!({"id": "b", "action": "tab_list"}));
            assert_eq!(r["success"], true, "{label}: {r}");
            assert_eq!(fake.0.lock().unwrap().created, 1, "{label}: opened another");
        }
        let r = d.send(json!({"id": "z", "action": "close"}));
        if contract {
            assert_eq!(r["success"], true, "{label}: {r}");
        } else {
            // An extension that cannot prove a tab gone (0.5.32) never
            // lets `close` claim it either; the tab is in fact gone.
            assert_eq!(r["success"], false, "{label}: {r}");
            assert!(
                r["error"]
                    .as_str()
                    .unwrap_or("")
                    .contains("close incomplete"),
                "{label}: {r}"
            );
        }
        assert!(
            open_targets(&fake).is_empty(),
            "{label}: {:?}",
            open_targets(&fake)
        );
        assert_eq!(fake.0.lock().unwrap().created, 1, "{label}: opened another");
    }
}

/// A setup that never answers is cut off inside the overall first-tab
/// deadline (under the client's 45 s), the tab is closed and read back.
#[test]
fn a_hanging_setup_ends_inside_the_deadline() {
    let (fake, cdp) = Fake::start();
    let d = Daemon::start("rc-setup-hangs", &cdp);
    publish_profile_endpoint(&d, &cdp);
    {
        let mut b = fake.0.lock().unwrap();
        b.page_enable_hangs = true;
    }
    let started = Instant::now();
    let r = d.send(json!({"id": "a", "action": "tab_list"}));
    let took = started.elapsed();
    assert!(took < Duration::from_secs(35), "took {took:?}: {r}");
    let error = r["error"].as_str().unwrap_or("");
    assert!(error.starts_with("first tab setup failed:"), "{r}");
    assert!(error.contains("did not finish in time"), "{r}");
    assert!(open_targets(&fake).is_empty());
}

/// The delete right cannot be saved (the record path is taken by a
/// directory): the tab is removed at once rather than kept without a record.
/// When that removal is not confirmed either, the error names the tab and the
/// daemon keeps the right: either the next command takes the tab back as the
/// session's own, or `close` removes it. Nothing else is opened.
#[test]
fn a_delete_right_that_cannot_be_saved_is_never_reported_as_clean() {
    for (i, (mode, then)) in [
        (CloseMode::Closes, ""),
        (CloseMode::AckButStays, "close"),
        (CloseMode::Refused, "reconnect"),
    ]
    .into_iter()
    .enumerate()
    {
        let label = format!("{mode:?}-{then}");
        let (fake, cdp) = Fake::start();
        let d = Daemon::start(&format!("rc-unsaved-{i}"), &cdp);
        publish_profile_endpoint(&d, &cdp);
        std::fs::create_dir(created_record(&d)).unwrap();
        {
            let mut b = fake.0.lock().unwrap();
            b.close = mode;
        }
        let r = d.send(json!({"id": "a", "action": "tab_list"}));
        let error = r["error"].as_str().unwrap_or("");
        assert_eq!(r["success"], false, "{label}: {r}");
        assert!(
            error.contains("saving this session's delete right"),
            "{label}: {r}"
        );
        assert_eq!(created_and_attached(&fake), (1, 0), "{label}");
        if mode == CloseMode::Closes {
            assert!(error.starts_with("first tab setup failed:"), "{r}");
            assert!(open_targets(&fake).is_empty());
            continue;
        }
        assert!(error.starts_with("first tab cleanup incomplete:"), "{r}");
        assert!(error.contains("target T1"), "{r}");
        assert!(error.contains("only this session's running daemon"), "{r}");
        assert!(!error.contains("nothing is left"), "{r}");
        assert_eq!(open_targets(&fake), vec!["T1".to_string()], "{label}");

        std::fs::remove_dir(created_record(&d)).unwrap();
        fake.0.lock().unwrap().close = CloseMode::Closes;
        if then == "reconnect" {
            // The next command takes the tab back as created, opening nothing.
            let r = d.send(json!({"id": "b", "action": "tab_list"}));
            assert_eq!(r["success"], true, "{label}: {r}");
            assert_eq!(fake.0.lock().unwrap().created, 1, "{label}: opened another");
            assert_eq!(created_by_one(&d), vec!["T1".to_string()], "{label}: {r}");
            let record = std::fs::read_to_string(created_record(&d)).unwrap_or_default();
            assert!(record.contains("T1"), "{label}: not saved again: {record}");
        }
        let r = d.send(json!({"id": "z", "action": "close"}));
        assert_eq!(r["success"], true, "{label}: {r}");
        assert!(
            open_targets(&fake).is_empty(),
            "{label}: {:?}",
            open_targets(&fake)
        );
        assert_eq!(fake.0.lock().unwrap().created, 1, "{label}: opened another");
    }
}

/// The target ids `tab list` marks as created by this session (any number).
fn created_by_one(d: &Daemon) -> Vec<String> {
    let r = d.send(json!({"id": "o", "action": "tab_list"}));
    r["data"]["tabs"]
        .as_array()
        .map(|tabs| {
            tabs.iter()
                .filter(|t| t["ownership"] == "created")
                .filter_map(|t| t["targetId"].as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

/// The cleanup close of a first tab goes through, but reading it back fails:
/// cleanup is not confirmed and the right is kept. The Chrome tab id the
/// verifier saw before the close is kept with it, so the same session's
/// `close` proves the tab gone once read-back works, without `--force`.
#[test]
fn a_closed_first_tab_whose_read_back_failed_is_confirmed_by_the_next_close() {
    let (fake, cdp) = Fake::start();
    let d = Daemon::start("rc-readback-fails", &cdp);
    publish_profile_endpoint(&d, &cdp);
    {
        let mut b = fake.0.lock().unwrap();
        b.page_enable_fails = true;
        b.presence_fails_after_close = true;
    }
    let r = d.send(json!({"id": "a", "action": "tab_list"}));
    let error = r["error"].as_str().unwrap_or("");
    assert!(error.starts_with("first tab cleanup incomplete:"), "{r}");
    assert!(error.contains("target T1"), "{r}");
    assert!(open_targets(&fake).is_empty(), "the close did go through");
    let record = std::fs::read_to_string(created_record(&d)).unwrap_or_default();
    assert!(record.contains("T1"), "delete right lost: {record}");

    fake.0.lock().unwrap().presence_fails_after_close = false;
    let r = d.send(json!({"id": "z", "action": "close"}));
    assert_eq!(r["success"], true, "{r}");
    assert_eq!(r["data"]["verifiedAbsent"], true, "{r}");
    assert!(!created_record(&d).exists(), "the right outlived the tab");
    assert_eq!(fake.0.lock().unwrap().created, 1, "opened another tab");
}
