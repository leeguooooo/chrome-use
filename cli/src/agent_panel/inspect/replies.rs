//! Replies to Inspect & Ask deliveries.
//!
//! A delivery is sent as a sender identity no session can answer to, so the
//! receiving agent's reply wakes nobody: it stays in the ocs channel, where
//! this module reads it and keeps it for the extension. The identity is
//! derived per Chrome profile and checked against the live roster and ocs
//! names before every send. Reply text is agent output about page data: it
//! is redacted and capped before it is stored, and the extension renders it
//! as text.
use super::{
    create_new, delivery_id, line, private_dir, read_regular, Inspect, Session, MAX_RECORD,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
    sync::Mutex,
    time::{Duration, Instant},
};

/// Every sender identity starts with this. The dots keep it out of the
/// `<workspace>-<hex>` shape of automatic Claude session names (a session in
/// the chrome-use repo is `chrome-use-<hex>`) and of the
/// `claude-`/`codex-`/`pi-<hex>` short addresses ocs resolves by prefix.
pub(super) const IDENTITY_PREFIX: &str = "chrome-use.inspect.";
/// Candidates tried in order when a live session or ocs name holds one.
const IDENTITY_CANDIDATES: u32 = 4;
const REPLY_MAX: usize = 16 * 1024;
const MAX_REPLIES: usize = 200;
const REPLY_MAX_AGE: u64 = 7 * 24 * 60 * 60 * 1000;
const MAX_REPLY_STORE: u64 = 8 * 1024 * 1024;
/// Only channels of deliveries this recent are read on a poll.
const POLL_WINDOW: u64 = 24 * 60 * 60 * 1000;
const MAX_CHANNELS: usize = 8;
const POLL_INTERVAL: Duration = Duration::from_secs(10);
pub(super) const READ_TIMEOUT: Duration = Duration::from_secs(8);
const STORE_VERSION: u32 = 1;

/// The sender identity candidate `attempt` for a Chrome profile.
pub(super) fn identity(profile: Option<&str>, attempt: u32) -> String {
    let digest = Sha256::digest(
        format!(
            "chrome-use inspect sender\n{}\n{attempt}",
            profile.unwrap_or("")
        )
        .as_bytes(),
    );
    format!("{IDENTITY_PREFIX}{}", &hex::encode(digest)[..12])
}

/// Every identity this profile may have sent as.
pub(super) fn identities(profile: Option<&str>) -> Vec<String> {
    (0..IDENTITY_CANDIDATES)
        .map(|n| identity(profile, n))
        .collect()
}

/// Whether ocs could deliver a message addressed to `name` to a session:
/// a live session's id, name, ocs name or workspace alias, or a saved ocs
/// name (which follows its session after a restart). Compared without case,
/// as ocs stores names.
pub(super) fn taken(name: &str, roster: &[Session], ocs_home: Option<&Path>) -> bool {
    let lower = name.to_ascii_lowercase();
    if roster
        .iter()
        .flat_map(|s| s.addresses.iter().chain(std::iter::once(&s.id)))
        .any(|a| a.to_ascii_lowercase() == lower)
    {
        return true;
    }
    ocs_home.is_some_and(|home| {
        fs::symlink_metadata(home.join("names").join(format!("{lower}.json"))).is_ok()
    })
}

/// The first candidate identity nothing else answers to.
pub(super) fn pick(
    profile: Option<&str>,
    roster: &[Session],
    ocs_home: Option<&Path>,
) -> Option<String> {
    identities(profile)
        .into_iter()
        .find(|id| !taken(id, roster, ocs_home))
}

/// `$OCS_HOME`, or `~/.ocs`.
pub(super) fn ocs_home() -> Option<PathBuf> {
    std::env::var_os("OCS_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| dirs::home_dir().map(|h| h.join(".ocs")))
}

/// The line that tells the receiving agent where its answer goes.
pub(super) fn reply_hint(identity: &str) -> String {
    format!(
        "Your reply is shown to the user in their browser (sender {identity}), not in a terminal: answer completely in one message using this note's Reply: line. A \"not woken\" or \"no live session\" result for it is expected."
    )
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct Reply {
    /// The batch it answers; `None` when it arrived on a channel no
    /// delivery used.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delivery_id: Option<String>,
    /// The identity it was addressed to.
    pub to: String,
    pub channel: String,
    pub seq: u64,
    pub from: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub at: Option<String>,
    pub text: String,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub truncated: bool,
    /// Milliseconds since the epoch when this host stored it.
    pub received_at: u64,
}

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Store {
    version: u32,
    /// Highest seq read, keyed `<identity> <channel>`.
    #[serde(default)]
    read: BTreeMap<String, u64>,
    #[serde(default)]
    replies: Vec<Reply>,
}

/// A delivery this profile sent and the channel it landed in.
#[derive(Clone, Debug)]
struct Sent {
    id: String,
    to: String,
    channel: String,
    seq: u64,
    at: u64,
}

fn valid_channel(channel: &str) -> bool {
    let mut bytes = channel.bytes();
    !channel.is_empty()
        && channel.len() <= 200
        && bytes.next().is_some_and(|b| b.is_ascii_alphanumeric())
        && bytes.all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
}

/// Reply text as stored: control characters other than line breaks and tabs
/// removed, secret shapes masked, at most [`REPLY_MAX`] bytes.
pub(super) fn clean(body: &str) -> (String, bool) {
    let text: String = body
        .chars()
        .filter(|c| !c.is_control() || *c == '\n' || *c == '\t')
        .collect();
    let text = crate::agent_panel::redact::text_for_compare(text.trim());
    if text.len() <= REPLY_MAX {
        return (text, false);
    }
    (super::clamp(&text, REPLY_MAX), true)
}

/// `ocs read --json` output: the messages after `since` that someone other
/// than `me` wrote, attributed to the delivery they answer.
fn parse_read(
    text: &str,
    me: &str,
    channel: &str,
    since: u64,
    sent: &[&Sent],
    now: u64,
) -> Result<Vec<Reply>, String> {
    let value: Value =
        serde_json::from_str(text.trim()).map_err(|_| "ocs read returned unreadable JSON")?;
    let messages = value
        .as_array()
        .ok_or("ocs read returned no message list")?;
    let mut out = Vec::new();
    for m in messages {
        let Some(seq) = m.get("seq").and_then(Value::as_u64) else {
            continue;
        };
        let from = m.get("from").and_then(Value::as_str).unwrap_or_default();
        let body = m.get("body").and_then(Value::as_str).unwrap_or_default();
        if seq <= since
            || from.is_empty()
            || from.eq_ignore_ascii_case(me)
            || m.get("self").and_then(Value::as_bool) == Some(true)
            || body.trim().is_empty()
        {
            continue;
        }
        // An explicit reply_to names the batch; otherwise the reply belongs
        // to the latest batch on this channel sent before it.
        let reply_to = m.get("reply_to").and_then(Value::as_u64);
        let answered = reply_to
            .and_then(|r| sent.iter().find(|s| s.seq == r))
            .or_else(|| sent.iter().filter(|s| s.seq < seq).max_by_key(|s| s.seq));
        let (text, truncated) = clean(body);
        out.push(Reply {
            delivery_id: answered.map(|s| s.id.clone()),
            to: me.to_string(),
            channel: channel.to_string(),
            seq,
            from: line(from, 80),
            at: m.get("ts").and_then(Value::as_str).map(|t| line(t, 40)),
            text,
            truncated,
            received_at: now,
        });
    }
    Ok(out)
}

/// `ocs inbox --json`: channels with unread messages for the identity.
fn parse_inbox(text: &str) -> Vec<String> {
    serde_json::from_str::<Value>(text.trim())
        .ok()
        .and_then(|v| v.as_array().cloned())
        .unwrap_or_default()
        .iter()
        .filter_map(|t| t.get("channel").and_then(Value::as_str))
        .filter(|c| valid_channel(c))
        .map(str::to_owned)
        .collect()
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct RepliesParams {
    delivery_id: Option<String>,
}

/// When each data root was last polled in this process.
static POLLED: Mutex<BTreeMap<PathBuf, Instant>> = Mutex::new(BTreeMap::new());

impl Inspect<'_> {
    fn replies_path(&self) -> PathBuf {
        self.root.join("replies.json")
    }

    fn load_replies(&self) -> Store {
        let data = read_regular(&self.replies_path(), MAX_REPLY_STORE)
            .ok()
            .flatten();
        data.and_then(|d| serde_json::from_slice::<Store>(&d).ok())
            .filter(|s| s.version == STORE_VERSION)
            .unwrap_or(Store {
                version: STORE_VERSION,
                ..Store::default()
            })
    }

    /// Deliveries this profile sent with a known channel and seq.
    fn sent(&self, mine: &[String]) -> Vec<Sent> {
        let Ok(directory) = private_dir(&self.root, &["deliveries"]) else {
            return Vec::new();
        };
        let Ok(entries) = fs::read_dir(&directory) else {
            return Vec::new();
        };
        let mut out = Vec::new();
        for entry in entries.flatten() {
            let name = entry.file_name();
            let Some(stem) = name.to_str().and_then(|n| n.strip_suffix(".json")) else {
                continue;
            };
            if delivery_id(stem).ok().as_deref() != Some(stem) {
                continue;
            }
            let Some(record) = read_regular(&entry.path(), MAX_RECORD)
                .ok()
                .flatten()
                .and_then(|d| serde_json::from_slice::<Value>(&d).ok())
            else {
                continue;
            };
            let to = record["as"].as_str().unwrap_or_default();
            let channel = record["channel"].as_str().unwrap_or_default();
            let (Some(seq), Some(at)) = (record["seq"].as_u64(), record["startedAt"].as_u64())
            else {
                continue;
            };
            if record["expired"] == true || !mine.iter().any(|m| m == to) || !valid_channel(channel)
            {
                continue;
            }
            out.push(Sent {
                id: stem.to_string(),
                to: to.to_string(),
                channel: channel.to_string(),
                seq,
                at,
            });
        }
        out
    }

    /// Read new replies from ocs into the reply store. At most once per
    /// [`POLL_INTERVAL`], and only when this profile delivered something in
    /// the last day; otherwise no process is started.
    fn poll(&self, mine: &[String]) -> Result<(), String> {
        let now = super::super::conversation::now();
        let sent = self.sent(mine);
        let recent: Vec<&Sent> = sent
            .iter()
            .filter(|s| now.saturating_sub(s.at) <= POLL_WINDOW)
            .collect();
        if recent.is_empty() {
            return Ok(());
        }
        {
            let mut polled = POLLED.lock().unwrap_or_else(|e| e.into_inner());
            if polled
                .get(&self.root)
                .is_some_and(|t| t.elapsed() < POLL_INTERVAL)
            {
                return Ok(());
            }
            polled.insert(self.root.clone(), Instant::now());
        }
        let Some(ocs) = self.sys.ocs() else {
            return Ok(());
        };
        let store = self.load_replies();
        let identities: BTreeSet<&str> = recent.iter().map(|s| s.to.as_str()).collect();
        let mut found = Vec::new();
        let mut read = BTreeMap::new();
        for me in identities {
            // The channels this identity's recent batches landed in, newest
            // first, then any other channel ocs lists unread for it.
            let mut channels: Vec<String> = Vec::new();
            let mut batches: Vec<&&Sent> = recent.iter().filter(|s| s.to == me).collect();
            batches.sort_by_key(|s| std::cmp::Reverse(s.at));
            for s in batches {
                if !channels.contains(&s.channel) {
                    channels.push(s.channel.clone());
                }
            }
            if let Ok(run) = self
                .sys
                .ocs_json(&ocs.path, &["inbox", "--as", me, "--json"])
            {
                if run.code == Some(0) && !run.timed_out {
                    for c in parse_inbox(&run.stdout) {
                        if !channels.contains(&c) {
                            channels.push(c);
                        }
                    }
                }
            }
            channels.truncate(MAX_CHANNELS);
            for channel in channels {
                let key = format!("{me} {channel}");
                let since = store.read.get(&key).copied().unwrap_or(0);
                let since_text = since.to_string();
                let Ok(run) = self.sys.ocs_json(
                    &ocs.path,
                    &[
                        "read",
                        &channel,
                        "--as",
                        me,
                        "--since",
                        &since_text,
                        "--json",
                    ],
                ) else {
                    continue;
                };
                if run.timed_out || run.code != Some(0) {
                    continue;
                }
                let on_channel: Vec<&Sent> = sent
                    .iter()
                    .filter(|s| s.to == me && s.channel == channel)
                    .collect();
                let Ok(replies) = parse_read(&run.stdout, me, &channel, since, &on_channel, now)
                else {
                    continue;
                };
                let top = replies.iter().map(|r| r.seq).max().unwrap_or(since);
                read.insert(key, top);
                found.extend(replies);
            }
        }
        if found.is_empty() && read.iter().all(|(k, v)| store.read.get(k) == Some(v)) {
            return Ok(());
        }
        self.merge(found, read, now)
    }

    /// Add replies under the reply lock: deduplicated by identity, channel
    /// and seq, high-water marks only rising, old and excess replies dropped.
    fn merge(
        &self,
        found: Vec<Reply>,
        read: BTreeMap<String, u64>,
        now: u64,
    ) -> Result<(), String> {
        self.locked_on("replies.lock", || {
            let mut store = self.load_replies();
            for (key, seq) in read {
                let entry = store.read.entry(key).or_insert(0);
                *entry = (*entry).max(seq);
            }
            for reply in found {
                if !store
                    .replies
                    .iter()
                    .any(|r| r.to == reply.to && r.channel == reply.channel && r.seq == reply.seq)
                {
                    store.replies.push(reply);
                }
            }
            store
                .replies
                .retain(|r| now.saturating_sub(r.received_at) <= REPLY_MAX_AGE);
            store.replies.sort_by_key(|r| r.received_at);
            let excess = store.replies.len().saturating_sub(MAX_REPLIES);
            store.replies.drain(..excess);
            let data = serde_json::to_vec(&store).map_err(|e| e.to_string())?;
            let path = self.replies_path();
            match create_new(&path, &data) {
                Ok(()) => Ok(()),
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                    super::super::config_transaction::atomic_write(&path, &data, None)
                }
                Err(_) => Err("Cannot save Inspect & Ask replies".into()),
            }
        })
    }

    /// `inspectReplies`: this profile's stored replies, newest last, after
    /// reading any new ones. A failed read still returns what is stored.
    pub(super) fn replies(&self, p: RepliesParams) -> Result<Value, String> {
        let wanted = p.delivery_id.as_deref().map(delivery_id).transpose()?;
        let mine = identities(self.profile);
        let warning = self.poll(&mine).err();
        let store = self.load_replies();
        let replies: Vec<&Reply> = store
            .replies
            .iter()
            .filter(|r| mine.contains(&r.to))
            .filter(|r| wanted.is_none() || r.delivery_id == wanted)
            .collect();
        let mut out = json!({ "replies": replies });
        if let Some(warning) = warning {
            out["warning"] = warning.into();
        }
        Ok(out)
    }
}

#[cfg(test)]
fn read_store(root: &Path) -> Value {
    use std::io::Read;
    let mut data = Vec::new();
    fs::File::open(root.join("replies.json"))
        .and_then(|mut f| f.read_to_end(&mut data))
        .unwrap();
    serde_json::from_slice(&data).unwrap()
}

#[cfg(test)]
fn reset_poll_clock(root: &Path) {
    POLLED.lock().unwrap().remove(root);
}

#[cfg(test)]
mod tests {
    use super::super::testkit::*;
    use super::*;
    use regex_lite::Regex;

    /// ocs 0.8.3 `NAME_RE`, the shapes it resolves by prefix
    /// (`resolveDmTarget`, `canonicalWakeAddress`), and the names Claude Code
    /// gives sessions automatically (`<workspace alias>-<2 hex>`).
    #[test]
    fn sender_identities_cannot_be_session_addresses() {
        let name = Regex::new(r"^[A-Za-z0-9][A-Za-z0-9._-]{0,63}$").unwrap();
        let short = Regex::new(r"(?i)^(claude|codex|pi)-[0-9a-f]+").unwrap();
        let uuid =
            Regex::new(r"(?i)^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$")
                .unwrap();
        let hermes = Regex::new(r"(?i)^hermes-").unwrap();
        let automatic = Regex::new(r"^[A-Za-z0-9._-]+-[0-9a-f]{2}$").unwrap();
        let profiles = [
            None,
            Some("Default"),
            Some("Profile 3"),
            Some("a".repeat(80).as_str()),
        ]
        .map(|p| p.map(str::to_owned));
        let mut all = BTreeSet::new();
        for profile in &profiles {
            for id in identities(profile.as_deref()) {
                assert!(name.is_match(&id), "{id} is not an ocs name");
                assert!(!short.is_match(&id), "{id} looks like a short address");
                assert!(!uuid.is_match(&id) && !hermes.is_match(&id), "{id}");
                assert!(
                    !automatic.is_match(&id),
                    "{id} looks like an automatic session name"
                );
                assert!(!id.eq_ignore_ascii_case("agy"));
                // Neither the old sender nor any `chrome-use-<n>` session
                // name is a prefix of it, or it of them.
                for session in [
                    "chrome-use",
                    "chrome-use-91",
                    "chrome-use-inspect",
                    "chrome-use-inspect-reply",
                ] {
                    assert!(
                        !id.starts_with(&format!("{session}-")) && id != session,
                        "{id} vs {session}"
                    );
                    assert!(!session.starts_with(&id));
                }
                assert!(id.starts_with(IDENTITY_PREFIX));
                assert!(all.insert(id), "identities repeat");
            }
        }
        // Stable per profile, different across profiles.
        assert_eq!(identity(Some("Default"), 0), identity(Some("Default"), 0));
        assert_ne!(identity(Some("Default"), 0), identity(Some("Profile 1"), 0));
    }

    #[test]
    fn a_taken_identity_is_skipped() {
        let w = world();
        let first = identity(Some("p"), 0);
        let mut s = session("claude-12345678", &w.project);
        assert_eq!(pick(Some("p"), &[s.clone()], None), Some(first.clone()));
        s.addresses = vec![first.to_uppercase()];
        let second = pick(Some("p"), &[s.clone()], None).unwrap();
        assert_ne!(second, first);
        // A saved ocs name holds it even with no live session.
        let home = w.project.join("ocs");
        fs::create_dir_all(home.join("names")).unwrap();
        fs::write(home.join("names").join(format!("{second}.json")), "{}").unwrap();
        let third = pick(Some("p"), &[s.clone()], Some(&home)).unwrap();
        assert!(third != first && third != second);
        let all: Vec<Session> = identities(Some("p"))
            .into_iter()
            .map(|id| Session {
                addresses: vec![id],
                ..s.clone()
            })
            .collect();
        assert_eq!(pick(Some("p"), &all, None), None);
    }

    fn sent(id: &str, seq: u64) -> Sent {
        Sent {
            id: id.into(),
            to: "me".into(),
            channel: "dm-a".into(),
            seq,
            at: 0,
        }
    }

    #[test]
    fn replies_are_attributed_filtered_redacted_and_capped() {
        let a = sent("a", 3);
        let b = sent("b", 7);
        let on = [&a, &b];
        let long = "x".repeat(REPLY_MAX + 100);
        let text = json!([
            {"seq": 3, "from": "me", "body": "the delivery", "self": true},
            {"seq": 4, "from": "agent-1", "body": "done\u{0007}\nline two"},
            {"seq": 8, "from": "agent-1", "body": "late answer to a", "reply_to": 3},
            {"seq": 9, "from": "agent-1", "body": "token sk-ant-api03-abcdefghijklmnopqrstuvwxyz0123456789ABCD"},
            {"seq": 10, "from": "agent-1", "body": long},
            {"seq": 11, "from": "ME", "body": "echo"},
            {"seq": 2, "from": "agent-1", "body": "old"},
        ])
        .to_string();
        let replies = parse_read(&text, "me", "dm-a", 2, &on, 5).unwrap();
        let seqs: Vec<u64> = replies.iter().map(|r| r.seq).collect();
        assert_eq!(seqs, [4, 8, 9, 10]);
        assert_eq!(replies[0].delivery_id.as_deref(), Some("a"));
        assert_eq!(replies[0].text, "done\nline two");
        assert_eq!(
            replies[1].delivery_id.as_deref(),
            Some("a"),
            "reply_to wins"
        );
        assert_eq!(replies[2].delivery_id.as_deref(), Some("b"));
        assert!(
            !replies[2].text.contains("abcdefghijklmnop"),
            "{}",
            replies[2].text
        );
        assert!(replies[3].truncated && replies[3].text.len() <= REPLY_MAX);
        // Before any batch on the channel: unattributed.
        let early = parse_read(
            r#"[{"seq":1,"from":"x","body":"hi"}]"#,
            "me",
            "dm-a",
            0,
            &on,
            5,
        )
        .unwrap();
        assert_eq!(early[0].delivery_id, None);
        assert!(parse_read("not json", "me", "dm-a", 0, &on, 5).is_err());
    }

    #[test]
    fn inbox_channels_are_validated() {
        let text = r#"[{"channel":"dm-ok--x","unread":1},{"channel":"../etc"},{"channel":""},{}]"#;
        assert_eq!(parse_inbox(text), ["dm-ok--x"]);
        assert!(parse_inbox("garbage").is_empty());
    }

    #[test]
    fn delivery_goes_out_as_the_profile_identity_and_replies_come_back() {
        const ID: &str = "00000000-0000-4000-8000-0000000000aa";
        let w = world();
        let mut live = session("claude-11111111", &w.project);
        // A live session in the chrome-use repo answers to the old sender.
        live.addresses = vec!["chrome-use-91".into(), "chrome-use".into()];
        let sys = Fake {
            installed: true,
            sessions: vec![live],
            ..Fake::default()
        };
        call(
            &w,
            &sys,
            "inspectBind",
            json!({"origin": ORIGIN, "root": w.project, "sessionId": "claude-11111111"}),
        )
        .unwrap();
        // Nothing delivered yet: no ocs process for replies.
        let none = call(&w, &sys, "inspectReplies", json!({})).unwrap();
        assert_eq!(none["replies"], json!([]));
        assert!(sys.ocs_calls.borrow().is_empty());

        let result = call(
            &w,
            &sys,
            "inspectDeliver",
            json!({"origin": ORIGIN, "kind": "edit", "instruction": "Make it blue",
            "pageUrl": "http://localhost:5173/", "env": "local",
            "annotations": [{"label": "x", "selector": "x", "markdown": "x"}], "deliveryId": ID}),
        )
        .unwrap();
        assert_eq!(result["state"], "sent");
        let me = identity(None, 0);
        assert_eq!(*sys.senders.borrow(), std::slice::from_ref(&me));
        assert!(sys.senders.borrow().iter().all(|s| s != "chrome-use"));
        let message = sys.sent.borrow()[0].1.clone();
        assert!(
            message.contains("Your reply is shown to the user in their browser"),
            "{message}"
        );
        assert!(message.len() <= super::super::MESSAGE_BUDGET);
        let record: Value = serde_json::from_slice(
            &fs::read(w.data.join(format!("deliveries/{ID}.json"))).unwrap(),
        )
        .unwrap();
        assert_eq!(record["as"], me);

        let read = format!("read dm-x --as {me} --since 0 --json");
        sys.ocs_answers.borrow_mut().extend([
            (format!("inbox --as {me} --json"), r#"[{"channel":"dm-x","unread":1},{"channel":"dm-other","unread":1}]"#.to_string()),
            (read.clone(), json!([
                {"seq": 7, "from": me, "body": "the delivery", "self": true},
                {"seq": 8, "from": "chrome-use-91", "body": "Done: **blue**", "reply_to": 7, "ts": "2026-10-15T00:00:00Z"},
            ]).to_string()),
            (format!("read dm-other --as {me} --since 0 --json"), json!([{"seq": 1, "from": "codex-1234abcd", "body": "elsewhere"}]).to_string()),
        ]);
        let got = call(&w, &sys, "inspectReplies", json!({"deliveryId": ID})).unwrap();
        let replies = got["replies"].as_array().unwrap();
        assert_eq!(replies.len(), 1, "{got}");
        assert_eq!(replies[0]["deliveryId"], ID);
        assert_eq!(replies[0]["text"], "Done: **blue**");
        assert_eq!(replies[0]["from"], "chrome-use-91");
        let all = call(&w, &sys, "inspectReplies", json!({})).unwrap();
        assert_eq!(all["replies"].as_array().unwrap().len(), 2);
        assert!(all["replies"][1].get("deliveryId").is_none());

        // Within the poll interval nothing runs again; after it, a re-read
        // of the same seqs adds nothing.
        let calls = sys.ocs_calls.borrow().len();
        call(&w, &sys, "inspectReplies", json!({})).unwrap();
        assert_eq!(sys.ocs_calls.borrow().len(), calls);
        assert_eq!(read_store(&w.data)["read"][format!("{me} dm-x")], 8);
        reset_poll_clock(&w.data);
        sys.ocs_answers.borrow_mut().push((
            format!("read dm-x --as {me} --since 8 --json"),
            json!([
                {"seq": 8, "from": "chrome-use-91", "body": "Done: **blue**"},
            ])
            .to_string(),
        ));
        sys.ocs_answers.borrow_mut().push((
            format!("read dm-other --as {me} --since 1 --json"),
            "[]".into(),
        ));
        let again = call(&w, &sys, "inspectReplies", json!({})).unwrap();
        assert_eq!(again["replies"].as_array().unwrap().len(), 2);
        assert!(sys.ocs_calls.borrow().len() > calls);
        // Another profile's identity never sees these replies.
        let other = Inspect {
            root: w.data.clone(),
            sys: &sys,
            profile: Some("Profile 2"),
        }
        .handle("inspectReplies", &json!({}))
        .unwrap();
        assert_eq!(other["replies"], json!([]));
        assert!(call(&w, &sys, "inspectReplies", json!({"deliveryId": "x"})).is_err());
    }

    #[test]
    fn delivery_is_refused_when_every_identity_is_taken() {
        let w = world();
        let mut live = session("claude-11111111", &w.project);
        live.addresses = identities(None);
        let sys = Fake {
            installed: true,
            sessions: vec![live],
            ..Fake::default()
        };
        call(
            &w,
            &sys,
            "inspectBind",
            json!({"origin": ORIGIN, "root": w.project, "sessionId": "claude-11111111"}),
        )
        .unwrap();
        let error = call(&w, &sys, "inspectDeliver", json!({"origin": ORIGIN, "kind": "edit", "instruction": "x",
            "pageUrl": "http://localhost:5173/", "env": "local",
            "annotations": [{"label": "x", "selector": "x", "markdown": "x"}], "deliveryId": "00000000-0000-4000-8000-0000000000ab"})).unwrap_err();
        assert!(error.contains("sender name"), "{error}");
        assert!(sys.sent.borrow().is_empty());
    }
}
