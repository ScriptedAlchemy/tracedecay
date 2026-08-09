use std::collections::BTreeSet;

use serde_json::{Value, json};
use tracedecay_domain::{FactId, FactOwnerV1};
use tracedecay_runtime_core::memory::encoding::HolographicEncoder;
use tracedecay_runtime_core::memory::similarity::{lexical_overlap, similarity_classification};
use tracedecay_store::{CurrentFactsQuery, ProjectMemoryFactStore, StoredFactV1};
use tracedecay_usecases::memory::MemoryApplication;

use crate::errors::Result;

use super::{memory_application_error, memory_contract_error};

const CURATION_FACT_SCAN_LIMIT: usize = 1_000;
// The classifier admits `merge_candidate` at 0.90 and `likely_duplicate` at
// 0.95, so the lower classification floor is the bounded review prefilter.
const CURATION_SIMILARITY_THRESHOLD_MILLIONTHS: u32 = 900_000;

pub(super) async fn memory_curator_review<A: ProjectMemoryFactStore>(
    memory: &MemoryApplication<A>,
    owner: &FactOwnerV1,
    max_pairs: usize,
) -> Result<(Value, BTreeSet<FactId>)> {
    let facts = memory
        .query_current_facts(
            CurrentFactsQuery::new(owner.clone(), None, CURATION_FACT_SCAN_LIMIT)
                .map_err(memory_contract_error)?,
        )
        .await
        .map_err(memory_application_error)?;
    let page_truncated = if facts.len() == CURATION_FACT_SCAN_LIMIT {
        let after = facts.last().map(|fact| fact.fact_id().clone());
        !memory
            .query_current_facts(
                CurrentFactsQuery::new(owner.clone(), after, 1).map_err(memory_contract_error)?,
            )
            .await
            .map_err(memory_application_error)?
            .is_empty()
    } else {
        false
    };
    let encoder = HolographicEncoder::new();
    let eligible = facts
        .iter()
        .filter_map(|fact| {
            fact.payload().map(|payload| {
                (
                    fact,
                    encoder.encode_fact(payload.content(), payload.entities()),
                )
            })
        })
        .collect::<Vec<_>>();
    let mut candidates = Vec::new();
    for (left_index, (left, left_vector)) in eligible.iter().enumerate() {
        for (right, right_vector) in eligible.iter().skip(left_index + 1) {
            let similarity = encoder.similarity(left_vector, right_vector);
            let similarity_millionths = (similarity.clamp(0.0, 1.0) * 1_000_000.0).round() as u32;
            if similarity_millionths < CURATION_SIMILARITY_THRESHOLD_MILLIONTHS {
                continue;
            }
            let left_content = left
                .payload()
                .map(|payload| payload.content())
                .unwrap_or("");
            let right_content = right
                .payload()
                .map(|payload| payload.content())
                .unwrap_or("");
            let (_, token_overlap, overlap_coefficient) =
                lexical_overlap(left_content, right_content);
            let classification =
                similarity_classification(similarity, token_overlap, overlap_coefficient);
            if matches!(classification, "merge_candidate" | "likely_duplicate") {
                candidates.push((similarity_millionths, *left, *right, classification));
            }
        }
    }
    candidates.sort_by(|left, right| {
        right
            .0
            .cmp(&left.0)
            .then_with(|| left.1.fact_id().cmp(right.1.fact_id()))
            .then_with(|| left.2.fact_id().cmp(right.2.fact_id()))
    });
    candidates.truncate(max_pairs);

    let mut allowed_fact_ids = BTreeSet::new();
    let pairs = candidates
        .into_iter()
        .map(|(similarity_millionths, left, right, classification)| {
            allowed_fact_ids.insert(left.fact_id().clone());
            allowed_fact_ids.insert(right.fact_id().clone());
            json!({
                "left": similarity_fact_json(left),
                "right": similarity_fact_json(right),
                "similarity_millionths": similarity_millionths,
                "classification": classification,
            })
        })
        .collect::<Vec<_>>();
    let status = match (pairs.is_empty(), page_truncated) {
        (true, true) => "partial_coverage_no_candidates",
        (true, false) => "up_to_date",
        (false, _) => "needs_llm_review",
    };
    let allowed_fact_id_values = allowed_fact_ids.iter().collect::<Vec<_>>();
    Ok((
        json!({
            "status": status,
            "clusters_reviewed": pairs.len(),
            "coverage": {
                "active_facts_scanned": facts.len(),
                "active_facts_eligible": eligible.len(),
                "active_facts_total": if page_truncated { Value::Null } else { json!(facts.len()) },
                "state": if page_truncated { "partial" } else { "complete" },
            },
            "page_truncated": page_truncated,
            "allowed_fact_ids": allowed_fact_id_values,
            "pairs": pairs,
            "messages": [{
                "role": "system",
                "content": "Return strict JSON {\"ops\":[]}. Supported operations are delete, merge, normalize_tags, merge_entities, add_alias, and link_facts. Every fact id and evidence fact id must be copied exactly from allowed_fact_ids. Every operation requires confidence in [min_confidence,1]. A batch may contain at most one destructive delete/merge and must not mix a destructive operation with grooming operations. Never use timestamps as truth or freshness evidence."
            }],
        }),
        allowed_fact_ids,
    ))
}

fn similarity_fact_json(fact: &StoredFactV1) -> Value {
    let payload = fact.payload();
    json!({
        "fact_id": fact.fact_id(),
        "content": payload.map(|payload| payload.content()),
        "category": payload.map(|payload| payload.category()),
        "tags": payload.map(|payload| payload.tags()).unwrap_or_default(),
        "trust": fact.trust(),
        "metadata": payload.map(|payload| payload.metadata()),
    })
}
