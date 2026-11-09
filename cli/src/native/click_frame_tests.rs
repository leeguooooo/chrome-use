//! A `@ref` in an in-process child frame (same-site, cross-origin) gets a
//! pointer click in top-frame coordinates, while the occlusion probe
//! hit-tests in the frame's own document: its point is moved by the frame's
//! offset, which is `DOM.getBoxModel` (top-frame) minus the node's own rect.
use super::*;
use futures_util::{SinkExt, StreamExt};
use std::sync::{Arc, Mutex};
use tokio_tungstenite::tungstenite::Message;

/// The node's rect in its frame, and its border box in the top frame.
const LOCAL: (f64, f64) = (10.0, 20.0);
const ROOT: (f64, f64) = (70.0, 178.0);

async fn run(in_child_frame: bool) -> Vec<Value> {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let log = Arc::new(Mutex::new(Vec::new()));
    let seen = log.clone();
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut ws = tokio_tungstenite::accept_async(stream).await.unwrap();
        while let Some(Ok(Message::Text(raw))) = ws.next().await {
            let request: Value = serde_json::from_str(&raw).unwrap();
            seen.lock().unwrap().push(request.clone());
            let result = match request["method"].as_str().unwrap() {
                "Page.getFrameTree" => {
                    json!({"frameTree":{"frame":{"id":"frame","loaderId":"l","url":"https://fixture.test/"}}})
                }
                "Page.createIsolatedWorld" => json!({"executionContextId":42}),
                "DOM.resolveNode" => {
                    json!({"object":{"type":"object","subtype":"node","objectId":"target"}})
                }
                "DOM.getBoxModel" => {
                    let (x, y) = ROOT;
                    json!({"model":{"border":[x,y,x+100.0,y,x+100.0,y+28.0,x,y+28.0],
                        "content":[x,y,x+100.0,y,x+100.0,y+28.0,x,y+28.0],"width":100,"height":28}})
                }
                "Runtime.callFunctionOn"
                    if request["params"]["functionDeclaration"] == OCCLUSION_PROBE_JS =>
                {
                    json!({"result":{"type":"object","subtype":"null","value":null}})
                }
                "Runtime.callFunctionOn" => {
                    json!({"result":{"type":"object","value":[LOCAL.0, LOCAL.1]}})
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
    let client = CdpClient::connect(&format!("ws://{address}"))
        .await
        .unwrap();
    let outcome = tokio::time::timeout(
        std::time::Duration::from_secs(10),
        verify_click_target(
            &client,
            "session",
            1,
            "e7",
            500.0,
            200.0,
            false,
            in_child_frame,
        ),
    )
    .await
    .expect("occlusion check exceeded its budget");
    assert_eq!(outcome, Ok(()));
    server.abort();
    let commands = log.lock().unwrap().clone();
    commands
}

fn probe_point(commands: &[Value]) -> (f64, f64) {
    let probe = commands
        .iter()
        .find(|r| r["params"]["functionDeclaration"] == OCCLUSION_PROBE_JS)
        .expect("the occlusion probe ran");
    let args = &probe["params"]["arguments"];
    (
        args[0]["value"].as_f64().unwrap(),
        args[1]["value"].as_f64().unwrap(),
    )
}

#[tokio::test]
async fn a_child_frame_target_is_hit_tested_at_its_frame_local_point() {
    let commands = run(true).await;
    assert_eq!(
        probe_point(&commands),
        (500.0 - (ROOT.0 - LOCAL.0), 200.0 - (ROOT.1 - LOCAL.1))
    );
}

#[tokio::test]
async fn a_top_frame_target_keeps_its_point_and_costs_no_extra_round_trip() {
    let commands = run(false).await;
    assert_eq!(probe_point(&commands), (500.0, 200.0));
    assert!(!commands.iter().any(|r| r["method"] == "DOM.getBoxModel"));
}
