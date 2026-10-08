//! ChooseBrowser rules against a session bound to a profile, through the real
//! binary and a stub daemon (no browser): what is refused never reaches the
//! daemon, and what is sent carries the url the guard checked. Also through
//! the MCP stdio server, which runs this binary per tool call.
//!
//! macOS-only: ChooseBrowser and the Chrome data root these fixtures write
//! (`~/Library/Application Support/Google/Chrome`) are macOS paths.
#![cfg(target_os = "macos")]
mod common;
use common::{text, Stub};
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::Stdio;

const RULES: &str = r#"{"version":2,"rules":[
  {"ruleId":"r-one","match":{"domain":"one.example"},
   "action":{"type":"always_open_in","bundleIdentifier":"com.google.Chrome::profile::111"}},
  {"ruleId":"r-two","match":{"domain":"two.example"},
   "action":{"type":"always_open_in","bundleIdentifier":"com.google.Chrome::profile::222"}},
  {"ruleId":"r-local","match":{"domain":"localhost"},
   "action":{"type":"always_open_in","bundleIdentifier":"com.google.Chrome::profile::222"}},
  {"ruleId":"r-v6","match":{"domain":"[::1]"},
   "action":{"type":"always_open_in","bundleIdentifier":"com.google.Chrome::profile::222"}},
  {"ruleId":"r-twin","match":{"domain":"twin.example"},
   "action":{"type":"always_open_in","bundleIdentifier":"com.google.Chrome::profile::555"}},
  {"ruleId":"r-gone","match":{"domain":"gone.example"},
   "action":{"type":"always_open_in","bundleIdentifier":"com.google.Chrome::profile::999"}}
]}"#;

const LOCAL_STATE: &str = r#"{"profile":{"info_cache":{
  "Profile 1":{"gaia_id":"111","user_name":"one@x.test","name":"One"},
  "Profile 2":{"gaia_id":"222","user_name":"two@x.test","name":"Two"},
  "Profile 3":{"gaia_id":"555","user_name":"twin@x.test","name":"Twin A"},
  "Profile 4":{"gaia_id":"555","user_name":"twin@x.test","name":"Twin B"}
}}}"#;

struct Fixture {
    stub: Stub,
    rules: PathBuf,
}

impl Fixture {
    /// A stub session bound to "One" (Profile 1), per its session record.
    fn new(name: &str) -> Self {
        let stub = Stub::start(name);
        let root = chrome_root(&stub);
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("Local State"), LOCAL_STATE).unwrap();
        let rules = stub.home().join("rules.json");
        std::fs::write(&rules, RULES).unwrap();
        let f = Fixture { stub, rules };
        f.bind(json!({
            "id": "relay-one",
            "root": root.display().to_string(),
            "dir": "Profile 1",
            "label": "One (Profile 1, one@x.test)",
        }));
        f
    }

    fn bind(&self, record: Value) {
        std::fs::write(
            self.stub
                .sock_dir()
                .join(format!("{}.browser-profile", self.stub.session())),
            record.to_string(),
        )
        .unwrap();
    }

    fn envs(&self) -> Vec<(&'static str, String)> {
        vec![(
            "CHROME_USE_CHOOSEBROWSER_RULES_FILE",
            self.rules.display().to_string(),
        )]
    }

    fn run(&self, args: &[&str]) -> std::process::Output {
        let envs = self.envs();
        let envs: Vec<(&str, &str)> = envs.iter().map(|(k, v)| (*k, v.as_str())).collect();
        self.stub.clear();
        self.stub.run_env(args, &envs)
    }

    /// Run, require success, return what reached the daemon for `action`.
    fn sent_ok(&self, args: &[&str], action: &str) -> Vec<Value> {
        let out = self.run(args);
        assert!(out.status.success(), "{args:?}: {}", text(&out));
        self.stub.sent(action)
    }

    /// Run, require a refusal that sent nothing at all.
    fn refused(&self, args: &[&str]) -> String {
        let out = self.run(args);
        let t = text(&out);
        assert!(!out.status.success(), "{args:?} should be refused: {t}");
        for action in ["navigate", "tab_new", "click"] {
            assert!(
                self.stub.sent(action).is_empty(),
                "{args:?} sent {action} although refused: {t}"
            );
        }
        t
    }
}

fn chrome_root(stub: &Stub) -> PathBuf {
    stub.home()
        .join("Library/Application Support/Google/Chrome")
}

#[test]
fn a_bound_session_opens_its_own_rule_site_and_refuses_another_profiles() {
    let f = Fixture::new("cbg-own");
    let sent = f.sent_ok(&["open", "https://one.example/a"], "navigate");
    assert_eq!(sent[0]["url"], "https://one.example/a");
    assert_eq!(sent[0]["_cbSkip"], false);

    let t = f.refused(&["open", "https://two.example/"]);
    assert!(t.contains("r-two"), "{t}");
    assert!(t.contains("is bound to One"), "{t}");
    assert!(t.contains("--session"), "{t}");
    assert!(t.contains("--no-choosebrowser"), "{t}");

    // Same via `goto`, `navigate` and `tab new`.
    f.refused(&["goto", "two.example"]);
    f.refused(&["navigate", "https://two.example/x"]);
    f.refused(&["tab", "new", "two.example"]);

    // A url no rule covers is unaffected.
    f.sent_ok(&["open", "https://example.org/"], "navigate");
}

#[test]
fn no_choosebrowser_opens_it_in_the_bound_profile_and_tells_the_daemon() {
    let f = Fixture::new("cbg-skip");
    let sent = f.sent_ok(
        &["open", "https://two.example/", "--no-choosebrowser"],
        "navigate",
    );
    assert_eq!(sent[0]["_cbSkip"], true);
}

/// The guard reads the url off the parsed command — the one sent on the
/// wire — so a flag value that looks like a host is never the one checked,
/// and hosts with no dot (localhost, IPv6) are still sites.
#[test]
fn the_url_checked_is_the_url_sent() {
    let f = Fixture::new("cbg-wire");
    let sent = f.sent_ok(
        &["open", "--label", "two.example", "https://one.example/x"],
        "navigate",
    );
    assert_eq!(sent[0]["url"], "https://one.example/x");
    let t = f.refused(&["open", "--label", "one.example", "https://two.example/x"]);
    assert!(t.contains("r-two"), "{t}");

    let t = f.refused(&["open", "localhost:8765/cb"]);
    assert!(t.contains("r-local"), "{t}");
    let t = f.refused(&["open", "http://[::1]:8765/cb"]);
    assert!(t.contains("r-v6"), "{t}");
}

#[test]
fn a_batch_is_checked_whole_before_any_step_runs() {
    let f = Fixture::new("cbg-batch");
    // Steps for two different profiles: refused up front.
    let t = f.refused(&[
        "batch",
        "open https://one.example/",
        "open https://two.example/",
    ]);
    assert!(t.contains("different Chrome profiles"), "{t}");
    // A step for another profile, then a click: neither runs.
    let t = f.refused(&["batch", "open https://two.example/", "click #buy"]);
    assert!(t.contains("is bound to One"), "{t}");
    // All steps on the bound profile's sites: runs.
    let sent = f.sent_ok(
        &[
            "batch",
            "open https://one.example/a",
            "snapshot",
            "goto https://one.example/b",
        ],
        "navigate",
    );
    assert_eq!(sent.len(), 2);
}

#[test]
fn an_ambiguous_key_refuses_and_a_stale_rule_does_not_block() {
    let f = Fixture::new("cbg-keys");
    let t = f.refused(&["open", "https://twin.example/"]);
    assert!(t.contains("matches 2 profiles"), "{t}");
    assert!(t.contains("Profile 3, Profile 4"), "{t}");
    f.sent_ok(&["open", "https://gone.example/"], "navigate");
}

/// An old record carries no data root, and the profile row has no relay id
/// to compare: not enough to tell, so refuse rather than guess.
#[test]
fn a_record_without_enough_identity_refuses() {
    let f = Fixture::new("cbg-old");
    f.bind(json!({"dir": "Profile 1", "label": "One"}));
    let t = f.refused(&["open", "https://one.example/"]);
    assert!(t.contains("cannot tell which profile"), "{t}");
}

/// MCP tool calls run this binary, so the guard applies to them the same
/// way: a standard JSON-RPC exchange over stdio.
#[test]
fn mcp_tool_calls_are_guarded_too() {
    let f = Fixture::new("cbg-mcp");
    let envs = f.envs();
    let envs: Vec<(&str, &str)> = envs.iter().map(|(k, v)| (*k, v.as_str())).collect();
    let mut child = f
        .stub
        .command(&envs)
        .arg("mcp")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("start mcp");
    let mut stdin = child.stdin.take().unwrap();
    let mut lines = BufReader::new(child.stdout.take().unwrap()).lines();
    let mut call = |id: u64, method: &str, params: Value| -> Value {
        let msg = json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params});
        writeln!(stdin, "{msg}").unwrap();
        stdin.flush().unwrap();
        let line = lines.next().expect("a response").unwrap();
        serde_json::from_str(&line).unwrap()
    };
    call(
        1,
        "initialize",
        json!({"protocolVersion": "2024-11-05", "capabilities": {},
               "clientInfo": {"name": "guard-test", "version": "0"}}),
    );
    let session = f.stub.session().to_string();

    f.stub.clear();
    let refused = call(
        2,
        "tools/call",
        json!({"name": "chrome_use_open",
               "arguments": {"url": "https://two.example/", "session": session}}),
    );
    assert_eq!(refused["result"]["isError"], true, "{refused}");
    let body = refused.to_string();
    assert!(body.contains("is bound to One"), "{body}");
    assert!(f.stub.sent("navigate").is_empty(), "{body}");

    f.stub.clear();
    let allowed = call(
        3,
        "tools/call",
        json!({"name": "chrome_use_open",
               "arguments": {"url": "https://one.example/", "session": session}}),
    );
    assert_eq!(allowed["result"]["isError"], false, "{allowed}");
    let sent = f.stub.sent("navigate");
    assert_eq!(sent.len(), 1, "{allowed}");
    assert_eq!(sent[0]["url"], "https://one.example/");

    drop(stdin);
    let _ = child.wait();
}
