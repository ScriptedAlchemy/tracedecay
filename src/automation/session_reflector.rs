use std::collections::BTreeSet;

use serde_json::{json, Value};

use crate::errors::Result;
use crate::memory::retrieval::FactRetriever;
use crate::memory::types::{AddFactRequest, MemoryCategory};
use crate::tracedecay::TraceDecay;

pub(crate) async fn validate_fact_proposals(
    cg: &TraceDecay,
    proposals: &[Value],
    evidence: &Value,
) -> Result<(Vec<Value>, Vec<Value>)> {
    let db = cg.open_project_store_db().await?;
    let conn = db.conn();
    let retriever = FactRetriever::new(conn);
    let citations = EvidenceCitationSet::from_evidence(evidence);
    let mut accepted = Vec::new();
    let mut rejected = Vec::new();
    for proposal in proposals {
        match validate_fact_proposal(conn, &retriever, proposal, &citations).await? {
            FactProposalValidation::Accepted(value) => accepted.push(value),
            FactProposalValidation::Rejected(value) => rejected.push(value),
        }
    }
    Ok((accepted, rejected))
}

enum FactProposalValidation {
    Accepted(Value),
    Rejected(Value),
}

struct EvidenceCitationSet {
    raw_messages: BTreeSet<(String, String)>,
    raw_store_ids: BTreeSet<i64>,
    summary_nodes: BTreeSet<String>,
}

impl EvidenceCitationSet {
    fn from_evidence(evidence: &Value) -> Self {
        let mut citations = Self {
            raw_messages: BTreeSet::new(),
            raw_store_ids: BTreeSet::new(),
            summary_nodes: BTreeSet::new(),
        };
        for hit in evidence
            .get("hits")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            let kind = hit.get("kind").and_then(Value::as_str);
            if kind == Some("raw_message") {
                if let (Some(session_id), Some(message_id)) = (
                    hit.get("session_id").and_then(Value::as_str),
                    hit.get("message_id").and_then(Value::as_str),
                ) {
                    citations
                        .raw_messages
                        .insert((session_id.to_string(), message_id.to_string()));
                }
                if let Some(store_id) = hit.get("store_id").and_then(value_as_i64) {
                    citations.raw_store_ids.insert(store_id);
                }
            } else if kind == Some("summary_node") {
                if let Some(node_id) = hit.get("node_id").and_then(Value::as_str) {
                    citations.summary_nodes.insert(node_id.to_string());
                }
            }
        }
        citations
    }

    fn contains(&self, source_span: &Value) -> bool {
        let Some(span) = source_span.as_object() else {
            return false;
        };
        if let Some(store_id) = span.get("store_id").and_then(value_as_i64) {
            return self.raw_store_ids.contains(&store_id);
        }
        if let (Some(session_id), Some(message_id)) = (
            span.get("session_id").and_then(Value::as_str),
            span.get("message_id").and_then(Value::as_str),
        ) {
            return self
                .raw_messages
                .contains(&(session_id.to_string(), message_id.to_string()));
        }
        span.get("node_id")
            .and_then(Value::as_str)
            .is_some_and(|node_id| self.summary_nodes.contains(node_id))
    }
}

async fn validate_fact_proposal(
    conn: &libsql::Connection,
    retriever: &FactRetriever<'_>,
    proposal: &Value,
    citations: &EvidenceCitationSet,
) -> Result<FactProposalValidation> {
    let Some(object) = proposal.as_object() else {
        return Ok(rejected_fact(proposal, "proposal must be a JSON object"));
    };
    let Some(content) = object
        .get("content")
        .and_then(Value::as_str)
        .and_then(normalized_non_empty)
    else {
        return Ok(rejected_fact(proposal, "content is required"));
    };
    if content.chars().count() > 1_000 {
        return Ok(rejected_fact(proposal, "content exceeds 1000 characters"));
    }
    let Some(category) = object
        .get("category")
        .and_then(Value::as_str)
        .and_then(|value| value.parse::<MemoryCategory>().ok())
    else {
        return Ok(rejected_fact(proposal, "valid category is required"));
    };
    let Some(tags) = string_array_field(object.get("tags")) else {
        return Ok(rejected_fact(proposal, "tags must be an array of strings"));
    };
    let Some(entities) = string_array_field(object.get("entities")) else {
        return Ok(rejected_fact(
            proposal,
            "entities must be an array of strings",
        ));
    };
    let trust = match object.get("trust") {
        Some(value) => match value.as_f64() {
            Some(trust) if (0.0..=1.0).contains(&trust) => Some(trust),
            _ => return Ok(rejected_fact(proposal, "trust must be between 0 and 1")),
        },
        None => return Ok(rejected_fact(proposal, "trust is required")),
    };
    if object.contains_key("confidence") {
        return Ok(rejected_fact(
            proposal,
            "confidence is not supported; use trust",
        ));
    }
    let Some(reason) = object
        .get("reason")
        .and_then(Value::as_str)
        .and_then(normalized_non_empty)
    else {
        return Ok(rejected_fact(proposal, "reason is required"));
    };
    let Some(source_span) = object.get("source_span") else {
        return Ok(rejected_fact(proposal, "source_span is required"));
    };
    if !citations.contains(source_span) {
        return Ok(rejected_fact(
            proposal,
            "source_span must cite a bounded session reflection evidence hit",
        ));
    }
    if let Some(fact_id) = exact_fact_content_id(conn, &content).await? {
        let reason = format!("exact duplicate of fact #{fact_id}");
        return Ok(rejected_fact_with_validation(
            proposal,
            &reason,
            &json!({
                "status": "rejected",
                "reason": reason,
                "dedupe": {
                    "exact_duplicate_fact_id": fact_id,
                },
            }),
        ));
    }
    let matches = retriever.search(&content, Some(category), None, 1).await?;
    let nearest = matches.first().map(|existing| {
        json!({
            "fact_id": existing.fact.fact_id,
            "score": existing.score,
            "category": existing.fact.category,
        })
    });
    if let Some(existing) = matches.first().filter(|result| result.score >= 0.90) {
        let reason = format!(
            "near duplicate of fact #{} with score {:.3}",
            existing.fact.fact_id, existing.score
        );
        return Ok(rejected_fact_with_validation(
            proposal,
            &reason,
            &json!({
                "status": "rejected",
                "reason": reason,
                "dedupe": {
                    "nearest": nearest,
                    "near_duplicate_threshold": 0.90,
                },
            }),
        ));
    }
    let request = AddFactRequest {
        content,
        category,
        source: Some("session_reflector".to_string()),
        tags,
        entities,
        trust,
        metadata: json!({
            "source": "session_reflector",
            "source_span": source_span,
            "reason": reason,
            "trust_reason": reason,
        }),
    };
    Ok(FactProposalValidation::Accepted(json!({
        "add_fact_request": request,
        "proposal": proposal,
        "validation": {
            "status": "accepted",
            "dedupe": {
                "nearest": nearest,
                "near_duplicate_threshold": 0.90,
            },
            "conflict": {
                "source": "apply_time_add_fact_diff",
                "note": "TraceDecay::add_fact reports possible_conflict during explicit approval apply",
            },
        },
    })))
}

fn value_as_i64(value: &Value) -> Option<i64> {
    value
        .as_i64()
        .or_else(|| value.as_u64().and_then(|number| i64::try_from(number).ok()))
}

async fn exact_fact_content_id(conn: &libsql::Connection, content: &str) -> Result<Option<i64>> {
    let mut rows = conn
        .query(
            "SELECT fact_id FROM memory_facts WHERE content = ?1 LIMIT 1",
            libsql::params![content],
        )
        .await?;
    Ok(rows
        .next()
        .await?
        .map(|row| row.get::<i64>(0))
        .transpose()?)
}

fn string_array_field(value: Option<&Value>) -> Option<Vec<String>> {
    let Some(value) = value else {
        return Some(Vec::new());
    };
    let array = value.as_array()?;
    if array.len() > 20 {
        return None;
    }
    let mut values = Vec::new();
    for item in array {
        values.push(item.as_str().and_then(normalized_non_empty)?);
    }
    Some(values)
}

fn rejected_fact(proposal: &Value, reason: &str) -> FactProposalValidation {
    rejected_fact_with_validation(
        proposal,
        reason,
        &json!({
            "status": "rejected",
            "reason": reason,
        }),
    )
}

fn rejected_fact_with_validation(
    proposal: &Value,
    reason: &str,
    validation: &Value,
) -> FactProposalValidation {
    FactProposalValidation::Rejected(json!({
        "proposal": proposal,
        "reason": reason,
        "validation": validation,
    }))
}

fn normalized_non_empty(value: &str) -> Option<String> {
    let value = value.trim();
    if value.is_empty() {
        None
    } else {
        Some(value.to_string())
    }
}
