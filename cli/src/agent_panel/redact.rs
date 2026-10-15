//! Privacy minimization for the conversation journal.
//!
//! Every event passes through [`event`] before `Store::event`, which accepts
//! only [`Redacted`]. Rules are schema driven: sensitive keys, headers, URL
//! parameters, `fill`/`type` values for sensitive fields, secret shapes, and
//! values already masked once in this conversation. Identity fields (message,
//! request, tool, item and session ids, cursors) are never masked or
//! scrubbed. Masks keep the field a JSON string, so a panel that calls
//! `String(...)` on it shows the marker text.

use crate::native::sensitive;
use regex_lite::Regex;
use serde::Serialize;
use serde_json::{json, Map, Value};
use std::collections::{HashMap, VecDeque};
use std::sync::OnceLock;

const MAX_KNOWN: usize = 64;
const MIN_SECRET: usize = 8;
const MAX_SECRET: usize = 4096;
/// Upper bound for streamed text held back while a secret may still be
/// arriving.
const MAX_HOLD: usize = 4096;
/// Held back even with no known secret, so a shape such as `Bearer <token>`
/// is seen whole.
const SHAPE_HOLD: usize = 64;
const TOOL_TEXT: usize = 6000;
const EVAL_TEXT: usize = 2000;
const COMMAND_TEXT: usize = 4000;

/// Event data that went through the redaction rules. The field is private to
/// this module, so no write path can hand the store raw data.
pub(super) struct Redacted(Value);

impl Redacted {
    pub(super) fn into_value(self) -> Value {
        self.0
    }
    pub(super) fn value(&self) -> &Value {
        &self.0
    }
    #[cfg(test)]
    pub(super) fn raw_for_tests(value: Value) -> Self {
        Self(value)
    }
}

/// The strongest kind of content an event holds.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) enum PrivacyClass {
    Public,
    PageContent,
    UserInput,
    Masked,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub(super) struct Redaction {
    pub path: String,
    pub kind: String,
    pub length: usize,
}

/// Per-conversation redaction state: values masked so far (scrubbed from
/// later display text) and the streaming scrubbers of open text items.
#[derive(Default)]
pub(super) struct Secrets {
    known: VecDeque<String>,
    streams: HashMap<String, TextScrubber>,
    /// Tool calls whose input reads cookies; their results are cookie text.
    cookie_calls: VecDeque<String>,
}

impl Secrets {
    fn learn(&mut self, value: &str) {
        if value.len() < MIN_SECRET
            || value.len() > MAX_SECRET
            || is_marker(value)
            || self.known.iter().any(|k| k == value)
        {
            return;
        }
        if self.known.len() >= MAX_KNOWN {
            self.known.pop_front();
        }
        self.known.push_back(value.to_string());
    }
    fn known(&self) -> Vec<String> {
        self.known.iter().cloned().collect()
    }
    /// Held streamed text, scrubbed, as final events: a `textDelta` per open
    /// delta item and a last `textFinal` part per unfinished final item. Call
    /// before journaling `terminal`, `exited` or `processUnconfirmed`.
    pub(super) fn flush(&mut self) -> Vec<(&'static str, Redacted)> {
        let known = self.known();
        let mut keys: Vec<_> = self.streams.keys().cloned().collect();
        keys.sort();
        let mut out = Vec::new();
        for key in keys {
            let Some(mut scrubber) = self.streams.remove(&key) else {
                continue;
            };
            let (text, masks) = scrubber.flush(&known);
            for secret in masks.iter().filter_map(|m| m.secret.as_deref()) {
                self.learn(secret);
            }
            let finished = key.starts_with("f:");
            if text.is_empty() && !finished {
                continue;
            }
            let mut data = json!({"itemId":scrubber.item_id,"text":text});
            if !scrubber.message_id.is_null() {
                data["messageId"] = scrubber.message_id.clone();
            }
            if finished {
                data["part"] = scrubber.next_part.into();
                data["last"] = true.into();
            }
            let redactions = mask_records("/text", &masks);
            stamp(&mut data, &redactions, PrivacyClass::Public);
            out.push((
                if finished { "textFinal" } else { "textDelta" },
                Redacted(data),
            ));
        }
        out
    }
}

/// Redact one event for the journal. `None` means a streamed chunk is held
/// back completely for now; nothing is journaled.
pub(super) fn event(kind: &str, mut data: Value, secrets: &mut Secrets) -> Option<Redacted> {
    if matches!(kind, "textDelta" | "textFinal") && data["text"].is_string() {
        return stream(kind, data, secrets);
    }
    let cookie = kind == "tool" && cookie_context(&data, secrets);
    // Pass 1: every rule, with the secrets known from earlier events. It
    // reports the values its own rules masked (a password typed into a
    // field, a header, a cookie). Pass 2 masks those values wherever else
    // this event repeats them ("typed blue river pass"); short values count
    // here too, down to IN_EVENT_MIN; only values of MIN_SECRET or more are
    // kept for later events.
    let known = secrets.known();
    let mut walker = Walker::new(&known, kind);
    walker.cookie = cookie;
    walker.top(&mut data, kind);
    let mut seen: std::collections::HashSet<&str> = known.iter().map(String::as_str).collect();
    let mut found: Vec<String> = Vec::new();
    let mut blanket = false;
    for secret in &walker.learned {
        if secret.len() < IN_EVENT_MIN || is_marker(secret) || !seen.insert(secret.as_str()) {
            continue;
        }
        if found.len() == MAX_EVENT_SECRETS {
            blanket = true;
            break;
        }
        found.push(secret.clone());
    }
    if blanket || !found.is_empty() {
        found.sort_by_key(|k| std::cmp::Reverse(k.len()));
        let exempt: &[&str] = match kind {
            "user" | "queueEdited" => &["text"],
            _ => &[],
        };
        let mut echo = Echoes {
            secrets: &found,
            numbers: found.iter().filter_map(|k| canonical_number(k)).collect(),
            blanket,
            redactions: &mut walker.redactions,
        };
        if let Some(object) = data.as_object_mut() {
            for (key, value) in object.iter_mut() {
                if !exempt.contains(&key.as_str()) && !kept_field(key) {
                    echo.walk(value, format!("/{}", pointer(key)));
                }
            }
        }
    }
    // Redacted whole above; only now bounded for the journal.
    if kind == "tool" && !data["item"].is_null() {
        data["item"] = bounded(&data["item"]);
    }
    for secret in std::mem::take(&mut walker.learned) {
        secrets.learn(&secret);
    }
    let redactions = std::mem::take(&mut walker.redactions);
    stamp(&mut data, &redactions, walker.class);
    fit(kind, &mut data);
    Some(Redacted(data))
}

/// Make the event fit one journal line, whatever it held: the item is cut
/// further, then reduced to a stub naming the call; the redaction records
/// go to a count. A write can then never fail for size.
fn fit(kind: &str, data: &mut Value) {
    let size = |data: &Value| data.to_string().len();
    let mut budget = ITEM_BUDGET;
    while kind == "tool" && size(data) > EVENT_BUDGET && budget > 1024 {
        budget /= 2;
        data["item"] = bounded_to(&data["item"], budget);
    }
    if size(data) <= EVENT_BUDGET {
        return;
    }
    if let Some(object) = data.as_object_mut() {
        if let Some(Value::Array(list)) = object.remove("redactions") {
            let omitted = object
                .get("redactionsOmitted")
                .and_then(Value::as_u64)
                .unwrap_or(0);
            object.insert(
                "redactionsOmitted".into(),
                (omitted + list.len() as u64).into(),
            );
        }
    }
    if size(data) <= EVENT_BUDGET {
        return;
    }
    let stub = |value: &Value| {
        let mut stub = serde_json::Map::new();
        for key in [
            "type",
            "name",
            "tool",
            "toolName",
            "tool_name",
            "id",
            "tool_use_id",
            "toolCallId",
        ] {
            if let Some(field) = value.get(key).filter(|f| f.to_string().len() <= 1024) {
                stub.insert(key.into(), field.clone());
            }
        }
        stub.insert("note".into(), "too large to record".into());
        Value::Object(stub)
    };
    match kind {
        "tool" => data["item"] = stub(&data["item"]),
        "request" => data["params"] = stub(&data["params"]),
        _ => {}
    }
}

/// Pass 2 of [`event`]: mask the values pass 1 found wherever else the
/// event repeats them, or withhold every free-text string when the event
/// revealed more secrets than are scanned for one by one.
struct Echoes<'a> {
    secrets: &'a [String],
    numbers: std::collections::HashSet<String>,
    blanket: bool,
    redactions: &'a mut Vec<Redaction>,
}

impl Echoes<'_> {
    fn walk(&mut self, value: &mut Value, path: String) {
        match value {
            Value::String(text) if !is_marker(text) && !text.is_empty() => {
                if self.blanket {
                    let length = text.chars().count();
                    *text = marker("withheld", length);
                    self.record(path, "withheld", length);
                    return;
                }
                let masks = known_masks(text, self.secrets);
                if masks.is_empty() {
                    return;
                }
                let mut masks = masks;
                masks.sort_by(|a, b| a.start.cmp(&b.start).then(b.end.cmp(&a.end)));
                let mut kept: Vec<Mask> = Vec::new();
                for mask in masks {
                    if kept.last().is_none_or(|last| mask.start >= last.end) {
                        kept.push(mask);
                    }
                }
                let (out, applied) = apply(text, &kept);
                for mask in &applied {
                    self.record(path.clone(), "secret", mask.length);
                }
                *text = out;
            }
            Value::Number(n)
                if self.blanket
                    || canonical_number(&n.to_string())
                        .is_some_and(|c| self.numbers.contains(&c)) =>
            {
                let length = n.to_string().len();
                let kind = if self.blanket { "withheld" } else { "secret" };
                *value = marker(kind, length).into();
                self.record(path, kind, length);
            }
            Value::Array(items) => {
                for (i, item) in items.iter_mut().enumerate() {
                    self.walk(item, child_path(&path, &i.to_string()));
                }
            }
            Value::Object(object) => {
                for (key, child) in object.iter_mut() {
                    if !kept_field(key) {
                        self.walk(child, child_path(&path, key));
                    }
                }
            }
            _ => {}
        }
    }
    fn record(&mut self, path: String, kind: &str, length: usize) {
        self.redactions.push(Redaction {
            path,
            kind: kind.into(),
            length,
        });
    }
}

/// Fields the panel and the service dispatch on. The echo pass leaves them
/// as they are, also when it withholds everything else.
const PROTOCOL_KEYS: &[&str] = &[
    "method",
    "type",
    "kind",
    "name",
    "tool",
    "tool_name",
    "toolName",
    "server",
    "serverName",
    "subtype",
    "role",
    "status",
    "phase",
    "outcome",
    "optionId",
    "mode",
    "codex_approval_kind",
    "multiSelect",
    "isOther",
    "completed",
];

fn kept_field(key: &str) -> bool {
    ID_KEYS.contains(&key) || PROTOCOL_KEYS.contains(&key)
}

/// One spelling per number, so 483927, 483927.0 and "483927" compare equal.
fn canonical_number(text: &str) -> Option<String> {
    let value: f64 = text.trim().parse().ok()?;
    if !value.is_finite() {
        return None;
    }
    Some(if value.fract() == 0.0 && value.abs() < 1e15 {
        format!("{}", value as i64)
    } else {
        format!("{value}")
    })
}

/// A child's JSON pointer, kept short: past MAX_PATH the parent's path is
/// reused, so a deep or long-keyed tree costs no more per node.
fn child_path(path: &str, key: &str) -> String {
    if path.len() >= MAX_PATH {
        return path.to_string();
    }
    let key: String = pointer(key).chars().take(64).collect();
    format!("{path}/{key}")
}

/// Shortest value of this event's own masked secrets that is also masked
/// where the event repeats it.
const IN_EVENT_MIN: usize = 4;
/// Most redaction records kept on one event; the rest are counted.
const MAX_REDACTIONS: usize = 64;
/// A redaction record's path is cut to this many characters.
const MAX_PATH: usize = 200;
/// Distinct new secrets one event may reveal and still be scanned for one
/// by one; past this the event's free text is withheld.
const MAX_EVENT_SECRETS: usize = 256;
/// Under the journal's 64 KiB line limit, with room for the envelope.
const EVENT_BUDGET: usize = 60 * 1024;

/// Whether a tool event carries cookie text: the call's own input reads
/// `document.cookie` (or the Cookie Store API), or it is the result of such a
/// call seen earlier (Claude sends the result as a separate block). There a
/// lone `name=value` is a cookie, not ordinary text.
fn cookie_context(data: &Value, secrets: &mut Secrets) -> bool {
    let item = &data["item"];
    let reads_cookie = ["input", "arguments", "rawInput", "args"]
        .iter()
        .filter_map(|k| item.get(*k))
        .any(|args| {
            let text = args.to_string();
            text.contains("document.cookie") || text.contains("cookieStore")
        });
    let call = data["toolCallId"]
        .as_str()
        .or_else(|| item["id"].as_str())
        .or_else(|| item["tool_use_id"].as_str())
        .or_else(|| item["toolCallId"].as_str())
        .map(String::from);
    if reads_cookie {
        if let Some(call) = &call {
            if !secrets.cookie_calls.contains(call) {
                if secrets.cookie_calls.len() >= 256 {
                    secrets.cookie_calls.pop_front();
                }
                secrets.cookie_calls.push_back(call.clone());
            }
        }
        return true;
    }
    call.is_some_and(|call| secrets.cookie_calls.contains(&call))
}

/// Every `name=value` of cookie text, values masked whatever their length.
fn cookie_masks(text: &str) -> Vec<Mask> {
    let mut masks = Vec::new();
    for c in patterns().cookie_pair.captures_iter(text) {
        let (Some(name), Some(value)) = (c.get(1), c.get(2)) else {
            continue;
        };
        if is_marker(value.as_str()) {
            continue;
        }
        masks.push(Mask::value(text, value.start(), value.end(), "cookie").anchored(name.start()));
    }
    masks
}

/// Display minimization for events written before this build. Applied by
/// `conversationRead`; the journal file is never rewritten.
pub(super) fn legacy(kind: &str, data: &mut Value) {
    if data.get("privacyClass").is_some() || data.get("v").is_some() {
        return;
    }
    let mut walker = Walker::new(&[], kind);
    walker.top(data, kind);
}

/// The page URL as it is stored and shown: no userinfo, sensitive query and
/// fragment values masked. Idempotent.
pub(super) fn url(text: &str) -> String {
    let masks = url_masks(text, 0);
    apply(text, &masks).0
}

/// The size one tool item or request may keep in the journal.
pub(super) const ITEM_BUDGET: usize = 24000;

/// Bound an already redacted tool item for the journal without losing its
/// structure: long strings are cut and long arrays shortened. Redaction runs
/// on the whole item first, so cutting can never hide a sensitive field.
pub(super) fn bounded(value: &Value) -> Value {
    bounded_to(value, ITEM_BUDGET)
}

fn bounded_to(value: &Value, budget: usize) -> Value {
    if value.to_string().len() <= budget {
        return value.clone();
    }
    for cap in [6000, 2000, 500, 100] {
        let cut = cut_to(value, cap, 0);
        if cut.to_string().len() <= budget {
            return cut;
        }
    }
    // Too wide or deep even then: keep what identifies the call and its
    // arguments, each bounded, and say the rest was dropped.
    let mut skeleton = serde_json::Map::new();
    if let Some(object) = value.as_object() {
        for key in [
            "type",
            "id",
            "name",
            "tool",
            "server",
            "toolName",
            "tool_name",
            "title",
            "toolCallId",
            "tool_use_id",
            "status",
            "is_error",
            "isError",
        ] {
            if let Some(field) = object.get(key) {
                skeleton.insert(key.into(), cut_to(field, 200, 18));
            }
        }
        for key in ["input", "arguments", "rawInput", "args"] {
            if let Some(field) = object.get(key) {
                let args = cut_to(field, 200, 16);
                if args.to_string().len() <= budget / 2 {
                    skeleton.insert(key.into(), args);
                }
            }
        }
    }
    skeleton.insert(
        "output".into(),
        "[The tool output was too large to keep]".into(),
    );
    Value::Object(skeleton)
}

fn cut_to(value: &Value, cap: usize, depth: usize) -> Value {
    match value {
        Value::String(text) if text.chars().count() > cap => {
            let mut short: String = text.chars().take(cap).collect();
            short.push_str("…[shortened]");
            Value::String(short)
        }
        Value::Array(items) => {
            let keep = (cap / 100).clamp(5, 50);
            let mut out: Vec<Value> = items
                .iter()
                .take(keep)
                .map(|item| cut_to(item, cap, depth + 1))
                .collect();
            if items.len() > keep {
                out.push(format!("[{} more items]", items.len() - keep).into());
            }
            Value::Array(out)
        }
        Value::Object(map) if depth >= 20 => {
            let _ = map;
            "[nested data shortened]".into()
        }
        Value::Object(map) => Value::Object(
            map.iter()
                .map(|(k, v)| (k.clone(), cut_to(v, cap, depth + 1)))
                .collect(),
        ),
        other => other.clone(),
    }
}

/// A redacted request whose params are too large to review: they become a
/// bounded preview of their redacted text, marked `truncated`.
pub(super) fn preview_params(mut request: Redacted) -> Redacted {
    let preview: String = request.0["params"]
        .to_string()
        .chars()
        .take(TOOL_TEXT)
        .collect();
    request.0["params"] = json!({"preview": preview, "truncated": true});
    request
}

/// Text kept outside the journal (conversation title, last error, schedule
/// results) as the rules and this conversation's known secrets redact it.
pub(super) fn display(text: &str, secrets: &Secrets) -> String {
    let known = secrets.known();
    apply(text, &find_masks(text, &known, false)).0
}

/// Free text as the rules alone redact it, without known secrets: the form a
/// predicate is compared in.
pub(super) fn text_for_compare(text: &str) -> String {
    let masks = find_masks(text, &[], false);
    apply(text, &masks).0
}

/// Strings that are already a mask produced here. Never masked again, so
/// redaction is a fixed point.
fn is_marker(text: &str) -> bool {
    text.starts_with("[redacted")
}

fn marker(kind: &str, length: usize) -> String {
    format!("[redacted {kind}, {length} chars]")
}

fn stamp(data: &mut Value, redactions: &[Redaction], class: PrivacyClass) {
    let Some(object) = data.as_object_mut() else {
        return;
    };
    let class = if redactions.is_empty() {
        class
    } else {
        PrivacyClass::Masked
    };
    object.insert("privacyClass".into(), json!(class));
    if !redactions.is_empty() {
        let kept: Vec<Redaction> = redactions[..redactions.len().min(MAX_REDACTIONS)]
            .iter()
            .map(|r| Redaction {
                path: if r.path.chars().count() > MAX_PATH {
                    let mut p: String = r.path.chars().take(MAX_PATH).collect();
                    p.push('…');
                    p
                } else {
                    r.path.clone()
                },
                kind: r.kind.chars().take(64).collect(),
                length: r.length,
            })
            .collect();
        object.insert("redactions".into(), json!(kept));
        if redactions.len() > kept.len() {
            object.insert(
                "redactionsOmitted".into(),
                (redactions.len() - kept.len()).into(),
            );
        }
    }
}

fn stream(kind: &str, mut data: Value, secrets: &mut Secrets) -> Option<Redacted> {
    let item = data["itemId"].as_str().unwrap_or("assistant").to_string();
    let chunk = data["text"].as_str().unwrap_or("").to_string();
    let known = secrets.known();
    let finished = kind == "textFinal";
    let key = format!("{}:{item}", if finished { "f" } else { "d" });
    if finished && data["part"] == 0 {
        // A completed text replaces what was streamed for the item.
        secrets.streams.remove(&format!("d:{item}"));
        secrets.streams.remove(&key);
    }
    let scrubber = secrets.streams.entry(key.clone()).or_default();
    scrubber.item_id = item;
    if let Some(id) = data.get("messageId") {
        scrubber.message_id = id.clone();
    }
    let (mut text, mut masks) = scrubber.push(&chunk, &known);
    if finished {
        scrubber.next_part = data["part"].as_u64().unwrap_or(0) + 1;
        if data["last"] == true {
            let (tail, tail_masks) = scrubber.flush(&known);
            let offset = text.len();
            text.push_str(&tail);
            masks.extend(tail_masks.into_iter().map(|mut m| {
                m.start += offset;
                m.end += offset;
                m
            }));
            secrets.streams.remove(&key);
        }
    }
    for secret in masks.iter().filter_map(|m| m.secret.as_deref()) {
        secrets.learn(secret);
    }
    if text.is_empty() && !finished {
        return None;
    }
    data["text"] = text.into();
    let redactions = mask_records("/text", &masks);
    stamp(&mut data, &redactions, PrivacyClass::Public);
    Some(Redacted(data))
}

fn mask_records(path: &str, masks: &[Mask]) -> Vec<Redaction> {
    masks
        .iter()
        .map(|m| Redaction {
            path: path.into(),
            kind: m.kind.clone(),
            length: m.length,
        })
        .collect()
}

/// Streaming scrubber for one text item. It holds back the tail of the text
/// that could still be the start of a secret: the longest known secret minus
/// one byte, a trailing unbroken token, or an open header, cookie or key
/// block, never more than [`MAX_HOLD`] bytes. A shape longer than that is
/// masked through to its end, and the rest of its token in later chunks is
/// masked too.
#[derive(Default)]
pub(super) struct TextScrubber {
    pending: String,
    run: bool,
    /// Values this item's own text revealed to a rule (`"password":"…"`),
    /// masked wherever the item repeats them, short ones included.
    local: Vec<String>,
    message_id: Value,
    item_id: String,
    next_part: u64,
}

impl TextScrubber {
    #[cfg(test)]
    fn held(&self) -> usize {
        self.pending.len()
    }
    /// Known secrets plus the ones this item revealed itself.
    fn merged(&self, known: &[String]) -> Vec<String> {
        let mut merged = known.to_vec();
        merged.extend(self.local.iter().cloned());
        merged
    }
    /// Values the rules masked in this text that are not known yet: kept
    /// for the rest of the item and returned for a rescan.
    fn adopt(&mut self, masks: &[Mask], merged: &[String], complete: usize) -> Vec<String> {
        let mut new = Vec::new();
        // A match running to the end of the buffer may still grow: its value
        // is adopted once the text after it arrives.
        for secret in masks
            .iter()
            .filter(|m| m.end < complete)
            .filter_map(|m| m.secret.as_ref())
        {
            if secret.len() >= IN_EVENT_MIN
                && !is_marker(secret)
                && !merged.contains(secret)
                && !new.contains(secret)
                && self.local.len() < MAX_EVENT_SECRETS
            {
                new.push(secret.clone());
                self.local.push(secret.clone());
            }
        }
        new
    }
    fn push(&mut self, chunk: &str, known: &[String]) -> (String, Vec<Mask>) {
        let merged = self.merged(known);
        let known = merged.as_slice();
        let mut out = String::new();
        let mut out_masks = Vec::new();
        let mut chunk = chunk;
        if self.run {
            let end = chunk.find(char::is_whitespace).unwrap_or(chunk.len());
            if end > 0 {
                let length = chunk[..end].chars().count();
                out = marker("secret", length);
                out_masks.push(Mask {
                    anchor: 0,
                    start: 0,
                    end: out.len(),
                    replacement: String::new(),
                    kind: "secret".into(),
                    length,
                    secret: None,
                });
            }
            if end < chunk.len() {
                self.run = false;
            }
            chunk = &chunk[end..];
        }
        let mut buffer = std::mem::take(&mut self.pending);
        buffer.push_str(chunk);
        let mut masks = find_masks(&buffer, known, false);
        let new = self.adopt(&masks, known, buffer.len());
        if !new.is_empty() {
            // Find the echoes too, so no cut falls inside one.
            let all = self.merged(known);
            masks = find_masks(&buffer, &all, false);
        }
        let len = buffer.len();
        let longest = known.iter().chain(&new).map(String::len).max().unwrap_or(0);
        let hold = longest.saturating_sub(1).clamp(SHAPE_HOLD, MAX_HOLD);
        let mut cut = len.saturating_sub(hold);
        let token = buffer
            .char_indices()
            .rev()
            .find(|(_, c)| c.is_whitespace())
            .map(|(i, c)| i + c.len_utf8())
            .unwrap_or(0);
        if len - token <= MAX_HOLD {
            cut = cut.min(token);
        }
        if let Some(open) = open_tail(&buffer) {
            if len - open <= MAX_HOLD {
                cut = cut.min(open);
            }
        }
        // Never cut inside a match or between its context and its value.
        let mut changed = true;
        let mut rounds = 0;
        while changed && rounds <= masks.len() {
            changed = false;
            rounds += 1;
            for mask in &masks {
                if mask.anchor < cut && mask.end > cut {
                    if len - mask.anchor <= MAX_HOLD {
                        cut = mask.anchor;
                    } else {
                        cut = mask.end;
                        if mask.end == len && mask.kind != "secret" {
                            self.run = true;
                        }
                    }
                    changed = true;
                }
            }
        }
        while !buffer.is_char_boundary(cut) {
            cut -= 1;
        }
        let ready: Vec<Mask> = masks.into_iter().filter(|m| m.end <= cut).collect();
        let (text, applied) = apply(&buffer[..cut], &ready);
        let offset = out.len();
        out.push_str(&text);
        out_masks.extend(applied.into_iter().map(|mut m| {
            m.start += offset;
            m.end += offset;
            m
        }));
        self.pending = buffer[cut..].to_string();
        rescan(&mut out, &new, &mut out_masks);
        (out, out_masks)
    }
    fn flush(&mut self, known: &[String]) -> (String, Vec<Mask>) {
        let merged = self.merged(known);
        let buffer = std::mem::take(&mut self.pending);
        self.run = false;
        let masks = find_masks(&buffer, &merged, false);
        let new = self.adopt(&masks, &merged, usize::MAX);
        let (mut out, mut applied) = apply(&buffer, &masks);
        rescan(&mut out, &new, &mut applied);
        (out, applied)
    }
}

/// Mask the echoes of secrets a text revealed itself, in text already
/// masked by the rules.
fn rescan(text: &mut String, new: &[String], masks: &mut Vec<Mask>) {
    if new.is_empty() {
        return;
    }
    let mut echoes = known_masks(text, new);
    echoes.sort_by(|a, b| a.start.cmp(&b.start).then(b.end.cmp(&a.end)));
    let mut kept: Vec<Mask> = Vec::new();
    for mask in echoes {
        if kept.last().is_none_or(|last| mask.start >= last.end) {
            kept.push(mask);
        }
    }
    if kept.is_empty() {
        return;
    }
    let (out, applied) = apply(text, &kept);
    *text = out;
    masks.extend(applied);
}

/// Start of an unfinished construct at the end of streamed text whose secret
/// part may continue in the next chunk.
fn open_tail(text: &str) -> Option<usize> {
    let mut start = None;
    if let Some(begin) = text.rfind("-----BEGIN") {
        let rest = &text[begin..];
        if rest.contains("PRIVATE KEY") && !rest.contains("-----END") {
            start = Some(begin);
        }
    }
    if let Some(m) = patterns().open_line.find(text) {
        start = Some(start.map_or(m.start(), |s: usize| s.min(m.start())));
    }
    start
}

struct Patterns {
    pem: Regex,
    url: Regex,
    json_key: Regex,
    header: Regex,
    cookie_header: Regex,
    cookie_run: Regex,
    cookie_pair: Regex,
    bearer: Regex,
    shapes: Vec<(&'static str, Regex)>,
    binary: Regex,
    open_line: Regex,
}

fn patterns() -> &'static Patterns {
    static PATTERNS: OnceLock<Patterns> = OnceLock::new();
    PATTERNS.get_or_init(|| {
        let r = |p: &str| Regex::new(p).expect("redaction pattern");
        Patterns {
            pem: r(r"(?s)-----BEGIN [A-Z0-9 ]*PRIVATE KEY-----.*?-----END [A-Z0-9 ]*PRIVATE KEY-----"),
            url: r(r#"(?i)\b(?:https?|wss?|ftp)://[^\s"'<>\\\])]+"#),
            json_key: r(r#"(?i)\\?"(password|passwd|pwd|secret|token|api_?key|authorization|proxy-authorization|cookie|set-cookie|otp|cvv|cvc|access_?token|refresh_?token|id_?token|client_?secret|x-api-key|x-auth-token|x-csrf-token)\\?"\s*:\s*\\?"([^"\\]+)"#),
            header: r(r"(?im)^[ \t]*(authorization|proxy-authorization|x-api-key|x-auth-token|x-csrf-token)[ \t]*:[ \t]*([^\r\n]+)"),
            cookie_header: r(r"(?im)^[ \t]*(set-cookie|cookie)[ \t]*:[ \t]*([^\r\n]+)"),
            cookie_pair: r(r#"([A-Za-z_][A-Za-z0-9_.\-]*)=([^;\s"'&,\\]+)"#),
            cookie_run: r(r#"[A-Za-z0-9_.\-]+=[^;\s"'&]*(?:;[ \t]*[A-Za-z0-9_.\-]+=[^;\s"'&]*)+"#),
            bearer: r(r"(?i)\bbearer[ \t]+([A-Za-z0-9._~+/=\-]{8,})"),
            shapes: vec![
                ("jwt", r(r"\beyJ[A-Za-z0-9_\-]{8,}\.[A-Za-z0-9_\-]{8,}\.[A-Za-z0-9_\-]{8,}")),
                ("api key", r(r"\bsk-[A-Za-z0-9_\-]{16,}")),
                ("github token", r(r"\bgh[pousr]_[A-Za-z0-9]{20,}")),
                ("slack token", r(r"\bxox[abpr]-[A-Za-z0-9\-]{10,}")),
                ("aws key", r(r"\bAKIA[A-Z0-9]{16}\b")),
            ],
            binary: r(r"[A-Za-z0-9+/]{512,}={0,2}"),
            open_line: r(r#"(?im)(?:^[ \t]*(?:authorization|proxy-authorization|x-api-key|x-auth-token|x-csrf-token|set-cookie|cookie)[ \t]*:|\bbearer\b|[A-Za-z0-9_.\-]+=[^;\s"'&]*(?:;[ \t]*[A-Za-z0-9_.\-]+=[^;\s"'&]*)*;)[^\n]*\z"#),
        }
    })
}

/// Lowercased query and fragment parameter names whose values are masked.
const URL_KEYS: &[&str] = &[
    "token",
    "access_token",
    "id_token",
    "refresh_token",
    "code",
    "key",
    "api_key",
    "apikey",
    "sig",
    "signature",
    "secret",
    "password",
    "session",
    "sessionid",
    "sid",
    "auth",
    "jwt",
    "otp",
    "ticket",
    "state",
    "x-amz-signature",
    "x-amz-credential",
    "x-amz-security-token",
    "x-goog-signature",
];

/// Argument and JSON keys (lowercased, `-` and `_` removed) whose values are
/// masked wherever they appear. Header names are included.
const SECRET_KEYS: &[&str] = &[
    "password",
    "passwd",
    "pwd",
    "secret",
    "token",
    "apikey",
    "authorization",
    "proxyauthorization",
    "cookie",
    "setcookie",
    "otp",
    "cvv",
    "cvc",
    "xapikey",
    "xauthtoken",
    "xcsrftoken",
    "accesstoken",
    "refreshtoken",
    "idtoken",
    "clientsecret",
];

/// Keys that carry identity or protocol state. Their values are never
/// masked or scrubbed, so links between events survive any secret.
const ID_KEYS: &[&str] = &[
    "id",
    "itemId",
    "item_id",
    "tool_use_id",
    "toolUseId",
    "toolCallId",
    "tool_call_id",
    "callId",
    "call_id",
    "requestId",
    "request_id",
    "messageId",
    "replacementId",
    "sessionId",
    "session_id",
    "actionId",
    "actionIds",
    "artifactId",
    "threadId",
    "thread_id",
    "turnId",
    "parent_tool_use_id",
    "uuid",
    "sequence",
    "cursor",
    "generation",
    "requestRevision",
    "skillId",
    "digest",
    "privacyClass",
    "redactions",
];

fn secret_key(key: &str) -> Option<String> {
    let normalized: String = key
        .chars()
        .filter(|c| *c != '-' && *c != '_')
        .flat_map(char::to_lowercase)
        .collect();
    SECRET_KEYS
        .contains(&normalized.as_str())
        .then(|| key.to_ascii_lowercase())
}

#[derive(Clone, Debug)]
struct Mask {
    /// Start of the whole match, including context such as `Bearer ` or a
    /// header name. Streamed text is never cut between it and `end`.
    anchor: usize,
    start: usize,
    end: usize,
    replacement: String,
    kind: String,
    length: usize,
    /// The raw value, remembered as a known secret.
    secret: Option<String>,
}

impl Mask {
    fn value(text: &str, start: usize, end: usize, kind: &str) -> Self {
        let raw = &text[start..end];
        let length = raw.chars().count();
        Self {
            anchor: start,
            start,
            end,
            replacement: marker(kind, length),
            kind: kind.into(),
            length,
            secret: Some(raw.to_string()),
        }
    }
    fn anchored(mut self, anchor: usize) -> Self {
        self.anchor = anchor;
        self
    }
}

/// URL masks for every `scheme://` URL starting at `offset`: userinfo is
/// removed, sensitive query and fragment values become `[redacted]`.
fn url_masks(url: &str, offset: usize) -> Vec<Mask> {
    let mut masks = Vec::new();
    let Some(scheme) = url.find("://") else {
        return masks;
    };
    let authority_start = scheme + 3;
    let rest = &url[authority_start..];
    let authority_end = authority_start + rest.find(['/', '?', '#']).unwrap_or(rest.len());
    if let Some(at) = url[authority_start..authority_end].rfind('@') {
        let end = authority_start + at + 1;
        masks.push(Mask {
            anchor: offset,
            start: offset + authority_start,
            end: offset + end,
            replacement: String::new(),
            kind: "url userinfo".into(),
            length: url[authority_start..end - 1].chars().count(),
            secret: url[authority_start..end - 1]
                .split_once(':')
                .map(|(_, password)| password.to_string()),
        });
    }
    let fragment = url[authority_end..].find('#').map(|i| authority_end + i);
    let query = url[authority_end..fragment.unwrap_or(url.len())]
        .find('?')
        .map(|i| authority_end + i);
    let mut parts = Vec::new();
    if let Some(q) = query {
        parts.push((q + 1, fragment.unwrap_or(url.len())));
    }
    if let Some(f) = fragment {
        parts.push((f + 1, url.len()));
    }
    for (start, end) in parts {
        let mut position = start;
        for pair in url[start..end].split('&') {
            let pair_start = position;
            position += pair.len() + 1;
            let Some(eq) = pair.find('=') else { continue };
            let name = urlencoding::decode(&pair[..eq])
                .map(|n| n.to_ascii_lowercase())
                .unwrap_or_else(|_| pair[..eq].to_ascii_lowercase());
            let value = &pair[eq + 1..];
            if value.is_empty() || is_marker(value) || !URL_KEYS.contains(&name.as_str()) {
                continue;
            }
            let value_start = pair_start + eq + 1;
            masks.push(Mask {
                anchor: offset,
                start: offset + value_start,
                end: offset + value_start + value.len(),
                replacement: "[redacted]".into(),
                kind: format!("url {name}"),
                length: value.chars().count(),
                secret: Some(
                    urlencoding::decode(value)
                        .map(|v| v.into_owned())
                        .unwrap_or_else(|_| value.to_string()),
                ),
            });
        }
    }
    masks
}

/// Every mask the free-text rules find in `text`, sorted and
/// non-overlapping.
fn find_masks(text: &str, known: &[String], tool_text: bool) -> Vec<Mask> {
    let p = patterns();
    let mut masks = Vec::new();
    // Cheap guards first: most strings are words, and every rule below
    // needs a marker character or prefix to match at all.
    let has = |needle: &str| text.contains(needle);
    if has("-----BEGIN") {
        for m in p.pem.find_iter(text) {
            masks.push(Mask::value(text, m.start(), m.end(), "private key"));
        }
    }
    if has("://") {
        for m in p.url.find_iter(text) {
            masks.extend(url_masks(m.as_str(), m.start()));
        }
    }
    let keyed: &[(&Regex, usize)] = if has(":") {
        &[(&p.json_key, 2), (&p.header, 2), (&p.cookie_header, 2)]
    } else {
        &[]
    };
    for (regex, group) in keyed.iter().copied() {
        for c in regex.captures_iter(text) {
            let (Some(key), Some(value)) = (c.get(1), c.get(group)) else {
                continue;
            };
            if is_marker(value.as_str()) {
                continue;
            }
            let kind = key.as_str().to_ascii_lowercase();
            let anchor = c.get(0).map_or(value.start(), |m| m.start());
            masks.push(Mask::value(text, value.start(), value.end(), &kind).anchored(anchor));
        }
    }
    let runs = if has("=") && has(";") {
        p.cookie_run.find_iter(text).collect()
    } else {
        Vec::new()
    };
    for run in runs {
        let mut position = run.start();
        for pair in run.as_str().split(';') {
            let pair_start = position;
            position += pair.len() + 1;
            let Some(eq) = pair.find('=') else { continue };
            let value = &pair[eq + 1..];
            if value.len() >= 8 && !is_marker(value) {
                let start = pair_start + eq + 1;
                masks.push(
                    Mask::value(text, start, start + value.len(), "cookie").anchored(run.start()),
                );
            }
        }
    }
    let bearer = text
        .as_bytes()
        .windows(6)
        .any(|w| w.eq_ignore_ascii_case(b"bearer"));
    let bearers = if bearer {
        p.bearer.captures_iter(text).collect()
    } else {
        Vec::new()
    };
    for c in bearers {
        // A token, not a word: "Bearer authentication" is prose.
        if let Some(token) = c
            .get(1)
            .filter(|t| t.as_str().chars().any(|c| !c.is_ascii_alphabetic()))
        {
            let anchor = c.get(0).map_or(token.start(), |m| m.start());
            masks.push(
                Mask::value(text, token.start(), token.end(), "bearer token").anchored(anchor),
            );
        }
    }
    for (kind, regex) in &p.shapes {
        let needle = match *kind {
            "jwt" => "eyJ",
            "api key" => "sk-",
            "github token" => "_",
            "slack token" => "xox",
            _ => "AKIA",
        };
        if text.len() < 16 || !has(needle) {
            continue;
        }
        for m in regex.find_iter(text) {
            masks.push(Mask::value(text, m.start(), m.end(), kind));
        }
    }
    if tool_text && text.len() >= 512 {
        for m in p.binary.find_iter(text) {
            let mut mask = Mask::value(text, m.start(), m.end(), "binary data");
            mask.secret = None;
            masks.push(mask);
        }
    }
    masks.extend(known_masks(text, known));
    masks.sort_by(|a, b| a.start.cmp(&b.start).then(b.end.cmp(&a.end)));
    let mut kept: Vec<Mask> = Vec::new();
    for mask in masks {
        if kept.last().is_none_or(|last| mask.start >= last.end) {
            kept.push(mask);
        }
    }
    kept
}

/// Every occurrence of a known secret (and its JSON-escaped spelling).
fn known_masks(text: &str, known: &[String]) -> Vec<Mask> {
    let mut masks = Vec::new();
    for secret in known {
        if secret.len() > text.len() {
            continue;
        }
        let needs_escape = secret.bytes().any(|b| b == b'"' || b == b'\\' || b < 0x20);
        let escaped = if needs_escape {
            serde_json::to_string(secret).unwrap_or_default()
        } else {
            String::new()
        };
        let escaped = escaped.trim_matches('"');
        for form in [secret.as_str(), escaped] {
            if form.is_empty() || form.len() > text.len() {
                continue;
            }
            for (start, _) in text.match_indices(form) {
                let mut mask = Mask::value(text, start, start + form.len(), "secret");
                mask.secret = None;
                masks.push(mask);
            }
        }
    }
    masks
}

/// Apply sorted, non-overlapping masks. Returns the text and the masks with
/// positions in the output.
fn apply(text: &str, masks: &[Mask]) -> (String, Vec<Mask>) {
    let mut out = String::with_capacity(text.len());
    let mut applied = Vec::with_capacity(masks.len());
    let mut position = 0;
    for mask in masks {
        out.push_str(&text[position..mask.start]);
        let start = out.len();
        out.push_str(&mask.replacement);
        let mut m = mask.clone();
        m.start = start;
        m.end = out.len();
        applied.push(m);
        position = mask.end;
    }
    out.push_str(&text[position..]);
    (out, applied)
}

/// Tool names an agent uses for `fill`/`type`, with the argument holding the
/// entered text.
fn entered_text(name: &str, args: &Map<String, Value>) -> Option<&'static str> {
    let name = name.rsplit("__").next().unwrap_or(name);
    let name = name.rsplit('.').next().unwrap_or(name);
    match name {
        "chrome_use_fill" | "fill" => Some("value"),
        "chrome_use_type" | "type" => Some("text"),
        "chrome_use_find" | "find"
            if matches!(
                args.get("action").and_then(Value::as_str),
                Some("fill" | "type")
            ) =>
        {
            Some("text")
        }
        _ => None,
    }
}

struct Walker<'a> {
    known: &'a [String],
    learned: Vec<String>,
    redactions: Vec<Redaction>,
    class: PrivacyClass,
    cap: Option<usize>,
    tool_text: bool,
    /// Strings are cookie text (see [`cookie_context`]).
    cookie: bool,
    /// Decimal texts among the secrets, for numbers that repeat one.
    known_numbers: std::collections::HashSet<String>,
}

impl<'a> Walker<'a> {
    fn new(known: &'a [String], kind: &str) -> Self {
        Self {
            known,
            learned: Vec::new(),
            redactions: Vec::new(),
            class: PrivacyClass::Public,
            cap: None,
            tool_text: kind == "tool",
            cookie: false,
            known_numbers: known.iter().filter_map(|k| canonical_number(k)).collect(),
        }
    }
    fn top(&mut self, data: &mut Value, kind: &str) {
        // The user's own text is their record; it is not redacted.
        let exempt: &[&str] = match kind {
            "user" | "queueEdited" => &["text"],
            _ => &[],
        };
        match kind {
            "user" | "queueEdited" => self.raise(PrivacyClass::UserInput),
            "tool" => {
                let item = &data["item"];
                if data["completed"] == true {
                    self.raise(PrivacyClass::PageContent);
                }
                let name = item["name"]
                    .as_str()
                    .or_else(|| item["tool"].as_str())
                    .unwrap_or("");
                self.cap = Some(if name.ends_with("chrome_use_eval") {
                    EVAL_TEXT
                } else if item["type"] == "commandExecution" {
                    COMMAND_TEXT
                } else {
                    TOOL_TEXT
                });
            }
            _ => {}
        }
        let Some(object) = data.as_object_mut() else {
            self.walk(data, String::new());
            return;
        };
        self.fill_rule(object, "");
        for (key, value) in object.iter_mut() {
            if exempt.contains(&key.as_str()) || ID_KEYS.contains(&key.as_str()) {
                continue;
            }
            let path = format!("/{}", pointer(key));
            if let Some(kind) = secret_key(key) {
                self.mask(value, path, &kind);
            } else {
                self.walk(value, path);
            }
        }
    }
    fn raise(&mut self, class: PrivacyClass) {
        self.class = self.class.max(class);
    }
    fn walk(&mut self, value: &mut Value, path: String) {
        match value {
            Value::String(text) => self.text(text, &path),
            // A number that is one of the secrets (a numeric OTP repeated).
            Value::Number(n)
                if canonical_number(&n.to_string())
                    .is_some_and(|c| self.known_numbers.contains(&c)) =>
            {
                let length = n.to_string().len();
                *value = marker("secret", length).into();
                self.redactions.push(Redaction {
                    path,
                    kind: "secret".into(),
                    length,
                });
            }
            Value::Array(items) => {
                for (i, item) in items.iter_mut().enumerate() {
                    self.walk(item, child_path(&path, &i.to_string()));
                }
            }
            Value::Object(object) => {
                self.image(object, &path);
                self.fill_rule(object, &path);
                for (key, child) in object.iter_mut() {
                    if ID_KEYS.contains(&key.as_str()) {
                        continue;
                    }
                    let path = child_path(&path, key);
                    if let Some(kind) = secret_key(key) {
                        self.mask(child, path, &kind);
                    } else {
                        self.walk(child, path);
                    }
                }
            }
            _ => {}
        }
    }
    fn text(&mut self, text: &mut String, path: &str) {
        let mut masks = find_masks(text, self.known, self.tool_text);
        // The attached page's URL, whatever its scheme (file://, chrome://).
        if path == "/page/url" && text.contains("://") && !text.starts_with("http") {
            masks.extend(url_masks(text, 0));
            masks.sort_by(|a, b| a.start.cmp(&b.start).then(b.end.cmp(&a.end)));
            let mut kept: Vec<Mask> = Vec::new();
            for mask in masks {
                if kept.last().is_none_or(|last| mask.start >= last.end) {
                    kept.push(mask);
                }
            }
            masks = kept;
        }
        if self.cookie {
            masks.extend(cookie_masks(text));
            // A rule's own label wins over the known-secret one.
            masks.sort_by(|a, b| {
                a.start
                    .cmp(&b.start)
                    .then(b.end.cmp(&a.end))
                    .then((a.kind == "secret").cmp(&(b.kind == "secret")))
            });
            let mut kept: Vec<Mask> = Vec::new();
            for mask in masks {
                if kept.last().is_none_or(|last| mask.start >= last.end) {
                    kept.push(mask);
                }
            }
            masks = kept;
        }
        if !masks.is_empty() {
            let (out, applied) = apply(text, &masks);
            for mask in &applied {
                if let Some(secret) = &mask.secret {
                    self.learned.push(secret.clone());
                }
                self.redactions.push(Redaction {
                    path: path.into(),
                    kind: mask.kind.clone(),
                    length: mask.length,
                });
            }
            *text = out;
        }
        if let Some(cap) = self.cap {
            if text.chars().count() > cap {
                let mut short: String = text.chars().take(cap).collect();
                short.push_str("\n[shortened]");
                *text = short;
            }
        }
    }
    /// Remove inline image bytes; pixels live only in artifacts.
    fn image(&mut self, object: &mut Map<String, Value>, path: &str) {
        if object.get("type").and_then(Value::as_str) != Some("image") {
            return;
        }
        let data = if object.get("data").is_some_and(Value::is_string) {
            object.get_mut("data")
        } else {
            object
                .get_mut("source")
                .and_then(|s| s.get_mut("data"))
                .filter(|d| d.is_string())
        };
        if let Some(data) = data {
            let length = data.as_str().map(|s| s.len()).unwrap_or(0);
            *data = format!("[image omitted, {length} chars]").into();
            self.redactions.push(Redaction {
                path: format!("{path}/data"),
                kind: "image".into(),
                length,
            });
        }
    }
    /// `fill`/`type` calls: the entered text is masked when the target names
    /// a sensitive field or the text has a secret shape.
    fn fill_rule(&mut self, object: &mut Map<String, Value>, path: &str) {
        let name = ["name", "tool", "toolName", "tool_name"]
            .iter()
            .find_map(|k| object.get(*k).and_then(Value::as_str))
            .unwrap_or("")
            .to_string();
        let Some(args_key) = ["input", "arguments", "rawInput", "args", "tool_params"]
            .into_iter()
            .find(|k| object.get(*k).is_some_and(Value::is_object))
        else {
            return;
        };
        let Some(args) = object.get_mut(args_key).and_then(Value::as_object_mut) else {
            return;
        };
        let Some(field) = entered_text(&name, args) else {
            return;
        };
        let Some(entered) = args.get(field).and_then(Value::as_str) else {
            return;
        };
        self.raise(PrivacyClass::UserInput);
        let target_sensitive = ["selector", "locator", "label", "name", "value"]
            .iter()
            .filter(|k| **k != field)
            .filter_map(|k| args.get(*k).and_then(Value::as_str))
            .any(sensitive::sensitive_reference);
        let shaped = !find_masks(entered, &[], false).is_empty();
        if target_sensitive || shaped {
            let child_path = format!("{path}/{}/{}", pointer(args_key), pointer(field));
            if let Some(value) = args.get_mut(field) {
                self.mask(value, child_path, "input");
            }
        }
    }
    fn mask(&mut self, value: &mut Value, path: String, kind: &str) {
        let length = match &*value {
            Value::Null => return,
            Value::String(text) if is_marker(text) => return,
            Value::String(text) => {
                self.learned.push(text.clone());
                text.chars().count()
            }
            other => {
                collect_strings(other, &mut self.learned);
                other.to_string().chars().count()
            }
        };
        *value = marker(kind, length).into();
        self.redactions.push(Redaction {
            path,
            kind: kind.into(),
            length,
        });
    }
}

/// Every string and number (as its decimal text) inside a masked value: an
/// OTP sent as a number is as secret as one sent as text.
fn collect_strings(value: &Value, out: &mut Vec<String>) {
    match value {
        Value::String(s) => out.push(s.clone()),
        Value::Number(n) => out.push(n.to_string()),
        Value::Array(items) => items.iter().for_each(|v| collect_strings(v, out)),
        Value::Object(map) => map.values().for_each(|v| collect_strings(v, out)),
        _ => {}
    }
}

fn pointer(key: &str) -> String {
    key.replace('~', "~0").replace('/', "~1")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn redact(kind: &str, data: Value) -> Value {
        event(kind, data, &mut Secrets::default())
            .unwrap()
            .into_value()
    }

    #[test]
    fn sensitive_keys_are_masked_as_strings_with_records() {
        for key in [
            "password",
            "passwd",
            "pwd",
            "secret",
            "token",
            "apiKey",
            "api_key",
            "authorization",
            "cookie",
            "set-cookie",
            "otp",
            "cvv",
            "cvc",
        ] {
            let out = redact(
                "tool",
                json!({"completed":false,"item":{"name":"x","input":{key:"hunter2-value"}}}),
            );
            let masked = &out["item"]["input"][key];
            assert!(masked.is_string(), "{key}");
            assert_eq!(
                masked.as_str().unwrap(),
                format!("[redacted {}, 13 chars]", key.to_ascii_lowercase()),
                "{key}"
            );
            assert_eq!(out["privacyClass"], "masked");
            assert_eq!(
                out["redactions"][0]["path"],
                format!("/item/input/{key}"),
                "{key}"
            );
            assert_eq!(out["redactions"][0]["length"], 13);
            // Non-string values become marker strings too, never objects.
            let out = redact("request", json!({"params":{key:{"nested":"value"}}}));
            assert!(out["params"][key].is_string(), "{key}");
        }
    }

    #[test]
    fn headers_are_masked_in_arguments_and_text() {
        for header in [
            "authorization",
            "proxy-authorization",
            "cookie",
            "set-cookie",
            "x-api-key",
            "x-auth-token",
            "x-csrf-token",
        ] {
            let out = redact(
                "tool",
                json!({"item":{"tool":"chrome_use_network_route","arguments":{"headers":{header.to_ascii_uppercase():"abc def ghi"}}}}),
            );
            let value = &out["item"]["arguments"]["headers"][header.to_ascii_uppercase()];
            assert_eq!(value, &json!(format!("[redacted {header}, 11 chars]")));
            let text = format!("HTTP/1.1 200\n{header}: abcdefgh12345\nok");
            let out = redact(
                "tool",
                json!({"item":{"type":"tool_result","content":text}}),
            );
            let content = out["item"]["content"].as_str().unwrap();
            assert!(!content.contains("abcdefgh12345"), "{header}: {content}");
            assert!(
                content.contains(&format!("{header}: [redacted")),
                "{content}"
            );
        }
    }

    #[test]
    fn url_parameters_fragments_and_userinfo_are_masked() {
        for key in URL_KEYS {
            let u = format!("https://shop.example/a/b?x=1&{key}=SECRETVALUE9#frag");
            assert_eq!(
                url(&u),
                format!("https://shop.example/a/b?x=1&{key}=[redacted]#frag"),
                "{key}"
            );
            let f = format!("https://shop.example/cb#{key}=SECRETVALUE9&y=2");
            assert_eq!(
                url(&f),
                format!("https://shop.example/cb#{key}=[redacted]&y=2")
            );
        }
        assert_eq!(
            url("https://Access_Token=no@shop.example/p?ACCESS_TOKEN=abc"),
            "https://shop.example/p?ACCESS_TOKEN=[redacted]"
        );
        assert_eq!(
            url("https://user:pa55word@shop.example/"),
            "https://shop.example/"
        );
        let once = url("https://u:p@h.example/x?token=a&code=b#state=c");
        assert_eq!(url(&once), once, "redaction is idempotent");
        // Host and path are kept.
        assert_eq!(
            url("https://bank.example/pay/42?amount=10"),
            "https://bank.example/pay/42?amount=10"
        );
        // URLs inside free text, arguments and results.
        let out = redact(
            "tool",
            json!({"item":{"name":"chrome_use_open","input":{"url":"https://a.example/?sig=XYZ12345"},"result":"landed on https://a.example/done?X-Amz-Signature=deadbeef1 ok"}}),
        );
        assert_eq!(
            out["item"]["input"]["url"],
            "https://a.example/?sig=[redacted]"
        );
        assert_eq!(
            out["item"]["result"],
            "landed on https://a.example/done?X-Amz-Signature=[redacted] ok"
        );
    }

    #[test]
    fn secret_shapes_and_cookie_strings_are_masked() {
        let cases = [
            "eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjM0NTY3ODkwIn0.dozjgNryP4J3jVmNHl0w5N_XgL0n3I9PlFUP0THsR8U",
            "Bearer abc123defghijklmnop",
            "sk-ant-api03-abcdefghijklmnopqrstuvwxyz",
            "ghp_abcdefghijklmnopqrstuvwxyz0123",
            "xoxb-1234567890-abcdefghij",
            "AKIAABCDEFGHIJKLMNOP",
            "-----BEGIN RSA PRIVATE KEY-----\nMIIEow\nIBAAK\n-----END RSA PRIVATE KEY-----",
            "sid=abcdefgh123; theme=dark; csrftoken=zzzzzzzz99",
        ];
        for case in cases {
            let out = redact(
                "textFinal",
                json!({"itemId":"a","text":format!("see {case} end"),"part":0,"last":true}),
            );
            let text = out["text"].as_str().unwrap();
            assert!(text.contains("[redacted"), "{case}: {text}");
            assert!(text.ends_with(" end"), "{text}");
            assert_eq!(out["privacyClass"], "masked");
        }
        let out = redact(
            "textFinal",
            json!({"itemId":"a","text":"cookie: sid=abcdefgh123; theme=dark; csrftoken=zzzzzzzz99","part":0,"last":true}),
        );
        let text = out["text"].as_str().unwrap();
        assert!(
            !text.contains("abcdefgh123") && !text.contains("zzzzzzzz99"),
            "{text}"
        );
        // Short cookie values and ordinary assignments stay.
        let out = redact(
            "textFinal",
            json!({"itemId":"a","text":"theme=dark; lang=en and x=1","part":0,"last":true}),
        );
        assert_eq!(out["text"], "theme=dark; lang=en and x=1");
        assert_eq!(out["privacyClass"], "public");
    }

    #[test]
    fn fill_and_type_values_follow_the_sensitive_lists() {
        let fill = |selector: &str, value: &str| {
            redact(
                "tool",
                json!({"item":{"type":"tool_use","name":"mcp__chrome_use__chrome_use_fill","id":"t1","input":{"selector":selector,"value":value}}}),
            )
        };
        let out = fill("#password", "hunter22");
        assert_eq!(out["item"]["input"]["value"], "[redacted input, 8 chars]");
        assert_eq!(out["privacyClass"], "masked");
        let out = fill("input[name=search]", "running shoes");
        assert_eq!(out["item"]["input"]["value"], "running shoes");
        assert_eq!(out["privacyClass"], "userInput");
        let out = fill("@e3", "sk-abcdefghijklmnopqrstu");
        assert!(out["item"]["input"]["value"]
            .as_str()
            .unwrap()
            .starts_with("[redacted"));
        // Codex spelling, type tool, label from the sensitive names.
        let out = redact(
            "tool",
            json!({"item":{"type":"mcpToolCall","tool":"chrome_use_type","arguments":{"selector":"Card number","text":"4242424242424242"}}}),
        );
        assert_eq!(
            out["item"]["arguments"]["text"],
            "[redacted input, 16 chars]"
        );
        let out = redact(
            "tool",
            json!({"item":{"tool":"chrome_use_find","arguments":{"locator":"label","value":"验证码","action":"fill","text":"123456"}}}),
        );
        assert_eq!(
            out["item"]["arguments"]["text"],
            "[redacted input, 6 chars]"
        );
        // An approval request carries the same input.
        let out = redact(
            "request",
            json!({"requestId":"r","method":"can_use_tool","params":{"tool_name":"mcp__chrome_use__chrome_use_fill","input":{"selector":"#cvc","value":"123"}}}),
        );
        assert_eq!(out["params"]["input"]["value"], "[redacted input, 3 chars]");
    }

    #[test]
    fn known_secrets_are_scrubbed_from_later_display_text_only() {
        let mut secrets = Secrets::default();
        let id = "11111111-2222-4333-8444-555555555555";
        // The secret is also a fragment of every id below.
        let secret = "2222-4333-8444";
        event(
            "tool",
            json!({"item":{"name":"chrome_use_fill","input":{"selector":"#password","value":secret}}}),
            &mut secrets,
        )
        .unwrap();
        let out = event(
            "tool",
            json!({"messageId":id,"completed":true,"item":{"type":"tool_result","tool_use_id":id,"id":id,"content":format!("typed {secret} into {id}")}}),
            &mut secrets,
        )
        .unwrap()
        .into_value();
        assert_eq!(out["messageId"], id);
        assert_eq!(out["item"]["tool_use_id"], id);
        assert_eq!(out["item"]["id"], id);
        let content = out["item"]["content"].as_str().unwrap();
        assert!(!content.contains(secret), "{content}");
        let out = event(
            "request",
            json!({"requestId":id,"method":"x","params":{"message":format!("use {secret}?")}}),
            &mut secrets,
        )
        .unwrap()
        .into_value();
        assert_eq!(out["requestId"], id);
        assert_eq!(out["params"]["message"], "use [redacted secret, 14 chars]?");
        // The user's own text is not redacted.
        let out = event(
            "user",
            json!({"messageId":id,"text":format!("my code is {secret}")}),
            &mut secrets,
        )
        .unwrap()
        .into_value();
        assert_eq!(out["text"], format!("my code is {secret}"));
        assert_eq!(out["privacyClass"], "userInput");
        let out = event(
            "terminal",
            json!({"status":"failed","error":format!("bad {secret}")}),
            &mut secrets,
        )
        .unwrap()
        .into_value();
        assert_eq!(out["error"], "bad [redacted secret, 14 chars]");
    }

    #[test]
    fn kept_fields_survive() {
        let out = redact(
            "request",
            json!({"requestId":"r","method":"item/commandExecution/requestApproval","params":{"amount":"$1,250.00","recipient":"Alice Example","url":"https://bank.example/transfer/confirm","exitCode":1,"durationMs":53,"tabId":"chrome-tab:7"}}),
        );
        assert_eq!(out["params"]["amount"], "$1,250.00");
        assert_eq!(out["params"]["recipient"], "Alice Example");
        assert_eq!(
            out["params"]["url"],
            "https://bank.example/transfer/confirm"
        );
        assert_eq!(out["params"]["exitCode"], 1);
        assert_eq!(out["params"]["durationMs"], 53);
        assert_eq!(out["params"]["tabId"], "chrome-tab:7");
        assert_eq!(out["privacyClass"], "public");
        assert!(out.get("redactions").is_none());
    }

    #[test]
    fn tool_text_caps_and_images() {
        let long = "word ".repeat(3000);
        let out = redact(
            "tool",
            json!({"completed":true,"item":{"tool":"chrome_use_eval","result":long.clone()}}),
        );
        assert_eq!(
            out["item"]["result"].as_str().unwrap().chars().count(),
            2000 + 12
        );
        let out = redact(
            "tool",
            json!({"completed":true,"item":{"type":"commandExecution","aggregatedOutput":long.clone()}}),
        );
        assert_eq!(
            out["item"]["aggregatedOutput"]
                .as_str()
                .unwrap()
                .chars()
                .count(),
            4000 + 12
        );
        let out = redact(
            "tool",
            json!({"completed":true,"item":{"type":"tool_result","content":long}}),
        );
        assert_eq!(
            out["item"]["content"].as_str().unwrap().chars().count(),
            6000 + 12
        );
        assert_eq!(out["privacyClass"], "pageContent");
        let out = redact(
            "tool",
            json!({"completed":true,"item":{"type":"tool_result","content":[{"type":"image","source":{"type":"base64","data":"QUJD"}},{"type":"image","data":"QUJD","mimeType":"image/png"}]}}),
        );
        assert_eq!(
            out["item"]["content"][0]["source"]["data"],
            "[image omitted, 4 chars]"
        );
        assert_eq!(
            out["item"]["content"][1]["data"],
            "[image omitted, 4 chars]"
        );
        let preview = format!("{{\"data\":\"{}\"}}", "A".repeat(4000));
        let out = redact("tool", json!({"item":{"preview":preview,"truncated":true}}));
        assert!(!out["item"]["preview"]
            .as_str()
            .unwrap()
            .contains("AAAAAAAAAAAA"));
    }

    #[test]
    fn legacy_read_redaction_skips_new_events_and_keeps_ids() {
        let mut data = json!({"messageId":"m","text":"hi","page":{"tabId":1,"url":"https://x.example/?token=abc","title":"t"}});
        legacy("user", &mut data);
        assert_eq!(data["page"]["url"], "https://x.example/?token=[redacted]");
        assert_eq!(data["text"], "hi");
        let new = redact(
            "user",
            json!({"messageId":"m","text":"x","page":{"url":"https://x.example/?q=1"}}),
        );
        let mut again = new.clone();
        legacy("user", &mut again);
        assert_eq!(again, new);
        // Masking an already masked event changes nothing.
        let first = redact(
            "tool",
            json!({"item":{"input":{"password":"abcdefgh","url":"https://a@b.example/?code=1"},"result":"Bearer abc123defghijkl"}}),
        );
        let mut second = first.clone();
        second.as_object_mut().unwrap().remove("privacyClass");
        legacy("tool", &mut second);
        second["privacyClass"] = first["privacyClass"].clone();
        assert_eq!(second, first);
    }

    fn stream_all(secrets: &mut Secrets, chunks: &[&str]) -> String {
        let mut out = String::new();
        for chunk in chunks {
            if let Some(r) = event(
                "textDelta",
                json!({"itemId":"i","messageId":"m","text":chunk}),
                secrets,
            ) {
                let v = r.into_value();
                assert_eq!(v["itemId"], "i");
                assert_eq!(v["messageId"], "m");
                out.push_str(v["text"].as_str().unwrap());
            }
            let held = secrets
                .streams
                .values()
                .map(TextScrubber::held)
                .max()
                .unwrap_or(0);
            assert!(held <= MAX_HOLD, "held {held}");
        }
        for (kind, r) in secrets.flush() {
            assert_eq!(kind, "textDelta");
            let v = r.into_value();
            assert_eq!(v["messageId"], "m");
            out.push_str(v["text"].as_str().unwrap());
        }
        assert!(secrets.streams.is_empty());
        out
    }

    fn splits(text: &str, parts: usize) -> Vec<Vec<usize>> {
        // Every placement of the first cut, the rest spread evenly after it.
        let mut out = Vec::new();
        for first in 1..text.len() {
            let mut cuts = vec![first];
            let rest = text.len() - first;
            for k in 1..parts - 1 {
                let c = first + rest * k / (parts - 1);
                if c > *cuts.last().unwrap() && c < text.len() {
                    cuts.push(c);
                }
            }
            out.push(cuts);
        }
        out
    }

    #[test]
    fn streamed_known_secret_with_spaces_is_caught_at_every_split() {
        let secret = "correct horse battery staple";
        let text = format!("Your passphrase is {secret} and nothing else.");
        for parts in [2, 3, 7] {
            for cuts in splits(&text, parts) {
                let mut secrets = Secrets::default();
                secrets.learn(secret);
                let mut chunks = Vec::new();
                let mut start = 0;
                for cut in cuts.iter().copied().chain([text.len()]) {
                    chunks.push(&text[start..cut]);
                    start = cut;
                }
                let out = stream_all(&mut secrets, &chunks);
                assert!(!out.contains(secret), "{parts} {cuts:?}: {out}");
                assert!(!out.contains("horse battery"), "{parts} {cuts:?}: {out}");
                assert_eq!(
                    out, "Your passphrase is [redacted secret, 28 chars] and nothing else.",
                    "{parts} {cuts:?}"
                );
            }
        }
    }

    #[test]
    fn streamed_shapes_split_across_chunks_are_caught() {
        let text = format!(
            "{}Authorization: Bearer abc123defghijklmnopqrstuvwxyz\nand Bearer abc123defghijklmnopqrstuvwxyz done",
            "lorem ipsum ".repeat(10)
        );
        let text = text.as_str();
        for cuts in splits(text, 3) {
            let mut secrets = Secrets::default();
            let chunks: Vec<&str> = [0]
                .into_iter()
                .chain(cuts.iter().copied())
                .zip(cuts.iter().copied().chain([text.len()]))
                .map(|(a, b)| &text[a..b])
                .collect();
            let out = stream_all(&mut secrets, &chunks);
            assert!(!out.contains("defghijklmnop"), "{cuts:?}: {out}");
            assert!(out.ends_with(" done"), "{cuts:?}: {out}");
        }
    }

    #[test]
    fn long_unbroken_run_is_bounded_and_masked_through() {
        let mut secrets = Secrets::default();
        let run = format!("sk-{}", "a".repeat(10000));
        let chunks: Vec<String> = run
            .as_bytes()
            .chunks(300)
            .map(|c| String::from_utf8(c.to_vec()).unwrap())
            .chain(["   tail text".to_string()])
            .collect();
        let refs: Vec<&str> = chunks.iter().map(String::as_str).collect();
        let out = stream_all(&mut secrets, &refs);
        assert!(!out.contains("aaaaaaaa"), "{}", &out[..200.min(out.len())]);
        assert!(out.ends_with("   tail text"));
        // Plain long text without spaces is released once the cap is reached.
        let mut secrets = Secrets::default();
        let plain = "x".repeat(9000);
        let refs: Vec<&str> = (0..9).map(|i| &plain[i * 1000..(i + 1) * 1000]).collect();
        assert_eq!(stream_all(&mut secrets, &refs), plain);
    }

    #[test]
    fn final_text_parts_keep_numbering_and_flush_on_last() {
        let mut secrets = Secrets::default();
        secrets.learn("abcd efgh ijkl");
        let a = event(
            "textFinal",
            json!({"itemId":"i","text":"start abcd ef","part":0,"last":false}),
            &mut secrets,
        )
        .unwrap()
        .into_value();
        let b = event(
            "textFinal",
            json!({"itemId":"i","text":"gh ijkl end","part":1,"last":true}),
            &mut secrets,
        )
        .unwrap()
        .into_value();
        assert_eq!((a["part"].clone(), b["part"].clone()), (json!(0), json!(1)));
        let text = format!(
            "{}{}",
            a["text"].as_str().unwrap(),
            b["text"].as_str().unwrap()
        );
        assert_eq!(text, "start [redacted secret, 14 chars] end");
        assert!(secrets.streams.is_empty());
        // A final text discards the streamed deltas of its item.
        event(
            "textDelta",
            json!({"itemId":"i","text":"partial"}),
            &mut secrets,
        );
        event(
            "textFinal",
            json!({"itemId":"i","text":"done","part":0,"last":true}),
            &mut secrets,
        );
        assert!(secrets.flush().is_empty());
    }

    #[test]
    fn a_lone_cookie_read_by_the_page_is_masked_and_its_echo_scrubbed() {
        let mut secrets = Secrets::default();
        // Codex: arguments and result in one item.
        let out = event(
            "tool",
            json!({"completed":true,"toolCallId":"c1","item":{"type":"mcpToolCall","id":"c1","tool":"chrome_use_eval","arguments":{"script":"document.cookie"},"result":{"content":[{"type":"text","text":"sid=SYNTHCOOKIE42"}]}}}),
            &mut secrets,
        )
        .unwrap()
        .into_value();
        let text = out["item"]["result"]["content"][0]["text"]
            .as_str()
            .unwrap();
        assert_eq!(text, "sid=[redacted cookie, 13 chars]");
        // The backend's echo in its answer is scrubbed too.
        let echo = event(
            "textFinal",
            json!({"itemId":"a","text":"Your session cookie is sid=SYNTHCOOKIE42.","part":0,"last":true}),
            &mut secrets,
        )
        .unwrap()
        .into_value();
        assert!(!echo["text"].as_str().unwrap().contains("SYNTHCOOKIE42"));
        // Claude: the result block arrives separately, by tool_use_id.
        let mut secrets = Secrets::default();
        event(
            "tool",
            json!({"completed":false,"toolCallId":"t9","item":{"type":"tool_use","id":"t9","name":"mcp__chrome_use__chrome_use_eval","input":{"script":"return document.cookie"}}}),
            &mut secrets,
        );
        let out = event(
            "tool",
            json!({"completed":true,"toolCallId":"t9","item":{"type":"tool_result","tool_use_id":"t9","content":"{\"stdout\":\"token_x=abc\"}"}}),
            &mut secrets,
        )
        .unwrap()
        .into_value();
        assert!(
            !out["item"]["content"].as_str().unwrap().contains("abc\""),
            "{out}"
        );
        // Outside a cookie read, ordinary assignments stay.
        let plain = event(
            "tool",
            json!({"completed":true,"item":{"type":"mcpToolCall","id":"c2","tool":"chrome_use_eval","arguments":{"script":"location.search"},"result":"page=2 sort=asc"}}),
            &mut Secrets::default(),
        )
        .unwrap()
        .into_value();
        assert_eq!(plain["item"]["result"], "page=2 sort=asc");
    }

    #[test]
    fn large_items_are_redacted_whole_before_they_are_bounded() {
        // The word that marks the field sensitive sits past any cut.
        let selector = format!("#{}password", "a".repeat(6100));
        let data = json!({"completed":false,"toolCallId":"t1","item":{"type":"tool_use","id":"t1",
            "name":"mcp__chrome_use__chrome_use_fill",
            "input":{"selector":selector,"value":"plain-pass-SYNTH"},
            "observation":"word ".repeat(8_000)}});
        let out = event("tool", data, &mut Secrets::default())
            .unwrap()
            .into_value();
        assert!(!out.to_string().contains("plain-pass-SYNTH"));
        assert!(out["item"].to_string().len() <= ITEM_BUDGET);
        assert_eq!(out["item"]["name"], "mcp__chrome_use__chrome_use_fill");
        assert!(out["item"]["observation"]
            .as_str()
            .unwrap()
            .contains("shortened"));
        // An item too wide to cut keeps its call and redacted arguments.
        let wide: serde_json::Map<String, Value> =
            (0..5000).map(|i| (format!("k{i}"), json!("v"))).collect();
        let data = json!({"completed":false,"item":{"type":"tool_use","id":"t2",
            "name":"mcp__chrome_use__chrome_use_fill",
            "input":{"selector":"#otp","value":"483927"},"extra":wide}});
        let out = event("tool", data, &mut Secrets::default())
            .unwrap()
            .into_value();
        assert!(out["item"].to_string().len() <= ITEM_BUDGET);
        assert_eq!(out["item"]["name"], "mcp__chrome_use__chrome_use_fill");
        assert_eq!(out["item"]["input"]["value"], "[redacted input, 6 chars]");
        assert!(!out.to_string().contains("483927"));
    }

    #[test]
    fn a_secret_found_in_an_event_is_masked_where_the_same_event_repeats_it() {
        let mut secrets = Secrets::default();
        let out = event(
            "tool",
            json!({"completed":true,"item":{"name":"chrome_use_fill","input":{"selector":"#password","value":"blue river pass"},"output":"typed blue river pass"}}),
            &mut secrets,
        )
        .unwrap()
        .into_value();
        assert!(!out.to_string().contains("blue river pass"), "{out}");
        assert_eq!(out["item"]["output"], "typed [redacted secret, 15 chars]");
        // A short code is masked where this event repeats it, but not
        // remembered for later events (below the learning floor).
        let out = event(
            "request",
            json!({"requestId":"r","method":"can_use_tool","params":{"tool_name":"mcp__chrome_use__chrome_use_fill",
                "input":{"selector":"#otp","value":"483927"},"description":"Enter 483927 into the code field"}}),
            &mut secrets,
        )
        .unwrap()
        .into_value();
        assert!(!out.to_string().contains("483927"), "{out}");
        assert!(!secrets.known().iter().any(|k| k == "483927"));
        // The longer secret was remembered.
        assert!(secrets.known().iter().any(|k| k == "blue river pass"));
    }

    #[test]
    fn many_redactions_still_fit_one_journal_line() {
        let many: Vec<Value> = (0..2000).map(|_| json!({"password":"x"})).collect();
        let out = event(
            "tool",
            json!({"completed":true,"item":{"type":"tool_result","tool_use_id":"t","content":many}}),
            &mut Secrets::default(),
        )
        .unwrap()
        .into_value();
        assert!(
            out.to_string().len() < 64 * 1024,
            "{}",
            out.to_string().len()
        );
        assert_eq!(out["redactions"].as_array().unwrap().len(), MAX_REDACTIONS);
        assert_eq!(out["redactionsOmitted"], 2000 - MAX_REDACTIONS);
        assert_eq!(out["privacyClass"], "masked");
    }

    #[test]
    fn numeric_secrets_are_masked_where_the_event_repeats_them() {
        let out = event(
            "tool",
            json!({"completed":true,"item":{"name":"x","input":{"otp":483927},"output":"typed 483927","again":483927}}),
            &mut Secrets::default(),
        )
        .unwrap()
        .into_value();
        assert!(!out.to_string().contains("483927"), "{out}");
    }

    #[test]
    fn long_redaction_paths_still_fit_one_journal_line() {
        let mut content = serde_json::Map::new();
        for i in 0..64 {
            content.insert(format!("{i}{}", "k".repeat(1100)), json!({"password":"x"}));
        }
        let out = event(
            "tool",
            json!({"completed":true,"item":{"type":"tool_result","tool_use_id":"t","content":content}}),
            &mut Secrets::default(),
        )
        .unwrap()
        .into_value();
        assert!(
            out.to_string().len() <= EVENT_BUDGET,
            "{}",
            out.to_string().len()
        );
        assert!(!out.to_string().contains("\"password\":\"x\""));
    }

    #[test]
    fn ten_thousand_distinct_secrets_are_withheld_quickly() {
        let rows: Vec<Value> = (0..10_000)
            .map(
                |i| json!({"password": format!("pw{i:06}xyz"), "label": format!("ok pw{i:06}xyz")}),
            )
            .collect();
        let started = std::time::Instant::now();
        let out = event(
            "tool",
            json!({"completed":true,"item":{"type":"tool_result","tool_use_id":"t","content":rows}}),
            &mut Secrets::default(),
        )
        .unwrap()
        .into_value();
        let elapsed = started.elapsed();
        let text = out.to_string();
        assert!(!text.contains("pw000123xyz"));
        assert!(text.len() <= EVENT_BUDGET);
        // Debug build; release is several times faster.
        assert!(elapsed < std::time::Duration::from_secs(3), "{elapsed:?}");
    }

    /// Deterministic random JSON with secrets in rule-triggering positions,
    /// as strings and numbers, and echoes of them in ordinary text: no
    /// secret survives, the event fits a journal line, and it is fast.
    #[test]
    fn random_events_never_leak_a_placed_secret() {
        struct Rng(u64);
        impl Rng {
            fn next(&mut self) -> u64 {
                self.0 ^= self.0 << 13;
                self.0 ^= self.0 >> 7;
                self.0 ^= self.0 << 17;
                self.0
            }
            fn below(&mut self, n: u64) -> u64 {
                self.next() % n.max(1)
            }
        }
        const WORDS: &[&str] = &["alpha", "bravo", "carol", "delta", "echo", "fox", "golf"];
        fn filler(rng: &mut Rng) -> String {
            (0..rng.below(6) + 1)
                .map(|_| WORDS[rng.below(WORDS.len() as u64) as usize])
                .collect::<Vec<_>>()
                .join(" ")
        }
        fn secret(rng: &mut Rng) -> String {
            const CHARS: &[u8] = b"ABCDEFGHJKLMNPQRSTUVWXYZ23456789";
            let len = 8 + rng.below(9) as usize;
            (0..len)
                .map(|_| CHARS[rng.below(CHARS.len() as u64) as usize] as char)
                .collect()
        }
        // A value placed where a rule masks it; returns it and its text.
        fn placed(rng: &mut Rng, secrets: &mut Vec<String>) -> (String, Value) {
            let numeric = rng.below(4) == 0;
            let text = if numeric {
                format!("{}", 100_000 + rng.below(899_999_999))
            } else {
                secret(rng)
            };
            secrets.push(text.clone());
            let as_value = || {
                if numeric {
                    json!(text.parse::<u64>().unwrap())
                } else {
                    json!(text)
                }
            };
            let key = [
                "password",
                "otp",
                "token",
                "apiKey",
                "Authorization",
                "cookie",
            ][rng.below(6) as usize];
            let value = match rng.below(4) {
                0 => json!({ key: as_value() }),
                1 => {
                    json!({"name":"chrome_use_fill","input":{"selector":"#password","value":text}})
                }
                2 => json!({"url": format!("https://x.example/cb?access_token={text}&page=2")}),
                _ => json!({"headers": {"Authorization": as_value()}}),
            };
            (key.to_string(), value)
        }
        fn tree(rng: &mut Rng, depth: u32, secrets: &mut Vec<String>) -> Value {
            match rng.below(if depth > 4 { 3 } else { 7 }) {
                0 => {
                    // An echo of an earlier secret in ordinary text.
                    if let Some(s) = secrets.get(rng.below(secrets.len() as u64 + 1) as usize) {
                        json!(format!("{} {s} {}", filler(rng), filler(rng)))
                    } else {
                        json!(filler(rng))
                    }
                }
                1 => json!(rng.below(1000)),
                2 => json!(filler(rng)),
                3 => placed(rng, secrets).1,
                4 => {
                    let n = if rng.below(20) == 0 {
                        50 + rng.below(200)
                    } else {
                        rng.below(5)
                    };
                    Value::Array((0..n).map(|_| tree(rng, depth + 1, secrets)).collect())
                }
                _ => {
                    let mut map = serde_json::Map::new();
                    for i in 0..rng.below(5) + 1 {
                        let key = if rng.below(8) == 0 {
                            format!("k{i}{}", "x".repeat(rng.below(1500) as usize))
                        } else {
                            format!("field{i}")
                        };
                        map.insert(key, tree(rng, depth + 1, secrets));
                    }
                    Value::Object(map)
                }
            }
        }
        let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
        for round in 0..2000 {
            let mut placed_secrets = Vec::new();
            // Place secrets first so later echoes can repeat them.
            let mut body = serde_json::Map::new();
            for i in 0..rng.below(3) + 1 {
                body.insert(format!("p{i}"), placed(&mut rng, &mut placed_secrets).1);
            }
            body.insert("rest".into(), tree(&mut rng, 0, &mut placed_secrets));
            for i in 0..rng.below(3) {
                if let Some(s) = placed_secrets.get(rng.below(placed_secrets.len() as u64) as usize)
                {
                    body.insert(format!("echo{i}"), json!(format!("typed {s} done")));
                }
            }
            let kind = if rng.below(2) == 0 { "tool" } else { "request" };
            if kind == "tool" && rng.below(4) == 0 {
                // A page's own cookie read: a lone name=value is a cookie.
                let cookie = secret(&mut rng);
                placed_secrets.push(cookie.clone());
                body.insert("input".into(), json!({"script": "return document.cookie"}));
                body.insert("result".into(), json!(format!("sid={cookie}")));
            }
            let data = if kind == "tool" {
                json!({"completed": true, "item": Value::Object(body)})
            } else {
                json!({"requestId": "r", "method": "can_use_tool", "params": Value::Object(body)})
            };
            let input = data.to_string().len();
            let started = std::time::Instant::now();
            let out = event(kind, data, &mut Secrets::default())
                .unwrap()
                .into_value();
            // Debug build, about 7 MB/s on these trees; release is
            // several times faster.
            let limit = std::time::Duration::from_millis(300 + (input / 3000) as u64);
            assert!(
                started.elapsed() < limit,
                "round {round} slow: {input} bytes in, {:?}",
                started.elapsed()
            );
            let text = out.to_string();
            assert!(
                text.len() <= EVENT_BUDGET,
                "round {round}: {} bytes",
                text.len()
            );
            for s in &placed_secrets {
                assert!(
                    !text.contains(s.as_str()),
                    "round {round}: {s} leaked in {text}"
                );
            }
        }
    }

    /// Every real entry point, with secrets placed where a rule masks them
    /// and repeated elsewhere: tool items, approval requests, streamed and
    /// final text (split anywhere), user events with a page, numeric echoes
    /// in every spelling, and events revealing more than 256 secrets.
    #[test]
    fn every_entry_point_keeps_placed_secrets_out() {
        struct Rng(u64);
        impl Rng {
            fn next(&mut self) -> u64 {
                self.0 ^= self.0 << 13;
                self.0 ^= self.0 >> 7;
                self.0 ^= self.0 << 17;
                self.0
            }
            fn below(&mut self, n: u64) -> u64 {
                self.next() % n.max(1)
            }
            fn secret(&mut self) -> String {
                const CHARS: &[u8] = b"ABCDEFGHJKLMNPQRSTUVWXYZ23456789";
                let len = 8 + self.below(9) as usize;
                (0..len)
                    .map(|_| CHARS[self.below(CHARS.len() as u64) as usize] as char)
                    .collect()
            }
        }
        let check = |out: &Value, secrets: &[String], what: &str| {
            let text = out.to_string();
            assert!(text.len() <= EVENT_BUDGET, "{what}: {} bytes", text.len());
            for s in secrets {
                assert!(!text.contains(s.as_str()), "{what}: {s} leaked in {text}");
            }
        };
        let mut rng = Rng(0xD1B5_4A32_D192_ED03);
        for round in 0..1500 {
            let what = format!("round {round}");
            let s = rng.secret();
            let n = 100_000 + rng.below(899_999_999);
            // Numeric secret echoed as a number, a float and a string.
            let out = event(
                "tool",
                json!({"completed":true,"item":{"name":"x","input":{"otp":n},"a":n,"b":n as f64,"c":format!("{n}"),"d":format!("code {n} sent")}}),
                &mut Secrets::default(),
            )
            .unwrap()
            .into_value();
            check(&out, &[n.to_string()], &what);
            // A request; its dispatch fields survive.
            let out = event(
                "request",
                json!({"requestId":"r","method":"can_use_tool","params":{"subtype":"can_use_tool","tool_name":"mcp__chrome_use__chrome_use_fill",
                    "input":{"selector":"#password","value":s},"description":format!("type {s}")}}),
                &mut Secrets::default(),
            )
            .unwrap()
            .into_value();
            check(&out, std::slice::from_ref(&s), &what);
            assert_eq!(out["method"], "can_use_tool");
            assert_eq!(
                out["params"]["tool_name"],
                "mcp__chrome_use__chrome_use_fill"
            );
            // A final text revealing and repeating a secret.
            let rule = match rng.below(3) {
                0 => format!("{{\"password\":\"{s}\"}}"),
                1 => format!("Authorization: Bearer {s}9"),
                _ => format!("https://x.example/cb?access_token={s}"),
            };
            let echoed = if rule.contains("Bearer") {
                format!("{s}9")
            } else {
                s.clone()
            };
            let text = format!("Done. {rule}\nYou typed {echoed} earlier and {echoed} again.");
            let out = event(
                "textFinal",
                json!({"itemId":"i","text":text,"part":0,"last":true}),
                &mut Secrets::default(),
            )
            .unwrap()
            .into_value();
            check(&out, std::slice::from_ref(&echoed), &what);
            // The same text streamed in random pieces.
            let mut secrets = Secrets::default();
            let mut streamed = String::new();
            let mut rest = text.as_str();
            while !rest.is_empty() {
                let mut cut = (1 + rng.below(12) as usize).min(rest.len());
                while !rest.is_char_boundary(cut) {
                    cut += 1;
                }
                let (piece, after) = rest.split_at(cut);
                rest = after;
                if let Some(r) = event(
                    "textDelta",
                    json!({"itemId":"i","text":piece}),
                    &mut secrets,
                ) {
                    streamed.push_str(r.value()["text"].as_str().unwrap());
                }
            }
            for (_, r) in secrets.flush() {
                streamed.push_str(r.value()["text"].as_str().unwrap());
            }
            assert!(
                !streamed.contains(&echoed),
                "{what}: {echoed} leaked in {streamed}"
            );
            // A user event: the page URL's token repeated in the title.
            let out = event(
                "user",
                json!({"messageId":"m","text":"hi","page":{"tabId":1,"url":format!("https://x.example/?access_token={s}&p=1"),"title":format!("echo {s}")}}),
                &mut Secrets::default(),
            )
            .unwrap()
            .into_value();
            check(&out, std::slice::from_ref(&s), &what);
            // More than 256 secrets: free text and numbers withheld, the
            // dispatch fields kept.
            if round % 50 == 0 {
                let many: Vec<String> = (0..300).map(|_| rng.secret()).collect();
                let nums: Vec<u64> = (0..300).map(|i| 1_000_000 + i).collect();
                let out = event(
                    "request",
                    json!({"requestId":"r","method":"can_use_tool","params":{"tool_name":"AskUserQuestion","otp":nums,
                        "rows":many.iter().map(|m| json!({"password":m})).collect::<Vec<_>>(),
                        "echo":format!("last {}", many[299]),"num":nums[299]}}),
                    &mut Secrets::default(),
                )
                .unwrap()
                .into_value();
                let mut all = many.clone();
                all.push(nums[299].to_string());
                check(&out, &all, &what);
                assert_eq!(out["method"], "can_use_tool");
                assert_eq!(out["params"]["tool_name"], "AskUserQuestion");
            }
        }
    }

    #[test]
    fn codex_round_five_cases() {
        // A final text revealing and repeating a password.
        let out = event(
            "textFinal",
            json!({"itemId":"i","text":"{\"password\":\"ABCD5678\"} echoed ABCD5678","part":0,"last":true}),
            &mut Secrets::default(),
        )
        .unwrap()
        .into_value();
        assert!(!out.to_string().contains("ABCD5678"), "{out}");
        // The page URL's token repeated in the page title.
        let out = event(
            "user",
            json!({"messageId":"m","text":"hi","page":{"tabId":1,"url":"https://x.example/?access_token=ABCD5678","title":"echo ABCD5678"}}),
            &mut Secrets::default(),
        )
        .unwrap()
        .into_value();
        assert!(!out.to_string().contains("ABCD5678"), "{out}");
        assert_eq!(
            out["page"]["url"],
            "https://x.example/?access_token=[redacted]"
        );
        // Over 256 numeric secrets: the 257th's echo is withheld too, and
        // the request still dispatches.
        let otp: Vec<u64> = (100_000..=100_256).collect();
        let out = event(
            "request",
            json!({"requestId":"r","method":"can_use_tool","params":{"tool_name":"x","otp":otp,"echo":100_256,"again":100_256.0}}),
            &mut Secrets::default(),
        )
        .unwrap()
        .into_value();
        assert!(!out.to_string().contains("100256"), "{out}");
        assert_eq!(out["method"], "can_use_tool");
        assert_eq!(out["params"]["tool_name"], "x");
    }

    #[test]
    fn known_secret_set_is_bounded() {
        let mut secrets = Secrets::default();
        secrets.learn("short");
        secrets.learn(&"x".repeat(MAX_SECRET + 1));
        assert!(secrets.known.is_empty());
        for i in 0..(MAX_KNOWN + 10) {
            secrets.learn(&format!("secret-value-{i}"));
        }
        assert_eq!(secrets.known.len(), MAX_KNOWN);
        assert_eq!(secrets.known[0], "secret-value-10");
    }
}
