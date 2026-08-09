use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::BTreeSet;
use std::sync::Arc;
use tracedecay_application::memory::{
    VerifiedMemorySimilarityPairQueryV1, VerifiedMemorySimilarityReadV1,
};
use tracedecay_domain::{ActorId, FactId, FactOwnerV1};

use super::apply_policy::{MemoryApplyDecision, MemoryApplyPolicy};
use super::artifacts::sha256_json;
use super::backend::{
    AgentTaskBackend, AgentTaskKind, AgentTaskRequest, AgentTaskResponse, AgentTaskRetryReport,
    BackendRetryPolicy, run_agent_task_with_retry_report,
};
use super::config::AutomationConfig;
use super::lifecycle::{AgentTaskRunContext, SchedulerGate, failed_backend_fallback_report};
use super::run_ledger::{AutomationRunLedgerRecord, AutomationTrigger};
use crate::errors::{Result, TraceDecayError};
use crate::ports::project_runtime::ProfileRuntime;
use crate::ports::project_runtime::TraceDecay;
use crate::store::memory::DatabaseFactStore;
use tracedecay_global_db::RegisteredGlobalDb;
use tracedecay_runtime_core::memory::types::MemoryGroomingOperation;
use tracedecay_usecases::memory::{MemoryApplication, MemoryOperationContext};

const CURATION_DEFAULT_MAX_CLUSTERS: usize = 12;
const CURATION_DEFAULT_MIN_CONFIDENCE: f64 = 0.72;
// The removed classifier admitted `merge_candidate` at 0.90 and
// `likely_duplicate` at 0.95. The canonical pair authority therefore uses
// the lower durable classification floor as its bounded review prefilter.
const CURATION_SIMILARITY_THRESHOLD_MILLIONTHS: u32 = 900_000;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MemoryCuratorAutomationOptions {
    #[serde(default)]
    pub trigger: AutomationTrigger,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
    #[serde(default = "default_max_clusters")]
    pub max_clusters: usize,
    #[serde(default = "default_min_confidence")]
    pub min_confidence: f64,
}

impl Default for MemoryCuratorAutomationOptions {
    fn default() -> Self {
        Self {
            trigger: AutomationTrigger::ManualCli,
            run_id: None,
            max_clusters: default_max_clusters(),
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
}

pub async fn run_memory_curator_with_backend(
    cg: &TraceDecay,
    config: &AutomationConfig,
    backend: &dyn AgentTaskBackend,
    options: MemoryCuratorAutomationOptions,
) -> Result<MemoryCuratorAutomationRun> {
    let sessions_db = super::runner::project_automation_sessions(cg).await?;
    run_memory_curator_for_store(
        MemoryCuratorStore::Project { cg, sessions_db },
        config,
        backend,
        options,
    )
    .await
}

/// Runs autonomous curation against profile-level user memory.
pub(crate) async fn run_user_memory_curator_with_backend(
    profile_root: &std::path::Path,
    session_registry: Arc<dyn ProfileRuntime>,
    config: &AutomationConfig,
    backend: &dyn AgentTaskBackend,
    options: MemoryCuratorAutomationOptions,
) -> Result<MemoryCuratorAutomationRun> {
    let sessions_db = session_registry.profile_sessions().await?;
    run_memory_curator_for_store(
        MemoryCuratorStore::User {
            profile_root,
            runtime: session_registry.as_ref(),
            sessions_db,
        },
        config,
        backend,
        options,
    )
    .await
}

enum MemoryCuratorStore<'a> {
    Project {
        cg: &'a TraceDecay,
        sessions_db: Arc<RegisteredGlobalDb>,
    },
    User {
        profile_root: &'a std::path::Path,
        runtime: &'a dyn ProfileRuntime,
        sessions_db: Arc<RegisteredGlobalDb>,
    },
}

impl MemoryCuratorStore<'_> {
    fn dashboard_root(&self) -> std::path::PathBuf {
        match self {
            Self::Project { cg, .. } => cg.store_layout().dashboard_root.clone(),
            Self::User { profile_root, .. } => super::runner::user_automation_root(profile_root),
        }
    }

    fn sessions_db(&self) -> Arc<RegisteredGlobalDb> {
        match self {
            Self::Project { sessions_db, .. } | Self::User { sessions_db, .. } => {
                Arc::clone(sessions_db)
            }
        }
    }

    fn owner(&self) -> Result<FactOwnerV1> {
        match self {
            Self::Project { cg, .. } => cg.project_memory_owner(),
            Self::User { .. } => Ok(FactOwnerV1::Profile),
        }
    }

    async fn read_similarity_pairs(
        &self,
        query: VerifiedMemorySimilarityPairQueryV1,
    ) -> Result<VerifiedMemorySimilarityReadV1> {
        match self {
            Self::Project { cg, .. } => cg.read_verified_memory_similarity_pairs(query).await,
            Self::User { runtime, .. } => {
                runtime.read_verified_memory_similarity_pairs(query).await
            }
        }
    }

    async fn open_memory_database(&self) -> Result<crate::db::Database> {
        match self {
            Self::Project { cg, .. } => cg.open_project_store_db().await,
            Self::User { runtime, .. } => runtime.open_user_memory_db().await,
        }
    }

    async fn refresh_digest(&self, memory: &MemoryApplication<DatabaseFactStore<'_>>) {
        if let Self::Project { cg, .. } = self {
            crate::automation::memory_digest::refresh_memory_digest_after_memory_change(
                memory,
                &cg.store_layout().project_root,
                true,
            )
            .await;
        }
    }
}

async fn run_memory_curator_for_store(
    store: MemoryCuratorStore<'_>,
    config: &AutomationConfig,
    backend: &dyn AgentTaskBackend,
    options: MemoryCuratorAutomationOptions,
) -> Result<MemoryCuratorAutomationRun> {
    let sessions_db = store.sessions_db();
    let mut run = AgentTaskRunContext::new(
        store.dashboard_root(),
        sessions_db,
        options.run_id.clone(),
        "memory_curator",
        options.trigger,
        config,
        AgentTaskKind::MemoryCurator,
    );
    let max_clusters = options.max_clusters.clamp(1, 50);
    if !options.min_confidence.is_finite() {
        return Err(TraceDecayError::Config {
            message: "memory curator minimum confidence must be finite".to_owned(),
        });
    }
    let min_confidence = options.min_confidence.clamp(0.0, 1.0);

    let _run_lock = match run.gate().await? {
        SchedulerGate::Proceed(lock) => lock,
        SchedulerGate::Skip(reason) => {
            return skipped_run(&run, reason, None).await;
        }
    };

    let owner = store.owner()?;
    let query = VerifiedMemorySimilarityPairQueryV1::new(
        owner.clone(),
        None,
        max_clusters,
        CURATION_SIMILARITY_THRESHOLD_MILLIONTHS,
    )
    .map_err(|error| TraceDecayError::Config {
        message: format!("invalid memory curator similarity query: {error}"),
    })?;
    let (llm_review, allowed_fact_ids) =
        memory_curator_review(store.read_similarity_pairs(query).await?)?;
    let evidence_hash = Some(sha256_json(&llm_review));
    if llm_review.get("status").and_then(Value::as_str) != Some("needs_llm_review") {
        let reason = match llm_review.get("status").and_then(Value::as_str) {
            Some("unavailable") => "similarity_authority_unavailable",
            Some("partial_coverage_no_candidates") => "partial_coverage_no_candidates",
            _ => "nothing_to_review",
        };
        return skipped_run(&run, reason, evidence_hash).await;
    }

    let request = AgentTaskRequest::new(
        run.run_id.clone(),
        AgentTaskKind::MemoryCurator,
        build_memory_curator_prompt(),
        evidence_hash.clone(),
        memory_curator_backend_context(&llm_review, min_confidence),
    );
    let input_hash = Some(request.input_hash.clone());
    let finalizer = run.finalizer(input_hash.clone());

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
                });
            }
        };
    let mut proposed_ops = finalizer
        .response_output_json(&response, evidence_hash.clone(), &retry_report)
        .await?;

    let mut validation_repairs = Vec::new();
    let (accepted_ops, rejected_ops) = loop {
        let (accepted_ops, rejected_ops) =
            validate_memory_curation_ops(&proposed_ops, &allowed_fact_ids, min_confidence);
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
            finalizer
                .append_failed_record(
                    response.model.clone(),
                    evidence_hash,
                    Some(proposed_ops),
                    error.to_string(),
                    &retry_report,
                )
                .await?;
            return Err(error);
        }

        let repair_request = AgentTaskRequest::new(
            run.run_id.clone(),
            AgentTaskKind::MemoryCurator,
            "Repair the previous memory curation JSON. Return only {\"ops\": [...]}. Preserve valid intent, fix every validation error, use only fact ids from context.allowed_fact_ids, and do not add unrelated operations."
                .to_string(),
            evidence_hash.clone(),
            json!({
                "previous_output": proposed_ops.clone(),
                "validation_errors": validation_repairs.last(),
                "allowed_fact_ids": allowed_fact_ids,
                "min_confidence": min_confidence,
                "apply": true,
                "memory_apply_policy": "validate_then_apply",
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
                retry_report.extend(&repair_retry_report);
                finalizer
                    .append_failed_record(
                        None,
                        evidence_hash,
                        Some(proposed_ops),
                        error.to_string(),
                        &retry_report,
                    )
                    .await?;
                return Err(error);
            }
        };
        retry_report.extend(&repair_retry_report);
        proposed_ops = finalizer
            .response_output_json(&response, evidence_hash.clone(), &retry_report)
            .await?;
    };
    let accepted_ops_value = Value::Array(accepted_ops.clone());
    let dry_run_apply_policy = memory_curation_apply_policy(Some(&accepted_ops_value), None);
    let should_apply = dry_run_apply_policy
        .get("mutates_store")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let (applied_count, receipts) = if should_apply && !accepted_ops.is_empty() {
        let database = store.open_memory_database().await?;
        let memory = MemoryApplication::new(owner.clone(), DatabaseFactStore::new(&database))
            .map_err(|error| TraceDecayError::Config {
                message: format!("initialize memory curator authority: {error}"),
            })?;
        let result = apply_memory_curation_ops(&memory, &run.run_id, &accepted_ops).await;
        let (applied_count, receipts) = match result {
            Ok(result) => result,
            Err(err) => {
                finalizer
                    .append_failed_record(
                        response.model.clone(),
                        evidence_hash,
                        Some(proposed_ops),
                        err.to_string(),
                        &retry_report,
                    )
                    .await?;
                return Err(err);
            }
        };
        if applied_count > 0 && config.export_memory_digest {
            store.refresh_digest(&memory).await;
        }
        (applied_count, receipts)
    } else {
        (0, Vec::new())
    };
    let apply_policy = if should_apply {
        memory_curation_apply_policy(Some(&accepted_ops_value), Some(applied_count))
    } else {
        dry_run_apply_policy
    };
    let clusters_reviewed = llm_review
        .get("clusters_reviewed")
        .cloned()
        .unwrap_or_else(|| json!(0));
    let mut validated_report = json!({
        "llm_review": llm_review,
        "llm_apply": {
            "clusters_reviewed": clusters_reviewed,
            "ops": accepted_ops,
            "rejected_ops": rejected_ops,
            "applied": applied_count,
            "receipts": receipts,
            "validation_repairs": validation_repairs,
        }
    });
    annotate_memory_curation_report(&mut validated_report, apply_policy);

    let validation_report = validated_report.get("llm_apply").cloned();
    let applied_ops = validated_report.pointer("/llm_apply/ops").cloned();
    let rejected_ops = validated_report.pointer("/llm_apply/rejected_ops").cloned();
    let accepted_count = validated_report
        .pointer("/llm_apply/ops")
        .and_then(Value::as_array)
        .map_or(0, Vec::len);
    let rejected_count = validated_report
        .pointer("/llm_apply/rejected_ops")
        .and_then(Value::as_array)
        .map_or(0, Vec::len);
    let mut record = finalizer.success_record(
        &response,
        evidence_hash,
        Some(proposed_ops),
        accepted_count,
        rejected_count,
    );
    record.applied_ops = applied_ops;
    record.rejected_ops = rejected_ops;
    record.validation_report = validation_report;
    let record = finalizer
        .append_success_record(&request, &response, &retry_report, record)
        .await?;

    Ok(MemoryCuratorAutomationRun {
        run_id: run.run_id,
        report: validated_report,
        ledger_record: record,
        backend_response: Some(response),
    })
}

async fn skipped_run(
    run: &AgentTaskRunContext<'_>,
    reason: &str,
    evidence_hash: Option<String>,
) -> Result<MemoryCuratorAutomationRun> {
    let (report, record) = run.skipped_parts(evidence_hash, reason, None).await?;
    Ok(MemoryCuratorAutomationRun {
        run_id: run.run_id.clone(),
        report,
        ledger_record: record,
        backend_response: None,
    })
}

fn build_memory_curator_prompt() -> String {
    "Review only the generation-bound memory pairs in context.llm_review. Return {\"ops\":[]} with zero or more bounded operations described by the system message. Never invent or rewrite a fact id.".to_string()
}

fn memory_curator_backend_context(llm_review: &Value, min_confidence: f64) -> Value {
    json!({
        "llm_review": llm_review,
        "apply": true,
        "memory_apply_policy": "validate_then_apply",
        "min_confidence": min_confidence,
    })
}

fn memory_curator_review(
    read: VerifiedMemorySimilarityReadV1,
) -> Result<(Value, BTreeSet<String>)> {
    match read {
        VerifiedMemorySimilarityReadV1::Unavailable { state, .. } => Ok((
            json!({
                "status": "unavailable",
                "reason": state.reason(),
                "pairs": [],
                "allowed_fact_ids": [],
            }),
            BTreeSet::new(),
        )),
        VerifiedMemorySimilarityReadV1::Available {
            projection_generation_id,
            watermark,
            model_config_digest,
            pairs,
            coverage,
            next_cursor,
            ..
        } => {
            let mut allowed_fact_ids = BTreeSet::new();
            let pairs = pairs
                .iter()
                .map(|pair| {
                    allowed_fact_ids.insert(pair.left().fact_id().as_str().to_owned());
                    allowed_fact_ids.insert(pair.right().fact_id().as_str().to_owned());
                    json!({
                        "left": similarity_fact_json(pair.left()),
                        "right": similarity_fact_json(pair.right()),
                        "similarity_millionths": pair.similarity_millionths(),
                    })
                })
                .collect::<Vec<_>>();
            let status = match (pairs.is_empty(), coverage.state()) {
                (true, tracedecay_application::memory::VerifiedMemorySimilarityCoverageStateV1::Partial) => {
                    "partial_coverage_no_candidates"
                }
                (true, tracedecay_application::memory::VerifiedMemorySimilarityCoverageStateV1::Complete) => {
                    "up_to_date"
                }
                (false, _) => "needs_llm_review",
            };
            let allowed_fact_id_values = allowed_fact_ids.iter().cloned().collect::<Vec<_>>();
            let review = json!({
                "status": status,
                "clusters_reviewed": pairs.len(),
                "projection_generation_id": projection_generation_id,
                "watermark": watermark,
                "model_config_digest": model_config_digest,
                "coverage": {
                    "active_facts_scanned": coverage.active_facts_scanned(),
                    "active_facts_eligible": coverage.active_facts_eligible(),
                    "active_facts_total": coverage.active_facts_total(),
                    "state": match coverage.state() {
                        tracedecay_application::memory::VerifiedMemorySimilarityCoverageStateV1::Complete => "complete",
                        tracedecay_application::memory::VerifiedMemorySimilarityCoverageStateV1::Partial => "partial",
                    },
                },
                "page_truncated": next_cursor.is_some(),
                "allowed_fact_ids": allowed_fact_id_values,
                "pairs": pairs,
                "messages": [
                    {
                        "role": "system",
                        "content": "Return strict JSON {\"ops\":[]}. Supported operations are delete, merge, normalize_tags, merge_entities, add_alias, and link_facts. Every fact id and evidence fact id must be copied exactly from allowed_fact_ids. Every operation requires confidence in [min_confidence,1]. A batch may contain at most one destructive delete/merge and must not mix a destructive operation with grooming operations. Never use updated_at as truth or freshness evidence."
                    }
                ],
            });
            Ok((review, allowed_fact_ids))
        }
    }
}

fn similarity_fact_json(
    fact: &tracedecay_application::memory::VerifiedMemorySimilarityFactV1,
) -> Value {
    json!({
        "fact_id": fact.fact_id().as_str(),
        "content": fact.content(),
        "category": fact.category(),
        "tags": fact.tags(),
        "trust": fact.trust(),
        "updated_at": fact.updated_at(),
        "metadata": fact.metadata(),
    })
}

fn validate_memory_curation_ops(
    output: &Value,
    allowed_fact_ids: &BTreeSet<String>,
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
        let valid = match op {
            "delete" => raw
                .get("fact_id")
                .and_then(Value::as_str)
                .is_some_and(|fact_id| valid_allowed_fact_id(fact_id, allowed_fact_ids)),
            "merge" => valid_merge_op(raw, allowed_fact_ids),
            "normalize_tags" | "merge_entities" | "add_alias" | "link_facts" => {
                valid_grooming_op(raw, allowed_fact_ids)
            }
            _ => false,
        };
        if valid {
            accepted.push(raw.clone());
        } else {
            rejected.push(rejected_memory_op(
                raw,
                "operation was unsupported or referenced evidence outside the verified page",
            ));
        }
    }
    let destructive_count = accepted
        .iter()
        .filter(|op| {
            matches!(
                op.get("op").and_then(Value::as_str),
                Some("delete" | "merge")
            )
        })
        .count();
    let has_grooming = accepted.iter().any(|op| {
        !matches!(
            op.get("op").and_then(Value::as_str),
            Some("delete" | "merge")
        )
    });
    if destructive_count > 1 || (destructive_count == 1 && has_grooming) {
        rejected.extend(accepted.drain(..).map(|raw| {
            rejected_memory_op(
                &raw,
                "a batch may contain one destructive operation or grooming operations, not both",
            )
        }));
    }
    (accepted, rejected)
}

fn valid_allowed_fact_id(fact_id: &str, allowed_fact_ids: &BTreeSet<String>) -> bool {
    allowed_fact_ids.contains(fact_id) && FactId::new(fact_id.to_owned()).is_ok()
}

fn valid_merge_op(raw: &Value, allowed_fact_ids: &BTreeSet<String>) -> bool {
    let Some(winner_id) = raw.get("winner_id").and_then(Value::as_str) else {
        return false;
    };
    let Some(loser_ids) = raw.get("loser_ids").and_then(Value::as_array) else {
        return false;
    };
    let mut unique_losers = BTreeSet::new();
    valid_allowed_fact_id(winner_id, allowed_fact_ids)
        && !loser_ids.is_empty()
        && loser_ids.iter().all(|loser_id| {
            loser_id.as_str().is_some_and(|loser_id| {
                loser_id != winner_id
                    && unique_losers.insert(loser_id)
                    && valid_allowed_fact_id(loser_id, allowed_fact_ids)
            })
        })
        && raw
            .get("merged_content")
            .is_none_or(|content| content.as_str().is_some())
}

fn valid_grooming_op(raw: &Value, allowed_fact_ids: &BTreeSet<String>) -> bool {
    let Ok(operation) = serde_json::from_value::<MemoryGroomingOperation>(raw.clone()) else {
        return false;
    };
    match operation {
        MemoryGroomingOperation::NormalizeTags {
            fact_id,
            evidence_fact_ids,
            ..
        } => {
            valid_allowed_fact_id(fact_id.as_str(), allowed_fact_ids)
                && valid_typed_evidence_ids(&evidence_fact_ids, allowed_fact_ids)
        }
        MemoryGroomingOperation::MergeEntities {
            evidence_fact_ids, ..
        }
        | MemoryGroomingOperation::AddAlias {
            evidence_fact_ids, ..
        } => valid_typed_evidence_ids(&evidence_fact_ids, allowed_fact_ids),
        MemoryGroomingOperation::LinkFacts {
            source_fact_id,
            target_fact_id,
            evidence_fact_ids,
            ..
        } => {
            source_fact_id != target_fact_id
                && valid_allowed_fact_id(source_fact_id.as_str(), allowed_fact_ids)
                && valid_allowed_fact_id(target_fact_id.as_str(), allowed_fact_ids)
                && valid_typed_evidence_ids(&evidence_fact_ids, allowed_fact_ids)
        }
    }
}

fn valid_typed_evidence_ids(ids: &[FactId], allowed_fact_ids: &BTreeSet<String>) -> bool {
    !ids.is_empty()
        && ids
            .iter()
            .all(|id| valid_allowed_fact_id(id.as_str(), allowed_fact_ids))
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

async fn apply_memory_curation_ops<A: tracedecay_store::ProjectMemoryFactStore>(
    memory: &MemoryApplication<A>,
    run_id: &str,
    operations: &[Value],
) -> Result<(usize, Vec<Value>)> {
    let actor = ActorId::new("automation:memory-curator").map_err(memory_contract_error)?;
    let context = MemoryOperationContext::from_logical_effect(
        memory.owner(),
        "automation-memory-curator",
        &(run_id, operations),
        Some(actor),
    )
    .map_err(memory_application_error)?;
    match operations
        .first()
        .and_then(|operation| operation.get("op").and_then(Value::as_str))
    {
        Some("delete") => {
            let fact_id = operations[0]
                .get("fact_id")
                .and_then(Value::as_str)
                .ok_or_else(|| memory_validation_error("validated delete lost fact_id"))?;
            let fact_id = FactId::new(fact_id.to_owned()).map_err(memory_contract_error)?;
            let removed = memory
                .remove_fact(fact_id.clone(), context)
                .await
                .map_err(memory_application_error)?;
            Ok((
                usize::from(removed),
                vec![json!({
                    "op": "delete",
                    "fact_id": fact_id,
                    "status": if removed { "deleted" } else { "not_found" },
                })],
            ))
        }
        Some("merge") => {
            let winner_id = operations[0]
                .get("winner_id")
                .and_then(Value::as_str)
                .ok_or_else(|| memory_validation_error("validated merge lost winner_id"))?;
            let winner_id = FactId::new(winner_id.to_owned()).map_err(memory_contract_error)?;
            let loser_ids = operations[0]
                .get("loser_ids")
                .and_then(Value::as_array)
                .ok_or_else(|| memory_validation_error("validated merge lost loser_ids"))?
                .iter()
                .map(|value| {
                    value
                        .as_str()
                        .ok_or_else(|| memory_validation_error("validated merge lost loser id"))
                        .and_then(|value| {
                            FactId::new(value.to_owned()).map_err(memory_contract_error)
                        })
                })
                .collect::<Result<Vec<_>>>()?;
            let merged_content = operations[0]
                .get("merged_content")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned);
            let outcome = memory
                .dashboard_merge_fact_ids(
                    winner_id.clone(),
                    loser_ids.clone(),
                    merged_content,
                    context,
                )
                .await
                .map_err(memory_application_error)?;
            Ok((
                1,
                vec![json!({
                    "op": "merge",
                    "winner_id": winner_id,
                    "loser_ids": loser_ids,
                    "deleted_loser_ids": outcome
                        .deleted_losers()
                        .iter()
                        .map(|target| target.fact_id().as_str())
                        .collect::<Vec<_>>(),
                    "status": "merged",
                })],
            ))
        }
        Some(_) => {
            let grooming = operations
                .iter()
                .cloned()
                .map(|operation| {
                    serde_json::from_value::<MemoryGroomingOperation>(operation).map_err(|error| {
                        memory_validation_error(format!(
                            "validated grooming operation could not be reconstructed: {error}"
                        ))
                    })
                })
                .collect::<Result<Vec<_>>>()?;
            let count = grooming.len();
            let report = memory
                .dashboard_apply_grooming(grooming, 0.0, context)
                .await
                .map_err(memory_application_error)?;
            Ok((
                count,
                vec![json!({
                    "op": "grooming_batch",
                    "status": "applied",
                    "operation_count": count,
                    "receipt": report,
                })],
            ))
        }
        None => Ok((0, Vec::new())),
    }
}

fn memory_application_error(error: impl std::fmt::Display) -> TraceDecayError {
    TraceDecayError::Database {
        operation: "apply validated memory curator operations".to_owned(),
        message: error.to_string(),
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

fn memory_curation_apply_policy(
    accepted_ops: Option<&Value>,
    applied_count: Option<usize>,
) -> Value {
    let ops = accepted_ops
        .and_then(Value::as_array)
        .map_or_else(|| &[] as &[Value], Vec::as_slice);
    let destructive = memory_destructive_op_counts(ops);
    let accepted_count = ops.len();
    let policy = applied_count.map_or_else(
        || MemoryApplyPolicy::curation_ops(accepted_count),
        |applied_count| MemoryApplyPolicy::applied_curation_ops(accepted_count, applied_count),
    );
    let apply_instructions = match policy.decision() {
        MemoryApplyDecision::AutoApplyAllowed => {
            "Accepted memory curation ops were applied autonomously and recorded in automation telemetry."
        }
        MemoryApplyDecision::ApplyIncomplete => {
            "Automation attempted to apply accepted memory curation ops, but one or more mutations did not complete."
        }
        MemoryApplyDecision::ProposalOnly => {
            "Automation recorded accepted memory curation ops without mutating the memory store."
        }
        MemoryApplyDecision::NoValidOps | MemoryApplyDecision::NoValidFacts => {
            "No accepted memory curation ops require apply."
        }
    };
    let mut payload = policy.to_json();
    if let Some(object) = payload.as_object_mut() {
        object.insert("validated_before_apply".to_string(), json!(true));
        object.insert("accepted_count".to_string(), json!(accepted_count));
        if let Some(applied_count) = applied_count {
            object.insert("applied_count".to_string(), json!(applied_count));
            object.insert(
                "fully_applied".to_string(),
                json!(accepted_count > 0 && applied_count >= accepted_count),
            );
        }
        object.insert(
            "permanent_delete_count".to_string(),
            json!(destructive.permanent_delete_count),
        );
        object.insert(
            "merge_loser_count".to_string(),
            json!(destructive.merge_loser_count),
        );
        object.insert(
            "destructive_target_count".to_string(),
            json!(destructive.permanent_delete_count + destructive.merge_loser_count),
        );
        object.insert("apply_instructions".to_string(), json!(apply_instructions));
    }
    payload
}

#[derive(Debug, Default)]
struct MemoryDestructiveOpCounts {
    permanent_delete_count: usize,
    merge_loser_count: usize,
}

fn memory_destructive_op_counts(ops: &[Value]) -> MemoryDestructiveOpCounts {
    let mut counts = MemoryDestructiveOpCounts::default();
    for op in ops {
        match op.get("op").and_then(Value::as_str) {
            Some("delete") => counts.permanent_delete_count += 1,
            Some("merge") => {
                counts.merge_loser_count += op
                    .get("loser_ids")
                    .and_then(Value::as_array)
                    .map_or(0, Vec::len);
            }
            _ => {}
        }
    }
    counts
}

fn annotate_memory_curation_report(report: &mut Value, apply_policy: Value) {
    if let Some(object) = report.as_object_mut() {
        object.insert("automation_apply_policy".to_string(), apply_policy.clone());
    }
    if let Some(llm_apply) = report.get_mut("llm_apply").and_then(Value::as_object_mut) {
        llm_apply.insert("apply_policy".to_string(), apply_policy);
    }
}

fn default_max_clusters() -> usize {
    CURATION_DEFAULT_MAX_CLUSTERS
}

fn default_min_confidence() -> f64 {
    CURATION_DEFAULT_MIN_CONFIDENCE
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn memory_curator_request_does_not_duplicate_review_messages() {
        let marker = "cluster-evidence-that-must-appear-once";
        let review = json!({
            "status": "needs_llm_review",
            "messages": [
                { "role": "system", "content": "return strict JSON" },
                { "role": "user", "content": marker },
            ],
        });

        let prompt = build_memory_curator_prompt();
        let request = AgentTaskRequest::new(
            "run-1".to_string(),
            AgentTaskKind::MemoryCurator,
            prompt.clone(),
            None,
            memory_curator_backend_context(&review, 0.8),
        );
        let backend_message = request.backend_message().unwrap();

        assert!(prompt.contains("generation-bound memory pairs"));
        assert_eq!(backend_message.matches(marker).count(), 1);
        assert_eq!(request.context["apply"], json!(true));
        assert_eq!(
            request.context["memory_apply_policy"],
            json!("validate_then_apply")
        );
    }

    #[test]
    fn memory_curator_request_stays_below_codex_limit_for_large_review() {
        const CODEX_APP_SERVER_MAX_INPUT_CHARS: usize = 1_048_576;
        let review = json!({
            "status": "needs_llm_review",
            "messages": [
                { "role": "system", "content": "return strict JSON" },
                { "role": "user", "content": "x".repeat(600_000) },
            ],
        });
        let request = AgentTaskRequest::new(
            "run-1".to_string(),
            AgentTaskKind::MemoryCurator,
            build_memory_curator_prompt(),
            None,
            memory_curator_backend_context(&review, 0.8),
        );

        let backend_message = request.backend_message().unwrap();

        assert!(backend_message.len() < CODEX_APP_SERVER_MAX_INPUT_CHARS);
    }
}
