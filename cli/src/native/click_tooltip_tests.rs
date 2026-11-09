//! #847: a hover tooltip over a `@ref` click target is cleared by moving the
//! pointer off it (trusted `mouseMoved` only), never by pressing or by DOM
//! dispatch; anything else that covers the target is still refused.
use super::*;
use futures_util::{SinkExt, StreamExt};
use std::sync::{Arc, Mutex};
use tokio_tungstenite::tungstenite::Message;

const REST: (f64, f64) = (40.0, 300.0);

#[derive(Clone, Copy, PartialEq)]
enum Cover {
    /// A tooltip that hides once the pointer leaves the control under it.
    Tooltip,
    /// A tooltip that stays whatever the pointer does.
    StuckTooltip,
    /// An ordinary overlay: no hover layer, no rest point.
    Overlay,
}

async fn run(cover: Cover, clear_hover_layer: bool) -> (Result<(), String>, Vec<Value>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let log = Arc::new(Mutex::new(Vec::new()));
    let seen = log.clone();
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut ws = tokio_tungstenite::accept_async(stream).await.unwrap();
        let mut pointer_off = false;
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
                "Runtime.evaluate" => json!({"result":{"type":"string","value":"visible"}}),
                "Runtime.callFunctionOn"
                    if request["params"]["functionDeclaration"] == OCCLUSION_PROBE_JS =>
                {
                    let covered = match cover {
                        Cover::Tooltip => !pointer_off,
                        Cover::StuckTooltip | Cover::Overlay => true,
                    };
                    let value = if !covered {
                        Value::Null
                    } else if cover == Cover::Overlay {
                        json!(json!({"tag":"DIV","role":null,"text":"Cookie consent"}).to_string())
                    } else {
                        json!(json!({
                            "tag":"DIV","role":"tooltip",
                            "text":"Download full data according to current view.",
                            "hover":true,"rest":{"x":REST.0,"y":REST.1}
                        })
                        .to_string())
                    };
                    json!({"result":{"type":"string","value":value}})
                }
                "Input.dispatchMouseEvent" => {
                    let p = &request["params"];
                    if p["type"] == "mouseMoved"
                        && p["x"].as_f64() == Some(REST.0)
                        && p["y"].as_f64() == Some(REST.1)
                    {
                        pointer_off = true;
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
    let client = CdpClient::connect(&format!("ws://{address}"))
        .await
        .unwrap();
    let outcome = tokio::time::timeout(
        std::time::Duration::from_secs(10),
        verify_click_target(
            &client,
            "session",
            1,
            "e25",
            500.0,
            200.0,
            clear_hover_layer,
            false,
        ),
    )
    .await
    .expect("occlusion check exceeded its budget");
    server.abort();
    let commands = log.lock().unwrap().clone();
    (outcome, commands)
}

fn mouse(commands: &[Value]) -> Vec<&Value> {
    commands
        .iter()
        .filter(|r| r["method"] == "Input.dispatchMouseEvent")
        .collect()
}

fn no_press_or_dom_click(commands: &[Value]) {
    assert!(
        mouse(commands)
            .iter()
            .all(|r| r["params"]["type"] == "mouseMoved"),
        "only pointer moves may be sent while checking: {commands:?}"
    );
    assert!(
        !commands
            .iter()
            .any(|r| r["method"] == "Runtime.callFunctionOn"
                && r["params"]["functionDeclaration"]
                    .as_str()
                    .is_some_and(|f| f.contains(".click()"))),
        "the covered target must never be clicked through the DOM"
    );
}

#[tokio::test]
async fn hover_tooltip_is_cleared_by_moving_the_pointer_off_it() {
    let (outcome, commands) = run(Cover::Tooltip, true).await;
    assert_eq!(outcome, Ok(()));
    let moves = mouse(&commands);
    assert!(!moves.is_empty(), "the pointer must move off the tooltip");
    let last = moves.last().unwrap();
    assert_eq!(
        (last["params"]["x"].as_f64(), last["params"]["y"].as_f64()),
        (Some(REST.0), Some(REST.1)),
        "the move lands on the probe's rest point"
    );
    // The probe ran again after the move, and that is what cleared it.
    let moved_at = commands
        .iter()
        .rposition(|r| r["method"] == "Input.dispatchMouseEvent")
        .unwrap();
    assert!(commands[moved_at..]
        .iter()
        .any(|r| r["params"]["functionDeclaration"] == OCCLUSION_PROBE_JS));
    no_press_or_dom_click(&commands);
}

#[tokio::test]
async fn a_tooltip_that_stays_is_still_refused_after_one_move() {
    let (outcome, commands) = run(Cover::StuckTooltip, true).await;
    let error = outcome.unwrap_err();
    assert!(error.contains("occluded by DIV[role=tooltip]"), "{error}");
    assert!(error.contains("The pointer was moved off it"), "{error}");
    let landings = mouse(&commands)
        .iter()
        .filter(|r| r["params"]["x"].as_f64() == Some(REST.0))
        .count();
    assert_eq!(landings, 1, "the pointer is moved off once, not per retry");
    no_press_or_dom_click(&commands);
}

#[tokio::test]
async fn without_pointer_clearing_a_tooltip_is_refused_and_the_pointer_stays() {
    let (outcome, commands) = run(Cover::Tooltip, false).await;
    let error = outcome.unwrap_err();
    assert!(error.contains("occluded by DIV[role=tooltip]"), "{error}");
    assert!(!error.contains("moved off"), "{error}");
    assert!(mouse(&commands).is_empty());
}

#[tokio::test]
async fn an_ordinary_overlay_never_moves_the_pointer() {
    let (outcome, commands) = run(Cover::Overlay, true).await;
    let error = outcome.unwrap_err();
    assert!(error.contains("is occluded by DIV"), "{error}");
    assert!(mouse(&commands).is_empty());
}
