//! `read <url> --links` (#503) through the real CLI and the stdio MCP server,
//! against a local HTTP server. `read <url>` is a plain HTTP fetch, so no
//! browser runs. The pages are built to hit the budgets: a 1 MB `<base>`
//! with 10,000 relative links, and 1,000 links of ~1.9 KB each.
#![cfg(unix)]
use serde_json::{json, Value};
use std::io::{Read, Write};
use std::process::{Command, Output, Stdio};

const BIN: &str = env!("CARGO_BIN_EXE_chrome-use");

/// Serve `pages` (path → html) until the test process ends.
fn serve(pages: Vec<(&'static str, String)>) -> u16 {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let pages = pages.clone();
            std::thread::spawn(move || {
                let mut stream = stream;
                let mut buf = [0u8; 4096];
                let n = stream.read(&mut buf).unwrap_or(0);
                let head = String::from_utf8_lossy(&buf[..n]);
                let path = head.split_whitespace().nth(1).unwrap_or("/").to_string();
                let body = pages
                    .iter()
                    .find(|(p, _)| *p == path)
                    .map(|(_, b)| b.clone())
                    .unwrap_or_default();
                let _ = stream.write_all(
                    format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\n\
                         Connection: close\r\n\r\n",
                        body.len()
                    )
                    .as_bytes(),
                );
                let _ = stream.write_all(body.as_bytes());
            });
        }
    });
    port
}

fn long_base_page() -> String {
    let mut html = format!(
        r#"<!doctype html><html><head><base href="http://127.0.0.1/{}/"></head><body>"#,
        "b".repeat(1024 * 1024)
    );
    for i in 0..10_000 {
        html.push_str(&format!(r#"<a href="r{i}">{i}</a>"#));
    }
    html.push_str("</body></html>");
    html
}

fn wide_links_page() -> String {
    let mut html = String::from("<!doctype html><html><body>");
    for i in 0..1000 {
        html.push_str(&format!(
            r#"<a href="https://h.example/{i}/{}">x</a>"#,
            "q".repeat(1900)
        ));
    }
    html.push_str("</body></html>");
    html
}

struct Env {
    home: tempfile::TempDir,
    sock: tempfile::TempDir,
    session: String,
}

impl Env {
    fn new(tag: &str) -> Self {
        Env {
            home: tempfile::tempdir().unwrap(),
            sock: tempfile::Builder::new()
                .prefix("cur")
                .tempdir_in("/tmp")
                .unwrap(),
            session: format!("read-links-{tag}-{}", std::process::id()),
        }
    }

    fn command(&self) -> Command {
        let mut c = Command::new(BIN);
        c.env("HOME", self.home.path())
            .env("AGENT_BROWSER_SOCKET_DIR", self.sock.path())
            .env_remove("AGENT_BROWSER_CDP")
            .env_remove("AGENT_BROWSER_AUTO_CONNECT")
            // `read <url>` is a plain HTTP fetch: no browser connection, and
            // never the extension-install page opened on the host's screen.
            .env("AGENT_BROWSER_NO_AUTO_CONNECT", "1")
            .env("AGENT_BROWSER_NO_AUTO_OPEN", "1")
            .env_remove("AGENT_BROWSER_PROVIDER")
            .env("NO_COLOR", "1");
        c
    }

    fn read(&self, args: &[&str]) -> (Value, Output) {
        let out = self
            .command()
            .args(["--session", &self.session, "--json", "read"])
            .args(args)
            .output()
            .unwrap();
        let v = serde_json::from_slice(&out.stdout).unwrap_or_else(|e| {
            panic!(
                "{e}: {} {}",
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            )
        });
        (v, out)
    }

    fn mcp_read(&self, arguments: Value) -> Value {
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
            json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-03-26","capabilities":{},"clientInfo":{"name":"read-links","version":"1"}}}),
            json!({"jsonrpc":"2.0","method":"notifications/initialized"}),
            json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"chrome_use_read","arguments":arguments}}),
        ] {
            writeln!(input, "{request}").unwrap();
        }
        drop(input);
        let output = child.wait_with_output().unwrap();
        let rows: Vec<Value> = String::from_utf8_lossy(&output.stdout)
            .lines()
            .filter_map(|s| serde_json::from_str(s).ok())
            .collect();
        rows.into_iter()
            .find(|r| r["id"] == 2)
            .expect("tools/call reply")
    }
}

impl Drop for Env {
    fn drop(&mut self) {
        let _ = self
            .command()
            .args(["--session", &self.session, "close"])
            .output();
    }
}

#[test]
fn read_links_omits_overlong_urls_and_bounds_the_output_through_the_cli() {
    let port = serve(vec![
        ("/long-base", long_base_page()),
        ("/wide", wide_links_page()),
    ]);
    let env = Env::new("cli");

    let started = std::time::Instant::now();
    let (v, out) = env.read(&[&format!("http://127.0.0.1:{port}/long-base"), "--links"]);
    assert!(out.status.success(), "{v}");
    assert!(started.elapsed() < std::time::Duration::from_secs(30));
    let d = &v["data"];
    assert_eq!(d["linksOmittedTooLong"], 10_000, "{}", d["linksBudgetsHit"]);
    assert_eq!(d["linksShown"], 0);
    assert_eq!(d["links"], json!([]));
    let content = d["content"].as_str().unwrap();
    assert!(content.contains("10000 links omitted: their URL is longer than 2048 bytes"));
    // Nothing near the 1 MB base made it into the reply.
    assert!(
        out.stdout.len() < 2 * 1024 * 1024 + 64 * 1024,
        "{}",
        out.stdout.len()
    );
    assert!(!content.contains(&"b".repeat(4096)));

    let (v, out) = env.read(&[
        &format!("http://127.0.0.1:{port}/wide"),
        "--max-links",
        "1000",
    ]);
    assert!(out.status.success(), "{v}");
    let d = &v["data"];
    assert_eq!(d["linksTotal"], 1000);
    assert_eq!(d["linksTotalExact"], true);
    assert!(d["linksShown"].as_u64().unwrap() < 1000);
    assert!(d["linksBudgetsHit"].to_string().contains("output"));
    let listed: usize = d["links"]
        .as_array()
        .unwrap()
        .iter()
        .map(|l| l["url"].as_str().unwrap().len() + l["text"].as_str().unwrap().len() + 8)
        .sum();
    assert!(listed <= 256 * 1024, "{listed}");
}

#[test]
fn read_links_budgets_and_argument_checks_through_stdio_mcp() {
    let port = serve(vec![
        ("/long-base", long_base_page()),
        ("/wide", wide_links_page()),
    ]);
    let env = Env::new("mcp");

    let r = env.mcp_read(json!({
        "url": format!("http://127.0.0.1:{port}/long-base"), "links": true, "session": env.session
    }));
    assert_eq!(r["result"]["isError"], false, "{r}");
    let d = &r["result"]["structuredContent"]["response"]["data"];
    assert_eq!(d["linksOmittedTooLong"], 10_000, "{r}");
    assert_eq!(d["linksShown"], 0);

    let r = env.mcp_read(json!({
        "url": format!("http://127.0.0.1:{port}/wide"), "maxLinks": 1000, "session": env.session
    }));
    assert_eq!(r["result"]["isError"], false, "{r}");
    let d = &r["result"]["structuredContent"]["response"]["data"];
    assert!(d["linksBudgetsHit"].to_string().contains("output"), "{d}");

    // A wrongly typed `links` is refused even next to `maxLinks`.
    let r = env.mcp_read(json!({
        "url": format!("http://127.0.0.1:{port}/wide"), "maxLinks": 5, "links": "bad"
    }));
    assert!(r.to_string().contains("links must be a boolean"), "{r}");
}
