use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::sync::Arc;
use tracedecay_domain::configuration::ConfigurationRevisionId;
use tracedecay_domain::{
    ActorId, Confidence, FactEventId, FactId, FactOwnerV1, ManifestDigest, canonical_sha256,
};

use super::artifacts::sha256_json;
use super::backend::{
    AgentTaskBackend, AgentTaskKind, AgentTaskRequest, AgentTaskResponse, AgentTaskRetryReport,
    BackendRetryPolicy, run_agent_task_with_retry_report,
};
use super::config::AutomationConfig;
use super::lifecycle::{
    AgentTaskRunContext, AutomationCommittedReceipt, AutomationRunControl, AutomationRunError,
    AutomationRunLedgerPublication, AutomationRunResult, AutomationRunSettlementGuard,
    RetainedAutomationRun, SchedulerGate, failed_backend_fallback_report,
};
use super::run_ledger::{AutomationRunLedgerRecord, AutomationTrigger};
use crate::errors::{Result, TraceDecayError};
use crate::ports::project_runtime::ProfileRuntime;
use crate::ports::project_runtime::TraceDecay;
use crate::store::memory::DatabaseFactStore;
use tracedecay_global_db::RegisteredGlobalDbLeaseV1;
use tracedecay_policy::{
    CurationApplyAuthorityV1, CurationApplyDecisionV1, CurationApplyPolicyInputV1,
    CurationApplySubjectV1, CurationValidationDispositionV1, evaluate_curation_apply,
};
use tracedecay_store::ProjectMemoryFactCurationReceiptV1;
use tracedecay_usecases::memory::{
    MemoryApplication, MemoryApplicationError, MemoryMutationError, MemoryOperationContext,
};

pub const CURATION_DEFAULT_FACT_REVIEW_LIMIT: usize = 24;
pub const CURATION_DEFAULT_MIN_CONFIDENCE: f64 = 0.72;
const CURATION_MAX_OPERATIONS: usize = 256;

mod review;
mod wire;
#[cfg(test)]
use review::memory_curator_review_value;
use review::{attach_pagination_summary, memory_curator_resume_cursor, memory_curator_review};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MemoryCuratorAutomationOptions {
    #[serde(default)]
    pub trigger: AutomationTrigger,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
    #[serde(default = "default_fact_review_limit")]
    pub fact_review_limit: usize,
    #[serde(default = "default_min_confidence")]
    pub min_confidence: f64,
}

impl Default for MemoryCuratorAutomationOptions {
    fn default() -> Self {
        Self {
            trigger: AutomationTrigger::ManualCli,
            run_id: None,
            fact_review_limit: default_fact_review_limit(),
            min_confidence: default_min_confidence(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MemoryCuratorAutomationRun {
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

pub async fn run_memory_curator_with_backend(
    cg: &TraceDecay,
    config: &AutomationConfig,
    configuration_revision_id: &ConfigurationRevisionId,
    backend: &dyn AgentTaskBackend,
    options: MemoryCuratorAutomationOptions,
    run_control: &AutomationRunControl,
) -> AutomationRunResult<MemoryCuratorAutomationRun> {
    let sessions_db = super::runner::project_automation_sessions(cg).await?;
    run_memory_curator_for_store_with_publication(
        MemoryCuratorStore::Project { cg, sessions_db },
        config,
        configuration_revision_id,
        backend,
        options,
        run_control,
        AutomationRunLedgerPublication::Immediate,
        None,
    )
    .await
}

/// Runs one admitted retained Memory Curator effect without publishing its
/// ledger terminal before the daemon accepts the outer application terminal.
pub async fn run_memory_curator_with_backend_for_retained_settlement(
    cg: &TraceDecay,
    config: &AutomationConfig,
    configuration_revision_id: &ConfigurationRevisionId,
    backend: &dyn AgentTaskBackend,
    options: MemoryCuratorAutomationOptions,
    run_control: &AutomationRunControl,
) -> RetainedAutomationRun<MemoryCuratorAutomationRun> {
    let settlement_guard = AutomationRunSettlementGuard::new();
    let result = match super::runner::project_automation_sessions(cg).await {
        Ok(sessions_db) => {
            run_memory_curator_for_store_with_publication(
                MemoryCuratorStore::Project { cg, sessions_db },
                config,
                configuration_revision_id,
                backend,
                options,
                run_control,
                AutomationRunLedgerPublication::DeferredUntilApplicationSettlement,
                Some(&settlement_guard),
            )
            .await
        }
        Err(error) => Err(error.into()),
    };
    RetainedAutomationRun::new(result, settlement_guard)
}

/// Runs autonomous curation against profile-level user memory.
pub(crate) async fn run_user_memory_curator_with_backend(
    profile_root: &std::path::Path,
    session_registry: Arc<dyn ProfileRuntime>,
    config: &AutomationConfig,
    configuration_revision_id: &ConfigurationRevisionId,
    backend: &dyn AgentTaskBackend,
    options: MemoryCuratorAutomationOptions,
    run_control: &AutomationRunControl,
) -> AutomationRunResult<MemoryCuratorAutomationRun> {
    let sessions_db = session_registry.profile_sessions().await?;
    run_memory_curator_for_store_with_publication(
        MemoryCuratorStore::User {
            profile_root,
            runtime: session_registry.as_ref(),
            sessions_db,
        },
        config,
        configuration_revision_id,
        backend,
        options,
        run_control,
        AutomationRunLedgerPublication::Immediate,
        None,
    )
    .await
}

enum MemoryCuratorStore<'a> {
    Project {
        cg: &'a TraceDecay,
        sessions_db: RegisteredGlobalDbLeaseV1,
    },
    User {
        profile_root: &'a std::path::Path,
        runtime: &'a dyn ProfileRuntime,
        sessions_db: RegisteredGlobalDbLeaseV1,
    },
}

impl MemoryCuratorStore<'_> {
    fn dashboard_root(&self) -> std::path::PathBuf {
        match self {
            Self::Project { cg, .. } => cg.store_layout().dashboard_root.clone(),
            Self::User { profile_root, .. } => super::runner::user_automation_root(profile_root),
        }
    }

    fn sessions_db(&self) -> RegisteredGlobalDbLeaseV1 {
        match self {
            Self::Project { sessions_db, .. } | Self::User { sessions_db, .. } => {
                sessions_db.clone()
            }
        }
    }

    fn owner(&self) -> Result<FactOwnerV1> {
        match self {
            Self::Project { cg, .. } => cg.project_memory_owner(),
            Self::User { .. } => Ok(FactOwnerV1::Profile),
        }
    }

    fn curation_authority(
        &self,
        configuration_revision_id: &ConfigurationRevisionId,
    ) -> Result<CurationApplyAuthorityV1> {
        let actor_id = ActorId::new("automation:memory-curator").map_err(memory_contract_error)?;
        let (project_id, profile_id) = match self {
            Self::Project { cg, .. } => {
                let project_id = match cg.project_memory_owner()? {
                    FactOwnerV1::Project { project_id } => project_id,
                    FactOwnerV1::Profile => {
                        return Err(memory_validation_error(
                            "project memory curator is missing project authority",
                        ));
                    }
                };
                (Some(project_id), cg.profile_id().clone())
            }
            Self::User { runtime, .. } => (None, runtime.profile_id().clone()),
        };
        Ok(CurationApplyAuthorityV1 {
            actor_id,
            project_id,
            profile_id,
            configuration_revision_id: configuration_revision_id.clone(),
        })
    }

    async fn open_memory_database(&self) -> Result<crate::db::Database> {
        match self {
            Self::Project { cg, .. } => cg.open_project_store_db().await,
            Self::User { runtime, .. } => runtime.open_user_memory_db().await,
        }
    }
}

async fn run_memory_curator_for_store_with_publication(
    store: MemoryCuratorStore<'_>,
    config: &AutomationConfig,
    configuration_revision_id: &ConfigurationRevisionId,
    backend: &dyn AgentTaskBackend,
    options: MemoryCuratorAutomationOptions,
    run_control: &AutomationRunControl,
    ledger_publication: AutomationRunLedgerPublication,
    settlement_guard: Option<&AutomationRunSettlementGuard>,
) -> AutomationRunResult<MemoryCuratorAutomationRun> {
    let curation_authority = store.curation_authority(configuration_revision_id)?;
    let sessions_db = store.sessions_db();
    let mut run = AgentTaskRunContext::new(
        store.dashboard_root(),
        sessions_db,
        options.run_id.clone(),
        "memory_curator",
        options.trigger,
        config,
        AgentTaskKind::MemoryCurator,
    )
    .with_ledger_publication(ledger_publication)
    .with_settlement_guard(settlement_guard);
    let fact_review_limit = options.fact_review_limit.clamp(1, 1_000);
    if !options.min_confidence.is_finite() {
        return Err(TraceDecayError::Config {
            message: "memory curator minimum confidence must be finite".to_owned(),
        }
        .into());
    }
    let min_confidence = options.min_confidence.clamp(0.0, 1.0);
    let _run_lock = match run.gate().await? {
        SchedulerGate::Proceed(lock) => lock,
        SchedulerGate::Skip(reason) => {
            return skipped_run(&run, reason, None, None)
                .await
                .map_err(Into::into);
        }
    };
    let after_fact_id = memory_curator_resume_cursor(&run.dashboard_root).await?;
    if run_control.read_control().interrupted() {
        return Err(TraceDecayError::Database {
            operation: "run memory curator".to_owned(),
            message: "memory curator run was interrupted before review".to_owned(),
        }
        .into());
    }

    let owner = store.owner()?;
    let database = store.open_memory_database().await?;
    let memory = MemoryApplication::new(owner.clone(), DatabaseFactStore::new(&database)).map_err(
        |error| TraceDecayError::Config {
            message: format!("initialize memory curator authority: {error}"),
        },
    )?;
    let review_page = memory_curator_review(
        &memory,
        &owner,
        fact_review_limit,
        after_fact_id.clone(),
        run_control,
    )
    .await?;
    let llm_review = review_page.review;
    let allowed_facts = review_page.allowed_facts;
    let resume_after_fact_id = review_page.resume_after_fact_id;
    let evidence_hash = Some(sha256_json(&llm_review));
    if llm_review.get("status").and_then(Value::as_str) != Some("needs_llm_review") {
        let reason = match llm_review.get("status").and_then(Value::as_str) {
            Some("unavailable") => "similarity_authority_unavailable",
            Some("partial_coverage_no_candidates") => "partial_coverage_no_candidates",
            _ => "nothing_to_review",
        };
        return skipped_run(&run, reason, evidence_hash, Some(resume_after_fact_id))
            .await
            .map_err(Into::into);
    }

    let request = AgentTaskRequest::new(
        run.run_id.clone(),
        AgentTaskKind::MemoryCurator,
        build_memory_curator_prompt(),
        evidence_hash.clone(),
        memory_curator_backend_context(&llm_review, min_confidence),
    );
    let input_hash = Some(request.input_hash.clone());
    let finalizer = run.finalizer(input_hash.clone())?;

    let retry_policy = BackendRetryPolicy::from_timeout_secs(config.timeout_secs);
    let mut retry_report = AgentTaskRetryReport::default();
    let mut response =
        match run_agent_task_with_retry_report(backend, &request, &retry_policy, &mut retry_report)
            .await
        {
            Ok(response) => response,
            Err(err) => {
                let record = finalizer
                    .append_backend_fallback_record(evidence_hash, err.to_string(), &retry_report)
                    .await?;
                return Ok(MemoryCuratorAutomationRun {
                    run_id: record.run_id.clone(),
                    report: failed_backend_fallback_report(&record),
                    ledger_record: record,
                    backend_response: None,
                    committed_receipt: None,
                });
            }
        };
    let mut proposed_ops = finalizer
        .response_output_json(&response, evidence_hash.clone(), &retry_report)
        .await?;

    let mut validation_repairs = Vec::new();
    let (accepted_ops, rejected_ops) = loop {
        let (accepted_ops, rejected_ops) =
            validate_memory_curation_ops(&proposed_ops, &allowed_facts, min_confidence);
        if rejected_ops.is_empty() {
            break (accepted_ops, rejected_ops);
        }
        let attempt = validation_repairs.len() + 1;
        validation_repairs.push(json!({
            "attempt": attempt,
            "errors": rejected_ops,
        }));
        if attempt == 2 {
            let error = TraceDecayError::Config {
                message: "memory curator validation repair budget exhausted; output quarantined"
                    .to_string(),
            };
            let ledger_record = finalizer
                .append_failed_record(
                    response.model.clone(),
                    evidence_hash,
                    None,
                    error.to_string(),
                    &retry_report,
                )
                .await?;
            return Err(AutomationRunError::RecordedFailure {
                error,
                ledger_record,
            });
        }

        let repair_request = AgentTaskRequest::new(
            run.run_id.clone(),
            AgentTaskKind::MemoryCurator,
            build_memory_curator_repair_prompt(),
            evidence_hash.clone(),
            json!({
                "previous_output": proposed_ops.clone(),
                "validation_errors": validation_repairs.last(),
                "allowed_fact_snapshots": allowed_facts,
                "min_confidence": min_confidence,
                "apply": true,
            }),
        );
        let mut repair_retry_report = AgentTaskRetryReport::default();
        response = match run_agent_task_with_retry_report(
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
                let ledger_record = finalizer
                    .append_failed_record(
                        None,
                        evidence_hash,
                        None,
                        error.to_string(),
                        &retry_report,
                    )
                    .await?;
                return Err(AutomationRunError::RecordedFailure {
                    error,
                    ledger_record,
                });
            }
        };
        retry_report.append(repair_retry_report);
        proposed_ops = finalizer
            .response_output_json(&response, evidence_hash.clone(), &retry_report)
            .await?;
    };
    let curation_decision = memory_curation_decision(
        config,
        &curation_authority,
        evidence_hash.as_deref(),
        &accepted_ops,
    )?;
    let facts_reviewed = llm_review
        .get("facts_reviewed")
        .cloned()
        .unwrap_or_else(|| json!(0));
    let accepted_count = accepted_ops.len();
    let rejected_count = rejected_ops.len();
    let (settled_count, mutation_count, receipts, committed_receipt) = if curation_decision
        .allows_apply()
    {
        let result = apply_memory_curation_ops(
            &memory,
            &run.run_id,
            &accepted_ops,
            min_confidence,
            run_control,
        )
        .await;
        let (settled_count, mutation_count, receipts, committed_receipt) = match result {
            Ok(result) => result,
            Err(MemoryCurationApplyFailure::Application(error)) => {
                let ledger_record = finalizer
                    .append_failed_record(
                        response.model.clone(),
                        evidence_hash,
                        None,
                        error.to_string(),
                        &retry_report,
                    )
                    .await?;
                return Err(AutomationRunError::RecordedFailure {
                    error,
                    ledger_record,
                });
            }
            Err(MemoryCurationApplyFailure::Settled {
                error,
                operation_count,
                receipt,
            }) => {
                let settled_count = receipt.operation_effects().len();
                let mutation_count = memory_curation_mutation_count(&receipt);
                let receipts = vec![memory_curation_receipt_json(
                    "failed_after_partial_effects",
                    operation_count,
                    &receipt,
                )];
                let curation_policy = memory_curation_report(
                    &accepted_ops,
                    &curation_decision,
                    settled_count,
                    mutation_count,
                    true,
                );
                let mut validated_report = json!({
                    "llm_review": llm_review,
                    "llm_apply": {
                        "status": "failed_after_partial_effects",
                        "facts_reviewed": facts_reviewed,
                        "ops": accepted_ops,
                        "rejected_ops": rejected_ops,
                        "settled": settled_count,
                        "applied": mutation_count,
                        "receipts": receipts,
                        "validation_repairs": validation_repairs,
                    }
                });
                annotate_memory_curation_report(&mut validated_report, curation_policy);
                let applied_ops = validated_report.pointer("/llm_apply/receipts").cloned();
                let validation_report = Some(memory_curation_validation_summary(
                    "failed_after_partial_effects",
                    facts_reviewed.as_u64().unwrap_or(0),
                    accepted_count,
                    rejected_count,
                    validation_repairs.len(),
                    settled_count,
                    mutation_count,
                ));
                let committed_receipt = AutomationCommittedReceipt::MemoryCuration(receipt);
                let record = match finalizer
                    .append_failed_record_with_effects(
                        response.model.clone(),
                        evidence_hash,
                        None,
                        error.to_string(),
                        &retry_report,
                        applied_ops,
                        None,
                        validation_report,
                        accepted_count,
                        rejected_count,
                    )
                    .await
                {
                    Ok(record) => Some(record),
                    Err(_) => None,
                };
                return Err(AutomationRunError::PartialEffect {
                    run_id: run.run_id.clone(),
                    committed_receipt,
                    ledger_record: record,
                    detail: "Memory curation committed, but its canonical result projection failed; reconcile the committed receipt before another run.",
                });
            }
        };
        (settled_count, mutation_count, receipts, committed_receipt)
    } else {
        (0, 0, Vec::new(), None)
    };
    let curation_policy = memory_curation_report(
        &accepted_ops,
        &curation_decision,
        settled_count,
        mutation_count,
        false,
    );
    let mut validated_report = json!({
        "llm_review": llm_review,
        "llm_apply": {
            "facts_reviewed": facts_reviewed,
            "ops": accepted_ops,
            "rejected_ops": rejected_ops,
            "settled": settled_count,
            "applied": mutation_count,
            "receipts": receipts,
            "validation_repairs": validation_repairs,
        }
    });
    annotate_memory_curation_report(&mut validated_report, curation_policy);

    let applied_ops = validated_report
        .pointer("/llm_apply/receipts")
        .filter(|value| {
            value
                .as_array()
                .is_some_and(|receipts| !receipts.is_empty())
        })
        .cloned();
    let mut terminal_summary = memory_curation_validation_summary(
        memory_curation_settlement_status(
            &curation_decision,
            accepted_count,
            settled_count,
            mutation_count,
        ),
        facts_reviewed.as_u64().unwrap_or(0),
        accepted_count,
        rejected_count,
        validation_repairs.len(),
        settled_count,
        mutation_count,
    );
    attach_pagination_summary(
        &mut terminal_summary,
        after_fact_id.as_ref(),
        resume_after_fact_id.as_ref(),
    );
    if let Some(object) = terminal_summary.as_object_mut() {
        object.insert("validation_repairs".to_owned(), json!(validation_repairs));
        if let Some(policy) = validated_report.get("curation_policy").cloned() {
            object.insert("curation_policy".to_owned(), policy);
        }
    }
    let validation_report = Some(terminal_summary);
    let mut record = match finalizer.success_record(
        &response,
        evidence_hash,
        Some(proposed_ops),
        accepted_count,
        rejected_count,
    ) {
        Ok(record) => record,
        Err(error) => {
            if let Some(committed_receipt) = committed_receipt {
                return Err(AutomationRunError::PartialEffect {
                    run_id: run.run_id.clone(),
                    committed_receipt: AutomationCommittedReceipt::MemoryCuration(
                        committed_receipt,
                    ),
                    ledger_record: None,
                    detail: "Memory curation committed, but its exact completion time could not be recorded; reconcile the committed receipt before another run.",
                });
            }
            return Err(error.into());
        }
    };
    record.applied_ops = applied_ops;
    record.rejected_ops = Some(json!(rejected_ops));
    record.validation_report = validation_report;
    let record = match finalizer
        .append_success_record(&request, &response, &retry_report, record)
        .await
    {
        Ok(record) => record,
        Err(error) => {
            if let Some(committed_receipt) = committed_receipt {
                return Err(AutomationRunError::PartialEffect {
                    run_id: run.run_id.clone(),
                    committed_receipt: AutomationCommittedReceipt::MemoryCuration(
                        committed_receipt,
                    ),
                    ledger_record: None,
                    detail: "Memory curation committed, but the automation terminal could not be published; reconcile the committed receipt before another run.",
                });
            }
            return Err(error.into());
        }
    };
    Ok(MemoryCuratorAutomationRun {
        run_id: run.run_id,
        report: validated_report,
        ledger_record: record,
        backend_response: Some(response),
        committed_receipt: committed_receipt.map(AutomationCommittedReceipt::MemoryCuration),
    })
}

fn memory_curation_settlement_status(
    decision: &CurationApplyDecisionV1,
    accepted_count: usize,
    settled_count: usize,
    mutation_count: usize,
) -> &'static str {
    if accepted_count == 0 {
        "no_candidate"
    } else if decision.allows_apply() && settled_count == accepted_count {
        if mutation_count == 0 {
            "settled_noop"
        } else {
            "applied"
        }
    } else {
        "quarantined"
    }
}

async fn skipped_run(
    run: &AgentTaskRunContext<'_>,
    reason: &str,
    evidence_hash: Option<String>,
    pagination: Option<Option<FactId>>,
) -> Result<MemoryCuratorAutomationRun> {
    let (report, record) = if let Some(resume_after_fact_id) = pagination {
        let mut summary = json!({"status": "skipped", "reason": reason});
        attach_pagination_summary(&mut summary, None, resume_after_fact_id.as_ref());
        run.skipped_parts_with_validation_report(evidence_hash, reason, None, summary)
            .await?
    } else {
        run.skipped_parts(evidence_hash, reason, None).await?
    };
    Ok(MemoryCuratorAutomationRun {
        run_id: run.run_id.clone(),
        report,
        ledger_record: record,
        backend_response: None,
        committed_receipt: None,
    })
}

fn build_memory_curator_prompt() -> String {
    format!(
        "Review only the canonical current facts in context.llm_review. Return {{\"ops\":[]}} with at most {CURATION_MAX_OPERATIONS} operations. Never invent or rewrite a fact id.\n{}",
        wire::MODEL_CONTRACT
    )
}

fn build_memory_curator_repair_prompt() -> String {
    format!(
        "Repair the previous memory curation JSON. Return only {{\"ops\": [...]}} with at most {CURATION_MAX_OPERATIONS} operations. Preserve valid intent, fix every validation error, use only exact fact snapshots from context.allowed_fact_snapshots, and do not add unrelated operations.\n{}",
        wire::MODEL_CONTRACT
    )
}

fn memory_curator_backend_context(llm_review: &Value, min_confidence: f64) -> Value {
    json!({
        "llm_review": llm_review,
        "apply": true,
        "min_confidence": min_confidence,
    })
}

fn validate_memory_curation_ops(
    output: &Value,
    allowed_facts: &BTreeMap<FactId, FactEventId>,
    min_confidence: f64,
) -> (Vec<Value>, Vec<Value>) {
    let Some(ops) = output.get("ops").and_then(Value::as_array) else {
        return (
            Vec::new(),
            vec![json!({
                "rejected_reason": "memory curator output did not contain an ops array"
            })],
        );
    };
    if ops.len() > CURATION_MAX_OPERATIONS {
        return (
            Vec::new(),
            vec![json!({
                "rejected_reason": format!(
                    "memory curator output exceeded the {CURATION_MAX_OPERATIONS}-operation limit"
                )
            })],
        );
    }
    let mut accepted = Vec::new();
    let mut rejected = Vec::new();
    for raw in ops {
        let Some(op) = raw.get("op").and_then(Value::as_str) else {
            rejected.push(rejected_memory_op(raw, "missing operation kind"));
            continue;
        };
        let confidence = raw.get("confidence").and_then(Value::as_f64);
        if !confidence
            .is_some_and(|value| value.is_finite() && (min_confidence..=1.0).contains(&value))
        {
            rejected.push(rejected_memory_op(
                raw,
                "confidence was missing or below the pinned threshold",
            ));
            continue;
        }
        if uses_timestamp_as_truth(raw) {
            rejected.push(rejected_memory_op(
                raw,
                "updated_at is not authoritative truth or freshness evidence",
            ));
            continue;
        }
        let valid = matches!(
            op,
            "add" | "update" | "merge" | "remove" | "normalize_tags" | "link_facts"
        ) && wire::valid_curation_op(raw, allowed_facts);
        if valid {
            accepted.push(raw.clone());
        } else {
            rejected.push(rejected_memory_op(
                raw,
                "operation was unsupported or referenced evidence outside the verified page",
            ));
        }
    }
    (accepted, rejected)
}

fn uses_timestamp_as_truth(raw: &Value) -> bool {
    raw.get("reason")
        .and_then(Value::as_str)
        .is_some_and(|reason| reason.to_ascii_lowercase().contains("updated_at"))
        || raw.get("freshness_field").and_then(Value::as_str) == Some("updated_at")
}

fn rejected_memory_op(raw: &Value, reason: &str) -> Value {
    let mut rejected = raw.as_object().cloned().unwrap_or_default();
    rejected.insert("rejected_reason".to_owned(), json!(reason));
    Value::Object(rejected)
}

#[derive(Debug)]
enum MemoryCurationApplyFailure {
    Application(TraceDecayError),
    Settled {
        error: TraceDecayError,
        operation_count: usize,
        receipt: ProjectMemoryFactCurationReceiptV1,
    },
}

impl From<TraceDecayError> for MemoryCurationApplyFailure {
    fn from(error: TraceDecayError) -> Self {
        Self::Application(error)
    }
}

fn settle_memory_curation_result(
    result: std::result::Result<
        ProjectMemoryFactCurationReceiptV1,
        MemoryMutationError<ProjectMemoryFactCurationReceiptV1>,
    >,
    operation_count: usize,
) -> std::result::Result<
    (
        usize,
        usize,
        Vec<Value>,
        Option<ProjectMemoryFactCurationReceiptV1>,
    ),
    MemoryCurationApplyFailure,
> {
    match result {
        Ok(receipt) => {
            let projected = memory_curation_receipt_json("applied", operation_count, &receipt);
            let settled_count = receipt.operation_effects().len();
            let mutation_count = memory_curation_mutation_count(&receipt);
            Ok((
                settled_count,
                mutation_count,
                vec![projected],
                Some(receipt),
            ))
        }
        Err(MemoryMutationError::Application(error)) => Err(
            MemoryCurationApplyFailure::Application(memory_application_error(error)),
        ),
        Err(MemoryMutationError::InvalidAuthorityResult {
            error,
            authority_result,
        }) => Err(MemoryCurationApplyFailure::Settled {
            error: memory_application_error(error),
            operation_count,
            receipt: authority_result,
        }),
    }
}

fn memory_curation_mutation_count(receipt: &ProjectMemoryFactCurationReceiptV1) -> usize {
    receipt
        .operation_effects()
        .iter()
        .filter(|effect| effect.primary_commit().is_some())
        .count()
}

fn memory_curation_receipt_json(
    status: &'static str,
    operation_count: usize,
    receipt: &ProjectMemoryFactCurationReceiptV1,
) -> Value {
    json!({
        "op": "curation_batch",
        "status": status,
        "operation_count": operation_count,
        "receipt": receipt,
    })
}

fn memory_curation_validation_summary(
    status: &'static str,
    facts_reviewed: u64,
    accepted_count: usize,
    rejected_count: usize,
    validation_repair_count: usize,
    settled_count: usize,
    mutation_count: usize,
) -> Value {
    json!({
        "status": status,
        "facts_reviewed": facts_reviewed,
        "accepted_count": accepted_count,
        "rejected_count": rejected_count,
        "validation_repair_count": validation_repair_count,
        "settled_count": settled_count,
        "applied_count": mutation_count,
        "mutates_store": mutation_count > 0,
    })
}

async fn apply_memory_curation_ops<A: tracedecay_store::ProjectMemoryFactStore>(
    memory: &MemoryApplication<A>,
    run_id: &str,
    operations: &[Value],
    min_confidence: f64,
    run_control: &AutomationRunControl,
) -> std::result::Result<
    (
        usize,
        usize,
        Vec<Value>,
        Option<ProjectMemoryFactCurationReceiptV1>,
    ),
    MemoryCurationApplyFailure,
> {
    if operations.is_empty() {
        return Ok((0, 0, Vec::new(), None));
    }
    let actor = ActorId::new("automation:memory-curator").map_err(memory_contract_error)?;
    let context = MemoryOperationContext::from_logical_effect(
        memory.owner(),
        "automation-memory-curator",
        &(run_id, operations),
        Some(actor),
    )
    .map_err(memory_application_error)?;
    let curation = operations
        .iter()
        .cloned()
        .map(|operation| {
            serde_json::from_value::<wire::CanonicalCurationWire>(operation)
                .map_err(|error| error.to_string())
                .and_then(wire::CanonicalCurationWire::into_operation)
                .map_err(|error| {
                    memory_validation_error(format!(
                        "validated curation operation could not be reconstructed: {error}"
                    ))
                })
        })
        .collect::<Result<Vec<_>>>()?;
    let count = curation.len();
    let minimum = Confidence::new(min_confidence).map_err(memory_contract_error)?;
    let write_control = run_control.write_control();
    let automation_run_id =
        tracedecay_domain::RunId::new(run_id.to_owned()).map_err(memory_contract_error)?;
    settle_memory_curation_result(
        memory
            .apply_project_memory_curation(
                curation,
                minimum,
                context,
                Some(automation_run_id),
                &write_control,
            )
            .await,
        count,
    )
}

fn memory_application_error(error: MemoryApplicationError) -> TraceDecayError {
    use std::error::Error as _;

    let mut message = error.to_string();
    let mut source = error.source();
    while let Some(error) = source {
        message.push_str(": ");
        message.push_str(&error.to_string());
        source = error.source();
    }
    TraceDecayError::Database {
        operation: "apply validated memory curator operations".to_owned(),
        message,
    }
}

fn memory_contract_error(error: impl std::fmt::Display) -> TraceDecayError {
    memory_validation_error(error.to_string())
}

fn memory_validation_error(message: impl Into<String>) -> TraceDecayError {
    TraceDecayError::Config {
        message: message.into(),
    }
}

fn memory_curation_report(
    ops: &[Value],
    decision: &CurationApplyDecisionV1,
    settled_count: usize,
    mutation_count: usize,
    settled_failure: bool,
) -> Value {
    let accepted_count = ops.len();
    json!({
        "decision": decision,
        "effect": {
            "accepted_count": accepted_count,
            "settled_count": settled_count,
            "applied_count": mutation_count,
            "fully_applied": !settled_failure && decision.allows_apply() && settled_count == accepted_count,
            "mutates_store": mutation_count > 0,
        },
    })
}

fn memory_curation_decision(
    config: &AutomationConfig,
    authority: &CurationApplyAuthorityV1,
    evidence_hash: Option<&str>,
    accepted_ops: &[Value],
) -> Result<CurationApplyDecisionV1> {
    let evidence_digest = evidence_hash
        .map(|hash| ManifestDigest::new(hash.to_owned()))
        .transpose()
        .map_err(memory_contract_error)?;
    let output_digest = canonical_sha256(&accepted_ops).map_err(memory_contract_error)?;
    let configuration_digest = canonical_sha256(config).map_err(memory_contract_error)?;
    evaluate_curation_apply(&CurationApplyPolicyInputV1 {
        authority: authority.clone(),
        subject: CurationApplySubjectV1::MemoryCurator,
        evidence_digest,
        output_digest,
        validation: if accepted_ops.is_empty() {
            CurationValidationDispositionV1::NoCandidate
        } else {
            CurationValidationDispositionV1::Accepted
        },
        configuration_digest,
    })
    .map_err(memory_contract_error)
}

fn annotate_memory_curation_report(report: &mut Value, curation_policy: Value) {
    if let Some(object) = report.as_object_mut() {
        object.insert("curation_policy".to_string(), curation_policy.clone());
    }
    if let Some(llm_apply) = report.get_mut("llm_apply").and_then(Value::as_object_mut) {
        llm_apply.insert("curation_policy".to_string(), curation_policy);
    }
}

fn default_fact_review_limit() -> usize {
    CURATION_DEFAULT_FACT_REVIEW_LIMIT
}

fn default_min_confidence() -> f64 {
    CURATION_DEFAULT_MIN_CONFIDENCE
}

#[cfg(test)]
mod tests;
