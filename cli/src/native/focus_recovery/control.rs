//! Private foreground control protocol, exercised before product integration.
//! No PID, OS call, public wire DTO, new cancellation protocol, or lease owner.
#![allow(dead_code)]
use serde::{Deserialize, Serialize};
use std::{future::Future, time::Duration};
use tokio::{
    io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader},
    sync::{mpsc, oneshot},
    time::{self, Instant, MissedTickBehavior},
};

const LIMIT: usize = 4096;
const WAIT: Duration = Duration::from_secs(3);
const PREFIX: &str = "{\"_chrome_use_focus_control\":";
const OFFER: &str = "_chrome_use_focus_offer";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct RelayHint {
    pub local_port: u16,
    pub host_port: u16,
    pub generation: u64,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", deny_unknown_fields)]
pub(super) enum Decision {
    AlreadyForegroundProcess,
    Admitted,
    OsDenied { code: u32 },
    Unavailable { reason: String },
    Refused { reason: String },
}
impl Decision {
    fn valid(&self) -> bool {
        match self {
            Self::Unavailable { reason } | Self::Refused { reason } => reason.len() <= 256,
            _ => true,
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum GrantError {
    Unavailable,
    Protocol,
    Timeout,
    Eof,
    Transport,
    Cancelled,
    Refused,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Offer {
    version: u8,
    nonce: String,
}
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Envelope {
    _chrome_use_focus_control: Frame,
}
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "kind", deny_unknown_fields)]
enum Frame {
    Request {
        version: u8,
        nonce: String,
        serial: u8,
        hint: RelayHint,
    },
    Ack {
        version: u8,
        nonce: String,
        serial: u8,
        generation: u64,
        decision: Decision,
    },
}
fn nonce_ok(s: &str) -> bool {
    s.len() == 64
        && s.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
fn offer_valid(o: &Offer) -> bool {
    o.version == 1 && nonce_ok(&o.nonce)
}
fn line(frame: Frame) -> Result<Vec<u8>, GrantError> {
    let mut b = serde_json::to_vec(&Envelope {
        _chrome_use_focus_control: frame,
    })
    .map_err(|_| GrantError::Protocol)?;
    b.push(b'\n');
    if b.len() > LIMIT {
        return Err(GrantError::Protocol);
    }
    Ok(b)
}
// Called only for Windows explicit boolean true. `nonce` MUST be 32 CSPRNG bytes;
// generation belongs to caller so this module has no RNG or platform dependency.
pub(super) fn attach_offer(
    cmd: &mut serde_json::Value,
    enabled: bool,
    nonce: [u8; 32],
) -> Result<Option<Client>, GrantError> {
    if !enabled {
        return Ok(None);
    }
    let obj = cmd.as_object_mut().ok_or(GrantError::Protocol)?;
    if obj.contains_key(OFFER) {
        return Err(GrantError::Protocol);
    }
    let nonce: String = nonce.iter().map(|b| format!("{b:02x}")).collect();
    obj.insert(
        OFFER.into(),
        serde_json::to_value(Offer {
            version: 1,
            nonce: nonce.clone(),
        })
        .map_err(|_| GrantError::Protocol)?,
    );
    Ok(Some(Client { nonce, used: false }))
}
// Call before *any* generic command dispatch, including daemon/bootstrap routing.
// Offers use a different key and are allowed on ordinary action commands. Any
// top-level control key is reserved, even if other fields (e.g. action) exist.
pub(super) fn reject_reserved_command(cmd: &serde_json::Value) -> Result<(), GrantError> {
    if cmd
        .as_object()
        .is_some_and(|o| o.contains_key("_chrome_use_focus_control"))
    {
        Err(GrantError::Protocol)
    } else {
        Ok(())
    }
}
// A bad/missing offer is remembered, not rejected during an otherwise normal command.
pub(super) fn service(cmd: &serde_json::Value, enabled: bool) -> (Broker, Service) {
    let offer = if !enabled || cmd.get(OFFER).is_none() {
        Err(GrantError::Unavailable)
    } else {
        match serde_json::from_value::<Offer>(cmd[OFFER].clone()) {
            Ok(o) if o.version != 1 => Err(GrantError::Unavailable),
            Ok(o) if offer_valid(&o) => Ok(o),
            _ => Err(GrantError::Protocol),
        }
    };
    let (tx, rx) = mpsc::channel(1);
    (
        Broker { tx: Some(tx) },
        Service {
            offer,
            rx,
            used: false,
        },
    )
}
struct Request {
    hint: RelayHint,
    reply: oneshot::Sender<Result<Decision, GrantError>>,
}
pub(super) struct Broker {
    tx: Option<mpsc::Sender<Request>>,
}
impl Broker {
    // Owner calls after consuming the existing permit and registering the one lease.
    pub(super) async fn request_once(&mut self, hint: RelayHint) -> Result<Decision, GrantError> {
        let tx = self.tx.take().ok_or(GrantError::Protocol)?;
        let (reply, rx) = oneshot::channel();
        tx.send(Request { hint, reply })
            .await
            .map_err(|_| GrantError::Unavailable)?;
        rx.await.map_err(|_| GrantError::Unavailable)?
    }
}
pub(super) struct Service {
    offer: Result<Offer, GrantError>,
    rx: mpsc::Receiver<Request>,
    used: bool,
}
pub(super) struct Client {
    nonce: String,
    used: bool,
}
pub(super) enum ClientLine {
    Ordinary,
    Request(ClientRequest),
}
pub(super) struct ClientRequest {
    nonce: String,
    hint: RelayHint,
}
impl Client {
    // Inspect only the entire top-level line before Response parsing, never its data field.
    pub(super) fn inspect(&mut self, raw: &[u8]) -> Result<ClientLine, GrantError> {
        if !raw.starts_with(PREFIX.as_bytes()) {
            // Only our canonical envelope can authorize a callback. Reject a
            // reserved top-level key with other ordering/spacing too: Response
            // intentionally tolerates extra fields and must not swallow it.
            if serde_json::from_slice::<serde_json::Value>(raw)
                .ok()
                .is_some_and(|value| value.get("_chrome_use_focus_control").is_some())
            {
                return Err(GrantError::Protocol);
            }
            return Ok(ClientLine::Ordinary);
        }
        if self.used || raw.len() > LIMIT {
            return Err(GrantError::Protocol);
        }
        let env: Envelope = serde_json::from_slice(raw).map_err(|_| GrantError::Protocol)?;
        match env._chrome_use_focus_control {
            Frame::Request {
                version: 1,
                nonce,
                serial: 1,
                hint,
            } if nonce == self.nonce && hint.local_port != 0 && hint.host_port != 0 => {
                self.used = true;
                Ok(ClientLine::Request(ClientRequest { nonce, hint }))
            }
            _ => Err(GrantError::Protocol),
        }
    }
}
impl ClientRequest {
    pub(super) fn hint(&self) -> RelayHint {
        self.hint
    }
    // Called once after the platform authorizer independently checks real sockets.
    pub(super) fn reply(self, decision: Decision) -> Result<Vec<u8>, GrantError> {
        if !decision.valid() {
            return Err(GrantError::Protocol);
        }
        line(Frame::Ack {
            version: 1,
            nonce: self.nonce,
            serial: 1,
            generation: self.hint.generation,
            decision,
        })
    }
}
// Max-length reader. Never buffer an unbounded line; no timeout renewal per byte.
async fn bounded_line<R: AsyncRead + Unpin>(r: &mut BufReader<R>) -> Result<Vec<u8>, GrantError> {
    let mut v = Vec::new();
    loop {
        let buf = r.fill_buf().await.map_err(|_| GrantError::Transport)?;
        if buf.is_empty() {
            return Err(GrantError::Eof);
        }
        let n = buf
            .iter()
            .position(|b| *b == b'\n')
            .map_or(buf.len(), |i| i + 1);
        if v.len() + n > LIMIT {
            return Err(GrantError::Protocol);
        }
        v.extend_from_slice(&buf[..n]);
        let done = buf[n - 1] == b'\n';
        r.consume(n);
        if done {
            return Ok(v);
        }
    }
}
fn parse_ack(raw: &[u8], offer: &Offer, hint: RelayHint) -> Result<Decision, GrantError> {
    if !raw.starts_with(PREFIX.as_bytes()) {
        return Err(GrantError::Protocol);
    }
    let env: Envelope = serde_json::from_slice(raw).map_err(|_| GrantError::Protocol)?;
    match env._chrome_use_focus_control {
        Frame::Ack {
            version: 1,
            nonce,
            serial: 1,
            generation,
            decision,
        } if nonce == offer.nonce && generation == hint.generation && decision.valid() => {
            match decision {
                Decision::Refused { .. } => Err(GrantError::Refused),
                d => Ok(d),
            }
        }
        _ => Err(GrantError::Protocol),
    }
}
pub(super) struct DriverOutcome<T> {
    pub value: T,
    pub transport_usable: bool,
    pub close_after_final: bool,
}
// Sole reader/writer owner while the caller's command holds the session mutex.
// Writer errors do not cancel an already-running command. Once control has been
// sent, unread duplicate/late input makes this connection non-reusable.
pub(super) async fn drive<R, W, F>(
    reader: &mut BufReader<R>,
    writer: &mut W,
    mut service: Service,
    keepalive: bool,
    period: Duration,
    command: F,
) -> DriverOutcome<F::Output>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
    F: Future,
{
    tokio::pin!(command);
    let mut usable = true;
    let mut sent_control = false;
    let mut ticks = time::interval_at(Instant::now() + period, period);
    ticks.set_missed_tick_behavior(MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            value = &mut command => return DriverOutcome { value, transport_usable: usable, close_after_final: sent_control },
            request = service.rx.recv(), if !service.used => {
                let Some(mut req) = request else { service.used = true; continue };
                service.used = true;
                if req.reply.is_closed() { continue; }
                if !usable { let _ = req.reply.send(Err(GrantError::Transport)); continue; }
                let offer = match &service.offer { Ok(o) => o.clone(), Err(e) => { let _ = req.reply.send(Err(e.clone())); continue; } };
                if req.hint.local_port == 0 || req.hint.host_port == 0 { let _ = req.reply.send(Err(GrantError::Protocol)); continue; }
                let bytes = match line(Frame::Request { version: 1, nonce: offer.nonce.clone(), serial: 1, hint: req.hint }) {
                    Ok(b) => b, Err(e) => { let _ = req.reply.send(Err(e)); continue; }
                };
                let deadline = Instant::now() + WAIT;
                // A dropped waiter before sending is skipped. Once bytes are delivered,
                // a client may still authorize even if the waiter is later dropped.
                // Once a write starts, a dropped waiter makes the stream unusable; client must
                // check its own request deadline before authorization (see adapter notes).
                let wrote = tokio::select! {
                    value = &mut command => return DriverOutcome { value, transport_usable: false, close_after_final: true },
                    _ = req.reply.closed() => { usable = false; false },
                    result = time::timeout_at(deadline, writer.write_all(&bytes)) => match result { Ok(Ok(())) => true, _ => { usable = false; false } },
                };
                if !wrote { let _ = req.reply.send(Err(GrantError::Transport)); continue; }
                sent_control = true;
                // A sent control frame means this stream must not accept another command: a
                // late acknowledgement could otherwise be mistaken for a root request.
                // It may still carry the current final response after a valid ack.
                let read_ack = bounded_line(reader);
                tokio::pin!(read_ack);
                let result = loop {
                    tokio::select! {
                        value = &mut command => return DriverOutcome { value, transport_usable: false, close_after_final: true },
                        _ = req.reply.closed() => { usable = false; break Err(GrantError::Cancelled); },
                        _ = time::sleep_until(deadline) => { usable = false; break Err(GrantError::Timeout); },
                        raw = &mut read_ack => { let v = raw.and_then(|b| parse_ack(&b, &offer, req.hint)); if v.is_err() { usable = false; } break v; },
                        _ = ticks.tick(), if keepalive && usable => {
                            // A blocked write must not hold the command or callback deadline.
                            let ok = tokio::select! {
                                value = &mut command => return DriverOutcome { value, transport_usable: false, close_after_final: true },
                                _ = req.reply.closed() => false,
                                _ = time::sleep_until(deadline) => false,
                                result = writer.write_all(b"\n") => result.is_ok(),
                            };
                            if !ok { usable = false; break Err(GrantError::Transport); }
                        }
                    }
                };
                let _ = req.reply.send(result);
            },
            _ = ticks.tick(), if keepalive && usable => {
                let ok = tokio::select! {
                    value = &mut command => return DriverOutcome { value, transport_usable: false, close_after_final: true },
                    result = writer.write_all(b"\n") => result.is_ok(),
                };
                if !ok { usable = false; }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt};
    fn hint() -> RelayHint {
        RelayHint {
            local_port: 12,
            host_port: 34,
            generation: 7,
        }
    }
    fn setup(enabled: bool) -> (serde_json::Value, Option<Client>, Broker, Service) {
        let mut cmd = serde_json::json!({"action":"click"});
        let c = attach_offer(&mut cmd, enabled, [42; 32]).unwrap();
        let (b, s) = service(&cmd, enabled);
        (cmd, c, b, s)
    }
    async fn session(decision: Decision) -> (Result<Decision, GrantError>, bool) {
        let (_, mut client, mut broker, svc) = setup(true);
        let (a, b) = tokio::io::duplex(8192);
        let (ar, mut aw) = tokio::io::split(a);
        let (br, mut bw) = tokio::io::split(b);
        let mut reader = BufReader::new(ar);
        let mut peer = BufReader::new(br);
        let client_task = async move {
            let mut line = String::new();
            loop {
                line.clear();
                peer.read_line(&mut line).await.unwrap();
                if line != "\n" {
                    break;
                }
            }
            let ClientLine::Request(req) =
                client.as_mut().unwrap().inspect(line.as_bytes()).unwrap()
            else {
                panic!()
            };
            assert_eq!(req.hint(), hint());
            bw.write_all(&req.reply(decision).unwrap()).await.unwrap();
        };
        let command = async move { broker.request_once(hint()).await };
        let (out, ()) = tokio::join!(
            drive(
                &mut reader,
                &mut aw,
                svc,
                true,
                Duration::from_millis(1),
                command
            ),
            client_task
        );
        (out.value, out.close_after_final)
    }
    #[tokio::test]
    async fn full_exchange_and_decisions() {
        assert_eq!(
            session(Decision::Admitted).await,
            (Ok(Decision::Admitted), true)
        );
        assert_eq!(
            session(Decision::OsDenied { code: 5 }).await.0,
            Ok(Decision::OsDenied { code: 5 })
        );
        assert_eq!(
            session(Decision::Unavailable {
                reason: "legacy".into()
            })
            .await
            .0,
            Ok(Decision::Unavailable {
                reason: "legacy".into()
            })
        );
        assert_eq!(
            session(Decision::Refused {
                reason: "identity".into()
            })
            .await
            .0,
            Err(GrantError::Refused)
        );
    }
    #[tokio::test]
    async fn default_and_legacy_emit_no_control() {
        for enabled in [false, true] {
            let (mut cmd, _, _, _) = setup(false);
            if enabled {
                cmd[OFFER] = serde_json::json!({"version":99,"nonce":"bad"});
            }
            let (mut broker, svc) = service(&cmd, enabled);
            let (a, mut peer) = tokio::io::duplex(8192);
            let (r, mut w) = tokio::io::split(a);
            let mut r = BufReader::new(r);
            let out = drive(
                &mut r,
                &mut w,
                svc,
                false,
                Duration::from_secs(1),
                async move { broker.request_once(hint()).await },
            )
            .await;
            assert_eq!(out.value, Err(GrantError::Unavailable));
            assert!(out.transport_usable);
            drop(w);
            // tokio::io::split keeps the duplex transport alive until both
            // halves drop. Close the test reader before waiting for peer EOF.
            drop(r);
            let mut buf = Vec::new();
            tokio::io::AsyncReadExt::read_to_end(&mut peer, &mut buf)
                .await
                .unwrap();
            assert!(buf.is_empty());
        }
    }
    #[test]
    fn page_data_is_opaque_and_duplicate_is_rejected() {
        let (_, mut client, _, _) = setup(true);
        let c = client.as_mut().unwrap();
        for raw in [
            b"{\"success\":true,\"_chrome_use_focus_control\":{}}\n".as_slice(),
            b" {\"_chrome_use_focus_control\":{}}\n".as_slice(),
        ] {
            assert!(matches!(c.inspect(raw), Err(GrantError::Protocol)));
        }
        assert!(matches!(
            c.inspect(b"{\"success\":true,\"data\":{\"_chrome_use_focus_control\":{}}}\n")
                .unwrap(),
            ClientLine::Ordinary
        ));
        let raw = line(Frame::Request {
            version: 1,
            nonce: c.nonce.clone(),
            serial: 1,
            hint: hint(),
        })
        .unwrap();
        assert!(matches!(c.inspect(&raw).unwrap(), ClientLine::Request(_)));
        assert!(matches!(c.inspect(&raw), Err(GrantError::Protocol)));
    }
    #[test]
    fn wrong_and_malformed_ack_are_hard_errors() {
        let offer = Offer {
            version: 1,
            nonce: "aa".repeat(32),
        };
        let bad = line(Frame::Ack {
            version: 1,
            nonce: "bb".repeat(32),
            serial: 1,
            generation: 7,
            decision: Decision::Admitted,
        })
        .unwrap();
        assert_eq!(parse_ack(&bad, &offer, hint()), Err(GrantError::Protocol));
        let late = line(Frame::Ack {
            version: 1,
            nonce: offer.nonce.clone(),
            serial: 1,
            generation: 8,
            decision: Decision::Admitted,
        })
        .unwrap();
        assert_eq!(parse_ack(&late, &offer, hint()), Err(GrantError::Protocol));
        assert_eq!(
            parse_ack(b"{\"_chrome_use_focus_control\":{}}\n", &offer, hint()),
            Err(GrantError::Protocol)
        );
    }
    #[tokio::test]
    async fn eof_returns_error_before_recovery_continues() {
        let (_, _, mut broker, svc) = setup(true);
        let (a, b) = tokio::io::duplex(8192);
        let (r, mut w) = tokio::io::split(a);
        let mut r = BufReader::new(r);
        let peer = async move {
            let mut reader = BufReader::new(b);
            let mut l = String::new();
            reader.read_line(&mut l).await.unwrap();
            drop(reader);
        };
        let cmd = async move { broker.request_once(hint()).await };
        let (out, ()) = tokio::join!(
            drive(&mut r, &mut w, svc, false, Duration::from_secs(1), cmd),
            peer
        );
        assert_eq!(out.value, Err(GrantError::Eof));
        assert!(!out.transport_usable);
    }
    #[tokio::test]
    async fn heartbeat_failure_does_not_cancel_started_command() {
        let (_, _, _broker, svc) = setup(false);
        let (a, b) = tokio::io::duplex(32);
        drop(b);
        let (r, mut w) = tokio::io::split(a);
        let mut r = BufReader::new(r);
        let out = drive(&mut r, &mut w, svc, true, Duration::from_millis(1), async {
            time::sleep(Duration::from_millis(10)).await;
            42
        })
        .await;
        assert_eq!(out.value, 42);
        assert!(!out.transport_usable);
    }
    #[tokio::test]
    async fn timeout_is_bounded_and_no_final_after_partial_control() {
        let (_, _, mut broker, svc) = setup(true);
        let (a, _b) = tokio::io::duplex(8192);
        let (r, mut w) = tokio::io::split(a);
        let mut r = BufReader::new(r);
        let out = drive(
            &mut r,
            &mut w,
            svc,
            false,
            Duration::from_secs(1),
            async move { broker.request_once(hint()).await },
        )
        .await;
        assert_eq!(out.value, Err(GrantError::Timeout));
        assert!(!out.transport_usable);
    }
    #[tokio::test]
    async fn fragmented_and_oversized_lines() {
        let (mut tx, rx) = tokio::io::duplex(8192);
        let mut reader = BufReader::new(rx);
        let write = async move {
            tx.write_all(b"abc").await.unwrap();
            tokio::task::yield_now().await;
            tx.write_all(b"def\n").await.unwrap();
        };
        let (got, ()) = tokio::join!(bounded_line(&mut reader), write);
        assert_eq!(got.unwrap(), b"abcdef\n");
        let (mut tx, rx) = tokio::io::duplex(8192);
        let mut reader = BufReader::new(rx);
        let write = async move {
            tx.write_all(&vec![b'x'; LIMIT + 1]).await.unwrap();
        };
        let (got, ()) = tokio::join!(bounded_line(&mut reader), write);
        assert_eq!(got, Err(GrantError::Protocol));
    }
    #[tokio::test]
    async fn locally_abandoned_request_is_skipped_before_send() {
        use std::{
            future::{pending, poll_fn, Future},
            task::Poll,
        };
        use tokio::io::AsyncRead;
        let (_, _, broker, svc) = setup(true);
        let (reply, rx) = oneshot::channel();
        drop(rx);
        broker
            .tx
            .as_ref()
            .unwrap()
            .send(Request {
                hint: hint(),
                reply,
            })
            .await
            .unwrap();
        let (a, mut peer) = tokio::io::duplex(128);
        let (r, mut w) = tokio::io::split(a);
        let mut r = BufReader::new(r);
        let f = drive(
            &mut r,
            &mut w,
            svc,
            false,
            Duration::from_secs(1),
            pending::<()>(),
        );
        tokio::pin!(f);
        // One explicit poll drains the queued abandoned request and leaves the command pending.
        poll_fn(|cx| {
            assert!(f.as_mut().poll(cx).is_pending());
            Poll::Ready(())
        })
        .await;
        let mut b = [0u8; 1];
        let mut read_buf = tokio::io::ReadBuf::new(&mut b);
        poll_fn(|cx| {
            assert!(std::pin::Pin::new(&mut peer)
                .poll_read(cx, &mut read_buf)
                .is_pending());
            Poll::Ready(())
        })
        .await;
    }
    #[test]
    fn reserved_control_cannot_become_a_command() {
        for v in [
            serde_json::json!({"_chrome_use_focus_control":{}}),
            serde_json::json!({"action":"open","_chrome_use_focus_control":{}}),
        ] {
            assert_eq!(reject_reserved_command(&v), Err(GrantError::Protocol));
        }
        assert_eq!(
            reject_reserved_command(
                &serde_json::json!({"action":"open","_chrome_use_focus_offer":{}})
            ),
            Ok(())
        );
        // A late or duplicate ACK uses the same reserved key and is rejected regardless of its content.
        let ack = line(Frame::Ack {
            version: 1,
            nonce: "aa".repeat(32),
            serial: 1,
            generation: 7,
            decision: Decision::Admitted,
        })
        .unwrap();
        let v: serde_json::Value = serde_json::from_slice(&ack).unwrap();
        assert_eq!(reject_reserved_command(&v), Err(GrantError::Protocol));
    }
    struct PartialWriter {
        bytes: std::sync::Arc<std::sync::Mutex<Vec<u8>>>,
        entered: Option<oneshot::Sender<()>>,
    }
    impl tokio::io::AsyncWrite for PartialWriter {
        fn poll_write(
            mut self: std::pin::Pin<&mut Self>,
            _cx: &mut std::task::Context<'_>,
            buf: &[u8],
        ) -> std::task::Poll<std::io::Result<usize>> {
            if let Some(tx) = self.entered.take() {
                self.bytes.lock().unwrap().push(buf[0]);
                let _ = tx.send(());
                return std::task::Poll::Ready(Ok(1));
            }
            std::task::Poll::Pending
        }
        fn poll_flush(
            self: std::pin::Pin<&mut Self>,
            _: &mut std::task::Context<'_>,
        ) -> std::task::Poll<std::io::Result<()>> {
            std::task::Poll::Ready(Ok(()))
        }
        fn poll_shutdown(
            self: std::pin::Pin<&mut Self>,
            _: &mut std::task::Context<'_>,
        ) -> std::task::Poll<std::io::Result<()>> {
            std::task::Poll::Ready(Ok(()))
        }
    }
    #[tokio::test]
    async fn interrupted_partial_write_forbids_final_append() {
        let (_, _, broker, svc) = setup(true);
        let (reply, _rx) = oneshot::channel();
        broker
            .tx
            .as_ref()
            .unwrap()
            .send(Request {
                hint: hint(),
                reply,
            })
            .await
            .unwrap();
        let (entered, wait) = oneshot::channel();
        let bytes = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let mut w = PartialWriter {
            bytes: bytes.clone(),
            entered: Some(entered),
        };
        let (_peer, r) = tokio::io::duplex(32);
        let mut r = BufReader::new(r);
        let outcome = drive(&mut r, &mut w, svc, false, Duration::from_secs(1), async {
            wait.await.unwrap();
            7
        })
        .await;
        assert_eq!(outcome.value, 7);
        assert!(!outcome.transport_usable);
        assert!(outcome.close_after_final);
        assert_eq!(bytes.lock().unwrap().len(), 1);
    }
    #[tokio::test]
    async fn heartbeat_and_control_lines_are_serialized() {
        let (_, mut client, mut broker, svc) = setup(true);
        let (a, b) = tokio::io::duplex(8192);
        let (ar, mut aw) = tokio::io::split(a);
        let (br, mut bw) = tokio::io::split(b);
        let mut r = BufReader::new(ar);
        let mut peer = BufReader::new(br);
        let (first, wait) = oneshot::channel();
        let command = async move {
            wait.await.unwrap();
            broker.request_once(hint()).await
        };
        let other = async move {
            let mut l = String::new();
            peer.read_line(&mut l).await.unwrap();
            assert_eq!(l, "\n");
            first.send(()).unwrap();
            loop {
                l.clear();
                peer.read_line(&mut l).await.unwrap();
                if l != "\n" {
                    break;
                }
            }
            assert!(l.starts_with(PREFIX));
            let ClientLine::Request(req) = client.as_mut().unwrap().inspect(l.as_bytes()).unwrap()
            else {
                panic!()
            };
            bw.write_all(&req.reply(Decision::Admitted).unwrap())
                .await
                .unwrap();
        };
        let (out, ()) = tokio::join!(
            drive(
                &mut r,
                &mut aw,
                svc,
                true,
                Duration::from_millis(1),
                command
            ),
            other
        );
        assert_eq!(out.value, Ok(Decision::Admitted));
    }
}
