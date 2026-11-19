//! Browser evidence for agent-panel conversations (plan section 3.2).
//!
//! In a host-bound panel conversation every chrome_use browser tool runs
//! under an action lease: the MCP asks the agent service for one before the
//! CLI runs, runs the CLI with the lease's action id and output paths, derives
//! a verdict from the native reply, and reports it. A refused or unanswered
//! lease means the tool does not run. A report that fails never fails the
//! tool; the result says the evidence was not recorded.
//!
//! Nothing here reads the panel environment by itself: callers pass a
//! [`PanelContext`], so tests drive the whole path with a fake client.

use crate::agent_panel::artifacts;
use crate::agent_panel::evidence_proto::{
    self as proto, ActionBegin, ActionReport, AllocatedOutput, ArtifactKind, ArtifactOutcome,
    Basis, BeginReply, Check, CheckKind, Deadline, DeliveryReply, DeliveryReport, DeliveryState,
    EvidenceClient, Observation, Outcome, OutputRequest, RecordingAuthorize, RecordingGrant,
    RecordingStop, ReportReply, ReportState, ReportedArtifact, TaskAssess, ToolClass, Verdict,
    VerdictReport,
};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::path::Path;
use std::sync::LazyLock;
use std::time::{Duration, Instant};

/// Who is calling and how to reach the service.
pub(crate) struct PanelContext {
    pub conversation: String,
    /// The backend process generation (`CHROME_USE_PANEL_GENERATION`). A
    /// panel MCP without one cannot take a lease.
    pub generation: Option<u64>,
    pub client: Box<dyn EvidenceClient>,
}

/// The lease context of a host-bound panel MCP server, from the environment
/// the host wrote. `None` for every other MCP client.
pub(crate) fn context() -> Option<PanelContext> {
    let (profile, conversation) = crate::agent_panel::panel_mcp_context()?;
    let generation = std::env::var(proto::GENERATION_ENV)
        .ok()
        .and_then(|v| v.parse().ok());
    Some(PanelContext {
        conversation,
        generation,
        client: Box::new(proto::ServiceClient { profile }),
    })
}

/// The acting tools that accept `observe` (plan D3).
pub(crate) const OBSERVE_TOOLS: &[&str] = &[
    "chrome_use_click",
    "chrome_use_fill",
    "chrome_use_type",
    "chrome_use_press",
    "chrome_use_select",
    "chrome_use_hover",
    "chrome_use_open",
    "chrome_use_back",
    "chrome_use_forward",
    "chrome_use_reload",
];

fn str_arg<'a>(arguments: &'a Value, key: &str) -> Option<&'a str> {
    arguments.get(key).and_then(Value::as_str)
}
fn bool_arg(arguments: &Value, key: &str) -> bool {
    arguments.get(key).and_then(Value::as_bool) == Some(true)
}

/// What a tool does to the browser. Anything not clearly a read or a capture
/// acts. `expect` and `wait` observe: they never change the page, so they do
/// not make an earlier check stale.
pub(crate) fn tool_class(tool: &str, arguments: &Value) -> ToolClass {
    match tool {
        "chrome_use_screenshot" | "chrome_use_download" | "chrome_use_download_url" => {
            ToolClass::Capture
        }
        "chrome_use_read"
        | "chrome_use_snapshot"
        | "chrome_use_extract"
        | "chrome_use_expect"
        | "chrome_use_diag_pages" => ToolClass::Observe,
        "chrome_use_wait" if !bool_arg(arguments, "download") => ToolClass::Observe,
        "chrome_use_a11y" if str_arg(arguments, "url").is_none_or(str::is_empty) => {
            ToolClass::Observe
        }
        "chrome_use_find"
            if str_arg(arguments, "locator") == Some("query")
                || matches!(str_arg(arguments, "action"), None | Some("locate" | "text")) =>
        {
            ToolClass::Observe
        }
        "chrome_use_tabs" if matches!(str_arg(arguments, "action"), Some("list" | "inspect")) => {
            ToolClass::Observe
        }
        "chrome_use_downloads" if !bool_arg(arguments, "clear") => ToolClass::Observe,
        _ => ToolClass::Act,
    }
}

/// Tools that leave the page as they found it or reach the same end state
/// when run twice, as the daemon's `safe_to_repeat` (`native/actions.rs`)
/// classifies their commands. A timeout of any other tool may have acted.
fn safe_to_repeat(tool: &str, arguments: &Value) -> bool {
    tool_class(tool, arguments) == ToolClass::Observe
        || matches!(
            tool,
            "chrome_use_fill" | "chrome_use_select" | "chrome_use_screenshot"
        )
}

/// The caller's own output path and what the lease must allocate instead,
/// as `artifacts::capture_request` decides both.
fn capture_plan(tool: &str, arguments: &Value) -> (Option<String>, Vec<OutputRequest>) {
    match artifacts::capture_request(tool, arguments) {
        Some(capture) => (
            capture.requested_destination,
            vec![OutputRequest {
                kind: wire_kind(capture.kind),
                name: capture.name,
            }],
        ),
        None => (None, Vec::new()),
    }
}

fn wire_kind(kind: artifacts::Kind) -> ArtifactKind {
    match kind {
        artifacts::Kind::Screenshot => ArtifactKind::Screenshot,
        artifacts::Kind::Download => ArtifactKind::Download,
        artifacts::Kind::Recording => ArtifactKind::Recording,
    }
}

/// The page a tool names in its own arguments: the tab it targets and the
/// URL it opens. `None` when it names neither.
pub(crate) fn requested_target(tool: &str, arguments: &Value) -> Option<Value> {
    let tab = str_arg(arguments, "tabId").filter(|t| !t.is_empty());
    let url = match tool {
        "chrome_use_open" => str_arg(arguments, "url"),
        "chrome_use_tabs" if str_arg(arguments, "action") == Some("new") => {
            str_arg(arguments, "url")
        }
        _ => None,
    }
    .filter(|u| !u.is_empty());
    (tab.is_some() || url.is_some()).then(|| json!({"tabHandle": tab, "url": url}))
}

/// A per-process key: argument digests compare equal within one MCP process
/// (one backend generation) and reveal nothing about typed values outside it,
/// where a plain hash of a short password could be reversed by guessing.
static DIGEST_KEY: LazyLock<[u8; 32]> = LazyLock::new(|| {
    let mut key = [0u8; 32];
    let _ = getrandom::getrandom(&mut key);
    key
});

/// Keyed digest of the arguments without the per-call fields.
pub(crate) fn arguments_digest(arguments: &Value) -> String {
    use hmac::Mac;
    let mut canonical = arguments.clone();
    if let Some(map) = canonical.as_object_mut() {
        map.remove("timeoutMs");
        map.remove("session");
    }
    // serde_json maps are ordered, so this serialization is canonical.
    let bytes = serde_json::to_vec(&canonical).unwrap_or_default();
    let mut mac = hmac::Hmac::<Sha256>::new_from_slice(&*DIGEST_KEY).expect("any key length");
    mac.update(&bytes);
    format!("hmac-sha256:{}", hex::encode(mac.finalize().into_bytes()))
}

fn sha256_hex(bytes: &[u8]) -> String {
    format!("sha256:{}", hex::encode(Sha256::digest(bytes)))
}

/// Remove timing fields so the same result digests the same.
fn without_timing(value: &Value) -> Value {
    match value {
        Value::Object(map) => Value::Object(
            map.iter()
                .filter(|(k, _)| !matches!(k.as_str(), "timing" | "durationMs" | "elapsedMs"))
                .map(|(k, v)| (k.clone(), without_timing(v)))
                .collect(),
        ),
        Value::Array(items) => Value::Array(items.iter().map(without_timing).collect()),
        other => other.clone(),
    }
}

/// What the MCP derived from one finished tool call.
#[derive(Debug, PartialEq)]
pub(crate) struct Derived {
    pub outcome: Outcome,
    pub error_code: Option<String>,
    pub action_context: Option<Value>,
    pub verdict: Option<VerdictReport>,
    pub observation: Option<Observation>,
    pub result_digest: Option<String>,
}

fn verdict(
    verdict: Verdict,
    basis: Basis,
    possibly_executed: bool,
    check: Option<Check>,
    detail: Option<String>,
) -> Option<VerdictReport> {
    Some(VerdictReport {
        verdict,
        basis,
        possibly_executed,
        check,
        detail,
    })
}

/// A navigation landing taken from the action's own reply, never from the
/// daemon's cached target.
fn landed_url(context: Option<&Value>) -> Option<String> {
    let after = context?.get("after")?;
    (after.get("provenance").and_then(Value::as_str) == Some("actionReply"))
        .then(|| after.get("url").and_then(Value::as_str).map(str::to_string))
        .flatten()
}
fn before_url(context: Option<&Value>) -> Option<String> {
    context?
        .get("before")?
        .get("url")
        .and_then(Value::as_str)
        .map(str::to_string)
}

/// URLs compared the way the browser would load them: a missing scheme is
/// https, host case and default ports do not matter.
fn normalize_url(raw: &str) -> Option<url::Url> {
    let raw = raw.trim();
    let parsed = url::Url::parse(raw)
        .ok()
        .filter(|u| u.has_host() || u.scheme() == "about")
        .or_else(|| url::Url::parse(&format!("https://{raw}")).ok())?;
    Some(parsed)
}

/// The expect's own arguments, as the predicate the host later compares with
/// what an assessment cites.
pub(crate) fn expect_predicate(arguments: &Value) -> String {
    let mut parts = Vec::new();
    if bool_arg(arguments, "not") {
        parts.push("not".to_string());
    }
    for key in [
        "kind",
        "selector",
        "state",
        "attr",
        "op",
        "count",
        "predicate",
        "expected",
        "filter",
        "method",
        "status",
        "type",
        "requestCount",
    ] {
        match arguments.get(key) {
            Some(Value::String(s)) => parts.push(s.clone()),
            Some(Value::Number(n)) => parts.push(n.to_string()),
            _ => {}
        }
    }
    if bool_arg(arguments, "regex") {
        parts.push("(regex)".to_string());
    }
    parts.join(" ").chars().take(500).collect()
}

/// A read-back the page masked (a password field) proves nothing.
fn masked(read_back: &str, requested: &str) -> bool {
    !read_back.is_empty()
        && read_back != requested
        && read_back.chars().all(|c| matches!(c, '•' | '*' | '●'))
}

/// Verdict rules of plan 3.2, from the native reply only. Tool-specific rules
/// run first, so a failed expect reads basis `expect`, not `toolError`.
pub(crate) fn derive(tool: &str, arguments: &Value, result: &Value) -> Derived {
    let structured = &result["structuredContent"];
    let response = structured.get("response").filter(|r| r.is_object());
    let data = response
        .and_then(|r| r.get("data"))
        .filter(|d| d.is_object());
    let is_error = result["isError"] == true;
    let timed_out = structured.get("exitCode").is_some_and(Value::is_null)
        && structured["stderr"]
            .as_str()
            .is_some_and(|s| s.contains("chrome-use command timed out after"));
    let action_context = response
        .and_then(|r| r.get("actionContext"))
        .or_else(|| data.and_then(|d| d.get("actionContext")))
        .filter(|c| c.is_object())
        .cloned();
    let error_code = response
        .and_then(|r| r.get("code"))
        .or_else(|| data.and_then(|d| d.get("code")))
        .and_then(Value::as_str)
        .map(|c| c.chars().take(80).collect());
    let class = tool_class(tool, arguments);
    let observed = data
        .and_then(|d| d.get("observed"))
        .filter(|o| o.is_object());
    let observation = observed.map(|o| Observation {
        changed: o.get("changed").and_then(Value::as_bool),
        no_progress: o.get("noProgress").is_some_and(|v| !v.is_null()),
        human_check: o.get("humanCheck").is_some_and(|v| !v.is_null()),
    });
    let outcome = if timed_out {
        Outcome::Timeout
    } else if is_error {
        Outcome::Error
    } else {
        Outcome::Ok
    };
    let result_digest = (class == ToolClass::Observe).then(|| {
        // The digest names what the page returned, so it leaves out the parts
        // that differ on every call: timings and the per-call actionContext
        // (the target it names is compared separately by the stall detector).
        let body = response
            .map(|value| {
                let mut value = without_timing(value);
                if let Value::Object(map) = &mut value {
                    map.remove("actionContext");
                }
                value
            })
            .unwrap_or_else(|| json!(structured["stdout"].as_str().unwrap_or("")));
        sha256_hex(body.to_string().as_bytes())
    });
    let derived = |verdict| Derived {
        outcome,
        error_code: error_code.clone(),
        action_context: action_context.clone(),
        verdict,
        observation: observation.clone(),
        result_digest: result_digest.clone(),
    };

    if timed_out {
        let possibly = !safe_to_repeat(tool, arguments);
        return derived(verdict(
            Verdict::Uncertain,
            Basis::None,
            possibly,
            None,
            Some("the tool timed out before chrome-use answered".into()),
        ));
    }

    // Tool-specific rules.
    match tool {
        "chrome_use_expect" => {
            let check = Some(Check {
                kind: CheckKind::Expect,
                predicate: expect_predicate(arguments),
            });
            return derived(if is_error {
                verdict(Verdict::Failed, Basis::Expect, false, check, None)
            } else {
                verdict(Verdict::Confirmed, Basis::Expect, false, check, None)
            });
        }
        "chrome_use_fill" | "chrome_use_type" if !is_error => {
            let (requested, appended) = if tool == "chrome_use_fill" {
                (str_arg(arguments, "value").unwrap_or(""), false)
            } else {
                (
                    str_arg(arguments, "text").unwrap_or(""),
                    !bool_arg(arguments, "clear"),
                )
            };
            let target = str_arg(arguments, "selector").unwrap_or("the focused element");
            let check = Some(Check {
                kind: CheckKind::ReadBack,
                predicate: format!("{target} has the typed value")
                    .chars()
                    .take(500)
                    .collect(),
            });
            // Only this boolean leaves the MCP process, never the text.
            let read_back = data.and_then(|d| d.get("readBack")).and_then(Value::as_str);
            let v = match read_back {
                Some(text) if masked(text, requested) => verdict(
                    Verdict::Uncertain,
                    Basis::None,
                    true,
                    None,
                    Some("the field's value is masked".into()),
                ),
                Some(text) if text == requested || (appended && text.ends_with(requested)) => {
                    verdict(Verdict::Confirmed, Basis::ReadBack, true, check, None)
                }
                Some(_) => verdict(
                    Verdict::Failed,
                    Basis::ReadBack,
                    true,
                    check,
                    Some("the field does not hold the typed value".into()),
                ),
                None => verdict(Verdict::Uncertain, Basis::None, true, None, None),
            };
            return derived(apply_observation(v, observation.as_ref()));
        }
        "chrome_use_open" | "chrome_use_back" | "chrome_use_forward" | "chrome_use_reload"
            if !is_error =>
        {
            let Some(landed) = landed_url(action_context.as_ref()) else {
                let v = verdict(Verdict::Uncertain, Basis::None, true, None, None);
                return derived(apply_observation(v, observation.as_ref()));
            };
            let check = Some(Check {
                kind: CheckKind::UrlMatch,
                predicate: landed.chars().take(2048).collect(),
            });
            let requested = match tool {
                "chrome_use_open" => str_arg(arguments, "url")
                    .filter(|u| !u.is_empty())
                    .map(str::to_string),
                "chrome_use_reload" => before_url(action_context.as_ref()),
                _ => None,
            };
            let landed_url = normalize_url(&landed);
            let v = match (requested.as_deref().and_then(normalize_url), landed_url) {
                (Some(asked), Some(landed)) if asked == landed => {
                    verdict(Verdict::Confirmed, Basis::Navigation, true, check, None)
                }
                (Some(asked), Some(landed)) if asked.origin() != landed.origin() => verdict(
                    Verdict::Uncertain,
                    Basis::Navigation,
                    true,
                    check,
                    Some(format!(
                        "landed on {}",
                        landed.origin().ascii_serialization()
                    )),
                ),
                (Some(_), Some(_)) => verdict(
                    Verdict::Uncertain,
                    Basis::Navigation,
                    true,
                    check,
                    Some("landed on another page of the same site".into()),
                ),
                _ => verdict(Verdict::Uncertain, Basis::Navigation, true, check, None),
            };
            return derived(apply_observation(v, observation.as_ref()));
        }
        "chrome_use_tabs"
            if !is_error
                && str_arg(arguments, "action") == Some("switch")
                && data.and_then(|d| d.get("verified")).and_then(Value::as_str)
                    == Some("confirmed") =>
        {
            return derived(verdict(
                Verdict::Confirmed,
                Basis::TabVerified,
                true,
                None,
                None,
            ));
        }
        _ => {}
    }

    // Capture tools: advisory only; the service decides after the artifact.
    if class == ToolClass::Capture {
        let basis = if tool == "chrome_use_screenshot" {
            Basis::Capture
        } else {
            Basis::Download
        };
        return derived(if is_error {
            verdict(Verdict::Failed, Basis::ToolError, true, None, None)
        } else {
            verdict(Verdict::Confirmed, basis, true, None, None)
        });
    }
    // Generic error.
    if is_error {
        return derived(verdict(
            Verdict::Failed,
            Basis::ToolError,
            class == ToolClass::Act,
            None,
            None,
        ));
    }
    if class == ToolClass::Observe {
        return derived(None);
    }
    let v = verdict(Verdict::Uncertain, Basis::None, true, None, None);
    derived(apply_observation(v, observation.as_ref()))
}

/// `observe: true` results. A page that changed is not the requested effect,
/// and a page waiting for a person confirms nothing.
fn apply_observation(
    report: Option<VerdictReport>,
    observation: Option<&Observation>,
) -> Option<VerdictReport> {
    let Some(observation) = observation else {
        return report;
    };
    let mut report = report?;
    if observation.human_check {
        report.verdict = Verdict::Uncertain;
        report.basis = Basis::None;
        report.detail = Some("the page is waiting for a person".into());
    } else if observation.changed == Some(true)
        && report.verdict == Verdict::Uncertain
        && report.basis == Basis::None
    {
        report.basis = Basis::PageChanged;
    }
    Some(report)
}

/// A tool error that never reached the browser.
fn refused(reason: &str) -> Value {
    json!({
        "content": [{"type": "text", "text": reason}],
        "structuredContent": {"chromeUseAction": {"actionId": null, "notes": [reason]}},
        "isError": true,
    })
}

/// Put the action id and every note where each backend keeps them: the first
/// content block (Codex, Pi) and `structuredContent` (Claude keeps only that).
pub(crate) fn decorate(result: &mut Value, action_id: Option<&str>, notes: &[String]) {
    if !result.is_object() {
        *result = json!({});
    }
    if !result["structuredContent"].is_object() {
        result["structuredContent"] = json!({});
    }
    result["structuredContent"]["chromeUseAction"] = json!({"actionId": action_id, "notes": notes});
    if !result["content"].is_array() {
        result["content"] = json!([]);
    }
    let content = result["content"]
        .as_array_mut()
        .expect("content is an array");
    if let Some(id) = action_id {
        content.insert(
            0,
            json!({"type": "text", "text": format!("chrome-use action {id}")}),
        );
    }
    for note in notes {
        content.push(json!({"type": "text", "text": note}));
    }
}

/// Send a report and wait out a concurrent ingest. A lost reply is sent once
/// more with the same action id; the service answers a recorded report with
/// its first reply.
fn send_report(
    client: &dyn EvidenceClient,
    report: &ActionReport,
    budget: Duration,
) -> Result<ReportReply, String> {
    let deadline = Instant::now() + budget;
    let params = proto::encode(report)?;
    let mut retried = false;
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err("the agent service did not finish recording in time".into());
        }
        match client.exchange(proto::OP_ACTION_REPORT, params.clone(), remaining) {
            Ok(reply) => {
                let reply: ReportReply = proto::decode(reply)?;
                if reply.state == Some(ReportState::Pending) {
                    std::thread::sleep(Duration::from_millis(250).min(remaining));
                    continue;
                }
                return Ok(reply);
            }
            Err(_) if !retried => retried = true,
            Err(error) => return Err(error),
        }
    }
}

const NOT_RECORDED: &str = "browser evidence was not recorded";

/// Run one browser tool under a lease. `run` receives the arguments with any
/// output path replaced by the lease's allocation and the environment the CLI
/// child must get; it is never called when the lease is refused.
pub(crate) fn run_leased<E>(
    context: &PanelContext,
    tool: &str,
    arguments: &Value,
    run: impl FnOnce(&Value, &[(&str, String)]) -> Result<Value, E>,
) -> Result<Value, E> {
    let Some(generation) = context.generation else {
        return Ok(refused(
            "This conversation's browser tools have no turn identity, so the action was not run.",
        ));
    };
    let action_id = uuid::Uuid::new_v4().to_string();
    let class = tool_class(tool, arguments);
    let (requested, outputs) = capture_plan(tool, arguments);
    let digest = arguments_digest(arguments);
    let Ok(not_after) = Deadline::after(proto::BEGIN_BUDGET) else {
        return Ok(refused(
            "This computer's boot clock is unavailable, so the action was not run.",
        ));
    };
    // `restore` on a tool that cannot restore is refused before anything
    // runs: silently dropping it would let the agent believe its earlier
    // checks still count.
    let restore = bool_arg(arguments, "restore");
    if restore && !proto::accepts_restore(tool, class) {
        return Ok(refused(
            "The action was not run: `restore` is only for a call that puts the page back as it \
             was (click, press, eval, fill, type, select, hover, scroll or an acting find). \
             Drop `restore` from this call.",
        ));
    }
    let begin = ActionBegin {
        caller_conversation: context.conversation.clone(),
        generation,
        boot_id: not_after.boot_id.clone(),
        not_after: not_after.not_after,
        action_id: action_id.clone(),
        tool: tool.to_string(),
        tool_class: class,
        arguments_digest: digest.clone(),
        requested_destination: requested.clone(),
        outputs: outputs.clone(),
        requested: requested_target(tool, arguments),
        restore,
    };
    let reply = proto::encode(&begin).and_then(|params| {
        context
            .client
            .exchange(proto::OP_ACTION_BEGIN, params, not_after.remaining())
    });
    let lease: BeginReply = match reply.and_then(proto::decode::<BeginReply>) {
        Err(error) => {
            return Ok(refused(&format!(
                "chrome-use could not record this action with the agent panel, so it was not run: {error}"
            )))
        }
        Ok(lease) if !lease.admitted => {
            let reason = lease
                .reason
                .unwrap_or_else(|| "the conversation is not running a request".into());
            return Ok(refused(&format!("The action was not run: {reason}.")));
        }
        Ok(lease) => lease,
    };
    // A reply after `notAfter` may belong to a Begin the service should have
    // refused; never act on it.
    if not_after.passed() {
        return Ok(refused(
            "The agent panel answered too late, so the action was not run.",
        ));
    }
    if !outputs_match(&outputs, &lease.outputs) {
        return Ok(refused(
            "The agent panel allocated different outputs than requested, so the action was not run.",
        ));
    }

    let effective = match lease.outputs.first() {
        Some(output) => artifacts::rewrite_capture_arguments(
            &if arguments.is_object() {
                arguments.clone()
            } else {
                json!({})
            },
            Path::new(&output.path),
        ),
        None => arguments.clone(),
    };
    let started = Instant::now();
    let environment = [(proto::ACTION_ID_ENV, action_id.clone())];
    let ran = run(&effective, &environment);
    let duration_ms = started.elapsed().as_millis() as u64;

    let artifacts: Vec<ReportedArtifact> = lease
        .outputs
        .iter()
        .map(|o| ReportedArtifact {
            artifact_id: o.artifact_id.clone(),
            path: o.path.clone(),
        })
        .collect();
    let report = |derived: Derived| ActionReport {
        caller_conversation: context.conversation.clone(),
        generation,
        action_id: action_id.clone(),
        tool: tool.to_string(),
        tool_class: class,
        arguments_digest: digest.clone(),
        outcome: derived.outcome,
        error_code: derived.error_code,
        action_context: derived.action_context,
        verdict: derived.verdict,
        observation: derived.observation,
        result_digest: derived.result_digest,
        duration_ms,
        artifacts: artifacts.clone(),
    };
    let budget = if artifacts.is_empty() {
        proto::REPORT_BUDGET
    } else {
        proto::ARTIFACT_REPORT_BUDGET
    };

    let mut result = match ran {
        Ok(result) => result,
        Err(error) => {
            // Arguments were refused, or the CLI could not start: the browser
            // was not reached. Record that; the protocol error stands.
            let not_run = Derived {
                outcome: Outcome::Error,
                error_code: Some("not_run".into()),
                action_context: None,
                verdict: verdict(
                    Verdict::Failed,
                    Basis::ToolError,
                    false,
                    None,
                    Some("the tool call was refused before chrome-use ran".into()),
                ),
                observation: None,
                result_digest: None,
            };
            let _ = send_report(&*context.client, &report(not_run), budget);
            return Err(error);
        }
    };

    let derived = derive(tool, arguments, &result);
    let check = derived.verdict.as_ref().and_then(|v| v.check.clone());
    let mut notes = Vec::new();
    let recorded = send_report(&*context.client, &report(derived), budget);
    match &recorded {
        Err(_)
        | Ok(ReportReply {
            bound: Some(false), ..
        }) => notes.push(NOT_RECORDED.to_string()),
        Ok(_) => {}
    }
    for output in &lease.outputs {
        notes.extend(after_capture(
            context,
            generation,
            &action_id,
            output,
            requested.as_deref(),
            recorded.as_ref().ok(),
        ));
    }
    decorate(&mut result, Some(&action_id), &notes);
    // The check this action recorded, for the agent to cite by action id in
    // its assessment; the host checks against this record, not a restatement.
    if let Some(check) = check {
        result["structuredContent"]["chromeUseAction"]["check"] =
            json!({"kind": check.kind, "predicate": check.predicate});
        if let Some(content) = result["content"].as_array_mut() {
            content.push(json!({"type": "text", "text": format!(
                "recorded check {}: {} (cite action {action_id})",
                serde_json::to_value(check.kind).ok().and_then(|k| k.as_str().map(String::from)).unwrap_or_default(),
                check.predicate
            )}));
        }
    }
    Ok(result)
}

fn outputs_match(requested: &[OutputRequest], allocated: &[AllocatedOutput]) -> bool {
    requested.len() == allocated.len()
        && requested.iter().zip(allocated).all(|(r, a)| {
            r.kind == a.kind && Path::new(&a.path).is_absolute() && !a.artifact_id.is_empty()
        })
}

/// The capture's outcome and the separate delivery to the caller's path
/// (plan 6.2a). Returns the notes for the agent.
fn after_capture(
    context: &PanelContext,
    generation: u64,
    action_id: &str,
    output: &AllocatedOutput,
    requested: Option<&str>,
    recorded: Option<&ReportReply>,
) -> Vec<String> {
    let state = recorded.and_then(|r| {
        r.artifacts
            .iter()
            .find(|a| a.artifact_id == output.artifact_id)
    });
    let ready = state.filter(|s| s.state == ArtifactOutcome::Ready);
    let mut notes = Vec::new();
    if let Some(s) = state.filter(|s| s.state == ArtifactOutcome::Unavailable) {
        notes.push(format!(
            "the capture was not kept ({})",
            s.reason.as_deref().unwrap_or("unavailable")
        ));
    }
    let Some(requested) = requested else {
        if ready.is_some() {
            notes.push(format!(
                "saved to {} (artifact {})",
                output.path, output.artifact_id
            ));
        }
        return notes;
    };
    let not_written = || {
        format!(
            "the requested location {requested} was not written; the capture is artifact {} at {}",
            output.artifact_id, output.path
        )
    };
    let (state, digest, detail, note) = match ready.and_then(|r| r.digest.as_deref()) {
        None => (
            DeliveryState::NotAttempted,
            None,
            Some("the capture was not ready".to_string()),
            // Nothing exists at the artifact path either: pointing there right
            // after "the capture was not kept" contradicts it (#894).
            format!("the requested location {requested} was not written; no capture was kept"),
        ),
        Some(expected) => {
            let delivery = artifacts::deliver(Path::new(&output.path), requested, expected);
            match (delivery.state, &delivery.path) {
                (artifacts::DeliveryState::Delivered, Some(path)) => (
                    DeliveryState::Delivered,
                    delivery.digest.clone(),
                    None,
                    format!("saved to {}", path.display()),
                ),
                _ => (
                    DeliveryState::Failed,
                    delivery.digest.clone(),
                    Some(delivery.detail.clone()),
                    not_written(),
                ),
            }
        }
    };
    notes.push(note);
    let delivery = DeliveryReport {
        caller_conversation: context.conversation.clone(),
        generation,
        action_id: action_id.to_string(),
        artifact_id: output.artifact_id.clone(),
        state,
        digest,
        detail: detail.map(|d| d.chars().take(1000).collect()),
    };
    let sent = proto::encode(&delivery)
        .and_then(|params| {
            context
                .client
                .exchange(proto::OP_DELIVERY_REPORT, params, proto::DELIVERY_BUDGET)
        })
        .and_then(proto::decode::<DeliveryReply>);
    // The service checks the digest itself; it may record less than claimed,
    // never more.
    if sent.is_err() {
        notes.push("the save to your path was not recorded".to_string());
    }
    notes
}

/// Shape of a panel tool's reply as an MCP result.
fn panel_result(reply: Result<Value, String>) -> Value {
    match reply {
        Ok(value) => {
            json!({"content":[{"type":"text","text":value.to_string()}],"structuredContent":value,"isError":false})
        }
        Err(error) => json!({"content":[{"type":"text","text":error}],"isError":true}),
    }
}

/// `chrome_use_task`: record the agent's assessment of the active request.
pub(crate) fn call_task(context: &PanelContext, arguments: &Value) -> Value {
    panel_result((|| {
        let generation = context
            .generation
            .ok_or("This conversation has no turn identity")?;
        let mut assessment = arguments
            .as_object()
            .cloned()
            .ok_or("chrome_use_task arguments must be an object")?;
        if assessment.remove("action").as_ref().and_then(Value::as_str) != Some("assess") {
            return Err("chrome_use_task supports only action assess".to_string());
        }
        if assessment
            .get("requirements")
            .and_then(Value::as_array)
            .is_some_and(|r| r.len() > 12)
        {
            return Err("List at most 12 requirements".to_string());
        }
        let not_after = Deadline::after(proto::BEGIN_BUDGET)?;
        let request = TaskAssess {
            caller_conversation: context.conversation.clone(),
            generation,
            boot_id: not_after.boot_id.clone(),
            not_after: not_after.not_after,
            assessment: Value::Object(assessment),
        };
        context.client.exchange(
            proto::OP_TASK_ASSESS,
            proto::encode(&request)?,
            not_after.remaining(),
        )
    })())
}

/// `chrome_use_record`: start a recording the user allowed for this message,
/// or stop it. `run` runs a CLI command (without `--json`) and returns its
/// tool result.
pub(crate) fn call_record<E>(
    context: &PanelContext,
    arguments: &Value,
    run: impl FnOnce(Vec<String>) -> Result<Value, E>,
) -> Result<Value, E> {
    let Some(generation) = context.generation else {
        return Ok(panel_result(Err(
            "This conversation has no turn identity".into()
        )));
    };
    let Ok(not_after) = Deadline::after(proto::BEGIN_BUDGET) else {
        return Ok(panel_result(Err(
            "This computer's boot clock is unavailable".into(),
        )));
    };
    match str_arg(arguments, "action") {
        Some("start") => {
            let request = RecordingAuthorize {
                caller_conversation: context.conversation.clone(),
                generation,
                boot_id: not_after.boot_id.clone(),
                not_after: not_after.not_after,
            };
            let grant = proto::encode(&request)
                .and_then(|params| {
                    context.client.exchange(
                        proto::OP_RECORDING_AUTHORIZE,
                        params,
                        not_after.remaining(),
                    )
                })
                .and_then(proto::decode::<RecordingGrant>);
            let grant = match grant {
                Ok(grant) if !not_after.passed() && Path::new(&grant.path).is_absolute() => grant,
                Ok(_) => {
                    return Ok(panel_result(Err(
                        "The recording was not started: the agent panel answered too late".into(),
                    )))
                }
                Err(error) => {
                    return Ok(panel_result(Err(format!(
                        "The recording was not started: {error}"
                    ))))
                }
            };
            let mut result = run(vec!["record".into(), "start".into(), grant.path.clone()])?;
            if result.is_object() {
                if !result["structuredContent"].is_object() {
                    result["structuredContent"] = json!({});
                }
                result["structuredContent"]["recording"] =
                    json!({"artifactId": grant.artifact_id, "path": grant.path});
                if let Some(content) = result["content"].as_array_mut() {
                    content.push(json!({"type":"text","text":format!("recording artifact {}", grant.artifact_id)}));
                }
            }
            Ok(result)
        }
        Some("stop") => {
            let request = RecordingStop {
                caller_conversation: context.conversation.clone(),
                generation,
                boot_id: not_after.boot_id.clone(),
                not_after: not_after.not_after,
                artifact_id: str_arg(arguments, "artifactId").map(str::to_string),
            };
            Ok(panel_result(proto::encode(&request).and_then(|params| {
                context.client.exchange(
                    proto::OP_RECORDING_STOP,
                    params,
                    proto::RECORDING_STOP_BUDGET,
                )
            })))
        }
        _ => Ok(panel_result(Err(
            "chrome_use_record action must be start or stop".into(),
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::rc::Rc;

    type Answers = Vec<Result<Value, String>>;

    /// A scripted service: each operation answers from its queue (the last
    /// answer repeats) and every request is kept.
    #[derive(Clone, Default)]
    struct Fake {
        calls: Rc<RefCell<Vec<(String, Value)>>>,
        answers: Rc<RefCell<Vec<(String, Answers)>>>,
    }
    impl Fake {
        fn answer(&self, operation: &str, answers: Vec<Result<Value, String>>) {
            self.answers
                .borrow_mut()
                .push((operation.to_string(), answers));
        }
        fn sent(&self, operation: &str) -> Vec<Value> {
            self.calls
                .borrow()
                .iter()
                .filter(|(op, _)| op == operation)
                .map(|(_, v)| v.clone())
                .collect()
        }
    }
    impl EvidenceClient for Fake {
        fn exchange(&self, operation: &str, params: Value, _: Duration) -> Result<Value, String> {
            self.calls
                .borrow_mut()
                .push((operation.to_string(), params));
            let mut answers = self.answers.borrow_mut();
            let Some((_, queue)) = answers.iter_mut().find(|(op, _)| op == operation) else {
                return Err("no answer scripted".into());
            };
            if queue.len() > 1 {
                queue.remove(0)
            } else {
                queue[0].clone()
            }
        }
    }
    fn context(fake: &Fake) -> PanelContext {
        PanelContext {
            conversation: "11111111-2222-4333-8444-555555555555".into(),
            generation: Some(7),
            client: Box::new(fake.clone()),
        }
    }
    fn cli(response: Value, exit: Option<i32>) -> Value {
        let success = exit == Some(0) && response["success"] != false;
        json!({
            "content": [{"type": "text", "text": response.to_string()}],
            "structuredContent": {"exitCode": exit, "stdout": response.to_string(), "stderr": "", "response": response},
            "isError": !success,
        })
    }
    fn admitted() -> Result<Value, String> {
        Ok(json!({"admitted": true, "messageId": "m1"}))
    }
    fn bound() -> Result<Value, String> {
        Ok(json!({"bound": true, "verdict": "uncertain"}))
    }
    fn reply_url(url: &str) -> Value {
        json!({"after": {"provenance": "actionReply", "url": url, "targetId": "T"}})
    }

    // --- verdict table (plan 3.2) ---

    #[test]
    fn expect_records_its_own_predicate_either_way() {
        let args = json!({"kind": "text", "selector": "#total", "predicate": "contains", "expected": "42"});
        let ok = derive(
            "chrome_use_expect",
            &args,
            &cli(json!({"success": true, "data": {}}), Some(0)),
        );
        let v = ok.verdict.unwrap();
        assert_eq!((v.verdict, v.basis), (Verdict::Confirmed, Basis::Expect));
        let check = v.check.unwrap();
        assert_eq!(check.kind, CheckKind::Expect);
        assert_eq!(check.predicate, "text #total contains 42");
        let failed = derive(
            "chrome_use_expect",
            &args,
            &cli(
                json!({"success": false, "error": "no", "code": "expect_failed"}),
                Some(1),
            ),
        );
        let v = failed.verdict.unwrap();
        assert_eq!((v.verdict, v.basis), (Verdict::Failed, Basis::Expect));
        assert_eq!(v.check.unwrap().predicate, "text #total contains 42");
        assert_eq!(failed.outcome, Outcome::Error);
        assert_eq!(failed.error_code.as_deref(), Some("expect_failed"));
        assert_eq!(tool_class("chrome_use_expect", &args), ToolClass::Observe);
    }

    #[test]
    fn read_back_is_compared_in_process_and_only_a_boolean_leaves() {
        let args = json!({"selector": "#q", "text": "s3cret-typed"});
        let typed = |read_back: Value| {
            derive(
                "chrome_use_type",
                &json!({"selector": "#q", "text": "s3cret-typed", "clear": true}),
                &cli(
                    json!({"success": true, "data": {"readBack": read_back}}),
                    Some(0),
                ),
            )
        };
        let same = typed(json!("s3cret-typed"));
        let v = same.verdict.clone().unwrap();
        assert_eq!((v.verdict, v.basis), (Verdict::Confirmed, Basis::ReadBack));
        assert_eq!(v.check.as_ref().unwrap().kind, CheckKind::ReadBack);
        assert_eq!(
            v.check.as_ref().unwrap().predicate,
            "#q has the typed value"
        );
        let wire = serde_json::to_string(&v).unwrap();
        assert!(!wire.contains("s3cret"), "{wire}");
        let different = typed(json!("other"));
        assert_eq!(different.verdict.unwrap().verdict, Verdict::Failed);
        let masked = typed(json!("••••••••••••"));
        assert_eq!(masked.verdict.unwrap().verdict, Verdict::Uncertain);
        // Without clear, typing appends to what the field held.
        let appended = derive(
            "chrome_use_type",
            &args,
            &cli(
                json!({"success": true, "data": {"readBack": "x s3cret-typed"}}),
                Some(0),
            ),
        );
        assert_eq!(appended.verdict.unwrap().verdict, Verdict::Confirmed);
        // fill replies carry no read-back today: uncertain, never confirmed.
        let fill = derive(
            "chrome_use_fill",
            &json!({"selector": "#q", "value": "v"}),
            &cli(json!({"success": true, "data": {"filled": "#q"}}), Some(0)),
        );
        let v = fill.verdict.unwrap();
        assert_eq!((v.verdict, v.basis), (Verdict::Uncertain, Basis::None));
    }

    #[test]
    fn navigation_is_confirmed_only_by_the_actions_own_reply() {
        let open = |asked: &str, context: Value| {
            derive(
                "chrome_use_open",
                &json!({"url": asked}),
                &cli(
                    json!({"success": true, "data": {"actionContext": context}}),
                    Some(0),
                ),
            )
            .verdict
            .unwrap()
        };
        let v = open("Example.com/a", reply_url("https://example.com/a"));
        assert_eq!(
            (v.verdict, v.basis),
            (Verdict::Confirmed, Basis::Navigation)
        );
        assert_eq!(v.check.unwrap().predicate, "https://example.com/a");
        let v = open(
            "https://example.com/a",
            reply_url("https://login.example.net/sso"),
        );
        assert_eq!(v.verdict, Verdict::Uncertain);
        assert_eq!(
            v.detail.as_deref(),
            Some("landed on https://login.example.net")
        );
        assert_eq!(v.check.unwrap().kind, CheckKind::UrlMatch);
        let daemon =
            json!({"after": {"provenance": "daemonState", "url": "https://example.com/a"}});
        let v = open("https://example.com/a", daemon);
        assert_eq!((v.verdict, v.basis), (Verdict::Uncertain, Basis::None));
        assert!(v.check.is_none());
        // The context may also sit at the top of the reply.
        let top = derive(
            "chrome_use_reload",
            &json!({}),
            &cli(
                json!({"success": true, "data": {}, "actionContext": {"before": {"url": "https://a.test/"}, "after": {"provenance": "actionReply", "url": "https://a.test/"}}}),
                Some(0),
            ),
        );
        assert_eq!(top.verdict.unwrap().verdict, Verdict::Confirmed);
    }

    #[test]
    fn tab_switch_confirms_only_when_verified() {
        let args = json!({"action": "switch", "tabId": "t2"});
        let ok = derive(
            "chrome_use_tabs",
            &args,
            &cli(
                json!({"success": true, "data": {"verified": "confirmed"}}),
                Some(0),
            ),
        );
        assert_eq!(ok.verdict.unwrap().basis, Basis::TabVerified);
        let unconfirmed = derive(
            "chrome_use_tabs",
            &args,
            &cli(
                json!({"success": true, "data": {"verified": "unconfirmed"}}),
                Some(0),
            ),
        );
        assert_eq!(unconfirmed.verdict.unwrap().verdict, Verdict::Uncertain);
    }

    #[test]
    fn captures_are_advisory_and_errors_fail() {
        let shot = derive(
            "chrome_use_screenshot",
            &json!({}),
            &cli(
                json!({"success": true, "data": {"path": "/x.png"}}),
                Some(0),
            ),
        );
        assert_eq!(shot.verdict.unwrap().basis, Basis::Capture);
        let download = derive(
            "chrome_use_download_url",
            &json!({"url": "https://a.test/f.pdf"}),
            &cli(
                json!({"success": false, "error": "downloadUrl: interrupted", "code": "download_interrupted"}),
                Some(1),
            ),
        );
        let v = download.verdict.unwrap();
        assert_eq!((v.verdict, v.basis), (Verdict::Failed, Basis::ToolError));
        assert_eq!(download.error_code.as_deref(), Some("download_interrupted"));
    }

    #[test]
    fn timeouts_may_have_acted_unless_repeatable() {
        let timed_out = |tool: &str, args: Value| {
            let result = json!({
                "content": [], "isError": true,
                "structuredContent": {"exitCode": null, "stdout": "", "stderr": "chrome-use command timed out after 30000ms", "response": null},
            });
            derive(tool, &args, &result)
        };
        let click = timed_out("chrome_use_click", json!({"selector": "#pay"}));
        assert_eq!(click.outcome, Outcome::Timeout);
        let v = click.verdict.unwrap();
        assert!(v.possibly_executed && v.verdict == Verdict::Uncertain);
        let read = timed_out("chrome_use_read", json!({}));
        assert!(!read.verdict.unwrap().possibly_executed);
        let fill = timed_out("chrome_use_fill", json!({"selector": "#a", "value": "b"}));
        assert!(!fill.verdict.unwrap().possibly_executed);
    }

    #[test]
    fn observe_results_never_confirm_an_effect() {
        let click = |observed: Value| {
            derive(
                "chrome_use_click",
                &json!({"selector": "#go", "observe": true}),
                &cli(
                    json!({"success": true, "data": {"observed": observed}}),
                    Some(0),
                ),
            )
        };
        let changed = click(json!({"changed": true}));
        let v = changed.verdict.unwrap();
        assert_eq!(
            (v.verdict, v.basis),
            (Verdict::Uncertain, Basis::PageChanged)
        );
        assert!(v.possibly_executed);
        let stalled = click(json!({"changed": false, "noProgress": {"count": 3}}));
        assert!(stalled.observation.unwrap().no_progress);
        let human = click(json!({"changed": true, "humanCheck": {"kind": "captcha"}}));
        assert!(human.observation.as_ref().unwrap().human_check);
        assert_eq!(human.verdict.unwrap().verdict, Verdict::Uncertain);
        let plain = click(Value::Null);
        assert!(plain.observation.is_none());
        let v = plain.verdict.unwrap();
        assert_eq!((v.verdict, v.basis), (Verdict::Uncertain, Basis::None));
    }

    #[test]
    fn reads_get_a_stable_digest_and_no_verdict() {
        let read = |ms: u64| {
            derive(
                "chrome_use_read",
                &json!({}),
                &cli(
                    json!({"success": true, "data": {"text": "hi"}, "timing": {"ms": ms}}),
                    Some(0),
                ),
            )
        };
        let (a, b) = (read(3), read(90));
        // Real replies carry a per-call actionContext; it must not change the digest.
        let stamped = |id: &str| {
            derive(
                "chrome_use_read",
                &json!({}),
                &cli(
                    json!({"success": true, "data": {"text": "hi"}, "timing": {"ms": 5},
                        "actionContext": {"actionId": id, "before": null, "after": {"targetId": "T", "url": "https://x.test/"}}}),
                    Some(0),
                ),
            )
        };
        assert_eq!(
            stamped("11111111-1111-4111-8111-111111111111").result_digest,
            a.result_digest
        );
        assert_eq!(
            stamped("22222222-2222-4222-8222-222222222222").result_digest,
            a.result_digest
        );
        assert!(a.verdict.is_none());
        assert!(a.result_digest.as_deref().unwrap().starts_with("sha256:"));
        assert_eq!(a.result_digest, b.result_digest);
        let failed = derive(
            "chrome_use_read",
            &json!({}),
            &cli(json!({"success": false, "error": "x"}), Some(1)),
        );
        let v = failed.verdict.unwrap();
        assert_eq!(
            (v.verdict, v.basis, v.possibly_executed),
            (Verdict::Failed, Basis::ToolError, false)
        );
    }

    #[test]
    fn classes_and_digests() {
        assert_eq!(
            tool_class(
                "chrome_use_find",
                &json!({"locator": "role", "value": "button", "action": "click"})
            ),
            ToolClass::Act
        );
        assert_eq!(
            tool_class(
                "chrome_use_find",
                &json!({"locator": "role", "value": "button"})
            ),
            ToolClass::Observe
        );
        assert_eq!(
            tool_class("chrome_use_tabs", &json!({"action": "close"})),
            ToolClass::Act
        );
        assert_eq!(
            tool_class("chrome_use_downloads", &json!({"clear": true})),
            ToolClass::Act
        );
        assert_eq!(
            tool_class("chrome_use_site", &json!({"spec": "a/b"})),
            ToolClass::Act
        );
        let a = arguments_digest(&json!({"value": "pw", "timeoutMs": 5}));
        assert_eq!(a, arguments_digest(&json!({"value": "pw"})));
        assert_ne!(a, arguments_digest(&json!({"value": "pw2"})));
        assert_ne!(
            a,
            sha256_hex(br#"{"value":"pw"}"#).replace("sha256", "hmac-sha256")
        );
    }

    // --- lease and report path ---

    #[test]
    fn a_refused_lease_never_runs_the_cli() {
        for answer in [
            Ok(json!({"admitted": false, "reason": "the user stopped this request"})),
            Err("Agent service did not answer".to_string()),
        ] {
            let fake = Fake::default();
            fake.answer(proto::OP_ACTION_BEGIN, vec![answer]);
            let mut ran = false;
            let result = run_leased::<()>(
                &context(&fake),
                "chrome_use_click",
                &json!({"selector": "#pay"}),
                |_, _| {
                    ran = true;
                    Ok(json!({}))
                },
            )
            .unwrap();
            assert!(!ran);
            assert_eq!(result["isError"], true);
            let notes = &result["structuredContent"]["chromeUseAction"]["notes"];
            assert!(notes[0].as_str().unwrap().contains("not run"), "{notes}");
            assert!(fake.sent(proto::OP_ACTION_REPORT).is_empty());
        }
        // No generation in the environment: no lease is even asked for.
        let fake = Fake::default();
        let mut panel = context(&fake);
        panel.generation = None;
        let mut ran = false;
        run_leased::<()>(&panel, "chrome_use_click", &json!({}), |_, _| {
            ran = true;
            Ok(json!({}))
        })
        .unwrap();
        assert!(!ran && fake.sent(proto::OP_ACTION_BEGIN).is_empty());
    }

    #[test]
    fn a_recorded_check_is_returned_for_the_agent_to_cite() {
        let fake = Fake::default();
        fake.answer(proto::OP_ACTION_BEGIN, vec![admitted()]);
        fake.answer(proto::OP_ACTION_REPORT, vec![bound()]);
        let result = run_leased::<()>(
            &context(&fake),
            "chrome_use_expect",
            &json!({"kind": "text", "selector": "#total", "predicate": "contains", "expected": "42"}),
            |_, _| Ok(cli(json!({"success": true, "data": {}}), Some(0))),
        )
        .unwrap();
        let action = &result["structuredContent"]["chromeUseAction"];
        assert_eq!(
            action["check"],
            json!({"kind": "expect", "predicate": "text #total contains 42"})
        );
        let id = action["actionId"].as_str().unwrap();
        assert!(result["content"].as_array().unwrap().iter().any(|c| {
            c["text"]
                == format!("recorded check expect: text #total contains 42 (cite action {id})")
        }));
        // An action without a check carries none.
        let fake = Fake::default();
        fake.answer(proto::OP_ACTION_BEGIN, vec![admitted()]);
        fake.answer(proto::OP_ACTION_REPORT, vec![bound()]);
        let click = run_leased::<()>(
            &context(&fake),
            "chrome_use_click",
            &json!({"selector": "#go"}),
            |_, _| Ok(cli(json!({"success": true, "data": {}}), Some(0))),
        )
        .unwrap();
        assert!(click["structuredContent"]["chromeUseAction"]
            .get("check")
            .is_none());
    }

    #[test]
    fn a_leased_call_carries_the_id_and_reports_once() {
        let fake = Fake::default();
        fake.answer(proto::OP_ACTION_BEGIN, vec![admitted()]);
        fake.answer(proto::OP_ACTION_REPORT, vec![bound()]);
        let mut seen_env = Vec::new();
        let result = run_leased::<()>(
            &context(&fake),
            "chrome_use_click",
            &json!({"selector": "#go", "session": "s"}),
            |args, env| {
                seen_env = env
                    .iter()
                    .map(|(k, v)| (k.to_string(), v.clone()))
                    .collect();
                assert_eq!(args["selector"], "#go");
                Ok(cli(json!({"success": true, "data": {}}), Some(0)))
            },
        )
        .unwrap();
        let begin = &fake.sent(proto::OP_ACTION_BEGIN)[0];
        let id = begin["actionId"].as_str().unwrap();
        assert!(uuid::Uuid::parse_str(id).is_ok());
        assert_eq!(begin["generation"], 7);
        assert_eq!(begin["toolClass"], "act");
        assert!(begin["argumentsDigest"]
            .as_str()
            .unwrap()
            .starts_with("hmac-sha256:"));
        assert_eq!(
            seen_env,
            vec![(proto::ACTION_ID_ENV.to_string(), id.to_string())]
        );
        assert_eq!(
            result["content"][0]["text"],
            format!("chrome-use action {id}")
        );
        assert_eq!(
            result["structuredContent"]["chromeUseAction"]["actionId"],
            id
        );
        assert_eq!(result["structuredContent"]["exitCode"], 0);
        assert_eq!(result["isError"], false);
        let reports = fake.sent(proto::OP_ACTION_REPORT);
        assert_eq!(reports.len(), 1);
        assert_eq!(reports[0]["actionId"], id);
        assert_eq!(reports[0]["verdict"]["verdict"], "uncertain");
    }

    #[test]
    fn a_failed_report_leaves_the_tool_successful_with_a_note() {
        let fake = Fake::default();
        fake.answer(proto::OP_ACTION_BEGIN, vec![admitted()]);
        fake.answer(proto::OP_ACTION_REPORT, vec![Err("reply lost".into())]);
        let result = run_leased::<()>(&context(&fake), "chrome_use_read", &json!({}), |_, _| {
            Ok(cli(
                json!({"success": true, "data": {"text": "page"}}),
                Some(0),
            ))
        })
        .unwrap();
        assert_eq!(result["isError"], false);
        // Sent once, retried once with the same action id.
        let reports = fake.sent(proto::OP_ACTION_REPORT);
        assert_eq!(reports.len(), 2);
        assert_eq!(reports[0]["actionId"], reports[1]["actionId"]);
        assert_eq!(
            result["structuredContent"]["chromeUseAction"]["notes"],
            json!([NOT_RECORDED])
        );
        assert_eq!(
            result["structuredContent"]["stdout"],
            json!({"success": true, "data": {"text": "page"}}).to_string()
        );
        let texts: Vec<&str> = result["content"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|b| b["text"].as_str())
            .collect();
        assert_eq!(texts.last(), Some(&NOT_RECORDED));
    }

    #[test]
    fn a_pending_report_is_polled_until_answered() {
        let fake = Fake::default();
        fake.answer(proto::OP_ACTION_BEGIN, vec![admitted()]);
        fake.answer(
            proto::OP_ACTION_REPORT,
            vec![Ok(json!({"state": "pending"})), bound()],
        );
        let result = run_leased::<()>(
            &context(&fake),
            "chrome_use_click",
            &json!({"selector": "#a"}),
            |_, _| Ok(cli(json!({"success": true, "data": {}}), Some(0))),
        )
        .unwrap();
        assert_eq!(fake.sent(proto::OP_ACTION_REPORT).len(), 2);
        assert_eq!(
            result["structuredContent"]["chromeUseAction"]["notes"],
            json!([])
        );
    }

    #[test]
    fn refused_arguments_are_reported_as_not_run() {
        let fake = Fake::default();
        fake.answer(proto::OP_ACTION_BEGIN, vec![admitted()]);
        fake.answer(proto::OP_ACTION_REPORT, vec![bound()]);
        let error = run_leased(&context(&fake), "chrome_use_click", &json!({}), |_, _| {
            Err::<Value, _>("selector required")
        })
        .unwrap_err();
        assert_eq!(error, "selector required");
        let report = &fake.sent(proto::OP_ACTION_REPORT)[0];
        assert_eq!(report["verdict"]["possiblyExecuted"], false);
        assert_eq!(report["errorCode"], "not_run");
    }

    /// #893: `restore: true` travels with the Begin of an in-place acting
    /// call, is left off the wire otherwise, and stops a tool that cannot
    /// restore before anything is sent or run.
    #[test]
    fn restore_is_sent_at_begin_and_refused_where_it_cannot_apply() {
        let fake = Fake::default();
        fake.answer(proto::OP_ACTION_BEGIN, vec![admitted(), admitted()]);
        fake.answer(proto::OP_ACTION_REPORT, vec![bound(), bound()]);
        let ran = |arguments: Value| {
            run_leased::<()>(&context(&fake), "chrome_use_eval", &arguments, |_, _| {
                Ok(cli(
                    json!({"success": true, "data": {"result": 1}}),
                    Some(0),
                ))
            })
            .unwrap()
        };
        ran(json!({"script": "closeDrawer()", "restore": true}));
        ran(json!({"script": "1"}));
        let begins = fake.sent(proto::OP_ACTION_BEGIN);
        assert_eq!(begins[0]["restore"], true);
        assert!(begins[1].get("restore").is_none(), "{}", begins[1]);

        for (tool, arguments) in [
            (
                "chrome_use_open",
                json!({"url": "https://example.test/", "restore": true}),
            ),
            ("chrome_use_reload", json!({"restore": true})),
            ("chrome_use_expect", json!({"text": "x", "restore": true})),
        ] {
            let fake = Fake::default();
            let result = run_leased::<()>(&context(&fake), tool, &arguments, |_, _| {
                panic!("{tool} must not run with restore")
            })
            .unwrap();
            assert_eq!(result["isError"], true, "{tool}");
            assert!(
                result["content"][0]["text"]
                    .as_str()
                    .unwrap()
                    .contains("`restore` is only for"),
                "{result}"
            );
            assert!(fake.sent(proto::OP_ACTION_BEGIN).is_empty(), "{tool}");
        }
    }

    #[test]
    fn captures_go_to_the_lease_path_and_are_delivered_separately() {
        let folder = tempfile::tempdir().unwrap();
        let capture = folder.path().join("out").join("shot.png");
        std::fs::create_dir_all(capture.parent().unwrap()).unwrap();
        let requested = folder.path().join("mine").join("shot.png");
        let bytes = b"\x89PNG\r\n\x1a\nfake";
        let fake = Fake::default();
        fake.answer(
            proto::OP_ACTION_BEGIN,
            vec![Ok(json!({"admitted": true, "messageId": "m1", "outputs": [{"artifactId": "art1", "kind": "screenshot", "name": "shot.png", "path": capture}]}))],
        );
        fake.answer(
            proto::OP_ACTION_REPORT,
            vec![Ok(json!({"bound": true, "verdict": "confirmed", "artifacts": [{"artifactId": "art1", "state": "ready", "digest": sha256_hex(bytes), "bytes": bytes.len()}]}))],
        );
        fake.answer(
            proto::OP_DELIVERY_REPORT,
            vec![Ok(json!({"state": "delivered"}))],
        );
        let result = run_leased::<()>(
            &context(&fake),
            "chrome_use_screenshot",
            &json!({"path": requested}),
            |args, _| {
                assert_eq!(args["path"], json!(capture));
                std::fs::write(&capture, bytes).unwrap();
                Ok(cli(
                    json!({"success": true, "data": {"path": capture}}),
                    Some(0),
                ))
            },
        )
        .unwrap();
        let begin = &fake.sent(proto::OP_ACTION_BEGIN)[0];
        assert_eq!(begin["requestedDestination"], json!(requested));
        assert_eq!(
            begin["outputs"],
            json!([{"kind": "screenshot", "name": "shot.png"}])
        );
        assert_eq!(
            fake.sent(proto::OP_ACTION_REPORT)[0]["artifacts"],
            json!([{"artifactId": "art1", "path": capture}])
        );
        assert_eq!(std::fs::read(&requested).unwrap(), bytes);
        let delivery = &fake.sent(proto::OP_DELIVERY_REPORT)[0];
        assert_eq!(delivery["state"], "delivered");
        assert_eq!(delivery["digest"], sha256_hex(bytes));
        let notes = &result["structuredContent"]["chromeUseAction"]["notes"];
        // The folder is resolved before writing (`/var` → `/private/var`).
        let saved = requested
            .parent()
            .unwrap()
            .canonicalize()
            .unwrap()
            .join("shot.png");
        assert_eq!(notes[0], format!("saved to {}", saved.display()));
    }

    #[test]
    fn an_unready_capture_is_never_said_to_be_saved() {
        let folder = tempfile::tempdir().unwrap();
        let capture = folder.path().join("report.pdf");
        let requested = folder.path().join("mine.pdf");
        let fake = Fake::default();
        fake.answer(
            proto::OP_ACTION_BEGIN,
            vec![Ok(json!({"admitted": true, "outputs": [{"artifactId": "art2", "kind": "download", "name": "mine.pdf", "path": capture}]}))],
        );
        fake.answer(
            proto::OP_ACTION_REPORT,
            vec![Ok(json!({"bound": true, "verdict": "failed", "artifacts": [{"artifactId": "art2", "state": "unavailable", "reason": "interrupted"}]}))],
        );
        fake.answer(proto::OP_DELIVERY_REPORT, vec![Err("down".into())]);
        let result = run_leased::<()>(
            &context(&fake),
            "chrome_use_download_url",
            &json!({"url": "https://a.test/r.pdf", "path": requested}),
            |_, _| Ok(cli(json!({"success": false, "error": "interrupted", "code": "download_interrupted"}), Some(1))),
        )
        .unwrap();
        assert!(!requested.exists());
        let notes: Vec<String> =
            serde_json::from_value(result["structuredContent"]["chromeUseAction"]["notes"].clone())
                .unwrap();
        assert_eq!(notes[0], "the capture was not kept (interrupted)");
        assert!(notes[1].starts_with(&format!(
            "the requested location {} was not written",
            requested.display()
        )));
        assert_eq!(notes[2], "the save to your path was not recorded");
        assert_eq!(
            fake.sent(proto::OP_DELIVERY_REPORT)[0]["state"],
            "notAttempted"
        );
        assert!(!notes.iter().any(|n| n.starts_with("saved to")));
    }

    #[test]
    fn begin_names_the_page_the_tool_asks_for() {
        assert_eq!(
            requested_target(
                "chrome_use_open",
                &json!({"url": "https://a.test/x", "tabId": "t1"})
            ),
            Some(json!({"tabHandle": "t1", "url": "https://a.test/x"}))
        );
        assert_eq!(
            requested_target(
                "chrome_use_tabs",
                &json!({"action": "new", "url": "https://a.test/"})
            )
            .unwrap()["url"],
            "https://a.test/"
        );
        assert_eq!(
            requested_target(
                "chrome_use_tabs",
                &json!({"action": "switch", "tabId": "chrome-tab:4"})
            )
            .unwrap(),
            json!({"tabHandle": "chrome-tab:4", "url": null})
        );
        assert!(requested_target("chrome_use_click", &json!({"selector": "#a"})).is_none());
        // A download URL is a file, not the page the action lands on.
        assert!(requested_target(
            "chrome_use_download_url",
            &json!({"url": "https://a.test/f.pdf"})
        )
        .is_none());
    }

    #[test]
    fn download_url_without_a_path_is_captured_under_its_url_name() {
        let (requested, outputs) = capture_plan(
            "chrome_use_download_url",
            &json!({"url": "https://a.test/files/q3.csv?x=1"}),
        );
        assert!(requested.is_none());
        assert_eq!(outputs[0].name, "q3.csv");
        let (_, outputs) = capture_plan(
            "chrome_use_download",
            &json!({"selector": "#d", "path": "../x/../evil/"}),
        );
        assert_eq!(outputs[0].name, "download");
    }

    #[test]
    fn assess_and_record_carry_the_lease_context() {
        let fake = Fake::default();
        fake.answer(
            proto::OP_TASK_ASSESS,
            vec![Ok(json!({"phase": "provisional", "status": "notChecked"}))],
        );
        let result = call_task(
            &context(&fake),
            &json!({"action": "assess", "status": "complete", "requirements": [{"text": "pay", "status": "met"}]}),
        );
        assert_eq!(result["isError"], false);
        let sent = &fake.sent(proto::OP_TASK_ASSESS)[0];
        assert_eq!(sent["generation"], 7);
        assert!(sent["notAfter"].is_u64() && sent["bootId"].is_string());
        assert!(sent["assessment"].get("action").is_none());
        assert_eq!(sent["assessment"]["requirements"][0]["text"], "pay");
        let too_many = json!({"action": "assess", "status": "complete", "requirements": vec![json!({"text": "x", "status": "met"}); 13]});
        assert_eq!(call_task(&context(&fake), &too_many)["isError"], true);

        fake.answer(
            proto::OP_RECORDING_AUTHORIZE,
            vec![Err("this message did not ask for a recording".into())],
        );
        let mut ran = false;
        let refused = call_record::<()>(&context(&fake), &json!({"action": "start"}), |_| {
            ran = true;
            Ok(json!({}))
        })
        .unwrap();
        assert!(!ran && refused["isError"] == true);
        let fake = Fake::default();
        // The grant must be absolute on this platform; `/w/...` is not on Windows.
        let path = std::env::temp_dir()
            .join(".chrome-use-out/rec1/recording.webm")
            .display()
            .to_string();
        fake.answer(
            proto::OP_RECORDING_AUTHORIZE,
            vec![Ok(json!({"artifactId": "rec1", "path": &path}))],
        );
        let started = call_record::<()>(&context(&fake), &json!({"action": "start"}), |argv| {
            assert_eq!(argv, ["record", "start", path.as_str()]);
            Ok(cli(json!({"success": true, "data": {}}), Some(0)))
        })
        .unwrap();
        assert_eq!(
            started["structuredContent"]["recording"]["artifactId"],
            "rec1"
        );
    }
}
