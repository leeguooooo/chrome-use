//! Relay between the `ab-connect` browser extension and the daemon's `CdpClient`.
//!
//! The extension speaks a small CDP-over-WebSocket "envelope" protocol (adapted
//! from openclaw-browser-relay) and drives the user's real tabs via per-tab
//! `chrome.debugger`. The daemon's `CdpClient`, however, expects a **browser-
//! level** CDP endpoint (`Target.getTargets` / `Target.attachToTarget` → a
//! `sessionId`, then per-session commands). This relay bridges the two: it
//! tracks the targets the extension reports, answers the browser-level
//! `Target.*` discovery commands LOCALLY, and forwards everything else to the
//! extension as `forwardCDPCommand`. That keeps `CdpClient` and `browser.rs`
//! unchanged.
//!
//! ## Multiple clients (concurrent agents on one shared browser)
//!
//! Several chrome-use daemons (one per `--session`) can connect to the same
//! relay/Chrome at once. The extension is a single peer, so the relay must
//! demultiplex: every forwarded command is re-keyed to a relay-global id mapped
//! back to the originating client, and the extension's reply is routed to **only
//! that client** (with its original id restored). Command ids from different
//! clients therefore never collide, and one client never sees another's command
//! replies. CDP *events* (no id) fan out to all clients, which ignore events for
//! sessions they didn't attach.
//!
//! This module is the pure translation core (no I/O) so the protocol can be
//! unit-tested; the tokio WebSocket server that drives it lives alongside.

use std::collections::{HashMap, HashSet};

use serde_json::{json, Value};

/// Protocol version advertised in the connect handshake (matches the extension).
pub const RELAY_PROTOCOL: i64 = 3;

/// Identifies one connected CDP client (chrome-use daemon) for routing.
pub type ClientId = u64;

/// One target (tab) the extension has attached, as the relay tracks it.
#[derive(Clone)]
struct TargetEntry {
    session_id: String,
    target_info: Value,
}

/// Relay translation state: the targets the extension exposes, plus the
/// in-flight command map used to route extension replies back to the right
/// client.
#[derive(Default)]
pub struct RelayState {
    /// Capabilities announced by the connected extension. An older extension
    /// reports none, allowing daemons to fail unsupported operations clearly.
    extension_capabilities: HashSet<String>,
    /// targetId -> entry
    targets: HashMap<String, TargetEntry>,
    /// relay-global command id -> (client that sent it, its original id)
    pending: HashMap<i64, (ClientId, Value)>,
    /// monotonic source of relay-global command ids
    next_global_id: i64,
    /// Group-scoped isolation (issue #40). A tab group belongs to exactly one
    /// agent/session; the relay scopes `Target.getTargets` per client to its own
    /// group so the daemon can safely adopt new tabs (follow-popup, cross-session
    /// adopt) without ever seeing the user's or another agent's tabs.
    ///
    /// clientId -> group name. A client that never announced a group (older
    /// daemon) is absent here and gets the full, UNSCOPED target list — so this
    /// is fully backward-compatible.
    client_groups: HashMap<ClientId, String>,
    /// targetId -> group name. Created and duplicated tabs are tagged from the
    /// command's `agentGroup`; an explicitly adopted tab is tagged to the adopter;
    /// a pop-up inherits its opener's group.
    target_group: HashMap<String, String>,
    /// Relay-global id of an in-flight target-creating command to its
    /// `agentGroup`, so the reply's `targetId` can be tagged with that group.
    pending_target_group: HashMap<i64, String>,
    /// Sessions of OOPIF (cross-origin iframe) child targets the extension
    /// auto-attached (flat `Target.setAutoAttach`). Unlike the page-level
    /// `cb-tab-*` attaches (which the relay consumes to maintain `targets`),
    /// these are FORWARDED to clients so the daemon can build its
    /// `iframe_sessions` map and pierce cross-origin iframes (e.g. the Google
    /// GSI sign-in iframe). Tracked here so the matching `detachedFromTarget`
    /// is forwarded too — and so they're never mistaken for a tab in
    /// `getTargets` (they're not added to `targets`). (OAuth/OOPIF support)
    oopif_sessions: HashSet<String>,
}

/// What to do with a raw CDP command received from a `CdpClient`.
#[derive(Debug, PartialEq)]
pub enum ClientRoute {
    /// Answer locally; the value is a raw CDP response `{id, result}` to send
    /// back to the originating client only.
    Local(Value),
    /// Forward to the extension; the value is a `forwardCDPCommand` envelope
    /// already re-keyed to a relay-global id.
    Forward(Value),
}

/// An output the relay emits while handling an extension message.
#[derive(Debug, PartialEq)]
pub enum RelayOut {
    /// Send this raw CDP message to clients. `to = Some(id)` targets one client
    /// (a command reply); `to = None` broadcasts (a CDP event).
    ToClient { to: Option<ClientId>, msg: Value },
    /// Send this envelope message back to the extension.
    ToExt(Value),
}

impl RelayState {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn set_extension_capabilities<I, S>(&mut self, capabilities: I)
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.extension_capabilities = capabilities.into_iter().map(Into::into).collect();
    }

    /// The challenge the relay sends to the extension as soon as it connects,
    /// kicking off the connect handshake.
    pub fn connect_challenge(nonce: &str) -> Value {
        json!({ "type": "event", "event": "connect.challenge", "payload": { "nonce": nonce } })
    }

    /// A keepalive ping for the extension.
    pub fn ping() -> Value {
        json!({ "method": "ping" })
    }

    /// Forget a disconnected client's in-flight commands so its orphaned
    /// `pending` entries don't leak.
    pub fn drop_client(&mut self, client_id: ClientId) {
        let dropped: Vec<i64> = self
            .pending
            .iter()
            .filter_map(|(gid, (cid, _))| (*cid == client_id).then_some(*gid))
            .collect();
        for gid in dropped {
            self.pending_target_group.remove(&gid);
        }
        self.pending.retain(|_, (cid, _)| *cid != client_id);
        self.client_groups.remove(&client_id);
    }

    /// Route a raw CDP command `{id, method, params?, sessionId?}` from a
    /// `CdpClient`: answer browser-level `Target.*` discovery locally, forward
    /// the rest to the extension under a relay-global id keyed to `client_id`.
    pub fn route_client_command(&mut self, client_id: ClientId, raw: &Value) -> ClientRoute {
        let id = raw.get("id").cloned().unwrap_or(Value::Null);
        let method = raw.get("method").and_then(|m| m.as_str()).unwrap_or("");
        let params = raw.get("params").cloned().unwrap_or_else(|| json!({}));
        let session_id = raw.get("sessionId").and_then(|s| s.as_str());

        match method {
            // Browser-level command the daemon uses as its liveness probe
            // (`is_connection_alive` → `Browser.getVersion`). The extension only
            // speaks per-tab `chrome.debugger`, so forwarding it errors → the
            // daemon would deem the connection dead and reconnect+re-discover on
            // EVERY command, resetting the active tab (eval/screenshot drift).
            // Answer it locally so the relay connection reads as alive.
            "Browser.getVersion" => ClientRoute::Local(json!({
                "id": id,
                "result": {
                    "protocolVersion": "1.3",
                    "product": "Chrome/ab-connect-relay",
                    "revision": "",
                    "userAgent": "",
                    "jsVersion": ""
                }
            })),
            // Non-CDP control message: a daemon announces which tab group
            // (session) it owns, so getTargets can be scoped to it (issue #40).
            "ABRelay.setGroup" => {
                if let Some(g) = params.get("group").and_then(|g| g.as_str()) {
                    if !g.is_empty() {
                        self.client_groups.insert(client_id, g.to_string());
                    }
                }
                ClientRoute::Local(json!({ "id": id, "result": {} }))
            }
            "ABRelay.getCapabilities" => {
                let mut capabilities: Vec<&str> = self
                    .extension_capabilities
                    .iter()
                    .map(String::as_str)
                    .collect();
                capabilities.sort_unstable();
                ClientRoute::Local(json!({
                    "id": id,
                    "result": { "capabilities": capabilities }
                }))
            }
            // Discovery is best-effort and event-driven in real CDP; abs only
            // reads the getTargets result, so an empty ack is enough here.
            "Target.setDiscoverTargets" | "Target.setAutoAttach" => {
                ClientRoute::Local(json!({ "id": id, "result": {} }))
            }
            // Unscoped discovery for EXPLICIT cross-group adoption (`chrome-use
            // adopt`): returns every target the extension has attached, ignoring
            // group scoping, so an agent can find a specific pre-existing tab (the
            // user's, another session's) by URL/targetId and adopt it. Isolation
            // is preserved because the daemon only acts on the one tab it then
            // attaches (which the relay re-tags into the adopter's group).
            "ABRelay.getAllTargets" => {
                let infos: Vec<Value> = self
                    .targets
                    .values()
                    .map(|t| t.target_info.clone())
                    .collect();
                ClientRoute::Local(json!({ "id": id, "result": { "targetInfos": infos } }))
            }
            "Target.getTargets" => {
                // Scope to the client's own group when it announced one; an
                // un-announced (legacy) client gets the full list (back-compat).
                let scoped = self.client_groups.get(&client_id).cloned();
                let infos: Vec<Value> = self
                    .targets
                    .iter()
                    .filter(|(tid, _)| match &scoped {
                        Some(g) => self
                            .target_group
                            .get(*tid)
                            .map(|tg| tg == g)
                            .unwrap_or(false),
                        None => true,
                    })
                    .map(|(_, t)| t.target_info.clone())
                    .collect();
                ClientRoute::Local(json!({ "id": id, "result": { "targetInfos": infos } }))
            }
            "Target.attachToTarget" => {
                let target_id = params
                    .get("targetId")
                    .and_then(|t| t.as_str())
                    .unwrap_or("");
                match self.targets.get(target_id) {
                    Some(entry) => {
                        let session_id = entry.session_id.clone();
                        // Explicitly adopting a target makes it this client's
                        // (cross-session adopt, #21) — tag it into the adopter's
                        // group so it stays in that client's scoped getTargets and
                        // isn't churn-pruned.
                        if let Some(g) = self.client_groups.get(&client_id).cloned() {
                            self.target_group.insert(target_id.to_string(), g);
                        }
                        ClientRoute::Local(
                            json!({ "id": id, "result": { "sessionId": session_id } }),
                        )
                    }
                    None => ClientRoute::Local(json!({
                        "id": id,
                        "error": { "code": -32602, "message": format!("No such target {target_id}") }
                    })),
                }
            }
            // Everything else goes to the extension's chrome.debugger. Re-key the
            // id so this client's reply can be routed back unambiguously.
            _ => {
                self.next_global_id += 1;
                let gid = self.next_global_id;
                self.pending.insert(gid, (client_id, id));
                // Remember the group a target-creating command carries so its
                // reply can be attributed even when attach arrives first.
                if method == "Target.createTarget" || method == "ABExt.duplicateTab" {
                    if let Some(g) = params.get("agentGroup").and_then(|g| g.as_str()) {
                        if !g.is_empty() {
                            self.pending_target_group.insert(gid, g.to_string());
                            self.client_groups
                                .entry(client_id)
                                .or_insert_with(|| g.to_string());
                        }
                    }
                }
                ClientRoute::Forward(json!({
                    "id": gid,
                    "method": "forwardCDPCommand",
                    "params": { "method": method, "params": params, "sessionId": session_id },
                }))
            }
        }
    }

    /// Handle one decoded message from the extension. Updates target state and
    /// returns the messages to emit (routed to a client and/or back to the
    /// extension). `expected_token` is matched against the connect handshake.
    pub fn handle_ext_message(&mut self, msg: &Value, expected_token: &str) -> Vec<RelayOut> {
        // Connect handshake request from the extension.
        if msg.get("type").and_then(|t| t.as_str()) == Some("req")
            && msg.get("method").and_then(|m| m.as_str()) == Some("connect")
        {
            let id = msg.get("id").cloned().unwrap_or(Value::Null);
            let token = msg
                .get("params")
                .and_then(|p| p.get("auth"))
                .and_then(|a| a.get("token"))
                .and_then(|t| t.as_str())
                .unwrap_or("");
            let ok = !expected_token.is_empty() && token == expected_token;
            let mut res = json!({ "type": "res", "id": id, "ok": ok });
            if !ok {
                res["error"] = json!({ "message": "invalid relay token" });
            }
            return vec![RelayOut::ToExt(res)];
        }

        // Keepalive.
        if msg.get("method").and_then(|m| m.as_str()) == Some("pong") {
            return vec![];
        }

        // Response to a forwardCDPCommand we sent → route the raw CDP response
        // back to the client that issued it, with its original id restored.
        if msg.get("id").is_some()
            && (msg.get("result").is_some() || msg.get("error").is_some())
            && msg.get("method").is_none()
        {
            let gid = msg.get("id").and_then(|i| i.as_i64());
            // A target-creating reply: tag the new target with the request's
            // group, including when its attach event arrived before this reply.
            if let Some(g) = gid.and_then(|g| self.pending_target_group.remove(&g)) {
                if let Some(tid) = msg
                    .get("result")
                    .and_then(|r| r.get("targetId"))
                    .and_then(|t| t.as_str())
                {
                    self.target_group.insert(tid.to_string(), g);
                }
            }
            let (to, orig_id) = match gid.and_then(|g| self.pending.remove(&g)) {
                Some((client_id, orig)) => (Some(client_id), orig),
                // No mapping (stale/unknown id) — fall back to broadcasting with
                // whatever id the extension echoed.
                None => (None, msg.get("id").cloned().unwrap_or(Value::Null)),
            };
            let mut out = json!({ "id": orig_id });
            if let Some(r) = msg.get("result") {
                out["result"] = r.clone();
            }
            if let Some(e) = msg.get("error") {
                // CdpClient expects an error object; wrap a bare string.
                out["error"] = match e {
                    Value::String(s) => json!({ "code": -32000, "message": s }),
                    other => other.clone(),
                };
            }
            return vec![RelayOut::ToClient { to, msg: out }];
        }

        // CDP event forwarded from a tab.
        if msg.get("method").and_then(|m| m.as_str()) == Some("forwardCDPEvent") {
            let p = msg.get("params").cloned().unwrap_or_else(|| json!({}));
            let inner_method = p.get("method").and_then(|m| m.as_str()).unwrap_or("");
            let inner_params = p.get("params").cloned().unwrap_or_else(|| json!({}));
            let session_id = p.get("sessionId").and_then(|s| s.as_str());

            // Learn/forget targets from the extension's synthesized Target events.
            // We consume these to maintain state and do NOT forward them: abs
            // discovers targets by pulling getTargets, and forwarding a second
            // attachedToTarget would duplicate the one attachToTarget emits.
            match inner_method {
                "Target.attachedToTarget" => {
                    // OOPIF (cross-origin iframe) child session: the extension
                    // flat-auto-attached it. The daemon builds `iframe_sessions`
                    // from this event to snapshot INTO the iframe (so the GSI
                    // "Continue as …" button gets a ref instead of an opaque
                    // `Iframe` leaf). Forward it through instead of consuming —
                    // it's a sub-frame session, NOT a tab, so it never enters
                    // `targets`/`getTargets`. Remember the sid so its detach is
                    // forwarded too. (OAuth/OOPIF support)
                    let is_oopif = inner_params
                        .get("targetInfo")
                        .and_then(|i| i.get("type"))
                        .and_then(|t| t.as_str())
                        == Some("iframe");
                    if is_oopif {
                        if let Some(sid) = inner_params.get("sessionId").and_then(|s| s.as_str()) {
                            self.oopif_sessions.insert(sid.to_string());
                        }
                        let mut ev = json!({ "method": inner_method, "params": inner_params });
                        if let Some(sid) = session_id {
                            ev["sessionId"] = json!(sid);
                        }
                        return vec![RelayOut::ToClient { to: None, msg: ev }];
                    }
                    if let Some(info) = inner_params.get("targetInfo") {
                        if let Some(tid) = info.get("targetId").and_then(|t| t.as_str()) {
                            let sid = inner_params
                                .get("sessionId")
                                .and_then(|s| s.as_str())
                                .unwrap_or("")
                                .to_string();
                            // Attribute the tab to a group for scoping (issue #40),
                            // unless we already know it (createTarget tag). An
                            // explicit `abGroup` from the extension wins; otherwise a
                            // pop-up inherits its opener's group via `openerTargetId`.
                            if !self.target_group.contains_key(tid) {
                                if let Some(g) = info
                                    .get("abGroup")
                                    .and_then(|g| g.as_str())
                                    .filter(|g| !g.is_empty())
                                {
                                    self.target_group.insert(tid.to_string(), g.to_string());
                                } else if let Some(opener) = info
                                    .get("openerTargetId")
                                    .and_then(|o| o.as_str())
                                    .filter(|o| !o.is_empty())
                                {
                                    if let Some(g) = self.target_group.get(opener).cloned() {
                                        self.target_group.insert(tid.to_string(), g);
                                    }
                                }
                            }
                            self.targets.insert(
                                tid.to_string(),
                                TargetEntry {
                                    session_id: sid,
                                    target_info: info.clone(),
                                },
                            );
                        }
                    }
                    return vec![];
                }
                "Target.detachedFromTarget" => {
                    let gone = inner_params.get("sessionId").and_then(|s| s.as_str());
                    // An OOPIF child session went away (iframe removed / cross-
                    // process re-nav) — forward it so the daemon prunes the stale
                    // entry from `iframe_sessions`. (Pairs with the iframe attach
                    // forward above.)
                    if let Some(g) = gone {
                        if self.oopif_sessions.remove(g) {
                            let mut ev = json!({ "method": inner_method, "params": inner_params });
                            if let Some(sid) = session_id {
                                ev["sessionId"] = json!(sid);
                            }
                            return vec![RelayOut::ToClient { to: None, msg: ev }];
                        }
                    }
                    if let Some(gone) = gone {
                        let gone_tids: Vec<String> = self
                            .targets
                            .iter()
                            .filter(|(_, e)| e.session_id == gone)
                            .map(|(tid, _)| tid.clone())
                            .collect();
                        for tid in gone_tids {
                            self.target_group.remove(&tid);
                        }
                        self.targets.retain(|_, e| e.session_id != gone);
                    }
                    return vec![];
                }
                _ => {}
            }

            // Regular CDP event → fan out to all clients (each filters by the
            // sessions it attached to).
            let mut ev = json!({ "method": inner_method, "params": inner_params });
            if let Some(sid) = session_id {
                ev["sessionId"] = json!(sid);
            }
            return vec![RelayOut::ToClient { to: None, msg: ev }];
        }

        vec![]
    }

    #[cfg(test)]
    fn seed_target(&mut self, target_id: &str, session_id: &str) {
        self.targets.insert(
            target_id.to_string(),
            TargetEntry {
                session_id: session_id.to_string(),
                target_info: json!({
                    "targetId": target_id,
                    "type": "page",
                    "title": "",
                    "url": "about:blank",
                    "attached": true,
                }),
            },
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn attached_event(target_id: &str, session_id: &str) -> Value {
        json!({
            "method": "forwardCDPEvent",
            "params": {
                "sessionId": session_id,
                "method": "Target.attachedToTarget",
                "params": {
                    "sessionId": session_id,
                    "targetInfo": { "targetId": target_id, "type": "page", "url": "https://x", "title": "X" }
                }
            }
        })
    }

    #[test]
    fn learns_target_from_attached_event_and_does_not_forward_it() {
        let mut s = RelayState::new();
        let out = s.handle_ext_message(&attached_event("T1", "cb-tab-1"), "tok");
        assert!(
            out.is_empty(),
            "attachedToTarget should be consumed, not forwarded"
        );
        // Now getTargets must report it.
        let route = s.route_client_command(1, &json!({ "id": 1, "method": "Target.getTargets" }));
        match route {
            ClientRoute::Local(v) => {
                let infos = v["result"]["targetInfos"].as_array().unwrap();
                assert_eq!(infos.len(), 1);
                assert_eq!(infos[0]["targetId"], "T1");
            }
            _ => panic!("getTargets must be local"),
        }
    }

    #[test]
    fn reattach_with_same_session_restores_target() {
        // Issue #17 recovery contract. A tab's chrome.debugger session is torn
        // down (cross-process nav, SW restart, …) then re-attached. The fix has
        // the extension reuse the SAME `cb-tab-<tabId>` id across that churn, so
        // after detach+reattach the relay must expose the NEW target under the
        // SAME session — which is exactly the session the daemon is still bound
        // to, so its eval/snapshot auto-follow the new page instead of going stale.
        let mut s = RelayState::new();
        s.handle_ext_message(&attached_event("T_old", "cb-tab-42"), "tok");
        s.handle_ext_message(
            &json!({
                "method": "forwardCDPEvent",
                "params": { "method": "Target.detachedFromTarget", "params": { "sessionId": "cb-tab-42" } }
            }),
            "tok",
        );
        s.handle_ext_message(&attached_event("T_new", "cb-tab-42"), "tok");

        let route = s.route_client_command(1, &json!({ "id": 1, "method": "Target.getTargets" }));
        match route {
            ClientRoute::Local(v) => {
                let infos = v["result"]["targetInfos"].as_array().unwrap();
                assert_eq!(infos.len(), 1, "only the new target should remain");
                assert_eq!(infos[0]["targetId"], "T_new");
            }
            _ => panic!("getTargets must be local"),
        }
        // The daemon's existing session id still resolves — to the new target.
        let route = s.route_client_command(
            1,
            &json!({ "id": 2, "method": "Target.attachToTarget", "params": { "targetId": "T_new" } }),
        );
        assert_eq!(
            route,
            ClientRoute::Local(json!({ "id": 2, "result": { "sessionId": "cb-tab-42" } }))
        );
    }

    #[test]
    fn oopif_iframe_attach_is_forwarded_not_consumed() {
        // A cross-origin iframe (OOPIF) child session the extension flat-auto-
        // attached must FLOW THROUGH to the daemon (it builds iframe_sessions
        // from it), unlike a page-level `cb-tab-*` attach which is consumed.
        let mut s = RelayState::new();
        let ev = json!({
            "method": "forwardCDPEvent",
            "params": {
                "sessionId": "cb-tab-9",
                "method": "Target.attachedToTarget",
                "params": {
                    "sessionId": "OOPIF-SID",
                    "targetInfo": {
                        "targetId": "FRAME-1", "type": "iframe",
                        "url": "https://accounts.google.com/gsi/", "attached": true
                    }
                }
            }
        });
        let out = s.handle_ext_message(&ev, "tok");
        // Forwarded to all clients (broadcast), carrying the child sessionId.
        match &out[..] {
            [RelayOut::ToClient { to, msg }] => {
                assert_eq!(*to, None);
                assert_eq!(msg["method"], "Target.attachedToTarget");
                assert_eq!(msg["params"]["sessionId"], "OOPIF-SID");
                assert_eq!(msg["params"]["targetInfo"]["type"], "iframe");
            }
            _ => panic!("OOPIF attach must be forwarded as a single ToClient event"),
        }
        // It is NOT a tab — never surfaces in getTargets.
        let route = s.route_client_command(1, &json!({ "id": 1, "method": "Target.getTargets" }));
        match route {
            ClientRoute::Local(v) => {
                assert!(v["result"]["targetInfos"].as_array().unwrap().is_empty());
            }
            _ => panic!("getTargets must be local"),
        }
        // Its detach is forwarded too, so the daemon can prune iframe_sessions.
        let det = json!({
            "method": "forwardCDPEvent",
            "params": {
                "sessionId": "cb-tab-9",
                "method": "Target.detachedFromTarget",
                "params": { "sessionId": "OOPIF-SID" }
            }
        });
        let out = s.handle_ext_message(&det, "tok");
        match &out[..] {
            [RelayOut::ToClient { to, msg }] => {
                assert_eq!(*to, None);
                assert_eq!(msg["method"], "Target.detachedFromTarget");
                assert_eq!(msg["params"]["sessionId"], "OOPIF-SID");
            }
            _ => panic!("OOPIF detach must be forwarded"),
        }
    }

    #[test]
    fn browser_get_version_is_answered_locally() {
        // Liveness probe must NOT be forwarded (the extension can't do
        // browser-level commands) — else the daemon reconnects on every command.
        let mut s = RelayState::new();
        let route = s.route_client_command(1, &json!({ "id": 7, "method": "Browser.getVersion" }));
        match route {
            ClientRoute::Local(v) => {
                assert_eq!(v["id"], 7);
                assert!(v["result"]["protocolVersion"].is_string());
            }
            _ => panic!("Browser.getVersion must be answered locally"),
        }
    }

    #[test]
    fn attach_to_target_returns_known_session() {
        let mut s = RelayState::new();
        s.seed_target("T1", "cb-tab-1");
        let route = s.route_client_command(
            7,
            &json!({ "id": 5, "method": "Target.attachToTarget", "params": { "targetId": "T1", "flatten": true } }),
        );
        assert_eq!(
            route,
            ClientRoute::Local(json!({ "id": 5, "result": { "sessionId": "cb-tab-1" } }))
        );
    }

    #[test]
    fn attach_to_unknown_target_errors_locally() {
        let mut s = RelayState::new();
        let route = s.route_client_command(
            1,
            &json!({ "id": 6, "method": "Target.attachToTarget", "params": { "targetId": "nope" } }),
        );
        match route {
            ClientRoute::Local(v) => assert!(v.get("error").is_some()),
            _ => panic!("should answer locally"),
        }
    }

    #[test]
    fn other_commands_forward_under_global_id() {
        let mut s = RelayState::new();
        let route = s.route_client_command(
            42,
            &json!({ "id": 9, "method": "Page.navigate", "params": { "url": "https://x" }, "sessionId": "cb-tab-1" }),
        );
        match route {
            ClientRoute::Forward(v) => {
                assert_eq!(v["method"], "forwardCDPCommand");
                // id is re-keyed to a relay-global id (not the client's 9).
                assert_eq!(v["id"], 1);
                assert_eq!(v["params"]["method"], "Page.navigate");
                assert_eq!(v["params"]["sessionId"], "cb-tab-1");
                assert_eq!(v["params"]["params"]["url"], "https://x");
            }
            _ => panic!("Page.navigate must forward"),
        }
    }

    #[test]
    fn reply_routes_back_to_the_issuing_client_with_original_id() {
        let mut s = RelayState::new();
        // Two clients each send a command that happens to share original id 1.
        let r1 = s.route_client_command(
            100,
            &json!({ "id": 1, "method": "Page.navigate", "params": {} }),
        );
        let r2 = s.route_client_command(
            200,
            &json!({ "id": 1, "method": "Page.reload", "params": {} }),
        );
        let g1 = match r1 {
            ClientRoute::Forward(v) => v["id"].as_i64().unwrap(),
            _ => panic!(),
        };
        let g2 = match r2 {
            ClientRoute::Forward(v) => v["id"].as_i64().unwrap(),
            _ => panic!(),
        };
        assert_ne!(g1, g2, "global ids must be distinct across clients");

        // Extension replies for g2 → must go to client 200 with original id 1.
        let out = s.handle_ext_message(&json!({ "id": g2, "result": { "ok": true } }), "tok");
        assert_eq!(
            out,
            vec![RelayOut::ToClient {
                to: Some(200),
                msg: json!({ "id": 1, "result": { "ok": true } })
            }]
        );
        // And g1 → client 100.
        let out = s.handle_ext_message(&json!({ "id": g1, "result": { "ok": false } }), "tok");
        assert_eq!(
            out,
            vec![RelayOut::ToClient {
                to: Some(100),
                msg: json!({ "id": 1, "result": { "ok": false } })
            }]
        );
    }

    #[test]
    fn forward_command_error_is_wrapped_and_routed() {
        let mut s = RelayState::new();
        let r = s.route_client_command(
            5,
            &json!({ "id": 3, "method": "Page.navigate", "params": {} }),
        );
        let gid = match r {
            ClientRoute::Forward(v) => v["id"].as_i64().unwrap(),
            _ => panic!(),
        };
        let out = s.handle_ext_message(&json!({ "id": gid, "error": "boom" }), "tok");
        match &out[0] {
            RelayOut::ToClient { to, msg } => {
                assert_eq!(*to, Some(5));
                assert_eq!(msg["id"], 3);
                assert_eq!(msg["error"]["message"], "boom");
            }
            _ => panic!("expected ToClient"),
        }
    }

    #[test]
    fn regular_event_broadcasts_with_session() {
        let mut s = RelayState::new();
        let ev = json!({
            "method": "forwardCDPEvent",
            "params": { "sessionId": "cb-tab-1", "method": "Page.loadEventFired", "params": { "timestamp": 1.0 } }
        });
        let out = s.handle_ext_message(&ev, "tok");
        assert_eq!(
            out,
            vec![RelayOut::ToClient {
                to: None,
                msg: json!({
                    "method": "Page.loadEventFired",
                    "params": { "timestamp": 1.0 },
                    "sessionId": "cb-tab-1"
                })
            }]
        );
    }

    #[test]
    fn drop_client_clears_its_pending() {
        let mut s = RelayState::new();
        let r = s.route_client_command(
            9,
            &json!({ "id": 1, "method": "Page.navigate", "params": {} }),
        );
        let gid = match r {
            ClientRoute::Forward(v) => v["id"].as_i64().unwrap(),
            _ => panic!(),
        };
        s.drop_client(9);
        // Reply now has no mapping → broadcast fallback (to: None), echoed id.
        let out = s.handle_ext_message(&json!({ "id": gid, "result": {} }), "tok");
        match &out[0] {
            RelayOut::ToClient { to, .. } => assert_eq!(*to, None),
            _ => panic!(),
        }

        let duplicate = s.route_client_command(
            9,
            &json!({ "id": 2, "method": "ABExt.duplicateTab", "params": {
                "sourceTargetId": "source", "agentGroup": "agent-a"
            } }),
        );
        let duplicate_gid = match duplicate {
            ClientRoute::Forward(v) => v["id"].as_i64().unwrap(),
            _ => panic!(),
        };
        assert!(s.pending_target_group.contains_key(&duplicate_gid));
        s.drop_client(9);
        assert!(!s.pending_target_group.contains_key(&duplicate_gid));
    }

    #[test]
    fn connect_handshake_validates_token() {
        let mut s = RelayState::new();
        let req = json!({ "type": "req", "id": "c1", "method": "connect", "params": { "auth": { "token": "good" } } });
        let ok = s.handle_ext_message(&req, "good");
        assert_eq!(
            ok,
            vec![RelayOut::ToExt(
                json!({ "type": "res", "id": "c1", "ok": true })
            )]
        );

        let bad = s.handle_ext_message(&req, "different");
        match &bad[0] {
            RelayOut::ToExt(v) => {
                assert_eq!(v["ok"], false);
                assert!(v.get("error").is_some());
            }
            _ => panic!("expected ToExt"),
        }
    }

    #[test]
    fn capabilities_are_empty_until_the_extension_announces_them() {
        let mut s = RelayState::new();
        let empty =
            s.route_client_command(1, &json!({ "id": 1, "method": "ABRelay.getCapabilities" }));
        assert_eq!(
            empty,
            ClientRoute::Local(json!({
                "id": 1,
                "result": { "capabilities": [] }
            }))
        );

        s.set_extension_capabilities(["nativeTabDuplicate"]);
        let announced =
            s.route_client_command(1, &json!({ "id": 2, "method": "ABRelay.getCapabilities" }));
        assert_eq!(
            announced,
            ClientRoute::Local(json!({
                "id": 2,
                "result": { "capabilities": ["nativeTabDuplicate"] }
            }))
        );
    }

    // === Group-scoped isolation (issue #40) ===

    /// Drive the real create path: announce group, createTarget(agentGroup), feed
    /// the ext reply (tags target→group) + the attachedToTarget event (creates the
    /// entry). Returns nothing; mutates `s`.
    fn create_in_group(s: &mut RelayState, client: ClientId, group: &str, tid: &str, sid: &str) {
        s.route_client_command(
            client,
            &json!({ "id": 1, "method": "ABRelay.setGroup", "params": { "group": group } }),
        );
        let route = s.route_client_command(
            client,
            &json!({ "id": 2, "method": "Target.createTarget",
                     "params": { "url": "about:blank", "agentGroup": group } }),
        );
        let gid = match route {
            ClientRoute::Forward(env) => env["id"].as_i64().unwrap(),
            _ => panic!("createTarget must forward"),
        };
        s.handle_ext_message(&json!({ "id": gid, "result": { "targetId": tid } }), "");
        s.handle_ext_message(
            &json!({ "method": "forwardCDPEvent", "params": {
                "method": "Target.attachedToTarget",
                "params": { "sessionId": sid, "targetInfo": {
                    "targetId": tid, "type": "page", "url": "about:blank", "attached": true } } } }),
            "",
        );
    }

    #[test]
    fn duplicate_reply_attributes_target_to_requesting_group() {
        let mut s = RelayState::new();
        s.route_client_command(
            1,
            &json!({ "id": 1, "method": "ABRelay.setGroup", "params": { "group": "agent-a" } }),
        );
        let route = s.route_client_command(
            1,
            &json!({ "id": 2, "method": "ABExt.duplicateTab", "params": {
                "sourceTargetId": "source", "agentGroup": "agent-a"
            } }),
        );
        let gid = match route {
            ClientRoute::Forward(env) => env["id"].as_i64().unwrap(),
            _ => panic!("duplicateTab must forward to the extension"),
        };

        // attachTab announces the target before the extension sends the command
        // response. The response must still make the target visible to agent-a.
        s.handle_ext_message(
            &json!({ "method": "forwardCDPEvent", "params": {
                "method": "Target.attachedToTarget",
                "params": { "sessionId": "sd", "targetInfo": {
                    "targetId": "duplicate", "type": "page", "url": "https://example.com",
                    "attached": true
                } }
            } }),
            "",
        );
        assert!(get_target_ids(&mut s, 1).is_empty());

        s.handle_ext_message(
            &json!({ "id": gid, "result": {
                "sourceTargetId": "source", "targetId": "duplicate"
            } }),
            "",
        );
        assert_eq!(get_target_ids(&mut s, 1), vec!["duplicate"]);
    }

    fn get_target_ids(s: &mut RelayState, client: ClientId) -> Vec<String> {
        match s.route_client_command(client, &json!({ "id": 9, "method": "Target.getTargets" })) {
            ClientRoute::Local(v) => v["result"]["targetInfos"]
                .as_array()
                .unwrap()
                .iter()
                .map(|t| t["targetId"].as_str().unwrap().to_string())
                .collect(),
            _ => panic!("getTargets must be local"),
        }
    }

    #[test]
    fn get_targets_is_scoped_to_each_clients_group() {
        let mut s = RelayState::new();
        create_in_group(&mut s, 1, "agent-a", "ta", "sa");
        create_in_group(&mut s, 2, "agent-b", "tb", "sb");
        // Each client sees ONLY its own group's tab — never the other agent's.
        assert_eq!(get_target_ids(&mut s, 1), vec!["ta"]);
        assert_eq!(get_target_ids(&mut s, 2), vec!["tb"]);
    }

    #[test]
    fn legacy_client_without_group_sees_all_targets() {
        let mut s = RelayState::new();
        create_in_group(&mut s, 1, "agent-a", "ta", "sa");
        create_in_group(&mut s, 2, "agent-b", "tb", "sb");
        // Client 3 never announced a group → full, unscoped list (back-compat).
        let mut all = get_target_ids(&mut s, 3);
        all.sort();
        assert_eq!(all, vec!["ta", "tb"]);
    }

    #[test]
    fn popup_inherits_opener_group_and_is_visible_to_that_client_only() {
        let mut s = RelayState::new();
        create_in_group(&mut s, 1, "agent-a", "ta", "sa");
        create_in_group(&mut s, 2, "agent-b", "tb", "sb");
        // A pop-up that agent-a's tab opened: extension reports openerTargetId=ta.
        s.handle_ext_message(
            &json!({ "method": "forwardCDPEvent", "params": {
                "method": "Target.attachedToTarget",
                "params": { "sessionId": "sp", "targetInfo": {
                    "targetId": "tp", "type": "page", "url": "https://oauth.example/",
                    "attached": true, "openerTargetId": "ta" } } } }),
            "",
        );
        // Only agent-a sees the pop-up; agent-b never does.
        let mut a = get_target_ids(&mut s, 1);
        a.sort();
        assert_eq!(a, vec!["ta", "tp"]);
        assert_eq!(get_target_ids(&mut s, 2), vec!["tb"]);
    }

    #[test]
    fn explicit_attach_tags_target_into_adopter_group() {
        let mut s = RelayState::new();
        // A pre-existing, ungrouped tab the extension reported (e.g. user's tab).
        s.handle_ext_message(
            &json!({ "method": "forwardCDPEvent", "params": {
                "method": "Target.attachedToTarget",
                "params": { "sessionId": "su", "targetInfo": {
                    "targetId": "tu", "type": "page", "url": "https://user.example/", "attached": true } } } }),
            "",
        );
        // Client 1 (group agent-a) explicitly adopts it by targetId (#21).
        s.route_client_command(
            1,
            &json!({ "id": 1, "method": "ABRelay.setGroup", "params": { "group": "agent-a" } }),
        );
        s.route_client_command(
            1,
            &json!({ "id": 2, "method": "Target.attachToTarget", "params": { "targetId": "tu" } }),
        );
        // Now it's in agent-a's scope and survives the scoped getTargets.
        assert_eq!(get_target_ids(&mut s, 1), vec!["tu"]);
        // A different agent still doesn't see it.
        s.route_client_command(
            2,
            &json!({ "id": 1, "method": "ABRelay.setGroup", "params": { "group": "agent-b" } }),
        );
        assert!(get_target_ids(&mut s, 2).is_empty());
    }

    #[test]
    fn get_all_targets_is_unscoped() {
        let mut s = RelayState::new();
        create_in_group(&mut s, 1, "agent-a", "ta", "sa");
        create_in_group(&mut s, 2, "agent-b", "tb", "sb");
        // Client 1's scoped getTargets sees only its own group...
        assert_eq!(get_target_ids(&mut s, 1), vec!["ta"]);
        // ...but ABRelay.getAllTargets returns EVERY target regardless of group
        // (for explicit cross-group adoption).
        let all = match s
            .route_client_command(1, &json!({ "id": 1, "method": "ABRelay.getAllTargets" }))
        {
            ClientRoute::Local(v) => {
                let mut ids: Vec<String> = v["result"]["targetInfos"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|t| t["targetId"].as_str().unwrap().to_string())
                    .collect();
                ids.sort();
                ids
            }
            _ => panic!("getAllTargets must be local"),
        };
        assert_eq!(all, vec!["ta", "tb"]);
    }

    #[test]
    fn detach_clears_target_group() {
        let mut s = RelayState::new();
        create_in_group(&mut s, 1, "agent-a", "ta", "sa");
        assert_eq!(get_target_ids(&mut s, 1), vec!["ta"]);
        s.handle_ext_message(
            &json!({ "method": "forwardCDPEvent", "params": {
                "method": "Target.detachedFromTarget", "params": { "sessionId": "sa" } } }),
            "",
        );
        assert!(get_target_ids(&mut s, 1).is_empty());
        assert!(!s.target_group.contains_key("ta"));
    }
}
