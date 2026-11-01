//! Side-panel action stamping: a top-level command carrying `_panelActionId`
//! gets `actionContext {actionId, before, after}` on its reply, naming the page
//! the browser tools were pointed at before and after it ran. Everything here
//! reads fields the daemon already holds; it sends no CDP command of its own.
//!
//! A panel `observe` leaves request tracking alone. The daemon tracks requests
//! from start (`DaemonState::new_with_stream`), so there is nothing to scope;
//! what bounds that memory is the request log itself (`tracked_requests`).

use serde_json::{json, Value};

use super::actions::{execute_command_recovering, DaemonState};
use super::browser::{ActingTarget, BrowserManager};

/// What a panel command noted before it ran.
pub(crate) struct PanelStamp {
    action_id: String,
    before: Option<ActingTarget>,
}

/// Run one top-level command, stamping its reply when it carries
/// `_panelActionId`. Nested script and batch steps run below this and are
/// never stamped.
pub(crate) async fn execute_command_stamped(cmd: &Value, state: &mut DaemonState) -> Value {
    super::focus_recovery::scope(cmd, execute_command_stamped_inner(cmd, state)).await
}

async fn execute_command_stamped_inner(cmd: &Value, state: &mut DaemonState) -> Value {
    let stamp = begin(cmd, state);
    let logged = log_start(cmd);
    let mut response = execute_command_recovering(cmd, state).await;
    if let Some(entry) = logged {
        log_finish(entry, &response);
    }
    if let Some(stamp) = stamp {
        finish(stamp, cmd, &mut response, state);
    }
    response
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Record that a top-level command started, for `session last` (#687): its
/// id, action and, for a site adapter, which one. Never its arguments.
/// Liveness probes are not commands anyone asked for.
fn log_start(cmd: &Value) -> Option<(String, Value)> {
    let session = super::browser::DAEMON_SESSION.get()?;
    let id = cmd.get("id").and_then(Value::as_str)?;
    if id == "probe" {
        return None;
    }
    let mut entry = json!({
        "id": id,
        "action": cmd.get("action").and_then(Value::as_str).unwrap_or(""),
        "state": "running",
        "startedAt": now_ms(),
        "daemonPid": std::process::id(),
    });
    if let Some(spec) = cmd.get("spec").and_then(Value::as_str) {
        entry["site"] = json!(spec);
    }
    crate::connection::write_last_command(session, entry.clone());
    Some((session.clone(), entry))
}

/// Record how the command ended: completed or failed, with the error's code
/// (never its text, which can quote the page).
fn log_finish((session, mut entry): (String, Value), response: &Value) {
    let ok = response.get("success").and_then(Value::as_bool) == Some(true);
    entry["state"] = json!(if ok { "completed" } else { "failed" });
    entry["finishedAt"] = json!(now_ms());
    if !ok {
        let code = response
            .get("code")
            .and_then(Value::as_str)
            .map(str::to_string)
            .or_else(|| {
                response
                    .get("error")
                    .and_then(Value::as_str)
                    .map(|e| crate::error_envelope::classify_error(e).code.to_string())
            });
        entry["errorCode"] = json!(code);
    }
    crate::connection::write_last_command(&session, entry);
}

pub(crate) fn begin(cmd: &Value, state: &DaemonState) -> Option<PanelStamp> {
    let action_id =
        crate::connection::panel_action_id(cmd.get("_panelActionId").and_then(Value::as_str))?;
    Some(PanelStamp {
        action_id: action_id.to_string(),
        before: state
            .browser
            .as_ref()
            .and_then(BrowserManager::acting_target),
    })
}

/// Stamp the reply, on success and on error alike.
pub(crate) fn finish(
    stamp: PanelStamp,
    cmd: &Value,
    response: &mut Value,
    state: &mut DaemonState,
) {
    let action = cmd.get("action").and_then(Value::as_str).unwrap_or("");
    let (after, opened) = match state.browser.as_ref() {
        Some(mgr) => reply_targets(action, response, mgr),
        None => (None, None),
    };
    let mut context = json!({
        "actionId": stamp.action_id,
        "before": stamp.before,
        "after": after,
    });
    if let Some(opened) = opened {
        context["opened"] = json!(opened);
    }
    if let Some(obj) = response.as_object_mut() {
        obj.insert("actionContext".to_string(), context);
    }
}

/// `(after, opened)`. `after` is the page the session is on after the
/// command: from the reply (`actionReply`) when the reply names it, otherwise
/// where the session is pointed (`daemonState`). `opened` is a pop-up the
/// command adopted and did not follow.
fn reply_targets(
    action: &str,
    response: &Value,
    mgr: &BrowserManager,
) -> (Option<ActingTarget>, Option<ActingTarget>) {
    let pointed = mgr.acting_target();
    if response.get("success").and_then(Value::as_bool) != Some(true) {
        return (pointed, None);
    }
    let data = response.get("data").unwrap_or(&Value::Null);
    let reported = |key: &str| {
        data.get(key)
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
    };
    let named = |target: Option<ActingTarget>, url: Option<String>, title: Option<String>| {
        target.map(|mut t| {
            if let Some(url) = url {
                t.url = url;
            }
            t.title = title;
            t.provenance = "actionReply";
            t
        })
    };

    // A pop-up the command adopted: `openedTab` without `openedTabStatus`
    // (that one says it was not adopted, or not known to have loaded).
    let popup = data
        .get("openedTab")
        .filter(|_| data.get("openedTabStatus").is_none())
        .and_then(|tab| {
            let target = mgr.tab_target(tab.get("tabId")?.as_str()?);
            let field = |k: &str| {
                tab.get(k)
                    .and_then(Value::as_str)
                    .filter(|s| !s.is_empty())
                    .map(str::to_string)
            };
            named(target, field("url"), field("title"))
        });
    if let Some(popup) = popup {
        let followed = data.get("followed").and_then(Value::as_bool) == Some(true)
            && pointed.as_ref().map(|p| &p.target_id) == Some(&popup.target_id);
        return if followed {
            (Some(popup), None)
        } else {
            (pointed, Some(popup))
        };
    }

    match action {
        // The url the navigation landed on, after redirects.
        "navigate" | "back" | "forward" | "reload" => match reported("url") {
            Some(url) => (named(pointed, Some(url), reported("title")), None),
            None => (pointed, None),
        },
        // A switch whose liveness probe read the requested tab's url back.
        "tab_switch" | "tab_adopt" if reported("verified").as_deref() == Some("confirmed") => {
            let title = reported("title");
            (named(pointed, reported("url"), title), None)
        }
        _ => (pointed, None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::native::actions::TrackedRequest;
    use crate::native::browser::{fake_cdp, PageInfo};

    const ID: &str = "0b5c3f4e-8a7d-4c2b-9e1f-6a2d8c4b7e90";

    fn page(tab_id: u32, session_id: &str) -> PageInfo {
        PageInfo {
            tab_id,
            label: None,
            target_id: format!("T{tab_id}"),
            session_id: session_id.to_string(),
            url: format!("https://shop.example/{tab_id}"),
            title: format!("Page {tab_id}"),
            target_type: "page".to_string(),
        }
    }

    async fn state_with(pages: Vec<PageInfo>) -> (DaemonState, fake_cdp::Log) {
        let (url, log) = fake_cdp::start().await;
        let mut state = DaemonState::new();
        state.browser = Some(BrowserManager::for_test(&url, pages).await);
        (state, log)
    }

    fn request(n: usize) -> TrackedRequest {
        TrackedRequest {
            session_id: None,
            url: format!("https://shop.example/api/{n}"),
            method: "POST".into(),
            headers: json!({"authorization": "Bearer x"}),
            timestamp: n as u64,
            resource_type: "Fetch".into(),
            request_id: format!("r{n}"),
            post_data: Some("card=4242".into()),
            status: None,
            response_headers: None,
            mime_type: None,
            truncated: Vec::new(),
        }
    }

    fn stamped(cmd: Value, response: Value, state: &mut DaemonState) -> Value {
        let stamp = begin(&cmd, state).expect("a panel command");
        let mut response = response;
        finish(stamp, &cmd, &mut response, state);
        response["actionContext"].clone()
    }

    #[tokio::test]
    async fn only_a_command_with_a_panel_action_id_is_stamped() {
        let (state, _) = state_with(vec![page(1, "cb-tab-41")]).await;
        assert!(begin(&json!({"action": "click", "selector": "#a"}), &state).is_none());
        for bad in ["", "not-a-uuid", "0b5c3f4e8a7d4c2b9e1f6a2d8c4b7e90"] {
            assert!(
                begin(&json!({"action": "click", "_panelActionId": bad}), &state).is_none(),
                "{bad:?}"
            );
        }
        assert!(begin(&json!({"action": "click", "_panelActionId": ID}), &state).is_some());
    }

    /// The stamp is the same command plus field reads: run with and without
    /// `_panelActionId`, the endpoint sees the same methods.
    #[tokio::test]
    async fn stamping_adds_no_cdp_traffic() {
        let mut logs = Vec::new();
        for id in [None, Some(ID)] {
            let (mut state, log) = state_with(vec![page(1, "cb-tab-41")]).await;
            let mut cmd = json!({"id": "1", "action": "url"});
            if let Some(id) = id {
                cmd["_panelActionId"] = json!(id);
            }
            let reply = execute_command_stamped(&cmd, &mut state).await;
            assert_eq!(
                reply.get("actionContext").is_some(),
                id.is_some(),
                "{reply}"
            );
            if id.is_some() {
                let context = &reply["actionContext"];
                assert_eq!(context["actionId"], ID);
                assert_eq!(context["before"]["chromeTabId"], 41);
                assert_eq!(context["before"]["provenance"], "daemonState");
            }
            logs.push(log.lock().unwrap().clone());
        }
        assert!(!logs[0].is_empty(), "the command sent nothing to compare");
        assert_eq!(logs[0], logs[1]);
    }

    #[tokio::test]
    async fn a_navigation_names_where_it_landed() {
        let (mut state, _) = state_with(vec![page(1, "cb-tab-41")]).await;
        let context = stamped(
            json!({"action": "navigate", "url": "https://shop.example/go", "_panelActionId": ID}),
            json!({"success": true, "data": {"url": "https://shop.example/landed", "title": "Landed"}}),
            &mut state,
        );
        assert_eq!(context["before"]["url"], "https://shop.example/1");
        assert_eq!(context["before"]["provenance"], "daemonState");
        assert_eq!(context["after"]["url"], "https://shop.example/landed");
        assert_eq!(context["after"]["title"], "Landed");
        assert_eq!(context["after"]["chromeTabId"], 41);
        assert_eq!(context["after"]["provenance"], "actionReply");
        assert!(context.get("opened").is_none());

        for action in ["back", "forward", "reload"] {
            let context = stamped(
                json!({"action": action, "_panelActionId": ID}),
                json!({"success": true, "data": {"url": "https://shop.example/prev"}}),
                &mut state,
            );
            assert_eq!(context["after"]["provenance"], "actionReply", "{action}");
            assert_eq!(context["after"]["url"], "https://shop.example/prev");
            assert_eq!(context["after"]["title"], Value::Null, "{action}");
        }
    }

    #[tokio::test]
    async fn a_failed_command_is_pointed_at_never_acted_on() {
        let (mut state, _) = state_with(vec![page(1, "cb-tab-41")]).await;
        let context = stamped(
            json!({"action": "navigate", "url": "https://x.example", "_panelActionId": ID}),
            json!({"success": false, "error": "Timeout", "data": {"url": "https://x.example"}}),
            &mut state,
        );
        assert_eq!(context["after"]["provenance"], "daemonState");
        assert_eq!(context["after"]["url"], "https://shop.example/1");
    }

    #[tokio::test]
    async fn only_a_confirmed_tab_switch_is_an_action_reply() {
        let (mut state, _) = state_with(vec![page(1, "cb-tab-41")]).await;
        let context = stamped(
            json!({"action": "tab_switch", "_panelActionId": ID}),
            json!({"success": true, "data": {"verified": "confirmed", "url": "https://shop.example/1", "title": "Page 1"}}),
            &mut state,
        );
        assert_eq!(context["after"]["provenance"], "actionReply");
        let context = stamped(
            json!({"action": "tab_switch", "_panelActionId": ID}),
            json!({"success": true, "data": {"verified": "unconfirmed", "url": "https://shop.example/1"}}),
            &mut state,
        );
        assert_eq!(context["after"]["provenance"], "daemonState");
    }

    #[tokio::test]
    async fn an_adopted_popup_is_named_by_the_reply() {
        let (mut state, _) = state_with(vec![page(1, "cb-tab-41"), page(2, "cb-tab-42")]).await;
        // Not followed: the session stays on the clicked tab.
        let context = stamped(
            json!({"action": "click", "selector": "#pay", "_panelActionId": ID}),
            json!({"success": true, "data": {"clicked": "#pay",
                "openedTab": {"tabId": "t2", "url": "https://pay.example/", "title": "Pay"}}}),
            &mut state,
        );
        assert_eq!(context["after"]["targetId"], "T1");
        assert_eq!(context["after"]["provenance"], "daemonState");
        assert_eq!(context["opened"]["targetId"], "T2");
        assert_eq!(context["opened"]["chromeTabId"], 42);
        assert_eq!(context["opened"]["url"], "https://pay.example/");
        assert_eq!(context["opened"]["provenance"], "actionReply");

        // A pop-up seen but not adopted is not named at all.
        let context = stamped(
            json!({"action": "click", "selector": "#pay", "_panelActionId": ID}),
            json!({"success": true, "data": {"openedTab": {"tabId": "t2", "url": "https://pay.example/"},
                "openedTabStatus": "unknown"}}),
            &mut state,
        );
        assert!(context.get("opened").is_none());
        assert_eq!(context["after"]["provenance"], "daemonState");

        // Followed: the session is on the pop-up now.
        state.browser.as_mut().unwrap().set_active_for_test("T2");
        let context = stamped(
            json!({"action": "click", "selector": "#pay", "_panelActionId": ID}),
            json!({"success": true, "data": {"followed": true,
                "openedTab": {"tabId": "t2", "url": "https://pay.example/", "title": "Pay"}}}),
            &mut state,
        );
        assert_eq!(context["after"]["targetId"], "T2");
        assert_eq!(context["after"]["provenance"], "actionReply");
        assert!(context.get("opened").is_none());
    }

    #[tokio::test]
    async fn no_browser_means_no_targets() {
        let mut state = DaemonState::new();
        let context = stamped(
            json!({"action": "click", "_panelActionId": ID}),
            json!({"success": false, "error": "Browser not launched"}),
            &mut state,
        );
        assert_eq!(context["actionId"], ID);
        assert_eq!(context["before"], Value::Null);
        assert_eq!(context["after"], Value::Null);
    }

    /// A panel `observe` leaves tracking and the request log as the command
    /// left them: tracking is on from daemon start, so dropping the command's
    /// requests would take them from `network requests` and `expect request`.
    #[tokio::test]
    async fn panel_observe_leaves_request_tracking_alone() {
        for success in [true, false] {
            let (mut state, log) = state_with(vec![page(1, "cb-tab-41")]).await;
            state.request_tracking = true;
            state.tracked_requests.push(request(1));
            let cmd = json!({"action": "click", "selector": "#save", "observe": true, "_panelActionId": ID});
            let stamp = begin(&cmd, &state).unwrap();
            state.tracked_requests.push(request(2));
            let mut response = if success {
                json!({"success": true, "data": {}})
            } else {
                json!({"success": false, "error": "Element not found"})
            };
            finish(stamp, &cmd, &mut response, &mut state);
            assert!(state.request_tracking, "success={success}");
            let ids: Vec<_> = state
                .tracked_requests
                .iter()
                .map(|r| r.request_id.as_str())
                .collect();
            assert_eq!(ids, ["r1", "r2"], "success={success}");
            assert!(
                !log.lock().unwrap().iter().any(|m| m == "Network.disable"),
                "sent Network.disable"
            );
        }
    }
}
