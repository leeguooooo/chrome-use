//! Action leases and task assessment (plan 3.3, 4.3).
//!
//! A lease binds one browser action to the turn that admitted it. Its state
//! machine is the one in the plan: `Open` → `Reporting` → `Reported`, or
//! `Open` → `Unreported` at the turn's end → `LateReported` on a late report.
//! Every state is rebuilt from the journal, so a retry after a crash gets the
//! answer that was recorded. This module holds the state and the pure
//! computations; the engine journals what they return.

use super::artifacts::{Artifacts, Output, State};
use super::conversation::Event;
use super::evidence_proto::{CheckKind, ToolClass, Verdict};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::Path;

pub(super) const MAX_OPEN_LEASES: usize = 256;
const MAX_REQUIREMENTS: usize = 12;
const MAX_TEXT: usize = 500;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum LeaseState {
    Open,
    Reporting,
    Reported,
    Unreported,
    LateReported,
}

/// What one acted-on page looked like after the action.
#[derive(Clone, Debug, Default, PartialEq)]
pub(super) struct Landing {
    pub target_id: Option<String>,
    pub url: Option<String>,
    pub from_reply: bool,
}

#[derive(Clone, Debug)]
pub(super) struct Lease {
    pub message_id: String,
    pub generation: u64,
    pub tool: String,
    pub tool_class: ToolClass,
    pub outputs: Vec<Output>,
    pub state: LeaseState,
    /// Admitted before a stop that was requested later.
    pub after_stop: bool,
    /// Its turn has a `turnEnded`.
    pub turn_ended: bool,
    /// Sequence of its `browserActionTarget`.
    pub report_sequence: Option<u64>,
    pub landing: Landing,
    pub verdict: Option<Verdict>,
    /// The recorded check: kind and predicate as journaled (redacted), plus
    /// the raw predicate while this service has it in memory.
    pub check: Option<(CheckKind, String, Option<String>)>,
    pub delivery: Option<Value>,
    /// The page the tool named (`{tabHandle, url}`), journaled as
    /// `browserActionTarget.requested`.
    pub requested: Option<Value>,
    /// Admitted with `restore: true` (#893): the agent declared that this
    /// acting call only puts the page back. Journaled at admission, never
    /// taken from an assessment.
    pub restore: bool,
}

impl Lease {
    pub fn new(
        message_id: &str,
        generation: u64,
        tool: &str,
        tool_class: ToolClass,
        outputs: Vec<Output>,
    ) -> Self {
        Self {
            message_id: message_id.into(),
            generation,
            tool: tool.into(),
            tool_class,
            outputs,
            state: LeaseState::Open,
            after_stop: false,
            turn_ended: false,
            report_sequence: None,
            landing: Landing::default(),
            verdict: None,
            check: None,
            delivery: None,
            requested: None,
            restore: false,
        }
    }
    /// Whether the report computes the verdict from the artifacts.
    pub fn captures(&self) -> bool {
        self.tool_class == ToolClass::Capture || !self.outputs.is_empty()
    }
    /// The reply every report of a recorded lease gets: the verdict and each
    /// artifact's terminal state as recorded.
    pub fn reply(&self, artifacts: &Artifacts) -> Value {
        let states: Vec<Value> = self
            .outputs
            .iter()
            .filter_map(|o| artifact_state(artifacts, &o.artifact_id))
            .collect();
        json!({"bound": true, "verdict": self.verdict, "artifacts": states})
    }
}

/// `ArtifactStateReply` for a terminal artifact.
pub(super) fn artifact_state(artifacts: &Artifacts, artifact_id: &str) -> Option<Value> {
    match &artifacts.entries.get(artifact_id)?.state {
        State::Ready(info) => Some(
            json!({"artifactId": artifact_id, "state": "ready", "digest": info.digest, "bytes": info.bytes}),
        ),
        State::Unavailable(reason) => {
            Some(json!({"artifactId": artifact_id, "state": "unavailable", "reason": reason}))
        }
        _ => None,
    }
}

/// The page a report's `actionContext` says the action left behind.
pub(super) fn landing(action_context: Option<&Value>) -> Landing {
    let Some(after) = action_context
        .map(|c| &c["after"])
        .filter(|a| a.is_object())
    else {
        return Landing::default();
    };
    Landing {
        target_id: after["targetId"].as_str().map(String::from),
        url: after["url"].as_str().map(String::from),
        from_reply: after["provenance"] == "actionReply",
    }
}

fn parse<T: serde::de::DeserializeOwned>(value: &Value) -> Option<T> {
    serde_json::from_value(value.clone()).ok()
}

/// Rebuild one conversation's leases from its journal.
pub(super) fn rebuild<'a>(
    agent_cwd: &Path,
    events: impl IntoIterator<Item = &'a Event>,
) -> HashMap<String, Lease> {
    let mut leases: HashMap<String, Lease> = HashMap::new();
    for event in events {
        let d = &event.data;
        let action = d["actionId"].as_str().unwrap_or("");
        match event.kind.as_str() {
            "actionAdmitted" => {
                let outputs = d["outputs"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|o| Output::from_admitted(agent_cwd, action, Some(action), o))
                    .collect();
                let mut lease = Lease::new(
                    d["messageId"].as_str().unwrap_or(""),
                    d["generation"].as_u64().unwrap_or(0),
                    d["tool"].as_str().unwrap_or(""),
                    parse(&d["toolClass"]).unwrap_or(ToolClass::Act),
                    outputs,
                );
                lease.requested = d.get("requested").filter(|r| r.is_object()).cloned();
                lease.restore = d["restore"] == true;
                leases.insert(action.into(), lease);
            }
            "browserActionTarget" => {
                if let Some(lease) = leases.get_mut(action) {
                    if d["lateReported"] != true {
                        lease.state = LeaseState::Reporting;
                    }
                    lease.report_sequence = Some(event.sequence);
                    lease.after_stop |= d["afterStop"] == true;
                    lease.landing = landing(d.get("observed"));
                }
            }
            "actionObserved" => {
                if let Some(lease) = leases.get_mut(action) {
                    lease.state = if d["lateReported"] == true {
                        LeaseState::LateReported
                    } else {
                        LeaseState::Reported
                    };
                    lease.verdict = parse(&d["verdict"]);
                    lease.check = parse::<CheckKind>(&d["check"]["kind"]).map(|kind| {
                        (
                            kind,
                            d["check"]["predicate"].as_str().unwrap_or("").to_string(),
                            None,
                        )
                    });
                }
            }
            "fileDelivered" => {
                if let Some(lease) = leases.get_mut(action) {
                    lease.delivery = Some(json!({"state": d["state"]}));
                }
            }
            "turnEnded" => {
                // Every lease of the ended turn still open was closed then;
                // `openLeases` is only a bounded display list.
                let message = d["messageId"].as_str().unwrap_or("");
                for lease in leases.values_mut() {
                    if lease.message_id == message {
                        lease.turn_ended = true;
                        if lease.state == LeaseState::Open {
                            lease.state = LeaseState::Unreported;
                        }
                    }
                }
            }
            _ => {}
        }
    }
    leases
}

fn cap(text: &str) -> String {
    text.chars().take(MAX_TEXT).collect()
}

/// Compute a `taskAssessment` (plan 4.3) for `message_id` from what the
/// agent declared and the evidence leased under that message. `outcome` is
/// the turn's outcome for the final assessment, `None` while provisional.
pub(super) fn assess(
    declared: &Value,
    message_id: &str,
    leases: &HashMap<String, Lease>,
    artifacts: &Artifacts,
    outcome: Option<&str>,
    source: &str,
) -> Value {
    let phase = if outcome.is_some() {
        "final"
    } else {
        "provisional"
    };
    let mut downgraded: Vec<String> = Vec::new();
    let mut requirements = Vec::new();
    let empty = Vec::new();
    let listed = declared["requirements"].as_array().unwrap_or(&empty);
    if listed.len() > MAX_REQUIREMENTS {
        downgraded.push(format!(
            "only the first {MAX_REQUIREMENTS} requirements were considered"
        ));
    }
    for (index, requirement) in listed.iter().take(MAX_REQUIREMENTS).enumerate() {
        let text = cap(requirement["text"].as_str().unwrap_or(""));
        let status = requirement["status"]
            .as_str()
            .filter(|s| matches!(*s, "met" | "unmet" | "unknown"))
            .unwrap_or("unknown");
        let supplied = requirement.get("check").filter(|c| c.is_object());
        let (computed, because, recorded) = match status {
            "unmet" => ("unmet", "declared unmet".to_string(), None),
            "unknown" => ("unknown", "declared unknown".to_string(), None),
            _ => match supplied {
                None => ("claimed", "no check was named".to_string(), None),
                Some(check) => verify(check, message_id, leases, artifacts),
            },
        };
        if status == "met" && computed != "checked" {
            downgraded.push(format!("requirement {}: {because}", index + 1));
        }
        // The predicate shown is the one the cited action recorded; the
        // agent's own wording is only a label.
        let check = supplied.map(|c| {
            json!({
                "kind": c["kind"].as_str().map(cap),
                "predicate": recorded
                    .clone()
                    .or_else(|| c["predicate"].as_str().map(String::from))
                    .map(|p| cap(&p)),
                "label": c["predicate"].as_str().map(cap),
                "evidence": c["evidence"].as_str().map(cap),
            })
        });
        requirements.push(json!({
            "id": index + 1,
            "text": text,
            "declared": status,
            "check": check,
            "computed": computed,
            "because": cap(&because),
        }));
    }
    let blocker = declared.get("blocker").filter(|b| b.is_object()).map(|b| {
        let kind = b["kind"]
            .as_str()
            .filter(|k| {
                matches!(
                    *k,
                    "humanCheck" | "login" | "permission" | "missingInput" | "siteError" | "other"
                )
            })
            .unwrap_or("other");
        json!({"kind": kind, "text": cap(b["text"].as_str().unwrap_or(""))})
    });
    let checked = requirements
        .iter()
        .filter(|r| r["computed"] == "checked")
        .count();
    let all_checked = !requirements.is_empty() && checked == requirements.len();
    let mut status = if blocker.is_some() {
        "blocked"
    } else if all_checked {
        "checksPassed"
    } else if checked > 0 {
        "someChecksPassed"
    } else {
        "notChecked"
    };
    if status == "checksPassed" && outcome.is_some_and(|o| o != "completed") {
        status = "someChecksPassed";
        downgraded.push(format!(
            "the request ended {}",
            outcome.unwrap_or("without completing")
        ));
    }
    let declared_status = declared["status"].as_str().map(cap);
    json!({
        "v": 1,
        "messageId": message_id,
        "phase": phase,
        "source": source,
        "status": status,
        "declaredStatus": declared_status,
        "reason": declared["reason"].as_str().map(cap),
        "requirements": requirements,
        "downgradedBecause": downgraded,
        "blocker": blocker,
    })
}

/// The final assessment when the agent never assessed.
pub(super) fn not_assessed(message_id: &str) -> Value {
    json!({
        "v": 1,
        "messageId": message_id,
        "phase": "final",
        "source": "host",
        "status": "notChecked",
        "declaredStatus": null,
        "reason": "notAssessed",
        "requirements": [],
        "downgradedBecause": [],
        "blocker": null,
    })
}

/// One requirement's check against the evidence: `(computed, because,
/// recorded predicate)`. For expect, readBack and urlMatch the predicate is
/// the one the cited action recorded, and the agent's copy of it must equal
/// it up to the recorder's grammar: see [`canonical`].
fn verify(
    check: &Value,
    message_id: &str,
    leases: &HashMap<String, Lease>,
    artifacts: &Artifacts,
) -> (&'static str, String, Option<String>) {
    let evidence = check["evidence"].as_str().unwrap_or("");
    let predicate = check["predicate"].as_str().unwrap_or("");
    let kind = check["kind"].as_str().unwrap_or("");
    if kind == "downloadType" {
        let (computed, because) = download_type(evidence, predicate, message_id, artifacts);
        return (computed, because, None);
    }
    let wanted = match kind {
        "expect" => CheckKind::Expect,
        "readBack" => CheckKind::ReadBack,
        "urlMatch" => CheckKind::UrlMatch,
        _ => {
            return (
                "claimed",
                "this kind of check cannot be verified".into(),
                None,
            )
        }
    };
    let Some(lease) = leases.get(evidence) else {
        return (
            "claimed",
            "the cited action is not one of this conversation's".into(),
            None,
        );
    };
    if lease.message_id != message_id {
        return (
            "claimed",
            "the cited action belongs to another request".into(),
            None,
        );
    }
    let Some((recorded, journaled, raw)) = &lease.check else {
        return ("claimed", "the cited action recorded no check".into(), None);
    };
    let shown = Some(journaled.clone());
    if *recorded != wanted {
        return (
            "claimed",
            "the cited action recorded another kind of check".into(),
            shown,
        );
    }
    if predicate.trim().is_empty() {
        return ("claimed", "the check was not described".into(), shown);
    }
    // Compared with the raw predicate while this service holds it, else
    // with the journaled one in the same redacted form.
    let same = match raw {
        Some(raw) => states(predicate, raw),
        None => states(&super::redact::text_for_compare(predicate), journaled),
    };
    if !same {
        return (
            "claimed",
            "the cited check records something else".into(),
            shown,
        );
    }
    match lease.verdict {
        Some(Verdict::Confirmed) => {}
        Some(Verdict::Failed) => return ("checkFailed", "the check failed".into(), shown),
        _ => {
            return (
                "claimed",
                "the check did not confirm anything".into(),
                shown,
            )
        }
    }
    if wanted == CheckKind::UrlMatch && !lease.landing.from_reply {
        return (
            "claimed",
            "the landing page was not named by the action's own reply".into(),
            shown,
        );
    }
    let recorded_because = "the browser recorded this check";
    // Fresh: no later acting action of this turn on the same page, other
    // than ones the agent declared as restoring it (#893).
    let (Some(sequence), Some(target)) = (lease.report_sequence, &lease.landing.target_id) else {
        return ("checked", recorded_because.into(), shown);
    };
    let mut later: Vec<(u64, &String, &Lease)> = leases
        .iter()
        .filter_map(|(id, other)| {
            let at = other.report_sequence.filter(|s| *s > sequence)?;
            (other.message_id == message_id
                && other.tool_class == ToolClass::Act
                && other.landing.target_id.as_ref() == Some(target))
            .then_some((at, id, other))
        })
        .collect();
    later.sort_by_key(|(at, _, _)| *at);
    // One unmarked action is enough: whatever was restored around it, the
    // page may no longer show what the check saw.
    if let Some((_, id, other)) = later.iter().find(|(_, _, other)| !other.restore) {
        return (
            "stale",
            format!(
                "a later action changed the same page: {} (action {id})",
                other.tool
            ),
            shown,
        );
    }
    if later.is_empty() {
        return ("checked", recorded_because.into(), shown);
    }
    // Counted on the agent's word that those calls only put the page back;
    // nothing measured that, so the reason says it.
    let ids: Vec<&str> = later.iter().map(|(_, id, _)| id.as_str()).collect();
    (
        "checked",
        format!(
            "{recorded_because}; {} later action(s) on the page were declared restore and not \
             verified: {}",
            ids.len(),
            ids.join(", ")
        ),
        shown,
    )
}

/// A check predicate in one spelling, so an agent that copies the recorded
/// predicate matches it and anything else does not. Only the recorder's
/// grammar before the first operator is normalised: whitespace between
/// words, the case of the leading kind and `not`, `==` glued to a selector,
/// and the operator's vocabulary (`==`, `eq`, `equals`; `contain`,
/// `includes`; `!=`, `doesn't contain`, `is not` as the recorder's leading
/// `not`). Everything after the operator is one operand, compared byte for
/// byte: only its outer whitespace is dropped, the agent may put one pair of
/// quotes around all of it (the recorder writes it as given), and a trailing
/// `(regex)` is a flag of its own.
#[derive(Debug, PartialEq)]
struct Canonical {
    negated: bool,
    prefix: Vec<String>,
    operator: Option<&'static str>,
    operand: Option<String>,
    regex: bool,
}

fn canonical(text: &str) -> Canonical {
    let mut text = text.trim();
    let mut regex = false;
    if let Some(rest) = text.strip_suffix("(regex)") {
        if rest.is_empty() || rest.ends_with(char::is_whitespace) {
            regex = true;
            text = rest.trim_end();
        }
    }
    let words = words(text);
    let mut out = Canonical {
        negated: false,
        prefix: Vec::new(),
        operator: None,
        operand: None,
        regex,
    };
    let mut i = 0;
    // The recorder's leading `not`.
    while words
        .get(i)
        .is_some_and(|(w, _)| w.eq_ignore_ascii_case("not"))
    {
        out.negated = !out.negated;
        i += 1;
    }
    if let Some((w, _)) = words.get(i) {
        if KINDS.iter().any(|k| w.eq_ignore_ascii_case(k)) {
            out.prefix.push(w.to_ascii_lowercase());
            i += 1;
        }
    }
    let lower = |k: usize| words.get(k).map(|(w, _)| w.to_lowercase());
    let mut operand_start = None;
    while i < words.len() {
        let (word, end) = words[i];
        let start = end - word.len();
        // `#total==42`: the selector, the operator, the operand's start.
        if let Some((at, op)) = ["!==", "===", "!=", "=="]
            .iter()
            .find_map(|op| word.find(op).map(|at| (at, *op)))
            .filter(|(_, op)| word != *op)
        {
            if at > 0 {
                out.prefix.push(word[..at].to_string());
            }
            let (name, inverted) = operator(op).unwrap_or(("equals", false));
            out.operator = Some(name);
            out.negated ^= inverted;
            operand_start = Some(start + at + op.len());
            break;
        }
        let this = lower(i).unwrap_or_default();
        let next = lower(i + 1);
        // "doesn't contain", "does not contain", "is not", "isn't", "not equals".
        let found = match (this.as_str(), next.as_deref()) {
            ("doesn't" | "doesnt" | "isn't" | "isnt" | "not", Some(n)) if operator(n).is_some() => {
                operator(n).map(|op| (true, op, i + 1))
            }
            ("does" | "is", Some("not")) => match lower(i + 2).as_deref().and_then(operator) {
                Some(op) => Some((true, op, i + 2)),
                None if this == "is" => Some((true, ("equals", false), i + 1)),
                None => None,
            },
            ("isn't" | "isnt", _) => Some((true, ("equals", false), i)),
            _ => operator(&this).map(|op| (false, op, i)),
        };
        match found {
            Some((flip, (name, inverted), last)) => {
                out.operator = Some(name);
                out.negated ^= flip ^ inverted;
                operand_start = Some(words[last].1);
                break;
            }
            None => out.prefix.push(word.to_string()),
        }
        i += 1;
    }
    out.operand = operand_start.map(|at| text[at..].trim().to_string());
    out
}

/// Whether the agent's predicate states the recorded one.
fn states(claimed: &str, recorded: &str) -> bool {
    let (claimed, recorded) = (canonical(claimed), canonical(recorded));
    let operand = match (&claimed.operand, &recorded.operand) {
        (Some(c), Some(r)) => c == r || unquoted(c) == r,
        (c, r) => c == r,
    };
    operand
        && claimed.negated == recorded.negated
        && claimed.prefix == recorded.prefix
        && claimed.operator == recorded.operator
        && claimed.regex == recorded.regex
}

/// Words of a predicate with the byte offset where each ends.
fn words(text: &str) -> Vec<(&str, usize)> {
    let mut out = Vec::new();
    let mut start = None;
    for (at, c) in text
        .char_indices()
        .chain(std::iter::once((text.len(), ' ')))
    {
        match (start, c.is_whitespace()) {
            (None, false) => start = Some(at),
            (Some(s), true) => {
                out.push((&text[s..at], at));
                start = None;
            }
            _ => {}
        }
    }
    out
}

/// An operand without one pair of quotes around all of it.
fn unquoted(operand: &str) -> &str {
    for (open, close) in [
        ('"', '"'),
        ('\'', '\''),
        ('`', '`'),
        ('\u{201c}', '\u{201d}'),
        ('\u{2018}', '\u{2019}'),
    ] {
        if let Some(inner) = operand
            .strip_prefix(open)
            .and_then(|rest| rest.strip_suffix(close))
        {
            return inner;
        }
    }
    operand
}

/// The expect kinds, as the recorder writes them first.
const KINDS: &[&str] = &[
    "element",
    "count",
    "text",
    "value",
    "attr",
    "url",
    "request",
    "no-errors",
];

/// An operator word in the recorder's spelling, and whether it inverts.
fn operator(word: &str) -> Option<(&'static str, bool)> {
    Some(match word {
        "==" | "===" | "eq" | "equals" | "equal" | "is" => ("equals", false),
        "!=" | "!==" | "ne" => ("equals", true),
        "contains" | "contain" | "includes" | "include" => ("contains", false),
        "matches" | "match" => ("matches", false),
        ">" => (">", false),
        "<" => ("<", false),
        ">=" => (">=", false),
        "<=" => ("<=", false),
        _ => return None,
    })
}

fn download_type(
    evidence: &str,
    predicate: &str,
    message_id: &str,
    artifacts: &Artifacts,
) -> (&'static str, String) {
    let Some(entry) = artifacts.entries.get(evidence) else {
        return (
            "claimed",
            "the cited file is not one of this conversation's".into(),
        );
    };
    if entry.message_id.as_deref() != Some(message_id) {
        return (
            "claimed",
            "the cited file belongs to another request".into(),
        );
    }
    if entry.kind != super::artifacts::Kind::Download {
        return ("claimed", "the cited file is not a download".into());
    }
    match &entry.state {
        State::Ready(info) if info.bytes == 0 => ("checkFailed", "the file is empty".into()),
        State::Ready(info) if info.type_matches != Some(true) => (
            "checkFailed",
            "the file's content does not match its extension".into(),
        ),
        State::Ready(info)
            if !predicate.trim().is_empty()
                && info.mime.as_deref().map(str::to_ascii_lowercase)
                    != Some(predicate.trim().to_ascii_lowercase()) =>
        {
            ("checkFailed", "the file is not of the stated type".into())
        }
        State::Ready(_) => ("checked", "a non-empty file of that type was saved".into()),
        State::Unavailable(_) => ("checkFailed", "the file was not kept".into()),
        _ => ("claimed", "the file is not ready".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn same(label: &str, recorded: &str) -> bool {
        states(label, recorded)
    }

    #[test]
    fn an_exact_copy_and_operator_vocabulary_match() {
        let recorded = "text #total contains 42";
        for label in [
            "text #total contains 42",
            "  text   #total contains   42 ",
            "TEXT #total CONTAINS \"42\"",
            "text #total includes '42'",
            "text #total contain `42`",
        ] {
            assert!(same(label, recorded), "{label}");
        }
        assert!(same(
            "text #title contains \"Page A\"",
            "text #title contains Page A"
        ));
        assert!(same(
            "text #title == \"Page A\"",
            "text #title equals Page A"
        ));
        assert!(same("text #title eq Page A", "text #title equals Page A"));
        assert!(same("count .item==3", "count .item == 3"));
        assert!(same("count .item eq 3", "count .item == 3"));
        // The recorder's leading `not` and the agent's negations.
        let negated = "not text #err contains Error";
        for label in [
            "not text #err contains Error",
            "NOT text #err contains \"Error\"",
            "text #err doesn't contain Error",
            "text #err does not contain Error",
            "text #err not contains Error",
        ] {
            assert!(same(label, negated), "{label}");
        }
        assert!(same("text #s != Done", "not text #s equals Done"));
        assert!(same("text #s is not Done", "not text #s equals Done"));
        assert!(same("count .x != 3", "not count .x == 3"));
        assert!(same("https://example.com/a", "https://example.com/a"));
        assert!(same(
            "#email  has the typed value",
            "#email has the typed value"
        ));
    }

    #[test]
    fn the_operand_after_the_operator_is_compared_whole() {
        // Codex round 9: an operator inside the value is part of it.
        assert!(!same("text #s equals \"a == b\"", "text #s equals a==b"));
        assert!(same("text #s equals \"a==b\"", "text #s equals a==b"));
        assert!(same(
            "text #s contains \"a==b\" (regex)",
            "text #s contains a==b (regex)"
        ));
        assert!(!same(
            "text #s contains a==b",
            "text #s contains a==b (regex)"
        ));
        // Inner whitespace is the value's own.
        assert!(!same("text #s contains \"A  B\"", "text #s contains A B"));
        assert!(same("text #s contains \"A  B\"", "text #s contains A  B"));
        assert!(same("text #s contains   A  B  ", "text #s contains A  B"));
        // Only one pair around the whole operand is dropped.
        assert!(!same(
            "text #s contains \"A\" \"B\"",
            "text #s contains A B"
        ));
        // A value that has quotes of its own: copied as is, or quoted again.
        assert!(same("text #s contains \"A\"", "text #s contains \"A\""));
        assert!(same("text #s contains '\"A\"'", "text #s contains \"A\""));
        assert!(!same("text #s contains A", "text #s contains \"A\""));
    }

    #[test]
    fn anything_else_does_not_match() {
        // Acceptance check 3 and Codex round 8's counterexamples.
        assert!(!same(
            "text #total contains 42",
            "text #title contains Page"
        ));
        assert!(!same(
            "text #status contains \"on\"",
            "text #status contains off"
        ));
        assert!(!same("contains", "text #status contains off"));
        assert!(!same("input == alice", "#email has the typed value"));
        assert!(!same("#total == 42", "text #total contains 42"));
        assert!(!same("text #total contains 42", "text #total contains 420"));
        assert!(!same(
            "text #total contains 42",
            "not text #total contains 42"
        ));
        assert!(!same(
            "text #err doesn't contain Error",
            "text #err contains Error"
        ));
        assert!(!same("text #x equals Page", "text #x contains Page"));
        assert!(!same("text #x contains page a", "text #x contains Page A"));
        assert!(!same("example.com/a", "https://example.com/a"));
        assert!(!same("text Order placed", "text #x contains Order placed"));
        // An operator word inside the value is an operand, not an operator.
        assert!(!same(
            "text #x contains \"found\"",
            "text #x contains not found"
        ));
        assert!(same(
            "text #x contains \"not found\"",
            "text #x contains not found"
        ));
    }
}
