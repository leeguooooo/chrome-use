use std::collections::HashMap;
use std::io::Write;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use futures_util::{SinkExt, StreamExt};
use serde_json::Value;
use tokio::sync::{broadcast, oneshot, Mutex};
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::protocol::WebSocketConfig;
use tokio_tungstenite::tungstenite::Message;

use super::types::{CdpCommand, CdpEvent, CdpMessage};

type PendingMap = Arc<Mutex<HashMap<u64, oneshot::Sender<CdpMessage>>>>;

/// Interval between WebSocket ping frames sent to keep the connection alive
/// through intermediate proxies (reverse proxies, load balancers, service meshes).
const WS_KEEPALIVE_INTERVAL_SECS: u64 = 30;

fn normalize_websocket_root_path(url: &str) -> String {
    let Some(scheme_end) = url.find("://").map(|index| index + 3) else {
        return url.to_string();
    };
    let authority = &url[scheme_end..];
    let Some(query_offset) = authority.find('?') else {
        return url.to_string();
    };
    if authority[..query_offset].contains('/') {
        return url.to_string();
    }
    let query_index = scheme_end + query_offset;
    format!("{}/{}", &url[..query_index], &url[query_index..])
}

/// The command id of a message that failed typed parsing, when it is a
/// reply to one of our commands (a positive integer `id`).
fn malformed_reply_id(raw: &str) -> Option<u64> {
    serde_json::from_str::<Value>(raw).ok()?.get("id")?.as_u64()
}

/// The error a command receives when its reply could not be parsed.
fn malformed_reply(id: u64, error: &serde_json::Error) -> CdpMessage {
    CdpMessage {
        id: Some(id),
        result: None,
        error: Some(super::types::CdpError {
            code: None,
            message: format!("malformed CDP reply: {error}"),
            data: None,
        }),
        method: None,
        params: None,
        session_id: None,
    }
}

/// Raw incoming CDP message (text) broadcast to all subscribers.
/// Used by the inspect proxy to forward responses and events to DevTools.
#[derive(Debug, Clone)]
pub struct RawCdpMessage {
    pub text: String,
    pub session_id: Option<String>,
}

pub struct CdpClient {
    ws_tx: Arc<
        Mutex<
            futures_util::stream::SplitSink<
                tokio_tungstenite::WebSocketStream<
                    tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
                >,
                Message,
            >,
        >,
    >,
    next_id: AtomicU64,
    pending: PendingMap,
    event_tx: broadcast::Sender<CdpEvent>,
    raw_tx: broadcast::Sender<RawCdpMessage>,
    _reader_handle: tokio::task::JoinHandle<()>,
    _keepalive_handle: tokio::task::JoinHandle<()>,
}

/// Flat budget for an ordinary CDP round trip.
const CDP_COMMAND_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);
/// Extra budget per byte of a size-proportional payload. `Input.insertText`
/// makes the renderer process the text character by character, so its cost
/// scales with size, not with round-trip health: 34 KB into a rich editor
/// measured ~0.45s/KB. A flat budget for a payload-sized command is a size
/// limit in disguise — it made the one-call `keyboard inserttext --file` path
/// fail at exactly the sizes it exists for. 4ms/byte is generous headroom over
/// the measured worst case; the ceiling keeps a pathological payload from
/// pinning a session.
const CDP_PAYLOAD_MICROS_PER_BYTE: u64 = 4_000;
/// Ceiling for a payload-scaled CDP command. Must stay above the extension's
/// own ceiling (`PAYLOAD_MAX_TIMEOUT_MS`, 300s) so the relay's more specific
/// error reaches the caller instead of the daemon cutting it off first. Raised
/// with the extension's in #315.
const CDP_PAYLOAD_MAX_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(360);

/// Budget for one CDP command. Pure so the scaling rule is testable without a
/// browser. Anything without a size-proportional payload keeps the flat budget,
/// so an ordinary hung command still fails as fast as it used to.
pub(crate) fn command_timeout(method: &str, params: Option<&Value>) -> std::time::Duration {
    if method != "Input.insertText" {
        return CDP_COMMAND_TIMEOUT;
    }
    let len = params
        .and_then(|p| p.get("text"))
        .and_then(|t| t.as_str())
        .map(|t| t.len() as u64)
        .unwrap_or(0);
    if len == 0 {
        return CDP_COMMAND_TIMEOUT;
    }
    // The daemon must outlast the extension's own (also payload-scaled) budget,
    // or the daemon cuts the command off first and the relay's more specific
    // error never reaches the caller.
    let scaled = CDP_COMMAND_TIMEOUT
        + std::time::Duration::from_micros(len.saturating_mul(CDP_PAYLOAD_MICROS_PER_BYTE));
    scaled.min(CDP_PAYLOAD_MAX_TIMEOUT)
}

impl CdpClient {
    pub async fn connect(url: &str) -> Result<Self, String> {
        Self::connect_with_headers(url, None).await
    }

    pub async fn connect_with_headers(
        url: &str,
        headers: Option<Vec<(String, String)>>,
    ) -> Result<Self, String> {
        let normalized_url = normalize_websocket_root_path(url);
        let mut request = normalized_url
            .as_str()
            .into_client_request()
            .map_err(|e| format!("Invalid WebSocket URL: {}", e))?;

        if let Some(hdrs) = headers {
            let req_headers = request.headers_mut();
            for (key, value) in hdrs {
                if let (Ok(name), Ok(val)) = (
                    key.parse::<tokio_tungstenite::tungstenite::http::header::HeaderName>(),
                    value.parse::<tokio_tungstenite::tungstenite::http::header::HeaderValue>(),
                ) {
                    req_headers.insert(name, val);
                }
            }
        }

        let ws_config = WebSocketConfig {
            max_message_size: None,
            max_frame_size: None,
            ..Default::default()
        };

        let (ws_stream, _) =
            tokio_tungstenite::connect_async_with_config(request, Some(ws_config), false)
                .await
                .map_err(|e| format!("CDP WebSocket connect failed: {}", e))?;

        enable_tcp_keepalive(ws_stream.get_ref());

        let (ws_tx, mut ws_rx) = ws_stream.split();
        let ws_tx = Arc::new(Mutex::new(ws_tx));

        let pending: PendingMap = Arc::new(Mutex::new(HashMap::new()));
        let (event_tx, _) = broadcast::channel(4096);
        let (raw_tx, _) = broadcast::channel(4096);

        let pending_clone = pending.clone();
        let event_tx_clone = event_tx.clone();
        let raw_tx_clone = raw_tx.clone();

        // Notify used to stop the keepalive task when the reader loop exits.
        let (cancel_tx, mut cancel_rx) = tokio::sync::watch::channel(false);

        let reader_handle = tokio::spawn(async move {
            while let Some(msg) = ws_rx.next().await {
                // Accept both Text and Binary frames — remote CDP proxies
                // (e.g. Browserless) may send responses as Binary frames.
                let msg = match msg {
                    Ok(Message::Text(text)) => text,
                    Ok(Message::Binary(data)) => match String::from_utf8(data) {
                        Ok(text) => text,
                        Err(_) => continue,
                    },
                    Ok(Message::Close(frame)) => {
                        if std::env::var("AGENT_BROWSER_DEBUG").is_ok() {
                            let reason = frame
                                .as_ref()
                                .map(|f| format!("code={}, reason={}", f.code, f.reason))
                                .unwrap_or_else(|| "no frame".to_string());
                            let _ =
                                writeln!(std::io::stderr(), "[cdp] WebSocket Close: {}", reason);
                        }
                        break;
                    }
                    Ok(Message::Pong(_)) => continue,
                    Ok(_) => continue,
                    Err(e) => {
                        if std::env::var("AGENT_BROWSER_DEBUG").is_ok() {
                            let _ = writeln!(std::io::stderr(), "[cdp] WebSocket Error: {}", e);
                        }
                        break;
                    }
                };

                // Broadcast raw message for inspect proxy subscribers before typed parse,
                // so messages with negative IDs (used by the inspect proxy) are still delivered.
                if raw_tx_clone.receiver_count() > 0 {
                    let session_id = serde_json::from_str::<serde_json::Value>(&msg)
                        .ok()
                        .and_then(|v| v.get("sessionId")?.as_str().map(String::from));
                    let _ = raw_tx_clone.send(RawCdpMessage {
                        text: msg.clone(),
                        session_id,
                    });
                }

                let parsed: CdpMessage = match serde_json::from_str(&msg) {
                    Ok(m) => m,
                    Err(e) => {
                        // A reply to one of our commands that does not fit the
                        // typed shape (an `error.data` object, say) used to be
                        // dropped, leaving that command to wait out its whole
                        // timeout. Fail it now with what went wrong. Anything
                        // else (inspect-proxy messages with negative ids) is
                        // handled by the raw broadcast above.
                        if let Some(id) = malformed_reply_id(&msg) {
                            if let Some(tx) = pending_clone.lock().await.remove(&id) {
                                let _ = tx.send(malformed_reply(id, &e));
                            }
                        }
                        continue;
                    }
                };

                if let Some(id) = parsed.id {
                    // Response to a command
                    let mut pending = pending_clone.lock().await;
                    if let Some(tx) = pending.remove(&id) {
                        let _ = tx.send(parsed);
                    }
                } else if let Some(ref method) = parsed.method {
                    // Event
                    let event = CdpEvent {
                        method: method.clone(),
                        params: parsed.params.clone().unwrap_or(Value::Null),
                        session_id: parsed.session_id.clone(),
                    };
                    let _ = event_tx_clone.send(event);
                }
            }

            // Reader loop exited (connection closed or error). Drop all pending
            // command senders so callers get an immediate channel-closed error
            // instead of waiting for the 30-second timeout.
            pending_clone.lock().await.clear();

            // Stop the keepalive task — the connection is gone.
            let _ = cancel_tx.send(true);
        });

        // Spawn a keepalive task that sends WebSocket Ping frames at a regular
        // interval. This prevents intermediate proxies (Envoy, nginx, OpenResty,
        // cloud load balancers) from closing idle WebSocket connections. If the
        // send fails, the connection is dead and we stop pinging.
        let keepalive_tx = ws_tx.clone();
        let keepalive_handle = tokio::spawn(async move {
            let interval = std::time::Duration::from_secs(WS_KEEPALIVE_INTERVAL_SECS);
            loop {
                tokio::select! {
                    _ = tokio::time::sleep(interval) => {}
                    _ = cancel_rx.changed() => break,
                }
                let mut tx = keepalive_tx.lock().await;
                if tx.send(Message::Ping(Vec::new())).await.is_err() {
                    break;
                }
            }
        });

        Ok(Self {
            ws_tx,
            next_id: AtomicU64::new(1),
            pending,
            event_tx,
            raw_tx,
            _reader_handle: reader_handle,
            _keepalive_handle: keepalive_handle,
        })
    }

    pub async fn send_command(
        &self,
        method: &str,
        params: Option<Value>,
        session_id: Option<&str>,
    ) -> Result<Value, String> {
        // Charged to the command being served, for its `timing` summary.
        let started = std::time::Instant::now();
        let out = self.send_command_untimed(method, params, session_id).await;
        crate::native::timing::record_span(method, started, std::time::Instant::now());
        out
    }

    async fn send_command_untimed(
        &self,
        method: &str,
        params: Option<Value>,
        session_id: Option<&str>,
    ) -> Result<Value, String> {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);

        // Computed before `params` moves into the command: the budget for a
        // payload-sized command depends on that payload.
        let budget = command_timeout(method, params.as_ref());

        let cmd = CdpCommand {
            id,
            method: method.to_string(),
            params,
            session_id: session_id.filter(|s| !s.is_empty()).map(|s| s.to_string()),
        };

        let json = serde_json::to_string(&cmd)
            .map_err(|e| format!("Failed to serialize CDP command: {}", e))?;

        let (tx, rx) = oneshot::channel();

        {
            let mut pending = self.pending.lock().await;
            pending.insert(id, tx);
        }

        {
            let mut ws_tx = self.ws_tx.lock().await;
            ws_tx
                .send(Message::Text(json))
                .await
                .map_err(|e| format!("Failed to send CDP command: {}", e))?;
        }

        let response = match tokio::time::timeout(budget, rx).await {
            Ok(Ok(resp)) => resp,
            Ok(Err(_)) => return Err("CDP response channel closed".to_string()),
            Err(_) => {
                self.pending.lock().await.remove(&id);
                return Err(crate::system_load::annotate_timeout(format!(
                    "CDP command timed out after {}s: {}",
                    budget.as_secs(),
                    method
                )));
            }
        };

        if let Some(error) = response.error {
            return Err(crate::system_load::annotate_timeout(format!(
                "CDP error ({}): {}",
                method, error
            )));
        }

        Ok(response.result.unwrap_or(Value::Null))
    }

    pub fn subscribe(&self) -> broadcast::Receiver<CdpEvent> {
        self.event_tx.subscribe()
    }

    /// Subscribe to all raw incoming CDP messages (responses + events).
    /// Used by the inspect proxy to forward traffic to the DevTools frontend.
    pub fn subscribe_raw(&self) -> broadcast::Receiver<RawCdpMessage> {
        self.raw_tx.subscribe()
    }

    /// Create a lightweight handle for the inspect WebSocket proxy.
    /// Contains only what's needed to forward messages bidirectionally.
    pub fn inspect_handle(&self) -> InspectProxyHandle {
        InspectProxyHandle {
            ws_tx: self.ws_tx.clone(),
            raw_tx: self.raw_tx.clone(),
        }
    }

    pub async fn send_command_typed<P: serde::Serialize, R: serde::de::DeserializeOwned>(
        &self,
        method: &str,
        params: &P,
        session_id: Option<&str>,
    ) -> Result<R, String> {
        let params_value = serde_json::to_value(params)
            .map_err(|e| format!("Failed to serialize params: {}", e))?;
        let result = self
            .send_command(method, Some(params_value), session_id)
            .await?;
        serde_json::from_value(result)
            .map_err(|e| format!("Failed to deserialize CDP response for {}: {}", method, e))
    }

    pub async fn send_command_no_params(
        &self,
        method: &str,
        session_id: Option<&str>,
    ) -> Result<Value, String> {
        self.send_command(method, None, session_id).await
    }

    /// Send raw JSON through the WebSocket without tracking a response.
    /// Used by the inspect proxy to forward DevTools frontend messages.
    pub async fn send_raw(&self, json: String) -> Result<(), String> {
        let mut ws_tx = self.ws_tx.lock().await;
        ws_tx
            .send(Message::Text(json))
            .await
            .map_err(|e| format!("Failed to send raw CDP message: {}", e))
    }
}

type WsTx = Arc<
    Mutex<
        futures_util::stream::SplitSink<
            tokio_tungstenite::WebSocketStream<
                tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
            >,
            Message,
        >,
    >,
>;

/// Lightweight handle for the inspect WebSocket proxy, holding only
/// the cloneable parts of CdpClient needed for bidirectional message forwarding.
pub struct InspectProxyHandle {
    ws_tx: WsTx,
    raw_tx: broadcast::Sender<RawCdpMessage>,
}

impl InspectProxyHandle {
    pub async fn send_raw(&self, json: String) -> Result<(), String> {
        let mut ws_tx = self.ws_tx.lock().await;
        ws_tx
            .send(Message::Text(json))
            .await
            .map_err(|e| format!("Failed to send raw CDP message: {}", e))
    }

    pub fn subscribe_raw(&self) -> broadcast::Receiver<RawCdpMessage> {
        self.raw_tx.subscribe()
    }
}

/// Enable TCP SO_KEEPALIVE on the underlying socket of a WebSocket connection.
/// This is best-effort: failures are silently ignored since the WebSocket-level
/// Ping keepalive provides the primary connection liveness mechanism.
fn enable_tcp_keepalive(stream: &tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>) {
    let tcp_stream = match stream {
        tokio_tungstenite::MaybeTlsStream::Plain(s) => s,
        tokio_tungstenite::MaybeTlsStream::Rustls(s) => s.get_ref().0,
        _ => return,
    };

    // SockRef borrows the fd without taking ownership.
    let sock = socket2::SockRef::from(tcp_stream);
    let keepalive = socket2::TcpKeepalive::new().with_time(std::time::Duration::from_secs(30));

    // with_interval sets TCP_KEEPINTVL — the time between probes after the
    // first keepalive probe goes unanswered. Available on most platforms
    // (Linux, macOS, Windows, FreeBSD, etc.) but not OpenBSD or Haiku.
    #[cfg(not(any(target_os = "openbsd", target_os = "haiku")))]
    let keepalive = keepalive.with_interval(std::time::Duration::from_secs(10));

    let _ = sock.set_tcp_keepalive(&keepalive);
}

#[cfg(test)]
mod timeout_tests {
    use super::command_timeout;
    use serde_json::json;

    #[test]
    fn only_payload_sized_commands_get_a_scaled_budget() {
        // An ordinary command keeps the flat 30s — scaling must never slow the
        // failure of a genuinely hung round trip.
        assert_eq!(command_timeout("Runtime.evaluate", None).as_secs(), 30);
        let big_eval = json!({ "expression": "x".repeat(50_000) });
        assert_eq!(
            command_timeout("Runtime.evaluate", Some(&big_eval)).as_secs(),
            30
        );
        // insertText without text, or with empty text, is still a flat command.
        assert_eq!(command_timeout("Input.insertText", None).as_secs(), 30);
        let empty = json!({ "text": "" });
        assert_eq!(
            command_timeout("Input.insertText", Some(&empty)).as_secs(),
            30
        );
    }

    #[test]
    fn insert_text_budget_grows_with_the_payload_and_is_capped() {
        // 20KB is where the flat relay budget used to cut the one-call
        // `keyboard inserttext --file` path off (#301).
        let twenty_kb = json!({ "text": "a".repeat(20_000) });
        assert_eq!(
            command_timeout("Input.insertText", Some(&twenty_kb)).as_secs(),
            30 + 80
        );
        // 34KB — the payload that motivated the flag.
        let thirty_four_kb = json!({ "text": "a".repeat(34_000) });
        assert_eq!(
            command_timeout("Input.insertText", Some(&thirty_four_kb)).as_secs(),
            30 + 136
        );
        // 80KB still scales at the full 4ms/byte: the old 180s ceiling cut in
        // at 37.5KB, which is what made the budgets deliver far less than they
        // implied (#309, #315).
        let eighty_kb = json!({ "text": "a".repeat(80_000) });
        assert_eq!(
            command_timeout("Input.insertText", Some(&eighty_kb)).as_secs(),
            30 + 320
        );
        // 150KB — the size #309 measured failing while the renderer was working
        // correctly. Capped now, but at a ceiling that still outlasts the
        // extension's own 300s, which is the invariant that matters.
        let one_fifty_kb = json!({ "text": "a".repeat(150_000) });
        assert_eq!(
            command_timeout("Input.insertText", Some(&one_fifty_kb)).as_secs(),
            360
        );
        // Still capped, so a pathological payload cannot pin a session
        // indefinitely — just not so low that an ordinary large insert reaches
        // it (#315).
        let huge = json!({ "text": "a".repeat(10_000_000) });
        assert_eq!(
            command_timeout("Input.insertText", Some(&huge)).as_secs(),
            360
        );
    }

    #[test]
    fn the_daemon_budget_outlasts_the_extension_budget() {
        // The extension scales at 2ms/byte on top of its own 8s flat budget and
        // caps at 300s. The daemon must always be the looser of the two, or it
        // cuts the command off first and the relay's more specific error never
        // reaches the caller.
        //
        // The sizes span both ceilings: the daemon's binds at 82.5KB and the
        // extension's at 146KB, so the invariant is checked while each is
        // scaling and while each is capped.
        for len in [
            1_000u64, 20_000, 34_000, 80_000, 100_000, 150_000, 300_000, 10_000_000,
        ] {
            let params = json!({ "text": "a".repeat(len as usize) });
            let daemon = command_timeout("Input.insertText", Some(&params)).as_millis() as u64;
            let extension = std::cmp::min(8_000 + len * 2, 300_000);
            assert!(
                daemon > extension,
                "daemon budget {daemon}ms must outlast extension {extension}ms for {len} bytes"
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::net::TcpListener;
    use tokio::sync::oneshot;

    #[test]
    fn normalizes_only_root_websocket_queries() {
        assert_eq!(
            normalize_websocket_root_path("wss://browser.example?token=a%2Fb"),
            "wss://browser.example/?token=a%2Fb"
        );
        assert_eq!(
            normalize_websocket_root_path("ws://[::1]:9222?token=test"),
            "ws://[::1]:9222/?token=test"
        );
        assert_eq!(
            normalize_websocket_root_path("wss://user:pass@browser.example?token=test"),
            "wss://user:pass@browser.example/?token=test"
        );
        assert_eq!(
            normalize_websocket_root_path("wss://browser.example?"),
            "wss://browser.example/?"
        );
        assert_eq!(
            normalize_websocket_root_path("wss://browser.example/?token=a%2Fb"),
            "wss://browser.example/?token=a%2Fb"
        );
        assert_eq!(
            normalize_websocket_root_path("wss://browser.example/cdp?token=a%2Fb"),
            "wss://browser.example/cdp?token=a%2Fb"
        );
    }

    // tungstenite's handshake callback signature returns its own large Err.
    #[allow(clippy::result_large_err)]
    #[tokio::test]
    async fn root_websocket_url_with_query_sends_slash_request_target() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let (path_tx, path_rx) = oneshot::channel();

        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut path_tx = Some(path_tx);
            let _ = tokio_tungstenite::accept_hdr_async(
                stream,
                move |
                    request: &tokio_tungstenite::tungstenite::handshake::server::Request,
                    response: tokio_tungstenite::tungstenite::handshake::server::Response,
                | {
                    if let Some(tx) = path_tx.take() {
                        let path = request
                            .uri()
                            .path_and_query()
                            .map(|value| value.as_str().to_string())
                            .unwrap_or_default();
                        let _ = tx.send(path);
                    }
                    Ok(response)
                },
            )
            .await
            .unwrap();
        });

        let url = format!("ws://127.0.0.1:{}?token=a%2Fb&scope=browser%20test", port);
        let _client = CdpClient::connect(&url).await.unwrap();

        assert_eq!(path_rx.await.unwrap(), "/?token=a%2Fb&scope=browser%20test");
        server.await.unwrap();
    }
}

#[cfg(test)]
mod malformed_reply_tests {
    use super::*;
    use tokio::net::TcpListener;

    #[test]
    fn only_positive_integer_ids_are_replies() {
        assert_eq!(
            malformed_reply_id(r#"{"id":7,"error":{"message":1}}"#),
            Some(7)
        );
        assert_eq!(malformed_reply_id(r#"{"id":-3,"result":{}}"#), None);
        assert_eq!(malformed_reply_id(r#"{"method":"X"}"#), None);
        assert_eq!(malformed_reply_id("not json"), None);
    }

    #[test]
    fn an_error_with_object_data_still_parses() {
        let m: CdpMessage = serde_json::from_str(
            r#"{"id":1,"error":{"code":-32000,"message":"boom","data":{"detail":"x"}}}"#,
        )
        .unwrap();
        assert_eq!(m.error.unwrap().message, "boom");
    }

    /// A reply our types cannot parse fails its command at once instead of
    /// leaving it to the command timeout (after upstream #1739).
    #[tokio::test]
    async fn a_malformed_reply_fails_its_command_at_once() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut ws = tokio_tungstenite::accept_async(stream).await.unwrap();
            while let Some(Ok(msg)) = ws.next().await {
                if let Message::Text(text) = msg {
                    let id = serde_json::from_str::<Value>(&text).unwrap()["id"]
                        .as_u64()
                        .unwrap();
                    // `message` must be a string: this cannot be parsed.
                    let reply = format!(r#"{{"id":{id},"error":{{"message":42}}}}"#);
                    ws.send(Message::Text(reply)).await.unwrap();
                }
            }
        });
        let client = CdpClient::connect(&format!("ws://127.0.0.1:{port}"))
            .await
            .unwrap();
        let started = std::time::Instant::now();
        let err = client
            .send_command("Runtime.evaluate", None, None)
            .await
            .unwrap_err();
        assert!(err.contains("malformed CDP reply"), "{err}");
        assert!(started.elapsed() < std::time::Duration::from_secs(5));
        server.abort();
    }
}
