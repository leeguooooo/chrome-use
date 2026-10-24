//! `chrome-use mobile` end to end, without Chrome, an agent or the real
//! panel service: a fake panel service answers on loopback through a
//! `service.json` in a temporary CHROME_USE_RELAY_DIR, a fake push Worker
//! records pushes, and a phone written here from the spec pairs, talks and
//! receives a push. Run through scripts/remote-cargo.py.
#![cfg(unix)]

use aes_gcm::aead::{Aead, KeyInit, Payload};
use aes_gcm::{Aes256Gcm, Nonce};
use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use base64::Engine;
use futures_util::{SinkExt, StreamExt};
use hmac::{Hmac, Mac};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio_tungstenite::tungstenite::Message;

const PROFILE: &str = "mobile-fixture";
const CONVERSATION: &str = "7d444840-9dc0-41a2-9fb3-1d5b5d3b6c2e";
/// A desktop conversation with full access.
const ELEVATED: &str = "5a1f2b3c-4d5e-4f60-8a7b-9c0d1e2f3a4b";
/// A schedule with full access.
const NIGHTLY: &str = "6b2e3c4d-5e6f-4a70-8b8c-9d0e1f2a3b4c";

fn test_binary() -> PathBuf {
    std::env::var_os("CHROME_USE_TEST_BIN")
        .map(PathBuf::from)
        .unwrap_or_else(|| env!("CARGO_BIN_EXE_chrome-use").into())
}

fn hkdf(ikm: &[u8], salt: &[u8], info: &[u8]) -> [u8; 32] {
    let zeros = [0u8; 32];
    let salt = if salt.is_empty() { &zeros[..] } else { salt };
    let mut mac = <Hmac<Sha256> as Mac>::new_from_slice(salt).unwrap();
    mac.update(ikm);
    let prk = mac.finalize().into_bytes();
    let mut mac = <Hmac<Sha256> as Mac>::new_from_slice(&prk).unwrap();
    mac.update(info);
    mac.update(&[1]);
    mac.finalize().into_bytes().into()
}

struct Keys {
    auth: [u8; 32],
    frame: [u8; 32],
    push: [u8; 32],
}

fn keys(secret: &[u8]) -> Keys {
    let frame = hkdf(secret, b"chrome-use mobile v1", b"frame");
    Keys {
        auth: hkdf(secret, b"chrome-use mobile v1", b"auth"),
        push: hkdf(&frame, b"", b"push"),
        frame,
    }
}

fn proof(key: &[u8; 32], label: &[u8], g: &[u8], p: &[u8], device: &str) -> Vec<u8> {
    let mut mac = <Hmac<Sha256> as Mac>::new_from_slice(key).unwrap();
    mac.update(label);
    mac.update(g);
    mac.update(p);
    mac.update(device.as_bytes());
    mac.finalize().into_bytes().to_vec()
}

/// Shared state of the fake panel service.
#[derive(Default)]
struct Panel {
    phase: String,
    events: Vec<Value>,
    received: Vec<(String, Value)>,
}

fn event(sequence: u64, kind: &str, data: Value) -> Value {
    json!({"sequence":sequence,"kind":kind,"data":data})
}

fn serve_panel(listener: TcpListener, hash: String, state: Arc<Mutex<Panel>>) {
    std::thread::spawn(move || {
        for incoming in listener.incoming() {
            let Ok(mut stream) = incoming else { continue };
            let mut length = [0u8; 4];
            if stream.read_exact(&mut length).is_err() {
                continue;
            }
            let mut bytes = vec![0; u32::from_le_bytes(length) as usize];
            if stream.read_exact(&mut bytes).is_err() {
                continue;
            }
            let request: Value = serde_json::from_slice(&bytes).unwrap();
            assert_eq!(request["capability"], "c".repeat(64));
            let operation = request["operation"].as_str().unwrap().to_string();
            let params = request["params"].clone();
            let mut panel = state.lock().unwrap();
            if operation != "health" {
                panel.received.push((operation.clone(), params.clone()));
            }
            let summary = json!({"id":CONVERSATION,"agentId":"claude","title":"Weekly report","phase":panel.phase,"fullAccess":false,"options":{},"scheduleId":null,"lastTurn":null,"updated":1});
            let reply = match operation.as_str() {
                "health" => json!({"result":{"schemaVersion":1,"binaryHash":hash,"active":0}}),
                "conversationList" => {
                    let elevated = json!({"id":ELEVATED,"agentId":"codex","title":"Deploy","phase":"idle","fullAccess":true,"options":{"toolAccess":"writeFiles"},"scheduleId":null,"lastTurn":null,"updated":1});
                    json!({"result":{"schemaVersion":1,"conversations":[summary, elevated]}})
                }
                "scheduleList" => {
                    json!({"result":{"schemaVersion":1,"schedules":[{"id":NIGHTLY,"name":"Nightly","agentId":"codex","fullAccess":true,"options":{},"enabled":false,"finishedAt":null}]}})
                }
                "scheduleSave" => {
                    json!({"result":{"schemaVersion":1,"schedule":{"id":params["id"],"enabled":params["enabled"]}}})
                }
                "conversationSend" => {
                    panel.phase = "running".into();
                    let n = panel.events.len() as u64 + 1;
                    panel
                        .events
                        .push(event(n, "user", json!({"text":params["text"]})));
                    json!({"result":{"schemaVersion":1,"conversation":summary,"message":{"id":params["messageId"],"accepted":true}}})
                }
                "conversationRead" => {
                    let after = params["after"].as_u64().unwrap_or(0);
                    let events: Vec<Value> = panel
                        .events
                        .iter()
                        .filter(|e| e["sequence"].as_u64().unwrap() > after)
                        .cloned()
                        .collect();
                    let cursor = events
                        .last()
                        .map(|e| e["sequence"].as_u64().unwrap())
                        .unwrap_or(after);
                    json!({"result":{"schemaVersion":1,"conversation":summary,"journal":{"events":events,"cursor":cursor,"more":false},"queuedMessages":[]}})
                }
                "conversationSetOptions" => {
                    json!({"result":{"schemaVersion":1,"conversation":summary}})
                }
                "conversationStop" => json!({"result":{"schemaVersion":1,"conversation":summary}}),
                "agentInspect" => {
                    json!({"result":{"schemaVersion":1,"inspection":{"agentId":params["agentId"],"models":[{"id":"gpt-5"}],"complete":true}}})
                }
                "conversationRespond" => {
                    panel.phase = "running".into();
                    json!({"result":{"schemaVersion":1,"conversation":summary,"response":{"dispatched":true}}})
                }
                _ => json!({"error":"unexpected operation in fixture"}),
            };
            drop(panel);
            let mut reply = reply;
            reply["requestId"] = request["requestId"].clone();
            let raw = reply.to_string().into_bytes();
            let _ = stream.write_all(&(raw.len() as u32).to_le_bytes());
            let _ = stream.write_all(&raw);
        }
    });
}

/// A minimal HTTP/1.1 server standing in for the push Worker.
/// Answers 200 until `gone` is set, then 410 `unregistered`.
fn serve_push(
    listener: TcpListener,
    pushes: Arc<Mutex<Vec<Value>>>,
    gone: Arc<std::sync::atomic::AtomicBool>,
) {
    std::thread::spawn(move || {
        for incoming in listener.incoming() {
            let Ok(stream) = incoming else { continue };
            let mut reader = BufReader::new(stream);
            let mut length = 0usize;
            let mut line = String::new();
            loop {
                line.clear();
                if reader.read_line(&mut line).unwrap_or(0) == 0 {
                    break;
                }
                let lower = line.to_ascii_lowercase();
                if let Some(v) = lower.strip_prefix("content-length:") {
                    length = v.trim().parse().unwrap_or(0);
                }
                if line == "\r\n" {
                    break;
                }
            }
            let mut body = vec![0; length];
            reader.read_exact(&mut body).unwrap();
            pushes
                .lock()
                .unwrap()
                .push(serde_json::from_slice(&body).unwrap());
            let (status, reply): (&str, &[u8]) = if gone.load(std::sync::atomic::Ordering::SeqCst) {
                ("410 Gone", br#"{"status":"unregistered"}"#)
            } else {
                ("200 OK", br#"{"status":"sent"}"#)
            };
            let mut stream = reader.into_inner();
            let _ = write!(
                stream,
                "HTTP/1.1 {status}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
                reply.len()
            );
            let _ = stream.write_all(reply);
        }
    });
}

fn private_dir(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::create_dir_all(path).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).unwrap();
}

struct Gateway(Child);
impl Drop for Gateway {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn start_gateway(relay: &Path, home: &Path, extra: &[&str]) -> (Gateway, String) {
    let mut child = Command::new(test_binary())
        .args(["mobile", "serve", "--bind", "127.0.0.1:0"])
        .args(extra)
        .env("CHROME_USE_RELAY_DIR", relay)
        .env("HOME", home)
        .env("CHROME_USE_NO_UPDATE_CHECK", "1")
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut line = String::new();
    BufReader::new(child.stdout.take().unwrap())
        .read_line(&mut line)
        .unwrap();
    let address = serde_json::from_str::<Value>(&line).unwrap()["listening"]
        .as_str()
        .unwrap()
        .to_string();
    (Gateway(child), address)
}

fn cli(relay: &Path, home: &Path, args: &[&str]) -> Value {
    let output = Command::new(test_binary())
        .arg("mobile")
        .args(args)
        .env("CHROME_USE_RELAY_DIR", relay)
        .env("HOME", home)
        .env("CHROME_USE_NO_UPDATE_CHECK", "1")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "mobile {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}

type Ws =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

struct Phone {
    ws: Ws,
    send: Aes256Gcm,
    receive: Aes256Gcm,
    sent: u64,
    received: u64,
    next_id: u64,
    events: Vec<Value>,
    last_frame: Vec<u8>,
    ikm: Vec<u8>,
}

fn nonce(sequence: u64) -> [u8; 12] {
    let mut n = [0u8; 12];
    n[4..].copy_from_slice(&sequence.to_be_bytes());
    n
}

/// Connect and run the handshake; `Err(close code)` when the gateway refuses.
/// `ikm` is the key material the phone holds: the QR's pair secret when
/// pairing, `pair_secret || phone_secret` afterwards (kept in `Phone::ikm`).
async fn connect(address: &str, device: &str, ikm: &[u8], pairing: bool) -> Result<Phone, u16> {
    let keys = &keys(ikm);
    let (mut ws, _) = tokio_tungstenite::connect_async(format!("ws://{address}/"))
        .await
        .unwrap();
    let Some(Ok(Message::Text(challenge))) = ws.next().await else {
        panic!("no challenge")
    };
    let challenge: Value = serde_json::from_str(&challenge).unwrap();
    assert_eq!(challenge["type"], "challenge");
    let gateway: [u8; 32] = URL_SAFE_NO_PAD
        .decode(challenge["gatewayNonce"].as_str().unwrap())
        .unwrap()
        .try_into()
        .unwrap();
    let phone: [u8; 32] = rand_bytes();
    let mine = proof(
        &keys.auth,
        b"chrome-use mobile v1 phone proof",
        &gateway,
        &phone,
        device,
    );
    let auth = json!({"type":"auth","v":1,"deviceId":device,"phoneNonce":URL_SAFE_NO_PAD.encode(phone),"proof":URL_SAFE_NO_PAD.encode(mine),"pairing":pairing});
    ws.send(Message::Text(auth.to_string())).await.unwrap();
    match ws.next().await {
        Some(Ok(Message::Text(ready))) => {
            let ready: Value = serde_json::from_str(&ready).unwrap();
            assert_eq!(ready["type"], "ready");
            let expected = proof(
                &keys.auth,
                b"chrome-use mobile v1 gateway proof",
                &gateway,
                &phone,
                device,
            );
            assert_eq!(
                URL_SAFE_NO_PAD
                    .decode(ready["proof"].as_str().unwrap())
                    .unwrap(),
                expected,
                "gateway proof"
            );
            let mut salt = gateway.to_vec();
            salt.extend_from_slice(&phone);
            let session = |ws: Ws, keys: &Keys, ikm: Vec<u8>| Phone {
                ws,
                send: Aes256Gcm::new(&hkdf(&keys.frame, &salt, b"p2c").into()),
                receive: Aes256Gcm::new(&hkdf(&keys.frame, &salt, b"c2p").into()),
                sent: 0,
                received: 0,
                next_id: 1,
                events: Vec::new(),
                last_frame: Vec::new(),
                ikm,
            };
            let mut phone_session = session(ws, keys, ikm.to_vec());
            if !pairing {
                return Ok(phone_session);
            }
            // Pairing: send our own secret under the pair-secret keys, wait
            // for the acknowledgement, then switch to the combined keys.
            let phone_secret: [u8; 32] = rand_bytes();
            phone_session
                .send_frame(&json!({"id":0,"op":"pairComplete","params":{"phoneSecret":URL_SAFE_NO_PAD.encode(phone_secret)}}))
                .await;
            let ack = phone_session.next().await?;
            assert_eq!(ack, json!({"id":0,"result":{"paired":true}}));
            let mut combined = ikm.to_vec();
            combined.extend_from_slice(&phone_secret);
            let paired = self::keys(&combined);
            Ok(session(phone_session.ws, &paired, combined))
        }
        Some(Ok(Message::Close(Some(frame)))) => Err(frame.code.into()),
        other => panic!("unexpected handshake reply {other:?}"),
    }
}

fn rand_bytes<const N: usize>() -> [u8; N] {
    let mut b = [0u8; N];
    getrandom::getrandom(&mut b).unwrap();
    b
}

impl Phone {
    async fn send_frame(&mut self, value: &Value) {
        let header = self.sent.to_be_bytes();
        let mut frame = header.to_vec();
        frame.extend(
            self.send
                .encrypt(
                    Nonce::from_slice(&nonce(self.sent)),
                    Payload {
                        msg: value.to_string().as_bytes(),
                        aad: &header,
                    },
                )
                .unwrap(),
        );
        self.sent += 1;
        self.last_frame = frame.clone();
        self.ws.send(Message::Binary(frame)).await.unwrap();
    }

    /// The next decrypted frame, or the close code.
    async fn next(&mut self) -> Result<Value, u16> {
        loop {
            let message = tokio::time::timeout(Duration::from_secs(15), self.ws.next())
                .await
                .expect("gateway went silent");
            match message {
                Some(Ok(Message::Binary(frame))) => {
                    let header: [u8; 8] = frame[..8].try_into().unwrap();
                    assert_eq!(u64::from_be_bytes(header), self.received, "c2p sequence");
                    let plain = self
                        .receive
                        .decrypt(
                            Nonce::from_slice(&nonce(self.received)),
                            Payload {
                                msg: &frame[8..],
                                aad: &header,
                            },
                        )
                        .expect("c2p frame authenticates");
                    self.received += 1;
                    return Ok(serde_json::from_slice(&plain).unwrap());
                }
                Some(Ok(Message::Close(frame))) => {
                    return Err(frame.map(|f| f.code.into()).unwrap_or(1005))
                }
                Some(Ok(_)) => continue,
                _ => return Err(1006),
            }
        }
    }

    async fn request(&mut self, op: &str, params: Value) -> Value {
        let id = self.next_id;
        self.next_id += 1;
        self.send_frame(&json!({"id":id,"op":op,"params":params}))
            .await;
        loop {
            let frame = self.next().await.expect("connection open");
            if frame["id"] == id {
                return frame;
            }
            self.events.push(frame);
        }
    }

    async fn event(&mut self, wanted: impl Fn(&Value) -> bool) -> Value {
        if let Some(i) = self.events.iter().position(&wanted) {
            return self.events.remove(i);
        }
        loop {
            let frame = self.next().await.expect("connection open");
            if wanted(&frame) {
                return frame;
            }
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn phone_pairs_drives_the_panel_and_gets_an_encrypted_push() {
    let temp = tempfile::tempdir().unwrap();
    let relay = temp.path().join("relay");
    let home = temp.path().join("home");
    private_dir(&home);
    let profile_dir = relay.join("agent-panel/profiles").join(PROFILE);
    private_dir(&relay);
    private_dir(&relay.join("agent-panel"));
    private_dir(&relay.join("agent-panel/profiles"));
    private_dir(&profile_dir);

    // Fake panel service, bound to the profile through service.json.
    let state = Arc::new(Mutex::new(Panel {
        phase: "idle".into(),
        ..Default::default()
    }));
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let hash = hex::encode(Sha256::digest(std::fs::read(test_binary()).unwrap()));
    let endpoint = profile_dir.join("service.json");
    std::fs::write(
        &endpoint,
        json!({"schemaVersion":1,"profile":PROFILE,"address":listener.local_addr().unwrap(),"capability":"c".repeat(64),"binaryHash":hash,"pid":std::process::id()}).to_string(),
    )
    .unwrap();
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&endpoint, std::fs::Permissions::from_mode(0o600)).unwrap();
    }
    serve_panel(listener, hash, state.clone());
    let pushes = Arc::new(Mutex::new(Vec::new()));
    let push_listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let push_url = format!("http://{}", push_listener.local_addr().unwrap());
    let gone = Arc::new(std::sync::atomic::AtomicBool::new(false));
    serve_push(push_listener, pushes.clone(), gone.clone());

    // Pair: the QR link carries the device id and the pair secret.
    let paired = cli(
        &relay,
        &home,
        &[
            "pair",
            "--url",
            "ws://127.0.0.1:1",
            "--name",
            "Test phone",
            "--json",
        ],
    );
    assert_eq!(paired["profile"], PROFILE);
    let uri = url::Url::parse(paired["pairUri"].as_str().unwrap()).unwrap();
    let query: std::collections::HashMap<_, _> = uri.query_pairs().into_owned().collect();
    assert_eq!(query["v"], "1");
    assert_eq!(query["url"], "ws://127.0.0.1:1");
    // Before any `serve --name`, the link carries the system computer name.
    assert!(!query["n"].is_empty());
    let device = query["id"].clone();
    let secret = URL_SAFE_NO_PAD.decode(&query["k"]).unwrap();
    assert_eq!(secret.len(), 32);
    let mobile = relay.join("mobile");
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&mobile), 0o700);
        assert_eq!(mode(&mobile.join("devices.json")), 0o600);
        assert_eq!(mode(&mobile.join(format!("keys/{device}.key"))), 0o600);
    }
    assert!(!home.join(".chrome-use/mobile").exists());

    let (_gateway, address) = start_gateway(
        &relay,
        &home,
        &["--push-url", &push_url, "--name", "Studio Mac"],
    );

    // A wrong key fails and counts against the device.
    assert_eq!(
        connect(&address, &device, &[9u8; 32], true).await.err(),
        Some(4401)
    );
    // A pending device cannot skip pairing.
    assert_eq!(
        connect(&address, &device, &secret, false).await.err(),
        Some(4401)
    );

    let mut phone = connect(&address, &device, &secret, true).await.unwrap();
    let paired = phone.ikm.clone();
    assert_eq!(paired.len(), 64);
    let hello = phone.request("hello", json!({})).await;
    assert_eq!(hello["result"]["computerName"], "Studio Mac");
    assert!(hello["result"]["gatewayVersion"].is_string());
    assert_eq!(hello["result"]["pushMutes"], json!([]));
    let muted = phone
        .request(
            "pushMute",
            json!({"conversationId":CONVERSATION,"muted":true}),
        )
        .await;
    assert_eq!(muted["result"]["pushMutes"], json!([CONVERSATION]));
    let hello = phone.request("hello", json!({})).await;
    assert_eq!(hello["result"]["pushMutes"], json!([CONVERSATION]));
    let unmuted = phone
        .request(
            "pushMute",
            json!({"conversationId":CONVERSATION,"muted":false}),
        )
        .await;
    assert_eq!(unmuted["result"]["pushMutes"], json!([]));
    // Model listing: one real inspection per agent per 30 s, then cached.
    let first = phone
        .request("agentInspect", json!({"agentId":"codex"}))
        .await;
    assert_eq!(first["result"]["inspection"]["models"][0]["id"], "gpt-5");
    let second = phone
        .request("agentInspect", json!({"agentId":"codex"}))
        .await;
    assert_eq!(second["result"], first["result"]);
    let denied = phone
        .request(
            "agentInspect",
            json!({"agentId":"codex","executable":"/bin/sh"}),
        )
        .await;
    assert_eq!(denied["error"]["code"], "denied");
    {
        let panel = state.lock().unwrap();
        let inspections: Vec<_> = panel
            .received
            .iter()
            .filter(|(o, _)| o == "agentInspect")
            .collect();
        assert_eq!(inspections.len(), 1);
        assert_eq!(inspections[0].1, json!({"agentId":"codex"}));
    }
    let list = phone.request("conversationList", json!({})).await;
    assert_eq!(list["result"]["conversations"][0]["id"], CONVERSATION);

    // Denied operations and fields never reach the panel service.
    let denied = phone
        .request(
            "conversationSetAccess",
            json!({"conversationId":CONVERSATION,"fullAccess":true}),
        )
        .await;
    assert_eq!(denied["error"]["code"], "denied");
    let denied = phone
        .request(
            "conversationCreate",
            json!({"agentId":"claude","fullAccess":true}),
        )
        .await;
    assert_eq!(denied["error"]["code"], "denied");
    let denied = phone.request("serviceStop", json!({})).await;
    assert_eq!(denied["error"]["code"], "denied");

    let message = "1b4e28ba-2fa1-41d2-883f-0016d3cca427";
    let sent = phone
        .request(
            "conversationSend",
            json!({"conversationId":CONVERSATION,"messageId":message,"text":"Summarize my inbox"}),
        )
        .await;
    assert_eq!(sent["result"]["message"]["accepted"], true);

    let subscribed = phone
        .request(
            "subscribe",
            json!({"conversationId":CONVERSATION,"after":0}),
        )
        .await;
    assert_eq!(subscribed["result"]["subscribed"], CONVERSATION);
    let journal = phone
        .event(|f| f["event"] == "journal" && f["events"].as_array().is_some_and(|e| !e.is_empty()))
        .await;
    assert_eq!(journal["events"][0]["kind"], "user");
    assert_eq!(journal["cursor"], 1);

    let registered = phone
        .request("pushRegister", json!({"relayToken":"eyJwIjoiaW9zIn0.c2ln"}))
        .await;
    assert_eq!(registered["result"]["registered"], true);
    // Let the push watcher record the current state before it changes.
    tokio::time::sleep(Duration::from_millis(4500)).await;
    {
        let mut panel = state.lock().unwrap();
        panel.phase = "waitingInput".into();
        let n = panel.events.len() as u64 + 1;
        panel.events.push(event(n, "request", json!({"requestId":7,"method":"can_use_tool","generation":1,"requestRevision":3,"params":{"tool_name":"Bash"}})));
    }
    let request = phone
        .event(|f| f["event"] == "journal" && f["events"][0]["kind"] == "request")
        .await;
    assert_eq!(request["cursor"], 2);
    assert_eq!(request["conversation"]["phase"], "waitingInput");
    let deadline = Instant::now() + Duration::from_secs(10);
    let push = loop {
        if let Some(p) = pushes.lock().unwrap().first().cloned() {
            break p;
        }
        assert!(Instant::now() < deadline, "no push within 10 s");
        tokio::time::sleep(Duration::from_millis(100)).await;
    };
    assert_eq!(push["relayToken"], "eyJwIjoiaW9zIn0.c2ln");
    assert_eq!(push["priority"], "high");
    assert_eq!(push["collapseId"], CONVERSATION);
    let raw = STANDARD.decode(push["payload"].as_str().unwrap()).unwrap();
    let push_key = keys(&paired).push;
    let mut kid = <Hmac<Sha256> as Mac>::new_from_slice(&push_key).unwrap();
    kid.update(b"chrome-use mobile v1 kid");
    assert_eq!(raw[..4], kid.finalize().into_bytes()[..4], "key id");
    let plain = Aes256Gcm::new(&push_key.into())
        .decrypt(Nonce::from_slice(&raw[4..16]), &raw[16..])
        .expect("push payload decrypts with the push key");
    let plain: Value = serde_json::from_slice(&plain).unwrap();
    assert_eq!(plain["v"], 1);
    assert_eq!(plain["kind"], "needsInput");
    assert_eq!(plain["code"], "needsInput");
    assert_eq!(plain["conversationId"], CONVERSATION);
    assert_eq!(plain["title"], "Weekly report");
    // Answered only after the push: the watcher must see waitingInput first.
    let answer = phone
        .request(
            "conversationRespond",
            json!({"conversationId":CONVERSATION,"requestId":7,"generation":1,"requestRevision":3,"decision":"allow"}),
        )
        .await;
    assert_eq!(answer["result"]["response"]["dispatched"], true);

    // The Worker now refuses the token: the next push reports it to the
    // phone, and registering the same token again is refused.
    gone.store(true, std::sync::atomic::Ordering::SeqCst);
    tokio::time::sleep(Duration::from_millis(2500)).await;
    state.lock().unwrap().phase = "waitingInput".into();
    let rejected = phone.event(|f| f["event"] == "pushTokenRejected").await;
    assert_eq!(rejected["reason"], "unregistered");
    let again = phone
        .request("pushRegister", json!({"relayToken":"eyJwIjoiaW9zIn0.c2ln"}))
        .await;
    assert_eq!(
        again["result"],
        json!({"registered":false,"reason":"unregistered"})
    );
    let fresh = phone
        .request("pushRegister", json!({"relayToken":"eyJwIjoiaW9zIn0.bmV3"}))
        .await;
    assert_eq!(fresh["result"], json!({"registered":true}));
    gone.store(false, std::sync::atomic::Ordering::SeqCst);
    // A later pair link names the computer as `serve --name` saved it.
    let later = cli(
        &relay,
        &home,
        &["pair", "--url", "ws://127.0.0.1:1", "--json"],
    );
    let later = url::Url::parse(later["pairUri"].as_str().unwrap()).unwrap();
    let later: std::collections::HashMap<_, _> = later.query_pairs().into_owned().collect();
    assert_eq!(later["n"], "Studio Mac");
    cli(&relay, &home, &["revoke", &later["id"], "--json"]);

    // No browser session exists for the conversation: reported, not started.
    let shot = phone
        .request("screenshot", json!({"conversationId":CONVERSATION}))
        .await;
    assert_eq!(shot["error"]["code"], "screenshot");

    {
        let panel = state.lock().unwrap();
        let operations: Vec<&str> = panel.received.iter().map(|(o, _)| o.as_str()).collect();
        for op in ["conversationSetAccess", "conversationCreate", "serviceStop"] {
            assert!(!operations.contains(&op), "{op} reached the panel");
        }
        let send = panel
            .received
            .iter()
            .find(|(o, _)| o == "conversationSend")
            .unwrap();
        assert_eq!(
            send.1,
            json!({"conversationId":CONVERSATION,"messageId":message,"text":"Summarize my inbox"})
        );
    }

    // Scanning the same QR again fails; the paired device reconnects.
    assert_eq!(
        connect(&address, &device, &secret, true).await.err(),
        Some(4401)
    );
    // Holding only the QR link is useless once paired.
    assert_eq!(
        connect(&address, &device, &secret, false).await.err(),
        Some(4401)
    );
    let mut second = connect(&address, &device, &paired, false).await.unwrap();
    let ok = second.request("scheduleList", json!({})).await;
    assert!(ok["result"]["schedules"].is_array());
    // A replayed frame closes the connection.
    let replay = second.last_frame.clone();
    second.ws.send(Message::Binary(replay)).await.unwrap();
    assert_eq!(second.next().await.err(), Some(4400));

    // A gateway limited to another profile turns the device away with its
    // own close code, and never backs it off.
    {
        let (_other, other) = start_gateway(&relay, &home, &["--profile", "other-profile"]);
        for _ in 0..5 {
            assert_eq!(
                connect(&other, &device, &paired, false).await.err(),
                Some(4409)
            );
        }
    }

    // Revoking drops the live connection within a second or so.
    let mut second = connect(&address, &device, &paired, false).await.unwrap();
    // A desktop conversation with full access: reading is fine, acting needs
    // the computer's permission; stopping and declining never do.
    let send_elevated = json!({"conversationId":ELEVATED,"messageId":"9b2f8a7e-1c3d-4e5f-8a9b-0c1d2e3f4a5b","text":"Ship it"});
    let refused = second
        .request("conversationSend", send_elevated.clone())
        .await;
    assert_eq!(refused["error"]["code"], "accessTooHigh");
    let allow = second
        .request(
            "conversationRespond",
            json!({"conversationId":ELEVATED,"requestId":1,"generation":1,"requestRevision":1,"decision":"accept"}),
        )
        .await;
    assert_eq!(allow["error"]["code"], "accessTooHigh");
    let decline = second
        .request(
            "conversationRespond",
            json!({"conversationId":ELEVATED,"requestId":1,"generation":1,"requestRevision":1,"decision":"decline"}),
        )
        .await;
    assert!(decline.get("result").is_some(), "{decline}");
    let stop = second
        .request("conversationStop", json!({"conversationId":ELEVATED}))
        .await;
    assert!(stop.get("result").is_some(), "{stop}");
    let read = second
        .request("conversationRead", json!({"conversationId":ELEVATED}))
        .await;
    assert!(read.get("result").is_some(), "{read}");
    let enable = second
        .request("scheduleSave", json!({"id":NIGHTLY,"enabled":true}))
        .await;
    assert_eq!(enable["error"]["code"], "accessTooHigh");
    let disable = second
        .request("scheduleSave", json!({"id":NIGHTLY,"enabled":false}))
        .await;
    assert_eq!(disable["result"]["schedule"]["enabled"], false);
    let hello = second.request("hello", json!({})).await;
    assert_eq!(hello["result"]["fullAccessAllowed"], false);
    let granted = cli(&relay, &home, &["access", &device, "full", "--json"]);
    assert_eq!(granted["fullAccessAllowed"], true);
    let hello = second.request("hello", json!({})).await;
    assert_eq!(hello["result"]["fullAccessAllowed"], true);
    let sent = second.request("conversationSend", send_elevated).await;
    assert_eq!(sent["result"]["message"]["accepted"], true);
    let listed = cli(&relay, &home, &["devices", "--json"]);
    assert_eq!(listed["devices"][0]["fullAccessAllowed"], true);
    // Changing effort alone keeps the conversation's current tool access.
    let options = second
        .request(
            "conversationSetOptions",
            json!({"conversationId":ELEVATED,"options":{"effort":"high"}}),
        )
        .await;
    assert!(options.get("result").is_some(), "{options}");
    {
        let panel = state.lock().unwrap();
        let (_, params) = panel
            .received
            .iter()
            .rfind(|(o, _)| o == "conversationSetOptions")
            .unwrap();
        assert_eq!(
            params,
            &json!({"conversationId":ELEVATED,"options":{"effort":"high","toolAccess":"writeFiles"}})
        );
    }

    // Browsers (any Origin header) are refused before the WebSocket opens.
    let mut request =
        tokio_tungstenite::tungstenite::client::IntoClientRequest::into_client_request(format!(
            "ws://{address}/"
        ))
        .unwrap();
    request
        .headers_mut()
        .insert("Origin", "https://evil.example".parse().unwrap());
    match tokio_tungstenite::connect_async(request).await {
        Err(tokio_tungstenite::tungstenite::Error::Http(response)) => {
            assert_eq!(response.status(), 403)
        }
        other => panic!("browser origin accepted: {:?}", other.map(|_| ())),
    }

    let started = Instant::now();
    let revoked = cli(&relay, &home, &["revoke", &device, "--json"]);
    assert_eq!(revoked["revoked"], device.as_str());
    let code = loop {
        match phone.next().await {
            Err(code) => break code,
            Ok(_) => continue,
        }
    };
    assert_eq!(code, 4403);
    assert!(started.elapsed() < Duration::from_secs(3));
    assert_eq!(
        connect(&address, &device, &paired, false).await.err(),
        Some(4401)
    );
    let listed = cli(&relay, &home, &["devices", "--json"]);
    assert_eq!(listed["devices"], json!([]));

    // Unauthenticated connections: at most 4 per source, each closed after 5 s.
    let idle: Vec<_> = (0..4)
        .map(|_| std::net::TcpStream::connect(&address).unwrap())
        .collect();
    std::thread::sleep(Duration::from_millis(200));
    let mut fifth = std::net::TcpStream::connect(&address).unwrap();
    fifth
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    let mut byte = [0u8; 1];
    assert_eq!(fifth.read(&mut byte).unwrap_or(0), 0, "fifth was kept open");
    let mut first = idle.into_iter().next().unwrap();
    first
        .set_read_timeout(Some(Duration::from_secs(8)))
        .unwrap();
    let opened = Instant::now();
    assert_eq!(first.read(&mut byte).unwrap_or(0), 0);
    assert!(
        opened.elapsed() < Duration::from_secs(7),
        "pre-auth timeout"
    );
}

/// A missing interface must not make a service manager repeatedly respawn us,
/// or produce the machine-readable readiness line before a socket exists.
#[test]
fn missing_bind_address_waits_in_one_process_without_ready_output() {
    let temp = tempfile::tempdir().unwrap();
    let stdout = temp.path().join("stdout");
    let stderr = temp.path().join("stderr");
    let mut gateway = Gateway(
        Command::new(test_binary())
            .args(["mobile", "serve", "--bind", "192.0.2.123:17345"])
            .env("CHROME_USE_RELAY_DIR", temp.path().join("relay"))
            .env("HOME", temp.path())
            .env("CHROME_USE_NO_UPDATE_CHECK", "1")
            .stdout(std::fs::File::create(&stdout).unwrap())
            .stderr(std::fs::File::create(&stderr).unwrap())
            .spawn()
            .unwrap(),
    );
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        assert!(gateway.0.try_wait().unwrap().is_none(), "gateway exited");
        let errors = std::fs::read_to_string(&stderr).unwrap();
        if errors.contains("retrying in 2s") {
            assert!(errors.contains("192.0.2.123:17345"));
            assert!(errors.contains("No listener is ready"));
            break;
        }
        assert!(Instant::now() < deadline, "no retry diagnostic: {errors}");
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(std::fs::read(&stdout).unwrap().is_empty());
}
