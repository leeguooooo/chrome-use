//! Exercise domain preflight over CDP and record both navigation and adapter calls.
use super::*;
use crate::native::browser::PageInfo;
use futures_util::{SinkExt, StreamExt};
use std::sync::Mutex;
use tokio_tungstenite::tungstenite::Message;

const ADAPTER: &str = "'fixture-adapter-ran'";

async fn run_site(current: Result<&str, &str>) -> (Result<Value, String>, Vec<Value>) {
    let current = current.map(str::to_owned).map_err(str::to_owned);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("ws://{}", listener.local_addr().unwrap());
    let log = Arc::new(Mutex::new(Vec::new()));
    let seen = log.clone();
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut ws = tokio_tungstenite::accept_async(stream).await.unwrap();
        while let Some(Ok(Message::Text(raw))) = ws.next().await {
            let request: Value = serde_json::from_str(&raw).unwrap();
            seen.lock().unwrap().push(request.clone());
            let result: Result<Value, String> = match request["method"].as_str().unwrap() {
                "Page.getFrameTree" => Ok(json!({"frameTree":{"frame":{"id":"frame"}}})),
                "Page.createIsolatedWorld" => Ok(json!({"executionContextId":42})),
                "Runtime.evaluate" => match request["params"]["expression"].as_str() {
                    Some("location.href") => current
                        .as_ref()
                        .map(|url| json!({"result":{"type":"string","value":url}}))
                        .map_err(Clone::clone),
                    Some(ADAPTER) => Ok(json!({"result":{"type":"string","value":"adapter-ran"}})),
                    _ => Ok(json!({"result":{"type":"undefined"}})),
                },
                // Refuse deliberately: the assertion is about dispatch, not loading a page.
                "Page.navigate" => Err("fixture navigation dispatched".into()),
                _ => Ok(json!({})),
            };
            let mut response = match result {
                Ok(result) => json!({"id":request["id"],"result":result}),
                Err(message) => {
                    json!({"id":request["id"],"error":{"code":-32000,"message":message}})
                }
            };
            if let Some(session) = request.get("sessionId") {
                response["sessionId"] = session.clone();
            }
            if ws.send(Message::Text(response.to_string())).await.is_err() {
                break;
            }
        }
    });
    let page = PageInfo {
        tab_id: 1,
        label: None,
        target_id: "target".into(),
        session_id: "session".into(),
        // Cached metadata is deliberately different; it must not replace a live URL read.
        url: "https://cached.invalid/".into(),
        title: "Fixture".into(),
        target_type: "page".into(),
    };
    let mut state = DaemonState::new();
    let mut browser = BrowserManager::for_test(&url, vec![page]).await;
    // for_test pins the page but intentionally grants no tab ownership.
    browser.reown_target("target").unwrap();
    assert_eq!(browser.active_session_id().unwrap(), "session");
    state.browser = Some(browser);
    let result = handle_site(
        &json!({"domain":"example.test","script":ADAPTER}),
        &mut state,
    )
    .await;
    server.abort();
    let requests = log.lock().unwrap().clone();
    (result, requests)
}

#[tokio::test]
async fn unreadable_url_preserves_error_without_navigation_or_adapter_execution() {
    for error in [
        "Timeout waiting for Runtime.evaluate",
        "Execution context was destroyed",
    ] {
        let (result, requests) = run_site(Err(error)).await;
        assert!(requests
            .iter()
            .any(|r| r["params"]["expression"] == "location.href"));
        assert!(
            !requests.iter().any(|r| r["method"] == "Page.navigate"),
            "URL read failure must not navigate: {requests:?}"
        );
        assert!(!requests
            .iter()
            .any(|r| r["params"]["expression"] == ADAPTER));
        let message = result.unwrap_err();
        assert!(message.starts_with("site_domain_unverified:"), "{message}");
        assert!(message.contains(error), "original error lost: {message}");
    }
}

#[tokio::test]
async fn matching_domain_and_subdomain_run_adapter_without_navigation() {
    for current in ["https://example.test/path", "https://sub.example.test/path"] {
        let (result, requests) = run_site(Ok(current)).await;
        assert_eq!(result.unwrap()["result"], "adapter-ran");
        assert!(!requests.iter().any(|r| r["method"] == "Page.navigate"));
        assert_eq!(
            requests
                .iter()
                .filter(|r| r["params"]["expression"] == ADAPTER)
                .count(),
            1
        );
    }
}

#[tokio::test]
async fn known_other_domain_and_blank_page_still_request_navigation() {
    for current in [
        "https://other.test/",
        "https://notexample.test/",
        "about:blank",
    ] {
        let (result, requests) = run_site(Ok(current)).await;
        let navs: Vec<_> = requests
            .iter()
            .filter(|r| r["method"] == "Page.navigate")
            .collect();
        assert_eq!(navs.len(), 1, "{current}: {requests:?}");
        assert_eq!(navs[0]["params"]["url"], "https://example.test/");
        assert!(result
            .unwrap_err()
            .contains("fixture navigation dispatched"));
        assert!(!requests
            .iter()
            .any(|r| r["params"]["expression"] == ADAPTER));
    }
}
