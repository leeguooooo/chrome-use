//! `screenshot` argument placement, through the real binary.
//!
//! `screenshot <path> --selector <sel>` used to send the path as the selector
//! and fail with "Element not found: <path>". The parse is unit-tested in
//! `commands.rs`; this checks the command the CLI actually puts on the wire,
//! against a stub daemon on a temp socket, so no browser is involved.
#![cfg(unix)]
mod common;
use common::{text, Stub};

#[test]
fn screenshot_path_and_selector_reach_the_daemon_in_any_order() {
    let stub = Stub::start("shot");
    let cases: [(&[&str], &str, &str); 7] = [
        (
            &["screenshot", "/tmp/cu-shot/out.png", "--selector", "main"],
            "main",
            "/tmp/cu-shot/out.png",
        ),
        (
            &["screenshot", "--selector", "main", "/tmp/cu-shot/out.png"],
            "main",
            "/tmp/cu-shot/out.png",
        ),
        (
            &["screenshot", "main", "/tmp/cu-shot/out.png"],
            "main",
            "/tmp/cu-shot/out.png",
        ),
        (
            &["screenshot", "/tmp/cu-shot/out.png", "main"],
            "main",
            "/tmp/cu-shot/out.png",
        ),
        // XPath starts with `/` too: selector-first, as before.
        (&["screenshot", "//main", "shot"], "//main", "shot"),
        (&["screenshot", "/html/body", "shot"], "/html/body", "shot"),
        // An explicit --selector beats every positional guess.
        (
            &["screenshot", "shot", "--selector", "//main"],
            "//main",
            "shot",
        ),
    ];
    for (args, selector, path) in cases {
        stub.clear();
        stub.ok(args);
        let sent = stub.sent("screenshot");
        let cmd = sent
            .last()
            .unwrap_or_else(|| panic!("{args:?}: no screenshot command reached the daemon"));
        assert_eq!(cmd["selector"], selector, "{args:?}: {cmd}");
        assert_eq!(cmd["path"], path, "{args:?}: {cmd}");
    }
}

#[test]
fn an_unknown_screenshot_option_is_refused_before_the_daemon() {
    let stub = Stub::start("shot2");
    let out = stub.run(&["screenshot", "out.png", "--sel", "main"]);
    assert!(!out.status.success());
    assert!(text(&out).contains("--sel"), "{}", text(&out));
    assert!(
        stub.sent("screenshot").is_empty(),
        "a refused command must not be sent"
    );
}
