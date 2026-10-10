//! `extract` field specs (#504) end to end: the real CLI and the stdio MCP
//! server, each driving its own daemon and a launched headless Chrome
//! (`AGENT_BROWSER_ALLOW_HEADLESS=1`, `AGENT_BROWSER_EXECUTABLE_PATH`).
//! Covered: a root attribute with no `rows` (`"@lang"` on `<html lang>`), an
//! attribute of the row element itself (`"@href"` on rows of links), and a
//! schema extract does not understand, which must be refused.
#![cfg(unix)]

#[cfg(feature = "e2e-tests")]
mod browser {
    use serde_json::{json, Value};
    use std::io::Write;
    use std::process::{Command, Output, Stdio};

    const BIN: &str = env!("CARGO_BIN_EXE_chrome-use");

    const PAGE: &str = r#"<!doctype html><html lang="en"><head><title>extract</title></head>
<body><ul>
<li><a class="item" href="/one">One</a></li>
<li><a class="item" href="https://example.test/two">Two</a></li>
</ul></body></html>"#;

    struct Session {
        home: tempfile::TempDir,
        sock: tempfile::TempDir,
        name: String,
        page: String,
    }

    impl Session {
        fn new(tag: &str) -> Self {
            let home = tempfile::tempdir().unwrap();
            let file = home.path().join("extract.html");
            std::fs::write(&file, PAGE).unwrap();
            Self {
                page: format!("file://{}", file.display()),
                home,
                sock: tempfile::Builder::new()
                    .prefix("cux")
                    .tempdir_in("/tmp")
                    .unwrap(),
                name: format!("extract-{tag}-{}", std::process::id()),
            }
        }

        fn command(&self) -> Command {
            let mut c = Command::new(BIN);
            c.env("HOME", self.home.path())
                .env("USERPROFILE", self.home.path())
                .env("AGENT_BROWSER_SOCKET_DIR", self.sock.path())
                .env("AGENT_BROWSER_ALLOW_HEADLESS", "1")
                .env_remove("AGENT_BROWSER_CDP")
                .env_remove("AGENT_BROWSER_PROVIDER")
                .env("NO_COLOR", "1");
            c
        }

        fn run(&self, args: &[&str]) -> Output {
            self.command()
                .args(["--session", &self.name, "--json", "--launch"])
                .args(args)
                .output()
                .unwrap()
        }

        fn json(&self, args: &[&str]) -> Value {
            let out = self.run(args);
            serde_json::from_slice(&out.stdout).unwrap_or_else(|e| {
                panic!(
                    "{e}: {} {}",
                    String::from_utf8_lossy(&out.stdout),
                    String::from_utf8_lossy(&out.stderr)
                )
            })
        }

        /// One `tools/call` of `chrome_use_extract` over a fresh stdio MCP
        /// server; returns the tool result.
        fn mcp_extract(&self, schema: Value) -> Value {
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
                json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-03-26","capabilities":{},"clientInfo":{"name":"extract-regression","version":"1"}}}),
                json!({"jsonrpc":"2.0","method":"notifications/initialized"}),
                json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"chrome_use_extract","arguments":{"schema": schema, "session": self.name}}}),
            ] {
                writeln!(input, "{request}").unwrap();
            }
            drop(input);
            let output = child.wait_with_output().unwrap();
            let rows: Vec<Value> = String::from_utf8_lossy(&output.stdout)
                .lines()
                .filter_map(|s| serde_json::from_str(s).ok())
                .collect();
            rows.iter()
                .find(|r| r["id"] == 2)
                .unwrap_or_else(|| panic!("no tools/call reply: {rows:?}"))["result"]
                .clone()
        }
    }

    impl Drop for Session {
        fn drop(&mut self) {
            let _ = self.run(&["close"]);
        }
    }

    fn cases() -> (Value, Value, Value) {
        (
            json!({"fields": {"lang": "@lang", "title": "title"}}),
            json!({"rows": "a.item", "fields": {"text": "", "href": "@href"}}),
            json!({"rows": "a.item", "fields": {"href": {"selector": "a"}}}),
        )
    }

    #[test]
    #[ignore = "isolated real Chrome and daemon"]
    fn e2e_extract_attr_shorthand_and_refusal_through_the_cli() {
        let s = Session::new("cli");
        let opened = s.json(&["open", &s.page]);
        assert_eq!(opened["success"], true, "{opened}");
        let (root, rows, bad) = cases();

        let r = s.json(&["extract", "--schema", &root.to_string()]);
        assert_eq!(r["success"], true, "{r}");
        assert_eq!(r["data"]["extracted"][0]["lang"], "en", "{r}");
        assert!(r["data"].get("warning").is_none(), "{r}");

        let r = s.json(&["extract", "--schema", &rows.to_string()]);
        assert_eq!(r["success"], true, "{r}");
        assert_eq!(r["data"]["extracted"][0]["href"], "/one", "{r}");
        assert_eq!(
            r["data"]["extracted"][1]["href"], "https://example.test/two",
            "{r}"
        );

        let out = s.run(&["extract", "--schema", &bad.to_string()]);
        assert!(!out.status.success());
        let text = String::from_utf8_lossy(&out.stdout);
        assert!(text.contains("unknown key \\\"selector\\\""), "{text}");
    }

    #[test]
    #[ignore = "isolated real Chrome and daemon"]
    fn e2e_extract_attr_shorthand_and_refusal_through_stdio_mcp() {
        let s = Session::new("mcp");
        let opened = s.json(&["open", &s.page]);
        assert_eq!(opened["success"], true, "{opened}");
        let (root, rows, bad) = cases();

        let r = s.mcp_extract(root);
        assert_eq!(r["isError"], false, "{r}");
        let data = &r["structuredContent"]["response"]["data"];
        assert_eq!(data["extracted"][0]["lang"], "en", "{r}");

        let r = s.mcp_extract(rows);
        assert_eq!(r["isError"], false, "{r}");
        let data = &r["structuredContent"]["response"]["data"];
        assert_eq!(data["extracted"][0]["href"], "/one", "{r}");

        let r = s.mcp_extract(bad);
        assert_eq!(r["isError"], true, "{r}");
        assert!(r.to_string().contains("unknown key"), "{r}");
    }
}
