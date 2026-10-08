//! Per-step flags in `batch`, through the real binary: what each step
//! actually sends to the daemon (a stub on a temp socket; no browser).
#![cfg(unix)]
mod common;
use common::{text, Stub};
use serde_json::{json, Value};

fn step(stub: &Stub, outer: &[&str], step: &str, action: &str) -> Value {
    stub.clear();
    let mut args: Vec<&str> = outer.to_vec();
    args.push("batch");
    args.push(step);
    stub.ok(&args);
    stub.sent(action)
        .pop()
        .unwrap_or_else(|| panic!("{outer:?} {step:?}: no {action} reached the daemon"))
}

/// A step's `--observe` reaches the daemon; one without it inherits the
/// batch's flag; an explicit `false` in a step overrides the batch's `true`.
#[test]
fn step_observe_and_if_present_override_the_batch_both_ways() {
    let stub = Stub::start("bflags");
    let c = step(&stub, &[], "click #x --observe", "click");
    assert_eq!(c["observe"], json!(true), "{c}");
    let c = step(&stub, &[], "click #x", "click");
    assert!(c.get("observe").is_none(), "{c}");
    // Inherit.
    let c = step(&stub, &["--observe"], "click #x", "click");
    assert_eq!(c["observe"], json!(true), "{c}");
    let c = step(&stub, &["--if-present"], "click #missing", "click");
    assert_eq!(c["ifPresent"], json!(true), "{c}");
    // Explicit false wins over the batch's true.
    let c = step(&stub, &["--observe"], "click #x --observe false", "click");
    assert_eq!(c["observe"], json!(false), "{c}");
    let c = step(
        &stub,
        &["--if-present"],
        "click #missing --if-present false",
        "click",
    );
    assert_eq!(
        c["ifPresent"],
        json!(false),
        "a missing element must fail, not skip: {c}"
    );
}

/// A step's `--tab` may replace the tab the batch inherited, never the
/// command's own target; naming two different tabs is refused before anything
/// is sent.
#[test]
fn a_step_tab_never_replaces_the_commands_own_target() {
    let stub = Stub::start("btabs");
    for (cmd, action) in [
        ("tab close t1", "tab_close"),
        ("tab select t1", "tab_switch"),
        ("tab inspect t1", "tab_inspect"),
    ] {
        // Same tab twice is fine.
        let c = step(&stub, &[], &format!("{cmd} --tab t1"), action);
        assert_eq!(c["tabId"], json!("t1"), "{cmd}: {c}");
        // The batch's inherited --tab does not override the command's own.
        let c = step(&stub, &["--tab", "t2"], cmd, action);
        assert_eq!(c["tabId"], json!("t1"), "{cmd}: {c}");
        // A conflicting step --tab is refused, and nothing is sent.
        stub.clear();
        let out = stub.run(&["batch", &format!("{cmd} --tab t2")]);
        assert!(!out.status.success(), "{cmd}: {}", text(&out));
        assert!(text(&out).contains("drop one of them"), "{}", text(&out));
        assert!(
            stub.sent(action).is_empty(),
            "{cmd}: a refused step was sent"
        );
    }
    // Without its own target, a step --tab replaces the inherited one.
    let c = step(&stub, &["--tab", "t2"], "click #x --tab t3", "click");
    assert_eq!(c["tabId"], json!("t3"), "{c}");
    let c = step(&stub, &["--tab", "t2"], "click #x", "click");
    assert_eq!(c["tabId"], json!("t2"), "{c}");
}
