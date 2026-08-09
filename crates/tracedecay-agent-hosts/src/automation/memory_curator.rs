use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::BTreeSet;
use std::sync::Arc;
use tracedecay_domain::configuration::ConfigurationRevisionId;
use tracedecay_domain::{
    ActorId, Confidence, FactId, FactOwnerV1, ManifestDigest, canonical_sha256,
};

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
use tracedecay_policy::{
    CurationApplyAuthorityV1, CurationApplyDecisionV1, CurationApplyPolicyInputV1,
    CurationApplySubjectV1, CurationValidationDispositionV1, evaluate_curation_apply,
};
use tracedecay_runtime_core::memory::types::FactRelationKind;
use tracedecay_store::ProjectMemoryFactRelationV1;
use tracedecay_usecases::memory::{
    CanonicalMemoryGroomingOperation, MemoryApplication, MemoryOperationContext,
};

const CURATION_DEFAULT_MAX_CLUSTERS: usize = 12;
const CURATION_DEFAULT_MIN_CONFIDENCE: f64 = 0.72;

mod review;
use review::memory_curator_review;

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
    configuration_revision_id: &ConfigurationRevisionId,
    backend: &dyn AgentTaskBackend,
    options: MemoryCuratorAutomationOptions,
) -> Result<MemoryCuratorAutomationRun> {
    let sessions_db = super::runner::project_automation_sessions(cg).await?;
    run_memory_curator_for_store(
        MemoryCuratorStore::Project { cg, sessions_db },
        config,
        configuration_revision_id,
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
    configuration_revision_id: &ConfigurationRevisionId,
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
        configuration_revision_id,
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

async fn run_memory_curator_for_store(
    store: MemoryCuratorStore<'_>,
    config: &AutomationConfig,
    configuration_revision_id: &ConfigurationRevisionId,
    backend: &dyn AgentTaskBackend,
    options: MemoryCuratorAutomationOptions,
) -> Result<MemoryCuratorAutomationRun> {
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
    let database = store.open_memory_database().await?;
    let memory = MemoryApplication::new(owner.clone(), DatabaseFactStore::new(&database)).map_err(
        |error| TraceDecayError::Config {
            message: format!("initialize memory curator authority: {error}"),
        },
    )?;
    let (llm_review, allowed_fact_ids) =
        memory_curator_review(&memory, &owner, max_clusters).await?;
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
                retry_report = repair_retry_report;
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
        retry_report = repair_retry_report;
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
    let (applied_count, receipts) = if curation_decision.allows_apply() {
        let result =
            apply_memory_curation_ops(&memory, &run.run_id, &accepted_ops, min_confidence).await;
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
        (applied_count, receipts)
    } else {
        (0, Vec::new())
    };
    let curation_policy = memory_curation_report(&accepted_ops, curation_decision, applied_count);
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
    annotate_memory_curation_report(&mut validated_report, curation_policy);

    let validation_report = validated_report.get("llm_apply").cloned();
    let applied_ops = validated_report
        .pointer("/llm_apply/receipts")
        .filter(|value| {
            value
                .as_array()
                .is_some_and(|receipts| !receipts.is_empty())
        })
        .cloned();
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
    "Review only the canonical current-fact pairs in context.llm_review. Return {\"ops\":[]} with zero or more bounded operations described by the system message. Never invent or rewrite a fact id.".to_string()
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
    allowed_fact_ids: &BTreeSet<FactId>,
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

fn valid_allowed_fact_id(fact_id: &str, allowed_fact_ids: &BTreeSet<FactId>) -> bool {
    FactId::new(fact_id.to_owned()).is_ok_and(|fact_id| allowed_fact_ids.contains(&fact_id))
}

fn valid_merge_op(raw: &Value, allowed_fact_ids: &BTreeSet<FactId>) -> bool {
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

#[derive(Clone, Debug, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
enum CanonicalGroomingWire {
    NormalizeTags {
        fact_id: FactId,
        tags: Vec<String>,
        evidence_fact_ids: Vec<FactId>,
        confidence: Confidence,
    },
    MergeEntities {
        winner_entity_id: i64,
        loser_entity_ids: Vec<i64>,
        evidence_fact_ids: Vec<FactId>,
        confidence: Confidence,
    },
    AddAlias {
        entity_id: i64,
        alias: String,
        evidence_fact_ids: Vec<FactId>,
        confidence: Confidence,
    },
    LinkFacts {
        source_fact_id: FactId,
        target_fact_id: FactId,
        relation: FactRelationKind,
        evidence_fact_ids: Vec<FactId>,
        confidence: Confidence,
        source: String,
        #[serde(default)]
        metadata: Value,
    },
}

impl CanonicalGroomingWire {
    fn into_operation(self) -> CanonicalMemoryGroomingOperation {
        match self {
            Self::NormalizeTags {
                fact_id,
                tags,
                evidence_fact_ids,
                confidence,
            } => CanonicalMemoryGroomingOperation::NormalizeTags {
                fact_id,
                tags,
                evidence_fact_ids,
                confidence,
            },
            Self::MergeEntities {
                winner_entity_id,
                loser_entity_ids,
                evidence_fact_ids,
                confidence,
            } => CanonicalMemoryGroomingOperation::MergeEntities {
                winner_entity_id,
                loser_entity_ids,
                evidence_fact_ids,
                confidence,
            },
            Self::AddAlias {
                entity_id,
                alias,
                evidence_fact_ids,
                confidence,
            } => CanonicalMemoryGroomingOperation::AddAlias {
                entity_id,
                alias,
                evidence_fact_ids,
                confidence,
            },
            Self::LinkFacts {
                source_fact_id,
                target_fact_id,
                relation,
                evidence_fact_ids,
                confidence,
                source,
                metadata,
            } => CanonicalMemoryGroomingOperation::LinkFacts {
                source_fact_id,
                target_fact_id,
                relation: match relation {
                    FactRelationKind::Supports => ProjectMemoryFactRelationV1::Supports,
                    FactRelationKind::Contradicts => ProjectMemoryFactRelationV1::Contradicts,
                    FactRelationKind::Supersedes => ProjectMemoryFactRelationV1::Supersedes,
                    FactRelationKind::DerivedFrom => ProjectMemoryFactRelationV1::DerivedFrom,
                },
                evidence_fact_ids,
                confidence,
                source,
                metadata,
            },
        }
    }
}

fn valid_grooming_op(raw: &Value, allowed_fact_ids: &BTreeSet<FactId>) -> bool {
    let Ok(operation) = serde_json::from_value::<CanonicalGroomingWire>(raw.clone()) else {
        return false;
    };
    match operation {
        CanonicalGroomingWire::NormalizeTags {
            fact_id,
            evidence_fact_ids,
            ..
        } => {
            valid_allowed_fact_id(fact_id.as_str(), allowed_fact_ids)
                && valid_typed_evidence_ids(&evidence_fact_ids, allowed_fact_ids)
        }
        CanonicalGroomingWire::MergeEntities {
            evidence_fact_ids, ..
        }
        | CanonicalGroomingWire::AddAlias {
            evidence_fact_ids, ..
        } => valid_typed_evidence_ids(&evidence_fact_ids, allowed_fact_ids),
        CanonicalGroomingWire::LinkFacts {
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

fn valid_typed_evidence_ids(ids: &[FactId], allowed_fact_ids: &BTreeSet<FactId>) -> bool {
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
    min_confidence: f64,
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
                .remove_canonical_fact(fact_id.clone(), context)
                .await
                .map_err(memory_application_error)?;
            Ok((
                usize::from(removed.removed()),
                vec![json!({
                    "op": "delete",
                    "fact_id": fact_id,
                    "status": if removed.removed() { "deleted" } else { "not_found" },
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
                .merge_canonical_facts(
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
                    serde_json::from_value::<CanonicalGroomingWire>(operation)
                        .map(CanonicalGroomingWire::into_operation)
                        .map_err(|error| {
                            memory_validation_error(format!(
                                "validated grooming operation could not be reconstructed: {error}"
                            ))
                        })
                })
                .collect::<Result<Vec<_>>>()?;
            let count = grooming.len();
            let minimum = Confidence::new(min_confidence).map_err(memory_contract_error)?;
            let report = memory
                .apply_canonical_grooming(grooming, minimum, context)
                .await
                .map_err(memory_application_error)?;
            Ok((
                count,
                vec![json!({
                    "op": "grooming_batch",
                    "status": "applied",
                    "operation_count": count,
                    "receipt": {
                        "changed_fact_ids": report.changed_facts().iter().map(|mapping| mapping.fact_id()).collect::<Vec<_>>(),
                        "normalized_tags": report.normalized_tags(),
                        "merged_entities": report.merged_entities(),
                        "aliases_added": report.aliases_added(),
                        "facts_linked": report.facts_linked(),
                    },
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

fn memory_curation_report(
    ops: &[Value],
    decision: CurationApplyDecisionV1,
    applied_count: usize,
) -> Value {
    let destructive = memory_destructive_op_counts(ops);
    let accepted_count = ops.len();
    json!({
        "decision": decision,
        "effect": {
            "accepted_count": accepted_count,
            "applied_count": applied_count,
            "fully_applied": decision.allows_apply() && applied_count >= accepted_count,
            "mutates_store": applied_count > 0,
            "permanent_delete_count": destructive.permanent_delete_count,
            "merge_loser_count": destructive.merge_loser_count,
            "destructive_target_count": destructive.permanent_delete_count + destructive.merge_loser_count,
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

fn annotate_memory_curation_report(report: &mut Value, curation_policy: Value) {
    if let Some(object) = report.as_object_mut() {
        object.insert("curation_policy".to_string(), curation_policy.clone());
    }
    if let Some(llm_apply) = report.get_mut("llm_apply").and_then(Value::as_object_mut) {
        llm_apply.insert("curation_policy".to_string(), curation_policy);
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

        assert!(prompt.contains("canonical current-fact pairs"));
        assert_eq!(backend_message.matches(marker).count(), 1);
        assert_eq!(request.context["apply"], json!(true));
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
