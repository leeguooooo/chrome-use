//! Script policy flow over a synthetic CDP relay; no browser or clipboard.
use super::*;
use crate::native::browser::{BrowserManager, PageInfo};
use crate::native::policy::ActionPolicy;
use futures_util::{SinkExt, StreamExt};
use std::sync::{Arc, Mutex};
use tokio_tungstenite::tungstenite::Message;

struct Mode;
impl Drop for Mode {
    fn drop(&mut self) {
        crate::native::browser::relay_for_test(false);
    }
}
async fn fixture() -> (
    DaemonState,
    tokio::task::JoinHandle<()>,
    Arc<Mutex<Vec<String>>>,
    tempfile::TempDir,
    Mode,
) {
    fixture_with_policy(
        json!({"confirm":["clipboard"]}),
        "https://fixture.test/editor",
    )
    .await
}

async fn fixture_with_policy(
    managed: Value,
    frame_url: &str,
) -> (
    DaemonState,
    tokio::task::JoinHandle<()>,
    Arc<Mutex<Vec<String>>>,
    tempfile::TempDir,
    Mode,
) {
    fixture_with_status(managed, frame_url, Some("decline")).await
}

async fn fixture_with_status(
    managed: Value,
    frame_url: &str,
    status: Option<&str>,
) -> (
    DaemonState,
    tokio::task::JoinHandle<()>,
    Arc<Mutex<Vec<String>>>,
    tempfile::TempDir,
    Mode,
) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("ws://{}", listener.local_addr().unwrap());
    let calls = Arc::new(Mutex::new(Vec::new()));
    let seen = calls.clone();
    let frame_url = frame_url.to_string();
    let status = status.map(str::to_string);
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut socket = tokio_tungstenite::accept_async(stream).await.unwrap();
        while let Some(Ok(Message::Text(raw))) = socket.next().await {
            let request: Value = serde_json::from_str(&raw).unwrap();
            let method = request["method"].as_str().unwrap();
            let mut error = None;
            let result = match method {
                "ABRelay.getCapabilities" => json!({"capabilities":["sitePolicy"]}),
                "ABExt.sitePolicy" => {
                    json!({"version":1,"user":{"ask":[]},"managed":{"actionPolicy":managed}})
                }
                "ABExt.siteApproval" if request["params"]["op"] == "status" => match &status {
                    Some(decision) => json!({"known":true,"decision":decision}),
                    None => {
                        error = Some("synthetic approval status transport failure");
                        json!({})
                    }
                },
                "ABExt.siteApproval" => json!({"held":true}),
                "Browser.getVersion" => json!({"product":"Chrome/156.0.0.0"}),
                "Page.getFrameTree" => {
                    json!({"frameTree":{"frame":{"id":"main","url":frame_url,"securityOrigin":url::Url::parse(&frame_url).unwrap().origin().ascii_serialization()}}})
                }
                "Page.createIsolatedWorld" => json!({"executionContextId":42}),
                "Runtime.evaluate" => {
                    let expression = request["params"]["expression"].as_str().unwrap_or("");
                    if expression.contains("_marker") {
                        seen.lock().unwrap().push(expression.to_string());
                    }
                    if expression == "ordinary_failure" {
                        error = Some("synthetic ordinary failure");
                    } else if expression == "missing_target" {
                        error = Some("no element; nothing was dispatched");
                    }
                    json!({"result":{"type":"string","value":"synthetic receipt"}})
                }
                _ => json!({}),
            };
            let mut response = if let Some(error) = error {
                json!({"id":request["id"],"error":{"code":-32000,"message":error}})
            } else {
                json!({"id":request["id"],"result":result})
            };
            if let Some(session) = request.get("sessionId") {
                response["sessionId"] = session.clone();
            }
            if socket
                .send(Message::Text(response.to_string()))
                .await
                .is_err()
            {
                break;
            }
        }
    });
    crate::native::browser::relay_for_test(true);
    let mut browser = BrowserManager::for_test(
        &endpoint,
        vec![PageInfo {
            tab_id: 1,
            label: None,
            target_id: "fixture".into(),
            session_id: "cb-tab-1".into(),
            url: "https://fixture.test/editor".into(),
            title: "Synthetic".into(),
            target_type: "page".into(),
        }],
    )
    .await;
    browser.reown_target("fixture").unwrap();
    let root = tempfile::tempdir().unwrap();
    let policy = root.path().join("policy.json");
    std::fs::write(&policy, r#"{"confirm":["fill"]}"#).unwrap();
    let mut state = DaemonState::new();
    state.browser = Some(browser);
    state.policy = Some(ActionPolicy::load(policy.to_str().unwrap()).unwrap());
    state.confirm_actions = None;
    (state, server, calls, root, Mode)
}

#[tokio::test]
async fn json_pending_approval_is_not_optional_or_a_fallback_success() {
    for auto in [true, false] {
        let (mut state, server, calls, _root, _mode) = fixture().await;
        let result = handle_script(&json!({"autoConfirm":auto,"program":[
            {"do":"evaluate","script":"before_marker"},
            {"do":"fill","selector":"#editor","value":"replacement","allowClipboardFallback":true,"optional":true,"or":[{"do":"evaluate","script":"fallback_marker"}]},
            {"do":"evaluate","script":"publish_marker"}
        ]}),&mut state).await.unwrap();
        assert_eq!(
            result["confirmation_required"], true,
            "auto={auto}: {result}"
        );
        assert_eq!(result["ok"], false, "{result}");
        assert!(result["confirmation_id"].is_string(), "{result}");
        assert!(state.pending_confirmation.is_some());
        assert_eq!(*calls.lock().unwrap(), ["before_marker"], "{result}");
        assert_eq!(result["steps"][0]["ok"], true, "{result}");
        server.abort();
    }
}

#[tokio::test]
async fn javascript_catch_or_raw_bridge_cannot_run_past_pending_approval() {
    for named in [false, true] {
        let (mut state, server, calls, _root, _mode) = fixture().await;
        let mut cmd = json!({"source":"cu.eval('before_marker'); try { cu.fill('#editor','replacement'); } catch (e) {} try { cu.eval('publish_marker'); } catch (e) {} __cu('evaluate', JSON.stringify({script:'raw_publish_marker'})); 'finished';"});
        if named {
            cmd["keepContext"] = json!("approval-fixture");
        }
        let result = handle_script(&cmd, &mut state).await.unwrap();
        assert_eq!(
            result["confirmation_required"], true,
            "named={named}: {result}"
        );
        assert_eq!(result["ok"], false, "{result}");
        assert_eq!(*calls.lock().unwrap(), ["before_marker"], "{result}");
        if named {
            let next = handle_script(
                &json!({"inContext":"approval-fixture","source":"cu.eval('next_marker')"}),
                &mut state,
            )
            .await
            .unwrap();
            assert_eq!(next["ok"], true, "{next}");
            assert_eq!(*calls.lock().unwrap(), ["before_marker", "next_marker"]);
        }
        server.abort();
    }
}

#[tokio::test]
async fn ordinary_javascript_errors_still_allow_explicit_catch_and_followup() {
    let (mut state, server, calls, _root, _mode) = fixture().await;
    let result=handle_script(&json!({"source":"try { cu.eval('ordinary_failure'); } catch (e) {} cu.eval('publish_marker');"}),&mut state).await.unwrap();
    assert_eq!(result["ok"], true, "{result}");
    assert_eq!(*calls.lock().unwrap(), ["publish_marker"]);
    server.abort();
}

#[tokio::test]
async fn nested_json_pending_cannot_be_auto_confirmed_by_the_parent() {
    let (mut state, server, calls, _root, _mode) = fixture().await;
    let result=handle_script(&json!({"autoConfirm":true,"program":[
        {"do":"script","optional":true,"program":[{"do":"fill","selector":"#editor","value":"replacement"}]},
        {"do":"evaluate","script":"publish_marker"}
    ]}),&mut state).await.unwrap();
    assert_eq!(result["confirmation_required"], true, "{result}");
    assert_eq!(result["ok"], false, "{result}");
    assert!(
        state.pending_confirmation.as_ref().unwrap().site.is_none(),
        "the original command gate must not be automatically promoted to a native gate"
    );
    assert!(calls.lock().unwrap().is_empty());
    server.abort();
}

#[tokio::test]
async fn approval_in_json_condition_stops_without_optional_probes_or_publish() {
    let (mut state, server, calls, root, _mode) = fixture().await;
    let path = root.path().join("read-policy.json");
    std::fs::write(&path, r#"{"confirm":["isvisible"]}"#).unwrap();
    state.policy = Some(ActionPolicy::load(path.to_str().unwrap()).unwrap());
    assert_eq!(
        state.policy.as_ref().unwrap().check("isvisible"),
        crate::native::policy::PolicyResult::RequiresConfirmation
    );
    let result=handle_script(&json!({"program":[{"waitUntil":{"exists":"#editor"},"timeoutMs":10,"pollMs":1},{"do":"evaluate","script":"publish_marker"}]}),&mut state).await.unwrap();
    assert_eq!(result["confirmation_required"], true, "{result}");
    assert_eq!(result["ok"], false, "{result}");
    assert!(calls.lock().unwrap().is_empty());
    assert!(state.pending_confirmation.is_some());
    server.abort();
}

#[tokio::test]
async fn native_decline_is_not_an_optional_json_operation_success() {
    let (mut state, server, calls, _root, _mode) = fixture().await;
    state.policy = None;
    let result=handle_script(&json!({"autoConfirm":true,"program":[{"do":"fill","selector":"#editor","value":"replacement","allowClipboardFallback":true,"optional":true},{"do":"evaluate","script":"publish_marker"}]}),&mut state).await.unwrap();
    assert_eq!(result["ok"], false, "{result}");
    assert!(
        result["error"].as_str().unwrap().contains("declined"),
        "{result}"
    );
    assert!(calls.lock().unwrap().is_empty());
    assert!(state.pending_confirmation.is_none());
    server.abort();
}

#[tokio::test]
async fn json_auto_confirmation_refusals_are_not_optional_operation_errors() {
    for (managed, frame_url, reason) in [
        (
            json!({"deny":["clipboard"]}),
            "https://fixture.test/editor",
            "denied",
        ),
        (
            json!({"confirm":["clipboard"]}),
            "https://other.test/editor",
            "target or origin changed",
        ),
    ] {
        let (mut state, server, calls, _root, _mode) =
            fixture_with_policy(managed, frame_url).await;
        let result=handle_script(&json!({"autoConfirm":true,"program":[{"do":"fill","selector":"#editor","value":"replacement","allowClipboardFallback":true,"optional":true,"or":[{"do":"evaluate","script":"fallback_marker"}]},{"do":"evaluate","script":"publish_marker"}]}),&mut state).await.unwrap();
        assert_eq!(result["ok"], false, "{result}");
        assert!(
            result["error"].as_str().unwrap().contains(reason),
            "{result}"
        );
        assert!(calls.lock().unwrap().is_empty(), "{result}");
        server.abort();
    }
}

#[test]
fn approval_refusal_classification_does_not_swallow_ordinary_errors() {
    for reason in [
        "selector not found; nothing was dispatched",
        "ordinary renderer error",
        "action_outcome_unknown: an ordinary input lost its reply",
    ] {
        assert!(approval_refusal(&json!({"success":false,"error":reason})).is_none());
    }
    for reason in [
        "Action 'clipboard' denied by policy: fixture",
        "Action 'clipboard' denied by your organization's chrome-use policy: fixture",
        "Action 'fill' refused: blocked host",
        "Confirmation target or origin changed",
        "Cannot classify the intended field for site policy; observe again",
    ] {
        assert!(
            approval_refusal(&json!({"success":false,"error":reason})).is_some(),
            "{reason}"
        );
    }
}

#[tokio::test]
async fn ordinary_json_errors_keep_optional_and_fallback_semantics() {
    for optional in [false, true] {
        let (mut state, server, calls, _root, _mode) = fixture().await;
        let result=handle_script(&json!({"program":[{"do":"evaluate","script":if optional {"ordinary_failure"} else {"missing_target"},"optional":optional,"or":[{"do":"evaluate","script":"fallback_marker"}]},{"do":"evaluate","script":"publish_marker"}]}),&mut state).await.unwrap();
        assert_eq!(result["ok"], true, "{result}");
        let expected = if optional {
            vec!["publish_marker"]
        } else {
            vec!["fallback_marker", "publish_marker"]
        };
        assert_eq!(*calls.lock().unwrap(), expected, "{result}");
        assert_ne!(result["confirmation_required"], true);
        server.abort();
    }
}

#[tokio::test]
async fn json_loop_does_not_continue_after_held_approval() {
    let (mut state, server, calls, _root, _mode) = fixture().await;
    let result=handle_script(&json!({"args":{"items":[1,2]},"program":[{"forEach":"{{items}}","as":"item","max":2,"body":[{"do":"fill","selector":"#editor","value":"replacement","optional":true}]},{"do":"evaluate","script":"publish_marker"}]}),&mut state).await.unwrap();
    assert_eq!(result["ok"], false, "{result}");
    assert_eq!(result["confirmation_required"], true, "{result}");
    assert!(calls.lock().unwrap().is_empty());
    assert_eq!(result["steps"].as_array().unwrap().len(), 1, "{result}");
    server.abort();
}

#[tokio::test]
async fn fallback_pending_policy_barrier_cannot_be_mistaken_for_a_missing_target() {
    let (mut state, server, calls, root, _mode) = fixture().await;
    let path = root.path().join("fallback-policy.json");
    std::fs::write(&path, r#"{"confirm":["not found"]}"#).unwrap();
    state.policy = Some(ActionPolicy::load(path.to_str().unwrap()).unwrap());
    let result=handle_script(&json!({"program":[{"do":"evaluate","script":"missing_target","or":[{"do":"not found"},{"do":"evaluate","script":"fallback_marker"}]},{"do":"evaluate","script":"publish_marker"}]}),&mut state).await.unwrap();
    assert_eq!(result["confirmation_required"], true, "{result}");
    assert_eq!(result["ok"], false, "{result}");
    assert!(
        calls.lock().unwrap().is_empty(),
        "policy barrier must win over error-word fallback matching: {:?}",
        calls.lock().unwrap()
    );
    server.abort();
}

#[tokio::test]
async fn nested_refusal_policy_barrier_survives_parent_optional_and_javascript_catch() {
    for javascript in [false, true] {
        let (mut state, server, calls, _root, _mode) =
            fixture_with_policy(json!({"deny":["clipboard"]}), "https://fixture.test/editor").await;
        let child = json!({"autoConfirm":true,"program":[{"do":"fill","selector":"#editor","value":"replacement","allowClipboardFallback":true}]});
        let command = if javascript {
            json!({"source":format!("try {{ cu._call('script', {}); }} catch (e) {{}} cu.eval('publish_marker');",child)})
        } else {
            json!({"program":[{"do":"script","optional":true,"autoConfirm":true,"program":child["program"]},{"do":"evaluate","script":"publish_marker"}]})
        };
        let result = handle_script(&command, &mut state).await.unwrap();
        assert_eq!(
            result["approval_refused"], true,
            "javascript={javascript}: {result}"
        );
        assert_eq!(result["ok"], false, "{result}");
        assert!(calls.lock().unwrap().is_empty(), "{result}");
        server.abort();
    }
}

#[tokio::test]
async fn initial_direct_denial_keeps_explicit_optional_and_catch_semantics() {
    for javascript in [false, true] {
        let (mut state, server, calls, root, _mode) = fixture().await;
        let path = root.path().join("direct-denial.json");
        std::fs::write(&path, r#"{"deny":["fill"]}"#).unwrap();
        state.policy = Some(ActionPolicy::load(path.to_str().unwrap()).unwrap());
        let command = if javascript {
            json!({"source":"try { cu.fill('#editor','replacement'); } catch (e) {} cu.eval('publish_marker');"})
        } else {
            json!({"program":[{"do":"fill","selector":"#editor","value":"replacement","optional":true},{"do":"evaluate","script":"publish_marker"}]})
        };
        let result = handle_script(&command, &mut state).await.unwrap();
        assert_eq!(result["ok"], true, "{result}");
        assert_ne!(result["approval_refused"], true, "{result}");
        assert_eq!(*calls.lock().unwrap(), ["publish_marker"]);
        server.abort();
    }
}

#[tokio::test]
async fn confirmed_parent_script_preserves_a_nested_typed_refusal() {
    let (mut state, server, calls, root, _mode) = fixture().await;
    let path = root.path().join("parent-confirmation.json");
    std::fs::write(&path, r#"{"confirm":["script","fill"]}"#).unwrap();
    state.policy = Some(ActionPolicy::load(path.to_str().unwrap()).unwrap());
    let result=handle_script(&json!({"autoConfirm":true,"program":[{"do":"script","optional":true,"autoConfirm":true,"program":[{"do":"fill","selector":"#editor","value":"replacement","allowClipboardFallback":true}]},{"do":"evaluate","script":"publish_marker"}]}),&mut state).await.unwrap();
    assert_eq!(result["approval_refused"], true, "{result}");
    assert_eq!(result["ok"], false, "{result}");
    assert!(calls.lock().unwrap().is_empty(), "{result}");
    assert!(state.pending_confirmation.is_none());
    server.abort();
}

#[tokio::test]
async fn native_pending_status_failure_cannot_be_swallowed_by_optional() {
    let (mut state, server, calls, _root, _mode) = fixture_with_status(
        json!({"confirm":["clipboard"]}),
        "https://fixture.test/editor",
        None,
    )
    .await;
    state.policy = None;
    let result=handle_script(&json!({"autoConfirm":true,"program":[{"do":"fill","selector":"#editor","value":"replacement","allowClipboardFallback":true,"optional":true},{"do":"evaluate","script":"publish_marker"}]}),&mut state).await.unwrap();
    assert_eq!(result["confirmation_required"], true, "{result}");
    assert_eq!(result["approval"], "user", "{result}");
    assert_eq!(result["ok"], false, "{result}");
    let pending = state
        .pending_confirmation
        .as_ref()
        .expect("native request must remain held");
    assert_eq!(result["confirmation_id"], pending.site.as_ref().unwrap().id);
    assert!(calls.lock().unwrap().is_empty(), "{result}");
    server.abort();
}

#[tokio::test]
async fn pending_javascript_trace_keeps_an_ordinary_nested_failure_false() {
    let (mut state, server, calls, _root, _mode) = fixture().await;
    let result=handle_script(&json!({"source":"try { cu._call('script', {program:[{assert:{eq:[1,2]}}]}); } catch (e) {} cu.fill('#editor','replacement');"}),&mut state).await.unwrap();
    assert_eq!(result["confirmation_required"], true, "{result}");
    assert_eq!(result["steps"][0]["op"], "script", "{result}");
    assert_eq!(result["steps"][0]["ok"], false, "{result}");
    assert!(calls.lock().unwrap().is_empty());
    server.abort();
}
