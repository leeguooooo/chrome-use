//! Durable conversation intents and events. Backend acceptance and completion
//! are separate states; reconnecting never dispatches an existing message.

use super::config_transaction::{atomic_write, private_tree};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, HashMap, VecDeque};
use std::fs::{self, OpenOptions};
use std::io::{BufRead, BufReader, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

const MAX_STATE: u64 = 8 * 1024 * 1024;
const MAX_HISTORY: u64 = 128 * 1024 * 1024;
const MAX_EVENT: usize = 64 * 1024;

pub(super) fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub(super) enum Phase {
    Idle,
    Starting,
    Running,
    WaitingInput,
    Stopping,
    Failed,
    Interrupted,
    RecoveryRequired,
}
impl Phase {
    pub(super) fn busy(&self) -> bool {
        matches!(
            self,
            Self::Starting | Self::Running | Self::WaitingInput | Self::Stopping
        )
    }
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct Message {
    pub id: String,
    pub text: String,
    /// The tab the user was looking at when they sent the message.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub page: Option<PageContext>,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct PageContext {
    pub tab_id: i64,
    pub url: String,
    pub title: String,
}

impl PageContext {
    pub(super) fn validate(&self) -> Result<(), String> {
        if self.tab_id < 0
            || self.url.len() > 8192
            || self.title.len() > 1024
            || self.url.contains(['\r', '\n', '\0'])
            || self.title.contains('\0')
        {
            return Err("Invalid page context".into());
        }
        Ok(())
    }
    /// The line that tells the agent which tab "this page" means.
    pub(super) fn preface(&self) -> String {
        let title = self.title.replace(['\r', '\n'], " ");
        format!(
            "[The user is looking at their own tab \"{title}\" ({url}). When they refer to \"this page\" or \"the current page\", read or act on it with the chrome_use tools using tabId \"chrome-tab:{id}\" and force: true.]",
            url = self.url,
            id = self.tab_id
        )
    }
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct Event {
    pub sequence: u64,
    pub at: u64,
    pub kind: String,
    pub data: Value,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct Conversation {
    pub schema_version: u32,
    pub id: String,
    pub agent_id: String,
    pub title: String,
    pub created: u64,
    pub updated: u64,
    pub phase: Phase,
    pub generation: u64,
    pub session_id: Option<String>,
    pub model: Option<String>,
    pub requested_model: Option<String>,
    pub active: Option<Message>,
    pub queue: VecDeque<Message>,
    pub queue_paused: bool,
    #[serde(default)]
    pub direct_message_id: Option<String>,
    #[serde(default)]
    pub direct_ready: bool,
    #[serde(default)]
    pub active_direct: bool,
    pub accepted: BTreeMap<String, String>,
    pub next_sequence: u64,
    pub last_error: Option<String>,
    pub executable: Option<PathBuf>,
    /// Run the agent without permission prompts (approvals answered by the
    /// service). Older state has no field and keeps asking.
    #[serde(default)]
    pub full_access: bool,
    /// Host of the page the conversation started from, for the per-site list.
    #[serde(default)]
    pub site: Option<String>,
}

impl Conversation {
    pub(super) fn new(
        agent_id: &str,
        model: Option<String>,
        executable: Option<PathBuf>,
    ) -> Result<Self, String> {
        if !super::CHAT_AGENTS.contains(&agent_id) {
            return Err("This agent has no conversational adapter yet".into());
        }
        if model
            .as_ref()
            .is_some_and(|s| s.is_empty() || s.len() > 256 || s.contains(['\r', '\n', '\0']))
        {
            return Err("Invalid conversation model".into());
        }
        let at = now();
        Ok(Self {
            schema_version: 1,
            id: uuid::Uuid::new_v4().to_string(),
            agent_id: agent_id.into(),
            title: "New conversation".into(),
            created: at,
            updated: at,
            phase: Phase::Idle,
            generation: 0,
            session_id: None,
            model: None,
            requested_model: model,
            active: None,
            queue: VecDeque::new(),
            queue_paused: false,
            direct_message_id: None,
            direct_ready: false,
            active_direct: false,
            accepted: BTreeMap::new(),
            next_sequence: 1,
            last_error: None,
            executable,
            full_access: false,
            site: None,
        })
    }
    pub(super) fn summary(&self) -> Value {
        json!({"id":self.id,"agentId":self.agent_id,"title":self.title,"created":self.created,"updated":self.updated,"phase":self.phase,"generation":self.generation,"model":self.model,"requestedModel":self.requested_model,"queued":self.queue.len(),"queuePaused":self.queue_paused,"directMessageId":self.direct_message_id,"activeMessageId":self.active.as_ref().map(|m|&m.id),"lastError":self.last_error,"lastSequence":self.next_sequence.saturating_sub(1),"fullAccess":self.full_access,"site":self.site,"target":{"mode":"agentSession","session":format!("panel-{}",self.id)}})
    }
    pub(super) fn queue_snapshot(&self) -> Value {
        Value::Array(
            self.queue
                .iter()
                .map(|message| {
                    let text: String = message.text.chars().take(1024).collect();
                    json!({"id":message.id,"truncated":text.len()!=message.text.len(),"text":text,"direct":self.direct_message_id.as_ref()==Some(&message.id)})
                })
                .collect(),
        )
    }
    pub(super) fn accept(&mut self, message: Message) -> Result<bool, String> {
        uuid::Uuid::parse_str(&message.id).map_err(|_| "A message needs a stable UUID")?;
        if message.text.trim().is_empty() || message.text.len() > 32 * 1024 {
            return Err("A message must contain between one and 32768 bytes of text".into());
        }
        let hash = hex::encode(Sha256::digest(message.text.as_bytes()));
        if let Some(existing) = self.accepted.get(&message.id) {
            if existing != &hash {
                return Err("A message id was reused for different text".into());
            }
            return Ok(false);
        }
        if self.phase == Phase::RecoveryRequired {
            return Err("The previous turn is unconfirmed; inspect it before continuing".into());
        }
        if self.queue.len() >= 64 || self.accepted.len() >= 50000 {
            return Err("Conversation queue or deduplication capacity reached".into());
        }
        if self.accepted.is_empty() {
            self.title = message
                .text
                .lines()
                .next()
                .unwrap_or("New conversation")
                .chars()
                .take(80)
                .collect();
        }
        self.accepted.insert(message.id.clone(), hash);
        self.queue.push_back(message);
        self.updated = now();
        Ok(true)
    }
    pub(super) fn begin_next(&mut self) -> Option<Message> {
        if self.phase.busy() || self.phase == Phase::RecoveryRequired {
            return None;
        }
        let next = if let Some(id) = &self.direct_message_id {
            if !self.direct_ready {
                return None;
            }
            let index = self.queue.iter().position(|m| &m.id == id)?;
            self.direct_message_id = None;
            self.direct_ready = false;
            self.active_direct = true;
            self.queue.remove(index)?
        } else {
            if self.queue_paused {
                return None;
            }
            self.active_direct = false;
            self.queue.pop_front()?
        };
        self.active = Some(next.clone());
        self.phase = Phase::Starting;
        self.last_error = None;
        Some(next)
    }
    pub(super) fn accept_direct(&mut self, message: Message) -> Result<bool, String> {
        if self.accepted.contains_key(&message.id) {
            return self.accept(message);
        }
        if self.direct_message_id.is_some() {
            return Err("A direct message is already waiting for stop confirmation".into());
        }
        let ready = !self.phase.busy();
        let id = message.id.clone();
        let accepted = self.accept(message)?;
        self.direct_message_id = Some(id);
        self.direct_ready = ready;
        // Ordinary queued messages remain paused after a direct turn. The user
        // resumes them explicitly instead of accidentally restarting old work.
        self.queue_paused = true;
        Ok(accepted)
    }
    pub(super) fn edit_queued(
        &mut self,
        id: &str,
        expected: &str,
        replacement: Message,
    ) -> Result<(), String> {
        let index = self
            .queue
            .iter()
            .position(|m| m.id == id)
            .ok_or("Message is no longer queued; read its current state")?;
        if self.queue[index].text != expected {
            return Err("Queued message changed; read its current state".into());
        }
        if self.accepted.contains_key(&replacement.id) {
            return Err("Replacement message id has already been used".into());
        }
        // Keep the original accepted ID as a tombstone so a delayed send cannot
        // put the superseded text back in the queue.
        uuid::Uuid::parse_str(&replacement.id)
            .map_err(|_| "Replacement message needs a stable UUID")?;
        if replacement.text.trim().is_empty() || replacement.text.len() > 32768 {
            return Err("Replacement text must contain between one and 32768 bytes".into());
        }
        if self.accepted.len() >= 50000 {
            return Err("Conversation deduplication capacity reached".into());
        }
        self.accepted.insert(
            replacement.id.clone(),
            hex::encode(Sha256::digest(replacement.text.as_bytes())),
        );
        if self.direct_message_id.as_deref() == Some(id) {
            self.direct_message_id = Some(replacement.id.clone());
        }
        let mut replacement = replacement;
        replacement.page = self.queue[index].page.take();
        self.queue[index] = replacement;
        self.updated = now();
        Ok(())
    }
    pub(super) fn remove_queued(&mut self, id: &str, expected: &str) -> Result<(), String> {
        let index = self
            .queue
            .iter()
            .position(|m| m.id == id)
            .ok_or("Message is no longer queued; read its current state")?;
        if self.queue[index].text != expected {
            return Err("Queued message changed; read its current state".into());
        }
        self.queue.remove(index);
        if self.direct_message_id.as_deref() == Some(id) {
            self.direct_message_id = None;
            self.direct_ready = false;
        }
        self.updated = now();
        Ok(())
    }
    pub(super) fn terminal(
        &mut self,
        generation: u64,
        outcome: &str,
        error: Option<String>,
    ) -> Result<bool, String> {
        if generation != self.generation || self.active.is_none() {
            return Ok(false);
        }
        self.phase = match outcome {
            "completed" => Phase::Idle,
            "interrupted" => Phase::Interrupted,
            "failed" => Phase::Failed,
            _ => return Err("Unknown backend terminal status".into()),
        };
        self.queue_paused = outcome != "completed"
            || self.direct_message_id.is_some()
            || (self.active_direct && self.queue_paused);
        if self.active_direct && outcome == "completed" && self.queue.is_empty() {
            self.queue_paused = false;
        }
        self.direct_ready =
            self.direct_message_id.is_some() && ["completed", "interrupted"].contains(&outcome);
        self.active_direct = false;
        self.last_error = error;
        self.active = None;
        self.updated = now();
        Ok(true)
    }
}

pub(super) struct Store {
    pub directory: PathBuf,
    index: BTreeMap<String, Value>,
    cursors: Mutex<HashMap<String, JournalCursor>>,
}

struct JournalCursor {
    sequence: u64,
    offset: u64,
    length: u64,
    modified: Option<std::time::SystemTime>,
    identity: (u64, u64),
}
impl Store {
    pub(super) fn open(directory: PathBuf) -> Result<Self, String> {
        private_tree(&directory)?;
        let path = directory.join("index.json");
        let mut index: BTreeMap<String, Value> = if path.exists() {
            let data = read_private(&path, MAX_STATE)?;
            serde_json::from_slice(&data)
                .map_err(|_| "Conversation index is corrupt; existing history was preserved")?
        } else {
            BTreeMap::new()
        };
        for value in index.values_mut() {
            if ["starting", "running", "waitingInput", "stopping"]
                .contains(&value["phase"].as_str().unwrap_or(""))
            {
                value["phase"] = "recoveryRequired".into();
                value["queuePaused"] = true.into();
                value["lastError"] =
                    "The previous service exited; active turn outcome is unconfirmed".into();
            }
        }
        Ok(Self {
            directory,
            index,
            cursors: Mutex::new(HashMap::new()),
        })
    }
    fn folder(&self, id: &str) -> Result<PathBuf, String> {
        uuid::Uuid::parse_str(id).map_err(|_| "Invalid conversation id")?;
        Ok(self.directory.join(id))
    }
    pub(super) fn list(&self) -> Vec<Value> {
        let mut rows: Vec<_> = self.index.values().cloned().collect();
        rows.sort_by_key(|v| std::cmp::Reverse(v["updated"].as_u64().unwrap_or(0)));
        rows
    }
    pub(super) fn create(&mut self, chat: &Conversation) -> Result<(), String> {
        if self.index.len() >= 10000 {
            return Err(
                "Conversation index capacity reached; archive or delete history first".into(),
            );
        }
        let folder = self.folder(&chat.id)?;
        if folder.exists() {
            return Err("Conversation id already exists".into());
        }
        private_tree(&folder)?;
        self.save(chat)
    }
    /// Remove one conversation's index entry, then its own folder. The index is
    /// written first, so an interrupted delete leaves an unlisted folder rather
    /// than a listed conversation without history. Only `<store>/<uuid>` is
    /// ever removed.
    pub(super) fn delete(&mut self, id: &str) -> Result<(), String> {
        let folder = self.folder(id)?;
        if self.index.remove(id).is_none() {
            return Err("Conversation was not found in this profile".into());
        }
        atomic_write(
            &self.directory.join("index.json"),
            &serde_json::to_vec(&self.index).map_err(|_| "Cannot encode conversation index")?,
            None,
        )?;
        if let Ok(mut cursors) = self.cursors.lock() {
            cursors.remove(id);
        }
        match fs::remove_dir_all(&folder) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(_) => Err("The conversation was removed from the list, but its local folder could not be deleted".into()),
        }
    }
    pub(super) fn load(&mut self, id: &str) -> Result<Conversation, String> {
        if !self.index.contains_key(id) {
            return Err("Conversation was not found in this profile".into());
        }
        let raw = read_private(&self.folder(id)?.join("state.json"), MAX_STATE)?;
        let mut chat: Conversation = serde_json::from_slice(&raw)
            .map_err(|_| "Conversation state is corrupt; it was not replaced")?;
        if chat.schema_version != 1 || chat.id != id {
            return Err("Conversation identity does not match its state".into());
        }
        let cursor = self.last_sequence(id)?;
        if chat.next_sequence > cursor + 1 {
            return Err(
                "Conversation state is ahead of its journal; history cannot be assumed complete"
                    .into(),
            );
        }
        chat.next_sequence = cursor + 1;
        if chat.phase.busy() {
            chat.phase = Phase::RecoveryRequired;
            chat.queue_paused = true;
            chat.last_error=Some("The service lost its previous process; turn outcome is unconfirmed and no message was replayed".into());
            self.save(&chat)?;
        }
        Ok(chat)
    }
    pub(super) fn save(&mut self, chat: &Conversation) -> Result<(), String> {
        let bytes = serde_json::to_vec(chat).map_err(|_| "Cannot encode conversation state")?;
        if bytes.len() as u64 > MAX_STATE {
            return Err("Conversation state capacity reached".into());
        }
        atomic_write(&self.folder(&chat.id)?.join("state.json"), &bytes, None)?;
        self.index.insert(chat.id.clone(), chat.summary());
        atomic_write(
            &self.directory.join("index.json"),
            &serde_json::to_vec(&self.index).map_err(|_| "Cannot encode conversation index")?,
            None,
        )
    }
    pub(super) fn event(
        &mut self,
        chat: &mut Conversation,
        kind: &str,
        data: Value,
    ) -> Result<Event, String> {
        let event = Event {
            sequence: chat.next_sequence,
            at: now(),
            kind: kind.into(),
            data,
        };
        let mut raw = serde_json::to_vec(&event).map_err(|_| "Cannot encode conversation event")?;
        if raw.len() > MAX_EVENT {
            return Err("Conversation event exceeds its size limit".into());
        }
        raw.push(b'\n');
        let path = self.folder(&chat.id)?.join("events.jsonl");
        if let Ok(meta) = fs::symlink_metadata(&path) {
            if !meta.is_file()
                || meta.file_type().is_symlink()
                || meta.len() + raw.len() as u64 > MAX_HISTORY
            {
                return Err("Conversation history cannot accept this event".into());
            }
        }
        let mut options = OpenOptions::new();
        options.append(true).create(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options
                .mode(0o600)
                .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
        }
        let mut file = options
            .open(path)
            .map_err(|_| "Cannot open conversation journal")?;
        let before = file
            .metadata()
            .map_err(|_| "Cannot inspect journal before append")?;
        file.write_all(&raw)
            .map_err(|_| "Cannot append conversation event")?;
        // Only our append can advance a cached prefix's file stamp. External
        // changes invalidate it and will be validated from the beginning.
        let after = file
            .metadata()
            .map_err(|_| "Cannot inspect journal after append")?;
        let mut cache = self
            .cursors
            .lock()
            .map_err(|_| "Journal cursor cache unavailable")?;
        if let Some(cursor) = cache.get_mut(&chat.id) {
            if cursor.length == before.len()
                && cursor.modified.is_some()
                && cursor.modified == before.modified().ok()
                && cursor.identity == journal_identity(&before)
            {
                cursor.length = after.len();
                cursor.modified = after.modified().ok();
            } else {
                cache.remove(&chat.id);
            }
        }
        drop(cache);
        if kind != "textDelta" {
            file.sync_data()
                .map_err(|_| "Cannot flush conversation event")?;
        }
        chat.next_sequence += 1;
        chat.updated = event.at;
        Ok(event)
    }
    pub(super) fn events(&self, id: &str, after: u64) -> Result<Value, String> {
        if !self.index.contains_key(id) {
            return Err("Conversation was not found in this profile".into());
        }
        let path = self.folder(id)?.join("events.jsonl");
        if !path.exists() {
            return Ok(json!({"events":[],"cursor":after,"more":false}));
        }
        let mut file = open_journal(&path)?;
        let meta = file.metadata().map_err(|_| "Cannot inspect journal")?;
        let cached = self
            .cursors
            .lock()
            .map_err(|_| "Journal cursor cache unavailable")?;
        let offset = cached
            .get(id)
            .filter(|c| {
                c.sequence == after
                    && c.length == meta.len()
                    && c.modified.is_some()
                    && c.modified == meta.modified().ok()
                    && c.identity == journal_identity(&meta)
            })
            .map(|c| c.offset)
            .unwrap_or(0);
        drop(cached);
        file.seek(SeekFrom::Start(offset))
            .map_err(|_| "Cannot seek journal")?;
        let mut rows = Vec::new();
        let mut bytes = 0;
        let mut cursor = after;
        let mut more = false;
        let mut previous = if offset == 0 { 0 } else { after };
        let mut reader = BufReader::new(file);
        let mut position = offset;
        let mut accepted_offset = offset;
        let mut line = Vec::new();
        loop {
            line.clear();
            let read = reader
                .read_until(b'\n', &mut line)
                .map_err(|_| "Cannot read conversation history")?;
            if read == 0 {
                break;
            }
            position += read as u64;
            if position > MAX_HISTORY {
                return Err("Conversation history exceeds its size limit".into());
            }
            if line == b"\n" {
                continue;
            }
            if line.len() > MAX_EVENT + 1 || !line.ends_with(b"\n") {
                return Err("Conversation journal line is incomplete or oversized".into());
            }
            let event:Event=serde_json::from_slice(&line).map_err(|_|"Conversation journal is incomplete or corrupt; do not assume missing events succeeded")?;
            if event.sequence != previous + 1 {
                return Err("Conversation event sequence has a gap or duplicate".into());
            }
            previous = event.sequence;
            if event.sequence <= after {
                continue;
            }
            if rows.len() >= 128 || bytes + line.len() > 512 * 1024 {
                more = true;
                break;
            }
            bytes += line.len();
            cursor = event.sequence;
            accepted_offset = position;
            rows.push(event);
        }
        let mut cache = self
            .cursors
            .lock()
            .map_err(|_| "Journal cursor cache unavailable")?;
        if cache.len() >= 64 {
            cache.clear();
        }
        cache.insert(
            id.into(),
            JournalCursor {
                sequence: cursor,
                offset: accepted_offset,
                length: meta.len(),
                modified: meta.modified().ok(),
                identity: journal_identity(&meta),
            },
        );
        Ok(json!({"events":rows,"cursor":cursor,"more":more}))
    }

    fn last_sequence(&self, id: &str) -> Result<u64, String> {
        let path = self.folder(id)?.join("events.jsonl");
        if !path.exists() {
            return Ok(0);
        }
        let file = open_journal(&path)?;
        let mut previous = 0;
        let mut consumed = 0;
        let mut reader = BufReader::new(file);
        let mut line = Vec::new();
        loop {
            line.clear();
            let read = reader
                .read_until(b'\n', &mut line)
                .map_err(|_| "Cannot read conversation history")?;
            consumed += read as u64;
            if consumed > MAX_HISTORY {
                return Err("Conversation history exceeds its size limit".into());
            }
            if read == 0 {
                break;
            }
            if line == b"\n" {
                continue;
            }
            if line.len() > MAX_EVENT + 1 || !line.ends_with(b"\n") {
                return Err("Conversation journal line is incomplete or oversized".into());
            }
            let event: Event = serde_json::from_slice(&line)
                .map_err(|_| "Conversation journal is incomplete or corrupt")?;
            if event.sequence != previous + 1 {
                return Err("Conversation event sequence has a gap or duplicate".into());
            }
            previous = event.sequence;
        }
        Ok(previous)
    }
}

fn journal_identity(meta: &fs::Metadata) -> (u64, u64) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        (meta.dev(), meta.ino())
    }
    #[cfg(not(unix))]
    {
        let _ = meta;
        (0, 0)
    }
}

fn open_journal(path: &Path) -> Result<fs::File, String> {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let file = options
        .open(path)
        .map_err(|_| "Cannot open conversation journal")?;
    let meta = file
        .metadata()
        .map_err(|_| "Cannot inspect conversation journal")?;
    if !meta.is_file() || meta.len() > MAX_HISTORY {
        return Err("Conversation journal is not a bounded regular file".into());
    }
    Ok(file)
}

fn read_private(path: &Path, max: u64) -> Result<Vec<u8>, String> {
    use std::io::Read;
    let meta = fs::symlink_metadata(path).map_err(|_| "Cannot inspect conversation file")?;
    if !meta.is_file() || meta.file_type().is_symlink() || meta.len() > max {
        return Err("Conversation file is not a bounded regular file".into());
    }
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let mut data = Vec::new();
    options
        .open(path)
        .map_err(|_| "Cannot open conversation file")?
        .take(max + 1)
        .read_to_end(&mut data)
        .map_err(|_| "Cannot read conversation file")?;
    if data.len() as u64 > max {
        return Err("Conversation file exceeds its size limit".into());
    }
    Ok(data)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn message(text: &str) -> Message {
        Message {
            id: uuid::Uuid::new_v4().to_string(),
            text: text.into(),
            page: None,
        }
    }
    #[test]
    fn journal_pages_seek_forward_after_our_appends_and_reject_external_corruption() {
        let root = tempfile::tempdir().unwrap();
        let mut store = Store::open(root.path().join("chats")).unwrap();
        let mut chat = Conversation::new("codex", None, None).unwrap();
        store.create(&chat).unwrap();
        for _ in 0..300 {
            store
                .event(&mut chat, "textDelta", json!({"text":"chunk"}))
                .unwrap();
        }
        store.save(&chat).unwrap();
        assert_eq!(store.load(&chat.id).unwrap().next_sequence, 301);
        assert_eq!(store.events(&chat.id, 0).unwrap()["cursor"], 128);
        let offset = store.cursors.lock().unwrap()[&chat.id].offset;
        assert!(offset > 0);
        store
            .event(&mut chat, "textDelta", json!({"text":"tail"}))
            .unwrap();
        assert_eq!(store.cursors.lock().unwrap()[&chat.id].offset, offset);
        assert_eq!(
            store.events(&chat.id, 128).unwrap()["events"][0]["sequence"],
            129
        );
        assert_eq!(store.events(&chat.id, 256).unwrap()["cursor"], 301);
        let path = store.folder(&chat.id).unwrap().join("events.jsonl");
        #[cfg(unix)]
        {
            let stamp = std::fs::metadata(&path).unwrap().modified().unwrap();
            let mut raw = std::fs::read(&path).unwrap();
            raw[0] = b'!';
            let replacement = path.with_extension("replacement");
            std::fs::write(&replacement, &raw).unwrap();
            std::fs::File::open(&replacement)
                .unwrap()
                .set_times(std::fs::FileTimes::new().set_modified(stamp))
                .unwrap();
            std::fs::rename(replacement, &path).unwrap();
            assert!(store.events(&chat.id, 301).is_err());
        }
        std::fs::write(path, b"corrupt\n").unwrap();
        assert!(store.events(&chat.id, 301).is_err());
    }
    #[test]
    fn duplicate_send_is_idempotent_but_reusing_an_id_for_other_text_is_refused() {
        let mut c = Conversation::new("codex", None, None).unwrap();
        let m = message("one");
        assert!(c.accept(m.clone()).unwrap());
        assert!(!c.accept(m.clone()).unwrap());
        let mut different = m;
        different.text = "two".into();
        assert!(c.accept(different).is_err());
        assert_eq!(c.queue.len(), 1);
    }
    #[test]
    fn failure_pauses_queued_messages_and_obsolete_processes_cannot_finish_a_turn() {
        let mut c = Conversation::new("codex", None, None).unwrap();
        c.accept(message("one")).unwrap();
        c.accept(message("two")).unwrap();
        c.begin_next().unwrap();
        c.generation = 2;
        assert!(!c.terminal(1, "completed", None).unwrap());
        assert!(c
            .terminal(2, "failed", Some("fixture failure".into()))
            .unwrap());
        assert!(c.begin_next().is_none());
        assert_eq!(c.queue.len(), 1);
    }
    #[test]
    fn queued_edits_preserve_order_and_old_ids_cannot_reinsert_superseded_text() {
        let mut c = Conversation::new("codex", None, None).unwrap();
        let first = message("first");
        let second = message("second");
        let edited = message("edited");
        c.accept(first.clone()).unwrap();
        c.accept(second.clone()).unwrap();
        c.edit_queued(&first.id, "first", edited.clone()).unwrap();
        assert!(!c.accept(first).unwrap());
        assert_eq!(c.queue.len(), 2);
        assert_eq!(c.begin_next().unwrap().id, edited.id);
        assert_eq!(c.queue[0].id, second.id);
    }
    #[test]
    fn large_queues_have_bounded_previews_without_truncating_stored_messages() {
        let mut c = Conversation::new("codex", None, None).unwrap();
        for _ in 0..64 {
            c.accept(message(&"\u{0001}".repeat(32768))).unwrap();
        }
        let snapshot = c.queue_snapshot();
        assert!(serde_json::to_vec(&snapshot).unwrap().len() < 450000);
        assert_eq!(snapshot[0]["truncated"], true);
        assert_eq!(c.queue[0].text.len(), 32768);
    }
    #[test]
    fn direct_message_waits_for_confirmation_and_leaves_ordinary_queue_paused() {
        let mut c = Conversation::new("codex", None, None).unwrap();
        c.accept(message("active")).unwrap();
        c.begin_next();
        let ordinary = message("ordinary");
        c.accept(ordinary.clone()).unwrap();
        let direct = message("direct");
        assert!(c.accept_direct(direct.clone()).unwrap());
        assert!(!c.accept_direct(direct.clone()).unwrap());
        assert!(c.begin_next().is_none());
        c.terminal(0, "interrupted", None).unwrap();
        assert_eq!(c.begin_next().unwrap().id, direct.id);
        c.terminal(0, "completed", None).unwrap();
        assert!(c.queue_paused);
        assert!(c.begin_next().is_none());
        c.queue_paused = false;
        assert_eq!(c.begin_next().unwrap().id, ordinary.id);
    }
    #[test]
    fn failed_or_recovered_turn_never_automatically_sends_pending_direct_text() {
        let mut c = Conversation::new("claude", None, None).unwrap();
        c.accept(message("active")).unwrap();
        c.begin_next();
        c.accept_direct(message("direct")).unwrap();
        assert!(c.accept_direct(message("another direct")).is_err());
        c.terminal(0, "failed", None).unwrap();
        assert!(c.begin_next().is_none());
        c.phase = Phase::RecoveryRequired;
        c.direct_ready = true;
        assert!(c.begin_next().is_none());
        assert_eq!(c.queue.len(), 1);
    }
    #[test]
    fn deleting_or_editing_pending_direct_text_updates_its_immutable_identity() {
        let mut c = Conversation::new("codex", None, None).unwrap();
        let old = message("old direct");
        c.accept_direct(old.clone()).unwrap();
        let new = message("new direct");
        c.edit_queued(&old.id, &old.text, new.clone()).unwrap();
        assert_eq!(c.direct_message_id.as_ref(), Some(&new.id));
        c.remove_queued(&new.id, &new.text).unwrap();
        assert!(c.direct_message_id.is_none());
        assert!(!c.direct_ready);
        assert!(c.begin_next().is_none());
    }
    #[test]
    fn restart_keeps_direct_intent_but_does_not_replay_it_after_an_unconfirmed_turn() {
        let directory = tempfile::tempdir().unwrap();
        let mut store = Store::open(directory.path().join("chats")).unwrap();
        let mut c = Conversation::new("codex", None, None).unwrap();
        c.accept(message("active")).unwrap();
        c.begin_next();
        let direct = message("direct after stop");
        c.accept_direct(direct.clone()).unwrap();
        c.phase = Phase::Stopping;
        store.create(&c).unwrap();
        let mut restored = store.load(&c.id).unwrap();
        assert_eq!(restored.phase, Phase::RecoveryRequired);
        assert_eq!(restored.direct_message_id, Some(direct.id));
        assert!(restored.begin_next().is_none());
        assert_eq!(restored.queue.len(), 1);
    }
    #[test]
    fn completion_race_releases_direct_intent_but_an_empty_direct_turn_does_not_pause_new_work() {
        let mut c = Conversation::new("codex", None, None).unwrap();
        c.accept(message("old active")).unwrap();
        c.begin_next();
        let direct = message("direct");
        c.accept_direct(direct.clone()).unwrap();
        c.terminal(0, "completed", None).unwrap();
        assert_eq!(c.begin_next().unwrap().id, direct.id);
        c.terminal(0, "completed", None).unwrap();
        assert!(!c.queue_paused);
        c.accept(message("new work")).unwrap();
        assert_eq!(c.begin_next().unwrap().text, "new work");
    }
    #[test]
    fn stale_edit_and_removal_never_change_active_or_other_queued_messages() {
        let mut c = Conversation::new("claude", None, None).unwrap();
        let active = message("active");
        let queued = message("queued");
        c.accept(active.clone()).unwrap();
        c.accept(queued.clone()).unwrap();
        c.begin_next();
        assert!(c
            .edit_queued(&active.id, "active", message("wrong"))
            .is_err());
        assert!(c.remove_queued(&active.id, "active").is_err());
        assert!(c
            .edit_queued(&queued.id, "stale", message("wrong"))
            .is_err());
        assert!(c.remove_queued(&queued.id, "stale").is_err());
        assert_eq!(c.queue[0].text, "queued");
        c.remove_queued(&queued.id, "queued").unwrap();
        assert!(!c.accept(queued).unwrap());
        assert!(c.queue.is_empty());
        assert_eq!(c.active.unwrap().id, active.id);
    }
    #[test]
    fn unknown_terminal_status_is_not_success() {
        let mut c = Conversation::new("claude", None, None).unwrap();
        c.accept(message("one")).unwrap();
        c.begin_next();
        assert!(c.terminal(0, "mystery", None).is_err());
        assert!(c.phase.busy());
    }
    #[test]
    fn restart_preserves_history_and_marks_running_work_unconfirmed_without_resubmitting() {
        let t = tempfile::tempdir().unwrap();
        let mut s = Store::open(t.path().join("chats")).unwrap();
        let mut c = Conversation::new("codex", None, None).unwrap();
        s.create(&c).unwrap();
        c.accept(message("one")).unwrap();
        c.begin_next();
        s.event(&mut c, "user", json!({"text":"one"})).unwrap();
        s.save(&c).unwrap();
        let mut reopened = Store::open(t.path().join("chats")).unwrap();
        let recovered = reopened.load(&c.id).unwrap();
        assert_eq!(recovered.phase, Phase::RecoveryRequired);
        assert!(recovered.queue_paused);
        assert_eq!(
            reopened.events(&c.id, 0).unwrap()["events"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
    }
    #[test]
    fn event_replay_is_cursor_based_and_corrupt_history_never_becomes_an_empty_success() {
        let t = tempfile::tempdir().unwrap();
        let mut s = Store::open(t.path().join("chats")).unwrap();
        let mut c = Conversation::new("codex", None, None).unwrap();
        s.create(&c).unwrap();
        s.event(&mut c, "user", json!({"text":"one"})).unwrap();
        s.event(&mut c, "text", json!({"text":"two"})).unwrap();
        s.save(&c).unwrap();
        assert_eq!(
            s.events(&c.id, 1).unwrap()["events"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
        let p = s.folder(&c.id).unwrap().join("events.jsonl");
        OpenOptions::new()
            .append(true)
            .open(p)
            .unwrap()
            .write_all(b"{broken")
            .unwrap();
        assert!(s.events(&c.id, 0).is_err());
    }
}
