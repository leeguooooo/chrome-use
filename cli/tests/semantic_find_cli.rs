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
        fn mcp_find(&self, arguments: Value) -> Value {
            use std::io::Write;
            use std::process::Stdio;
            let mut child = Command::new(BIN)
                .args(["mcp", "--tools", "all"])
                .env("HOME", self.home.path())
                .env("USERPROFILE", self.home.path())
                .env("AGENT_BROWSER_SOCKET_DIR", self.sock.path())
                .env("AGENT_BROWSER_ALLOW_HEADLESS", "1")
                .env_remove("AGENT_BROWSER_CDP")
                .env_remove("AGENT_BROWSER_PROVIDER")
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .unwrap();
            let mut input = child.stdin.take().unwrap();
            for request in [
                serde_json::json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-03-26","capabilities":{},"clientInfo":{"name":"semantic-regression","version":"1"}}}),
                serde_json::json!({"jsonrpc":"2.0","method":"notifications/initialized"}),
                serde_json::json!({"jsonrpc":"2.0","id":2,"method":"tools/list"}),
                serde_json::json!({"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"chrome_use_find","arguments":arguments}}),
            ] {
                writeln!(input, "{request}").unwrap();
            }
            drop(input);
            let output = child.wait_with_output().unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            let rows: Vec<Value> = String::from_utf8_lossy(&output.stdout)
                .lines()
                .map(|s| serde_json::from_str(s).unwrap())
                .collect();
            assert!(rows[0]["result"]["serverInfo"]["version"].is_string());
            let tools = rows[1]["result"]["tools"].as_array().unwrap();
            let find = tools
                .iter()
                .find(|t| t["name"] == "chrome_use_find")
                .unwrap();
            assert_eq!(
                find["inputSchema"]["properties"]["within"]["type"],
                "string"
            );
            rows[2]["result"].clone()
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
<img alt="Benchmark logo" width="16" height="16" src="data:image/svg+xml,%3Csvg xmlns='http://www.w3.org/2000/svg' width='16' height='16'%3E%3Crect width='16' height='16'/%3E%3C/svg%3E"><button style="display:none">Save</button><p id="receipt" role="status">untouched</p><iframe id="embedded" srcdoc="<p>Frame receipt</p><button onclick=&quot;parent.document.querySelector('#receipt').textContent='frame'&quot;>Frame action</button>"></iframe>"##).unwrap();
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
        let mcp=session.mcp_find(serde_json::json!({"locator":"role","value":"button","action":"click","name":"Save","exact":false,"within":scope,"session":session.name}));
        assert_eq!(mcp["isError"], false, "{mcp}");
        assert_eq!(session.ok(&["get", "text", "#receipt"])["text"], "beta");
        let mcp=session.mcp_find(serde_json::json!({"locator":"label","value":"Email","action":"fill","text":"--name --observe","within":scope,"session":session.name}));
        assert_eq!(mcp["isError"], false, "{mcp}");
        assert_eq!(
            session.ok(&["get", "value", "#account-b input"])["value"],
            "--name --observe"
        );
        assert!(
            !mcp.to_string().contains("was ignored"),
            "literal --observe was parsed as a flag: {mcp}"
        );
        let located = session.ok(&[
            "find", "role", "button", "--name", "Save", "--exact", "--within", &scope,
        ]);
        assert_eq!(located["located"]["name"], "Save");
        assert_eq!(located["located"]["role"], "button");
        assert_eq!(located["located"]["context"], "Beta account");
        let region = session.ok(&[
            "find",
            "role",
            "region",
            "--name",
            "Beta account",
            "--exact",
        ]);
        assert_eq!(region["located"]["name"], "Beta account");
        let mcp=session.mcp_find(serde_json::json!({"locator":"role","value":"button","action":"locate","name":"Save","exact":true,"within":scope,"session":session.name}));
        assert_eq!(mcp["isError"], false);
        assert_eq!(
            mcp["structuredContent"]["response"]["data"]["located"]["name"], "Save",
            "{mcp}"
        );
        let region_mcp=session.mcp_find(serde_json::json!({"locator":"role","value":"region","name":"Beta account","exact":true,"session":session.name}));
        assert_eq!(
            region_mcp["structuredContent"]["response"]["data"]["located"]["name"],
            "Beta account"
        );
        session.ok(&["eval", r##"document.querySelector('#account-b').insertAdjacentHTML('afterbegin',`<div id="many">${'<button data-testid="descriptor" hidden>Hidden descriptor</button>'.repeat(10)}<button data-testid="descriptor">Actual descriptor</button></div>`)"##]);
        let descriptor = session.ok(&["find", "testid", "descriptor", "--within", &scope]);
        assert_eq!(descriptor["located"]["name"], "Actual descriptor");
        assert_eq!(descriptor["visibleCount"], 1);
        session.ok(&["eval", "document.querySelector('#many').remove()"]);
        let image = session.ok(&["find", "role", "img", "--name", "Benchmark logo", "--exact"]);
        assert_eq!(image["located"]["name"], "Benchmark logo");
        let image_mcp=session.mcp_find(serde_json::json!({"locator":"role","value":"img","name":"Benchmark logo","exact":true,"session":session.name}));
        assert_eq!(image_mcp["isError"], false, "{image_mcp}");
        assert_eq!(
            image_mcp["structuredContent"]["response"]["data"]["located"]["name"],
            "Benchmark logo"
        );
        let mut timings = Vec::new();
        for _ in 0..5 {
            let started = Instant::now();
            let saved = session.ok(&[
                "find", "role", "button", "click", "--name", "Save", "--exact", "--within", &scope,
            ]);
            assert_eq!(saved["target"]["name"], "Save");
            assert_eq!(saved["target"]["role"], "button");
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
        session.ok(&["eval", r##"document.querySelector('#account-b').innerHTML=`<h2>Beta account</h2><button onclick="document.querySelector('#receipt').textContent='nested'"><span>Commit</span><span>Commit</span></button><div role="button" onclick="document.querySelector('#receipt').textContent='custom'"><span id="inner-leaf">Custom Save</span></div>`"##]);
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
        // Hidden text leaves must not acquire visibility from their ancestor.
        session.ok(&["eval", r##"document.querySelector('#account-b').innerHTML=`<button onclick="document.querySelector('#receipt').textContent='wrong'">Other<span hidden>Save</span></button>`;document.querySelector('#receipt').textContent='untouched'"##]);
        session.refused(
            &[
                "find", "text", "Save", "click", "--exact", "--within", &scope,
            ],
            "No element found",
        );
        assert_eq!(
            session.ok(&["get", "text", "#receipt"])["text"],
            "untouched"
        );
        session.ok(&["eval", r##"document.querySelector('#account-b').insertAdjacentHTML('beforeend',`<button onclick="document.querySelector('#receipt').textContent='visible'">Save</button>`)"##]);
        session.ok(&[
            "find", "text", "Save", "click", "--exact", "--within", &scope,
        ]);
        assert_eq!(session.ok(&["get", "text", "#receipt"])["text"], "visible");
        // Diagnostics must not print editable contents, including custom fields.
        session.ok(&["eval", r##"document.querySelector('#account-b').innerHTML=`<div data-testid="dup" contenteditable>PRIVATE-EDITABLE</div><div data-testid="dup" role="textbox">PRIVATE-CUSTOM</div>`"##]);
        let out = session.run(&["find", "testid", "dup", "--within", &scope]);
        assert!(!out.status.success());
        let diagnostic = String::from_utf8_lossy(&out.stdout);
        assert!(
            !diagnostic.contains("PRIVATE-EDITABLE") && !diagnostic.contains("PRIVATE-CUSTOM"),
            "{diagnostic}"
        );
        session.ok(&["eval", r##"document.querySelector('#account-b').innerHTML=`<div data-testid="dup"><div contenteditable>PRIVATE-NESTED-EDITABLE</div></div><div data-testid="dup"><div role="textbox">PRIVATE-NESTED-CUSTOM</div><textarea>PRIVATE-NESTED-TEXTAREA</textarea></div>`"##]);
        let out = session.run(&["find", "testid", "dup", "--within", &scope]);
        assert!(!out.status.success());
        let diagnostic = String::from_utf8_lossy(&out.stdout);
        assert!(!diagnostic.contains("PRIVATE-NESTED"), "{diagnostic}");
        session.ok(&["eval", r##"document.querySelector('#account-b').innerHTML=`<div data-testid="unique"><span>Editable container</span><div contenteditable>PRIVATE-NESTED-EDITABLE</div><textarea>PRIVATE-NESTED-TEXTAREA</textarea></div>`"##]);
        let safe = session.ok(&["find", "testid", "unique", "--within", &scope]);
        assert_eq!(safe["located"]["name"], "Editable container");
        assert!(!safe.to_string().contains("PRIVATE-NESTED"));
        let safe_mcp=session.mcp_find(serde_json::json!({"locator":"testid","value":"unique","within":scope,"session":session.name}));
        assert_eq!(
            safe_mcp["structuredContent"]["response"]["data"]["located"]["name"],
            "Editable container"
        );
        assert!(!safe_mcp.to_string().contains("PRIVATE-NESTED"));
        // Marker observers run between discovery and dispatch. They may move or
        // replace the selected node, but cannot redirect its pinned identity.
        for mutation in [
            "document.body.appendChild(target)",
            "target.replaceWith(target.cloneNode(true))",
            "target.parentElement.appendChild(target.cloneNode(true))",
        ] {
            session.ok(&["open", &format!("file://{}", page.display())]);
            let script=format!("window.observer=new MutationObserver(ms=>{{const target=document.querySelector('#account-b [data-chrome-use-located]');if(target){{window.observer.disconnect();{mutation};}}}});window.observer.observe(document.querySelector('#account-b'),{{attributes:true,subtree:true}})");
            session.ok(&["eval", &script]);
            let out = session.run(&[
                "find", "role", "button", "click", "--name", "Save", "--exact", "--within", &scope,
            ]);
            assert!(
                !out.status.success(),
                "moved or cloned target dispatched: {}",
                String::from_utf8_lossy(&out.stdout)
            );
            assert_eq!(
                session.ok(&["get", "text", "#receipt"])["text"],
                "untouched"
            );
        }
        // An outside clone is unrelated. The original target remains pinned.
        session.ok(&["open", &format!("file://{}", page.display())]);
        session.ok(&["eval", "window.observer=new MutationObserver(()=>{const target=document.querySelector('#account-b [data-chrome-use-located]');if(target){window.observer.disconnect();document.body.prepend(target.cloneNode(true));}});window.observer.observe(document.querySelector('#account-b'),{attributes:true,subtree:true})"]);
        session.ok(&[
            "find", "role", "button", "click", "--name", "Save", "--exact", "--within", &scope,
        ]);
        assert_eq!(session.ok(&["get", "text", "#receipt"])["text"], "beta");
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
