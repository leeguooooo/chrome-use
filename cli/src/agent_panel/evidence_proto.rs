//! Wire types of the panel evidence side channel (plan sections 3.2, 3.3).
//!
//! The chrome_use MCP server, running inside a panel conversation, asks the
//! agent service for an action lease before a browser tool runs and reports
//! what the tool did afterwards. Both ends compile against these types: the
//! MCP side builds the requests, the service side deserializes them and
//! answers with the reply types. Field names are the camelCase wire names.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::time::Duration;

pub(crate) const OP_ACTION_BEGIN: &str = "agentActionBegin";
pub(crate) const OP_ACTION_REPORT: &str = "agentActionReport";
pub(crate) const OP_DELIVERY_REPORT: &str = "agentDeliveryReport";
pub(crate) const OP_TASK_ASSESS: &str = "agentTaskAssess";
pub(crate) const OP_RECORDING_AUTHORIZE: &str = "agentRecordingAuthorize";
pub(crate) const OP_RECORDING_STOP: &str = "agentRecordingStop";

/// Every operation of the side channel. A tool call reaches only an already
/// running service; none of these may start one.
pub(crate) const OPERATIONS: &[&str] = &[
    OP_ACTION_BEGIN,
    OP_ACTION_REPORT,
    OP_DELIVERY_REPORT,
    OP_TASK_ASSESS,
    OP_RECORDING_AUTHORIZE,
    OP_RECORDING_STOP,
];

/// A Begin, an assess and a recording authorize carry `notAfter` = send time
/// plus this; the MCP gives up on the reply at the same moment (plan D1a 4).
pub(crate) const BEGIN_BUDGET: Duration = Duration::from_secs(5);
/// How far ahead of the service's clock a `notAfter` may lie: the budget plus
/// 250 ms of skew. Anything further is refused, so a caller cannot hand in an
/// arbitrarily distant deadline.
// Read by the service's admission handlers (Deadline::admit).
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) const NOT_AFTER_CAP: Duration = Duration::from_millis(5_250);
pub(crate) const REPORT_BUDGET: Duration = Duration::from_secs(5);
pub(crate) const ARTIFACT_REPORT_BUDGET: Duration = Duration::from_secs(60);
pub(crate) const DELIVERY_BUDGET: Duration = Duration::from_secs(5);
/// How long the MCP waits for a recording stop and its ingest.
pub(crate) const RECORDING_STOP_BUDGET: Duration = Duration::from_secs(20);

/// Environment the backend sets per process (`backend.rs` `drive`); the MCP
/// server inherits it and sends it with every lease request.
pub(crate) const GENERATION_ENV: &str = "CHROME_USE_PANEL_GENERATION";
/// Environment the MCP sets on the CLI child it runs for a leased action.
pub(crate) const ACTION_ID_ENV: &str = "CHROME_USE_PANEL_ACTION_ID";

/// What a chrome_use tool does to the browser, as the lease and the turn
/// record it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) enum ToolClass {
    Act,
    Observe,
    Capture,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) enum ArtifactKind {
    Screenshot,
    Download,
    Recording,
}

/// A deadline on the boot clock. It is sent as the request's `bootId` and
/// `notAfter` and used for nothing else: never stored, never compared across
/// boots, never used as proof of anything.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Deadline {
    pub boot_id: String,
    pub not_after: u64,
}

impl Deadline {
    /// Now plus `budget` on the boot clock.
    pub(crate) fn after(budget: Duration) -> Result<Self, String> {
        let now = monotonic_ms()?;
        Ok(Self {
            boot_id: boot_id().ok_or("This system does not identify its boot")?,
            not_after: now.saturating_add(budget.as_millis() as u64),
        })
    }
    /// Whether the deadline has passed. A deadline from another boot has.
    pub(crate) fn passed(&self) -> bool {
        Some(&self.boot_id) != boot_id().as_ref()
            || monotonic_ms().map_or(true, |now| now > self.not_after)
    }
    pub(crate) fn remaining(&self) -> Duration {
        if self.passed() {
            return Duration::ZERO;
        }
        monotonic_ms().map_or(Duration::ZERO, |now| {
            Duration::from_millis(self.not_after.saturating_sub(now))
        })
    }
    /// The service's admission check (plan D1a rule 4): this boot, not
    /// passed, and not beyond [`NOT_AFTER_CAP`]. The error is worded for the
    /// agent.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn admit(boot: &str, not_after: u64) -> Result<(), &'static str> {
        let now = monotonic_ms().map_err(|_| "the host clock is unavailable")?;
        if boot_id().as_deref() != Some(boot) {
            return Err("the request came from another boot of this computer");
        }
        if now > not_after {
            return Err("the request arrived after its deadline");
        }
        if not_after - now > NOT_AFTER_CAP.as_millis() as u64 {
            return Err("the request's deadline is too far ahead");
        }
        Ok(())
    }
}

/// Milliseconds on a clock shared by every process of this boot:
/// `CLOCK_MONOTONIC` on Unix, `QueryUnbiasedInterruptTime` on Windows.
pub(crate) fn monotonic_ms() -> Result<u64, String> {
    #[cfg(unix)]
    {
        let mut now = libc::timespec {
            tv_sec: 0,
            tv_nsec: 0,
        };
        if unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut now) } != 0 {
            return Err("The monotonic clock is unavailable".into());
        }
        Ok(now.tv_sec as u64 * 1000 + now.tv_nsec as u64 / 1_000_000)
    }
    #[cfg(windows)]
    {
        #[link(name = "kernel32")]
        extern "system" {
            fn QueryUnbiasedInterruptTime(time: *mut u64) -> i32;
        }
        let mut ticks = 0u64;
        if unsafe { QueryUnbiasedInterruptTime(&mut ticks) } == 0 {
            return Err("The monotonic clock is unavailable".into());
        }
        // 100 ns units.
        Ok(ticks / 10_000)
    }
}

/// The kernel's identity of this boot, where it offers one.
pub(crate) fn boot_id() -> Option<String> {
    #[cfg(target_os = "linux")]
    {
        std::fs::read_to_string("/proc/sys/kernel/random/boot_id")
            .ok()
            .map(|id| id.trim().to_string())
            .filter(|id| !id.is_empty())
    }
    #[cfg(target_os = "macos")]
    {
        let name = c"kern.bootsessionuuid";
        let mut buffer = [0u8; 64];
        let mut length = buffer.len();
        let status = unsafe {
            libc::sysctlbyname(
                name.as_ptr(),
                buffer.as_mut_ptr().cast(),
                &mut length,
                std::ptr::null_mut(),
                0,
            )
        };
        if status != 0 || length == 0 {
            return None;
        }
        let end = buffer[..length]
            .iter()
            .position(|b| *b == 0)
            .unwrap_or(length);
        std::str::from_utf8(&buffer[..end])
            .ok()
            .map(str::to_string)
            .filter(|id| !id.is_empty())
    }
    #[cfg(windows)]
    {
        // SYSTEM_TIMEOFDAY_INFORMATION: BootTime is its first field, in
        // 100 ns units since 1601; it stays the same for the whole boot.
        #[link(name = "ntdll")]
        extern "system" {
            fn NtQuerySystemInformation(
                class: u32,
                information: *mut core::ffi::c_void,
                length: u32,
                returned: *mut u32,
            ) -> i32;
        }
        const SYSTEM_TIME_OF_DAY_INFORMATION: u32 = 3;
        let mut information = [0u64; 6];
        let mut returned = 0u32;
        let status = unsafe {
            NtQuerySystemInformation(
                SYSTEM_TIME_OF_DAY_INFORMATION,
                information.as_mut_ptr().cast(),
                std::mem::size_of_val(&information) as u32,
                &mut returned,
            )
        };
        (status >= 0 && information[0] != 0).then(|| format!("boot-{}", information[0]))
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
    {
        None
    }
}

/// An output the lease must allocate before the tool runs (section 6.1).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct OutputRequest {
    pub kind: ArtifactKind,
    /// File name only, no directory part.
    pub name: String,
}

/// `agentActionBegin`. `callerConversation` is added by the client from the
/// host-set environment, never from tool arguments.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct ActionBegin {
    pub caller_conversation: String,
    pub generation: u64,
    pub boot_id: String,
    /// Milliseconds on the boot's monotonic clock.
    pub not_after: u64,
    pub action_id: String,
    pub tool: String,
    pub tool_class: ToolClass,
    /// `sha256:<hex>` of the canonical arguments. Identifies the call without
    /// carrying typed values to the service.
    pub arguments_digest: String,
    /// The caller's own output path (`path` of download, download_url,
    /// screenshot), as the agent wrote it. Redacted by the service before it
    /// is journaled.
    pub requested_destination: Option<String>,
    pub outputs: Vec<OutputRequest>,
    /// The page the tool names: `{tabHandle, url}` from its own arguments
    /// (a tab id, the URL it opens), when it names one. Journaled as
    /// `browserActionTarget.requested` so the panel can say "asked for X,
    /// landed on Y".
    #[serde(default)]
    pub requested: Option<Value>,
    /// The agent's `restore: true` on an acting call (#893): it declares the
    /// call only puts the page back the way it was before a check, such as
    /// closing a drawer it opened to look inside. A check recorded before it
    /// is then not made stale by it. Left off the wire when false, so a
    /// service older than this field still admits every unmarked call.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub restore: bool,
}

/// The acting tools that accept `restore`: ones that work on the page in
/// place. A tool that navigates, switches tabs, moves files or runs an
/// adapter replaces what the earlier checks looked at, so it never restores.
pub(crate) const RESTORE_TOOLS: &[&str] = &[
    "chrome_use_click",
    "chrome_use_press",
    "chrome_use_eval",
    "chrome_use_fill",
    "chrome_use_type",
    "chrome_use_select",
    "chrome_use_hover",
    "chrome_use_scroll",
    "chrome_use_find",
];

/// Whether `restore` may be honoured for this call.
pub(crate) fn accepts_restore(tool: &str, class: ToolClass) -> bool {
    class == ToolClass::Act && RESTORE_TOOLS.contains(&tool)
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct AllocatedOutput {
    pub artifact_id: String,
    pub kind: ArtifactKind,
    pub name: String,
    /// Absolute path inside `<workspace>/.chrome-use-out/<actionId>/`.
    pub path: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct BeginReply {
    pub admitted: bool,
    /// Why a lease was refused, worded for the agent.
    #[serde(default)]
    pub reason: Option<String>,
    #[serde(default)]
    pub message_id: Option<String>,
    #[serde(default)]
    pub outputs: Vec<AllocatedOutput>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) enum Outcome {
    Ok,
    Error,
    Timeout,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) enum Verdict {
    Confirmed,
    Failed,
    Uncertain,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) enum Basis {
    Expect,
    ReadBack,
    Navigation,
    TabVerified,
    Download,
    Capture,
    ToolError,
    PageChanged,
    None,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) enum CheckKind {
    Expect,
    ReadBack,
    UrlMatch,
}

/// A machine-checkable predicate the action recorded (section 4.3). The
/// service fills `target` and `observedSequence` from the lease and journal.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct Check {
    pub kind: CheckKind,
    /// Raw predicate text; the service redacts it per 7.2 before journaling.
    pub predicate: String,
}

/// The MCP's verdict. For capture tools it is advisory: the service computes
/// the journaled one after the artifact outcome.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct VerdictReport {
    pub verdict: Verdict,
    pub basis: Basis,
    pub possibly_executed: bool,
    pub check: Option<Check>,
    /// Short, already free of read-back text. Redacted by the service.
    pub detail: Option<String>,
}

/// What `--observe` reported for the action, booleans only.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct Observation {
    pub changed: Option<bool>,
    pub no_progress: bool,
    pub human_check: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct ReportedArtifact {
    pub artifact_id: String,
    /// Must equal the lease's allocated path byte for byte (section 6.2).
    pub path: String,
}

/// `agentActionReport`. `generation` is recorded, never used to attribute:
/// the lease names the turn.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct ActionReport {
    pub caller_conversation: String,
    pub generation: u64,
    pub action_id: String,
    pub tool: String,
    pub tool_class: ToolClass,
    pub arguments_digest: String,
    pub outcome: Outcome,
    pub error_code: Option<String>,
    /// The daemon's `actionContext {actionId, before, after}`, verbatim.
    pub action_context: Option<Value>,
    /// `None` for observe tools.
    pub verdict: Option<VerdictReport>,
    pub observation: Option<Observation>,
    /// `sha256:<hex>` of the result text without the marker and timings,
    /// for observe tools.
    pub result_digest: Option<String>,
    pub duration_ms: u64,
    pub artifacts: Vec<ReportedArtifact>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) enum ArtifactOutcome {
    Ready,
    Unavailable,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct ArtifactStateReply {
    pub artifact_id: String,
    pub state: ArtifactOutcome,
    #[serde(default)]
    pub digest: Option<String>,
    #[serde(default)]
    pub bytes: Option<u64>,
    /// `artifactUnavailable.reason` when unavailable.
    #[serde(default)]
    pub reason: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) enum ReportState {
    /// Another report for this lease is being ingested; ask again.
    Pending,
}

/// The reply to `agentActionReport`. A retry of a recorded report gets the
/// first answer unchanged.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct ReportReply {
    #[serde(default)]
    pub state: Option<ReportState>,
    #[serde(default)]
    pub bound: Option<bool>,
    #[serde(default)]
    pub verdict: Option<Verdict>,
    #[serde(default)]
    pub artifacts: Vec<ArtifactStateReply>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) enum DeliveryState {
    Delivered,
    Failed,
    NotAttempted,
}

/// `agentDeliveryReport` (section 6.2a), idempotent per `actionId`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct DeliveryReport {
    pub caller_conversation: String,
    pub generation: u64,
    pub action_id: String,
    pub artifact_id: String,
    pub state: DeliveryState,
    /// `sha256:<hex>` of the file read back from the requested path.
    pub digest: Option<String>,
    pub detail: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct DeliveryReply {
    pub state: DeliveryState,
}

/// `agentTaskAssess`: the `chrome_use_task` arguments plus the lease
/// context. The service validates `assessment` (section 4.3).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct TaskAssess {
    pub caller_conversation: String,
    pub generation: u64,
    pub boot_id: String,
    /// Milliseconds on the boot's monotonic clock.
    pub not_after: u64,
    /// `{status, reason?, requirements: [{text, status, check?}], blocker?}`
    /// as the agent sent it.
    pub assessment: Value,
}

/// `agentRecordingAuthorize` (section 6.4). Reply: [`RecordingGrant`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct RecordingAuthorize {
    pub caller_conversation: String,
    pub generation: u64,
    pub boot_id: String,
    /// Milliseconds on the boot's monotonic clock.
    pub not_after: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct RecordingGrant {
    pub artifact_id: String,
    /// `<workspace>/.chrome-use-out/<artifactId>/recording.webm`.
    pub path: String,
}

/// `agentRecordingStop`: the service runs `record stop --expect-path` off its
/// lock and ingests the file (section 6.4); the reply is that artifact's
/// state when it is known within the budget.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct RecordingStop {
    pub caller_conversation: String,
    pub generation: u64,
    pub boot_id: String,
    /// Milliseconds on the boot's monotonic clock.
    pub not_after: u64,
    pub artifact_id: Option<String>,
}

/// The transport the MCP side uses. The real one reaches an existing service
/// only; tests substitute their own.
pub(crate) trait EvidenceClient {
    /// One request/reply exchange that gives up after `budget`.
    fn exchange(&self, operation: &str, params: Value, budget: Duration) -> Result<Value, String>;
}

/// Reaches the profile's running agent service; never starts one.
pub(crate) struct ServiceClient {
    pub profile: String,
}

impl EvidenceClient for ServiceClient {
    fn exchange(&self, operation: &str, params: Value, budget: Duration) -> Result<Value, String> {
        super::service::call_existing(operation, &params, &self.profile, budget)
    }
}

pub(crate) fn encode<T: Serialize>(value: &T) -> Result<Value, String> {
    serde_json::to_value(value).map_err(|_| "Cannot encode a panel evidence request".into())
}

pub(crate) fn decode<T: for<'de> Deserialize<'de>>(value: Value) -> Result<T, String> {
    serde_json::from_value(value)
        .map_err(|_| "The agent service sent a malformed evidence reply".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn begin_uses_camel_case_wire_names() {
        let begin = ActionBegin {
            caller_conversation: "c".into(),
            generation: 3,
            boot_id: "b".into(),
            not_after: 9,
            action_id: "a".into(),
            tool: "chrome_use_screenshot".into(),
            tool_class: ToolClass::Capture,
            arguments_digest: "sha256:00".into(),
            requested_destination: Some("shot.png".into()),
            outputs: vec![OutputRequest {
                kind: ArtifactKind::Screenshot,
                name: "shot.png".into(),
            }],
            requested: None,
            restore: false,
        };
        let wire = encode(&begin).unwrap();
        assert_eq!(wire["callerConversation"], "c");
        assert_eq!(
            (&wire["bootId"], &wire["notAfter"]),
            (&json!("b"), &json!(9))
        );
        assert_eq!(wire["toolClass"], "capture");
        assert_eq!(wire["outputs"][0]["kind"], "screenshot");
        assert_eq!(decode::<ActionBegin>(wire).unwrap(), begin);
    }

    #[test]
    fn replies_accept_their_minimal_forms() {
        let refused: BeginReply =
            decode(json!({"admitted":false,"reason":"the user stopped this request"})).unwrap();
        assert!(!refused.admitted && refused.outputs.is_empty());
        let pending: ReportReply = decode(json!({"state":"pending"})).unwrap();
        assert_eq!(pending.state, Some(ReportState::Pending));
        let unbound: ReportReply = decode(json!({"bound":false})).unwrap();
        assert_eq!(unbound.bound, Some(false));
        assert!(decode::<ReportReply>(json!({"bound":true,"extra":1})).is_err());
    }

    #[test]
    fn deadlines_are_monotonic_boot_scoped_and_capped() {
        let soon = Deadline::after(BEGIN_BUDGET).unwrap();
        assert!(!soon.passed());
        assert!(soon.remaining() <= BEGIN_BUDGET);
        assert!(Deadline::admit(&soon.boot_id, soon.not_after).is_ok());
        let gone = soon.not_after - 5_001;
        assert!(Deadline {
            not_after: gone,
            ..soon.clone()
        }
        .passed());
        assert!(Deadline::admit(&soon.boot_id, gone).is_err());
        assert!(Deadline::admit(&soon.boot_id, soon.not_after + 1_000).is_err());
        assert!(Deadline::admit("another boot", soon.not_after).is_err());
        assert!(Deadline {
            boot_id: "another boot".into(),
            ..soon
        }
        .passed());
    }
}
