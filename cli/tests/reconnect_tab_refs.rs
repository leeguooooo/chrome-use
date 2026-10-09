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
}

#[derive(Clone)]
struct Fake(Arc<Mutex<Browser>>);

impl Fake {
    fn start() -> (Self, String) {
        let fake = Fake(Arc::new(Mutex::new(Browser {
            up: true,
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
            });
        });
        let url = rx.recv().unwrap();
        (fake, url)
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
                json!({"sessionId": format!("S-{}", params["targetId"].as_str().unwrap_or(""))})
            }
            "Target.closeTarget" => {
                let id = params["targetId"].as_str().unwrap_or("").to_string();
                b.targets.retain(|(t, _)| *t != id);
                json!({"success": true})
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
        let mut reply = json!({"id": req["id"], "result": fake.reply(&req)});
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
