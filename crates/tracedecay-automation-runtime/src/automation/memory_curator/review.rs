use std::collections::BTreeMap;
use std::path::Path;

use serde_json::{Value, json};
use tracedecay_domain::{FactEventId, FactId, FactOwnerV1};
use tracedecay_store::{
    ProjectMemoryFactListQueryV1, ProjectMemoryFactProjectionV1, ProjectMemoryFactStore,
    ProjectMemoryGraphQueryV1, ProjectMemoryGraphStore, ProjectMemoryGraphTargetV1,
};
use tracedecay_usecases::memory::MemoryApplication;

use crate::errors::Result;

use super::super::lifecycle::AutomationRunControl;
use super::super::run_ledger::load_latest_task_validation_pointer;
use super::{memory_application_error, memory_contract_error};
use crate::errors::TraceDecayError;

const CURATION_FACT_REVIEW_LIMIT: usize = 1_000;

pub(super) struct MemoryCuratorReviewPage {
    pub review: Value,
    pub allowed_facts: BTreeMap<FactId, FactEventId>,
    pub resume_after_fact_id: Option<FactId>,
}

pub(super) async fn memory_curator_resume_cursor(root: &Path) -> Result<Option<FactId>> {
    match load_latest_task_validation_pointer(
        root,
        "memory_curator",
        "/pagination/resume_after_fact_id",
    )
    .await?
    {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(value)) => {
            FactId::new(value)
                .map(Some)
                .map_err(|error| TraceDecayError::Config {
                    message: format!("invalid durable memory curator cursor: {error}"),
                })
        }
        Some(_) => Err(TraceDecayError::Config {
            message: "invalid durable memory curator cursor shape".to_owned(),
        }),
    }
}

pub(super) fn attach_pagination_summary(
    summary: &mut Value,
    started_after_fact_id: Option<&FactId>,
    resume_after_fact_id: Option<&FactId>,
) {
    if let Some(object) = summary.as_object_mut() {
        object.insert(
            "pagination".to_owned(),
            json!({
                "started_after_fact_id": started_after_fact_id,
                "resume_after_fact_id": resume_after_fact_id,
            }),
        );
    }
}

pub(super) async fn memory_curator_review<A: ProjectMemoryFactStore + ProjectMemoryGraphStore>(
    memory: &MemoryApplication<A>,
    owner: &FactOwnerV1,
    fact_review_limit: usize,
    after_fact_id: Option<FactId>,
    run_control: &AutomationRunControl,
) -> Result<MemoryCuratorReviewPage> {
    let limit = fact_review_limit.clamp(1, CURATION_FACT_REVIEW_LIMIT);
    let page = memory
        .list_project_memory_facts(
            ProjectMemoryFactListQueryV1::new(owner.clone(), None, None, after_fact_id, limit)
                .map_err(memory_contract_error)?,
            run_control.read_control(),
        )
        .await
        .map_err(memory_application_error)?;
    let mut allowed_facts = BTreeMap::new();
    let mut unavailable_count = 0usize;
    let facts = page
        .facts()
        .iter()
        .filter_map(|projection| match projection {
            ProjectMemoryFactProjectionV1::Available(fact) => {
                allowed_facts.insert(fact.fact_id().clone(), fact.last_event_id().clone());
                Some(json!({
                    "fact_id": fact.fact_id(),
                    "last_event_id": fact.last_event_id(),
                    "content": fact.content(),
                    "category": fact.category(),
                    "tags": fact.tags(),
                    "trust": fact.trust(),
                    "metadata": fact.metadata(),
                }))
            }
            ProjectMemoryFactProjectionV1::Unavailable(_) => {
                unavailable_count = unavailable_count.saturating_add(1);
                None
            }
        })
        .collect::<Vec<_>>();
    let next_after_fact_id = page.next_after_fact_id().cloned();
    let graph = memory
        .project_memory_graph(
            ProjectMemoryGraphQueryV1::new(
                owner.clone(),
                allowed_facts.keys().cloned().collect(),
                4_096,
            )
            .map_err(memory_contract_error)?,
            run_control.read_control(),
        )
        .await
        .map_err(memory_application_error)?;
    let relations = graph
        .relations()
        .iter()
        .filter_map(|relation| match (relation.source(), relation.target()) {
            (
                ProjectMemoryGraphTargetV1::Fact(source),
                ProjectMemoryGraphTargetV1::Fact(target),
            ) => Some(json!({
                "source_fact_id": source.fact_id(),
                "target_fact_id": target.fact_id(),
                "kind": relation.kind(),
            })),
            _ => None,
        })
        .collect();
    let mut review =
        memory_curator_review_value(facts, unavailable_count, next_after_fact_id.is_some());
    review["relations"] = Value::Array(relations);
    Ok(MemoryCuratorReviewPage {
        review,
        allowed_facts,
        resume_after_fact_id: next_after_fact_id,
    })
}

pub(super) fn memory_curator_review_value(
    facts: Vec<Value>,
    unavailable_count: usize,
    page_truncated: bool,
) -> Value {
    let status = if facts.is_empty() {
        if unavailable_count > 0 || page_truncated {
            "unavailable"
        } else {
            "up_to_date"
        }
    } else {
        "needs_llm_review"
    };
    let allowed_fact_ids = facts
        .iter()
        .filter_map(|fact| fact.get("fact_id"))
        .cloned()
        .collect::<Vec<_>>();
    json!({
        "status": status,
        "facts_reviewed": facts.len(),
        "coverage": {
            "active_facts_scanned": facts.len().saturating_add(unavailable_count),
            "active_facts_available": facts.len(),
            "active_facts_unavailable": unavailable_count,
            "active_facts_total": if page_truncated {
                Value::Null
            } else {
                json!(facts.len().saturating_add(unavailable_count))
            },
            "state": if page_truncated || unavailable_count > 0 { "partial" } else { "complete" },
        },
        "page_truncated": page_truncated,
        "allowed_fact_ids": allowed_fact_ids,
        "facts": facts,
        "messages": [{
            "role": "system",
            "content": "Return strict JSON {\"ops\":[]} with at most 256 operations. Review only the supplied canonical facts and current relations. Supported operations are add, update, merge, remove, normalize_tags, and link_facts. Every target, relation endpoint, and evidence item must copy the exact fact_id plus last_event_id pair from facts. Every operation requires nonempty evidence_facts, a bounded reason (except normalize_tags/link_facts), and confidence in [min_confidence,1]. Do not repeat a relation already listed. Link facts must use distinct source and target snapshots plus source_label and metadata. Never use timestamps as truth or freshness evidence."
        }],
    })
}
