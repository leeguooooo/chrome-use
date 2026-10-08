//! Real binary parser/wire checks and real CLI -> daemon -> Chrome acceptance.
#![cfg(unix)]
mod common;
use common::{text, Stub};

#[test]
fn semantic_find_scope_and_literal_flags_reach_daemon() {
    let stub = Stub::start("semantic-wire");
    stub.ok(&[
        "find", "role", "button", "click", "--name", "Save", "--exact", "false", "--within",
        "#account",
    ]);
    let sent = stub.sent("getbyrole");
    assert_eq!(sent[0]["within"], "#account");
    assert_eq!(sent[0]["exact"], false);
    stub.ok(&[
        "find",
        "label",
        "Email",
        "fill",
        "--",
        "--name",
        "--observe",
    ]);
    let sent = stub.sent("getbylabel");
    assert_eq!(sent[0]["value"], "--name --observe");
    assert!(sent[0].get("observe").is_none());
    for args in [
        vec![
            "find", "role", "button", "click", "--within", "#a", "--within", "#b",
        ],
        vec!["find", "role", "button", "click", "--strict"],
        vec!["find", "role", "button", "click", "--within"],
    ] {
        stub.clear();
        let out = stub.run(&args);
        assert!(!out.status.success(), "{}", text(&out));
        assert!(stub.sent("getbyrole").is_empty());
    }
}

#[cfg(feature = "e2e-tests")]
mod browser {
    use serde_json::Value;
    use std::process::{Command, Output};
    use std::time::Instant;
    const BIN: &str = env!("CARGO_BIN_EXE_chrome-use");
    struct Session {
        home: tempfile::TempDir,
        sock: tempfile::TempDir,
        name: String,
    }
    impl Session {
        fn new() -> Self {
            Self {
                home: tempfile::tempdir().unwrap(),
                sock: tempfile::Builder::new()
                    .prefix("cus")
                    .tempdir_in("/tmp")
                    .unwrap(),
                name: format!("semantic-{}", std::process::id()),
            }
        }
        fn run(&self, args: &[&str]) -> Output {
            Command::new(BIN)
                .args(["--session", &self.name, "--json", "--launch"])
                .args(args)
                .env("HOME", self.home.path())
                .env("USERPROFILE", self.home.path())
                .env("AGENT_BROWSER_SOCKET_DIR", self.sock.path())
                .env("AGENT_BROWSER_ALLOW_HEADLESS", "1")
                .env_remove("AGENT_BROWSER_CDP")
                .env_remove("AGENT_BROWSER_PROVIDER")
                .env("NO_COLOR", "1")
                .output()
                .unwrap()
        }
        fn ok(&self, args: &[&str]) -> Value {
            let start = Instant::now();
            let out = self.run(args);
            assert!(
                out.status.success(),
                "{args:?}: {} {}",
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            );
            eprintln!(
                "semantic timing {} ms: {}",
                start.elapsed().as_millis(),
                args[0]
            );
            serde_json::from_slice::<Value>(&out.stdout).unwrap()["data"].clone()
        }
        fn refused(&self, args: &[&str], word: &str) {
            let out = self.run(args);
            assert!(!out.status.success(), "unexpected success");
            let response = String::from_utf8_lossy(&out.stdout);
            assert!(response.contains(word), "{response}");
        }
    }
    impl Drop for Session {
        fn drop(&mut self) {
            let _ = self.run(&["close"]);
        }
    }
    #[test]
    #[ignore = "isolated real Chrome and daemon"]
    fn real_cli_semantic_scope_and_strict_no_mistarget() {
        let session = Session::new();
        let page = session.home.path().join("fixture.html");
        std::fs::write(&page,r##"<!doctype html><article id="account-a"><h2>Alpha account</h2><button onclick="document.querySelector('#receipt').textContent='alpha'">Save</button><h3>Email</h3><label>Email<input></label><input placeholder="Address"><input data-testid="secret" title="Private" value="fixture-secret"></article>
<section id="account-b" role="region" aria-labelledby="beta-title"><h2 id="beta-title">Beta account</h2><button aria-labelledby="save-label" onclick="document.querySelector('#receipt').textContent='beta'"><span id="save-label">Save</span></button><label>Email<input></label><input placeholder="Address"><input data-testid="secret" title="Private" value="fixture-secret"></section>
<button style="display:none">Save</button><p id="receipt" role="status">untouched</p><iframe id="embedded" srcdoc="<p>Frame receipt</p><button onclick=&quot;parent.document.querySelector('#receipt').textContent='frame'&quot;>Frame action</button>"></iframe>"##).unwrap();
        session.ok(&["open", &format!("file://{}", page.display())]);
        for strategy in ["role", "text"] {
            let args = if strategy == "role" {
                vec![
                    "find", "role", "button", "click", "--name", "Save", "--exact",
                ]
            } else {
                vec!["find", "text", "Save", "click", "--exact"]
            };
            session.refused(&args, "2 visible");
            assert_eq!(
                session.ok(&["get", "text", "#receipt"])["text"],
                "untouched"
            );
        }
        // Discover the scope from page content rather than preknowing its ID.
        let found = session.ok(&["find", "query", "Beta account"]);
        let scope = found["candidates"]
            .as_array()
            .unwrap()
            .iter()
            .find(|c| {
                c["role"] == "region"
                    && c["text"]
                        .as_str()
                        .unwrap_or("")
                        .to_ascii_lowercase()
                        .contains("beta account")
            })
            .unwrap()["selector"]
            .as_str()
            .unwrap()
            .to_string();
        session.refused(
            &[
                "find",
                "role",
                "button",
                "click",
                "--name",
                "Save",
                "--within",
                "article,section",
            ],
            "exactly one container",
        );
        session.ok(&[
            "find", "role", "button", "click", "--name", "Save", "--exact", "--within", &scope,
        ]);
        assert_eq!(session.ok(&["get", "text", "#receipt"])["text"], "beta");
        for (kind, value) in [
            ("label", "Email"),
            ("placeholder", "Address"),
            ("title", "Private"),
            ("testid", "secret"),
        ] {
            session.ok(&["find", kind, value, "--within", &scope]);
        }
        let mut timings = Vec::new();
        for _ in 0..5 {
            let started = Instant::now();
            session.ok(&[
                "find", "role", "button", "click", "--name", "Save", "--exact", "--within", &scope,
            ]);
            timings.push(started.elapsed().as_millis());
            assert_eq!(session.ok(&["get", "text", "#receipt"])["text"], "beta");
        }
        timings.sort();
        eprintln!(
            "SEMANTIC_WARM_CLI_MS {}",
            serde_json::json!({"samples":timings,"median":timings[2],"successes":5,"trials":5,"profile":"debug","model_time_included":false})
        );
        // All semantic strategies reject repeated visible targets; the supplied
        // fill value is never included in ambiguity diagnostics.
        for (kind, value) in [
            ("label", "Email"),
            ("placeholder", "Address"),
            ("title", "Private"),
            ("testid", "secret"),
        ] {
            session.refused(
                &["find", kind, value, "fill", "DO-NOT-LOG-THIS"],
                "2 visible",
            );
        }
        let skipped = session.ok(&[
            "find",
            "role",
            "button",
            "click",
            "--name",
            "Missing",
            "--if-present",
        ]);
        assert_eq!(skipped["skipped"], true);
        session.refused(
            &[
                "find",
                "role",
                "button",
                "click",
                "--name",
                "Save",
                "--if-present",
            ],
            "2 visible",
        );
        // Scope refs go through the existing identity guard after replacement.
        let snap = session.ok(&["snapshot", "-i"]);
        let tree = snap["snapshot"].as_str().unwrap();
        let button_ref = tree
            .lines()
            .filter(|l| l.contains("button") && l.contains("Save"))
            .nth(1)
            .unwrap()
            .split("[ref=")
            .nth(1)
            .unwrap()
            .split([',', ']'])
            .next()
            .unwrap();
        let scoped_ref = format!("@{button_ref}");
        session.ok(&["eval", "document.querySelector('#account-b').outerHTML=document.querySelector('#account-b').outerHTML"]);
        session.ok(&[
            "find",
            "role",
            "button",
            "--name",
            "Save",
            "--exact",
            "--within",
            &scoped_ref,
        ]);
        let snap = session.ok(&["snapshot", "-i"]);
        let frame_line = snap["snapshot"]
            .as_str()
            .unwrap()
            .lines()
            .find(|l| l.contains("Frame action"))
            .unwrap();
        let frame_ref = format!(
            "@{}",
            frame_line
                .split("[ref=")
                .nth(1)
                .unwrap()
                .split([',', ']'])
                .next()
                .unwrap()
        );
        session.refused(
            &["find", "role", "button", "--within", &frame_ref],
            "iframe",
        );
        session.ok(&["click", &frame_ref]);
        assert_eq!(session.ok(&["get", "text", "#receipt"])["text"], "frame");
        session.ok(&["frame", "#embedded"]);
        session.refused(
            &["find", "role", "button", "click", "--name", "Save"],
            "main document only",
        );
        session.ok(&["frame", "main"]);
        assert_eq!(session.ok(&["get", "text", "#receipt"])["text"], "frame");
        // A re-render after selection must fail before dispatch, not click a
        // newly inserted button at the old coordinates or replay the action.
        session.ok(&["eval", "document.querySelector('#receipt').textContent='untouched'; window.oldSet=Element.prototype.setAttribute; Element.prototype.setAttribute=function(n,v){if(n==='data-chrome-use-located' && this.closest('#account-b')){this.outerHTML=this.outerHTML;} return window.oldSet.call(this,n,v);}"]);
        let out = session.run(&[
            "find", "role", "button", "click", "--name", "Save", "--exact", "--within", &scope,
        ]);
        assert!(
            !out.status.success(),
            "detached semantic target unexpectedly succeeded"
        );
        assert_eq!(
            session.ok(&["get", "text", "#receipt"])["text"],
            "untouched"
        );
        session.ok(&["eval", "Element.prototype.setAttribute=window.oldSet"]);
        session.ok(&["eval", "document.querySelector('#account-b').innerHTML='<h2>Beta account</h2><button onclick=\"document.querySelector(\'#receipt\').textContent=\'nested\'\"><span>Commit</span><span>Commit</span></button><div role=\"button\" onclick=\"document.querySelector(\'#receipt\').textContent=\'custom\'\"><span id=\"inner-leaf\">Custom Save</span></div>'"]);
        session.ok(&[
            "find", "text", "Commit", "click", "--exact", "--within", &scope,
        ]);
        assert_eq!(session.ok(&["get", "text", "#receipt"])["text"], "nested");
        session.refused(
            &[
                "find",
                "text",
                "Custom Save",
                "click",
                "--exact",
                "--within",
                "#inner-leaf",
            ],
            "outside the scope",
        );
        assert_eq!(session.ok(&["get", "text", "#receipt"])["text"], "nested");
        session.ok(&[
            "find",
            "text",
            "Custom Save",
            "click",
            "--exact",
            "--within",
            &scope,
        ]);
        assert_eq!(session.ok(&["get", "text", "#receipt"])["text"], "custom");
        // Restore fixture for explicit first/nth compatibility checks.
        session.ok(&["open", &format!("file://{}", page.display())]);

        // Full text's iframe receipts and explicit ordering remain available.
        assert!(session
            .ok(&["get", "text"])
            .to_string()
            .contains("Frame receipt"));
        session.ok(&["find", "first", "article button, section button", "click"]);
        assert_eq!(session.ok(&["get", "text", "#receipt"])["text"], "alpha");
        session.ok(&[
            "find",
            "nth",
            "1",
            "article button, section button",
            "click",
        ]);
        assert_eq!(session.ok(&["get", "text", "#receipt"])["text"], "beta");
    }
}
