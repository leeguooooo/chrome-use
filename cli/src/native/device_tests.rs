use super::*;
use crate::native::browser::PageInfo;
use futures_util::{SinkExt, StreamExt};
use std::sync::Mutex;
use tokio_tungstenite::tungstenite::Message;

async fn fixture(
    failure: Option<&str>,
) -> (
    DaemonState,
    Arc<Mutex<Vec<Value>>>,
    tokio::task::JoinHandle<()>,
) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("ws://{}", listener.local_addr().unwrap());
    let log = Arc::new(Mutex::new(Vec::new()));
    let seen = log.clone();
    let failure = failure.map(str::to_string);
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut ws = tokio_tungstenite::accept_async(stream).await.unwrap();
        while let Some(Ok(Message::Text(raw))) = ws.next().await {
            let request: Value = serde_json::from_str(&raw).unwrap();
            seen.lock().unwrap().push(request.clone());
            let mut response = if request["method"].as_str() == failure.as_deref() {
                json!({"id":request["id"],"error":{"code":-32000,"message":"fixture refused reset"}})
            } else {
                let result = if request["method"] == "Page.getLayoutMetrics" {
                    json!({"cssLayoutViewport":{"clientWidth":1365,"clientHeight":768}})
                } else {
                    json!({})
                };
                json!({"id":request["id"],"result":result})
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
        url: "https://fixture.test/".into(),
        title: "Fixture".into(),
        target_type: "page".into(),
    };
    let mut browser = BrowserManager::for_test(&url, vec![page]).await;
    browser.reown_target("target").unwrap();
    let mut state = DaemonState::new();
    state.browser = Some(browser);
    state.viewport = Some((390, 844, 3.0, true));
    state.session_setup.user_agent = Some("mobile-fixture".into());
    (state, log, server)
}

#[tokio::test]
async fn reset_clears_overrides_and_future_tab_user_agent() {
    let (mut state, log, server) = fixture(None).await;
    for name in ["off", "reset", "none", " OFF "] {
        let result = handle_device(&json!({"device":name}), &mut state)
            .await
            .unwrap();
        assert_eq!(
            result,
            json!({"device":"off", "reset":true,"width":1365,"height":768})
        );
        assert!(state.viewport.is_none());
        assert!(state.session_setup.user_agent.is_none());
    }
    let requests = log.lock().unwrap();
    let methods: Vec<_> = requests
        .iter()
        .map(|r| r["method"].as_str().unwrap())
        .collect();
    assert_eq!(
        methods,
        [
            "Emulation.clearDeviceMetricsOverride",
            "Emulation.setUserAgentOverride",
            "Emulation.setTouchEmulationEnabled",
            "Page.getLayoutMetrics"
        ]
        .repeat(4)
    );
    for request in requests
        .iter()
        .filter(|r| r["method"] == "Emulation.setUserAgentOverride")
    {
        assert_eq!(request["params"], json!({"userAgent":""}));
    }
    for request in requests
        .iter()
        .filter(|r| r["method"] == "Emulation.setTouchEmulationEnabled")
    {
        assert_eq!(request["params"], json!({"enabled":false}));
    }
    server.abort();
}

#[tokio::test]
async fn reset_failures_report_partial_state_and_stop_before_later_steps() {
    for (failure, viewport_cleared, ua_cleared, count) in [
        ("Emulation.clearDeviceMetricsOverride", false, false, 1),
        ("Emulation.setUserAgentOverride", true, false, 2),
        ("Emulation.setTouchEmulationEnabled", true, true, 3),
        ("Page.getLayoutMetrics", true, true, 4),
    ] {
        let (mut state, log, server) = fixture(Some(failure)).await;
        let error = handle_device(&json!({"device":"off"}), &mut state)
            .await
            .unwrap_err();
        assert!(error.contains("fixture refused reset"), "{error}");
        assert_eq!(state.viewport.is_none(), viewport_cleared, "{failure}");
        assert_eq!(
            state.session_setup.user_agent.is_none(),
            ua_cleared,
            "{failure}"
        );
        assert_eq!(log.lock().unwrap().len(), count, "{failure}");
        server.abort();
    }
}

#[tokio::test]
async fn iphone_13_uses_its_css_geometry_and_user_agent() {
    let (mut state, log, server) = fixture(None).await;
    let result = handle_device(&json!({"device":"iPhone 13"}), &mut state)
        .await
        .unwrap();
    assert_eq!(result["width"], 390);
    assert_eq!(result["height"], 844);
    assert_eq!(result["deviceScaleFactor"], 3.0);
    let requests = log.lock().unwrap();
    assert_eq!(requests[0]["method"], "Emulation.setDeviceMetricsOverride");
    assert_eq!(
        requests[0]["params"],
        json!({"width":390,"height":844,"deviceScaleFactor":3.0,"mobile":true})
    );
    assert!(state
        .session_setup
        .user_agent
        .as_deref()
        .unwrap()
        .contains("iPhone OS 15_0"));
    server.abort();
}

#[tokio::test]
async fn headless_compatibility_uses_the_supplied_browser_ua_and_propagates_failure() {
    let (state, log, server) = fixture(None).await;
    let mgr = state.browser.as_ref().unwrap();
    let ua = "Mozilla/5.0 HeadlessChrome/155.0.0.0 Safari/537.36";
    let expected = "Mozilla/5.0 Chrome/155.0.0.0 Safari/537.36";
    assert_eq!(
        stealth::apply_headless_user_agent(&mgr.client, "session", ua, None)
            .await
            .unwrap(),
        expected
    );
    assert_eq!(
        stealth::apply_headless_user_agent(&mgr.client, "session", "NativeBrowser/1.0", None)
            .await
            .unwrap(),
        "NativeBrowser/1.0"
    );
    let requests = log.lock().unwrap().clone();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0]["method"], "Emulation.setUserAgentOverride");
    assert_eq!(requests[0]["params"]["userAgent"], expected);
    assert!(requests[0]["params"].get("acceptLanguage").is_none());
    server.abort();

    let (state, _, server) = fixture(Some("Emulation.setUserAgentOverride")).await;
    let mgr = state.browser.as_ref().unwrap();
    let error = stealth::apply_headless_user_agent(&mgr.client, "session", ua, None)
        .await
        .unwrap_err();
    assert!(error.contains("fixture refused reset"));
    server.abort();
}
