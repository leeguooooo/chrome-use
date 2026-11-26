use super::*;
use futures_util::SinkExt;
use tokio_tungstenite::tungstenite::Message;

const PAGE_TIMEOUT: &str = "relay timeout after 8000ms: chrome.debugger.sendCommand(Page.enable). [diag in-flight=1 oldest-in-flight=8001ms worker-age=9000000ms]";

struct RelayGuard;
impl Drop for RelayGuard {
    fn drop(&mut self) {
        relay_for_test(false);
    }
}

struct Fixture {
    requests: Vec<Value>,
    initialized: bool,
    closed: bool,
    page_error: Option<String>,
    network_error: Option<String>,
    update_error: Option<String>,
    retry_fails: bool,
    late_original_reply: bool,
    original_request: Option<Value>,
    pause_method: Option<String>,
    reached: Arc<tokio::sync::Notify>,
    release: Arc<tokio::sync::Notify>,
    responded: Arc<tokio::sync::Notify>,
    record: Value,
    state: Value,
    presence: Value,
    updated: Value,
}

impl Default for Fixture {
    fn default() -> Self {
        Self {
            requests: Vec::new(),
            initialized: false,
            closed: false,
            page_error: Some(PAGE_TIMEOUT.into()),
            network_error: None,
            update_error: None,
            retry_fails: false,
            late_original_reply: false,
            original_request: None,
            pause_method: None,
            reached: Arc::new(tokio::sync::Notify::new()),
            release: Arc::new(tokio::sync::Notify::new()),
            responded: Arc::new(tokio::sync::Notify::new()),
            record: json!({
                "chromeTabId":11,"targetId":"fresh-target","sessionId":"cb-tab-11",
                "url":"","committedUrl":"","pendingUrl":"","debuggerAttached":true,
                "active":false,"discarded":false,"frozen":false
            }),
            state: json!({"ownedTabs":[11],"userTouchedTabs":[]}),
            presence: json!({
                "tabPresenceVersion":1,"targetId":"fresh-target","tabId":11,"presence":"present"
            }),
            updated: json!({"id":11,"url":"about:blank","active":false}),
        }
    }
}

impl Fixture {
    fn count(&self, method: &str) -> usize {
        self.requests
            .iter()
            .filter(|r| r["method"] == method)
            .count()
    }

    fn updates(&self) -> usize {
        self.requests
            .iter()
            .filter(|r| r["method"] == "ABExt.call" && r["params"]["method"] == "update")
            .count()
    }

    fn respond(&mut self, req: &Value) -> Vec<Value> {
        self.requests.push(req.clone());
        let method = req["method"].as_str().unwrap();
        let params = &req["params"];
        let mut error = None;
        let mut replies = Vec::new();
        let result = match method {
            "Target.createTarget" => json!({"targetId":"fresh-target"}),
            "Target.attachToTarget" => json!({"sessionId":"cb-tab-11"}),
            "Page.enable" => {
                if !self.initialized || self.retry_fails {
                    error = self.page_error.clone();
                }
                self.original_request.get_or_insert_with(|| req.clone());
                json!({})
            }
            "Network.enable" => {
                error = self.network_error.clone();
                json!({})
            }
            "ABExt.state" => self.state.clone(),
            "ABExt.inspectTab" => self.record.clone(),
            "ABExt.call" if params["namespace"] == "windows" => {
                json!({"result":[{"id":1,"type":"normal"}]})
            }
            "ABExt.call" if params["method"] == "update" => {
                assert_eq!(params["namespace"], "tabs");
                assert_eq!(params["args"], json!([11,{"url":"about:blank"}]));
                error = self.update_error.clone();
                self.initialized = error.is_none();
                if self.late_original_reply {
                    let original = self.original_request.as_ref().unwrap();
                    replies.push(
                        json!({"id":original["id"],"sessionId":original["sessionId"],"result":{}}),
                    );
                }
                json!({"result":self.updated})
            }
            "ABExt.tabPresence" if self.closed => json!({
                "tabPresenceVersion":1,"targetId":"fresh-target","tabId":11,"presence":"absent"
            }),
            "ABExt.tabPresence" => self.presence.clone(),
            "ABExt.attachedTargets" => json!({"targets":[{
                "targetId":"fresh-target","tabId":11,"sessionId":"cb-tab-11","attached":true
            }]}),
            "Target.closeTarget" => {
                self.closed = true;
                json!({"success":true})
            }
            _ => json!({}),
        };
        let mut reply = match error {
            Some(error) => json!({"id":req["id"],"error":{"code":-32000,"message":error}}),
            None => json!({"id":req["id"],"result":result}),
        };
        if let Some(sid) = req.get("sessionId") {
            reply["sessionId"] = sid.clone();
        }
        replies.push(reply);
        replies
    }
}

async fn fixture() -> (
    BrowserManager,
    Arc<std::sync::Mutex<Fixture>>,
    tokio::task::JoinHandle<()>,
) {
    relay_for_test(true);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("ws://{}", listener.local_addr().unwrap());
    let state = Arc::new(std::sync::Mutex::new(Fixture::default()));
    let shared = state.clone();
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut ws = tokio_tungstenite::accept_async(stream).await.unwrap();
        while let Some(Ok(Message::Text(text))) = ws.next().await {
            let req: Value = serde_json::from_str(&text).unwrap();
            let (replies, pause) = {
                let mut state = shared.lock().unwrap();
                let pause = (state.pause_method.as_deref() == req["method"].as_str()).then(|| {
                    (
                        state.reached.clone(),
                        state.release.clone(),
                        state.responded.clone(),
                    )
                });
                (state.respond(&req), pause)
            };
            if let Some((reached, release, _)) = &pause {
                reached.notify_one();
                release.notified().await;
            }
            for reply in replies {
                if ws.send(Message::Text(reply.to_string())).await.is_err() {
                    return;
                }
            }
            if let Some((_, _, responded)) = pause {
                responded.notify_one();
            }
        }
    });
    let browser = BrowserManager::for_test(&url, Vec::new()).await;
    (browser, state, server)
}

fn assert_recovered(browser: &BrowserManager, state: &Fixture) {
    assert_eq!(browser.pages.len(), 1);
    assert_eq!(browser.pages[0].target_id, "fresh-target");
    assert_eq!(browser.pages[0].session_id, "cb-tab-11");
    assert_eq!(state.count("Target.createTarget"), 1);
    assert_eq!(state.count("Page.enable"), 2);
    assert_eq!(state.updates(), 1);
    assert_eq!(state.count("Network.enable"), 1);
    assert_eq!(state.count("Target.activateTarget"), 0);
    assert_eq!(state.count("Page.navigate"), 0);
    assert!(!state.closed);
    let update = state
        .requests
        .iter()
        .position(|r| r["method"] == "ABExt.call" && r["params"]["method"] == "update")
        .unwrap();
    let network = state
        .requests
        .iter()
        .position(|r| r["method"] == "Network.enable")
        .unwrap();
    assert!(update < network);
}

#[tokio::test]
async fn blank_bootstrap_first_tab_recovers_before_session_setup() {
    let (mut browser, state, server) = fixture().await;
    let _guard = RelayGuard;
    let result = browser.open_first_tab().await;
    assert!(result.is_ok(), "{result:?}");
    assert_recovered(&browser, &state.lock().unwrap());
    server.abort();
}

#[tokio::test]
async fn blank_bootstrap_new_tab_recovers_same_retained_target() {
    let (mut browser, state, server) = fixture().await;
    let _guard = RelayGuard;
    let result = browser.tab_new(None, None).await;
    assert!(result.is_ok(), "{result:?}");
    assert_recovered(&browser, &state.lock().unwrap());
    server.abort();
}

#[tokio::test]
async fn blank_bootstrap_ineligible_setup_ignores_recovery_deadline() {
    for (relay, url, activate) in [
        (true, "https://example.invalid/", false),
        (false, "about:blank", false),
        (true, "about:blank", true),
    ] {
        let (browser, state, server) = fixture().await;
        let _guard = RelayGuard;
        relay_for_test(relay);
        state.lock().unwrap().page_error = None;
        browser
            .enable_new_tab_domains(
                "fresh-target",
                "cb-tab-11",
                url,
                activate,
                tokio::time::Instant::now(),
            )
            .await
            .unwrap();
        let state = state.lock().unwrap();
        assert_eq!(state.count("Page.enable"), 1);
        assert_eq!(state.count("Network.enable"), 1);
        assert_eq!(state.updates(), 0);
        server.abort();
    }
}

#[tokio::test]
async fn blank_bootstrap_healthy_target_needs_no_recovery() {
    let (mut browser, state, server) = fixture().await;
    let _guard = RelayGuard;
    state.lock().unwrap().page_error = None;
    browser.tab_new(None, None).await.unwrap();
    let state = state.lock().unwrap();
    assert_eq!(state.count("Page.enable"), 1);
    assert_eq!(state.count("Network.enable"), 1);
    assert_eq!(state.count("ABExt.state"), 0);
    assert_eq!(state.updates(), 0);
    server.abort();
}

#[tokio::test]
async fn blank_bootstrap_requires_exact_inactive_blank_owned_target() {
    let cases = [
        ("url", json!("https://example.invalid/")),
        ("committedUrl", json!("https://example.invalid/")),
        ("pendingUrl", json!("https://example.invalid/next")),
        ("pendingUrl", Value::Null),
        ("targetId", json!("another-target")),
        ("sessionId", json!("cb-tab-99")),
        ("chromeTabId", json!(99)),
        ("active", json!(true)),
        ("active", Value::Null),
        ("debuggerAttached", json!(false)),
        ("discarded", json!(true)),
        ("frozen", json!(true)),
    ];
    for (key, value) in cases {
        let (mut browser, state, server) = fixture().await;
        let _guard = RelayGuard;
        state.lock().unwrap().record[key] = value;
        assert!(browser.tab_new(None, None).await.is_err(), "{key}");
        let state = state.lock().unwrap();
        assert_eq!(state.updates(), 0, "{key}");
        assert_eq!(state.count("Page.enable"), 1, "{key}");
        assert!(
            !state.closed,
            "new-tab failure must retain its exact target"
        );
        server.abort();
    }
    for state_value in [
        json!({"ownedTabs":[],"userTouchedTabs":[]}),
        json!({"ownedTabs":[11],"userTouchedTabs":[11]}),
        json!({"ownedTabs":[11]}),
        json!({"ownedTabs":["11"],"userTouchedTabs":[]}),
        json!({"ownedTabs":[11],"userTouchedTabs":["11"]}),
    ] {
        let (mut browser, state, server) = fixture().await;
        let _guard = RelayGuard;
        state.lock().unwrap().state = state_value;
        assert!(browser.tab_new(None, None).await.is_err());
        assert_eq!(state.lock().unwrap().updates(), 0);
        server.abort();
    }
}

#[tokio::test]
async fn blank_bootstrap_requires_authoritative_target_presence() {
    for (key, value) in [
        ("tabPresenceVersion", json!(0)),
        ("targetId", json!("another-target")),
        ("tabId", json!(99)),
        ("presence", json!("unknown")),
        ("presence", json!("absent")),
    ] {
        let (mut browser, state, server) = fixture().await;
        let _guard = RelayGuard;
        state.lock().unwrap().presence[key] = value;
        assert!(browser.tab_new(None, None).await.is_err(), "{key}");
        assert_eq!(state.lock().unwrap().updates(), 0, "{key}");
        server.abort();
    }
}

#[tokio::test]
async fn blank_bootstrap_never_recovers_nonblank_or_adopted_targets() {
    let (mut browser, state, server) = fixture().await;
    let _guard = RelayGuard;
    assert!(browser
        .tab_new(Some("https://example.invalid/"), None)
        .await
        .is_err());
    assert_eq!(state.lock().unwrap().updates(), 0);
    server.abort();

    let (mut browser, state, server) = fixture().await;
    let _guard = RelayGuard;
    browser.adopted_targets.insert("fresh-target".into());
    assert!(browser.enable_domains("cb-tab-11").await.is_err());
    assert_eq!(state.lock().unwrap().updates(), 0);
    assert_eq!(state.lock().unwrap().count("ABExt.state"), 0);
    server.abort();
}

#[tokio::test]
async fn blank_bootstrap_never_retries_unrelated_errors() {
    for error in [
        "debugger access denied",
        "permission denied: relay timeout after 8000ms: chrome.debugger.sendCommand(Page.enable).",
        "relay timeout after 8000ms: chrome.debugger.sendCommand(Runtime.evaluate).",
    ] {
        let (mut browser, state, server) = fixture().await;
        let _guard = RelayGuard;
        state.lock().unwrap().page_error = Some(error.into());
        assert!(browser.tab_new(None, None).await.is_err());
        assert_eq!(state.lock().unwrap().updates(), 0);
        assert_eq!(state.lock().unwrap().count("Page.enable"), 1);
        server.abort();
    }
    let (mut browser, state, server) = fixture().await;
    let _guard = RelayGuard;
    {
        let mut state = state.lock().unwrap();
        state.page_error = None;
        state.network_error = Some(PAGE_TIMEOUT.into());
    }
    assert!(browser.tab_new(None, None).await.is_err());
    assert_eq!(state.lock().unwrap().updates(), 0);
    server.abort();
}

#[tokio::test]
async fn blank_bootstrap_browser_update_failure_preserves_cleanup_contracts() {
    for first in [false, true] {
        let (mut browser, state, server) = fixture().await;
        let _guard = RelayGuard;
        state.lock().unwrap().update_error = Some("browser navigation refused".into());
        let result = if first {
            browser.open_first_tab().await
        } else {
            browser.tab_new(None, None).await.map(|_| ())
        };
        let error = result.unwrap_err();
        assert!(error.contains("Page.enable"), "{error}");
        assert!(error.contains("browser navigation refused"), "{error}");
        let state = state.lock().unwrap();
        assert_eq!(state.updates(), 1);
        assert_eq!(state.count("Page.enable"), 1);
        assert_eq!(state.closed, first);
        server.abort();
    }
}

#[tokio::test]
async fn blank_bootstrap_retries_only_once() {
    let (mut browser, state, server) = fixture().await;
    let _guard = RelayGuard;
    state.lock().unwrap().retry_fails = true;
    assert!(browser.tab_new(None, None).await.is_err());
    let state = state.lock().unwrap();
    assert_eq!(state.updates(), 1);
    assert_eq!(state.count("Page.enable"), 2);
    assert_eq!(state.count("Network.enable"), 0);
    assert!(!state.closed);
    server.abort();
}

#[tokio::test]
async fn blank_bootstrap_late_original_reply_does_not_complete_the_retry() {
    let (mut browser, state, server) = fixture().await;
    let _guard = RelayGuard;
    state.lock().unwrap().late_original_reply = true;
    browser.tab_new(None, None).await.unwrap();
    assert_recovered(&browser, &state.lock().unwrap());
    server.abort();
}

#[tokio::test]
async fn blank_bootstrap_rejects_unconfirmed_browser_update() {
    for (key, value) in [
        ("id", json!(99)),
        ("active", json!(true)),
        ("url", Value::Null),
        ("pendingUrl", json!("https://example.invalid/")),
    ] {
        let (mut browser, state, server) = fixture().await;
        let _guard = RelayGuard;
        state.lock().unwrap().updated[key] = value;
        assert!(browser.tab_new(None, None).await.is_err(), "{key}");
        let state = state.lock().unwrap();
        assert_eq!(state.updates(), 1, "{key}");
        assert_eq!(state.count("Page.enable"), 1, "{key}");
        assert!(!state.closed);
        server.abort();
    }
}

#[tokio::test]
async fn blank_bootstrap_direct_cdp_never_uses_browser_recovery() {
    let (mut browser, state, server) = fixture().await;
    let _guard = RelayGuard;
    relay_for_test(false);
    assert!(browser.tab_new(None, None).await.is_err());
    let state = state.lock().unwrap();
    assert_eq!(state.updates(), 0);
    assert_eq!(state.count("ABExt.state"), 0);
    assert_eq!(state.count("Page.enable"), 1);
    server.abort();
}

#[tokio::test]
async fn blank_bootstrap_cancellation_never_navigates_after_late_metadata() {
    let (mut browser, state, server) = fixture().await;
    let _guard = RelayGuard;
    let (reached, release, responded) = {
        let mut state = state.lock().unwrap();
        state.pause_method = Some("ABExt.state".into());
        (
            state.reached.clone(),
            state.release.clone(),
            state.responded.clone(),
        )
    };
    let mut setup = Box::pin(browser.tab_new(None, None));
    tokio::select! {
        result = &mut setup => panic!("setup finished before paused metadata: {result:?}"),
        _ = reached.notified() => {},
    }
    drop(setup);
    release.notify_one();
    responded.notified().await;
    browser
        .client
        .send_command_no_params("Browser.getVersion", None)
        .await
        .unwrap();
    assert_eq!(state.lock().unwrap().updates(), 0);
    assert_eq!(state.lock().unwrap().count("Page.enable"), 1);
    server.abort();
}

#[tokio::test]
async fn blank_bootstrap_deadline_never_recovers_an_unowned_or_expired_target() {
    let (mut browser, state, server) = fixture().await;
    let _guard = RelayGuard;
    let result = browser
        .enable_created_domains(
            "fresh-target",
            "cb-tab-11",
            "about:blank",
            tokio::time::Instant::now(),
        )
        .await;
    assert!(result.is_err());
    assert!(state.lock().unwrap().requests.is_empty());
    let result = browser
        .enable_created_domains(
            "fresh-target",
            "cb-tab-11",
            "about:blank",
            tokio::time::Instant::now() + Duration::from_secs(1),
        )
        .await;
    assert!(result.is_err());
    assert_eq!(state.lock().unwrap().updates(), 0);
    assert_eq!(state.lock().unwrap().count("ABExt.state"), 0);

    browser.created_targets.insert("fresh-target".into());
    browser.adopted_targets.insert("fresh-target".into());
    let result = browser
        .enable_created_domains(
            "fresh-target",
            "cb-tab-11",
            "about:blank",
            tokio::time::Instant::now() + Duration::from_secs(1),
        )
        .await;
    assert!(result.is_err());
    assert_eq!(state.lock().unwrap().count("ABExt.state"), 0);
    browser.adopted_targets.clear();
    server.abort();
}
