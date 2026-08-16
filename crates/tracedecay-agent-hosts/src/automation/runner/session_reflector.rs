use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use std::{path::PathBuf, sync::Arc};
use tracedecay_store::ProjectMemoryFactStore;

use crate::automation::automatic_facts::{
    AutomaticFactReceipt, AutomaticFactState, SettledAutomaticFactReceipt,
    record_session_automatic_facts,
};
use crate::automation::backend::{
    AgentTaskBackend, AgentTaskKind, AgentTaskRequest, AgentTaskResponse, AgentTaskRetryReport,
};
use crate::automation::config::AutomationConfig;
use crate::automation::lifecycle::{
    AgentRunFinalizer, AgentTaskRunContext, AutomationCommittedReceipt, AutomationRunControl,
    AutomationRunError, AutomationRunLedgerPublication, AutomationRunResult,
    AutomationRunSettlementGuard, BackendTaskRun, NonEmptyAutomaticFactReceipts,
    RetainedAutomationRun, SchedulerGate, failed_backend_fallback_report,
};
use crate::automation::run_ledger::{AutomationRunLedgerRecord, AutomationTrigger};
use crate::automation::session_reflector::validate_fact_candidates;
use crate::errors::{Result, TraceDecayError};
use crate::ports::project_runtime::TraceDecay;
use crate::ports::session_evidence::{LcmGrepSort, LcmScope};
use crate::store::memory::DatabaseFactStore;
use tracedecay_domain::configuration::ConfigurationRevisionId;
use tracedecay_global_db::RegisteredGlobalDbLeaseV1;
use tracedecay_runtime_core::tracedecay::current_timestamp;
use tracedecay_usecases::memory::MemoryApplication;

use super::curation::{evaluate_session_curation, unpersisted_rejected_parts};
use super::evidence::{
    SessionReflectorEvidenceBundle, SessionReflectorEvidenceOutcome,
    build_session_reflector_evidence,
};
use super::retrieval::{AutomationSessionRetrieval, production_project_automation_retrieval};

mod privacy;
use privacy::{
    fact_collection_summary, session_fact_finalization_failure_summary,
    session_fact_ledger_summary, validation_repairs_summary,
};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionReflectorAutomationOptions {
    #[serde(default)]
    pub trigger: AutomationTrigger,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
    #[serde(default = "default_session_provider")]
    pub provider: String,
    #[serde(default = "default_session_reflection_query")]
    pub query: String,
    #[serde(default = "default_lcm_grep_scope")]
    pub scope: LcmScope,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(default = "default_include_summaries")]
    pub include_summaries: bool,
    #[serde(default = "default_session_evidence_limit")]
    pub evidence_limit: usize,
    /// When true, include bounded turn-ordered slices of recently active
    /// sessions as a primary evidence channel alongside the keyword grep.
    #[serde(default = "default_include_recent_sessions")]
    pub include_recent_sessions: bool,
    /// How many recently active sessions to replay when `session_id` is not
    /// explicitly set.
    #[serde(default = "default_recent_sessions_limit")]
    pub recent_sessions_limit: usize,
    #[serde(default = "default_lcm_grep_sort")]
    pub sort: LcmGrepSort,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start_time: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub end_time: Option<i64>,
}

impl Default for SessionReflectorAutomationOptions {
    fn default() -> Self {
        Self {
            trigger: AutomationTrigger::ManualCli,
            run_id: None,
            provider: default_session_provider(),
            query: default_session_reflection_query(),
            scope: default_lcm_grep_scope(),
            session_id: None,
            include_summaries: default_include_summaries(),
            evidence_limit: default_session_evidence_limit(),
            include_recent_sessions: default_include_recent_sessions(),
            recent_sessions_limit: default_recent_sessions_limit(),
            sort: default_lcm_grep_sort(),
            source: None,
            role: None,
            start_time: None,
            end_time: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionReflectorAutomationRun {
    pub run_id: String,
    pub report: Value,
    pub ledger_record: AutomationRunLedgerRecord,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub backend_response: Option<AgentTaskResponse>,
    /// Canonical committed authority result retained in-process until the
    /// daemon persists the outer automation application terminal.
    #[serde(skip)]
    pub committed_receipt: Option<AutomationCommittedReceipt>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionFactCurationOutcome {
    Applied,
    NoCandidate,
    Quarantined,
    Partial,
    Retry,
}

impl SessionFactCurationOutcome {
    fn classify(
        admitted_count: usize,
        applied_count: usize,
        quarantined_count: usize,
        retry_required: bool,
    ) -> Self {
        if retry_required && applied_count == 0 {
            Self::Retry
        } else if admitted_count == 0 && quarantined_count == 0 {
            Self::NoCandidate
        } else if admitted_count == 0 {
            Self::Quarantined
        } else if retry_required || applied_count < admitted_count || quarantined_count > 0 {
            Self::Partial
        } else {
            Self::Applied
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionFactCurationReceipt {
    pub schema_version: u32,
    pub outcome: SessionFactCurationOutcome,
    pub repair_attempted: bool,
    pub admitted_count: usize,
    pub applied_count: usize,
    pub quarantined_count: usize,
    pub retry_required: bool,
    pub automatic_fact_receipt_ids: Vec<String>,
    pub applied_fact_ids: Vec<String>,
}

trait AutomaticFactReceiptSummary {
    fn summary_apply_id(&self) -> &str;
    fn summary_state(&self) -> AutomaticFactState;
    fn summary_applied_fact_id(&self) -> Option<&str>;
}

impl AutomaticFactReceiptSummary for AutomaticFactReceipt {
    fn summary_apply_id(&self) -> &str {
        &self.apply_id
    }

    fn summary_state(&self) -> AutomaticFactState {
        self.state
    }

    fn summary_applied_fact_id(&self) -> Option<&str> {
        self.applied_fact_id.as_deref()
    }
}

impl AutomaticFactReceiptSummary for SettledAutomaticFactReceipt {
    fn summary_apply_id(&self) -> &str {
        self.apply_id()
    }

    fn summary_state(&self) -> AutomaticFactState {
        self.state()
    }

    fn summary_applied_fact_id(&self) -> Option<&str> {
        self.applied_fact_id()
    }
}

fn automatic_fact_receipt_summary<T: AutomaticFactReceiptSummary>(receipt: &T) -> Value {
    json!({
        "apply_id": receipt.summary_apply_id(),
        "state": receipt.summary_state(),
        "applied_fact_id": receipt.summary_applied_fact_id(),
    })
}

fn session_fact_curation_receipt<T: AutomaticFactReceiptSummary>(
    admitted_count: usize,
    validation_quarantined_count: usize,
    repair_attempted: bool,
    retry_required: bool,
    receipts: &[T],
) -> SessionFactCurationReceipt {
    let applied_fact_ids = receipts
        .iter()
        .filter_map(|receipt| receipt.summary_applied_fact_id().map(ToOwned::to_owned))
        .collect::<Vec<_>>();
    let applied_count = receipts
        .iter()
        .filter(|receipt| receipt.summary_state() == AutomaticFactState::Applied)
        .count();
    let quarantined_count = validation_quarantined_count.saturating_add(
        receipts
            .iter()
            .filter(|receipt| receipt.summary_state() == AutomaticFactState::Quarantined)
            .count(),
    );
    SessionFactCurationReceipt {
        schema_version: 1,
        outcome: SessionFactCurationOutcome::classify(
            admitted_count,
            applied_count,
            quarantined_count,
            retry_required,
        ),
        repair_attempted,
        admitted_count,
        applied_count,
        quarantined_count,
        retry_required,
        automatic_fact_receipt_ids: receipts
            .iter()
            .map(|receipt| receipt.summary_apply_id().to_owned())
            .collect(),
        applied_fact_ids,
    }
}

pub(super) fn default_session_provider() -> String {
    "cursor".to_string()
}

fn default_lcm_grep_scope() -> LcmScope {
    LcmScope::All
}

fn default_include_summaries() -> bool {
    true
}

fn default_lcm_grep_sort() -> LcmGrepSort {
    LcmGrepSort::Recency
}

pub(super) fn default_session_reflection_query() -> String {
    "remember prefer decision requirement workflow".to_string()
}

fn default_session_evidence_limit() -> usize {
    20
}

pub(super) fn default_include_recent_sessions() -> bool {
    true
}

pub(super) fn default_recent_sessions_limit() -> usize {
    3
}

pub(super) fn build_session_reflector_prompt(evidence: &Value) -> String {
    const POLICY: &str = concat!(
        "Review these bounded TraceDecay session snippets and propose only durable memory facts.\n",
        "Evidence has two channels: recent_session_slices holds turn-ordered head/tail turns and summary nodes replayed from recently active sessions, and hits holds keyword search matches; both are citable.\n",
        "\n",
        "Signals worth capturing (any one is enough):\n",
        "- The user revealed durable preferences, persona, expectations, or ways they want the agent to operate.\n",
        "- The user corrected the agent's style, tone, format, verbosity, workflow, or approach. Frustration signals like 'stop doing X', 'this is too verbose', 'don't format like this', or an explicit 'remember this' are FIRST-CLASS signals: capture the correction as a durable user_pref or decision fact so the next session starts already knowing. These corrections should also end up embedded in the skill that governs that class of task, not only in memory; the skill writer handles the skill side, but the fact must still be recorded here.\n",
        "- A durable project, tool, decision, or code-area fact emerged that a future session would need.\n",
        "\n",
        "Do NOT capture (these harden into stale or self-defeating rules):\n",
        "- Environment-dependent failures: missing binaries, 'command not found', unconfigured credentials, uninstalled packages, post-migration path mismatches. The user can fix these; they are not durable facts.\n",
        "- Negative claims about tools or features ('X is broken', 'Y does not work'). These harden into self-imposed refusals cited long after the actual problem was fixed. If a tool failed because of setup state, the durable fact is the FIX (install command, config step, env var), never 'this tool does not work'.\n",
        "- Session-specific transient errors that resolved before the session ended. If retrying worked, the lesson is the retry pattern, not the original failure.\n",
        "- One-off task narratives. A single 'summarize this' or 'analyze this PR' request is not a durable fact about the user or project.\n",
        "- Secrets, credentials, tokens, or ephemeral status.\n",
        "\n",
        "Proposing nothing is a real option when the session ran smoothly and revealed nothing durable, but do not reach for it as a default.\n",
        "\n",
        "Response contract: Return only JSON with a facts array. Each fact must include content, category, optional tags, optional entities, trust, source_span, and reason. Category must be one of general, user_pref, project, tool, decision, or code_area. Use trust, not confidence; trust must be a JSON number from 0.0 to 1.0. Do not use string labels like high, medium, or low. source_span must cite one bounded evidence hit by session_id plus message_id for raw messages, by store_id for raw messages, or by node_id for summaries. Do not include secrets or ephemeral status.\n",
    );
    format!(
        "{POLICY}{}",
        serde_json::to_string_pretty(evidence).unwrap_or_else(|_| "{}".to_string())
    )
}

pub(super) async fn validate_session_fact_candidates<A: ProjectMemoryFactStore>(
    memory: &MemoryApplication<A>,
    run_control: &AutomationRunControl,
    proposals: &[Value],
    evidence: &Value,
) -> Result<(Vec<Value>, Vec<Value>)> {
    validate_fact_candidates(memory, run_control, proposals, evidence).await
}

async fn skipped_session_reflector_run(
    run: &AgentTaskRunContext<'_>,
    reason: &str,
    evidence_hash: Option<String>,
) -> Result<SessionReflectorAutomationRun> {
    let (report, record) = run
        .skipped_parts(evidence_hash, reason, Some("session_reflector"))
        .await?;
    Ok(SessionReflectorAutomationRun {
        run_id: run.run_id.clone(),
        report,
        ledger_record: record,
        backend_response: None,
        committed_receipt: None,
    })
}

pub(super) fn rejected_session_reflector_run(
    run: &AgentTaskRunContext<'_>,
    config: &AutomationConfig,
    reason: &str,
    evidence_hash: Option<String>,
) -> SessionReflectorAutomationRun {
    let (report, record) = unpersisted_rejected_parts(
        run,
        config,
        AgentTaskKind::SessionReflector,
        reason,
        evidence_hash,
        "session_reflector",
    );
    SessionReflectorAutomationRun {
        run_id: run.run_id.clone(),
        report,
        ledger_record: record,
        backend_response: None,
        committed_receipt: None,
    }
}

/// Validates and automatically applies the `facts` half of a reflector (or
/// combined) run, returning the report plus the not-yet-appended ledger record.
pub(super) struct ProposedAgentOutput<'a> {
    pub(super) response: &'a AgentTaskResponse,
    pub(super) retry_report: &'a AgentTaskRetryReport,
    pub(super) evidence: &'a Value,
    pub(super) evidence_hash: Option<String>,
    pub(super) proposals: &'a [Value],
}

pub(super) enum SessionReflectorFinalization {
    Completed {
        report: Value,
        record: AutomationRunLedgerRecord,
        committed_receipt: Option<AutomationCommittedReceipt>,
    },
    FailedRecorded {
        run_id: String,
        committed_receipt: AutomationCommittedReceipt,
        record: Option<AutomationRunLedgerRecord>,
        detail: &'static str,
    },
}

pub(super) async fn finalize_session_reflector_success<A: ProjectMemoryFactStore>(
    memory: &MemoryApplication<A>,
    run_control: &AutomationRunControl,
    config: &AutomationConfig,
    authority: &tracedecay_policy::CurationApplyAuthorityV1,
    finalizer: &AgentRunFinalizer<'_>,
    output: ProposedAgentOutput<'_>,
    validation_repairs: &[Value],
) -> Result<SessionReflectorFinalization> {
    let ProposedAgentOutput {
        response,
        retry_report,
        evidence,
        evidence_hash,
        proposals,
    } = output;
    let run_id = finalizer.run_id();
    let (accepted_facts, rejected_facts) =
        validate_session_fact_candidates(memory, run_control, proposals, evidence).await?;
    let curation_decision =
        evaluate_session_curation(config, authority, evidence_hash.as_deref(), &accepted_facts)?;
    let mut terminal_rejections = rejected_facts.clone();
    let admitted_facts = if curation_decision.allows_apply() {
        accepted_facts.as_slice()
    } else {
        terminal_rejections.extend(accepted_facts.iter().cloned());
        &[]
    };
    let admitted_count = admitted_facts.len();
    let quarantined_input_count = terminal_rejections.len();
    let apply_batch = record_session_automatic_facts(
        memory,
        run_control,
        run_id,
        evidence_hash.as_deref(),
        admitted_facts,
    )
    .await?;
    let automatic_fact_receipts = apply_batch.receipts;
    let settled_receipts = apply_batch.settled_receipts;
    let receipt = if apply_batch.retry_error.is_some() {
        session_fact_curation_receipt(
            admitted_count,
            quarantined_input_count,
            !validation_repairs.is_empty(),
            true,
            &settled_receipts,
        )
    } else {
        session_fact_curation_receipt(
            admitted_count,
            quarantined_input_count,
            !validation_repairs.is_empty(),
            false,
            &automatic_fact_receipts,
        )
    };
    let applied_receipt_ids: Vec<String> = if apply_batch.retry_error.is_some() {
        settled_receipts
            .iter()
            .filter(|record| record.state() == AutomaticFactState::Applied)
            .map(|record| record.apply_id().to_owned())
            .collect()
    } else {
        automatic_fact_receipts
            .iter()
            .filter(|record| record.state == AutomaticFactState::Applied)
            .map(|record| record.apply_id.clone())
            .collect()
    };
    let fully_applied = receipt.outcome == SessionFactCurationOutcome::Applied;
    if let Some(error) = apply_batch.retry_error {
        if settled_receipts.is_empty() {
            return Err(error);
        }
        let settled_values = settled_receipts
            .iter()
            .map(automatic_fact_receipt_summary)
            .collect::<Vec<_>>();
        let authority_quarantines = settled_receipts
            .iter()
            .filter(|receipt| receipt.state() == AutomaticFactState::Quarantined)
            .map(automatic_fact_receipt_summary)
            .collect::<Vec<_>>();
        let mut all_rejections = terminal_rejections.clone();
        all_rejections.extend(authority_quarantines.iter().cloned());
        let proposed_summary = session_fact_ledger_summary(
            proposals,
            &accepted_facts,
            admitted_facts,
            &terminal_rejections,
        )?;
        let rejected_summary = fact_collection_summary(&all_rejections)?;
        let repairs_summary = validation_repairs_summary(validation_repairs)?;
        let terminal_accepted_count = receipt.applied_count;
        let terminal_rejected_count = receipt.quarantined_count;
        let committed_receipt = NonEmptyAutomaticFactReceipts::from_vec(
            settled_receipts
                .into_iter()
                .map(SettledAutomaticFactReceipt::into_authority_result)
                .collect(),
        )
        .map(AutomationCommittedReceipt::AutomaticFacts)
        .ok_or_else(|| TraceDecayError::Config {
            message: "automatic fact settlement lost its committed authority receipt".to_owned(),
        })?;
        let record = match finalizer
            .append_failed_record_with_effects(
                response.model.clone(),
                evidence_hash,
                Some(proposed_summary),
                error.to_string(),
                retry_report,
                Some(json!({
                    "automatic_fact_receipts": settled_values,
                    "applied_receipt_ids": applied_receipt_ids,
                })),
                Some(rejected_summary),
                Some(json!({
                    "status": receipt.outcome,
                    "receipt": receipt,
                    "validation_repairs": repairs_summary,
                })),
                terminal_accepted_count,
                terminal_rejected_count,
            )
            .await
        {
            Ok(record) => Some(record),
            Err(_) => None,
        };
        return Ok(SessionReflectorFinalization::FailedRecorded {
            run_id: run_id.to_owned(),
            committed_receipt,
            record,
            detail: "Automatic facts committed, but their canonical projection failed; reconcile the committed receipt before another run.",
        });
    }
    terminal_rejections.extend(
        automatic_fact_receipts
            .iter()
            .filter(|receipt| receipt.state == AutomaticFactState::Quarantined)
            .map(automatic_fact_receipt_summary),
    );
    let curation_policy = json!({
        "decision": curation_decision,
        "effect": {
            "admitted_count": admitted_count,
            "applied_receipt_ids": applied_receipt_ids,
            "applied_fact_ids": receipt.applied_fact_ids,
            "applied_count": receipt.applied_count,
            "fully_applied": fully_applied,
            "mutates_store": receipt.applied_count > 0,
        },
    });
    let report = json!({
        "status": receipt.outcome,
        "dry_run": false,
        "task": "session_reflector",
        "receipt": receipt,
        "evidence_hash": evidence_hash,
        "accepted_facts": accepted_facts,
        "admitted_facts": admitted_facts,
        "quarantined_facts": terminal_rejections,
        "automatic_fact_receipts": automatic_fact_receipts,
        "curation_policy": curation_policy,
        "validation_repairs": validation_repairs,
    });
    let committed_receipt = NonEmptyAutomaticFactReceipts::from_vec(
        settled_receipts
            .into_iter()
            .map(SettledAutomaticFactReceipt::into_authority_result)
            .collect(),
    )
    .map(AutomationCommittedReceipt::AutomaticFacts);
    let mut record = match finalizer.success_record(
        response,
        report
            .get("evidence_hash")
            .and_then(Value::as_str)
            .map(str::to_string),
        Some(session_fact_ledger_summary(
            proposals,
            &accepted_facts,
            admitted_facts,
            &terminal_rejections,
        )?),
        receipt.applied_count,
        receipt.quarantined_count,
    ) {
        Ok(record) => record,
        Err(error) => {
            if let Some(committed_receipt) = committed_receipt {
                return Ok(SessionReflectorFinalization::FailedRecorded {
                    run_id: run_id.to_owned(),
                    committed_receipt,
                    record: None,
                    detail: "Automatic facts committed, but their exact completion time could not be recorded; reconcile the committed receipt before another run.",
                });
            }
            return Err(error);
        }
    };
    record.backend_attempt_count = retry_report.attempt_count();
    record.backend_attempts = retry_report.attempts().to_vec();
    record.applied_ops = report
        .pointer("/curation_policy/effect/applied_receipt_ids")
        .filter(|value| value.as_array().is_some_and(|items| !items.is_empty()))
        .cloned();
    record.rejected_ops = Some(fact_collection_summary(&terminal_rejections)?);
    let applied_receipt_ids = report
        .pointer("/curation_policy/effect/applied_receipt_ids")
        .cloned()
        .unwrap_or_else(|| json!([]));
    let mut validation_report = json!({
        "status": report.get("status").cloned().unwrap_or_else(|| json!("no_candidate")),
        "dry_run": report.get("dry_run").cloned().unwrap_or(json!(false)),
        "admitted_count": admitted_count,
        "quarantined_count": receipt.quarantined_count,
        "receipt": report.get("receipt").cloned().unwrap_or_else(|| json!({})),
        "validation_repairs": validation_repairs_summary(validation_repairs)?,
        "curation_policy": report.get("curation_policy").cloned().unwrap_or_else(|| json!({})),
    });
    if let Some(object) = validation_report.as_object_mut() {
        object.insert(
            "applied_receipts".to_string(),
            json!({
            "receipt_ids": applied_receipt_ids,
            "admitted_count": admitted_count,
            }),
        );
    }
    record.validation_report = Some(validation_report);
    Ok(SessionReflectorFinalization::Completed {
        report,
        record,
        committed_receipt,
    })
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn run_session_reflector_for_store<A: ProjectMemoryFactStore>(
    dashboard_root: PathBuf,
    sessions_db: RegisteredGlobalDbLeaseV1,
    retrieval: &dyn AutomationSessionRetrieval,
    memory: &MemoryApplication<A>,
    config: &AutomationConfig,
    run_control: &AutomationRunControl,
    authority: &tracedecay_policy::CurationApplyAuthorityV1,
    backend: &dyn AgentTaskBackend,
    options: SessionReflectorAutomationOptions,
    prebuilt_evidence: Option<SessionReflectorEvidenceBundle>,
) -> AutomationRunResult<SessionReflectorAutomationRun> {
    run_session_reflector_for_store_with_publication(
        dashboard_root,
        sessions_db,
        retrieval,
        memory,
        config,
        run_control,
        authority,
        backend,
        options,
        prebuilt_evidence,
        AutomationRunLedgerPublication::Immediate,
        None,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
async fn run_session_reflector_for_store_with_publication<A: ProjectMemoryFactStore>(
    dashboard_root: PathBuf,
    sessions_db: RegisteredGlobalDbLeaseV1,
    retrieval: &dyn AutomationSessionRetrieval,
    memory: &MemoryApplication<A>,
    config: &AutomationConfig,
    run_control: &AutomationRunControl,
    authority: &tracedecay_policy::CurationApplyAuthorityV1,
    backend: &dyn AgentTaskBackend,
    options: SessionReflectorAutomationOptions,
    prebuilt_evidence: Option<SessionReflectorEvidenceBundle>,
    ledger_publication: AutomationRunLedgerPublication,
    settlement_guard: Option<&AutomationRunSettlementGuard>,
) -> AutomationRunResult<SessionReflectorAutomationRun> {
    let mut run = AgentTaskRunContext::new(
        dashboard_root,
        sessions_db,
        options.run_id.clone(),
        "session_reflector",
        options.trigger,
        config,
        AgentTaskKind::SessionReflector,
    )
    .with_ledger_publication(ledger_publication)
    .with_settlement_guard(settlement_guard);
    let _run_lock = match run.gate().await? {
        SchedulerGate::Proceed(lock) => lock,
        SchedulerGate::Skip(reason) => {
            return skipped_session_reflector_run(&run, reason, None)
                .await
                .map_err(Into::into);
        }
    };
    let evidence_bundle = match prebuilt_evidence {
        Some(bundle) => bundle,
        None => match build_session_reflector_evidence(retrieval, &options).await? {
            SessionReflectorEvidenceOutcome::Ready(bundle) => bundle,
            SessionReflectorEvidenceOutcome::Skipped {
                reason,
                evidence_hash,
            } => {
                return Ok(rejected_session_reflector_run(
                    &run,
                    config,
                    reason,
                    evidence_hash,
                ));
            }
        },
    };
    let SessionReflectorEvidenceBundle {
        evidence,
        evidence_hash,
    } = evidence_bundle;
    crate::automation::outcomes::refresh_fact_outcomes(
        &run.dashboard_root,
        memory,
        current_timestamp(),
        run_control.read_control(),
    )
    .await?;

    let request = AgentTaskRequest::new(
        run.run_id.clone(),
        AgentTaskKind::SessionReflector,
        build_session_reflector_prompt(&evidence),
        evidence_hash.clone(),
        json!({
            "session_reflection_evidence": evidence,
            "apply": true,
        }),
    );
    let input_hash = Some(request.input_hash.clone());
    let finalizer = run.finalizer(input_hash.clone())?;
    let (mut response, mut retry_report) = match finalizer
        .run_backend_or_fallback(backend, &request, evidence_hash.clone())
        .await?
    {
        BackendTaskRun::Response {
            response,
            retry_report,
        } => (response, retry_report),
        BackendTaskRun::Fallback(record) => {
            let record = *record;
            return Ok(SessionReflectorAutomationRun {
                run_id: record.run_id.clone(),
                report: failed_backend_fallback_report(&record),
                ledger_record: record,
                backend_response: None,
                committed_receipt: None,
            });
        }
    };
    let (proposed_ops, mut proposals) = finalizer
        .response_output_array(
            &response,
            evidence_hash.clone(),
            &retry_report,
            "facts",
            "session reflector output must include a facts array",
        )
        .await?;
    let retry_policy =
        crate::automation::backend::BackendRetryPolicy::from_timeout_secs(config.timeout_secs);
    let mut validation_repairs = Vec::new();
    let (initial_accepted_facts, rejected_facts) =
        validate_session_fact_candidates(memory, run_control, &proposals, &evidence).await?;
    if !rejected_facts.is_empty() {
        validation_repairs.push(json!({
            "attempt": 1,
            "errors": rejected_facts,
        }));
        let repair_request = AgentTaskRequest::new(
            run.run_id.clone(),
            AgentTaskKind::SessionReflector,
            "Repair the previous session fact JSON. Return only {\"facts\": [...]}. Preserve valid intent, fix every validation error, cite only the supplied session evidence, and do not add unrelated facts."
                .to_string(),
            evidence_hash.clone(),
            json!({
                "previous_output": proposed_ops.clone(),
                "validation_errors": validation_repairs.last(),
                "session_reflection_evidence": evidence.clone(),
                "apply": true,
            }),
        );
        let mut repair_retry_report = AgentTaskRetryReport::default();
        response = match crate::automation::backend::run_agent_task_with_retry_report(
            backend,
            &repair_request,
            &retry_policy,
            &mut repair_retry_report,
        )
        .await
        {
            Ok(response) => response,
            Err(error) => {
                retry_report.append(repair_retry_report);
                let receipt = SessionFactCurationReceipt {
                    schema_version: 1,
                    outcome: SessionFactCurationOutcome::classify(
                        initial_accepted_facts.len(),
                        0,
                        rejected_facts.len(),
                        true,
                    ),
                    repair_attempted: true,
                    admitted_count: initial_accepted_facts.len(),
                    applied_count: 0,
                    quarantined_count: rejected_facts.len(),
                    retry_required: true,
                    automatic_fact_receipt_ids: Vec::new(),
                    applied_fact_ids: Vec::new(),
                };
                let ledger_record = finalizer
                    .append_failed_record_with_effects(
                        None,
                        evidence_hash,
                        Some(session_fact_ledger_summary(
                            &proposals,
                            &initial_accepted_facts,
                            &[],
                            &rejected_facts,
                        )?),
                        error.to_string(),
                        &retry_report,
                        None,
                        Some(fact_collection_summary(&rejected_facts)?),
                        Some(json!({
                            "status": receipt.outcome,
                            "receipt": receipt,
                            "validation_repairs": validation_repairs_summary(&validation_repairs)?,
                        })),
                        initial_accepted_facts.len(),
                        rejected_facts.len(),
                    )
                    .await?;
                return Err(AutomationRunError::RecordedFailure {
                    error,
                    ledger_record,
                });
            }
        };
        retry_report.append(repair_retry_report);
        (_, proposals) = finalizer
            .response_output_array(
                &response,
                evidence_hash.clone(),
                &retry_report,
                "facts",
                "session reflector repair output must include a facts array",
            )
            .await?;
    }
    let (report, record, committed_receipt) = match finalize_session_reflector_success(
        memory,
        run_control,
        config,
        authority,
        &finalizer,
        ProposedAgentOutput {
            response: &response,
            retry_report: &retry_report,
            evidence: &evidence,
            evidence_hash: evidence_hash.clone(),
            proposals: &proposals,
        },
        &validation_repairs,
    )
    .await
    {
        Ok(SessionReflectorFinalization::Completed {
            report,
            record,
            committed_receipt,
        }) => (report, record, committed_receipt),
        Ok(SessionReflectorFinalization::FailedRecorded {
            run_id,
            committed_receipt,
            record,
            detail,
        }) => {
            return Err(AutomationRunError::PartialEffect {
                run_id,
                committed_receipt,
                ledger_record: record,
                detail,
            });
        }
        Err(err) => {
            let ledger_record = finalizer
                .append_failed_record(
                    response.model.clone(),
                    evidence_hash,
                    Some(session_fact_finalization_failure_summary(&proposals)?),
                    err.to_string(),
                    &retry_report,
                )
                .await?;
            return Err(AutomationRunError::RecordedFailure {
                error: err,
                ledger_record,
            });
        }
    };
    let record = match finalizer
        .append_success_record(&request, &response, &retry_report, record)
        .await
    {
        Ok(record) => record,
        Err(error) => {
            if let Some(committed_receipt) = committed_receipt {
                return Err(AutomationRunError::PartialEffect {
                    run_id: run.run_id.clone(),
                    committed_receipt,
                    ledger_record: None,
                    detail: "Automatic facts committed, but the automation terminal could not be published; reconcile the committed receipt before another run.",
                });
            }
            return Err(error.into());
        }
    };

    Ok(SessionReflectorAutomationRun {
        run_id: run.run_id,
        report,
        ledger_record: record,
        backend_response: Some(response),
        committed_receipt,
    })
}

pub async fn run_session_reflector_with_backend_and_retrieval(
    cg: &TraceDecay,
    config: &AutomationConfig,
    run_control: &AutomationRunControl,
    configuration_revision_id: &ConfigurationRevisionId,
    backend: &dyn AgentTaskBackend,
    retrieval: &dyn AutomationSessionRetrieval,
    options: SessionReflectorAutomationOptions,
) -> AutomationRunResult<SessionReflectorAutomationRun> {
    run_session_reflector_with_backend_and_retrieval_publication(
        cg,
        config,
        run_control,
        configuration_revision_id,
        backend,
        retrieval,
        options,
        AutomationRunLedgerPublication::Immediate,
        None,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
async fn run_session_reflector_with_backend_and_retrieval_publication(
    cg: &TraceDecay,
    config: &AutomationConfig,
    run_control: &AutomationRunControl,
    configuration_revision_id: &ConfigurationRevisionId,
    backend: &dyn AgentTaskBackend,
    retrieval: &dyn AutomationSessionRetrieval,
    options: SessionReflectorAutomationOptions,
    ledger_publication: AutomationRunLedgerPublication,
    settlement_guard: Option<&AutomationRunSettlementGuard>,
) -> AutomationRunResult<SessionReflectorAutomationRun> {
    let authority = super::project_curation_authority(
        cg,
        "automation:session-reflector",
        configuration_revision_id,
    )?;
    let sessions_db = super::project_automation_sessions(cg).await?;
    let project_memory_db = cg.open_project_store_db().await?;
    let memory = MemoryApplication::new(
        cg.project_memory_owner()?,
        DatabaseFactStore::new(&project_memory_db),
    )
    .map_err(|error| TraceDecayError::Config {
        message: format!(
            "could not initialize project session reflector memory authority: {error}"
        ),
    })?;
    run_session_reflector_for_store_with_publication(
        cg.store_layout().dashboard_root.clone(),
        sessions_db,
        retrieval,
        &memory,
        config,
        run_control,
        &authority,
        backend,
        options,
        None,
        ledger_publication,
        settlement_guard,
    )
    .await
}

pub async fn run_session_reflector_with_backend(
    cg: &TraceDecay,
    config: &AutomationConfig,
    run_control: &AutomationRunControl,
    configuration_revision_id: &ConfigurationRevisionId,
    backend: &dyn AgentTaskBackend,
    options: SessionReflectorAutomationOptions,
) -> AutomationRunResult<SessionReflectorAutomationRun> {
    let retrieval = production_project_automation_retrieval(cg).await;
    run_session_reflector_with_backend_and_retrieval(
        cg,
        config,
        run_control,
        configuration_revision_id,
        backend,
        retrieval.as_ref(),
        options,
    )
    .await
}

/// Runs one already-admitted retained application effect without publishing
/// its ledger terminal ahead of outer settlement. The retained settlement
/// authority must bind and publish the returned exact record.
pub async fn run_session_reflector_with_backend_for_retained_settlement(
    cg: &TraceDecay,
    config: &AutomationConfig,
    run_control: &AutomationRunControl,
    configuration_revision_id: &ConfigurationRevisionId,
    backend: &dyn AgentTaskBackend,
    options: SessionReflectorAutomationOptions,
) -> RetainedAutomationRun<SessionReflectorAutomationRun> {
    let retrieval = production_project_automation_retrieval(cg).await;
    run_session_reflector_with_backend_and_retrieval_for_retained_settlement(
        cg,
        config,
        run_control,
        configuration_revision_id,
        backend,
        retrieval.as_ref(),
        options,
    )
    .await
}

/// Retained-settlement variant that preserves the caller's canonical session
/// retrieval authority instead of silently reopening the production route.
#[allow(clippy::too_many_arguments)]
pub async fn run_session_reflector_with_backend_and_retrieval_for_retained_settlement(
    cg: &TraceDecay,
    config: &AutomationConfig,
    run_control: &AutomationRunControl,
    configuration_revision_id: &ConfigurationRevisionId,
    backend: &dyn AgentTaskBackend,
    retrieval: &dyn AutomationSessionRetrieval,
    options: SessionReflectorAutomationOptions,
) -> RetainedAutomationRun<SessionReflectorAutomationRun> {
    let settlement_guard = AutomationRunSettlementGuard::new();
    let result = run_session_reflector_with_backend_and_retrieval_publication(
        cg,
        config,
        run_control,
        configuration_revision_id,
        backend,
        retrieval,
        options,
        AutomationRunLedgerPublication::DeferredUntilApplicationSettlement,
        Some(&settlement_guard),
    )
    .await;
    RetainedAutomationRun::new(result, settlement_guard)
}

#[cfg(test)]
mod tests;
