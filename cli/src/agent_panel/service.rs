//! A profile-owned agent service, independent of Chrome's native-message port.
//! The loopback protocol is private native IPC, not a page or browser endpoint.
//! Each endpoint is authenticated by a capability kept in a mode-0600 file.

use super::backend::{self, Command as BackendCommand, Notice, Setup};
use super::config_transaction::{atomic_write, private_open, private_tree};
use super::conversation::{Conversation, Message, PageContext, Phase, Store};
use super::DiscoveryContext;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::fs::File;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock};
use std::time::{Duration, Instant};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::{mpsc, Mutex};

const MAX_FRAME: usize = 768 * 1024;
static BINARY_HASH: LazyLock<Result<String, String>> = LazyLock::new(|| {
    let path =
        std::env::current_exe().map_err(|_| "Cannot identify the agent-service executable")?;
    let mut file = File::open(path).map_err(|_| "Cannot inspect the agent-service executable")?;
    let mut hash = Sha256::new();
    let mut bytes = [0u8; 65536];
    loop {
        let n = file
            .read(&mut bytes)
            .map_err(|_| "Cannot fingerprint the agent-service executable")?;
        if n == 0 {
            break;
        }
        hash.update(&bytes[..n]);
    }
    Ok(hex::encode(hash.finalize()))
});

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Endpoint {
    schema_version: u32,
    profile: String,
    address: SocketAddr,
    capability: String,
    binary_hash: String,
    pid: u32,
}
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Request {
    capability: String,
    request_id: String,
    operation: String,
    params: Value,
}

fn profile_directory(root: &Path, profile: &str) -> Result<PathBuf, String> {
    if !root.is_absolute()
        || profile.is_empty()
        || profile.len() > 80
        || !profile
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_')
    {
        return Err(
            "Agent service requires an absolute data directory and a bound profile identity".into(),
        );
    }
    for directory in [
        root.to_path_buf(),
        root.join("profiles"),
        root.join("profiles").join(profile),
    ] {
        private_tree(&directory)?;
    }
    Ok(root.join("profiles").join(profile))
}

pub(super) fn data_root(context: &DiscoveryContext) -> Result<PathBuf, String> {
    if let Some(directory) = context
        .environment
        .get("CHROME_USE_RELAY_DIR")
        .filter(|s| !s.is_empty())
    {
        let directory = PathBuf::from(directory);
        if !directory.is_absolute() {
            return Err("CHROME_USE_RELAY_DIR must be absolute".into());
        }
        Ok(directory.join("agent-panel"))
    } else {
        Ok(context.home.join(".chrome-use/agent-panel"))
    }
}

fn endpoint(directory: &Path, profile: &str) -> Result<Endpoint, String> {
    let path = directory.join("service.json");
    let meta =
        std::fs::symlink_metadata(&path).map_err(|_| "Agent service endpoint is unavailable")?;
    if !meta.is_file() || meta.file_type().is_symlink() || meta.len() > 8192 {
        return Err("Invalid agent service endpoint file".into());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        if meta.uid() != unsafe { libc::geteuid() } || meta.permissions().mode() & 0o077 != 0 {
            return Err("Agent service endpoint is not private".into());
        }
    }
    let endpoint: Endpoint = serde_json::from_slice(
        &std::fs::read(path).map_err(|_| "Cannot read agent service endpoint")?,
    )
    .map_err(|_| "Invalid agent service endpoint")?;
    if endpoint.schema_version != 1
        || endpoint.profile != profile
        || !endpoint.address.ip().is_loopback()
        || endpoint.capability.len() != 64
    {
        return Err("Agent service endpoint identity does not match this profile".into());
    }
    Ok(endpoint)
}

/// How long one service operation may take. A CLI model check can need a cold
/// start (OpenCode starts a private server); a delete waits for the agent to
/// exit and then closes the conversation's browser session.
fn operation_seconds(operation: &str) -> u64 {
    match operation {
        "agentInspect" => 30,
        "conversationDelete" => 45,
        _ => 10,
    }
}

fn exchange(endpoint: &Endpoint, operation: &str, params: Value) -> Result<Value, String> {
    let mut stream = TcpStream::connect_timeout(&endpoint.address, Duration::from_millis(750))
        .map_err(|_| "Agent service did not answer")?;
    stream
        .set_read_timeout(Some(Duration::from_secs(operation_seconds(operation) + 2)))
        .map_err(|_| "Cannot set agent service timeout")?;
    stream
        .set_write_timeout(Some(Duration::from_secs(3)))
        .map_err(|_| "Cannot set agent service timeout")?;
    let request = Request {
        capability: endpoint.capability.clone(),
        request_id: uuid::Uuid::new_v4().to_string(),
        operation: operation.into(),
        params,
    };
    let raw = serde_json::to_vec(&request).map_err(|_| "Cannot encode agent service request")?;
    if raw.len() > MAX_FRAME {
        return Err("Agent service request is too large".into());
    }
    stream
        .write_all(&(raw.len() as u32).to_le_bytes())
        .and_then(|_| stream.write_all(&raw))
        .map_err(|_| "Agent service request could not be written; outcome is unconfirmed")?;
    let mut length = [0u8; 4];
    stream
        .read_exact(&mut length)
        .map_err(|_| "Agent service reply is unconfirmed; no request was replayed")?;
    let length = u32::from_le_bytes(length) as usize;
    if length > MAX_FRAME {
        return Err("Agent service reply exceeds its size limit".into());
    }
    let mut raw = vec![0u8; length];
    stream
        .read_exact(&mut raw)
        .map_err(|_| "Agent service reply is incomplete; no request was replayed")?;
    let reply: Value = serde_json::from_slice(&raw).map_err(|_| "Invalid agent service reply")?;
    if reply["requestId"] != request.request_id {
        return Err("Agent service replied to a different request".into());
    }
    if let Some(error) = reply["error"].as_str() {
        return Err(error.into());
    }
    reply
        .get("result")
        .cloned()
        .ok_or_else(|| "Agent service reply has no result".into())
}

/// Reuse only a health-confirmed service of the exact binary. A different
/// build's active conversations are never killed or silently rebound. Idle
/// builds are replaced only after endpoint removal and OS lock release.
fn ensure(directory: &Path, root: &Path, profile: &str) -> Result<Endpoint, String> {
    let binary_hash = BINARY_HASH.as_ref().map_err(Clone::clone)?;
    if let Ok(existing) = endpoint(directory, profile) {
        if let Ok(health) = exchange(&existing, "health", json!({})) {
            if health["binaryHash"] == *binary_hash {
                return Ok(existing);
            }
        }
    }
    let bootstrap = private_open(&directory.join("bootstrap.lock"), true)?;
    let deadline = Instant::now() + Duration::from_secs(8);
    while bootstrap.try_lock().is_err() {
        if Instant::now() > deadline {
            return Err("Another agent-service startup is still in progress".into());
        }
        std::thread::sleep(Duration::from_millis(30));
    }
    if let Ok(existing) = endpoint(directory, profile) {
        if let Ok(health) = exchange(&existing, "health", json!({})) {
            if health["binaryHash"] == *binary_hash {
                return Ok(existing);
            }
            if health["active"].as_u64() != Some(0) {
                return Err("Another agent-panel build is running for this profile; stop its conversations before updating".into());
            }
            // The old service rechecks busy state under its own lock. Wait for
            // both endpoint removal and OS ownership release, never a PID.
            exchange(&existing, "serviceStop", json!({}))?;
            loop {
                let ownership = private_open(&directory.join("service.lock"), true)?;
                if !directory.join("service.json").exists() && ownership.try_lock().is_ok() {
                    break;
                }
                if Instant::now() > deadline {
                    return Err("Previous agent service shutdown is unconfirmed; no replacement was started".into());
                }
                std::thread::sleep(Duration::from_millis(30));
            }
        }
    }
    // Probe the real OS ownership lock, without trusting a stale PID file.
    let ownership = private_open(&directory.join("service.lock"), true)?;
    ownership.try_lock().map_err(|_| {
        "The profile's agent service owns its lock but does not respond; it was not restarted"
    })?;
    drop(ownership);
    let executable =
        std::env::current_exe().map_err(|_| "Cannot locate agent-service executable")?;
    let mut command = std::process::Command::new(executable);
    command
        .args(["__panel-service", "--profile", profile, "--data-dir"])
        .arg(root)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        unsafe {
            command.pre_exec(|| {
                if libc::setsid() < 0 {
                    Err(std::io::Error::last_os_error())
                } else {
                    Ok(())
                }
            });
        }
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000 | 0x00000200);
    }
    let mut child = command
        .spawn()
        .map_err(|_| "Agent service could not be started")?;
    while Instant::now() < deadline {
        if let Some(status) = child
            .try_wait()
            .map_err(|_| "Cannot inspect agent service startup")?
        {
            return Err(format!("Agent service exited during startup ({status})"));
        }
        if let Ok(endpoint) = endpoint(directory, profile) {
            if endpoint.binary_hash == *binary_hash
                && exchange(&endpoint, "health", json!({})).is_ok()
            {
                return Ok(endpoint);
            }
        }
        std::thread::sleep(Duration::from_millis(40));
    }
    Err("Agent-service startup has not been confirmed; its process was not restarted".into())
}

pub(super) fn call(
    operation: &str,
    params: &Value,
    profile: Option<&str>,
) -> Result<Value, String> {
    let profile = profile.ok_or("Conversation operations require a bound Chrome profile")?;
    let context = DiscoveryContext::current()?;
    let root = data_root(&context)?;
    let directory = profile_directory(&root, profile)?;
    // Explicit stop and read operations may reach an older schema-1 service.
    // This lets the user stop its work without dispatching a new turn.
    if [
        "serviceStop",
        "conversationList",
        "conversationRead",
        "conversationMessageStatus",
        "conversationStop",
    ]
    .contains(&operation)
    {
        if let Ok(existing) = endpoint(&directory, profile) {
            if exchange(&existing, "health", json!({})).is_ok() {
                return exchange(&existing, operation, params.clone());
            }
        }
    }
    let endpoint = ensure(&directory, &root, profile)?;
    exchange(
        &endpoint,
        if operation == "serviceHealth" {
            "health"
        } else {
            operation
        },
        params.clone(),
    )
}

struct Job {
    generation: u64,
    full_access: bool,
    sender: mpsc::Sender<BackendCommand>,
}
struct Engine {
    store: Store,
    chats: HashMap<String, Conversation>,
    jobs: HashMap<String, Job>,
    requests: HashMap<(String, String), (Value, String, Value, bool)>,
    profile: String,
    stop: bool,
}

impl Engine {
    /// Answer every waiting approval for a full-access conversation exactly as
    /// the panel's "Allow once" would. Questions still go to the user.
    fn approve_pending(&mut self, id: &str) {
        let Some(sender) = self.jobs.get(id).map(|j| j.sender.clone()) else {
            return;
        };
        for ((chat, _), (request_id, method, original, sent)) in self.requests.iter_mut() {
            if chat != id || *sent {
                continue;
            }
            let Some(result) = automatic_approval(method, original) else {
                continue;
            };
            if validate_reply(method, original, &result).is_err() {
                continue;
            }
            if sender
                .try_send(BackendCommand::Reply {
                    id: request_id.clone(),
                    result,
                })
                .is_ok()
            {
                *sent = true;
            }
        }
    }
    fn chat(&mut self, id: &str) -> Result<&mut Conversation, String> {
        if !self.chats.contains_key(id) {
            if self.chats.len() >= 32 {
                let victim = self
                    .chats
                    .iter()
                    .filter(|(key, c)| !self.jobs.contains_key(*key) && !c.phase.busy())
                    .min_by_key(|(_, c)| c.updated)
                    .map(|(key, _)| key.clone());
                if let Some(victim) = victim {
                    self.chats.remove(&victim);
                }
            }
            let chat = self.store.load(id)?;
            self.chats.insert(id.into(), chat);
        }
        self.chats
            .get_mut(id)
            .ok_or_else(|| "Conversation was not found".into())
    }
    fn persist(&mut self, id: &str) -> Result<(), String> {
        let chat = self.chats.get(id).ok_or("Conversation was not found")?;
        self.store.save(chat)
    }
    fn record(&mut self, id: &str, kind: &str, data: Value) -> Result<(), String> {
        let chat = self.chats.get_mut(id).ok_or("Conversation was not found")?;
        self.store.event(chat, kind, data)?;
        if kind != "textDelta" {
            self.store.save(chat)?;
        }
        Ok(())
    }
    fn notice(
        &mut self,
        id: &str,
        generation: u64,
        mut notice: Notice,
    ) -> Result<Option<Message>, String> {
        if self.chat(id)?.generation != generation {
            return Ok(None);
        }
        if self
            .jobs
            .get(id)
            .is_some_and(|job| job.generation != generation)
        {
            return Ok(None);
        }
        if let Some(message) = self.chat(id)?.active.as_ref() {
            notice.data["messageId"] = message.id.clone().into();
        }
        // Claude confirms a permission round trip by returning the matching
        // tool_use_id. A stdin write alone never resolves the request.
        if notice.kind == "tool" && notice.data["completed"] == true {
            if let Some(tool_id) = notice.data["item"]["tool_use_id"].as_str() {
                let resolved: Vec<_> = self
                    .requests
                    .iter()
                    .filter(|((chat, _), (_, method, params, sent))| {
                        chat == id
                            && *sent
                            && method == "can_use_tool"
                            && params["tool_use_id"] == tool_id
                    })
                    .map(|(key, (request_id, _, _, _))| (key.clone(), request_id.clone()))
                    .collect();
                for (key, request_id) in resolved {
                    self.requests.remove(&key);
                    self.record(id, "requestResolved", json!({"requestId":request_id}))?;
                }
            }
        }
        match notice.kind {
            "ready" => {
                let chat = self.chat(id)?;
                chat.session_id = notice.data["sessionId"].as_str().map(String::from);
                chat.model = notice.data["model"].as_str().map(String::from);
            }
            "running" | "textDelta" | "tool" => {
                let waiting = self.requests.keys().any(|(chat, _)| chat == id);
                let chat = self.chat(id)?;
                if chat.active.is_some() && chat.phase != Phase::Stopping {
                    chat.phase = if waiting {
                        Phase::WaitingInput
                    } else {
                        Phase::Running
                    };
                }
            }
            "request" => {
                let request_id = notice.data["requestId"].clone();
                let key =
                    serde_json::to_string(&request_id).map_err(|_| "Invalid backend request id")?;
                if key.len() > 1024 || self.requests.len() >= 64 {
                    return Err("Backend request capacity reached".into());
                }
                self.requests.insert(
                    (id.into(), key),
                    (
                        request_id,
                        notice.data["method"].as_str().unwrap_or("").into(),
                        notice.data["params"].clone(),
                        false,
                    ),
                );
                if self.chat(id)?.phase != Phase::Stopping {
                    self.chat(id)?.phase = Phase::WaitingInput;
                }
                if self.chat(id)?.full_access {
                    self.approve_pending(id);
                }
            }
            "requestResolved" => {
                let key = serde_json::to_string(&notice.data["requestId"])
                    .map_err(|_| "Invalid backend request id")?;
                self.requests.remove(&(id.into(), key));
            }
            "title" => {
                if let Some(title) = notice.data["title"].as_str() {
                    self.chat(id)?.title = title.chars().take(80).collect();
                }
            }
            "terminal" => {
                let status = notice.data["status"]
                    .as_str()
                    .ok_or("Missing backend terminal status")?;
                self.chat(id)?.terminal(
                    generation,
                    status,
                    notice.data["error"].as_str().map(String::from),
                )?;
                self.requests.retain(|(chat, _), _| chat != id);
            }
            "exited" => {
                self.jobs.remove(id);
                self.requests.retain(|(chat, _), _| chat != id);
                let chat = self.chat(id)?;
                if chat.active.is_some() {
                    let status = if chat.phase == Phase::Stopping {
                        "interrupted"
                    } else {
                        "failed"
                    };
                    chat.terminal(
                        generation,
                        status,
                        Some(
                            notice.data["error"]
                                .as_str()
                                .unwrap_or("Backend process ended before the turn completed")
                                .into(),
                        ),
                    )?;
                }
            }
            "processUnconfirmed" => {
                self.jobs.remove(id);
                self.requests.retain(|(chat, _), _| chat != id);
                let chat = self.chat(id)?;
                chat.phase = Phase::RecoveryRequired;
                chat.queue_paused = true;
                chat.last_error = Some(
                    notice.data["error"]
                        .as_str()
                        .unwrap_or("Backend process state is unconfirmed")
                        .into(),
                );
            }
            _ => {}
        }
        self.record(id, notice.kind, notice.data)?;
        if ["terminal", "exited"].contains(&notice.kind) {
            let next = self.chat(id)?.begin_next();
            if next.is_some() {
                self.persist(id)?;
            }
            Ok(next)
        } else {
            Ok(None)
        }
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Create {
    agent_id: String,
    model: Option<String>,
    executable: Option<PathBuf>,
    #[serde(default)]
    full_access: bool,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SetAccess {
    conversation_id: String,
    full_access: bool,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Identify {
    conversation_id: String,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Send {
    conversation_id: String,
    message_id: String,
    text: String,
    #[serde(default)]
    page: Option<PageContext>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct MessageStatus {
    conversation_id: String,
    message_id: String,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct QueueRemove {
    conversation_id: String,
    message_id: String,
    expected_text: String,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct QueueIdentify {
    conversation_id: String,
    message_id: String,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct QueueEdit {
    conversation_id: String,
    message_id: String,
    expected_text: String,
    replacement_id: String,
    text: String,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Events {
    conversation_id: String,
    #[serde(default)]
    after: u64,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Reply {
    conversation_id: String,
    request_id: Value,
    result: Value,
}

async fn dispatch(
    engine: &Arc<Mutex<Engine>>,
    operation: &str,
    params: &Value,
) -> Result<Value, String> {
    if engine.lock().await.stop && operation != "health" {
        return Err("Agent service is stopping; reconnect after its exit is confirmed".into());
    }
    match operation {
        "agentInspect" => {
            let root = engine.lock().await.store.directory.clone();
            super::inspection::inspect(params, &root).await
        }
        "health" => {
            if params != &json!({}) {
                return Err("health takes no parameters".into());
            }
            let e = engine.lock().await;
            Ok(
                json!({"schemaVersion":1,"profile":e.profile,"binaryHash":BINARY_HASH.as_ref().map_err(Clone::clone)?,"pid":std::process::id(),"active":e.chats.values().filter(|c|c.phase.busy()).count()}),
            )
        }
        "serviceStop" => {
            if params != &json!({}) {
                return Err("serviceStop takes no parameters".into());
            }
            let mut e = engine.lock().await;
            if e.chats.values().any(|c| c.phase.busy()) {
                return Err("Stop active conversations before stopping the service".into());
            }
            let senders: Vec<_> = e.jobs.values().map(|j| j.sender.clone()).collect();
            for sender in senders {
                sender
                    .try_send(BackendCommand::Shutdown)
                    .map_err(|_| "Backend shutdown could not be dispatched")?;
            }
            e.stop = true;
            Ok(json!({"schemaVersion":1,"service":{"stopRequested":true}}))
        }
        "conversationList" => {
            if params != &json!({}) {
                return Err("conversationList takes no parameters".into());
            }
            let e = engine.lock().await;
            Ok(json!({"schemaVersion":1,"conversations":e.store.list()}))
        }
        "conversationCreate" => {
            let p: Create = serde_json::from_value(params.clone()).map_err(|_| {
                "conversationCreate requires an agent id and optional model/executable"
            })?;
            if p.executable.as_ref().is_some_and(|p| !p.is_absolute()) {
                return Err("A custom agent executable must be an absolute path".into());
            }
            let mut chat = Conversation::new(&p.agent_id, p.model, p.executable)?;
            chat.full_access = p.full_access;
            let mut e = engine.lock().await;
            e.store.create(&chat)?;
            let result = chat.summary();
            e.chats.insert(chat.id.clone(), chat);
            Ok(json!({"schemaVersion":1,"conversation":result}))
        }
        "conversationRead" => {
            let p: Events = serde_json::from_value(params.clone()).map_err(|_| {
                "conversationRead requires conversationId and optional after cursor"
            })?;
            let mut e = engine.lock().await;
            let summary = e.chat(&p.conversation_id)?.summary();
            let events = e.store.events(&p.conversation_id, p.after)?;
            let queued = e.chat(&p.conversation_id)?.queue_snapshot();
            Ok(
                json!({"schemaVersion":1,"conversation":summary,"journal":events,"queuedMessages":queued}),
            )
        }
        "conversationQueueMessageRead" => {
            let p: QueueIdentify = serde_json::from_value(params.clone())
                .map_err(|_| "Queue message read requires conversationId and messageId")?;
            let mut e = engine.lock().await;
            let chat = e.chat(&p.conversation_id)?;
            let message = chat
                .queue
                .iter()
                .find(|m| m.id == p.message_id)
                .ok_or("Message is no longer queued; read its current state")?;
            Ok(json!({"schemaVersion":1,"conversation":chat.summary(),"queuedMessage":message}))
        }
        "conversationQueueEdit" => {
            let p: QueueEdit = serde_json::from_value(params.clone()).map_err(|_| "Queue edit requires conversationId, messageId, expectedText, replacementId and text")?;
            let mut e = engine.lock().await;
            e.chat(&p.conversation_id)?.edit_queued(
                &p.message_id,
                &p.expected_text,
                Message {
                    id: p.replacement_id.clone(),
                    text: p.text.clone(),
                    page: None,
                },
            )?;
            e.record(
                &p.conversation_id,
                "queueEdited",
                json!({"messageId":p.message_id,"replacementId":p.replacement_id,"text":p.text}),
            )?;
            Ok(
                json!({"schemaVersion":1,"conversation":e.chat(&p.conversation_id)?.summary(),"queueChange":{"messageId":p.message_id,"replacementId":p.replacement_id,"kind":"edited"}}),
            )
        }
        "conversationQueueRemove" => {
            let p: QueueRemove = serde_json::from_value(params.clone())
                .map_err(|_| "Queue removal requires conversationId, messageId and expectedText")?;
            let mut e = engine.lock().await;
            e.chat(&p.conversation_id)?
                .remove_queued(&p.message_id, &p.expected_text)?;
            e.record(
                &p.conversation_id,
                "queueRemoved",
                json!({"messageId":p.message_id}),
            )?;
            Ok(
                json!({"schemaVersion":1,"conversation":e.chat(&p.conversation_id)?.summary(),"queueChange":{"messageId":p.message_id,"kind":"removed"}}),
            )
        }
        "conversationMessageStatus" => {
            let p: MessageStatus = serde_json::from_value(params.clone())
                .map_err(|_| "Message status requires conversationId and messageId")?;
            let mut e = engine.lock().await;
            let chat = e.chat(&p.conversation_id)?;
            let known = chat.accepted.contains_key(&p.message_id);
            let status = if chat.active.as_ref().is_some_and(|m| m.id == p.message_id) {
                "active"
            } else if chat.queue.iter().any(|m| m.id == p.message_id) {
                "queued"
            } else if known {
                "previouslyAccepted"
            } else {
                "unconfirmed"
            };
            Ok(
                json!({"schemaVersion":1,"conversation":chat.summary(),"message":{"id":p.message_id,"known":known,"status":status}}),
            )
        }
        "conversationSend" | "conversationSendDirect" => {
            let p: Send = serde_json::from_value(params.clone())
                .map_err(|_| "conversationSend requires conversationId, messageId and text")?;
            let mut e = engine.lock().await;
            let direct = operation == "conversationSendDirect";
            let interrupt = if direct && e.chat(&p.conversation_id)?.phase.busy() {
                Some(
                    e.jobs
                        .get(&p.conversation_id)
                        .ok_or("Backend is unavailable; stop cannot be confirmed")?
                        .sender
                        .clone(),
                )
            } else {
                None
            };
            if let Some(page) = &p.page {
                page.validate()?;
                let chat = e.chat(&p.conversation_id)?;
                if chat.site.is_none() {
                    chat.site = page_site(&page.url);
                }
            }
            let message = Message {
                id: p.message_id.clone(),
                text: p.text.clone(),
                page: p.page.clone(),
            };
            let added = if direct {
                e.chat(&p.conversation_id)?.accept_direct(message)?
            } else {
                e.chat(&p.conversation_id)?.accept(message)?
            };
            e.persist(&p.conversation_id)?;
            if added {
                e.record(
                    &p.conversation_id,
                    "user",
                    json!({"messageId":p.message_id,"text":p.text,"direct":direct,"page":p.page}),
                )?;
            }
            if added {
                if let Some(sender) = interrupt {
                    e.chat(&p.conversation_id)?.phase = Phase::Stopping;
                    e.persist(&p.conversation_id)?;
                    if sender.try_send(BackendCommand::Interrupt).is_err() {
                        let chat = e.chat(&p.conversation_id)?;
                        chat.phase = Phase::RecoveryRequired;
                        chat.last_error =
                            Some("Direct message saved but stop dispatch is unconfirmed".into());
                        e.persist(&p.conversation_id)?;
                        return Err("Direct message saved but stop dispatch is unconfirmed; read its state without resending".into());
                    }
                }
            }
            let next = e.chat(&p.conversation_id)?.begin_next();
            e.persist(&p.conversation_id)?;
            drop(e);
            if let Some(message) = next {
                if let Err(error) = start(engine, &p.conversation_id, message).await {
                    let mut e = engine.lock().await;
                    let chat = e.chat(&p.conversation_id)?;
                    chat.phase = if direct {
                        Phase::RecoveryRequired
                    } else {
                        Phase::Failed
                    };
                    chat.queue_paused = true;
                    if !direct {
                        chat.active = None;
                    }
                    chat.last_error = Some(error);
                    e.persist(&p.conversation_id)?;
                }
            }
            let summary = engine.lock().await.chat(&p.conversation_id)?.summary();
            Ok(
                json!({"schemaVersion":1,"conversation":summary,"message":{"id":p.message_id,"accepted":added,"replayed":false}}),
            )
        }
        "conversationSetAccess" => {
            let p: SetAccess = serde_json::from_value(params.clone())
                .map_err(|_| "conversationSetAccess requires conversationId and fullAccess")?;
            let mut e = engine.lock().await;
            e.chat(&p.conversation_id)?.full_access = p.full_access;
            e.persist(&p.conversation_id)?;
            // Requests already waiting are answered now; the agent process is
            // restarted with matching flags before its next turn.
            if p.full_access {
                e.approve_pending(&p.conversation_id);
            }
            Ok(json!({"schemaVersion":1,"conversation":e.chat(&p.conversation_id)?.summary()}))
        }
        "conversationDelete" => {
            let p: Identify = serde_json::from_value(params.clone())
                .map_err(|_| "conversationDelete requires only conversationId")?;
            let id = p.conversation_id;
            let mut e = engine.lock().await;
            if e.chat(&id)?.phase.busy() {
                return Err("Stop the response before deleting this conversation".into());
            }
            // Files are removed only after the agent process is confirmed gone.
            if let Some(job) = e.jobs.get(&id) {
                job.sender
                    .try_send(BackendCommand::Shutdown)
                    .map_err(|_| "Agent shutdown was not dispatched; nothing was deleted")?;
                drop(e);
                let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
                loop {
                    let locked = engine.lock().await;
                    if !locked.jobs.contains_key(&id) {
                        e = locked;
                        break;
                    }
                    drop(locked);
                    if tokio::time::Instant::now() > deadline {
                        return Err("The agent process has not exited yet; nothing was deleted. Try again shortly.".into());
                    }
                    tokio::time::sleep(Duration::from_millis(30)).await;
                }
            }
            // Real deletion also removes the CLI's own session history and the
            // conversation's browser session. Both run without the engine lock.
            let (agent_id, session_id, executable, workspace) = {
                let chat = e.chat(&id)?;
                (
                    chat.agent_id.clone(),
                    chat.session_id.clone(),
                    chat.executable.clone(),
                    e.store.directory.join(&id).join("workspace"),
                )
            };
            let profile = e.profile.clone();
            drop(e);
            let mut removed = Vec::new();
            let mut failed = Vec::new();
            let mut history_error = None;
            if let Some(session_id) = &session_id {
                if agent_id == "opencode" {
                    let executable = executable
                        .or_else(|| DiscoveryContext::current().ok()?.executable("opencode"));
                    match executable {
                        Some(executable) => {
                            match super::acp::delete_session(&executable, &workspace, session_id)
                                .await
                            {
                                Ok(()) => removed
                                    .push(PathBuf::from(format!("opencode session {session_id}"))),
                                Err(error) => history_error = Some(error),
                            }
                        }
                        None => {
                            history_error = Some(
                                "OpenCode executable unavailable; CLI history deletion unconfirmed"
                                    .into(),
                            )
                        }
                    }
                }
                match super::cli_session_files(&agent_id, session_id) {
                    Ok(paths) => {
                        for path in paths {
                            match super::remove_session_path(&path) {
                                Ok(()) => removed.push(path),
                                Err(_) => failed.push(path),
                            }
                        }
                    }
                    Err(error) => history_error = Some(error),
                }
            }
            let browser = close_browser_session(&profile, &format!("panel-{id}")).await;
            let mut e = engine.lock().await;
            if e.jobs.contains_key(&id) {
                return Err(
                    "The conversation started again while it was being deleted; delete it again"
                        .into(),
                );
            }
            e.chats.remove(&id);
            e.requests.retain(|(chat, _), _| chat != &id);
            e.store.delete(&id)?;
            Ok(
                json!({"schemaVersion":1,"deleted":{"id":id,"cliHistory":{"agentId":agent_id,"sessionId":session_id,"removed":removed,"failed":failed,"error":history_error},"browserSession":browser}}),
            )
        }
        "conversationStop" | "conversationResumeQueue" => {
            let p: Identify = serde_json::from_value(params.clone())
                .map_err(|_| "Conversation control requires only conversationId")?;
            let mut e = engine.lock().await;
            if operation == "conversationResumeQueue" {
                let chat = e.chat(&p.conversation_id)?;
                if chat.phase == Phase::RecoveryRequired {
                    return Err(
                        "The previous outcome is unconfirmed; the queue cannot resume".into(),
                    );
                }
                chat.queue_paused = false;
                if !chat.phase.busy() && chat.direct_message_id.is_some() {
                    chat.direct_ready = true;
                }
                let next = e.chat(&p.conversation_id)?.begin_next();
                e.persist(&p.conversation_id)?;
                let summary = e.chat(&p.conversation_id)?.summary();
                drop(e);
                if let Some(message) = next {
                    start(engine, &p.conversation_id, message).await?;
                }
                return Ok(json!({"schemaVersion":1,"conversation":summary}));
            }
            if e.chat(&p.conversation_id)?.phase.busy() {
                let sender = e
                    .jobs
                    .get(&p.conversation_id)
                    .ok_or("Backend process is unavailable; interruption is unconfirmed")?
                    .sender
                    .clone();
                e.chat(&p.conversation_id)?.phase = Phase::Stopping;
                e.persist(&p.conversation_id)?;
                sender.try_send(BackendCommand::Interrupt).map_err(|_| {
                    "Backend could not receive interruption; outcome is unconfirmed"
                })?;
            }
            Ok(json!({"schemaVersion":1,"conversation":e.chat(&p.conversation_id)?.summary()}))
        }
        "conversationRespond" => {
            let p: Reply = serde_json::from_value(params.clone()).map_err(|_| {
                "Conversation response requires conversationId, requestId and result"
            })?;
            let mut e = engine.lock().await;
            let key = serde_json::to_string(&p.request_id).map_err(|_| "Invalid request id")?;
            let (id, method, original, sent) = e
                .requests
                .get(&(p.conversation_id.clone(), key.clone()))
                .ok_or("The backend request is stale or belongs to another conversation")?
                .clone();
            if sent {
                return Err(
                    "A response was already dispatched; wait for backend confirmation".into(),
                );
            }
            validate_reply(&method, &original, &p.result)?;
            let sender = e
                .jobs
                .get(&p.conversation_id)
                .ok_or("Backend is not available")?
                .sender
                .clone();
            sender
                .try_send(BackendCommand::Reply {
                    id,
                    result: p.result,
                })
                .map_err(|_| "Backend response could not be dispatched")?;
            e.requests
                .get_mut(&(p.conversation_id.clone(), key))
                .unwrap()
                .3 = true;
            Ok(
                json!({"schemaVersion":1,"conversation":e.chat(&p.conversation_id)?.summary(),"response":{"dispatched":true,"confirmed":false}}),
            )
        }
        _ => Err("Unsupported conversation service operation".into()),
    }
}

/// The host a conversation is listed under: the page's host without `www.`.
fn page_site(url: &str) -> Option<String> {
    let parsed = url::Url::parse(url).ok()?;
    if !matches!(parsed.scheme(), "http" | "https") {
        return None;
    }
    let host = parsed.host_str()?.to_ascii_lowercase();
    Some(
        host.strip_prefix("www.")
            .unwrap_or(&host)
            .chars()
            .take(253)
            .collect(),
    )
}

fn automatic_approval(method: &str, params: &Value) -> Option<Value> {
    match method {
        "session/request_permission" => params["options"]
            .as_array()?
            .iter()
            .find(|option| option["kind"] == "allow_once")
            .map(|option| json!({"outcome":{"outcome":"selected","optionId":option["optionId"]}})),
        "item/commandExecution/requestApproval" | "item/fileChange/requestApproval" => {
            Some(json!({"decision":"accept"}))
        }
        "item/permissions/requestApproval" => Some(
            json!({"permissions":params.get("permissions").cloned().unwrap_or(json!({})),"scope":"turn"}),
        ),
        "mcpServer/elicitation/request" => Some(json!({"action":"accept","content":{}})),
        "can_use_tool" if params["tool_name"] != "AskUserQuestion" => Some(
            json!({"behavior":"allow","updatedInput":params.get("input").cloned().unwrap_or(json!({}))}),
        ),
        _ => None,
    }
}

fn validate_reply(method: &str, original: &Value, result: &Value) -> Result<(), String> {
    if original["truncated"] == true {
        return Err("The request is too large to review through this surface".into());
    }
    let valid = match method {
        "session/request_permission" => {
            result.as_object().is_some_and(|o| o.len() == 1)
                && result["outcome"].as_object().is_some_and(|o| {
                    (o.len() == 1 && result["outcome"]["outcome"] == "cancelled")
                        || (o.len() == 2
                            && result["outcome"]["outcome"] == "selected"
                            && result["outcome"]["optionId"].is_string()
                            && original["options"].as_array().is_some_and(|options| {
                                options.iter().any(|option| {
                                    option["optionId"] == result["outcome"]["optionId"]
                                })
                            }))
                })
        }
        "item/commandExecution/requestApproval" | "item/fileChange/requestApproval" => {
            result.as_object().is_some_and(|o| {
                o.len() == 1
                    && ["accept", "acceptForSession", "decline", "cancel"]
                        .contains(&result["decision"].as_str().unwrap_or(""))
            })
        }
        "item/tool/requestUserInput" => {
            result.as_object().is_some_and(|o| o.len() == 1)
                && result["answers"].as_object().is_some_and(|o| {
                    o.iter().all(|(key, v)| {
                        original["questions"]
                            .as_array()
                            .is_some_and(|q| q.iter().any(|q| q["id"] == *key))
                            && v["answers"]
                                .as_array()
                                .is_some_and(|a| a.iter().all(Value::is_string))
                    })
                })
        }
        "can_use_tool" => {
            ["allow", "deny"].contains(&result["behavior"].as_str().unwrap_or(""))
                && (result["behavior"] == "deny" || result.get("updatedInput").is_some())
        }
        "mcpServer/elicitation/request" => {
            let action = result["action"].as_str().unwrap_or("");
            let known_fields = result.as_object().is_some_and(|o| {
                o.keys()
                    .all(|k| ["action", "content"].contains(&k.as_str()))
            });
            let approval = original["serverName"] == "chrome_use"
                && original["mode"] == "form"
                && original["_meta"]["codex_approval_kind"] == "mcp_tool_call"
                && original["requestedSchema"]["type"] == "object"
                && original["requestedSchema"]["properties"]
                    .as_object()
                    .is_some_and(|o| o.is_empty())
                && original["requestedSchema"]
                    .get("required")
                    .is_none_or(|value| value.as_array().is_some_and(|a| a.is_empty()))
                && original["_meta"]["tool_params"].is_object();
            known_fields
                && match action {
                    "accept" => {
                        approval && result["content"].as_object().is_some_and(|o| o.is_empty())
                    }
                    "decline" | "cancel" => result.get("content").is_none_or(Value::is_null),
                    _ => false,
                }
        }
        "item/permissions/requestApproval" => {
            result.get("permissions").is_some()
                && ["turn", "session"].contains(&result["scope"].as_str().unwrap_or(""))
        }
        _ => false,
    };
    if valid {
        Ok(())
    } else {
        Err("Response does not match the backend request's supported schema".into())
    }
}

fn start<'a>(
    engine: &'a Arc<Mutex<Engine>>,
    id: &'a str,
    message: Message,
) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), String>> + std::marker::Send + 'a>>
{
    Box::pin(async move {
        let mut e = engine.lock().await;
        let wanted = e.chat(id)?.full_access;
        // An idle process started with other permission flags is replaced.
        // Bumping the generation first makes its exit notice stale, so it
        // cannot fail the turn that is about to start.
        if e.jobs.get(id).is_some_and(|job| job.full_access != wanted) {
            if let Some(job) = e.jobs.remove(id) {
                let _ = job.sender.try_send(BackendCommand::Shutdown);
            }
            e.chat(id)?.generation += 1;
            e.requests.retain(|(chat, _), _| chat != id);
        }
        if let Some(job) = e.jobs.get(id) {
            let sender = job.sender.clone();
            drop(e);
            return sender
                .send(BackendCommand::Start(message))
                .await
                .map_err(|_| {
                    "Backend process became unavailable before receiving the turn".into()
                });
        }
        if e.jobs.len() >= 4 {
            let idle = e
                .jobs
                .keys()
                .filter(|key| {
                    e.chats
                        .get(*key)
                        .is_some_and(|c| !c.phase.busy() && c.phase != Phase::RecoveryRequired)
                })
                .min_by_key(|key| e.chats.get(*key).map(|c| c.updated).unwrap_or(0))
                .cloned();
            if let Some(idle) = idle {
                let sender = e
                    .jobs
                    .get(&idle)
                    .ok_or("Idle backend disappeared")?
                    .sender
                    .clone();
                sender
                    .try_send(BackendCommand::Shutdown)
                    .map_err(|_| "Idle backend shutdown was not dispatched")?;
                drop(e);
                let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
                loop {
                    let locked = engine.lock().await;
                    if !locked.jobs.contains_key(&idle) {
                        e = locked;
                        break;
                    }
                    drop(locked);
                    if tokio::time::Instant::now() > deadline {
                        return Err("Idle backend shutdown has not been confirmed".into());
                    }
                    tokio::time::sleep(Duration::from_millis(30)).await;
                }
            } else {
                return Err(
                    "Four conversations are running; wait or stop one before starting another"
                        .into(),
                );
            }
        }
        let context = DiscoveryContext::current()?;
        let chat = e.chat(id)?.clone();
        let binary = super::agent_binary(&chat.agent_id)?;
        let executable = chat
            .executable
            .clone()
            .or_else(|| context.executable(binary))
            .ok_or("The selected local agent CLI is not installed")?;
        if !super::is_executable(&executable, cfg!(windows)) {
            return Err("Selected agent program is not an executable file".into());
        }
        let workspace = e.store.directory.join(id).join("workspace");
        private_tree(&workspace)?;
        let profile = e.profile.clone();
        let generation = {
            let running = e.chat(id)?;
            running.generation += 1;
            running.generation
        };
        e.persist(id)?;
        let (commands, receive) = mpsc::channel(32);
        let (events, mut notices) = mpsc::channel(256);
        e.jobs.insert(
            id.into(),
            Job {
                generation,
                full_access: chat.full_access,
                sender: commands.clone(),
            },
        );
        let setup = Setup {
            agent_id: chat.agent_id,
            executable,
            workspace,
            profile,
            browser_session: format!("panel-{id}"),
            backend_session: chat.session_id,
            model: chat.requested_model,
            mcp_executable: std::env::current_exe()
                .map_err(|_| "Cannot locate browser tools executable")?,
            full_access: chat.full_access,
        };
        drop(e);
        let owner = engine.clone();
        let chat_id = id.to_string();
        tokio::spawn(async move {
            backend::run(setup, receive, events).await;
        });
        tokio::spawn(async move {
            while let Some(notice) = notices.recv().await {
                let mut e = owner.lock().await;
                let next = e.notice(&chat_id, generation, notice);
                match next {
                    Ok(Some(message)) => {
                        drop(e);
                        if let Err(error) = start(&owner, &chat_id, message).await {
                            let mut e = owner.lock().await;
                            if let Some(chat) = e.chats.get_mut(&chat_id) {
                                chat.phase = Phase::RecoveryRequired;
                                chat.queue_paused = true;
                                chat.last_error = Some(error);
                            }
                            let _ = e.persist(&chat_id);
                        }
                    }
                    Ok(None) => {}
                    Err(error) => {
                        if let Some(chat) = e.chats.get_mut(&chat_id) {
                            chat.phase = Phase::RecoveryRequired;
                            chat.queue_paused = true;
                            chat.last_error = Some(error);
                        }
                        let _ = e.persist(&chat_id);
                        if let Some(job) = e.jobs.get(&chat_id) {
                            let _ = job.sender.try_send(BackendCommand::Shutdown);
                        }
                    }
                }
            }
        });
        commands
            .send(BackendCommand::Start(message))
            .await
            .map_err(|_| "Backend process closed before accepting the turn".into())
    })
}

pub fn run(args: &[String]) -> Result<(), String> {
    if args.len() != 4 || args[0] != "--profile" || args[2] != "--data-dir" {
        return Err("Invalid private agent-service startup arguments".into());
    }
    let profile = args[1].clone();
    let root = PathBuf::from(&args[3]);
    let directory = profile_directory(&root, &profile)?;
    let ownership = private_open(&directory.join("service.lock"), true)?;
    ownership
        .try_lock()
        .map_err(|_| "Another agent service owns this profile")?;
    let store = Store::open(directory.join("conversations"))?;
    let engine = Arc::new(Mutex::new(Engine {
        store,
        chats: HashMap::new(),
        jobs: HashMap::new(),
        requests: HashMap::new(),
        profile: profile.clone(),
        stop: false,
    }));
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .map_err(|_| "Cannot initialize agent-service runtime")?;
    runtime.block_on(async {
        let listener=tokio::net::TcpListener::bind("127.0.0.1:0").await.map_err(|_|"Cannot bind private agent-service transport")?;
        let endpoint=Endpoint {schema_version:1,profile,address:listener.local_addr().map_err(|_|"Cannot inspect agent-service address")?,capability:format!("{}{}",uuid::Uuid::new_v4().simple(),uuid::Uuid::new_v4().simple()),binary_hash:BINARY_HASH.as_ref().map_err(Clone::clone)?.clone(),pid:std::process::id()};
        atomic_write(&directory.join("service.json"),&serde_json::to_vec(&endpoint).map_err(|_|"Cannot encode private service endpoint")?,None)?;
        let slots=Arc::new(tokio::sync::Semaphore::new(16));
        loop {
            tokio::select! {
                accepted=listener.accept()=>{
                    let (mut stream,_)=accepted.map_err(|_|"Agent-service accept failed")?;let permit=slots.clone().try_acquire_owned();let capability=endpoint.capability.clone();let engine=engine.clone();
                    tokio::spawn(async move {
                        let Ok(_permit)=permit else{return;};
                        let task=async {
                            let read=async {
                                let mut length=[0u8;4];stream.read_exact(&mut length).await.map_err(|_|())?;let length=u32::from_le_bytes(length) as usize;if length>MAX_FRAME {return Err(());}
                                let mut bytes=vec![0u8;length];stream.read_exact(&mut bytes).await.map_err(|_|())?;serde_json::from_slice::<Request>(&bytes).map_err(|_|())
                            };
                            let request=tokio::time::timeout(Duration::from_secs(10),read).await.map_err(|_|())??;
                            if request.capability!=capability || request.request_id.len()>100 {return Err(());}
                            // A slow operation gets its own budget; a timeout answers with an
                            // error instead of closing the connection without a reply.
                            let result=match tokio::time::timeout(Duration::from_secs(operation_seconds(&request.operation)),dispatch(&engine,&request.operation,&request.params)).await {Ok(result)=>result,Err(_)=>Err(format!("{} did not finish in time; its outcome is unconfirmed",request.operation))};
                            let reply=match result {Ok(result)=>json!({"requestId":request.request_id,"result":result}),Err(error)=>json!({"requestId":request.request_id,"error":error})};let bytes=reply.to_string().into_bytes();if bytes.len()>MAX_FRAME{return Err(());}
                            stream.write_all(&(bytes.len() as u32).to_le_bytes()).await.map_err(|_|())?;stream.write_all(&bytes).await.map_err(|_|())?;Ok::<(),()>(())
                        };let _=task.await;
                    });
                }
                _=tokio::time::sleep(Duration::from_millis(100))=>{let e=engine.lock().await;if e.stop && e.jobs.is_empty() && slots.available_permits()==16 {break;}}
            }
        }
        let _=std::fs::remove_file(directory.join("service.json"));Ok::<(),String>(())
    })
}

#[cfg(test)]
mod approval_tests {
    use super::*;
    #[test]
    fn mcp_tool_approval_is_once_only_and_requires_reviewable_known_server_metadata() {
        let original = json!({"serverName":"chrome_use","mode":"form","_meta":{"codex_approval_kind":"mcp_tool_call","tool_params":{"url":"http://127.0.0.1/fixture"}},"requestedSchema":{"type":"object","properties":{}}});
        let method = "mcpServer/elicitation/request";
        assert!(
            validate_reply(method, &original, &json!({"action":"accept","content":{}})).is_ok()
        );
        assert!(validate_reply(
            method,
            &original,
            &json!({"action":"accept","content":{},"_meta":{"persist":"always"}})
        )
        .is_err());
        for field in ["serverName", "mode", "_meta", "requestedSchema"] {
            let mut unknown = original.clone();
            unknown[field] = Value::Null;
            assert!(
                validate_reply(method, &unknown, &json!({"action":"accept","content":{}})).is_err()
            );
            assert!(validate_reply(
                method,
                &unknown,
                &json!({"action":"decline","content":null})
            )
            .is_ok());
        }
        let mut needed = original;
        needed["requestedSchema"]["properties"] = json!({"answer":{"type":"string"}});
        assert!(validate_reply(method, &needed, &json!({"action":"accept","content":{}})).is_err());
    }
}

/// Close a deleted conversation's own chrome-use browser session (its tabs).
/// A session with no daemon and no tabs returns at once without starting one.
async fn close_browser_session(profile: &str, session: &str) -> Value {
    let Ok(executable) = std::env::current_exe() else {
        return json!({"session":session,"closed":false,"error":"Cannot locate chrome-use"});
    };
    let run = |args: Vec<&str>, seconds: u64| {
        let mut command = tokio::process::Command::new(&executable);
        command
            .args(&args)
            .env("AGENT_BROWSER_BROWSER", profile)
            .env("AGENT_BROWSER_SESSION", session)
            .env("CHROME_USE_NO_UPDATE_CHECK", "1")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .kill_on_drop(true);
        async move {
            matches!(
                tokio::time::timeout(Duration::from_secs(seconds), command.status()).await,
                Ok(Ok(status)) if status.success()
            )
        }
    };
    // A normal close verifies every tab it closes. A record that can no longer
    // be identified (an old target id, or tabs opened before a Chrome restart)
    // keeps that close incomplete forever, so the deleted conversation's
    // session then drops untracked records, and finally its whole record,
    // which stops its daemon. Only the first step claims the tabs are closed.
    if run(vec!["close"], 12).await {
        return json!({"session":session,"closed":true});
    }
    if run(vec!["close", "--forget-untracked"], 10).await {
        return json!({"session":session,"closed":true,"ownershipDropped":true});
    }
    let stopped = run(vec!["session", "stop", session, "--force"], 8).await;
    json!({"session":session,"closed":false,"recordDropped":stopped,"error":if stopped {"chrome-use could not confirm the session's tabs closed; its record was dropped and its daemon stopped"} else {"chrome-use could not close or drop the session"}})
}

#[cfg(test)]
mod acp_approval_tests {
    use super::*;
    #[test]
    fn acp_only_accepts_advertised_options_and_auto_allows_once() {
        let options = json!({"options":[{"optionId":"reject","kind":"reject_once"},{"optionId":"once","kind":"allow_once"},{"optionId":"always","kind":"allow_always"}]});
        let once = automatic_approval("session/request_permission", &options).unwrap();
        assert_eq!(
            once,
            json!({"outcome":{"outcome":"selected","optionId":"once"}})
        );
        assert!(validate_reply("session/request_permission", &options, &once).is_ok());
        for result in [
            json!({"outcome":{"outcome":"selected","optionId":"forged"}}),
            json!({"outcome":{"outcome":"selected","optionId":null}}),
            json!({"outcome":{"outcome":"cancelled","optionId":"once"}}),
        ] {
            assert!(validate_reply("session/request_permission", &options, &result).is_err());
        }
        assert!(validate_reply(
            "session/request_permission",
            &options,
            &json!({"outcome":{"outcome":"cancelled"}})
        )
        .is_ok());
        assert!(automatic_approval(
            "session/request_permission",
            &json!({"options":[{"optionId":"always","kind":"allow_always"}]})
        )
        .is_none());
    }
}
