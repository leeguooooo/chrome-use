//! Protocol fault injection. Payload semantics are checked separately by Node;
//! these fixtures do not establish real Chrome clipboard permissions or events.
use super::{fill_monaco_via_clipboard, finish_fill, read_hidden_monaco, CdpClient};
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use std::sync::{Arc, Mutex};

#[derive(Clone, Copy)]
enum Operation {
    ClipboardFill,
    HiddenFill,
    HiddenRead,
}

async fn run(
    setup: bool,
    input: bool,
    cleanup: bool,
    changed: bool,
) -> (Result<String, String>, Vec<Value>) {
    run_mode(
        setup,
        input,
        cleanup,
        changed,
        Operation::ClipboardFill,
        true,
    )
    .await
}

async fn run_mode(
    setup_error: bool,
    input_error: bool,
    cleanup_error: bool,
    changed_before_input: bool,
    operation: Operation,
    allow_clipboard: bool,
) -> (Result<String, String>, Vec<Value>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let log = seen.clone();
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut ws = tokio_tungstenite::accept_async(stream).await.unwrap();
        while let Some(Ok(message)) = ws.next().await {
            let Ok(text) = message.into_text() else {
                continue;
            };
            let request: Value = serde_json::from_str(&text).unwrap();
            log.lock().unwrap().push(request.clone());
            let mut error = None;
            let result = match request["method"].as_str().unwrap() {
                "Page.getFrameTree" => json!({"frameTree":{"frame":{"id":"frame"}}}),
                "Page.createIsolatedWorld" => json!({"executionContextId":42}),
                "Runtime.evaluate" => json!({"result":{"type":"boolean","value":true}}),
                "Runtime.releaseObject" | "Runtime.releaseObjectGroup" => json!({}),
                "Input.dispatchKeyEvent" => {
                    if input_error {
                        error = Some(json!({"code":-32000,"message":"input rejected"}));
                    }
                    json!({})
                }
                "Runtime.callFunctionOn" => {
                    let script = request["params"]["functionDeclaration"].as_str().unwrap();
                    if request["params"]["objectId"] == "anchor"
                        && script != include_str!("monaco_clipboard.js")
                    {
                        json!({"result":{"type":"object","subtype":"null","value":null}})
                    } else if script == include_str!("monaco_clipboard.js") {
                        assert_eq!(request["params"]["returnByValue"], false);
                        json!({"result":{"type":"object","objectId":"transaction"}})
                    } else {
                        assert_eq!(request["params"]["objectId"], "transaction");
                        let data = if script.contains("this.start(") {
                            if setup_error {
                                json!({"ok":false,"error":"focus failed"})
                            } else {
                                json!({"ok":true})
                            }
                        } else if script.contains("this.active()") {
                            if changed_before_input {
                                json!({"ok":false,"error":"clipboard changed before editor input"})
                            } else {
                                json!({"ok":true})
                            }
                        } else if script.contains("this.finish()") {
                            if cleanup_error {
                                json!({"ok":false,"error":"restore denied"})
                            } else {
                                json!({"ok":true})
                            }
                        } else {
                            json!({"trusted":true,"prevented":true,"text":"replacement"})
                        };
                        json!({"result":{"type":"object","value":data}})
                    }
                }
                other => panic!("unexpected CDP method: {other}"),
            };
            let response = match error {
                Some(error) => json!({"id":request["id"],"error":error}),
                None => json!({"id":request["id"],"result":result}),
            };
            if ws
                .send(tokio_tungstenite::tungstenite::Message::Text(
                    response.to_string(),
                ))
                .await
                .is_err()
            {
                break;
            }
        }
    });
    let client = CdpClient::connect(&format!("ws://{address}"))
        .await
        .unwrap();
    let permission = if allow_clipboard {
        super::ClipboardFallback::Allowed
    } else {
        super::ClipboardFallback::Denied
    };
    let result = super::MONACO_CLIPBOARD_FALLBACK
        .scope(std::cell::Cell::new(permission), async {
            match operation {
                Operation::ClipboardFill => {
                    fill_monaco_via_clipboard(&client, "session", "anchor", "replacement").await
                }
                Operation::HiddenRead => read_hidden_monaco(&client, "session", "anchor").await,
                Operation::HiddenFill => {
                    let mut engine = "monaco-unsupported".to_string();
                    finish_fill(&client, "session", "anchor", "replacement", &mut engine)
                        .await
                        .map(|_| engine)
                }
            }
        })
        .await;
    server.abort();
    let commands = seen.lock().unwrap().clone();
    (result, commands)
}
fn assert_finished(commands: &[Value]) {
    assert_eq!(commands.iter().filter(|r| r["params"]["functionDeclaration"] == "function() { return this.finish(); }").count(), 1);
    assert_eq!(commands.last().unwrap()["method"], "Runtime.releaseObject");
}
#[tokio::test]
async fn monaco_private_handle_success_is_verified_and_released() {
    let (result, commands) = run(false, false, false, false).await;
    assert_eq!(result.unwrap(), "monaco-clipboard");
    assert_finished(&commands);
}
#[tokio::test]
async fn monaco_setup_failure_still_finishes_and_never_sends_input() {
    let (result, commands) = run(true, false, false, false).await;
    assert!(result.unwrap_err().contains("focus failed"));
    assert!(!commands
        .iter()
        .any(|r| r["method"] == "Input.dispatchKeyEvent"));
    assert_finished(&commands);
}
#[tokio::test]
async fn monaco_input_failure_keeps_both_operation_and_cleanup_errors() {
    let (result, commands) = run(false, true, true, false).await;
    let error = result.unwrap_err();
    assert!(error.contains("input rejected"));
    assert!(error.contains("restore denied"));
    assert_finished(&commands);
}
#[tokio::test]
async fn monaco_cleanup_failure_is_not_reported_as_fill_success() {
    let (result, commands) = run(false, false, true, false).await;
    assert!(result.unwrap_err().contains("restore denied"));
    assert_finished(&commands);
}

#[tokio::test]
async fn monaco_changed_clipboard_refuses_input_and_preserves_the_reason() {
    let (result, commands) = run(false, false, false, true).await;
    assert!(result
        .unwrap_err()
        .contains("clipboard changed before editor input"));
    assert!(!commands
        .iter()
        .any(|r| r["method"] == "Input.dispatchKeyEvent"));
    assert_finished(&commands);
}

#[tokio::test]
async fn undiscoverable_monaco_defaults_do_not_stage_clipboard() {
    for mode in [Operation::HiddenFill, Operation::HiddenRead] {
        let (result, commands) = run_mode(false, false, false, false, mode, false).await;
        let error = result.unwrap_err();
        assert!(error.contains("--allow-clipboard-fallback"), "{error}");
        assert!(
            !commands
                .iter()
                .any(|r| r["params"]["functionDeclaration"] == include_str!("monaco_clipboard.js")),
            "clipboard transaction was created: {commands:?}"
        );
        assert!(
            !commands.iter().any(|r| r["method"]
                .as_str()
                .is_some_and(|m| m.starts_with("Input."))),
            "input was dispatched: {commands:?}"
        );
    }
}

#[tokio::test]
async fn explicitly_allowed_hidden_monaco_uses_legacy_checks_without_sticky_permission() {
    for mode in [Operation::HiddenFill, Operation::HiddenRead] {
        let (result, commands) = run_mode(false, false, false, false, mode, true).await;
        assert!(result.is_ok(), "{result:?}");
        assert!(commands
            .iter()
            .any(|r| r["params"]["functionDeclaration"] == include_str!("monaco_clipboard.js")));
        assert_finished(&commands);
        let (result, commands) = run_mode(false, false, false, false, mode, false).await;
        assert!(result.is_err());
        assert!(!commands
            .iter()
            .any(|r| r["params"]["functionDeclaration"] == include_str!("monaco_clipboard.js")));
    }
}
