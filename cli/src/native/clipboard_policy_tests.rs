//! Policy boundaries over a fake relay only; no browser or system clipboard.
use super::*;
use futures_util::{SinkExt, StreamExt};
use std::sync::Mutex;

struct RelayMode;
impl Drop for RelayMode {
    fn drop(&mut self) {
        super::super::browser::relay_for_test(false);
    }
}

async fn fixture(
    managed: Value,
) -> (
    DaemonState,
    tokio::task::JoinHandle<()>,
    Arc<Mutex<Vec<Value>>>,
    RelayMode,
) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("ws://{}", listener.local_addr().unwrap());
    let commands = Arc::new(Mutex::new(Vec::new()));
    let log = commands.clone();
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut ws = tokio_tungstenite::accept_async(stream).await.unwrap();
        while let Some(Ok(tokio_tungstenite::tungstenite::Message::Text(raw))) = ws.next().await {
            let request: Value = serde_json::from_str(&raw).unwrap();
            log.lock().unwrap().push(request.clone());
            let result = match request["method"].as_str().unwrap() {
                "ABRelay.getCapabilities" => json!({"capabilities":["sitePolicy"]}),
                "ABExt.sitePolicy" => json!({"version":1,"user":{"ask":[]},"managed":managed}),
                "ABExt.siteApproval" if request["params"]["op"] == "status" => {
                    json!({"known":true,"decision":"decline"})
                }
                "ABExt.siteApproval" => json!({"held":true}),
                "Browser.getVersion" => json!({"product":"Chrome/156.0.0.0"}),
                _ => {
                    let error = json!({"id":request["id"],"error":{"code":-32000,"message":"fixture refuses browser work"}});
                    if ws
                        .send(tokio_tungstenite::tungstenite::Message::Text(
                            error.to_string(),
                        ))
                        .await
                        .is_err()
                    {
                        break;
                    }
                    continue;
                }
            };
            if ws
                .send(tokio_tungstenite::tungstenite::Message::Text(
                    json!({"id":request["id"],"result":result}).to_string(),
                ))
                .await
                .is_err()
            {
                break;
            }
        }
    });
    super::super::browser::relay_for_test(true);
    let mode = RelayMode;
    let mut browser = BrowserManager::for_test(
        &url,
        vec![super::super::browser::PageInfo {
            tab_id: 1,
            label: None,
            target_id: "fixture".into(),
            session_id: "cb-tab-1".into(),
            url: "https://fixture.test/editor".into(),
            title: "Fixture".into(),
            target_type: "page".into(),
        }],
    )
    .await;
    browser.reown_target("fixture").unwrap();
    let mut state = DaemonState::new();
    state.policy = None;
    state.confirm_actions = None;
    state.browser = Some(browser);
    (state, server, commands, mode)
}

#[tokio::test]
async fn file_confirmation_cannot_bypass_fresh_managed_clipboard_denial() {
    let (mut state, server, commands, _mode) =
        fixture(json!({"actionPolicy":{"deny":["clipboard"]}})).await;
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("policy.json");
    fs::write(&path, r#"{"confirm":["fill"]}"#).unwrap();
    state.policy = Some(ActionPolicy::load(path.to_str().unwrap()).unwrap());
    state.site_policy_cache=Some(SitePolicyCache {at:std::time::Instant::now(),on_relay:true,extension:Some(serde_json::from_value(json!({"user":{"ask":[]},"managed":{"actionPolicy":{"allow":["fill","clipboard"]}}})).unwrap())});
    let cmd = json!({"id":"file-first","action":"fill","selector":"#editor","value":"synthetic","allowClipboardFallback":true});
    let first = execute_command(&cmd, &mut state).await;
    assert_eq!(first["data"]["confirmation_required"], true, "{first}");
    assert!(commands.lock().unwrap().is_empty());
    let result = handle_confirm(&json!({"confirmationId":"file-first"}), &mut state).await;
    let error = result.unwrap_err();
    assert!(
        error.contains("organization"),
        "{error}; {:?}",
        commands.lock().unwrap()
    );
    assert_eq!(commands.lock().unwrap().len(), 2);
    assert_eq!(commands.lock().unwrap()[1]["method"], "ABExt.sitePolicy");
    assert!(!command_approved() && !site_approved("fixture.test"));
    let denied = CONFIRMED_APPROVAL
        .scope(
            ConfirmedApproval::Site("fixture.test".into()),
            site_gate(&cmd, "fill", "already-approved", &mut state),
        )
        .await
        .unwrap();
    assert_eq!(denied["success"], false, "{denied}");
    server.abort();
}

#[tokio::test]
async fn cli_confirmation_cannot_self_approve_a_native_site_request() {
    let (mut state, server, commands, _mode) =
        fixture(json!({"actionPolicy":{"confirm":["clipboard"]}})).await;
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("policy.json");
    fs::write(&path, r#"{"confirm":["fill"]}"#).unwrap();
    state.policy = Some(ActionPolicy::load(path.to_str().unwrap()).unwrap());
    let cmd = json!({"id":"file-first","action":"fill","selector":"#editor","value":"synthetic","allowClipboardFallback":true});
    let first = execute_command(&cmd, &mut state).await;
    assert_eq!(first["data"]["confirmation_required"], true, "{first}");
    let next = handle_confirm(&json!({"confirmationId":"file-first"}), &mut state)
        .await
        .unwrap();
    assert_eq!(next["confirmation_required"], true, "{next}");
    assert_eq!(next["approval"], "user");
    assert!(next["description"].as_str().unwrap().contains("clipboard"));
    assert_ne!(next["confirmed"], true);
    let response = handle_confirm(
        &json!({"confirmationId":next["confirmation_id"],"userDecision":"once"}),
        &mut state,
    )
    .await
    .unwrap();
    assert_eq!(response["denied"], true, "{response}");
    assert!(commands
        .lock()
        .unwrap()
        .iter()
        .any(|c| c["method"] == "ABExt.siteApproval" && c["params"]["op"] == "status"));
    assert!(!command_approved() && !site_approved("fixture.test"));
    server.abort();
}

#[tokio::test]
async fn managed_allow_does_not_add_a_clipboard_confirmation() {
    let (mut state, server, commands, _mode) =
        fixture(json!({"actionPolicy":{"allow":["fill","clipboard"]}})).await;
    let cmd = json!({"id":"allowed","action":"fill","allowClipboardFallback":true});
    assert!(site_gate(&cmd, "fill", "allowed", &mut state)
        .await
        .is_none());
    assert_eq!(commands.lock().unwrap().len(), 2);
    server.abort();
}

#[tokio::test]
async fn site_approval_does_not_authorize_another_host() {
    let (mut state, server, commands, _mode) =
        fixture(json!({"actionPolicy":{"confirm":["clipboard"]}})).await;
    let cmd = json!({"id":"different-host","action":"fill","allowClipboardFallback":true});
    let response = CONFIRMED_APPROVAL
        .scope(
            ConfirmedApproval::Site("other.test".into()),
            site_gate(&cmd, "fill", "different-host", &mut state),
        )
        .await
        .unwrap();
    assert_eq!(
        response["data"]["confirmation_required"], true,
        "{response}"
    );
    assert_eq!(response["data"]["approval"], "user");
    assert_eq!(response["data"]["site"], "fixture.test");
    assert!(commands
        .lock()
        .unwrap()
        .iter()
        .all(|c| c["method"].as_str().unwrap().starts_with("AB")));
    server.abort();
}

#[tokio::test]
async fn approved_site_scope_still_enforces_domain_blocks() {
    let (mut state, server, commands, _mode) =
        fixture(json!({"blockedDomains":["fixture.test"]})).await;
    let cmd = json!({"id":"blocked-host","action":"fill","allowClipboardFallback":true});
    let response = CONFIRMED_APPROVAL
        .scope(
            ConfirmedApproval::Site("fixture.test".into()),
            site_gate(&cmd, "fill", "blocked-host", &mut state),
        )
        .await
        .unwrap();
    assert_eq!(response["success"], false, "{response}");
    assert!(response["error"].as_str().unwrap().contains("fixture.test"));
    assert!(commands
        .lock()
        .unwrap()
        .iter()
        .all(|c| c["method"].as_str().unwrap().starts_with("AB")));
    server.abort();
}
