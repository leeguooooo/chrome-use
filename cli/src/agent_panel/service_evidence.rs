//! The service side of the panel evidence channel (plan 3.3, 4.3, 5.4, 6, 8):
//! action leases and their reports, artifact ingest, delivery records, task
//! assessment, recordings, the panel's artifact and recovery operations, and
//! the progress signals. A child of `service`, so it shares the engine.

use super::super::artifacts::{self, Artifacts, Output};
use super::super::evidence::{self, Lease, LeaseState};
use super::super::evidence_proto::{self as proto, Basis, ToolClass, Verdict};
use super::super::{progress, recovery};
use super::*;
use std::collections::HashSet;
use std::path::PathBuf;
use std::time::Instant;
use tokio::sync::watch;

/// A recording authorized for one message.
pub(super) struct Recording {
    pub message_id: String,
    pub output: Output,
    /// Set once a stop began; the final artifact state arrives here.
    pub stopping: Option<watch::Receiver<Option<Value>>>,
}

/// Per-conversation evidence state, rebuilt from the journal on first use.
#[derive(Default)]
pub(super) struct EvidenceState {
    pub loaded: HashSet<String>,
    pub leases: HashMap<String, HashMap<String, Lease>>,
    pub artifacts: HashMap<String, Artifacts>,
    pub detectors: HashMap<String, progress::StallDetector>,
    /// The agent's latest assessment per conversation: `(messageId, declared)`.
    pub assessments: HashMap<String, (String, Value)>,
    pub recordings: HashMap<String, Recording>,
    /// Recordings the host must stop (turn ended while capturing).
    pub pending_stops: Vec<String>,
    /// Tests: hold every ingest this long, to outlive a caller's budget.
    pub ingest_delay: Option<Duration>,
}

impl EvidenceState {
    pub fn forget(&mut self, id: &str) {
        self.loaded.remove(id);
        self.leases.remove(id);
        self.artifacts.remove(id);
        self.detectors.remove(id);
        self.assessments.remove(id);
        self.recordings.remove(id);
    }
}

fn kind_of(kind: proto::ArtifactKind) -> artifacts::Kind {
    match kind {
        proto::ArtifactKind::Screenshot => artifacts::Kind::Screenshot,
        proto::ArtifactKind::Download => artifacts::Kind::Download,
        proto::ArtifactKind::Recording => artifacts::Kind::Recording,
    }
}

fn wire_kind(kind: artifacts::Kind) -> proto::ArtifactKind {
    match kind {
        artifacts::Kind::Screenshot => proto::ArtifactKind::Screenshot,
        artifacts::Kind::Download => proto::ArtifactKind::Download,
        artifacts::Kind::Recording => proto::ArtifactKind::Recording,
    }
}

/// The tool's named page, bounded: a tab handle and the URL it opens.
fn requested_target(requested: Option<&Value>) -> Option<Value> {
    let requested = requested?;
    let field = |key: &str, max: usize| {
        requested[key]
            .as_str()
            .map(|v| v.chars().take(max).collect::<String>())
    };
    let (tab, url) = (field("tabHandle", 256), field("url", 2048));
    (tab.is_some() || url.is_some()).then(|| json!({"tabHandle": tab, "url": url}))
}

fn evaluator(basis: Basis) -> &'static str {
    match basis {
        Basis::Expect | Basis::ReadBack | Basis::Navigation | Basis::TabVerified => "browser",
        Basis::Download | Basis::Capture => "host",
        _ => "none",
    }
}

/// One artifact ingest prepared under the lock and run off it.
struct IngestJob {
    output: Output,
    reported: Option<PathBuf>,
    reserved: u64,
    native: Option<artifacts::Reason>,
}

enum ReportStep {
    Reply(Value),
    Ingest {
        conversation_dir: PathBuf,
        jobs: Vec<IngestJob>,
    },
}

impl Engine {
    pub(super) fn workspace(&self, id: &str) -> PathBuf {
        self.store.directory.join(id).join("workspace")
    }
    fn conversation_dir(&self, id: &str) -> PathBuf {
        self.store.directory.join(id)
    }

    /// Rebuild leases, artifacts and recordings from the journal once, and
    /// finish reports a crash cut short (plan 3.3 crash points).
    pub(super) fn ensure_evidence(&mut self, id: &str) -> Result<(), String> {
        if self.evidence.loaded.contains(id) {
            return Ok(());
        }
        let events = self.store.all_events(id)?;
        let workspace = self.workspace(id);
        let artifacts = Artifacts::rebuild(&self.conversation_dir(id), &events);
        let leases = evidence::rebuild(&workspace, &events);
        let mut recordings = HashMap::new();
        for event in &events {
            if event.kind == "artifactCreated" && event.data["kind"] == "recording" {
                let artifact = event.data["artifactId"].as_str().unwrap_or("");
                let open = artifacts.entries.get(artifact).is_some_and(|e| {
                    matches!(
                        e.state,
                        artifacts::State::Capturing | artifacts::State::Finalizing
                    )
                });
                if let (true, Some(output)) = (
                    open,
                    Output::from_admitted(&workspace, artifact, None, &event.data),
                ) {
                    recordings.insert(
                        id.to_string(),
                        Recording {
                            message_id: event.data["messageId"].as_str().unwrap_or("").into(),
                            output,
                            stopping: None,
                        },
                    );
                }
            }
        }
        self.evidence.loaded.insert(id.into());
        self.evidence.artifacts.insert(id.into(), artifacts);
        self.evidence.leases.insert(id.into(), leases);
        self.evidence.recordings.extend(recordings);
        // A report that wrote its target but not its verdict was cut short.
        let interrupted: Vec<String> = self.evidence.leases[id]
            .iter()
            .filter(|(_, lease)| lease.state == LeaseState::Reporting)
            .map(|(action, _)| action.clone())
            .collect();
        for action in interrupted {
            self.finish_interrupted(id, &action)?;
        }
        Ok(())
    }

    fn finish_interrupted(&mut self, id: &str, action: &str) -> Result<(), String> {
        let Some(lease) = self
            .evidence
            .leases
            .get(id)
            .and_then(|l| l.get(action))
            .cloned()
        else {
            return Ok(());
        };
        self.fail_outputs(
            id,
            &lease,
            artifacts::DetectedBy::Load,
            "report interrupted",
        )?;
        let verdict = if lease.captures() {
            Verdict::Failed
        } else {
            Verdict::Uncertain
        };
        self.record(
            id,
            "actionObserved",
            json!({"v":1,"messageId":lease.message_id,"actionId":action,"verdict":verdict,"evaluator":"none","basis":"none","detail":"report interrupted","possiblyExecuted":true,"check":null,"lateReported":false}),
        )?;
        if let Some(lease) = self
            .evidence
            .leases
            .get_mut(id)
            .and_then(|l| l.get_mut(action))
        {
            lease.state = LeaseState::Reported;
            lease.verdict = Some(verdict);
        }
        Ok(())
    }

    /// Mark a lease's non-terminal artifacts unavailable (`interrupted`).
    fn fail_outputs(
        &mut self,
        id: &str,
        lease: &Lease,
        detected_by: artifacts::DetectedBy,
        detail: &str,
    ) -> Result<(), String> {
        for output in &lease.outputs {
            let failure = artifacts::Unavailable {
                reason: artifacts::Reason::Interrupted,
                detail: detail.into(),
                bytes: None,
                digest: None,
            };
            let event = self.evidence.artifacts.entry(id.into()).or_default().fail(
                &output.artifact_id,
                failure,
                detected_by,
            );
            if let Some((kind, data)) = event {
                self.record(id, kind, data)?;
            }
        }
        Ok(())
    }

    /// D1a admission for Begin, assess and recording authorize.
    fn admit_caller(
        &mut self,
        conversation: &str,
        generation: u64,
        boot: &str,
        not_after: u64,
    ) -> Result<Message, String> {
        proto::Deadline::admit(boot, not_after).map_err(String::from)?;
        self.chat(conversation)
            .map_err(|_| "the conversation does not exist".to_string())?;
        self.ensure_evidence(conversation)?;
        self.admits(conversation, generation)
            .cloned()
            .map_err(String::from)
    }

    pub(super) fn action_begin(&mut self, p: proto::ActionBegin) -> Value {
        match self.try_begin(&p) {
            Ok(reply) => reply,
            Err(reason) => json!({"admitted":false,"reason":reason}),
        }
    }

    fn try_begin(&mut self, p: &proto::ActionBegin) -> Result<Value, String> {
        let id = p.caller_conversation.as_str();
        let message = self.admit_caller(id, p.generation, &p.boot_id, p.not_after)?;
        let action = uuid::Uuid::parse_str(&p.action_id)
            .map(|u| u.to_string())
            .ok()
            .filter(|canonical| canonical == &p.action_id)
            .ok_or("the action id is not a UUID")?;
        let leases = self.evidence.leases.entry(id.into()).or_default();
        if leases.contains_key(&action) {
            return Err("this action id was already used".into());
        }
        let open = leases
            .values()
            .filter(|l| l.state == LeaseState::Open)
            .count();
        if open >= evidence::MAX_OPEN_LEASES {
            return Err("too many browser actions are still unreported".into());
        }
        if p.outputs.len() > 1 {
            return Err("a browser action can capture at most one file".into());
        }
        if p.restore && !proto::accepts_restore(&p.tool, p.tool_class) {
            return Err("restore is not accepted on this tool".into());
        }
        let workspace = self.workspace(id);
        let mut outputs = Vec::new();
        for request in &p.outputs {
            let kind = kind_of(request.kind);
            let capture = artifacts::CaptureRequest {
                kind,
                name: artifacts::sanitize_name(&request.name, kind),
                requested_destination: p.requested_destination.clone(),
            };
            outputs.push(
                artifacts::allocate(&workspace, &action, &capture)
                    .map_err(|_| "the output folder could not be prepared")?,
            );
        }
        let reserved = self
            .evidence
            .artifacts
            .entry(id.into())
            .or_default()
            .admit(&message.id, &outputs);
        let requested = requested_target(p.requested.as_ref());
        let entries: Vec<Value> = outputs
            .iter()
            .zip(&reserved)
            .map(|(o, r)| o.admitted_entry(*r))
            .collect();
        self.record(
            id,
            "actionAdmitted",
            json!({"v":1,"messageId":message.id,"generation":p.generation,"actionId":action,"tool":p.tool,"toolClass":p.tool_class,"argumentsDigest":p.arguments_digest,"outputs":entries,"quotaReserved":reserved.iter().sum::<u64>(),"requested":requested,"restore":p.restore}),
        )?;
        // Each capture is an artifact from its admission on (plan 2.2).
        for output in &outputs {
            self.record(
                id,
                "artifactCreated",
                json!({"v":1,"messageId":message.id,"actionId":action,"artifactId":output.artifact_id,"kind":output.kind,"name":output.name,"state":"pending"}),
            )?;
        }
        let allocated: Vec<proto::AllocatedOutput> = outputs
            .iter()
            .map(|o| proto::AllocatedOutput {
                artifact_id: o.artifact_id.clone(),
                kind: wire_kind(o.kind),
                name: o.name.clone(),
                path: o.path().to_string_lossy().into_owned(),
            })
            .collect();
        let mut lease = Lease::new(&message.id, p.generation, &p.tool, p.tool_class, outputs);
        lease.requested = requested;
        lease.restore = p.restore;
        self.evidence
            .leases
            .entry(id.into())
            .or_default()
            .insert(action, lease);
        proto::encode(&proto::BeginReply {
            admitted: true,
            reason: None,
            message_id: Some(message.id),
            outputs: allocated,
        })
    }

    /// Lock 1 of a report: the lease decides what happens.
    fn report_open(&mut self, p: &proto::ActionReport) -> Result<ReportStep, String> {
        let id = p.caller_conversation.as_str();
        self.chat(id)?;
        self.ensure_evidence(id)?;
        let Some(lease) = self
            .evidence
            .leases
            .get(id)
            .and_then(|l| l.get(&p.action_id))
            .cloned()
        else {
            let malformed = uuid::Uuid::parse_str(&p.action_id).is_err();
            let elsewhere = self
                .evidence
                .leases
                .iter()
                .any(|(other, l)| other != id && l.contains_key(&p.action_id));
            let reason = if malformed {
                "malformed"
            } else if elsewhere {
                "otherConversation"
            } else {
                "noLease"
            };
            self.record(
                id,
                "unboundActionReport",
                json!({"v":1,"generation":p.generation,"actionId":(!malformed).then_some(&p.action_id),"tool":p.tool,"reason":reason}),
            )?;
            return Ok(ReportStep::Reply(json!({"bound":false})));
        };
        let artifacts = self.evidence.artifacts.entry(id.into()).or_default();
        match lease.state {
            LeaseState::Reported | LeaseState::LateReported => {
                Ok(ReportStep::Reply(lease.reply(artifacts)))
            }
            LeaseState::Reporting => Ok(ReportStep::Reply(json!({"state":"pending"}))),
            LeaseState::Unreported => {
                // The first late report: recorded once, under its own turn,
                // with no ingest; artifacts keep their turn-end state.
                self.journal_target(id, &lease, p, true)?;
                let verdict = if lease.captures() {
                    Some(Verdict::Failed)
                } else {
                    p.verdict.as_ref().map(|v| v.verdict)
                };
                self.journal_observed(id, &p.action_id, &lease, p, verdict, None, true)?;
                let lease = self.lease_mut(id, &p.action_id);
                lease.state = LeaseState::LateReported;
                lease.verdict = verdict;
                let reply = lease.clone().reply(&self.evidence.artifacts[id]);
                Ok(ReportStep::Reply(reply))
            }
            LeaseState::Open => {
                self.journal_target(id, &lease, p, false)?;
                let sequence = self.last_sequence(id);
                let landing = evidence::landing(p.action_context.as_ref());
                {
                    let lease = self.lease_mut(id, &p.action_id);
                    lease.state = LeaseState::Reporting;
                    lease.report_sequence = Some(sequence);
                    lease.landing = landing;
                }
                let native = p
                    .error_code
                    .as_deref()
                    .and_then(artifacts::Reason::from_native_code);
                let quota = &self.evidence.artifacts[id].quota;
                let jobs = lease
                    .outputs
                    .iter()
                    .map(|output| IngestJob {
                        output: output.clone(),
                        reported: p
                            .artifacts
                            .iter()
                            .find(|a| a.artifact_id == output.artifact_id)
                            .map(|a| PathBuf::from(&a.path)),
                        reserved: quota.reserved_for(&output.artifact_id).unwrap_or(0),
                        native,
                    })
                    .collect();
                Ok(ReportStep::Ingest {
                    conversation_dir: self.conversation_dir(id),
                    jobs,
                })
            }
        }
    }

    fn lease_mut(&mut self, id: &str, action: &str) -> &mut Lease {
        self.evidence
            .leases
            .get_mut(id)
            .and_then(|l| l.get_mut(action))
            .expect("the lease exists")
    }

    fn journal_target(
        &mut self,
        id: &str,
        lease: &Lease,
        p: &proto::ActionReport,
        late: bool,
    ) -> Result<(), String> {
        let recovery = self.chat(id)?.phase == Phase::RecoveryRequired;
        let observed = p.action_context.as_ref().map(|c| {
            json!({"before": c.get("before").cloned().unwrap_or(Value::Null), "after": c.get("after").cloned().unwrap_or(Value::Null)})
        });
        self.record(
            id,
            "browserActionTarget",
            json!({"v":1,"messageId":lease.message_id,"generation":lease.generation,"actionId":p.action_id,"tool":lease.tool,"toolClass":lease.tool_class,"requested":lease.requested,"observed":observed,"outcome":p.outcome,"errorCode":p.error_code,"durationMs":p.duration_ms,"resultDigest":p.result_digest,"afterStop":lease.after_stop,"afterTurnEnd":lease.turn_ended || late,"afterRecoveryRequired":recovery,"lateReported":late}),
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn journal_observed(
        &mut self,
        id: &str,
        action: &str,
        lease: &Lease,
        p: &proto::ActionReport,
        verdict: Option<Verdict>,
        artifact_detail: Option<String>,
        late: bool,
    ) -> Result<(), String> {
        let mcp = p.verdict.as_ref();
        let basis = if lease.captures() {
            if lease
                .outputs
                .iter()
                .any(|o| o.kind == artifacts::Kind::Download)
            {
                Basis::Download
            } else {
                Basis::Capture
            }
        } else {
            mcp.map_or(Basis::None, |v| v.basis)
        };
        let check = mcp.and_then(|v| v.check.as_ref()).map(|c| {
            json!({"kind": c.kind, "predicate": c.predicate, "target": p.action_context.as_ref().map(|ctx| ctx["after"].clone()), "observedSequence": lease.report_sequence.or(Some(self.last_sequence(id)))})
        });
        let detail = artifact_detail.or_else(|| mcp.and_then(|v| v.detail.clone()));
        let possibly = mcp.map_or(lease.tool_class != ToolClass::Observe, |v| {
            v.possibly_executed
        });
        self.record(
            id,
            "actionObserved",
            json!({"v":1,"messageId":lease.message_id,"actionId":action,"verdict":verdict,"evaluator":evaluator(basis),"basis":basis,"detail":detail,"possiblyExecuted":possibly,"check":check,"lateReported":late}),
        )?;
        if let Some(c) = mcp.and_then(|v| v.check.as_ref()) {
            self.lease_mut(id, action).check = Some((
                c.kind,
                redact::text_for_compare(&c.predicate),
                Some(c.predicate.clone()),
            ));
        }
        Ok(())
    }

    /// Lock 2 of a report: artifact terminals, then the verdict, last.
    fn report_finish(
        &mut self,
        p: &proto::ActionReport,
        outcomes: Vec<(String, artifacts::Outcome)>,
    ) -> Result<Value, String> {
        let id = p.caller_conversation.as_str();
        let lease = self
            .evidence
            .leases
            .get(id)
            .and_then(|l| l.get(&p.action_id))
            .cloned()
            .ok_or("The action lease disappeared")?;
        let mut failure = None;
        for (artifact, outcome) in &outcomes {
            if let artifacts::Outcome::Unavailable(u) = outcome {
                failure.get_or_insert_with(|| u.detail.clone());
            }
            let event = self
                .evidence
                .artifacts
                .entry(id.into())
                .or_default()
                .settle(artifact, outcome);
            if let Some((kind, data)) = event {
                self.record(id, kind, data)?;
            }
        }
        let verdict = if lease.captures() {
            let all_ready = lease.outputs.iter().all(|o| {
                matches!(
                    self.evidence.artifacts[id]
                        .entries
                        .get(&o.artifact_id)
                        .map(|e| &e.state),
                    Some(artifacts::State::Ready(_))
                )
            });
            Some(if all_ready {
                Verdict::Confirmed
            } else {
                Verdict::Failed
            })
        } else {
            p.verdict.as_ref().map(|v| v.verdict)
        };
        let lease = self.lease_mut(id, &p.action_id).clone();
        self.journal_observed(id, &p.action_id, &lease, p, verdict, failure, false)?;
        let observed_sequence = self.last_sequence(id);
        {
            let lease = self.lease_mut(id, &p.action_id);
            lease.state = LeaseState::Reported;
            lease.verdict = verdict;
        }
        // Progress: only reports of the running turn count.
        let current = self
            .turns
            .get(id)
            .is_some_and(|t| t.message_id == lease.message_id);
        if current {
            let landing = evidence::landing(p.action_context.as_ref());
            let sample = progress::ActionSample {
                sequence: observed_sequence,
                action_id: p.action_id.clone(),
                tool: lease.tool.clone(),
                arguments: json!(p.arguments_digest),
                after_target_id: landing.target_id,
                after_url: landing.url.map(|u| redact::url(&u)),
                result_digest: p.result_digest.clone(),
                failed: p.outcome != proto::Outcome::Ok || verdict == Some(Verdict::Failed),
                error_code: p.error_code.clone(),
                native_no_progress: p.observation.as_ref().is_some_and(|o| o.no_progress),
            };
            let signals = self
                .evidence
                .detectors
                .get_mut(id)
                .map(|d| d.observe_action(&sample, Instant::now()))
                .unwrap_or_default();
            self.journal_signals(id, &lease.message_id, signals)?;
        }
        Ok(self.evidence.leases[id][&p.action_id].reply(&self.evidence.artifacts[id]))
    }

    pub(super) fn journal_signals(
        &mut self,
        id: &str,
        message_id: &str,
        signals: Vec<progress::Signal>,
    ) -> Result<(), String> {
        for signal in signals {
            let (kind, data) = signal.event(message_id);
            self.record(id, kind, data)?;
        }
        Ok(())
    }

    fn delivery(&mut self, p: proto::DeliveryReport) -> Result<Value, String> {
        let id = p.caller_conversation.as_str();
        self.chat(id)?;
        self.ensure_evidence(id)?;
        let lease = self
            .evidence
            .leases
            .get(id)
            .and_then(|l| l.get(&p.action_id))
            .cloned()
            .ok_or("Unknown action")?;
        if let Some(recorded) = &lease.delivery {
            return Ok(recorded.clone());
        }
        let output = lease
            .outputs
            .iter()
            .find(|o| o.artifact_id == p.artifact_id)
            .ok_or("Unknown artifact for this action")?;
        let entry = self
            .evidence
            .artifacts
            .get(id)
            .and_then(|a| a.entries.get(&p.artifact_id))
            .filter(|e| e.action_id.as_deref() == Some(p.action_id.as_str()))
            .ok_or("Unknown artifact for this action")?;
        let digest = match entry.state.clone() {
            artifacts::State::Ready(info) => Some(info.digest),
            _ => None,
        };
        let report = match p.state {
            proto::DeliveryState::NotAttempted => None,
            state => Some(artifacts::Delivery {
                state: if state == proto::DeliveryState::Delivered {
                    artifacts::DeliveryState::Delivered
                } else {
                    artifacts::DeliveryState::Failed
                },
                path: None,
                digest: p.digest.clone(),
                detail: p.detail.clone().unwrap_or_default(),
            }),
        };
        let reply = match artifacts::file_delivered_event(
            &lease.message_id,
            output,
            report.as_ref(),
            digest.as_deref(),
        ) {
            Some(event) => {
                let state = event["state"].clone();
                self.record(id, "fileDelivered", event)?;
                json!({"state": state})
            }
            None => json!({"state":"notAttempted"}),
        };
        self.lease_mut(id, &p.action_id).delivery = Some(reply.clone());
        Ok(reply)
    }

    fn task_assess(&mut self, p: proto::TaskAssess) -> Result<Value, String> {
        let id = p.caller_conversation.as_str();
        let message = self.admit_caller(id, p.generation, &p.boot_id, p.not_after)?;
        if !p.assessment.is_object() || !p.assessment["requirements"].is_array() {
            return Err("An assessment needs a status and a list of requirements".into());
        }
        let data = evidence::assess(
            &p.assessment,
            &message.id,
            &self.evidence.leases[id],
            &self.evidence.artifacts[id],
            None,
            "agent",
        );
        self.evidence
            .assessments
            .insert(id.into(), (message.id.clone(), p.assessment));
        self.record(id, "taskAssessment", data.clone())?;
        Ok(data)
    }

    /// At `turnEnded`: leases still open become `Unreported` and their
    /// artifacts `interrupted`; every lease of the turn is marked ended.
    pub(super) fn close_leases(&mut self, id: &str, message_id: &str) -> Result<(), String> {
        self.ensure_evidence(id)?;
        let mut open = Vec::new();
        for (action, lease) in self.evidence.leases.entry(id.into()).or_default() {
            if lease.message_id != message_id {
                continue;
            }
            lease.turn_ended = true;
            if lease.state == LeaseState::Open {
                lease.state = LeaseState::Unreported;
                open.push(lease.clone());
            }
            let _ = action;
        }
        for lease in open {
            self.fail_outputs(
                id,
                &lease,
                artifacts::DetectedBy::TurnEnd,
                "the request ended",
            )?;
        }
        Ok(())
    }

    /// Open leases of a turn, sorted, for `turnEnded.openLeases`.
    pub(super) fn open_lease_ids(&mut self, id: &str, message_id: &str) -> Vec<String> {
        if self.ensure_evidence(id).is_err() {
            return Vec::new();
        }
        let mut ids: Vec<String> = self
            .evidence
            .leases
            .get(id)
            .into_iter()
            .flatten()
            .filter(|(_, l)| l.message_id == message_id && l.state == LeaseState::Open)
            .map(|(a, _)| a.clone())
            .take(64)
            .collect();
        ids.sort();
        ids
    }

    pub(super) fn lease_execution(&self, id: &str, action: &str) -> Option<&'static str> {
        let lease = self.evidence.leases.get(id)?.get(action)?;
        Some(match lease.state {
            LeaseState::Open | LeaseState::Unreported => "admitted",
            _ => "reported",
        })
    }

    /// The final assessment, recomputed against the journal as it stands.
    pub(super) fn final_assessment(
        &mut self,
        id: &str,
        message_id: &str,
        outcome: &str,
    ) -> Result<(), String> {
        let declared = self
            .evidence
            .assessments
            .remove(id)
            .filter(|(message, _)| message == message_id);
        let data = match declared {
            Some((_, declared)) => evidence::assess(
                &declared,
                message_id,
                self.evidence.leases.entry(id.into()).or_default(),
                self.evidence.artifacts.entry(id.into()).or_default(),
                Some(outcome),
                "agent",
            ),
            None => evidence::not_assessed(message_id),
        };
        self.record(id, "taskAssessment", data)
    }

    /// `turnSummary`, from this turn's journaled events.
    pub(super) fn summarize_turn(
        &mut self,
        id: &str,
        message_id: &str,
        first_sequence: u64,
    ) -> Result<(), String> {
        let events = self.store.events_from(id, first_sequence)?;
        let data = progress::turn_summary(&events, message_id);
        self.record(id, "turnSummary", data)
    }

    /// `contextCompacted` with the range since the turn's start or the last
    /// compaction.
    pub(super) fn compacted(&mut self, id: &str, data: &Value) -> Result<Value, String> {
        let Some(turn) = self.turns.get(id) else {
            return Ok(data.clone());
        };
        let (message, first) = (turn.message_id.clone(), turn.first_sequence);
        let events = self.store.events_from(id, first)?;
        let backend = data["backend"]
            .as_str()
            .map(String::from)
            .unwrap_or_else(|| {
                self.chats
                    .get(id)
                    .map(|c| c.agent_id.clone())
                    .unwrap_or_default()
            });
        Ok(progress::context_compacted(
            &events,
            &message,
            &backend,
            data["trigger"].as_str(),
            self.last_sequence(id) + 1,
        ))
    }

    /// Recording for the turn that ended: the host stops it.
    pub(super) fn stop_recording_after(&mut self, id: &str, message_id: &str) {
        if self
            .evidence
            .recordings
            .get(id)
            .is_some_and(|r| r.message_id == message_id && r.stopping.is_none())
        {
            self.evidence.pending_stops.push(id.into());
        }
    }

    fn recording_authorize(&mut self, p: &proto::RecordingAuthorize) -> Result<Value, String> {
        let id = p.caller_conversation.as_str();
        let message = self.admit_caller(id, p.generation, &p.boot_id, p.not_after)?;
        if !message.record {
            return Err("the user did not allow recording for this request".into());
        }
        if self.evidence.recordings.contains_key(id) {
            return Err("a recording is already running or still finishing".into());
        }
        let output = artifacts::allocate_recording(&self.workspace(id))
            .map_err(|_| "the recording folder could not be prepared")?;
        let data = self
            .evidence
            .artifacts
            .entry(id.into())
            .or_default()
            .start_recording(&message.id, &output);
        self.record(id, "artifactCreated", data)?;
        let grant = proto::RecordingGrant {
            artifact_id: output.artifact_id.clone(),
            path: output.path().to_string_lossy().into_owned(),
        };
        self.evidence.recordings.insert(
            id.into(),
            Recording {
                message_id: message.id,
                output,
                stopping: None,
            },
        );
        proto::encode(&grant)
    }
}

/// Run `record stop --expect-path` for the conversation's browser session.
/// `Err` carries the CLI's error text.
async fn run_record_stop(profile: &str, conversation: &str, path: &Path) -> Result<(), String> {
    let executable = std::env::current_exe().map_err(|_| "Cannot locate chrome-use".to_string())?;
    let mut command = tokio::process::Command::new(executable);
    command
        .args(["record", "stop", "--expect-path"])
        .arg(path)
        .arg("--json")
        .env("AGENT_BROWSER_BROWSER", profile)
        .env("AGENT_BROWSER_SESSION", format!("panel-{conversation}"))
        .env("CHROME_USE_NO_UPDATE_CHECK", "1")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    let output = tokio::time::timeout(Duration::from_secs(60), command.output())
        .await
        .map_err(|_| "The recording did not stop in time".to_string())?
        .map_err(|_| "Cannot run chrome-use".to_string())?;
    let reply: Value = serde_json::from_slice(&output.stdout).unwrap_or(Value::Null);
    if output.status.success() && reply["success"] != false {
        return Ok(());
    }
    Err(reply["error"]
        .as_str()
        .map(String::from)
        .unwrap_or_else(|| String::from_utf8_lossy(&output.stderr).into_owned()))
}

/// The artifact outcome of a failed stop.
fn stop_failure(error: &str) -> artifacts::Outcome {
    let tail: String = {
        let chars: Vec<char> = error.chars().collect();
        chars[chars.len().saturating_sub(500)..].iter().collect()
    };
    let reason = if error.contains("ffmpeg not found") {
        artifacts::Reason::EncoderMissing
    } else if error.contains("ffmpeg") {
        artifacts::Reason::EncoderFailed
    } else {
        artifacts::Reason::Failed
    };
    artifacts::Outcome::Unavailable(artifacts::Unavailable {
        reason,
        detail: redact::text_for_compare(&tail),
        bytes: None,
        digest: None,
    })
}

/// Begin (or join) the stop of a conversation's recording. The stop and the
/// ingest run in a task off the lock; the receiver gets the final state.
pub(super) async fn stop_recording(
    engine: &Arc<Mutex<Engine>>,
    id: &str,
) -> Option<watch::Receiver<Option<Value>>> {
    let (receiver, sender, output, reserved, directory, profile) = {
        let mut e = engine.lock().await;
        let recording = e.evidence.recordings.get_mut(id)?;
        if let Some(receiver) = &recording.stopping {
            return Some(receiver.clone());
        }
        let (sender, receiver) = watch::channel(None);
        recording.stopping = Some(receiver.clone());
        let output = recording.output.clone();
        let finalizing = e
            .evidence
            .artifacts
            .entry(id.into())
            .or_default()
            .finalizing(&output.artifact_id);
        if let Some(data) = finalizing {
            let _ = e.record(id, "artifactState", data);
        }
        let reserved = e
            .evidence
            .artifacts
            .get(id)
            .and_then(|a| a.quota.reserved_for(&output.artifact_id))
            .unwrap_or(0);
        (
            receiver,
            sender,
            output,
            reserved,
            e.conversation_dir(id),
            e.profile.clone(),
        )
    };
    let owner = engine.clone();
    let id = id.to_string();
    tokio::spawn(async move {
        let outcome = match run_record_stop(&profile, &id, &output.path()).await {
            Err(error) => stop_failure(&error),
            Ok(()) => {
                let output = output.clone();
                tokio::task::spawn_blocking(move || {
                    artifacts::ingest(&directory, &output, &output.path(), reserved)
                })
                .await
                .unwrap_or_else(|_| stop_failure("The recording could not be copied"))
            }
        };
        let mut e = owner.lock().await;
        let event = e
            .evidence
            .artifacts
            .entry(id.clone())
            .or_default()
            .settle(&output.artifact_id, &outcome);
        if let Some((kind, data)) = event {
            let _ = e.record(&id, kind, data);
        }
        e.evidence.recordings.remove(&id);
        let state = e
            .evidence
            .artifacts
            .get(&id)
            .and_then(|a| evidence::artifact_state(a, &output.artifact_id));
        let _ = sender.send(Some(state.unwrap_or(Value::Null)));
    });
    Some(receiver)
}

/// Stops the host owes after turns ended while recording.
pub(super) async fn run_pending_stops(engine: &Arc<Mutex<Engine>>) {
    let pending = std::mem::take(&mut engine.lock().await.evidence.pending_stops);
    for id in pending {
        let _ = stop_recording(engine, &id).await;
    }
}

async fn wait_state(
    mut receiver: watch::Receiver<Option<Value>>,
    budget: Duration,
) -> Option<Value> {
    let _ = tokio::time::timeout(budget, receiver.wait_for(|state| state.is_some())).await;
    let state = receiver.borrow().clone();
    state
}

fn ingest_job(directory: &Path, job: IngestJob) -> artifacts::Outcome {
    match (job.native, &job.reported) {
        (Some(reason), _) => artifacts::Outcome::Unavailable(artifacts::Unavailable {
            reason,
            detail: "The browser reported the capture failed".into(),
            bytes: None,
            digest: None,
        }),
        (None, Some(path)) => artifacts::ingest(directory, &job.output, path, job.reserved),
        (None, None) => artifacts::Outcome::Unavailable(artifacts::Unavailable {
            reason: artifacts::Reason::Missing,
            detail: "The tool reported no file".into(),
            bytes: None,
            digest: None,
        }),
    }
}

/// The agent-only side channel operations.
pub(super) async fn evidence_dispatch(
    engine: &Arc<Mutex<Engine>>,
    operation: &str,
    params: &Value,
) -> Result<Value, String> {
    match operation {
        proto::OP_ACTION_BEGIN => {
            let p: proto::ActionBegin = proto::decode(params.clone())
                .map_err(|_| "agentActionBegin request is malformed")?;
            Ok(engine.lock().await.action_begin(p))
        }
        proto::OP_ACTION_REPORT => {
            let p: proto::ActionReport = proto::decode(params.clone())
                .map_err(|_| "agentActionReport request is malformed")?;
            let (step, delay) = {
                let mut e = engine.lock().await;
                (e.report_open(&p)?, e.evidence.ingest_delay)
            };
            let (directory, jobs) = match step {
                ReportStep::Reply(reply) => return Ok(reply),
                ReportStep::Ingest {
                    conversation_dir,
                    jobs,
                } => (conversation_dir, jobs),
            };
            // The ingest and the lease's completion run in their own task:
            // a caller that gave up (its 60 s budget, a dropped connection)
            // must not leave the lease in Reporting. Later reports of this
            // action get `pending`, then the recorded reply.
            let owner = engine.clone();
            let finish = tokio::spawn(async move {
                let artifact_ids: Vec<String> = jobs
                    .iter()
                    .map(|job| job.output.artifact_id.clone())
                    .collect();
                if let Some(delay) = delay {
                    tokio::time::sleep(delay).await;
                }
                let outcomes = tokio::task::spawn_blocking(move || {
                    jobs.into_iter()
                        .map(|job| (job.output.artifact_id.clone(), ingest_job(&directory, job)))
                        .collect::<Vec<_>>()
                })
                .await
                .unwrap_or_else(|_| {
                    artifact_ids
                        .into_iter()
                        .map(|id| {
                            let failed = artifacts::Outcome::Unavailable(artifacts::Unavailable {
                                reason: artifacts::Reason::Failed,
                                detail: "The capture could not be copied".into(),
                                bytes: None,
                                digest: None,
                            });
                            (id, failed)
                        })
                        .collect()
                });
                owner.lock().await.report_finish(&p, outcomes)
            });
            finish
                .await
                .map_err(|_| "The report could not be recorded".to_string())?
        }
        proto::OP_DELIVERY_REPORT => {
            let p: proto::DeliveryReport = proto::decode(params.clone())
                .map_err(|_| "agentDeliveryReport request is malformed")?;
            engine.lock().await.delivery(p)
        }
        proto::OP_TASK_ASSESS => {
            let p: proto::TaskAssess = proto::decode(params.clone())
                .map_err(|_| "agentTaskAssess request is malformed")?;
            engine.lock().await.task_assess(p)
        }
        proto::OP_RECORDING_AUTHORIZE => {
            let p: proto::RecordingAuthorize = proto::decode(params.clone())
                .map_err(|_| "agentRecordingAuthorize request is malformed")?;
            // A previous stop of this conversation may still be finishing;
            // the daemon allows one recording at a time.
            let finishing = engine
                .lock()
                .await
                .evidence
                .recordings
                .get(&p.caller_conversation)
                .and_then(|r| r.stopping.clone());
            if let Some(receiver) = finishing {
                if wait_state(receiver, Duration::from_secs(15))
                    .await
                    .is_none()
                {
                    return Err("the previous recording is still finishing".into());
                }
            }
            engine.lock().await.recording_authorize(&p)
        }
        proto::OP_RECORDING_STOP => {
            let p: proto::RecordingStop = proto::decode(params.clone())
                .map_err(|_| "agentRecordingStop request is malformed")?;
            proto::Deadline::admit(&p.boot_id, p.not_after).map_err(String::from)?;
            let artifact = {
                let mut e = engine.lock().await;
                e.chat(&p.caller_conversation)?;
                e.ensure_evidence(&p.caller_conversation)?;
                let recording = e
                    .evidence
                    .recordings
                    .get(&p.caller_conversation)
                    .ok_or("No recording is running")?;
                if p.artifact_id
                    .as_ref()
                    .is_some_and(|a| a != &recording.output.artifact_id)
                {
                    return Err("That recording is not the running one".into());
                }
                recording.output.artifact_id.clone()
            };
            let receiver = stop_recording(engine, &p.caller_conversation)
                .await
                .ok_or("No recording is running")?;
            let budget = proto::RECORDING_STOP_BUDGET.saturating_sub(Duration::from_secs(2));
            Ok(wait_state(receiver, budget)
                .await
                .filter(|s| !s.is_null())
                .unwrap_or_else(|| json!({"artifactId":artifact,"state":"finalizing"})))
        }
        _ => Err("Unsupported panel evidence operation".into()),
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ArtifactRead {
    conversation_id: String,
    artifact_id: String,
    #[serde(default)]
    offset: u64,
    #[serde(default)]
    length: Option<u64>,
}

/// `conversationArtifactRead` (plan 6.3): one chunk of a ready artifact of
/// this conversation; the panel never receives a path.
pub(super) async fn artifact_read(
    engine: &Arc<Mutex<Engine>>,
    params: &Value,
) -> Result<Value, String> {
    let p: ArtifactRead = serde_json::from_value(params.clone())
        .map_err(|_| "conversationArtifactRead requires conversationId and artifactId")?;
    let (info, name, directory) = {
        let mut e = engine.lock().await;
        e.chat(&p.conversation_id)?;
        e.ensure_evidence(&p.conversation_id)?;
        let artifacts = e
            .evidence
            .artifacts
            .entry(p.conversation_id.clone())
            .or_default();
        let info = artifacts.readable(&p.artifact_id)?;
        let name = artifacts.entries[&p.artifact_id].name.clone();
        (info, name, e.conversation_dir(&p.conversation_id))
    };
    let length = p.length.unwrap_or(artifacts::MAX_CHUNK);
    let read = {
        let info = info.clone();
        tokio::task::spawn_blocking(move || {
            artifacts::read_chunk(&directory, &info, p.offset, length)
        })
        .await
        .map_err(|_| "artifact_unavailable".to_string())?
    };
    match read {
        Ok(chunk) => {
            let mut reply = chunk.reply(&p.artifact_id);
            reply["schemaVersion"] = 1.into();
            reply["name"] = name.into();
            Ok(reply)
        }
        Err(error) => {
            let mut e = engine.lock().await;
            let event = e
                .evidence
                .artifacts
                .entry(p.conversation_id.clone())
                .or_default()
                .read_failed(&p.artifact_id, &error);
            if let Some(data) = event {
                e.record(&p.conversation_id, "artifactUnavailable", data)?;
            }
            Err("artifact_unavailable".into())
        }
    }
}

/// What the recovery operations need from the journal, under the lock.
struct RecoveryView {
    events: Vec<super::super::conversation::Event>,
    record: Option<recovery::ProcessRecord>,
    message_id: Option<String>,
    summary: Value,
    phase: Phase,
    last_error: Option<String>,
    #[cfg(windows)]
    held: Option<Arc<super::super::windows::ProcessJob>>,
}

async fn recovery_view(engine: &Arc<Mutex<Engine>>, id: &str) -> Result<RecoveryView, String> {
    let mut e = engine.lock().await;
    let chat = e.chat(id)?.clone();
    let events = e.store.all_events(id)?;
    let record = recovery::ProcessRecord::from_events(&events);
    let message_id = chat.active.as_ref().map(|m| m.id.clone()).or_else(|| {
        events
            .iter()
            .rev()
            .find(|ev| ev.kind == "turnStarted")
            .and_then(|ev| ev.data["messageId"].as_str().map(String::from))
    });
    #[cfg(windows)]
    let held = e.held_jobs.get(id).map(|(_, job)| job.clone());
    Ok(RecoveryView {
        #[cfg(windows)]
        held,
        events,
        record,
        message_id,
        summary: chat.summary(),
        phase: chat.phase,
        last_error: chat.last_error,
    })
}

#[cfg(unix)]
fn probe(view: &RecoveryView) -> recovery::Probe {
    recovery::probe_group(view.record.as_ref(), recovery::ProofPolicy::running_os())
}

/// Only the job the service still holds proves anything; after a restart
/// the answer is `unknown`.
#[cfg(windows)]
fn probe(view: &RecoveryView) -> recovery::Probe {
    recovery::probe_job(view.held.as_deref())
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RecoveryRequest {
    conversation_id: String,
}

/// `conversationRecoveryReview`, `conversationRecoveryTerminate` and
/// `conversationRecoveryFork` (plan 5.4).
pub(super) async fn recovery_dispatch(
    engine: &Arc<Mutex<Engine>>,
    operation: &str,
    params: &Value,
) -> Result<Value, String> {
    let p: RecoveryRequest = serde_json::from_value(params.clone())
        .map_err(|_| "Recovery operations require only conversationId")?;
    let id = p.conversation_id.as_str();
    let view = recovery_view(engine, id).await?;
    let (view, probed) = tokio::task::spawn_blocking(move || {
        let probed = probe(&view);
        (view, probed)
    })
    .await
    .map_err(|_| "The process probe failed")?;
    match operation {
        "conversationRecoveryReview" => {
            let mut reply = match &view.message_id {
                Some(message) => recovery::review_turn(&view.events, message),
                None => json!({"actions":[],"unresolved":[],"artifacts":[],"lastTargets":[]}),
            };
            reply["schemaVersion"] = 1.into();
            reply["conversation"] = view.summary;
            reply["phase"] = json!(view.phase);
            reply["lastError"] = json!(view.last_error);
            reply["process"] = probed.review(view.record.as_ref());
            Ok(reply)
        }
        "conversationRecoveryTerminate" => {
            if view.phase != Phase::RecoveryRequired {
                return Err("Only a conversation that needs recovery can end its old agent".into());
            }
            if probed.state != recovery::ProcessState::Alive {
                return Err("The old agent process is not proven to be running".into());
            }
            #[cfg(unix)]
            let termination = recovery::terminate_group(
                view.record.as_ref().ok_or("No process record")?,
                recovery::ProofPolicy::running_os(),
                Duration::from_secs(5),
            )
            .await;
            #[cfg(windows)]
            let termination = recovery::terminate_job(
                view.held
                    .as_deref()
                    .ok_or("The service no longer holds the agent's job")?,
            )
            .await;
            let mut e = engine.lock().await;
            e.chat(id)?;
            e.record(
                id,
                "recoveryTerminated",
                termination.event(view.message_id.as_deref()),
            )?;
            Ok(
                json!({"schemaVersion":1,"confirmed":termination.confirmed,"detail":termination.detail}),
            )
        }
        "conversationRecoveryFork" => {
            if view.phase != Phase::RecoveryRequired {
                return Err(
                    "Only a conversation that needs recovery can be continued elsewhere".into(),
                );
            }
            if probed.state != recovery::ProcessState::Exited {
                return Err("The old agent process is not proven to have exited".into());
            }
            let message = view.message_id.clone().unwrap_or_default();
            let last = view.events.last().map_or(0, |ev| ev.sequence);
            let context = recovery::recovery_context(&view.events, id, &message, last);
            let mut e = engine.lock().await;
            let original = e.chat(id)?.clone();
            let mut fork = Conversation::new(
                &original.agent_id,
                original.requested_model.clone(),
                original.executable.clone(),
            )?;
            fork.options = original.options.clone();
            fork.full_access = original.full_access;
            fork.site = original.site.clone();
            fork.title = original.title.clone();
            e.store.create(&fork)?;
            let fork_id = fork.id.clone();
            e.controls.insert(fork_id.clone(), fork.control());
            e.chats.insert(fork_id.clone(), fork);
            e.evidence.loaded.insert(fork_id.clone());
            e.record(&fork_id, "recoveryContext", context)?;
            e.record(
                id,
                "recoveryForked",
                recovery::recovery_forked(Some(&message), &fork_id),
            )?;
            let summary = e.chat(&fork_id)?.summary();
            Ok(json!({"schemaVersion":1,"conversation":summary,"forkedFrom":id}))
        }
        _ => Err("Unsupported recovery operation".into()),
    }
}

/// The 1 s progress check: `noActivity` for running turns.
pub(super) async fn progress_tick(engine: &Arc<Mutex<Engine>>) -> Result<(), String> {
    let mut e = engine.lock().await;
    let now = Instant::now();
    let ids: Vec<String> = e.evidence.detectors.keys().cloned().collect();
    for id in ids {
        let Some(turn) = e.turns.get(&id) else {
            continue;
        };
        let message = turn.message_id.clone();
        let open: Vec<progress::OpenTool> = turn
            .open_tools
            .values()
            .map(|tool| progress::OpenTool {
                started: tool.started,
                timeout: None,
            })
            .collect();
        let running = e.chats.get(&id).is_some_and(|c| c.phase == Phase::Running);
        let pending = e.requests.keys().any(|(chat, _)| chat == &id);
        let signal = e
            .evidence
            .detectors
            .get_mut(&id)
            .and_then(|d| d.tick(now, running, pending, &open));
        if let Some(signal) = signal {
            e.journal_signals(&id, &message, vec![signal])?;
        }
    }
    Ok(())
}

/// The fork preface for a conversation's first message, when it continues a
/// recovered one.
pub(super) fn fork_preface(store: &Store, id: &str) -> Option<String> {
    let events = store.events_from(id, 1).ok()?;
    let context = events.iter().find(|e| e.kind == "recoveryContext")?;
    Some(recovery::fork_preface(&context.data))
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Chat {
        _dir: tempfile::TempDir,
        engine: Arc<Mutex<Engine>>,
        id: String,
        commands: mpsc::Receiver<BackendCommand>,
        /// The next Begin carries `restore: true`.
        restore_next: std::sync::atomic::AtomicBool,
    }

    async fn chat_with(agent: &str, allow_reuse: bool) -> Chat {
        let dir = tempfile::tempdir().unwrap();
        let engine = Arc::new(Mutex::new(Engine::new(
            Store::open(dir.path().join("conversations")).unwrap(),
            "fixture".into(),
            Default::default(),
        )));
        let created = dispatch(&engine, "conversationCreate", &json!({"agentId":agent}))
            .await
            .unwrap();
        let id = created["conversation"]["id"].as_str().unwrap().to_string();
        let (sender, commands) = mpsc::channel(32);
        {
            let mut e = engine.lock().await;
            e.jobs.insert(
                id.clone(),
                Job {
                    generation: 0,
                    full_access: false,
                    options: ModelOptions::default(),
                    retiring: false,
                    sender,
                },
            );
            if allow_reuse {
                e.reuse_allowlist = vec![(agent.into(), "1.0".into())];
            }
        }
        let c = Chat {
            _dir: dir,
            engine,
            id,
            commands,
            restore_next: Default::default(),
        };
        if allow_reuse {
            c.notice(
                "ready",
                json!({"sessionId":"s","backendVersion":"1.0","resumable":true,"mcpToolTimeoutSecs":120}),
            )
            .await;
        }
        c
    }

    impl Chat {
        async fn send(&self, text: &str, record: bool) -> String {
            let message = uuid::Uuid::new_v4().to_string();
            dispatch(
                &self.engine,
                "conversationSend",
                &json!({"conversationId":self.id,"messageId":message,"text":text,"record":record}),
            )
            .await
            .unwrap();
            message
        }
        async fn notice(&self, kind: &'static str, data: Value) {
            let mut e = self.engine.lock().await;
            let generation = e.chat(&self.id).unwrap().generation;
            e.notice(&self.id, generation, backend::notice(kind, data))
                .unwrap();
        }
        async fn complete(&self) {
            self.notice("terminal", json!({"status":"completed","error":null}))
                .await;
        }
        async fn op(&self, operation: &str, params: Value) -> Result<Value, String> {
            dispatch(&self.engine, operation, &params).await
        }
        fn deadline() -> proto::Deadline {
            proto::Deadline::after(proto::BEGIN_BUDGET).unwrap()
        }
        async fn begin_with(
            &self,
            generation: u64,
            tool: &str,
            class: ToolClass,
            outputs: Vec<proto::OutputRequest>,
            destination: Option<&str>,
        ) -> (String, Value) {
            let action = uuid::Uuid::new_v4().to_string();
            let d = Self::deadline();
            let begin = proto::ActionBegin {
                caller_conversation: self.id.clone(),
                generation,
                boot_id: d.boot_id,
                not_after: d.not_after,
                action_id: action.clone(),
                tool: tool.into(),
                tool_class: class,
                arguments_digest: format!("sha256:{tool}"),
                requested_destination: destination.map(String::from),
                outputs,
                requested: Some(
                    json!({"tabHandle":"chrome-tab:7","url":"https://shop.example/cart?token=SYNTH_TOKEN_REQ"}),
                ),
                restore: self
                    .restore_next
                    .swap(false, std::sync::atomic::Ordering::Relaxed),
            };
            let reply = self
                .op(proto::OP_ACTION_BEGIN, proto::encode(&begin).unwrap())
                .await
                .unwrap();
            (action, reply)
        }
        async fn begin(&self, tool: &str, class: ToolClass) -> String {
            let (action, reply) = self.begin_with(0, tool, class, vec![], None).await;
            assert_eq!(reply["admitted"], true, "{reply}");
            action
        }
        /// An acting call the agent marked `restore: true`, reported done.
        async fn acted(&self, tool: &str, restore: bool) -> String {
            self.restore_next
                .store(restore, std::sync::atomic::Ordering::Relaxed);
            let action = self.begin(tool, ToolClass::Act).await;
            self.send_report(&self.report(&action, tool, ToolClass::Act))
                .await;
            action
        }
        /// A passing `expect` with this predicate.
        async fn expected(&self, predicate: &str) -> String {
            let action = self.begin("chrome_use_expect", ToolClass::Observe).await;
            self.send_report(&expect_report(self, &action, predicate, true))
                .await;
            action
        }
        fn report(&self, action: &str, tool: &str, class: ToolClass) -> proto::ActionReport {
            proto::ActionReport {
                caller_conversation: self.id.clone(),
                generation: 0,
                action_id: action.into(),
                tool: tool.into(),
                tool_class: class,
                arguments_digest: format!("sha256:{tool}"),
                outcome: proto::Outcome::Ok,
                error_code: None,
                action_context: Some(
                    json!({"actionId":action,"after":{"targetId":"T1","url":"https://shop.example/orders","provenance":"actionReply"}}),
                ),
                verdict: Some(proto::VerdictReport {
                    verdict: Verdict::Uncertain,
                    basis: Basis::None,
                    possibly_executed: true,
                    check: None,
                    detail: None,
                }),
                observation: None,
                result_digest: None,
                duration_ms: 5,
                artifacts: vec![],
            }
        }
        async fn send_report(&self, report: &proto::ActionReport) -> Value {
            self.op(proto::OP_ACTION_REPORT, proto::encode(report).unwrap())
                .await
                .unwrap()
        }
        async fn events(&self) -> Vec<Value> {
            let events = self.engine.lock().await.store.all_events(&self.id).unwrap();
            events
                .into_iter()
                .map(|e| json!({"sequence":e.sequence,"kind":e.kind,"data":e.data}))
                .collect()
        }
        async fn kinds(&self, kind: &str) -> Vec<Value> {
            self.events()
                .await
                .into_iter()
                .filter(|e| e["kind"] == kind)
                .map(|e| e["data"].clone())
                .collect()
        }
        async fn assess(&self, generation: u64, assessment: Value) -> Result<Value, String> {
            let d = Self::deadline();
            self.op(
                proto::OP_TASK_ASSESS,
                proto::encode(&proto::TaskAssess {
                    caller_conversation: self.id.clone(),
                    generation,
                    boot_id: d.boot_id,
                    not_after: d.not_after,
                    assessment,
                })
                .unwrap(),
            )
            .await
        }
        fn drain(&mut self) {
            while self.commands.try_recv().is_ok() {}
        }
    }

    fn expect_report(chat: &Chat, action: &str, predicate: &str, ok: bool) -> proto::ActionReport {
        let mut report = chat.report(action, "chrome_use_expect", ToolClass::Observe);
        report.verdict = Some(proto::VerdictReport {
            verdict: if ok {
                Verdict::Confirmed
            } else {
                Verdict::Failed
            },
            basis: Basis::Expect,
            possibly_executed: false,
            check: Some(proto::Check {
                kind: proto::CheckKind::Expect,
                predicate: predicate.into(),
            }),
            detail: None,
        });
        report
    }

    #[tokio::test]
    async fn begin_is_admitted_only_into_the_live_running_turn() {
        let c = chat_with("claude", false).await;
        // Idle: no running turn.
        let (_, refused) = c
            .begin_with(0, "chrome_use_click", ToolClass::Act, vec![], None)
            .await;
        assert_eq!(refused["admitted"], false);
        let message = c.send("go", false).await;
        let (action, admitted) = c
            .begin_with(0, "chrome_use_click", ToolClass::Act, vec![], None)
            .await;
        assert_eq!(admitted["admitted"], true);
        assert_eq!(admitted["messageId"], message);
        let lease = &c.kinds("actionAdmitted").await[0];
        assert_eq!(lease["actionId"], action);
        assert_eq!(lease["messageId"], message);
        // Another generation, a passed or too distant deadline, a reused id.
        let (_, stale) = c
            .begin_with(7, "chrome_use_click", ToolClass::Act, vec![], None)
            .await;
        assert_eq!(stale["admitted"], false);
        let d = Chat::deadline();
        for (boot, not_after) in [
            (d.boot_id.clone(), d.not_after - 6_000),
            (d.boot_id.clone(), d.not_after + 2_000),
            ("another boot".to_string(), d.not_after),
        ] {
            let begin = json!({"callerConversation":c.id,"generation":0,"bootId":boot,"notAfter":not_after,"actionId":uuid::Uuid::new_v4().to_string(),"tool":"chrome_use_click","toolClass":"act","argumentsDigest":"sha256:x","requestedDestination":null,"outputs":[]});
            let reply = c.op(proto::OP_ACTION_BEGIN, begin).await.unwrap();
            assert_eq!(reply["admitted"], false, "{reply}");
        }
        let begin = json!({"callerConversation":c.id,"generation":0,"bootId":d.boot_id,"notAfter":d.not_after,"actionId":action,"tool":"chrome_use_click","toolClass":"act","argumentsDigest":"sha256:x","requestedDestination":null,"outputs":[]});
        assert_eq!(
            c.op(proto::OP_ACTION_BEGIN, begin).await.unwrap()["admitted"],
            false
        );
        // Stopping refuses; the turn's end retires the generation for good.
        c.op("conversationStop", json!({"conversationId":c.id}))
            .await
            .unwrap();
        let (_, stopping) = c
            .begin_with(0, "chrome_use_click", ToolClass::Act, vec![], None)
            .await;
        assert_eq!(stopping["admitted"], false);
        c.notice("terminal", json!({"status":"interrupted","error":null}))
            .await;
        let (_, retired) = c
            .begin_with(0, "chrome_use_click", ToolClass::Act, vec![], None)
            .await;
        assert_eq!(retired["admitted"], false);
        // RecoveryRequired refuses too.
        c.engine.lock().await.chat(&c.id).unwrap().phase = Phase::RecoveryRequired;
        let (_, recovery) = c
            .begin_with(0, "chrome_use_click", ToolClass::Act, vec![], None)
            .await;
        assert_eq!(recovery["admitted"], false);
    }

    #[tokio::test]
    async fn reports_complete_their_own_lease_once_and_late_ones_stay_in_their_turn() {
        let mut c = chat_with("claude", true).await;
        let first = c.send("turn A", false).await;
        let reported = c.begin("chrome_use_click", ToolClass::Act).await;
        let late = c.begin("chrome_use_click", ToolClass::Act).await;
        let report = c.report(&reported, "chrome_use_click", ToolClass::Act);
        let reply = c.send_report(&report).await;
        assert_eq!(reply["bound"], true);
        assert_eq!(reply["verdict"], "uncertain");
        // A retry gets the first answer and records nothing.
        let before = c.events().await.len();
        assert_eq!(c.send_report(&report).await, reply);
        assert_eq!(c.events().await.len(), before);
        // Turn A ends with one lease still open; B starts on the same process.
        c.complete().await;
        let ended = c.kinds("turnEnded").await;
        assert_eq!(ended[0]["openLeases"], json!([late]));
        c.drain();
        let second = c.send("turn B", false).await;
        assert_ne!(first, second);
        let late_reply = c
            .send_report(&c.report(&late, "chrome_use_click", ToolClass::Act))
            .await;
        assert_eq!(late_reply["bound"], true);
        let targets = c.kinds("browserActionTarget").await;
        let late_target = targets.iter().find(|t| t["actionId"] == late).unwrap();
        assert_eq!(
            late_target["requested"],
            json!({"tabHandle":"chrome-tab:7","url":"https://shop.example/cart?token=[redacted]"})
        );
        assert_eq!(late_target["messageId"], first);
        assert_eq!(late_target["afterTurnEnd"], true);
        assert_eq!(late_target["lateReported"], true);
        let observed = c.kinds("actionObserved").await;
        assert!(observed.iter().any(|o| o["actionId"] == late
            && o["lateReported"] == true
            && o["messageId"] == first));
        // Again: recorded once.
        let before = c.events().await.len();
        c.send_report(&c.report(&late, "chrome_use_click", ToolClass::Act))
            .await;
        assert_eq!(c.events().await.len(), before);
        // An open lease retires even an allowlisted generation.
        assert_eq!(c.kinds("generationRetired").await[0]["reason"], "openLease");
    }

    #[tokio::test]
    async fn every_open_lease_is_closed_on_reload_beyond_the_display_list() {
        let c = chat_with("claude", false).await;
        let message = c.send("many", false).await;
        let mut actions = Vec::new();
        for _ in 0..65 {
            actions.push(c.begin("chrome_use_click", ToolClass::Act).await);
        }
        c.complete().await;
        let listed = c.kinds("turnEnded").await[0]["openLeases"]
            .as_array()
            .unwrap()
            .len();
        assert_eq!(listed, 64);
        let root = c.engine.lock().await.store.directory.clone();
        let engine = Arc::new(Mutex::new(Engine::new(
            Store::open(root).unwrap(),
            "fixture".into(),
            Default::default(),
        )));
        for action in &actions {
            dispatch(
                &engine,
                proto::OP_ACTION_REPORT,
                &proto::encode(&c.report(action, "chrome_use_click", ToolClass::Act)).unwrap(),
            )
            .await
            .unwrap();
        }
        let events = engine.lock().await.store.all_events(&c.id).unwrap();
        let late: Vec<_> = events
            .iter()
            .filter(|e| e.kind == "browserActionTarget")
            .collect();
        assert_eq!(late.len(), 65);
        assert!(late
            .iter()
            .all(|e| e.data["lateReported"] == true && e.data["messageId"] == message.as_str()));
    }

    #[tokio::test]
    async fn unknown_leases_are_quarantined_and_never_join_a_turn() {
        let c = chat_with("claude", false).await;
        c.send("go", false).await;
        let other = chat_with("claude", false).await;
        other.send("elsewhere", false).await;
        let foreign = other.begin("chrome_use_click", ToolClass::Act).await;
        for (action, reason) in [
            (uuid::Uuid::new_v4().to_string(), "noLease"),
            (foreign.clone(), "noLease"),
            ("not-a-uuid".to_string(), "malformed"),
        ] {
            let reply = c
                .send_report(&c.report(&action, "chrome_use_click", ToolClass::Act))
                .await;
            assert_eq!(reply, json!({"bound": false}));
            let unbound = c.kinds("unboundActionReport").await;
            assert_eq!(unbound.last().unwrap()["reason"], reason, "{action}");
            assert!(unbound.last().unwrap().get("messageId").is_none());
        }
        assert!(c.kinds("browserActionTarget").await.is_empty());
        c.complete().await;
        let summary = &c.kinds("turnSummary").await[0];
        assert_eq!(summary["actions"]["confirmed"], 0);
        assert_eq!(summary["actions"]["uncertain"], 0);
    }

    #[tokio::test]
    async fn a_report_cut_short_by_a_crash_reloads_with_interrupted_states() {
        let c = chat_with("claude", false).await;
        c.send("go", false).await;
        let (action, reply) = c
            .begin_with(
                0,
                "chrome_use_screenshot",
                ToolClass::Capture,
                vec![proto::OutputRequest {
                    kind: proto::ArtifactKind::Screenshot,
                    name: "shot.png".into(),
                }],
                None,
            )
            .await;
        let artifact = reply["outputs"][0]["artifactId"]
            .as_str()
            .unwrap()
            .to_string();
        // Lock 1 ran (target journaled), then the service died.
        let report = c.report(&action, "chrome_use_screenshot", ToolClass::Capture);
        {
            let mut e = c.engine.lock().await;
            assert!(matches!(
                e.report_open(&report).unwrap(),
                ReportStep::Ingest { .. }
            ));
            // A concurrent duplicate while ingesting is told to wait.
            assert!(matches!(
                e.report_open(&report).unwrap(),
                ReportStep::Reply(ref r) if r == &json!({"state":"pending"})
            ));
        }
        let root = c.engine.lock().await.store.directory.clone();
        let engine = Arc::new(Mutex::new(Engine::new(
            Store::open(root).unwrap(),
            "fixture".into(),
            Default::default(),
        )));
        let retry = dispatch(
            &engine,
            proto::OP_ACTION_REPORT,
            &proto::encode(&report).unwrap(),
        )
        .await
        .unwrap();
        assert_eq!(retry["verdict"], "failed");
        assert_eq!(retry["artifacts"][0]["state"], "unavailable");
        assert_eq!(retry["artifacts"][0]["reason"], "interrupted");
        let events = engine.lock().await.store.all_events(&c.id).unwrap();
        let unavailable = events
            .iter()
            .find(|e| e.kind == "artifactUnavailable" && e.data["artifactId"] == artifact.as_str())
            .unwrap();
        assert_eq!(unavailable.data["detectedBy"], "load");
        assert_eq!(
            events.iter().filter(|e| e.kind == "actionObserved").count(),
            1
        );
    }

    fn png() -> Vec<u8> {
        let mut bytes = Vec::new();
        image::RgbImage::new(2, 2)
            .write_to(
                &mut std::io::Cursor::new(&mut bytes),
                image::ImageFormat::Png,
            )
            .unwrap();
        bytes
    }

    #[tokio::test]
    async fn captures_are_ingested_read_back_and_delivered_separately() {
        let c = chat_with("claude", false).await;
        c.send("shoot", false).await;
        let (action, reply) = c
            .begin_with(
                0,
                "chrome_use_screenshot",
                ToolClass::Capture,
                vec![proto::OutputRequest {
                    kind: proto::ArtifactKind::Screenshot,
                    name: "page.png".into(),
                }],
                Some("/Users/me/Desktop/page.png"),
            )
            .await;
        let output = &reply["outputs"][0];
        let path = output["path"].as_str().unwrap().to_string();
        assert!(path.contains(".chrome-use-out") && path.contains(&action));
        let artifact = output["artifactId"].as_str().unwrap().to_string();
        let created = c.kinds("artifactCreated").await;
        assert_eq!(created.len(), 1);
        assert_eq!(created[0]["artifactId"], artifact);
        assert_eq!(created[0]["actionId"], action);
        assert_eq!(created[0]["kind"], "screenshot");
        assert_eq!(created[0]["state"], "pending");
        let bytes = png();
        std::fs::write(&path, &bytes).unwrap();
        let mut report = c.report(&action, "chrome_use_screenshot", ToolClass::Capture);
        report.artifacts = vec![proto::ReportedArtifact {
            artifact_id: artifact.clone(),
            path: path.clone(),
        }];
        let reply = c.send_report(&report).await;
        assert_eq!(reply["verdict"], "confirmed");
        assert_eq!(reply["artifacts"][0]["state"], "ready");
        let digest = reply["artifacts"][0]["digest"]
            .as_str()
            .unwrap()
            .to_string();
        let observed = &c.kinds("actionObserved").await[0];
        assert_eq!(observed["basis"], "capture");
        assert_eq!(observed["evaluator"], "host");
        // actionObserved is the last event of the report.
        let events = c.events().await;
        let ready = events
            .iter()
            .position(|e| e["kind"] == "artifactReady")
            .unwrap();
        let verdict = events
            .iter()
            .position(|e| e["kind"] == "actionObserved")
            .unwrap();
        assert!(ready < verdict);
        // The panel reads the bytes back, never a path.
        let chunk = c
            .op(
                "conversationArtifactRead",
                json!({"conversationId":c.id,"artifactId":artifact,"offset":0,"length":393216}),
            )
            .await
            .unwrap();
        use base64::Engine as _;
        assert_eq!(
            base64::engine::general_purpose::STANDARD
                .decode(chunk["data"].as_str().unwrap())
                .unwrap(),
            bytes
        );
        assert_eq!(chunk["digest"], digest);
        assert!(!chunk.to_string().contains(".chrome-use-out"));
        // Another conversation cannot read it.
        let other = chat_with("claude", false).await;
        assert!(other
            .op(
                "conversationArtifactRead",
                json!({"conversationId":other.id,"artifactId":artifact,"offset":0}),
            )
            .await
            .is_err());
        // Delivery is its own record, checked against the artifact digest.
        let delivery = |digest: &str| json!({"callerConversation":c.id,"generation":0,"actionId":action,"artifactId":artifact,"state":"delivered","digest":digest,"detail":null});
        let delivered = c
            .op(proto::OP_DELIVERY_REPORT, delivery(&digest))
            .await
            .unwrap();
        assert_eq!(delivered["state"], "delivered");
        // Idempotent: a later claim changes nothing.
        let again = c
            .op(proto::OP_DELIVERY_REPORT, delivery("sha256:00"))
            .await
            .unwrap();
        assert_eq!(again["state"], "delivered");
        assert_eq!(c.kinds("fileDelivered").await.len(), 1);
    }

    #[tokio::test]
    async fn an_ingest_that_outlives_its_caller_still_completes_the_lease() {
        let c = chat_with("claude", false).await;
        c.send("shoot", false).await;
        let (action, reply) = c
            .begin_with(
                0,
                "chrome_use_screenshot",
                ToolClass::Capture,
                vec![proto::OutputRequest {
                    kind: proto::ArtifactKind::Screenshot,
                    name: "page.png".into(),
                }],
                None,
            )
            .await;
        let output = &reply["outputs"][0];
        std::fs::write(output["path"].as_str().unwrap(), png()).unwrap();
        let mut report = c.report(&action, "chrome_use_screenshot", ToolClass::Capture);
        report.artifacts = vec![proto::ReportedArtifact {
            artifact_id: output["artifactId"].as_str().unwrap().into(),
            path: output["path"].as_str().unwrap().into(),
        }];
        c.engine.lock().await.evidence.ingest_delay = Some(Duration::from_millis(400));
        // The caller gives up before the ingest finishes.
        let params = proto::encode(&report).unwrap();
        assert!(tokio::time::timeout(
            Duration::from_millis(50),
            dispatch(&c.engine, proto::OP_ACTION_REPORT, &params),
        )
        .await
        .is_err());
        // Meanwhile a retry is told to wait; afterwards it gets the result.
        assert_eq!(c.send_report(&report).await, json!({"state":"pending"}));
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        loop {
            let answer = c.send_report(&report).await;
            if answer["bound"] == true {
                assert_eq!(answer["verdict"], "confirmed");
                assert_eq!(answer["artifacts"][0]["state"], "ready");
                break;
            }
            assert!(tokio::time::Instant::now() < deadline, "still {answer}");
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        assert_eq!(c.kinds("actionObserved").await.len(), 1);
    }

    #[tokio::test]
    async fn a_file_not_at_the_allocated_path_is_never_read() {
        let c = chat_with("claude", false).await;
        c.send("shoot", false).await;
        let (action, reply) = c
            .begin_with(
                0,
                "chrome_use_screenshot",
                ToolClass::Capture,
                vec![proto::OutputRequest {
                    kind: proto::ArtifactKind::Screenshot,
                    name: "page.png".into(),
                }],
                None,
            )
            .await;
        let artifact = reply["outputs"][0]["artifactId"]
            .as_str()
            .unwrap()
            .to_string();
        let elsewhere = c._dir.path().join("elsewhere.png");
        std::fs::write(&elsewhere, png()).unwrap();
        let mut report = c.report(&action, "chrome_use_screenshot", ToolClass::Capture);
        report.artifacts = vec![proto::ReportedArtifact {
            artifact_id: artifact,
            path: elsewhere.to_string_lossy().into_owned(),
        }];
        let reply = c.send_report(&report).await;
        assert_eq!(reply["verdict"], "failed");
        assert_eq!(reply["artifacts"][0]["reason"], "pathMismatch");
    }

    /// #893. A check made while a drawer was open is followed by the call
    /// that closes it again. Marked `restore`, that call does not make the
    /// check stale, and the reason says the check rests on the declaration.
    /// Everything else still fails closed.
    #[tokio::test]
    async fn a_declared_restore_keeps_the_check_before_it_and_nothing_else() {
        let requirement = |predicate: &str, evidence: &str| json!({"text":predicate,"status":"met","check":{"kind":"expect","predicate":predicate,"evidence":evidence}});
        let assessed = |list: Vec<Value>| json!({"status":"done","requirements":list});

        // The order the tool description asks for: the temporary state
        // first, put back with `restore`, then the other checks.
        let c = chat_with("claude", true).await;
        c.send("check the orders page", false).await;
        c.acted("chrome_use_click", false).await;
        let drawer = c.expected("text .drawer h2 contains Order 42").await;
        let close = c.acted("chrome_use_eval", true).await;
        let rows = c.expected("count table tr == 5").await;
        let total = c.expected("text #total contains 42").await;
        let out = c
            .assess(
                0,
                assessed(vec![
                    requirement("text .drawer h2 contains Order 42", &drawer),
                    requirement("count table tr == 5", &rows),
                    requirement("text #total contains 42", &total),
                ]),
            )
            .await
            .unwrap();
        assert_eq!(out["status"], "checksPassed", "{out}");
        assert_eq!(out["downgradedBecause"], json!([]), "{out}");
        let because = out["requirements"][0]["because"].as_str().unwrap();
        assert!(
            because.contains("declared restore and not verified") && because.contains(&close),
            "{because}"
        );
        for later in [1, 2] {
            assert_eq!(
                out["requirements"][later]["because"],
                "the browser recorded this check"
            );
        }
        let admitted = c.kinds("actionAdmitted").await;
        let marked: Vec<&Value> = admitted.iter().filter(|a| a["restore"] == true).collect();
        assert_eq!(marked.len(), 1, "{admitted:?}");
        assert_eq!(marked[0]["actionId"], close.as_str());

        // An unmarked acting call after the restore makes every check
        // before it stale again, and the reason names that call.
        let click = c.acted("chrome_use_click", false).await;
        let out = c
            .assess(
                0,
                assessed(vec![
                    requirement("text .drawer h2 contains Order 42", &drawer),
                    requirement("count table tr == 5", &rows),
                ]),
            )
            .await
            .unwrap();
        assert_eq!(out["status"], "notChecked", "{out}");
        for r in out["requirements"].as_array().unwrap() {
            assert_eq!(r["computed"], "stale", "{r}");
            let because = r["because"].as_str().unwrap();
            assert!(
                because.starts_with("a later action changed the same page: chrome_use_click")
                    && because.contains(&click),
                "{because}"
            );
        }

        // The order of the report in #893: three checks, then the click
        // that opens the drawer. That click is not a restore, so the three
        // are stale whatever the closing call says.
        let c = chat_with("claude", true).await;
        c.send("check the orders page", false).await;
        let rows = c.expected("count table tr == 5").await;
        let open = c.acted("chrome_use_click", false).await;
        let drawer = c.expected("text .drawer h2 contains Order 42").await;
        c.acted("chrome_use_eval", true).await;
        let out = c
            .assess(
                0,
                assessed(vec![
                    requirement("count table tr == 5", &rows),
                    requirement("text .drawer h2 contains Order 42", &drawer),
                ]),
            )
            .await
            .unwrap();
        assert_eq!(out["status"], "someChecksPassed", "{out}");
        assert_eq!(out["requirements"][0]["computed"], "stale");
        assert!(out["requirements"][0]["because"]
            .as_str()
            .unwrap()
            .contains(&open));
        assert_eq!(out["requirements"][1]["computed"], "checked");

        // A tool that navigates cannot be a restore: it is not admitted, so
        // the MCP side never runs it.
        c.restore_next
            .store(true, std::sync::atomic::Ordering::Relaxed);
        let (_, reply) = c
            .begin_with(0, "chrome_use_open", ToolClass::Act, vec![], None)
            .await;
        assert_eq!(reply["admitted"], false, "{reply}");
        assert_eq!(reply["reason"], "restore is not accepted on this tool");

        // The mark survives a restart: it is rebuilt from the journal.
        let events = c.engine.lock().await.store.all_events(&c.id).unwrap();
        let leases = evidence::rebuild(std::path::Path::new("/tmp"), &events);
        assert_eq!(leases.values().filter(|l| l.restore).count(), 1);
    }

    #[tokio::test]
    async fn assessments_check_only_this_turns_matching_evidence() {
        let c = chat_with("claude", true).await;
        c.send("place the order", false).await;
        let expect = c.begin("chrome_use_expect", ToolClass::Observe).await;
        c.send_report(&expect_report(
            &c,
            &expect,
            "text #status contains Order placed",
            true,
        ))
        .await;
        let met = |predicate: &str, evidence: &str| json!({"status":"done","requirements":[{"text":"Order placed","status":"met","check":{"kind":"expect","predicate":predicate,"evidence":evidence}}]});
        let passed = c
            .assess(0, met("text #status contains Order placed", &expect))
            .await
            .unwrap();
        assert_eq!(passed["phase"], "provisional");
        assert_eq!(passed["status"], "checksPassed");
        assert_eq!(passed["requirements"][0]["computed"], "checked");
        // A copy of the recorded predicate with the operand quoted still
        // checks, and the card shows the predicate the action recorded; a
        // paraphrase that drops the selector does not.
        let copy = c
            .assess(0, met("text #status contains \"Order placed\"", &expect))
            .await
            .unwrap();
        assert_eq!(copy["requirements"][0]["computed"], "checked");
        assert_eq!(
            copy["requirements"][0]["check"]["predicate"],
            "text #status contains Order placed"
        );
        assert_eq!(
            copy["requirements"][0]["check"]["label"],
            "text #status contains \"Order placed\""
        );
        let paraphrase = c
            .assess(0, met("text == \"Order placed\"", &expect))
            .await
            .unwrap();
        assert_eq!(paraphrase["requirements"][0]["computed"], "claimed");
        // A label naming another selector and value than the recorded
        // check (acceptance check 3) is only claimed.
        let unrelated = c
            .assess(0, met("text #total contains 42", &expect))
            .await
            .unwrap();
        assert_eq!(unrelated["requirements"][0]["computed"], "claimed");
        assert_eq!(
            unrelated["requirements"][0]["because"],
            "the cited check records something else"
        );
        assert_eq!(unrelated["status"], "notChecked");
        assert_eq!(
            unrelated["downgradedBecause"][0],
            "requirement 1: the cited check records something else"
        );
        // No predicate named: only claimed, whatever the requirement says.
        let undescribed = |text: &str| json!({"status":"done","requirements":[{"text":text,"status":"met","check":{"kind":"expect","predicate":"","evidence":expect}}]});
        for text in ["Order placed", "Totals equal 42"] {
            let out = c.assess(0, undescribed(text)).await.unwrap();
            assert_eq!(out["requirements"][0]["computed"], "claimed");
            assert_eq!(
                out["requirements"][0]["because"],
                "the check was not described"
            );
        }
        // A failed expect fails the requirement whatever its label.
        let failing = c.begin("chrome_use_expect", ToolClass::Observe).await;
        c.send_report(&expect_report(&c, &failing, "text Paid", false))
            .await;
        let other = c.assess(0, met("text Paid", &failing)).await.unwrap();
        assert_eq!(other["requirements"][0]["computed"], "checkFailed");
        // Another kind than the one recorded is only claimed.
        let wrong_kind = c
            .assess(
                0,
                json!({"status":"done","requirements":[{"text":"x","status":"met","check":{"kind":"urlMatch","predicate":"shop.example","evidence":expect}}]}),
            )
            .await
            .unwrap();
        assert_eq!(wrong_kind["requirements"][0]["computed"], "claimed");
        let unknown = c
            .assess(
                0,
                met(
                    "text #status contains Order placed",
                    &uuid::Uuid::new_v4().to_string(),
                ),
            )
            .await
            .unwrap();
        assert_eq!(unknown["requirements"][0]["computed"], "claimed");
        assert_eq!(unknown["status"], "notChecked");
        // A later action on the same page makes the check stale.
        let click = c.begin("chrome_use_click", ToolClass::Act).await;
        c.send_report(&c.report(&click, "chrome_use_click", ToolClass::Act))
            .await;
        let stale = c
            .assess(0, met("text #status contains Order placed", &expect))
            .await
            .unwrap();
        assert_eq!(stale["requirements"][0]["computed"], "stale");
        // Final at turnEnded, recomputed; the badge never says more.
        c.assess(0, met("text #status contains Order placed", &expect))
            .await
            .unwrap();
        c.complete().await;
        let finals: Vec<Value> = c
            .kinds("taskAssessment")
            .await
            .into_iter()
            .filter(|a| a["phase"] == "final")
            .collect();
        assert_eq!(finals.len(), 1);
        assert_eq!(finals[0]["requirements"][0]["computed"], "stale");
        // Evidence from the earlier turn never checks the next one.
        c.send("next", false).await;
        let earlier = c
            .assess(0, met("text #status contains Order placed", &expect))
            .await
            .unwrap();
        assert_eq!(earlier["requirements"][0]["computed"], "claimed");
        // An interrupted turn caps the final status.
        c.notice("terminal", json!({"status":"interrupted","error":null}))
            .await;
        let last = c.kinds("taskAssessment").await.pop().unwrap();
        assert_eq!(last["phase"], "final");
        assert_ne!(last["status"], "checksPassed");
    }

    #[tokio::test]
    async fn a_turn_without_an_assessment_ends_not_checked_with_a_summary() {
        let c = chat_with("claude", false).await;
        let message = c.send("go", false).await;
        for _ in 0..4 {
            let read = c.begin("chrome_use_read", ToolClass::Observe).await;
            let mut report = c.report(&read, "chrome_use_read", ToolClass::Observe);
            report.verdict = None;
            report.result_digest = Some("sha256:same".into());
            c.send_report(&report).await;
        }
        let stalls = c.kinds("progressStalled").await;
        assert_eq!(stalls.len(), 1);
        assert_eq!(stalls[0]["kind"], "repeatedIdenticalResult");
        assert_eq!(stalls[0]["messageId"], message);
        c.notice(
            "contextCompacted",
            json!({"backend":"claude","trigger":"auto"}),
        )
        .await;
        let compacted = &c.kinds("contextCompacted").await[0];
        assert_eq!(compacted["messageId"], message);
        assert!(compacted["sourceSequences"].is_array());
        c.complete().await;
        let last = c.kinds("taskAssessment").await.pop().unwrap();
        assert_eq!(last["source"], "host");
        assert_eq!(last["status"], "notChecked");
        assert_eq!(last["reason"], "notAssessed");
        let summary = &c.kinds("turnSummary").await[0];
        assert_eq!(summary["messageId"], message);
        assert_eq!(summary["assessment"], "notChecked");
        assert_eq!(summary["stalls"], 1);
        // Order at the end: turnEnded, final assessment, summary.
        let kinds: Vec<String> = c
            .events()
            .await
            .iter()
            .map(|e| e["kind"].as_str().unwrap().to_string())
            .collect();
        let end = kinds.iter().position(|k| k == "turnEnded").unwrap();
        let summary = kinds.iter().position(|k| k == "turnSummary").unwrap();
        assert!(end < summary);
    }

    #[tokio::test]
    async fn recording_needs_the_users_consent_for_this_message() {
        let c = chat_with("claude", false).await;
        c.send("no consent", false).await;
        let d = Chat::deadline();
        let caller = c.id.clone();
        let authorize = move |d: &proto::Deadline| {
            proto::encode(&proto::RecordingAuthorize {
                caller_conversation: caller.clone(),
                generation: 0,
                boot_id: d.boot_id.clone(),
                not_after: d.not_after,
            })
            .unwrap()
        };
        assert!(c
            .op(proto::OP_RECORDING_AUTHORIZE, authorize(&d))
            .await
            .unwrap_err()
            .contains("did not allow"));
        c.complete().await;
        let mut c = c;
        c.drain();
        // A retired generation's process exits; consent on the next message.
        {
            let mut e = c.engine.lock().await;
            e.jobs.get_mut(&c.id).unwrap().retiring = false;
        }
        let message = c.send("record it", true).await;
        let generation = c.engine.lock().await.chat(&c.id).unwrap().generation;
        {
            let mut e = c.engine.lock().await;
            let job = e.jobs.get_mut(&c.id).unwrap();
            job.generation = generation;
        }
        let d = Chat::deadline();
        let mut request = authorize(&d);
        request["generation"] = generation.into();
        let grant = c
            .op(proto::OP_RECORDING_AUTHORIZE, request.clone())
            .await
            .unwrap();
        assert!(grant["path"].as_str().unwrap().ends_with("recording.webm"));
        let created = &c.kinds("artifactCreated").await[0];
        assert_eq!(created["state"], "capturing");
        assert_eq!(created["messageId"], message);
        assert!(c.op(proto::OP_RECORDING_AUTHORIZE, request).await.is_err());
        // The turn ends while recording: the host owes a stop.
        c.complete().await;
        assert_eq!(
            c.engine.lock().await.evidence.pending_stops,
            vec![c.id.clone()]
        );
    }

    #[test]
    fn record_consent_keeps_old_ids_and_survives_queue_edits() {
        let mut message = Message {
            id: uuid::Uuid::new_v4().to_string(),
            text: "Summarize this page".into(),
            page: None,
            attachments: Vec::new(),
            skill: None,
            continuation: None,
            record: false,
            preface: None,
            tab_grant: None,
        };
        assert_eq!(
            message.fingerprint().unwrap(),
            "d598de4e6b338ec44e8abb56be9eedb3994e828291fc2a521d10951c6f29180f"
        );
        message.record = true;
        assert_ne!(
            message.fingerprint().unwrap(),
            "d598de4e6b338ec44e8abb56be9eedb3994e828291fc2a521d10951c6f29180f"
        );
        let mut chat = Conversation::new("claude", None, None).unwrap();
        chat.accept(message.clone()).unwrap();
        let replacement = Message {
            id: uuid::Uuid::new_v4().to_string(),
            record: false,
            text: "Edited".into(),
            ..message.clone()
        };
        chat.edit_queued(&message.id, &message.text, replacement)
            .unwrap();
        assert!(chat.queue[0].record);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn recovery_reviews_terminates_and_forks_only_on_proof() {
        let c = chat_with("claude", false).await;
        let message = c.send("go", false).await;
        let click = c.begin("chrome_use_click", ToolClass::Act).await;
        c.send_report(&c.report(&click, "chrome_use_click", ToolClass::Act))
            .await;
        // An unconfirmed process without group identity: nothing provable.
        c.notice(
            "processUnconfirmed",
            json!({"error":"lost","processExitConfirmed":false}),
        )
        .await;
        let review = c
            .op("conversationRecoveryReview", json!({"conversationId":c.id}))
            .await
            .unwrap();
        assert_eq!(review["process"]["state"], "unknown");
        assert_eq!(review["messageId"], message);
        assert_eq!(review["actions"][0]["tool"], "chrome_use_click");
        assert!(c
            .op(
                "conversationRecoveryTerminate",
                json!({"conversationId":c.id})
            )
            .await
            .is_err());
        assert!(c
            .op("conversationRecoveryFork", json!({"conversationId":c.id}))
            .await
            .is_err());
        // A process record whose group is gone proves the exit.
        // A runner started by launchd can report session 1, which a record
        // rejects; the group is gone either way, so any valid session works.
        let sid = unsafe { libc::getsid(0) }.max(2);
        {
            let mut e = c.engine.lock().await;
            e.record(
                &c.id,
                "ready",
                json!({"sessionId":"s","pid":99_999_901u64,"pgid":99_999_901u64,"processStartTime":1,"serviceSid":sid}),
            )
            .unwrap();
        }
        let review = c
            .op("conversationRecoveryReview", json!({"conversationId":c.id}))
            .await
            .unwrap();
        assert_eq!(review["process"]["state"], "exited", "{review}");
        let fork = c
            .op("conversationRecoveryFork", json!({"conversationId":c.id}))
            .await
            .unwrap();
        assert_eq!(fork["forkedFrom"], c.id);
        let fork_id = fork["conversation"]["id"].as_str().unwrap().to_string();
        let forked = c.kinds("recoveryForked").await;
        assert_eq!(forked[0]["forkedTo"], fork_id);
        let events = c.engine.lock().await.store.all_events(&fork_id).unwrap();
        assert_eq!(events[0].kind, "recoveryContext");
        assert_eq!(
            c.engine.lock().await.chat(&c.id).unwrap().phase,
            Phase::RecoveryRequired
        );
        // The fork's first message continues the recovered one.
        let mut e = c.engine.lock().await;
        let chat = e.chat(&fork_id).unwrap();
        chat.accept(Message {
            id: uuid::Uuid::new_v4().to_string(),
            text: "carry on".into(),
            page: None,
            attachments: Vec::new(),
            skill: None,
            continuation: None,
            record: false,
            preface: None,
            tab_grant: None,
        })
        .unwrap();
        let next = e.begin(&fork_id).unwrap().unwrap();
        assert_eq!(next.continuation, Some(Continuation::AfterRecovery));
        assert!(next
            .prompt_text()
            .starts_with("[This conversation continues one whose last request ended"));
        assert!(next.prompt_text().contains("click"));
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn the_held_job_of_an_unconfirmed_exit_proves_the_tree() {
        let c = chat_with("claude", false).await;
        c.send("go", false).await;
        let job = Arc::new(super::super::super::windows::ProcessJob::new().unwrap());
        let mut notice = backend::notice(
            "processUnconfirmed",
            json!({"error":"lost","processExitConfirmed":false}),
        );
        notice.job = Some(job);
        c.engine.lock().await.notice(&c.id, 0, notice).unwrap();
        assert!(c.engine.lock().await.held_jobs.contains_key(&c.id));
        // The job is empty: the tree is proven gone, so a fork is allowed.
        let review = c
            .op("conversationRecoveryReview", json!({"conversationId":c.id}))
            .await
            .unwrap();
        assert_eq!(review["process"]["state"], "exited", "{review}");
        assert!(c
            .op("conversationRecoveryFork", json!({"conversationId":c.id}))
            .await
            .is_ok());
        // After a restart the job is gone and nothing can be proven.
        let root = c.engine.lock().await.store.directory.clone();
        let engine = Arc::new(Mutex::new(Engine::new(
            Store::open(root).unwrap(),
            "fixture".into(),
            Default::default(),
        )));
        let review = dispatch(
            &engine,
            "conversationRecoveryReview",
            &json!({"conversationId":c.id}),
        )
        .await
        .unwrap();
        assert_eq!(review["process"]["state"], "unknown");
    }

    #[tokio::test]
    async fn stop_while_waiting_for_a_retiring_process_ends_the_turn_without_touching_it() {
        let mut c = chat_with("claude", false).await;
        c.engine.lock().await.retire_wait = Duration::from_secs(5);
        c.send("first", false).await;
        c.complete().await;
        c.drain();
        // The next message waits for the retired process to exit.
        let send = {
            let engine = c.engine.clone();
            let id = c.id.clone();
            tokio::spawn(async move {
                dispatch(
                    &engine,
                    "conversationSend",
                    &json!({"conversationId":id,"messageId":uuid::Uuid::new_v4().to_string(),"text":"second"}),
                )
                .await
            })
        };
        tokio::time::sleep(Duration::from_millis(100)).await;
        c.op("conversationStop", json!({"conversationId":c.id}))
            .await
            .unwrap();
        // The retiring process got no interrupt.
        assert!(!matches!(
            c.commands.try_recv(),
            Ok(BackendCommand::Interrupt)
        ));
        let ended = c.kinds("turnEnded").await;
        assert_eq!(ended.last().unwrap()["outcome"], "interrupted");
        assert_eq!(
            c.engine.lock().await.chat(&c.id).unwrap().phase,
            Phase::Interrupted
        );
        // The old process exits; nothing is started for the stopped turn.
        {
            let mut e = c.engine.lock().await;
            e.notice(&c.id, 0, backend::notice("exited", json!({"error":null})))
                .unwrap();
        }
        send.await.unwrap().unwrap();
        assert!(c.kinds("turnStartFailed").await.is_empty());
        assert!(!c.engine.lock().await.jobs.contains_key(&c.id));
    }
}
