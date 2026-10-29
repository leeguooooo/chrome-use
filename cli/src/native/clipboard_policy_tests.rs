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
    fixture_with_site(managed, "fixture.test", "decline").await
}

async fn fixture_with_site(
    managed: Value,
    host: &str,
    decision: &str,
) -> (
    DaemonState,
    tokio::task::JoinHandle<()>,
    Arc<Mutex<Vec<Value>>>,
    RelayMode,
) {
    fixture_with_fields(managed, host, decision, HashMap::new()).await
}

async fn fixture_with_fields(
    managed: Value,
    host: &str,
    decision: &str,
    fields: HashMap<String, Value>,
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
    let decision = decision.to_string();
    let renderer_url = format!("https://{host}/editor");
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
                    json!({"known":true,"decision":decision})
                }
                "ABExt.siteApproval" => json!({"held":true}),
                "Page.getFrameTree" => {
                    json!({"frameTree":{"frame":{"id":"root","url":renderer_url}}})
                }
                "DOM.getDocument"
                    if fields.contains_key(request["sessionId"].as_str().unwrap_or("")) =>
                {
                    json!({"root":{"nodeId":1}})
                }
                "DOM.querySelector"
                    if fields.contains_key(request["sessionId"].as_str().unwrap_or("")) =>
                {
                    json!({"nodeId":2})
                }
                "DOM.describeNode"
                    if fields.contains_key(request["sessionId"].as_str().unwrap_or("")) =>
                {
                    json!({"node":{"attributes":fields[request["sessionId"].as_str().unwrap()]}})
                }
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
            url: format!("https://{host}/editor"),
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
    assert_eq!(commands.lock().unwrap().len(), 3);
    assert_eq!(commands.lock().unwrap()[2]["method"], "ABExt.sitePolicy");
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
    assert!(commands.lock().unwrap().iter().all(|c| c["method"]
        .as_str()
        .unwrap()
        .starts_with("AB")
        || c["method"] == "Page.getFrameTree"));
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
    assert!(commands.lock().unwrap().iter().all(|c| c["method"]
        .as_str()
        .unwrap()
        .starts_with("AB")
        || c["method"] == "Page.getFrameTree"));
    server.abort();
}

#[tokio::test]
async fn approved_site_cannot_suppress_file_confirmation_on_a_different_allowed_host() {
    let (mut state, server, commands, _mode) = fixture_with_site(
        json!({"actionPolicy":{"allow":["fill","clipboard"]}}),
        "other.test",
        "once",
    )
    .await;
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("policy.json");
    fs::write(&path, r#"{"confirm":["fill"]}"#).unwrap();
    state.policy = Some(ActionPolicy::load(path.to_str().unwrap()).unwrap());
    state.pending_confirmation = Some(PendingConfirmation {
        action: "fill".into(),
        target: Some(ConfirmationTarget {
            target_id: "fixture".into(),
            origin: "https://fixture.test".into(),
        }),
        cmd: json!({"id":"original","action":"fill","selector":"#editor","value":"synthetic","allowClipboardFallback":true}),
        site: Some(SitePending {
            id: "site-a".into(),
            host: "fixture.test".into(),
            category: "typing".into(),
            reason: "managed_action".into(),
            user_approval: true,
        }),
    });
    let result = handle_confirm(&json!({"confirmationId":"site-a"}), &mut state).await;
    println!(
        "result={result:?}; methods={:?}",
        commands
            .lock()
            .unwrap()
            .iter()
            .map(|r| r["method"].clone())
            .collect::<Vec<_>>()
    );
    let safely_stopped = match &result {
        Ok(value) => value["confirmation_required"] == true,
        Err(error) => {
            error.contains("host changed")
                || error.contains("origin changed")
                || error.contains("tab changed")
        }
    };
    assert!(
        safely_stopped,
        "native approval for fixture.test must not approve fill on allowed other.test: {result:?}"
    );
    server.abort();
}

#[tokio::test]
async fn pending_css_confirmation_refuses_a_different_target_before_renderer_work() {
    let (mut state, server, commands, _mode) = fixture(json!({})).await;
    state.confirm_actions = Some(ConfirmActions {
        categories: ["fill".into()].into_iter().collect(),
    });
    let cmd = json!({"id":"bound","action":"fill","selector":"#editor","value":"synthetic"});
    let first = execute_command(&cmd, &mut state).await;
    assert_eq!(first["data"]["confirmation_required"], true, "{first}");
    let browser = state.browser.as_mut().unwrap();
    browser.add_background_page(super::super::browser::PageInfo {
        tab_id: 2,
        label: None,
        target_id: "other".into(),
        session_id: "cb-tab-2".into(),
        url: "https://fixture.test/editor".into(),
        title: "Other".into(),
        target_type: "page".into(),
    });
    browser.reown_target("other").unwrap();
    browser.set_active_for_test("other");
    let error = handle_confirm(&json!({"confirmationId":"bound"}), &mut state)
        .await
        .unwrap_err();
    assert!(error.contains("target or origin changed"), "{error}");
    assert!(commands.lock().unwrap().is_empty());
    assert!(state.pending_confirmation.is_none());
    assert!(!command_approved());
    server.abort();
}

#[tokio::test]
async fn pending_css_confirmation_checks_live_renderer_origin_not_cached_url() {
    let (mut state, server, commands, _mode) =
        fixture_with_site(json!({}), "other.test", "once").await;
    state.confirm_actions = Some(ConfirmActions {
        categories: ["fill".into()].into_iter().collect(),
    });
    // The daemon snapshot still describes A while the renderer now reports B.
    state.browser.as_mut().unwrap().update_page_target_info(&serde_json::from_value(json!({"targetId":"fixture","url":"https://fixture.test/editor","title":"Fixture","type":"page"})).unwrap());
    let cmd = json!({"id":"origin-bound","action":"fill","selector":"#editor","value":"synthetic"});
    let first = execute_command(&cmd, &mut state).await;
    assert_eq!(first["data"]["confirmation_required"], true, "{first}");
    let error = handle_confirm(&json!({"confirmationId":"origin-bound"}), &mut state)
        .await
        .unwrap_err();
    assert!(error.contains("target or origin changed"), "{error}");
    let log = commands.lock().unwrap();
    assert_eq!(log.len(), 1);
    assert_eq!(log[0]["method"], "Page.getFrameTree");
    server.abort();
}

#[tokio::test]
async fn explicit_pending_tab_keeps_its_original_owned_target_when_current_tab_changes() {
    let (mut state, server, commands, _mode) = fixture(json!({})).await;
    state.confirm_actions = Some(ConfirmActions {
        categories: ["fill".into()].into_iter().collect(),
    });
    let cmd =
        json!({"id":"named","action":"fill","tab":"t1","selector":"#editor","value":"synthetic"});
    let first = execute_command(&cmd, &mut state).await;
    assert_eq!(first["data"]["confirmation_required"], true, "{first}");
    let browser = state.browser.as_mut().unwrap();
    browser.add_background_page(super::super::browser::PageInfo {
        tab_id: 2,
        label: None,
        target_id: "other".into(),
        session_id: "cb-tab-2".into(),
        url: "https://other.test/editor".into(),
        title: "Other".into(),
        target_type: "page".into(),
    });
    browser.reown_target("other").unwrap();
    browser.set_active_for_test("other");
    validate_confirmation_target(state.pending_confirmation.as_ref().unwrap(), &state)
        .await
        .unwrap();
    assert_eq!(
        state.browser.as_ref().unwrap().active_target_id().unwrap(),
        "other"
    );
    let log = commands.lock().unwrap();
    assert_eq!(log.len(), 1);
    assert_eq!(log[0]["sessionId"], "cb-tab-1");
    assert_eq!(log[0]["method"], "Page.getFrameTree");
    server.abort();
}

#[tokio::test]
async fn named_tab_site_prompt_uses_intended_origin_instead_of_current_tab() {
    let (mut state, server, _commands, _mode) =
        fixture(json!({"actionPolicy":{"confirm":["clipboard"]}})).await;
    let browser = state.browser.as_mut().unwrap();
    browser.add_background_page(super::super::browser::PageInfo {
        tab_id: 2,
        label: None,
        target_id: "other".into(),
        session_id: "cb-tab-2".into(),
        url: "https://other.test/editor".into(),
        title: "Other".into(),
        target_type: "page".into(),
    });
    browser.reown_target("other").unwrap();
    browser.set_active_for_test("other");
    let cmd = json!({"id":"named-site","action":"fill","tab":"t1","selector":"#editor","value":"synthetic","allowClipboardFallback":true});
    let gate = site_gate(&cmd, "fill", "named-site", &mut state)
        .await
        .unwrap();
    assert_eq!(gate["data"]["site"], "fixture.test", "{gate}");
    assert_eq!(
        state
            .pending_confirmation
            .as_ref()
            .unwrap()
            .target
            .as_ref()
            .unwrap()
            .target_id,
        "fixture"
    );
    assert_eq!(
        state.browser.as_ref().unwrap().active_target_id().unwrap(),
        "other"
    );
    server.abort();
}

#[tokio::test]
async fn actual_native_site_approval_cannot_move_a_css_command_to_an_allowed_host() {
    let (mut state, server, commands, _mode) = fixture_with_site(
        json!({"actionPolicy":{"confirm":["clipboard"]}}),
        "fixture.test",
        "once",
    )
    .await;
    let cmd = json!({"id":"native-bound","action":"fill","selector":"#editor","value":"synthetic","allowClipboardFallback":true});
    let gate = site_gate(&cmd, "fill", "native-bound", &mut state)
        .await
        .unwrap();
    assert_eq!(gate["data"]["approval"], "user", "{gate}");
    let browser = state.browser.as_mut().unwrap();
    browser.add_background_page(super::super::browser::PageInfo {
        tab_id: 2,
        label: None,
        target_id: "other".into(),
        session_id: "cb-tab-2".into(),
        url: "https://other.test/editor".into(),
        title: "Other".into(),
        target_type: "page".into(),
    });
    browser.reown_target("other").unwrap();
    browser.set_active_for_test("other");
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("policy.json");
    fs::write(&path, r#"{"confirm":["fill"]}"#).unwrap();
    state.policy = Some(ActionPolicy::load(path.to_str().unwrap()).unwrap());
    // B is otherwise allowed. Approval for A must still stop before any
    // renderer work on B or any reuse of the local file confirmation grant.
    state.site_session_allow.push("other.test".into());
    let before = commands.lock().unwrap().len();
    let error = handle_confirm(
        &json!({"confirmationId":gate["data"]["confirmation_id"]}),
        &mut state,
    )
    .await
    .unwrap_err();
    assert!(error.contains("target or origin changed"), "{error}");
    let log = commands.lock().unwrap();
    assert_eq!(log.len(), before + 1);
    assert_eq!(log.last().unwrap()["method"], "ABExt.siteApproval");
    assert_eq!(log.last().unwrap()["params"]["op"], "status");
    assert!(state.pending_confirmation.is_none());
    assert!(!command_approved());
    server.abort();
}

#[tokio::test]
async fn confirmation_refuses_a_target_released_while_pending() {
    let (mut state, server, commands, _mode) = fixture(json!({})).await;
    state.confirm_actions = Some(ConfirmActions {
        categories: ["fill".into()].into_iter().collect(),
    });
    let cmd = json!({"id":"released","action":"fill","selector":"#editor","value":"synthetic"});
    let first = execute_command(&cmd, &mut state).await;
    assert_eq!(first["data"]["confirmation_required"], true, "{first}");
    state
        .browser
        .as_mut()
        .unwrap()
        .unown_target("fixture")
        .unwrap();
    let error = handle_confirm(&json!({"confirmationId":"released"}), &mut state)
        .await
        .unwrap_err();
    assert!(error.contains("target or origin changed"), "{error}");
    assert!(commands.lock().unwrap().is_empty());
    server.abort();
}

fn select_other_fixture_page(state: &mut DaemonState) {
    let browser = state.browser.as_mut().unwrap();
    browser.add_background_page(super::super::browser::PageInfo {
        tab_id: 2,
        label: None,
        target_id: "other".into(),
        session_id: "cb-tab-2".into(),
        url: "https://other.test/editor".into(),
        title: "Other".into(),
        target_type: "page".into(),
    });
    browser.reown_target("other").unwrap();
    browser.set_active_for_test("other");
}

fn field_policy() -> Value {
    json!({"ask":["password_input","payment_input"],"sensitiveSites":false,"firstVisit":[]})
}

#[tokio::test]
async fn named_css_field_policy_reads_only_the_intended_renderer_in_both_directions() {
    for (a, b, expected) in [
        (
            json!(["type", "password"]),
            json!(["type", "text"]),
            Some("password_input"),
        ),
        (
            json!(["autocomplete", "cc-number"]),
            json!(["type", "text"]),
            Some("payment_input"),
        ),
        (json!(["type", "text"]), json!(["type", "password"]), None),
        (
            json!(["type", "text"]),
            json!(["autocomplete", "cc-number"]),
            None,
        ),
    ] {
        let (mut state, server, commands, _mode) = fixture_with_fields(
            field_policy(),
            "fixture.test",
            "once",
            [("cb-tab-1".into(), a), ("cb-tab-2".into(), b)]
                .into_iter()
                .collect(),
        )
        .await;
        select_other_fixture_page(&mut state);
        let cmd = json!({"id":"field","action":"fill","tab":"t1","selector":"#field","value":"synthetic"});
        let gate = site_gate(&cmd, "fill", "field", &mut state).await;
        match expected {
            Some(kind) => {
                let gate = gate.unwrap();
                assert_eq!(gate["data"]["site"], "fixture.test", "{gate}");
                assert_eq!(gate["data"]["reason"], kind, "{gate}");
                assert_eq!(gate["data"]["approval"], "user");
            }
            None => assert!(gate.is_none(), "{gate:?}"),
        }
        let log = commands.lock().unwrap();
        let reads: Vec<_> = log
            .iter()
            .filter(|r| r["method"].as_str().unwrap().starts_with("DOM."))
            .collect();
        assert_eq!(reads.len(), 3);
        assert!(
            reads.iter().all(|r| r["sessionId"] == "cb-tab-1"),
            "{reads:?}"
        );
        assert_eq!(
            state.browser.as_ref().unwrap().active_target_id().unwrap(),
            "other"
        );
        server.abort();
    }
}

#[tokio::test]
async fn named_ref_field_policy_uses_saved_target_map_and_iframe_session() {
    for (a, b, expected) in [
        (
            json!(["type", "password"]),
            json!(["type", "text"]),
            Some("password_input"),
        ),
        (
            json!(["autocomplete", "cc-number"]),
            json!(["type", "text"]),
            Some("payment_input"),
        ),
        (json!(["type", "text"]), json!(["type", "password"]), None),
    ] {
        let (mut state, server, commands, _mode) = fixture_with_fields(
            field_policy(),
            "fixture.test",
            "once",
            [("frame-a".into(), a), ("cb-tab-2".into(), b)]
                .into_iter()
                .collect(),
        )
        .await;
        let mut refs = RefMap::new();
        refs.add_with_frame(
            "e1".into(),
            Some(41),
            "textbox",
            "ordinary",
            None,
            Some("child-a"),
        );
        state.tab_states.insert(
            "fixture".into(),
            TabState {
                ref_map: refs,
                last_snapshot: None,
                iframe_sessions: [("child-a".into(), "frame-a".into())].into_iter().collect(),
                active_frame_id: None,
            },
        );
        select_other_fixture_page(&mut state);
        state.ref_map.add(
            "e1".into(),
            Some(99),
            "textbox",
            "payment card number",
            None,
        );
        let cmd = json!({"id":"ref-field","action":"fill","tab":"t1","selector":"@e1","value":"synthetic"});
        let gate = site_gate(&cmd, "fill", "ref-field", &mut state).await;
        match expected {
            Some(kind) => {
                let gate = gate.unwrap();
                assert_eq!(gate["data"]["reason"], kind, "{gate}");
                assert_eq!(gate["data"]["site"], "fixture.test");
            }
            None => assert!(gate.is_none(), "{gate:?}"),
        }
        let log = commands.lock().unwrap();
        let reads: Vec<_> = log
            .iter()
            .filter(|r| r["method"].as_str().unwrap().starts_with("DOM."))
            .collect();
        assert_eq!(reads.len(), 1);
        assert_eq!(reads[0]["sessionId"], "frame-a");
        assert_eq!(reads[0]["params"]["backendNodeId"], 41);
        assert_eq!(
            state.browser.as_ref().unwrap().active_target_id().unwrap(),
            "other"
        );
        server.abort();
    }
}

#[tokio::test]
async fn missing_or_unreadable_saved_ref_context_refuses_without_current_tab_fallback() {
    for saved in [false, true] {
        let (mut state, server, commands, _mode) = fixture_with_fields(
            field_policy(),
            "fixture.test",
            "once",
            [("cb-tab-2".into(), json!(["type", "text"]))]
                .into_iter()
                .collect(),
        )
        .await;
        if saved {
            let mut refs = RefMap::new();
            refs.add_with_frame(
                "e1".into(),
                Some(41),
                "textbox",
                "ordinary",
                None,
                Some("child-a"),
            );
            state.tab_states.insert(
                "fixture".into(),
                TabState {
                    ref_map: refs,
                    last_snapshot: None,
                    iframe_sessions: [("child-a".into(), "lost-frame-a".into())]
                        .into_iter()
                        .collect(),
                    active_frame_id: None,
                },
            );
        }
        select_other_fixture_page(&mut state);
        state
            .ref_map
            .add("e1".into(), Some(99), "textbox", "ordinary", None);
        let cmd = json!({"id":"missing-ref","action":"fill","tab":"t1","selector":"@e1","value":"synthetic"});
        let gate = site_gate(&cmd, "fill", "missing-ref", &mut state)
            .await
            .unwrap();
        assert_eq!(gate["success"], false, "{gate}");
        assert!(gate["error"]
            .as_str()
            .unwrap()
            .contains("Cannot classify the intended field"));
        let log = commands.lock().unwrap();
        assert!(log.iter().all(|r| r["sessionId"] != "cb-tab-2"), "{log:?}");
        assert!(!log.iter().any(|r| r["method"] == "ABExt.siteApproval"));
        assert!(state.pending_confirmation.is_none());
        server.abort();
    }
}

#[tokio::test]
async fn managed_domain_denial_precedes_intended_field_reads() {
    let (mut state, server, commands, _mode) =
        fixture(json!({"blockedDomains":["fixture.test"],"ask":["password_input"]})).await;
    let cmd = json!({"id":"blocked-field","action":"fill","selector":"#field","value":"synthetic"});
    let gate = site_gate(&cmd, "fill", "blocked-field", &mut state)
        .await
        .unwrap();
    assert_eq!(gate["success"], false, "{gate}");
    assert!(gate["error"].as_str().unwrap().contains("blocked"));
    assert!(commands
        .lock()
        .unwrap()
        .iter()
        .all(|r| r["method"].as_str().unwrap().starts_with("AB")));
    server.abort();
}

#[tokio::test]
async fn field_policy_read_failure_preserves_kept_ref_no_recovery_contract() {
    for fields in [
        HashMap::new(),
        [("cb-tab-1".into(), Value::Null)].into_iter().collect(),
    ] {
        let (mut state, server, commands, _mode) =
            fixture_with_fields(field_policy(), "fixture.test", "once", fields).await;
        state
            .ref_map
            .add("e1".into(), Some(41), "textbox", "ordinary", None);
        state.ref_map.keep_after_failed_capture(
            super::super::element::DocumentIdentity::default(),
            "post-action capture denied (debugger_access_denied)",
        );
        let cmd = json!({"id":"kept-field","action":"fill","selector":"@e1","value":"synthetic"});
        let gate = site_gate(&cmd, "fill", "kept-field", &mut state)
            .await
            .unwrap();
        assert_eq!(gate["success"], false, "{gate}");
        assert_eq!(gate["code"], "kept_ref_unverified", "{gate}");
        assert_eq!(gate["retryable"], false);
        let error = gate["error"].as_str().unwrap();
        assert!(error.contains("debugger_access_denied"), "{error}");
        assert!(error.contains("Nothing was acted on"), "{error}");
        assert!(commands.lock().unwrap().iter().all(|r| r["method"]
            .as_str()
            .unwrap()
            .starts_with("AB")
            || r["method"] == "DOM.describeNode"));
        server.abort();
    }
}
