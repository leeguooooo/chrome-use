//! Faulted CDP acknowledgements cannot trigger a second scroll or a press.
use super::*;
use futures_util::{SinkExt, StreamExt};
use std::sync::{Arc, Mutex};
use std::time::Instant;
use tokio::sync::Notify;
use tokio_tungstenite::tungstenite::Message;

async fn run(change_epoch: bool) -> (String, Vec<Value>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let log = Arc::new(Mutex::new(Vec::new()));
    let seen = log.clone();
    let reached = Arc::new(Notify::new());
    let signal = reached.clone();
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut ws = tokio_tungstenite::accept_async(stream).await.unwrap();
        let mut changed = false;
        while let Some(Ok(Message::Text(raw))) = ws.next().await {
            let request: Value = serde_json::from_str(&raw).unwrap();
            seen.lock().unwrap().push(request.clone());
            let result = match request["method"].as_str().unwrap() {
                "Page.getFrameTree" => {
                    json!({"frameTree":{"frame":{"id":"frame","loaderId":if changed{"new"}else{"old"},"url":"https://fixture.test/"}}})
                }
                "Page.createIsolatedWorld" => json!({"executionContextId":42}),
                "DOM.resolveNode" => {
                    json!({"object":{"type":"object","subtype":"node","objectId":"target"}})
                }
                "DOM.describeNode" => json!({"node":{"backendNodeId":1}}),
                "Runtime.evaluate" => json!({"result":{"type":"string","value":"visible"}}),
                "Runtime.callFunctionOn" => {
                    if request["params"]["functionDeclaration"] == SCROLL_AND_PROBE_DISABLED_JS {
                        assert_eq!(
                            request["params"]["arguments"].as_array().unwrap().len(),
                            3,
                            "rechecks must not repeat nearest-edge preparation"
                        );
                    }
                    json!({"result":{"type":"object","value":{"header":true}}})
                }
                "Input.dispatchMouseEvent" => {
                    if change_epoch && request["params"]["type"] == "mouseMoved" {
                        changed = true;
                        signal.notify_one();
                    }
                    if !change_epoch && request["params"]["type"] == "mouseWheel" {
                        signal.notify_one();
                        continue;
                    }
                    json!({})
                }
                _ => json!({}),
            };
            if ws
                .send(Message::Text(
                    json!({"id":request["id"],"result":result}).to_string(),
                ))
                .await
                .is_err()
            {
                break;
            }
        }
    });
    let client = Arc::new(
        CdpClient::connect(&format!("ws://{address}"))
            .await
            .unwrap(),
    );
    let resolved = client
        .send_command(
            "DOM.resolveNode",
            Some(json!({"backendNodeId":1})),
            Some("session"),
        )
        .await
        .unwrap();
    assert_eq!(resolved["object"]["objectId"], "target");
    let task_client = client.clone();
    let started = Instant::now();
    let action = tokio::spawn(async move {
        recover_header_with_wheel(
            &task_client,
            &ClickTargetGuard {
                object_id: "target".into(),
                session_id: "session".into(),
                epoch: Some(ClickEpoch {
                    frame_id: "frame".into(),
                    loader_id: "old".into(),
                }),
            },
            HeaderScrollPlan {
                x: 500.0,
                y: 300.0,
                dy: -200.0,
            },
            Duration::from_secs(2),
        )
        .await
    });
    tokio::time::timeout(Duration::from_secs(3), reached.notified())
        .await
        .expect("fixture must observe the faulted input");
    let error = tokio::time::timeout(Duration::from_secs(3), action)
        .await
        .expect("header gesture exceeded its outer budget")
        .unwrap()
        .unwrap_err();
    assert!(started.elapsed() < Duration::from_secs(3));
    client
        .send_command("Browser.getVersion", None, Some("session"))
        .await
        .unwrap();
    server.abort();
    let commands = log.lock().unwrap().clone();
    (error, commands)
}

#[tokio::test]
async fn pending_header_wheel_stops_at_budget_without_replay_or_press() {
    let (error, commands) = run(false).await;
    assert!(error.starts_with("action_outcome_unknown:"), "{error}");
    assert!(error.contains("did not finish"), "{error}");
    assert_eq!(
        commands
            .iter()
            .filter(|r| r["params"]["type"] == "mouseWheel")
            .count(),
        1
    );
    assert!(!commands.iter().any(|r| matches!(
        r["params"]["type"].as_str(),
        Some("mousePressed" | "mouseReleased")
    )));
}

#[tokio::test]
async fn changed_header_document_epoch_refuses_wheel_and_press() {
    let (error, commands) = run(true).await;
    assert!(error.starts_with("action_outcome_unknown:"), "{error}");
    assert!(error.contains("document changed"), "{error}");
    assert!(!commands.iter().any(|r| matches!(
        r["params"]["type"].as_str(),
        Some("mouseWheel" | "mousePressed" | "mouseReleased")
    )));
}
