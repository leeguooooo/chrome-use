//! `session handoff --wait` end to end with a launched headless Chrome
//! (`AGENT_BROWSER_ALLOW_HEADLESS=1`). The "user" is a timer in the page —
//! the agent can't drive a handed-off page, so the page changes itself, the
//! way it would under the human's hands.
//!
//! Pinned down:
//! - `--until-url` / `--until-text` return once the page gets there, give the
//!   session back (exit 0) and report URL/title before → after;
//! - `session done` from another process ends a bare `--wait`;
//! - `--until-change` and a timeout end the wait but leave the session with
//!   the user (exit 2), and driving commands stay refused.
#![cfg(unix)]

#[cfg(feature = "e2e-tests")]
mod browser {
    use serde_json::Value;
    use std::process::{Command, Output};
    use std::time::Duration;

    const BIN: &str = env!("CARGO_BIN_EXE_chrome-use");

    /// Page A: after `delay` ms, writes "Verified" and goes to page B.
    fn page_a(delay: u64) -> String {
        format!(
            r#"<!doctype html><html><head><title>Login</title></head><body>
<p id="captcha">solve me</p>
<script>
setTimeout(() => {{
  document.getElementById('captcha').remove();
  document.body.insertAdjacentHTML('beforeend', '<p>Verified</p>');
}}, {delay});
setTimeout(() => {{ location.href = location.href.replace('a.html', 'b.html'); }}, {delay} + 1500);
</script></body></html>"#
        )
    }

    const PAGE_B: &str = r#"<!doctype html><html><head><title>Dashboard</title></head>
<body><h1>Dashboard</h1><button>Continue</button></body></html>"#;

    struct Session {
        home: tempfile::TempDir,
        sock: tempfile::TempDir,
        name: String,
        page: String,
    }

    impl Session {
        fn new(tag: &str, delay: u64) -> Self {
            let home = tempfile::tempdir().unwrap();
            std::fs::write(home.path().join("a.html"), page_a(delay)).unwrap();
            std::fs::write(home.path().join("b.html"), PAGE_B).unwrap();
            let page = format!("file://{}/a.html", home.path().display());
            let s = Self {
                page,
                home,
                sock: tempfile::Builder::new()
                    .prefix("cuhw")
                    .tempdir_in("/tmp")
                    .unwrap(),
                name: format!("handoff-{tag}-{}", std::process::id()),
            };
            let out = s.run(&["--launch", "open", &s.page.clone()]);
            assert!(out.status.success(), "open: {}", text(&out));
            s
        }

        fn command(&self) -> Command {
            let mut c = Command::new(BIN);
            c.env("HOME", self.home.path())
                .env("USERPROFILE", self.home.path())
                .env("AGENT_BROWSER_SOCKET_DIR", self.sock.path())
                .env("AGENT_BROWSER_ALLOW_HEADLESS", "1")
                .env_remove("AGENT_BROWSER_CDP")
                .env_remove("AGENT_BROWSER_PROVIDER")
                .env_remove("AGENT_BROWSER_SESSION")
                .env("NO_COLOR", "1");
            c.args(["--session", &self.name]);
            c
        }

        fn run(&self, args: &[&str]) -> Output {
            self.command().args(args).output().unwrap()
        }

        fn handoff(&self, args: &[&str]) -> (i32, Value) {
            let out = self
                .command()
                .args(["--json", "session", "handoff"])
                .args(args)
                .output()
                .unwrap();
            let v: Value = serde_json::from_slice(&out.stdout)
                .unwrap_or_else(|e| panic!("{e}: {}", text(&out)));
            (out.status.code().unwrap_or(-1), v)
        }
    }

    impl Drop for Session {
        fn drop(&mut self) {
            let _ = self.run(&["session", "resume"]);
            let _ = self.run(&["close"]);
        }
    }

    fn text(out: &Output) -> String {
        format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        )
    }

    #[test]
    fn until_url_resumes_and_reports_the_change() {
        let s = Session::new("url", 1000);
        let (code, v) = s.handoff(&["--until-url", "**/b.html", "--timeout", "60s"]);
        assert_eq!(code, 0, "{v}");
        assert_eq!(v["outcome"], "condition", "{v}");
        assert_eq!(v["owner"], "agent", "{v}");
        let c = &v["changed"];
        assert!(c["urlBefore"].as_str().unwrap().ends_with("a.html"), "{v}");
        assert!(c["url"].as_str().unwrap().ends_with("b.html"), "{v}");
        assert_eq!(c["titleBefore"], "Login", "{v}");
        assert_eq!(c["title"], "Dashboard", "{v}");
        assert_eq!(c["navigated"], true, "{v}");
        assert!(
            c["snapshot"].as_str().unwrap_or("").contains("Continue"),
            "{v}"
        );
        // The agent drives again.
        let out = s.run(&["get", "title"]);
        assert!(out.status.success(), "{}", text(&out));
    }

    #[test]
    fn until_text_and_until_gone_see_in_page_changes() {
        let s = Session::new("text", 1000);
        let (code, v) = s.handoff(&["--until-text", "Verified", "--timeout", "60s"]);
        assert_eq!((code, v["outcome"].as_str()), (0, Some("condition")), "{v}");

        let s = Session::new("gone", 1000);
        let (code, v) = s.handoff(&["--until-gone", "#captcha", "--timeout", "60s"]);
        assert_eq!((code, v["outcome"].as_str()), (0, Some("condition")), "{v}");
    }

    #[test]
    fn session_done_ends_a_bare_wait() {
        let s = Session::new("done", 600_000);
        let mut child = s
            .command()
            .args(["--json", "session", "handoff", "--wait", "--timeout", "60s"])
            .stdout(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        std::thread::sleep(Duration::from_secs(3));
        // Still waiting, and the agent is locked out meanwhile.
        assert!(child.try_wait().unwrap().is_none(), "returned early");
        let out = s.run(&["click", "body"]);
        assert!(!out.status.success(), "drove a handed-off page");
        let out = s.run(&["session", "done"]);
        assert!(out.status.success(), "{}", text(&out));
        let out = child.wait_with_output().unwrap();
        let v: Value = serde_json::from_slice(&out.stdout).unwrap();
        assert_eq!(out.status.code(), Some(0), "{v}");
        assert_eq!(v["outcome"], "user_done", "{v}");
        assert_eq!(v["changed"]["navigated"], false, "{v}");
    }

    #[test]
    fn change_and_timeout_leave_the_session_with_the_user() {
        let s = Session::new("change", 500);
        let (code, v) = s.handoff(&["--until-change", "--settle", "1s", "--timeout", "60s"]);
        assert_eq!(code, 2, "{v}");
        assert_eq!(v["outcome"], "changed", "{v}");
        assert_eq!(v["owner"], "user", "{v}");
        assert!(
            v["changed"]["url"].as_str().unwrap().ends_with("b.html"),
            "{v}"
        );
        let out = s.run(&["click", "body"]);
        assert!(!out.status.success(), "drove a handed-off page");

        // Re-waiting keeps the original baseline; a timeout keeps the handoff.
        let (code, v) = s.handoff(&["--wait", "--timeout", "2s"]);
        assert_eq!(code, 2, "{v}");
        assert_eq!(v["outcome"], "timeout", "{v}");
        assert!(
            v["changed"]["urlBefore"]
                .as_str()
                .unwrap()
                .ends_with("a.html"),
            "{v}"
        );
        let out = s.run(&["click", "body"]);
        assert!(!out.status.success(), "drove a handed-off page");
    }
}
