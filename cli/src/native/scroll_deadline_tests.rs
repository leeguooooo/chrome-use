//! A canceled CDP waiter can leave Chrome processing the request. Never follow
//! a stalled wheel phase with another mutating scroll in the same command.
use super::*;
use futures_util::{SinkExt, StreamExt};
use std::sync::Mutex;
use tokio::sync::Notify;
use tokio_tungstenite::tungstenite::Message;

#[derive(Clone, Copy, PartialEq)]
enum Fault {
    Measurement,
    Pointer,
    Wheel,
    WheelError,
}

async fn run(fault: Fault) -> (String, Duration, Vec<Value>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let log = Arc::new(Mutex::new(Vec::new()));
    let seen = log.clone();
    let stalled = Arc::new(Notify::new());
    let notified = stalled.clone();
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut ws = tokio_tungstenite::accept_async(stream).await.unwrap();
        while let Some(Ok(Message::Text(raw))) = ws.next().await {
            let request: Value = serde_json::from_str(&raw).unwrap();
            seen.lock().unwrap().push(request.clone());
            let method = request["method"].as_str().unwrap();
            let mut error = None;
            let result = match method {
                "Page.getFrameTree" => json!({"frameTree":{"frame":{"id":"main"}}}),
                "Page.createIsolatedWorld" => json!({"executionContextId":42}),
                "Runtime.evaluate" => {
                    if request["params"]["expression"]
                        .as_str()
                        .is_some_and(|e| e.starts_with("document.querySelector"))
                    {
                        json!({"result":{"objectId":"node"}})
                    } else {
                        json!({"result":{"value":"visible"}})
                    }
                }
                "DOM.describeNode" => json!({"node":{"backendNodeId":1}}),
                "DOM.resolveNode" => json!({"object":{"objectId":"node"}}),
                "Runtime.callFunctionOn" => {
                    if request["params"]["functionDeclaration"]
                        == super::super::quiet_dom::MEASURE_FN
                    {
                        if fault == Fault::Measurement {
                            notified.notify_one();
                            continue;
                        }
                        json!({"result":{"value":{"x":40,"y":1200,"w":80,"h":40,"vw":1000,"vh":700,"scroller":null}}})
                    } else {
                        json!({"result":{"value":true}})
                    }
                }
                "Input.dispatchMouseEvent" => {
                    let kind = request["params"]["type"].as_str().unwrap_or("");
                    if (fault == Fault::Pointer && kind == "mouseMoved")
                        || (matches!(fault, Fault::Wheel | Fault::WheelError)
                            && kind == "mouseWheel")
                    {
                        notified.notify_one();
                        if fault != Fault::WheelError {
                            continue;
                        }
                        error = Some("action_outcome_unknown: wheel acknowledgement lost");
                    }
                    json!({})
                }
                _ => json!({}),
            };
            let mut reply = json!({"id":request["id"],"result":result});
            if let Some(message) = error {
                reply = json!({"id":request["id"],"error":{"code":-32000,"message":message}});
            }
            if ws.send(Message::Text(reply.to_string())).await.is_err() {
                break;
            }
        }
    });
    let page = super::super::browser::PageInfo {
        tab_id: 1,
        label: None,
        target_id: "target".into(),
        session_id: "session".into(),
        url: "https://example.test/".into(),
        title: "Fixture".into(),
        target_type: "page".into(),
    };
    let mut browser = BrowserManager::for_test(&format!("ws://{address}"), vec![page]).await;
    browser.reown_target("target").unwrap();
    let client = browser.client.clone();
    let mut state = DaemonState::new();
    state.browser = Some(browser);
    let budget = if fault == Fault::Measurement {
        Duration::from_millis(120)
    } else {
        Duration::from_secs(2)
    };
    let started = std::time::Instant::now();
    let action = tokio::spawn(async move {
        scroll_into_view_with_budget(&json!({"selector":"#target"}), &mut state, budget).await
    });
    tokio::time::timeout(Duration::from_secs(3), stalled.notified())
        .await
        .expect("fixture must observe the intended in-flight request");
    let error = tokio::time::timeout(Duration::from_secs(3), action)
        .await
        .expect("wheel phase exceeded its outer deadline")
        .unwrap()
        .unwrap_err();
    let elapsed = started.elapsed();
    assert!(
        elapsed < budget + Duration::from_secs(1),
        "elapsed {elapsed:?}, budget {budget:?}"
    );
    // This read-only barrier confirms all earlier queued messages were observed.
    client
        .send_command("Browser.getVersion", None, Some("session"))
        .await
        .unwrap();
    server.abort();
    let commands = log.lock().unwrap().clone();
    (error, elapsed, commands)
}

fn assert_no_direct_scroll(commands: &[Value]) {
    assert!(
        !commands.iter().any(|r| r["params"]["functionDeclaration"]
            .as_str()
            .is_some_and(|f| f.contains("scrollIntoView"))),
        "must not issue a direct-scroll fallback after an unfinished phase"
    );
}

#[tokio::test]
async fn stalled_measurement_stops_within_wheel_budget_without_input_or_fallback() {
    let (error, _, commands) = run(Fault::Measurement).await;
    assert!(error.contains("while measuring"), "{error}");
    assert!(!error.contains("action_outcome_unknown"), "{error}");
    assert!(!commands
        .iter()
        .any(|r| r["method"].as_str().unwrap_or("").starts_with("Input.")));
    assert_no_direct_scroll(&commands);
}

#[tokio::test]
async fn pending_pointer_is_unknown_without_scroll_fallback() {
    let (error, _, commands) = run(Fault::Pointer).await;
    assert!(error.starts_with("action_outcome_unknown:"), "{error}");
    assert_eq!(
        commands
            .iter()
            .filter(|r| r["method"] == "Input.dispatchMouseEvent")
            .count(),
        1
    );
    assert_no_direct_scroll(&commands);
}

#[tokio::test]
async fn pending_wheel_is_unknown_without_replay_or_fallback() {
    let (error, _, commands) = run(Fault::Wheel).await;
    assert!(error.starts_with("action_outcome_unknown:"), "{error}");
    assert_eq!(
        commands
            .iter()
            .filter(|r| r["params"]["type"] == "mouseWheel")
            .count(),
        1
    );
    assert_no_direct_scroll(&commands);
}

#[tokio::test]
async fn lost_wheel_acknowledgement_stops_without_direct_fallback() {
    let (error, _, commands) = run(Fault::WheelError).await;
    assert!(error.starts_with("action_outcome_unknown:"), "{error}");
    assert_eq!(
        commands
            .iter()
            .filter(|r| r["params"]["type"] == "mouseWheel")
            .count(),
        1
    );
    assert_no_direct_scroll(&commands);
}
