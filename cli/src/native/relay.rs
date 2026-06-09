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
//! This module is the pure translation core (no I/O) so the protocol can be
//! unit-tested; the tokio WebSocket server that drives it lives alongside.

use std::collections::HashMap;

use serde_json::{json, Value};

/// Protocol version advertised in the connect handshake (matches the extension).
pub const RELAY_PROTOCOL: i64 = 3;

/// One target (tab) the extension has attached, as the relay tracks it.
#[derive(Clone)]
struct TargetEntry {
    session_id: String,
    target_info: Value,
}

/// Relay translation state: the set of targets the extension currently exposes.
#[derive(Default)]
pub struct RelayState {
    /// targetId -> entry
    targets: HashMap<String, TargetEntry>,
}

/// What to do with a raw CDP command received from the `CdpClient`.
#[derive(Debug, PartialEq)]
pub enum ClientRoute {
    /// Answer locally; the value is a raw CDP response `{id, result}`.
    Local(Value),
    /// Forward to the extension; the value is a `forwardCDPCommand` envelope.
    Forward(Value),
}

/// An output the relay emits while handling an extension message.
#[derive(Debug, PartialEq)]
pub enum RelayOut {
    /// Send this raw CDP message to the `CdpClient`.
    ToClient(Value),
    /// Send this envelope message back to the extension.
    ToExt(Value),
}

impl RelayState {
    pub fn new() -> Self {
        Self::default()
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

    /// Route a raw CDP command `{id, method, params?, sessionId?}` from the
    /// `CdpClient`: answer browser-level `Target.*` discovery locally, forward
    /// the rest to the extension.
    pub fn route_client_command(&self, raw: &Value) -> ClientRoute {
        let id = raw.get("id").cloned().unwrap_or(Value::Null);
        let method = raw.get("method").and_then(|m| m.as_str()).unwrap_or("");
        let params = raw.get("params").cloned().unwrap_or_else(|| json!({}));
        let session_id = raw.get("sessionId").and_then(|s| s.as_str());

        match method {
            // Discovery is best-effort and event-driven in real CDP; abs only
            // reads the getTargets result, so an empty ack is enough here.
            "Target.setDiscoverTargets" | "Target.setAutoAttach" => {
                ClientRoute::Local(json!({ "id": id, "result": {} }))
            }
            "Target.getTargets" => {
                let infos: Vec<Value> =
                    self.targets.values().map(|t| t.target_info.clone()).collect();
                ClientRoute::Local(json!({ "id": id, "result": { "targetInfos": infos } }))
            }
            "Target.attachToTarget" => {
                let target_id = params.get("targetId").and_then(|t| t.as_str()).unwrap_or("");
                match self.targets.get(target_id) {
                    Some(entry) => ClientRoute::Local(
                        json!({ "id": id, "result": { "sessionId": entry.session_id } }),
                    ),
                    None => ClientRoute::Local(json!({
                        "id": id,
                        "error": { "code": -32602, "message": format!("No such target {target_id}") }
                    })),
                }
            }
            // Everything else goes to the extension's chrome.debugger.
            _ => ClientRoute::Forward(json!({
                "id": id,
                "method": "forwardCDPCommand",
                "params": { "method": method, "params": params, "sessionId": session_id },
            })),
        }
    }

    /// Handle one decoded message from the extension. Updates target state and
    /// returns the messages to emit (to the client and/or back to the
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

        // Response to a forwardCDPCommand we sent → raw CDP response to client.
        if msg.get("id").is_some()
            && (msg.get("result").is_some() || msg.get("error").is_some())
            && msg.get("method").is_none()
        {
            let mut out = json!({ "id": msg.get("id").cloned().unwrap_or(Value::Null) });
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
            return vec![RelayOut::ToClient(out)];
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
                    if let Some(info) = inner_params.get("targetInfo") {
                        if let Some(tid) = info.get("targetId").and_then(|t| t.as_str()) {
                            let sid = inner_params
                                .get("sessionId")
                                .and_then(|s| s.as_str())
                                .unwrap_or("")
                                .to_string();
                            self.targets.insert(
                                tid.to_string(),
                                TargetEntry { session_id: sid, target_info: info.clone() },
                            );
                        }
                    }
                    return vec![];
                }
                "Target.detachedFromTarget" => {
                    let gone = inner_params.get("sessionId").and_then(|s| s.as_str());
                    if let Some(gone) = gone {
                        self.targets.retain(|_, e| e.session_id != gone);
                    }
                    return vec![];
                }
                _ => {}
            }

            // Regular CDP event → deliver to the client with its sessionId.
            let mut ev = json!({ "method": inner_method, "params": inner_params });
            if let Some(sid) = session_id {
                ev["sessionId"] = json!(sid);
            }
            return vec![RelayOut::ToClient(ev)];
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
        assert!(out.is_empty(), "attachedToTarget should be consumed, not forwarded");
        // Now getTargets must report it.
        let route = s.route_client_command(&json!({ "id": 1, "method": "Target.getTargets" }));
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
    fn attach_to_target_returns_known_session() {
        let mut s = RelayState::new();
        s.seed_target("T1", "cb-tab-1");
        let route = s.route_client_command(
            &json!({ "id": 5, "method": "Target.attachToTarget", "params": { "targetId": "T1", "flatten": true } }),
        );
        assert_eq!(
            route,
            ClientRoute::Local(json!({ "id": 5, "result": { "sessionId": "cb-tab-1" } }))
        );
    }

    #[test]
    fn attach_to_unknown_target_errors_locally() {
        let s = RelayState::new();
        let route = s.route_client_command(
            &json!({ "id": 6, "method": "Target.attachToTarget", "params": { "targetId": "nope" } }),
        );
        match route {
            ClientRoute::Local(v) => assert!(v.get("error").is_some()),
            _ => panic!("should answer locally"),
        }
    }

    #[test]
    fn other_commands_forward_as_envelope() {
        let s = RelayState::new();
        let route = s.route_client_command(&json!({
            "id": 9, "method": "Page.navigate",
            "params": { "url": "https://x" }, "sessionId": "cb-tab-1"
        }));
        match route {
            ClientRoute::Forward(v) => {
                assert_eq!(v["method"], "forwardCDPCommand");
                assert_eq!(v["id"], 9);
                assert_eq!(v["params"]["method"], "Page.navigate");
                assert_eq!(v["params"]["sessionId"], "cb-tab-1");
                assert_eq!(v["params"]["params"]["url"], "https://x");
            }
            _ => panic!("Page.navigate must forward"),
        }
    }

    #[test]
    fn forward_command_response_becomes_client_response() {
        let mut s = RelayState::new();
        let out = s.handle_ext_message(&json!({ "id": 9, "result": { "frameId": "F1" } }), "tok");
        assert_eq!(
            out,
            vec![RelayOut::ToClient(json!({ "id": 9, "result": { "frameId": "F1" } }))]
        );
    }

    #[test]
    fn forward_command_error_is_wrapped() {
        let mut s = RelayState::new();
        let out = s.handle_ext_message(&json!({ "id": 9, "error": "boom" }), "tok");
        match &out[0] {
            RelayOut::ToClient(v) => {
                assert_eq!(v["id"], 9);
                assert_eq!(v["error"]["message"], "boom");
            }
            _ => panic!("expected ToClient"),
        }
    }

    #[test]
    fn regular_event_is_forwarded_with_session() {
        let mut s = RelayState::new();
        let ev = json!({
            "method": "forwardCDPEvent",
            "params": { "sessionId": "cb-tab-1", "method": "Page.loadEventFired", "params": { "timestamp": 1.0 } }
        });
        let out = s.handle_ext_message(&ev, "tok");
        assert_eq!(
            out,
            vec![RelayOut::ToClient(json!({
                "method": "Page.loadEventFired",
                "params": { "timestamp": 1.0 },
                "sessionId": "cb-tab-1"
            }))]
        );
    }

    #[test]
    fn connect_handshake_validates_token() {
        let mut s = RelayState::new();
        let req = json!({ "type": "req", "id": "c1", "method": "connect", "params": { "auth": { "token": "good" } } });
        let ok = s.handle_ext_message(&req, "good");
        assert_eq!(ok, vec![RelayOut::ToExt(json!({ "type": "res", "id": "c1", "ok": true }))]);

        let bad = s.handle_ext_message(&req, "different");
        match &bad[0] {
            RelayOut::ToExt(v) => {
                assert_eq!(v["ok"], false);
                assert!(v.get("error").is_some());
            }
            _ => panic!("expected ToExt"),
        }
    }
}
