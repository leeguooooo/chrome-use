//! `chrome-use mobile serve`: the WebSocket endpoint a paired phone connects
//! to. Plain JSON is exchanged only for the handshake; every later frame is
//! AES-256-GCM under per-connection keys (see `crypto`). Requests are checked
//! by `policy` and reach the panel service through `panel` only.

use super::crypto::{self, Channel, DeviceKeys, NONCE_LEN};
use super::panel::Panel;
use super::policy::{self, Action, Refusal};
use super::push;
use super::store::{self, State, Store};
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::net::{IpAddr, Ipv6Addr, SocketAddr};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{mpsc, watch, Semaphore};
use tokio_tungstenite::tungstenite::handshake::server::{ErrorResponse, Request, Response};
use tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode;
use tokio_tungstenite::tungstenite::protocol::{CloseFrame, WebSocketConfig};
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::WebSocketStream;

const MAX_CONNECTIONS: usize = 32;
/// Connections from one source that have not authenticated yet.
const MAX_PRE_AUTH_PER_SOURCE: usize = 4;
const MAX_MESSAGE: usize = 2 * 1024 * 1024;
/// Upgrade plus handshake (and pairing) must finish within this.
const PRE_AUTH_TIMEOUT: Duration = Duration::from_secs(5);
const REQUESTS_IN_FLIGHT: usize = 8;
const MAX_SUBSCRIPTIONS: usize = 16;
const SCREENSHOT_INTERVAL: Duration = Duration::from_secs(2);
const SCREENSHOT_WIDTH: u32 = 1080;
const INSPECT_INTERVAL: Duration = Duration::from_secs(30);
/// Failures from one source before backoff starts, and the longest backoff.
const FREE_FAILURES: u32 = 3;
const MAX_BACKOFF: Duration = Duration::from_secs(300);
/// Entries kept per limiter table; the least recently touched goes first.
const LIMITER_ENTRIES: usize = 4096;

const CLOSE_PROTOCOL: u16 = 4400;
const CLOSE_UNAUTHORIZED: u16 = 4401;
const CLOSE_REVOKED: u16 = 4403;
const CLOSE_TIMEOUT: u16 = 4408;
/// A paired device of another profile reached a `--profile` gateway.
const CLOSE_OTHER_PROFILE: u16 = 4409;
const CLOSE_RATE_LIMITED: u16 = 4429;

pub struct Options {
    pub bind: SocketAddr,
    /// Accept only devices paired to this panel profile.
    pub profile: Option<String>,
    pub push_url: String,
    /// Overrides (and saves) the computer name shown to phones.
    pub name: Option<String>,
    /// Take the client address from `CF-Connecting-IP` on loopback
    /// connections (cloudflared on this computer).
    pub trust_cf_header: bool,
}

/// The address rate limits are kept for: IPv4 as is (also when mapped into
/// IPv6), IPv6 by its /64, since one host usually holds a whole /64.
pub(crate) fn source_key(ip: IpAddr) -> IpAddr {
    match ip {
        IpAddr::V4(_) => ip,
        IpAddr::V6(v6) => match v6.to_ipv4_mapped() {
            Some(v4) => IpAddr::V4(v4),
            None => {
                let s = v6.segments();
                IpAddr::V6(Ipv6Addr::new(s[0], s[1], s[2], s[3], 0, 0, 0, 0))
            }
        },
    }
}

struct Strike {
    count: u32,
    until: Option<Instant>,
    touched: Instant,
}

/// Failed handshakes per source and per (source, claimed device), with
/// exponential backoff. Kept in memory only; a device is never disabled by
/// failures, since anyone who knows its id could cause them.
#[derive(Default)]
pub(crate) struct Limiter {
    sources: HashMap<IpAddr, Strike>,
    devices: HashMap<(IpAddr, String), Strike>,
}

impl Limiter {
    fn wait<K: std::hash::Hash + Eq>(map: &HashMap<K, Strike>, key: &K, now: Instant) -> bool {
        map.get(key)
            .and_then(|s| s.until)
            .is_some_and(|until| until > now)
    }
    fn strike<K: std::hash::Hash + Eq + Clone>(map: &mut HashMap<K, Strike>, key: K, now: Instant) {
        if !map.contains_key(&key) && map.len() >= LIMITER_ENTRIES {
            if let Some(oldest) = map
                .iter()
                .min_by_key(|(_, s)| s.touched)
                .map(|(k, _)| k.clone())
            {
                map.remove(&oldest);
            }
        }
        let strike = map.entry(key).or_insert(Strike {
            count: 0,
            until: None,
            touched: now,
        });
        strike.count = strike.count.saturating_add(1);
        strike.touched = now;
        if strike.count >= FREE_FAILURES {
            let exponent = (strike.count - FREE_FAILURES).min(16);
            let delay = Duration::from_secs(1u64 << exponent).min(MAX_BACKOFF);
            strike.until = Some(now + delay);
        }
    }
    pub(crate) fn source_blocked(&self, ip: IpAddr, now: Instant) -> bool {
        Self::wait(&self.sources, &source_key(ip), now)
    }
    pub(crate) fn device_blocked(&self, ip: IpAddr, id: &str, now: Instant) -> bool {
        Self::wait(&self.devices, &(source_key(ip), id.to_string()), now)
    }
    pub(crate) fn fail(&mut self, ip: IpAddr, device: Option<&str>, now: Instant) {
        let ip = source_key(ip);
        Self::strike(&mut self.sources, ip, now);
        if let Some(id) = device {
            Self::strike(&mut self.devices, (ip, id.to_string()), now);
        }
    }
    pub(crate) fn succeed(&mut self, ip: IpAddr, device: &str) {
        let ip = source_key(ip);
        self.sources.remove(&ip);
        self.devices.remove(&(ip, device.to_string()));
    }
    #[cfg(test)]
    fn len(&self) -> usize {
        self.sources.len() + self.devices.len()
    }
}

/// One authenticated connection: its device, close signal and event queue.
struct Live {
    device: String,
    close: watch::Sender<bool>,
    outbox: Outbox,
}

pub(crate) struct Shared {
    pub(crate) store: Store,
    pub(crate) panel: Panel,
    pub(crate) profile: Option<String>,
    trust_cf_header: bool,
    limiter: Mutex<Limiter>,
    /// Unauthenticated connections per source key.
    pre_auth: Mutex<HashMap<IpAddr, usize>>,
    live: Mutex<HashMap<u64, Live>>,
    /// Shown to the phone by `hello`.
    computer_name: String,
    next_connection: AtomicU64,
    screenshots: Mutex<HashMap<String, Instant>>,
    /// (profile, agent) -> (started, result once finished).
    inspections: Mutex<InspectCache>,
}

type InspectCache = HashMap<(String, String), (Instant, Option<Result<Value, String>>)>;

impl Shared {
    /// Queue an event for every live connection of `device`; dropped when a
    /// connection's queue is full.
    pub(crate) fn notify(&self, device: &str, event: Value) {
        if let Ok(live) = self.live.lock() {
            for connection in live.values().filter(|c| c.device == device) {
                let _ = connection.outbox.try_send(event.clone());
            }
        }
    }

    /// Hold one of a source's few unauthenticated connection slots.
    fn pre_auth(self: &Arc<Self>, ip: IpAddr) -> Option<PreAuth> {
        let key = source_key(ip);
        let mut slots = self.pre_auth.lock().ok()?;
        let count = slots.entry(key).or_insert(0);
        if *count >= MAX_PRE_AUTH_PER_SOURCE {
            return None;
        }
        *count += 1;
        Some(PreAuth {
            shared: self.clone(),
            key,
        })
    }

    fn strike(&self, ip: IpAddr) {
        if let Ok(mut limiter) = self.limiter.lock() {
            limiter.fail(ip, None, Instant::now());
        }
    }

    fn blocked(&self, ip: IpAddr) -> bool {
        self.limiter
            .lock()
            .map(|l| l.source_blocked(ip, Instant::now()))
            .unwrap_or(true)
    }
}

struct PreAuth {
    shared: Arc<Shared>,
    key: IpAddr,
}

impl Drop for PreAuth {
    fn drop(&mut self) {
        if let Ok(mut slots) = self.shared.pre_auth.lock() {
            if let Some(count) = slots.get_mut(&self.key) {
                *count -= 1;
                if *count == 0 {
                    slots.remove(&self.key);
                }
            }
        }
    }
}

/// Wait for the explicitly selected interface after startup or a network change.
/// Never fall back to another address, and never announce readiness before bind.
/// Other errors (including an occupied port) still fail immediately.
async fn bind_when_available(address: SocketAddr) -> Result<TcpListener, String> {
    let mut delay = Duration::from_secs(1);
    loop {
        match TcpListener::bind(address).await {
            Ok(listener) => return Ok(listener),
            Err(error) if error.kind() == std::io::ErrorKind::AddrNotAvailable => {
                eprintln!(
                    "mobile gateway waiting: {address} is not available ({error}); retrying in {}s. No listener is ready; check --bind if the network changed.",
                    delay.as_secs()
                );
                tokio::time::sleep(delay).await;
                delay = (delay * 2).min(Duration::from_secs(60));
            }
            Err(error) => return Err(format!("Cannot listen on {address}: {error}")),
        }
    }
}

pub async fn serve(options: Options) -> Result<(), String> {
    let store = Store::open()?;
    let computer_name = match options.name {
        Some(name) => {
            let saved = name.clone();
            store.update(|r| {
                r.computer_name = Some(saved);
                Ok(())
            })?;
            name
        }
        None => store
            .read()?
            .computer_name
            .unwrap_or_else(super::computer_name),
    };
    let listener = bind_when_available(options.bind).await?;
    let local = listener
        .local_addr()
        .map_err(|_| "Cannot read the listening address")?;
    let shared = Arc::new(Shared {
        store,
        panel: Panel::default(),
        profile: options.profile,
        trust_cf_header: options.trust_cf_header,
        limiter: Mutex::new(Limiter::default()),
        pre_auth: Mutex::new(HashMap::new()),
        live: Mutex::new(HashMap::new()),
        computer_name,
        next_connection: AtomicU64::new(1),
        screenshots: Mutex::new(HashMap::new()),
        inspections: Mutex::new(HashMap::new()),
    });
    eprintln!("mobile gateway listening on ws://{local}");
    // Machine-readable readiness line for scripts and tests.
    println!("{}", json!({"listening": local.to_string()}));
    tokio::spawn(watch_revocations(shared.clone()));
    tokio::spawn(push::watch(shared.clone(), options.push_url));
    let slots = Arc::new(Semaphore::new(MAX_CONNECTIONS));
    loop {
        let (stream, peer) = match listener.accept().await {
            Ok(v) => v,
            Err(_) => {
                tokio::time::sleep(Duration::from_millis(100)).await;
                continue;
            }
        };
        let Ok(slot) = slots.clone().try_acquire_owned() else {
            continue;
        };
        // A trusted tunnel connects from loopback for every client; its
        // source is known only from the header, after the upgrade.
        let forwarded = shared.trust_cf_header && is_loopback(peer.ip());
        let early = if forwarded {
            None
        } else {
            if shared.blocked(peer.ip()) {
                continue;
            }
            match shared.pre_auth(peer.ip()) {
                Some(guard) => Some(guard),
                None => continue,
            }
        };
        let shared = shared.clone();
        tokio::spawn(async move {
            let _slot = slot;
            connection(shared, stream, peer, forwarded, early).await;
        });
    }
}

/// Drop connections of revoked devices within half a second.
async fn watch_revocations(shared: Arc<Shared>) {
    loop {
        tokio::time::sleep(Duration::from_millis(500)).await;
        let store = shared.store.clone();
        let Ok(Ok(registry)) = tokio::task::spawn_blocking(move || store.read()).await else {
            continue;
        };
        if let Ok(live) = shared.live.lock() {
            for Live { device, close, .. } in live.values() {
                if registry
                    .device(device)
                    .is_none_or(|d| d.state != State::Active)
                {
                    let _ = close.send(true);
                }
            }
        }
    }
}

async fn close(ws: &mut WebSocketStream<TcpStream>, code: u16, reason: &'static str) {
    let _ = ws
        .send(Message::Close(Some(CloseFrame {
            code: CloseCode::from(code),
            reason: reason.into(),
        })))
        .await;
    let _ = tokio::time::timeout(Duration::from_secs(1), ws.close(None)).await;
}

fn is_loopback(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => v4.is_loopback(),
        IpAddr::V6(v6) => {
            v6.is_loopback() || v6.to_ipv4_mapped().is_some_and(|v4| v4.is_loopback())
        }
    }
}

/// Browsers always send `Origin`; the phone apps never do. Refusing it keeps
/// web pages (including ones on this computer) from opening a socket here.
#[allow(clippy::result_large_err)]
fn check_upgrade(request: &Request) -> Result<(), ErrorResponse> {
    if request.headers().contains_key("origin") {
        let mut refusal = ErrorResponse::new(Some("Browser connections are not accepted".into()));
        *refusal.status_mut() = tokio_tungstenite::tungstenite::http::StatusCode::FORBIDDEN;
        return Err(refusal);
    }
    Ok(())
}

// The upgrade callback's error type is tungstenite's HTTP response.
#[allow(clippy::result_large_err)]
async fn connection(
    shared: Arc<Shared>,
    stream: TcpStream,
    peer: SocketAddr,
    forwarded: bool,
    early: Option<PreAuth>,
) {
    let deadline = tokio::time::Instant::now() + PRE_AUTH_TIMEOUT;
    let mut header_source: Option<IpAddr> = None;
    let config = WebSocketConfig {
        max_message_size: Some(MAX_MESSAGE),
        max_frame_size: Some(MAX_MESSAGE),
        ..Default::default()
    };
    let upgrade = tokio_tungstenite::accept_hdr_async_with_config(
        stream,
        |request: &Request, response: Response| {
            check_upgrade(request)?;
            if forwarded {
                header_source = request
                    .headers()
                    .get("cf-connecting-ip")
                    .and_then(|v| v.to_str().ok())
                    .and_then(|v| v.trim().parse().ok());
            }
            Ok(response)
        },
        Some(config),
    );
    let upgraded = tokio::time::timeout_at(deadline, upgrade).await;
    // Without the header a trusted tunnel's client is unknown; the loopback
    // address itself is never struck or checked.
    let source = if forwarded {
        header_source
    } else {
        Some(peer.ip())
    };
    let mut ws = match upgraded {
        Ok(Ok(ws)) => ws,
        _ => {
            if let Some(source) = source {
                shared.strike(source);
            }
            return;
        }
    };
    let _late = match (forwarded, source) {
        (true, Some(source)) => {
            if shared.blocked(source) {
                close(&mut ws, CLOSE_RATE_LIMITED, "try again later").await;
                return;
            }
            match shared.pre_auth(source) {
                Some(guard) => Some(guard),
                None => {
                    close(&mut ws, CLOSE_RATE_LIMITED, "try again later").await;
                    return;
                }
            }
        }
        _ => None,
    };
    let session = match tokio::time::timeout_at(deadline, handshake(&shared, &mut ws, source)).await
    {
        Ok(Ok(session)) => session,
        Ok(Err(code)) => {
            let reason = if code == CLOSE_OTHER_PROFILE {
                "device is paired to another profile"
            } else {
                "handshake failed"
            };
            close(&mut ws, code, reason).await;
            return;
        }
        Err(_) => {
            if let Some(source) = source {
                shared.strike(source);
            }
            close(&mut ws, CLOSE_TIMEOUT, "handshake timed out").await;
            return;
        }
    };
    drop(early);
    drop(_late);
    let (closer, closed) = watch::channel(false);
    let connection_id = shared.next_connection.fetch_add(1, Ordering::Relaxed);
    let (outbox, queue) = mpsc::channel::<Value>(64);
    if let Ok(mut live) = shared.live.lock() {
        live.insert(
            connection_id,
            Live {
                device: session.device.clone(),
                close: closer,
                outbox: outbox.clone(),
            },
        );
    }
    let shown = source.map_or_else(|| "a tunnel".to_string(), |s| s.to_string());
    eprintln!("mobile: device {} connected from {shown}", session.device);
    let code = run_session(&shared, ws, session.clone(), closed, outbox, queue).await;
    if let Ok(mut live) = shared.live.lock() {
        live.remove(&connection_id);
    }
    eprintln!("mobile: device {} disconnected ({code})", session.device);
}

#[derive(Clone)]
struct Session {
    device: String,
    profile: String,
    send_key: [u8; 32],
    receive_key: [u8; 32],
}

/// Run the challenge-response. On failure the reply is a close code only; it
/// never says whether the device exists.
async fn handshake(
    shared: &Arc<Shared>,
    ws: &mut WebSocketStream<TcpStream>,
    source: Option<IpAddr>,
) -> Result<Session, u16> {
    let registry = {
        let store = shared.store.clone();
        tokio::task::spawn_blocking(move || store.read())
            .await
            .map_err(|_| CLOSE_PROTOCOL)?
            .map_err(|_| CLOSE_PROTOCOL)?
    };
    let gateway_nonce: [u8; NONCE_LEN] = crypto::random().map_err(|_| CLOSE_PROTOCOL)?;
    let challenge = json!({"type":"challenge","v":1,"gatewayNonce":crypto::b64url(&gateway_nonce),"gateway":registry.gateway_id});
    ws.send(Message::Text(challenge.to_string()))
        .await
        .map_err(|_| CLOSE_PROTOCOL)?;
    let reply = loop {
        match ws.next().await {
            Some(Ok(Message::Text(text))) => break text,
            Some(Ok(Message::Ping(_) | Message::Pong(_))) => continue,
            _ => return Err(CLOSE_PROTOCOL),
        }
    };
    let reply: Value = serde_json::from_str(&reply).map_err(|_| CLOSE_PROTOCOL)?;
    let device_id = reply["deviceId"].as_str().unwrap_or("").to_string();
    let pairing = reply["pairing"] == json!(true);
    let phone_nonce = reply["phoneNonce"]
        .as_str()
        .and_then(crypto::from_b64url::<NONCE_LEN>);
    let proof = reply["proof"].as_str().and_then(crypto::from_b64url::<32>);
    let now = Instant::now();
    let fail = |known: bool| {
        if let (Some(source), Ok(mut limiter)) = (source, shared.limiter.lock()) {
            limiter.fail(source, known.then_some(device_id.as_str()), now);
        }
    };
    if reply["type"] != "auth" || reply["v"] != 1 {
        fail(false);
        return Err(CLOSE_PROTOCOL);
    }
    let (Some(phone_nonce), Some(proof)) = (phone_nonce, proof) else {
        fail(false);
        return Err(CLOSE_UNAUTHORIZED);
    };
    let wall = store::now_ms();
    let device = registry.device(&device_id).cloned();
    let Some(device) = device.filter(|d| d.usable(wall)) else {
        fail(false);
        return Err(CLOSE_UNAUTHORIZED);
    };
    if let Some(source) = source {
        if shared
            .limiter
            .lock()
            .map(|l| l.device_blocked(source, &device.id, now))
            .unwrap_or(true)
        {
            return Err(CLOSE_RATE_LIMITED);
        }
    }
    // A pending device is only reachable by pairing, and pairing only a
    // pending device: scanning the same QR again fails.
    let mode_ok = pairing == (device.state == State::Pending);
    // Pending keys come from the pair secret alone, paired keys also from the
    // phone's secret; a key file of the other kind authenticates nobody.
    let expected_len = if device.state == State::Pending {
        crypto::KEY_LEN
    } else {
        2 * crypto::KEY_LEN
    };
    let secret = shared
        .store
        .secret(&device.id)
        .ok()
        .filter(|s| s.len() == expected_len);
    let keys = secret.as_deref().map(DeviceKeys::derive);
    let verified = keys.as_ref().is_some_and(|keys| {
        crypto::verify_phone_proof(keys, &gateway_nonce, &phone_nonce, &device.id, &proof)
    });
    if !verified {
        fail(true);
        return Err(CLOSE_UNAUTHORIZED);
    }
    // A genuine device of another profile is not an attack: it is told so
    // and does not count toward backoff.
    if shared
        .profile
        .as_ref()
        .is_some_and(|p| p != &device.profile)
    {
        return Err(CLOSE_OTHER_PROFILE);
    }
    if !mode_ok {
        fail(true);
        return Err(CLOSE_UNAUTHORIZED);
    }
    let (Some(keys), Some(secret)) = (keys, secret) else {
        return Err(CLOSE_UNAUTHORIZED);
    };
    let ready = json!({"type":"ready","proof":crypto::b64url(&crypto::gateway_proof(&keys, &gateway_nonce, &phone_nonce, &device.id))});
    let ip = source.map_or_else(|| "tunnel".to_string(), |s| s.to_string());
    let keys = if pairing {
        ws.send(Message::Text(ready.to_string()))
            .await
            .map_err(|_| CLOSE_PROTOCOL)?;
        let phone_secret = complete_pairing(
            shared,
            ws,
            &keys,
            &gateway_nonce,
            &phone_nonce,
            &device.id,
            ip,
        )
        .await?;
        let mut ikm = secret;
        ikm.extend_from_slice(&phone_secret);
        DeviceKeys::derive(&ikm)
    } else {
        // Committed under the lock, so a revoke racing this handshake wins.
        let seen = {
            let store = shared.store.clone();
            let id = device.id.clone();
            tokio::task::spawn_blocking(move || {
                store.update(|r| {
                    let d = r.device_mut(&id).ok_or("revoked")?;
                    if d.state != State::Active {
                        return Err("state changed".to_string());
                    }
                    d.last_seen = Some(store::now_ms());
                    d.last_ip = Some(ip);
                    Ok(())
                })
            })
            .await
        };
        if !matches!(seen, Ok(Ok(()))) {
            return Err(CLOSE_UNAUTHORIZED);
        }
        ws.send(Message::Text(ready.to_string()))
            .await
            .map_err(|_| CLOSE_PROTOCOL)?;
        keys
    };
    if let (Some(source), Ok(mut limiter)) = (source, shared.limiter.lock()) {
        limiter.succeed(source, &device.id);
    }
    let (send_key, receive_key) = keys.session(&gateway_nonce, &phone_nonce);
    Ok(Session {
        device: device.id,
        profile: device.profile,
        send_key,
        receive_key,
    })
}

/// The pairing connection's only exchange under the pair-secret keys: the
/// phone sends `pairComplete {phoneSecret}` as its first frame, the gateway
/// stores it, activates the device and acknowledges. Both sides then switch
/// to keys derived from `pair_secret || phone_secret`, with sequence numbers
/// starting again at 0.
async fn complete_pairing(
    shared: &Arc<Shared>,
    ws: &mut WebSocketStream<TcpStream>,
    keys: &DeviceKeys,
    gateway_nonce: &[u8; NONCE_LEN],
    phone_nonce: &[u8; NONCE_LEN],
    device: &str,
    ip: String,
) -> Result<[u8; crypto::KEY_LEN], u16> {
    let (send_key, receive_key) = keys.session(gateway_nonce, phone_nonce);
    let frame = loop {
        match ws.next().await {
            Some(Ok(Message::Binary(frame))) => break frame,
            Some(Ok(Message::Ping(_) | Message::Pong(_))) => continue,
            _ => return Err(CLOSE_PROTOCOL),
        }
    };
    let plaintext = Channel::new(&receive_key)
        .open(&frame)
        .map_err(|_| CLOSE_PROTOCOL)?;
    let request: Value = serde_json::from_slice(&plaintext).map_err(|_| CLOSE_PROTOCOL)?;
    let phone_secret = request["params"]["phoneSecret"]
        .as_str()
        .and_then(crypto::from_b64url::<{ crypto::KEY_LEN }>)
        .filter(|_| request["op"] == "pairComplete")
        .ok_or(CLOSE_PROTOCOL)?;
    let committed = {
        let store = shared.store.clone();
        let device = device.to_string();
        tokio::task::spawn_blocking(move || store.complete_pairing(&device, &phone_secret, ip))
            .await
    };
    if !matches!(committed, Ok(Ok(()))) {
        return Err(CLOSE_UNAUTHORIZED);
    }
    let ack =
        json!({"id":request.get("id").cloned().unwrap_or(Value::Null),"result":{"paired":true}});
    let sealed = Channel::new(&send_key)
        .seal(ack.to_string().as_bytes())
        .map_err(|_| CLOSE_PROTOCOL)?;
    ws.send(Message::Binary(sealed))
        .await
        .map_err(|_| CLOSE_PROTOCOL)?;
    Ok(phone_secret)
}

type Outbox = mpsc::Sender<Value>;

async fn run_session(
    shared: &Arc<Shared>,
    ws: WebSocketStream<TcpStream>,
    session: Session,
    mut closed: watch::Receiver<bool>,
    outbox: Outbox,
    mut queue: mpsc::Receiver<Value>,
) -> &'static str {
    let (mut sink, mut source) = ws.split();
    let mut sender = Channel::new(&session.send_key);
    let mut receiver = Channel::new(&session.receive_key);
    let writer = tokio::spawn(async move {
        let mut ping = tokio::time::interval(Duration::from_secs(25));
        ping.tick().await;
        loop {
            let message = tokio::select! {
                next = queue.recv() => match next {
                    Some(Value::Null) | None => break,
                    Some(value) => match serde_json::to_vec(&value).map_err(|e| e.to_string()).and_then(|b| sender.seal(&b)) {
                        Ok(frame) => Message::Binary(frame),
                        Err(_) => break,
                    },
                },
                _ = ping.tick() => Message::Ping(Vec::new()),
            };
            if sink.send(message).await.is_err() {
                return sink;
            }
        }
        sink
    });
    let requests = Arc::new(Semaphore::new(REQUESTS_IN_FLIGHT));
    let mut running = tokio::task::JoinSet::new();
    let mut subscriptions: HashMap<String, tokio::task::JoinHandle<()>> = HashMap::new();
    let outcome: (&'static str, u16) = loop {
        let frame = tokio::select! {
            _ = closed.changed() => break ("revoked", CLOSE_REVOKED),
            frame = source.next() => frame,
        };
        let bytes = match frame {
            Some(Ok(Message::Binary(bytes))) => bytes,
            Some(Ok(Message::Ping(_) | Message::Pong(_))) => continue,
            Some(Ok(Message::Close(_))) | None => break ("closed", 1000),
            Some(Ok(_)) => break ("plaintext frame", CLOSE_PROTOCOL),
            Some(Err(_)) => break ("transport error", CLOSE_PROTOCOL),
        };
        let Ok(plaintext) = receiver.open(&bytes) else {
            break ("rejected frame", CLOSE_PROTOCOL);
        };
        let Ok(request) = serde_json::from_slice::<Value>(&plaintext) else {
            break ("invalid request", CLOSE_PROTOCOL);
        };
        let id = request.get("id").cloned().unwrap_or(Value::Null);
        let op = request["op"].as_str().unwrap_or("").to_string();
        let params = request.get("params").cloned().unwrap_or(json!({}));
        let action = match policy::check(&op, &params) {
            Ok(action) => action,
            Err(refusal) => {
                let _ = outbox.send(refused(&id, refusal)).await;
                continue;
            }
        };
        match action {
            Action::Subscribe {
                conversation_id,
                after,
            } => {
                if let Some(old) = subscriptions.remove(&conversation_id) {
                    old.abort();
                }
                subscriptions.retain(|_, task| !task.is_finished());
                if subscriptions.len() >= MAX_SUBSCRIPTIONS {
                    let _ = outbox
                        .send(refused(
                            &id,
                            Refusal {
                                code: "limit",
                                message: format!("At most {MAX_SUBSCRIPTIONS} subscriptions"),
                            },
                        ))
                        .await;
                    continue;
                }
                let task = tokio::spawn(follow(
                    shared.panel.clone(),
                    session.profile.clone(),
                    conversation_id.clone(),
                    after,
                    outbox.clone(),
                ));
                subscriptions.insert(conversation_id.clone(), task);
                let _ = outbox
                    .send(json!({"id":id,"result":{"subscribed":conversation_id,"after":after}}))
                    .await;
            }
            Action::Unsubscribe { conversation_id } => {
                let found = subscriptions.remove(&conversation_id);
                if let Some(task) = &found {
                    task.abort();
                }
                let _ = outbox
                    .send(json!({"id":id,"result":{"unsubscribed":found.is_some()}}))
                    .await;
            }
            action => {
                // A full request queue must not keep a revoked device open.
                let permit = tokio::select! {
                    _ = closed.changed() => break ("revoked", CLOSE_REVOKED),
                    permit = requests.clone().acquire_owned() => permit,
                };
                let Ok(permit) = permit else {
                    break ("shutdown", 1001);
                };
                while running.try_join_next().is_some() {}
                let shared = shared.clone();
                let session = session.clone();
                let outbox = outbox.clone();
                running.spawn(async move {
                    let _permit = permit;
                    let reply = match perform(&shared, &session, action).await {
                        Ok(result) => json!({"id":id,"result":result}),
                        Err(refusal) => refused(&id, refusal),
                    };
                    let _ = outbox.send(reply).await;
                });
            }
        }
    };
    for task in subscriptions.values() {
        task.abort();
    }
    // Requests still running are not answered on a closed connection.
    running.abort_all();
    let _ = outbox.send(Value::Null).await;
    drop(outbox);
    if let Ok(mut sink) = writer.await {
        let _ = sink
            .send(Message::Close(Some(CloseFrame {
                code: CloseCode::from(outcome.1),
                reason: outcome.0.into(),
            })))
            .await;
        let _ = sink.close().await;
    }
    outcome.0
}

fn refused(id: &Value, refusal: Refusal) -> Value {
    json!({"id":id,"error":{"code":refusal.code,"message":refusal.message}})
}

fn panel_error(message: String) -> Refusal {
    Refusal {
        code: "panel",
        message,
    }
}

async fn perform(
    shared: &Arc<Shared>,
    session: &Session,
    action: Action,
) -> Result<Value, Refusal> {
    let profile = session.profile.as_str();
    match action {
        Action::Panel { operation, params } => {
            match operation {
                "conversationSend" | "conversationSendDirect" => {
                    let id = params["conversationId"].as_str().unwrap_or("");
                    let row = conversation_row(shared, profile, id).await?;
                    require_access(shared, session, &row).await?;
                }
                "conversationRespond" if !policy::declines(&params["decision"]) => {
                    let id = params["conversationId"].as_str().unwrap_or("");
                    let row = conversation_row(shared, profile, id).await?;
                    require_access(shared, session, &row).await?;
                }
                "scheduleRun" => {
                    let id = params["id"].as_str().unwrap_or("");
                    let schedule = schedule_row(shared, profile, id).await?;
                    require_access(shared, session, &schedule).await?;
                }
                _ => {}
            }
            shared
                .panel
                .call(operation, params, profile)
                .await
                .map_err(panel_error)
        }
        Action::SetOptions {
            conversation_id,
            options,
        } => {
            let mut options = options;
            let row = conversation_row(shared, profile, &conversation_id).await?;
            require_access(shared, session, &row).await?;
            let agent = row["agentId"].as_str().unwrap_or("");
            let full = row["fullAccess"] == true;
            let current_access = row["options"]["toolAccess"].as_str();
            // Options replace each other whole: an omitted toolAccess would
            // fall back to the agent default, so keep the current one.
            if options.get("toolAccess").is_none_or(Value::is_null) {
                match current_access {
                    Some(access) => options.insert("toolAccess".into(), access.into()),
                    None => options.remove("toolAccess"),
                };
            }
            let current = policy::effective_rank(agent, current_access, full);
            let requested = policy::effective_rank(
                agent,
                options.get("toolAccess").and_then(Value::as_str),
                full,
            );
            if requested > current {
                return Err(Refusal {
                    code: "denied",
                    message: "The phone cannot raise a conversation's tool access".into(),
                });
            }
            if agent == "pi" && requested > 0 {
                return Err(Refusal {
                    code: "denied",
                    message: "Pi runs tools without asking, so from the phone it gets browser tools only (browserOnly)".into(),
                });
            }
            shared
                .panel
                .call(
                    "conversationSetOptions",
                    json!({"conversationId":conversation_id,"options":options}),
                    profile,
                )
                .await
                .map_err(panel_error)
        }
        Action::ScheduleToggle { id, enabled } => {
            let list = shared
                .panel
                .call("scheduleList", json!({}), profile)
                .await
                .map_err(panel_error)?;
            let schedule = list["schedules"]
                .as_array()
                .and_then(|s| s.iter().find(|s| s["id"] == id.as_str()))
                .ok_or_else(|| Refusal {
                    code: "notFound",
                    message: "Schedule not found; the phone cannot create schedules".into(),
                })?;
            // Turning a schedule on lets it run by itself later; turning one
            // off is always allowed.
            if enabled {
                require_access(shared, session, schedule).await?;
            }
            shared
                .panel
                .call("scheduleSave", json!({"id":id,"enabled":enabled}), profile)
                .await
                .map_err(panel_error)
        }
        Action::PushRegister { relay_token } => {
            let store = shared.store.clone();
            let device = session.device.clone();
            let refused =
                tokio::task::spawn_blocking(move || store.register_token(&device, &relay_token))
                    .await
                    .map_err(|_| "Device store unavailable".to_string())
                    .and_then(|r| r)
                    .map_err(|message| Refusal {
                        code: "store",
                        message,
                    })?;
            Ok(match refused {
                Some(reason) => json!({"registered":false,"reason":reason}),
                None => json!({"registered":true}),
            })
        }
        Action::PushUnregister => {
            set_relay_token(shared, &session.device, None).await?;
            Ok(json!({"registered":false}))
        }
        Action::Hello => {
            let store = shared.store.clone();
            let registry = tokio::task::spawn_blocking(move || store.read())
                .await
                .map_err(|_| "Device store unavailable".to_string())
                .and_then(|r| r)
                .map_err(|message| Refusal {
                    code: "store",
                    message,
                })?;
            let device = registry.device(&session.device);
            Ok(json!({
                "computerName": shared.computer_name,
                "gatewayVersion": env!("CARGO_PKG_VERSION"),
                "pushMutes": device.map(|d| d.muted.clone()).unwrap_or_default(),
                "fullAccessAllowed": device.is_some_and(|d| d.allow_full_access),
            }))
        }
        Action::PushMute {
            conversation_id,
            muted,
        } => {
            let store = shared.store.clone();
            let device = session.device.clone();
            let conversation = conversation_id.clone();
            let all =
                tokio::task::spawn_blocking(move || store.set_mute(&device, &conversation, muted))
                    .await
                    .map_err(|_| "Device store unavailable".to_string())
                    .and_then(|r| r)
                    .map_err(|message| Refusal {
                        code: "store",
                        message,
                    })?;
            Ok(json!({"conversationId":conversation_id,"muted":muted,"pushMutes":all}))
        }
        Action::Inspect { agent_id } => inspect(shared, profile, agent_id).await,
        Action::Screenshot { conversation_id } => {
            {
                let mut last = shared.screenshots.lock().map_err(|_| Refusal {
                    code: "panel",
                    message: "Screenshot state unavailable".into(),
                })?;
                let now = Instant::now();
                if last
                    .get(&conversation_id)
                    .is_some_and(|at| now.duration_since(*at) < SCREENSHOT_INTERVAL)
                {
                    return Err(Refusal {
                        code: "rateLimited",
                        message: "One screenshot per conversation every 2 seconds".into(),
                    });
                }
                last.retain(|_, at| now.duration_since(*at) < SCREENSHOT_INTERVAL);
                last.insert(conversation_id.clone(), now);
            }
            // Only a conversation of this device's profile can be captured.
            conversation_row(shared, profile, &conversation_id).await?;
            capture(&conversation_id).await.map_err(|message| Refusal {
                code: "screenshot",
                message,
            })
        }
        Action::Subscribe { .. } | Action::Unsubscribe { .. } => unreachable!("handled inline"),
    }
}

/// `agentInspect` starts the agent's CLI to list its models, so a phone gets
/// at most one real inspection per agent and profile every 30 s; within that
/// window the last result (or refusal) is returned again.
async fn inspect(shared: &Arc<Shared>, profile: &str, agent: String) -> Result<Value, Refusal> {
    let key = (profile.to_string(), agent.clone());
    {
        let mut cache = shared.inspections.lock().map_err(|_| Refusal {
            code: "panel",
            message: "Inspection state unavailable".into(),
        })?;
        let now = Instant::now();
        cache.retain(|_, (at, _)| now.duration_since(*at) < INSPECT_INTERVAL);
        match cache.get(&key) {
            Some((_, Some(Ok(result)))) => return Ok(result.clone()),
            Some((_, Some(Err(message)))) => return Err(panel_error(message.clone())),
            Some((_, None)) => {
                return Err(Refusal {
                    code: "rateLimited",
                    message: "This agent is already being inspected".into(),
                })
            }
            None => {
                cache.insert(key.clone(), (now, None));
            }
        }
    }
    let result = shared
        .panel
        .call("agentInspect", json!({"agentId":agent}), profile)
        .await;
    if let Ok(mut cache) = shared.inspections.lock() {
        if let Some(entry) = cache.get_mut(&key) {
            entry.1 = Some(result.clone());
        }
    }
    result.map_err(panel_error)
}

/// Refuse a conversation or schedule that acts beyond what a phone may start
/// unless this device was allowed full access on the computer.
async fn require_access(
    shared: &Arc<Shared>,
    session: &Session,
    row: &Value,
) -> Result<(), Refusal> {
    let elevated = policy::elevated(
        row["agentId"].as_str().unwrap_or(""),
        row["options"]["toolAccess"].as_str(),
        row["fullAccess"] == true,
    );
    if !elevated || device_allows_full_access(shared, &session.device).await {
        return Ok(());
    }
    Err(Refusal {
        code: "accessTooHigh",
        message: "This conversation runs with full access, commands or Pi tools. Allow it for this phone on the computer: chrome-use mobile access <device> full".into(),
    })
}

async fn device_allows_full_access(shared: &Arc<Shared>, device: &str) -> bool {
    let store = shared.store.clone();
    let device = device.to_string();
    tokio::task::spawn_blocking(move || store.read())
        .await
        .ok()
        .and_then(Result::ok)
        .and_then(|r| r.device(&device).map(|d| d.allow_full_access))
        .unwrap_or(false)
}

async fn schedule_row(shared: &Arc<Shared>, profile: &str, id: &str) -> Result<Value, Refusal> {
    let list = shared
        .panel
        .call("scheduleList", json!({}), profile)
        .await
        .map_err(panel_error)?;
    list["schedules"]
        .as_array()
        .and_then(|rows| rows.iter().find(|r| r["id"] == id))
        .cloned()
        .ok_or_else(|| Refusal {
            code: "notFound",
            message: "Schedule not found".into(),
        })
}

async fn conversation_row(
    shared: &Arc<Shared>,
    profile: &str,
    conversation_id: &str,
) -> Result<Value, Refusal> {
    let list = shared
        .panel
        .call("conversationList", json!({}), profile)
        .await
        .map_err(panel_error)?;
    list["conversations"]
        .as_array()
        .and_then(|rows| rows.iter().find(|r| r["id"] == conversation_id))
        .cloned()
        .ok_or_else(|| Refusal {
            code: "notFound",
            message: "Conversation was not found in this profile".into(),
        })
}

async fn set_relay_token(
    shared: &Arc<Shared>,
    device: &str,
    token: Option<String>,
) -> Result<(), Refusal> {
    let store = shared.store.clone();
    let device = device.to_string();
    tokio::task::spawn_blocking(move || {
        store.update(|r| {
            r.device_mut(&device)
                .ok_or("Device was revoked")?
                .relay_token = token;
            Ok(())
        })
    })
    .await
    .map_err(|_| "Device store unavailable".to_string())
    .and_then(|r| r)
    .map_err(|message| Refusal {
        code: "store",
        message,
    })
}

fn busy(phase: &Value) -> bool {
    matches!(
        phase.as_str(),
        Some("starting" | "running" | "waitingInput" | "stopping")
    )
}

/// Poll one conversation's journal for a subscribed phone, at the panel's
/// own cadence, from the phone's cursor. Events are sent in order and the
/// cursor travels with them, so a reconnect resumes without gaps or repeats.
async fn follow(
    panel: Panel,
    profile: String,
    conversation: String,
    mut cursor: u64,
    outbox: Outbox,
) {
    let mut last_summary = Value::Null;
    let mut reported = false;
    loop {
        let read = panel
            .poll(
                "conversationRead",
                json!({"conversationId":conversation,"after":cursor}),
                &profile,
            )
            .await;
        match read {
            Ok(reply) => {
                reported = false;
                let events = reply["journal"]["events"].clone();
                let next = reply["journal"]["cursor"].as_u64().unwrap_or(cursor);
                let more = reply["journal"]["more"] == true;
                let summary = reply["conversation"].clone();
                let has_events = events.as_array().is_some_and(|e| !e.is_empty());
                if has_events || summary != last_summary {
                    let frame = json!({"event":"journal","conversationId":conversation,"conversation":summary,"events":events,"cursor":next,"more":more,"queuedMessages":reply["queuedMessages"]});
                    if outbox.send(frame).await.is_err() {
                        return;
                    }
                }
                cursor = next;
                let wait = if busy(&summary["phase"]) { 600 } else { 1800 };
                last_summary = summary;
                if more {
                    continue;
                }
                tokio::time::sleep(Duration::from_millis(wait)).await;
            }
            Err(error) => {
                if error.contains("not found") {
                    let _ = outbox
                        .send(json!({"event":"subscriptionEnded","conversationId":conversation,"reason":error}))
                        .await;
                    return;
                }
                if !reported {
                    reported = true;
                    let frame = json!({"event":"subscriptionError","conversationId":conversation,"error":error});
                    if outbox.send(frame).await.is_err() {
                        return;
                    }
                }
                tokio::time::sleep(Duration::from_secs(3)).await;
            }
        }
    }
}

/// Capture the conversation's own browser session (`panel-<id>`, the session
/// the panel service gives its agent) as a downscaled JPEG. The command goes
/// straight to that session's running daemon: none is started, none is
/// restarted, so a phone never opens a browser.
async fn capture(conversation: &str) -> Result<Value, String> {
    let session = format!("panel-{conversation}");
    if cfg!(test) {
        return Err("This conversation has no open browser session".into());
    }
    let directory = tempfile::tempdir().map_err(|_| "Cannot create a screenshot folder")?;
    let path = directory.path().join("screenshot.png");
    let command = json!({"id":"mobile-screenshot","action":"screenshot","path":path,"base64":true});
    let reply = tokio::time::timeout(
        Duration::from_secs(20),
        tokio::task::spawn_blocking(move || {
            crate::connection::send_to_running_daemon(&command, &session)
        }),
    )
    .await
    .map_err(|_| "The screenshot did not finish within 20 seconds")?
    .map_err(|_| "The screenshot did not finish")?
    .map_err(|_| "This conversation has no open browser session")?;
    if !reply.success {
        return Err(reply
            .error
            .unwrap_or_else(|| "The browser session could not take a screenshot".into()));
    }
    use base64::Engine;
    let bytes = reply
        .data
        .as_ref()
        .and_then(|d| d["base64"].as_str())
        .and_then(|b| base64::engine::general_purpose::STANDARD.decode(b).ok())
        .ok_or("The browser session returned no image")?;
    drop(directory);
    tokio::task::spawn_blocking(move || jpeg(&bytes))
        .await
        .map_err(|_| "Screenshot encoding failed".to_string())?
}

pub(crate) fn jpeg(bytes: &[u8]) -> Result<Value, String> {
    use base64::Engine;
    let mut image = image::load_from_memory(bytes).map_err(|_| "Unreadable screenshot")?;
    if image.width() > SCREENSHOT_WIDTH {
        let height =
            (image.height() as u64 * SCREENSHOT_WIDTH as u64 / image.width() as u64).max(1) as u32;
        image = image.resize_exact(
            SCREENSHOT_WIDTH,
            height,
            image::imageops::FilterType::Triangle,
        );
    }
    let rgb = image.to_rgb8();
    let mut out = Vec::new();
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, 70)
        .encode_image(&rgb)
        .map_err(|_| "Screenshot encoding failed")?;
    Ok(
        json!({"mediaType":"image/jpeg","width":rgb.width(),"height":rgb.height(),"data":base64::engine::general_purpose::STANDARD.encode(out)}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn bind_waits_for_missing_address_without_claiming_readiness() {
        let address = "192.0.2.123:17345".parse().unwrap();
        assert!(
            tokio::time::timeout(Duration::from_millis(50), bind_when_available(address))
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn bind_reports_occupied_port_and_accepts_available_address() {
        let listener = bind_when_available("127.0.0.1:0".parse().unwrap())
            .await
            .unwrap();
        let address = listener.local_addr().unwrap();
        let error = tokio::time::timeout(Duration::from_secs(1), bind_when_available(address))
            .await
            .expect("occupied ports must fail without retrying")
            .unwrap_err();
        assert!(error.contains(&format!("Cannot listen on {address}")));
    }

    #[test]
    fn backoff_starts_after_free_failures_and_clears_on_success() {
        let mut limiter = Limiter::default();
        let ip: IpAddr = "203.0.113.9".parse().unwrap();
        let now = Instant::now();
        for _ in 0..FREE_FAILURES - 1 {
            limiter.fail(ip, Some("d"), now);
        }
        assert!(!limiter.source_blocked(ip, now));
        limiter.fail(ip, Some("d"), now);
        assert!(limiter.source_blocked(ip, now));
        assert!(limiter.device_blocked(ip, "d", now));
        // Backoff for a device is per source: another address is not held.
        let other: IpAddr = "198.51.100.7".parse().unwrap();
        assert!(!limiter.device_blocked(other, "d", now));
        assert!(!limiter.source_blocked(ip, now + Duration::from_secs(2)));
        for _ in 0..40 {
            limiter.fail(ip, None, now);
        }
        assert!(limiter.source_blocked(ip, now + Duration::from_secs(299)));
        assert!(!limiter.source_blocked(ip, now + MAX_BACKOFF + Duration::from_secs(1)));
        limiter.succeed(ip, "d");
        assert!(!limiter.source_blocked(ip, now));
        assert!(!limiter.device_blocked(ip, "d", now));
    }

    #[test]
    fn ipv6_sources_share_their_64_and_tables_stay_bounded() {
        let a: IpAddr = "2001:db8:1:2:aaaa::1".parse().unwrap();
        let b: IpAddr = "2001:db8:1:2:bbbb::9".parse().unwrap();
        let c: IpAddr = "2001:db8:1:3::1".parse().unwrap();
        assert_eq!(source_key(a), source_key(b));
        assert_ne!(source_key(a), source_key(c));
        let mapped: IpAddr = "::ffff:203.0.113.9".parse().unwrap();
        assert_eq!(source_key(mapped), "203.0.113.9".parse::<IpAddr>().unwrap());
        let mut limiter = Limiter::default();
        let now = Instant::now();
        for _ in 0..FREE_FAILURES {
            limiter.fail(a, None, now);
        }
        assert!(limiter.source_blocked(b, now));
        let start = Instant::now();
        for i in 0..(LIMITER_ENTRIES as u32 + 10) {
            let ip = IpAddr::V4(std::net::Ipv4Addr::from(0x0a00_0000 + i));
            limiter.fail(ip, None, start + Duration::from_millis(i as u64));
        }
        assert!(limiter.len() <= LIMITER_ENTRIES);
    }

    #[test]
    fn browser_origins_are_refused_at_upgrade() {
        let request = |origin: Option<&str>| {
            let mut builder = Request::builder().uri("/");
            if let Some(origin) = origin {
                builder = builder.header("Origin", origin);
            }
            builder.body(()).unwrap()
        };
        assert!(check_upgrade(&request(None)).is_ok());
        let refused = check_upgrade(&request(Some("https://evil.example"))).unwrap_err();
        assert_eq!(refused.status(), 403);
    }

    #[test]
    fn screenshots_are_downscaled_jpeg() {
        let image = image::DynamicImage::new_rgb8(2160, 1200);
        let mut png = std::io::Cursor::new(Vec::new());
        image.write_to(&mut png, image::ImageFormat::Png).unwrap();
        let shot = jpeg(&png.into_inner()).unwrap();
        assert_eq!(shot["mediaType"], "image/jpeg");
        assert_eq!(shot["width"], 1080);
        assert_eq!(shot["height"], 600);
    }
}
