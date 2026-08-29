use std::collections::BTreeSet;

use serde_json::{Value, json};
use tracedecay_domain::{Confidence, FactCategoryV1, PayloadAccessState};
use tracedecay_store::{
    ProjectMemoryFactProjectionV1, ProjectMemoryFactSearchFilterV1, ProjectMemoryFactSearchKindV1,
    ProjectMemoryFactSearchQuery, ProjectMemoryFactStore,
};
use tracedecay_usecases::memory::ProjectMemoryFactAddRequest;

use crate::application::memory::MemoryApplication;
use crate::automation::lifecycle::AutomationRunControl;
use crate::errors::{Result, TraceDecayError};
use crate::memory::trust::{DEFAULT_TRUST, HIGH_TRUST_REPRESENTATIVE, LOW_TRUST_REPRESENTATIVE};

pub(crate) async fn validate_fact_candidates<A: ProjectMemoryFactStore>(
    memory: &MemoryApplication<A>,
    run_control: &AutomationRunControl,
    proposals: &[Value],
    evidence: &Value,
) -> Result<(Vec<Value>, Vec<Value>)> {
    let citations = EvidenceCitationSet::from_evidence(evidence);
    let mut accepted = Vec::new();
    let mut quarantined = Vec::new();
    for proposal in proposals {
        match validate_fact_candidate(memory, run_control, proposal, &citations).await? {
            FactCandidateValidation::Accepted(value) => accepted.push(value),
            FactCandidateValidation::Quarantined(value) => quarantined.push(value),
        }
    }
    Ok((accepted, quarantined))
}

enum FactCandidateValidation {
    Accepted(Value),
    Quarantined(Value),
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
            } else if kind == Some("summary_node")
                && let Some(node_id) = hit.get("node_id").and_then(Value::as_str)
            {
                citations.summary_nodes.insert(node_id.to_string());
            }
        }
        citations.collect_replay_slices(evidence);
        citations
    }

    /// Registers the session-replay evidence channel (`recent_session_slices`)
    /// so facts can cite turns and summary nodes surfaced by replay even when
    /// they did not match the keyword grep.
    fn collect_replay_slices(&mut self, evidence: &Value) {
        for session in evidence
            .pointer("/recent_session_slices/sessions")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            let Some(session_id) = session.get("session_id").and_then(Value::as_str) else {
                continue;
            };
            for slice_key in ["head", "tail"] {
                for message in session
                    .get(slice_key)
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                {
                    if let Some(message_id) = message.get("message_id").and_then(Value::as_str) {
                        self.raw_messages
                            .insert((session_id.to_string(), message_id.to_string()));
                    }
                    if let Some(store_id) = message.get("store_id").and_then(value_as_i64) {
                        self.raw_store_ids.insert(store_id);
                    }
                }
            }
            for node in session
                .get("summary_nodes")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
            {
                if let Some(node_id) = node.get("node_id").and_then(Value::as_str) {
                    self.summary_nodes.insert(node_id.to_string());
                }
            }
        }
    }

    fn contains(&self, source_span: &Value) -> bool {
        let Some(span) = source_span.as_object() else {
            return false;
        };
        let raw_store = span.get("store_id").and_then(value_as_i64);
        let raw_message = (
            span.get("session_id").and_then(Value::as_str),
            span.get("message_id").and_then(Value::as_str),
        );
        let summary_node = span.get("node_id").and_then(Value::as_str);
        let complete_raw_message = raw_message.0.is_some() == raw_message.1.is_some();
        if !complete_raw_message
            || usize::from(raw_store.is_some())
                + usize::from(raw_message.0.is_some())
                + usize::from(summary_node.is_some())
                != 1
        {
            return false;
        }
        raw_store.is_some_and(|store_id| self.raw_store_ids.contains(&store_id))
            || raw_message
                .0
                .zip(raw_message.1)
                .is_some_and(|(session_id, message_id)| {
                    self.raw_messages
                        .contains(&(session_id.to_string(), message_id.to_string()))
                })
            || summary_node.is_some_and(|node_id| self.summary_nodes.contains(node_id))
    }
}

async fn validate_fact_candidate<A: ProjectMemoryFactStore>(
    memory: &MemoryApplication<A>,
    run_control: &AutomationRunControl,
    proposal: &Value,
    citations: &EvidenceCitationSet,
) -> Result<FactCandidateValidation> {
    let Some(object) = proposal.as_object() else {
        return Ok(quarantined_fact(proposal, "item must be a JSON object"));
    };
    const ALLOWED_FACT_FIELDS: &[&str] = &[
        "content",
        "category",
        "tags",
        "entities",
        "trust",
        "reason",
        "source_span",
    ];
    if object
        .keys()
        .any(|field| !ALLOWED_FACT_FIELDS.contains(&field.as_str()))
    {
        return Ok(quarantined_fact(
            proposal,
            "fact proposal contains an unsupported field",
        ));
    }
    let Some(content) = object
        .get("content")
        .and_then(Value::as_str)
        .and_then(normalized_non_empty)
    else {
        return Ok(quarantined_fact(proposal, "content is required"));
    };
    if content.chars().count() > 1_000 {
        return Ok(quarantined_fact(
            proposal,
            "content exceeds 1000 characters",
        ));
    }
    if content.chars().any(char::is_control) {
        return Ok(quarantined_fact(
            proposal,
            "content contains a control character",
        ));
    }
    let Some(category) = object
        .get("category")
        .and_then(Value::as_str)
        .and_then(session_fact_category)
    else {
        return Ok(quarantined_fact(proposal, "valid category is required"));
    };
    let Some(tags) = string_array_field(object.get("tags")) else {
        return Ok(quarantined_fact(
            proposal,
            "tags must be an array of strings",
        ));
    };
    let Some(entities) = string_array_field(object.get("entities")) else {
        return Ok(quarantined_fact(
            proposal,
            "entities must be an array of strings",
        ));
    };
    let Some(trust) = object.get("trust") else {
        return Ok(quarantined_fact(proposal, "trust is required"));
    };
    let Some(trust) = candidate_trust_value(trust) else {
        return Ok(quarantined_fact(
            proposal,
            "trust must be a number between 0 and 1, or one of low, medium, high",
        ));
    };
    let Some(reason) = object
        .get("reason")
        .and_then(Value::as_str)
        .and_then(normalized_non_empty)
    else {
        return Ok(quarantined_fact(proposal, "reason is required"));
    };
    if reason.len() > 4_096 || reason.chars().any(char::is_control) {
        return Ok(quarantined_fact(
            proposal,
            "reason exceeds the public evidence contract",
        ));
    }
    let Some(source_span) = object.get("source_span") else {
        return Ok(quarantined_fact(proposal, "source_span is required"));
    };
    let Some(source_span_object) = source_span.as_object() else {
        return Ok(quarantined_fact(proposal, "source_span must be an object"));
    };
    const ALLOWED_SOURCE_SPAN_FIELDS: &[&str] =
        &["session_id", "message_id", "store_id", "node_id"];
    if source_span_object
        .keys()
        .any(|field| !ALLOWED_SOURCE_SPAN_FIELDS.contains(&field.as_str()))
    {
        return Ok(quarantined_fact(
            proposal,
            "source_span contains an unsupported field",
        ));
    }
    if source_span_object.values().any(Value::is_null) {
        return Ok(quarantined_fact(
            proposal,
            "source_span optional identities must be omitted instead of null",
        ));
    }
    if source_span_object.values().any(|value| {
        value
            .as_str()
            .is_some_and(|text| !public_evidence_text(text, 4_096))
    }) {
        return Ok(quarantined_fact(
            proposal,
            "source_span contains an invalid public evidence identity",
        ));
    }
    if !citations.contains(source_span) {
        return Ok(quarantined_fact(
            proposal,
            "source_span must cite a bounded session reflection evidence hit",
        ));
    }
    let exact_duplicate_id = match memory
        .find_exact_fact_by_content(&content, run_control.read_control())
        .await
        .map_err(|error| {
            TraceDecayError::database_operation(
                "validate session reflector exact duplicate through memory authority",
                error,
            )
        })? {
        None => None,
        Some(ProjectMemoryFactProjectionV1::Available(fact)) => {
            Some(fact.fact_id().as_str().to_owned())
        }
        Some(ProjectMemoryFactProjectionV1::Unavailable(unavailable)) => {
            return Ok(quarantined_unavailable_exact_duplicate(
                proposal,
                unavailable.fact_id().as_str(),
                unavailable.payload_access(),
            ));
        }
    };
    if let Some(fact_id) = exact_duplicate_id {
        let reason = format!("exact duplicate of canonical fact {fact_id}");
        return Ok(quarantined_fact_with_validation(
            proposal,
            &reason,
            &json!({
                "status": "quarantined",
                "reason": reason,
                "dedupe": {
                    "exact_duplicate_canonical_fact_id": fact_id,
                },
            }),
        ));
    }
    let filter =
        ProjectMemoryFactSearchFilterV1::new(Some(category), None, None).map_err(|error| {
            TraceDecayError::database_operation(
                "construct session reflector canonical dedupe filter",
                error,
            )
        })?;
    let query = ProjectMemoryFactSearchQuery::with_filter(
        memory.owner().clone(),
        ProjectMemoryFactSearchKindV1::Search,
        Some(content.clone()),
        filter,
        None,
        1,
    )
    .map_err(|error| {
        TraceDecayError::database_operation(
            "construct session reflector canonical dedupe query",
            error,
        )
    })?;
    let matches = memory
        .search_project_memory_facts(query, run_control.read_control())
        .await
        .map_err(|error| {
            TraceDecayError::database_operation(
                "validate session reflector near duplicate through memory authority",
                error,
            )
        })?;
    let nearest = matches.hits().first().map(|existing| {
        json!({
            "canonical_fact_id": existing.fact().fact_id().as_str(),
            "score": f64::from(existing.score_millionths()) / 1_000_000.0,
            "category": existing.fact().category(),
        })
    });
    if let Some(existing) = matches
        .hits()
        .first()
        .filter(|result| result.score_millionths() >= 900_000)
    {
        let score = f64::from(existing.score_millionths()) / 1_000_000.0;
        let reason = format!(
            "near duplicate of canonical fact {} with score {score:.3}",
            existing.fact().fact_id().as_str(),
        );
        return Ok(quarantined_fact_with_validation(
            proposal,
            &reason,
            &json!({
                "status": "quarantined",
                "reason": reason,
                "dedupe": {
                    "nearest": nearest,
                    "near_duplicate_threshold": 0.90,
                },
            }),
        ));
    }
    let trust = Confidence::new(trust).map_err(|error| {
        TraceDecayError::database_operation(
            "construct session reflector canonical fact confidence",
            error,
        )
    })?;
    let evidence_item = json!({
        "content": content.clone(),
        "category": category,
        "tags": tags.clone(),
        "entities": entities.clone(),
        "trust": canonical_evidence_trust(object.get("trust"), trust.as_f64()),
        "source_span": source_span,
        "reason": reason.clone(),
    });
    let request = ProjectMemoryFactAddRequest {
        content,
        category,
        source_label: Some("session_reflector".to_string()),
        tags,
        entities,
        trust: Some(trust),
        metadata: json!({
            "source": "session_reflector",
            "source_span": source_span,
            "reason": reason,
            "trust_reason": reason,
        }),
    };
    Ok(FactCandidateValidation::Accepted(json!({
        "add_fact_request": request,
        "item": evidence_item,
        "validation": {
            "status": "accepted",
            "dedupe": {
                "nearest": nearest,
                "near_duplicate_threshold": 0.90,
            },
            "conflict": {
                "source": "apply_time_add_fact_diff",
                "note": "TraceDecay::add_fact reports possible_conflict during automatic apply",
            },
        },
    })))
}

fn session_fact_category(category: &str) -> Option<FactCategoryV1> {
    match category {
        "general" => Some(FactCategoryV1::General),
        "user_pref" => Some(FactCategoryV1::UserPref),
        "project" => Some(FactCategoryV1::Project),
        "tool" => Some(FactCategoryV1::Tool),
        "decision" => Some(FactCategoryV1::Decision),
        "code_area" => Some(FactCategoryV1::CodeArea),
        _ => None,
    }
}

/// Accepts numeric trust in `[0, 1]` plus the `low`/`medium`/`high` bucket
/// labels models frequently emit despite the numeric prompt instruction.
/// Buckets map to the representative scores defined next to
/// [`crate::memory::trust::trust_bucket`], so they cannot drift out of their
/// documented ranges.
///
/// Deliberate decision: the prompt forbids string labels, but they are
/// accepted defensively rather than rejecting an otherwise valid fact. Note
/// that the "high" representative can clear automatic-apply thresholds —
/// this is intentional.
fn candidate_trust_value(value: &Value) -> Option<f64> {
    if let Some(trust) = value.as_f64() {
        return (0.0..=1.0).contains(&trust).then_some(trust);
    }
    match value.as_str()?.trim().to_ascii_lowercase().as_str() {
        "low" => Some(LOW_TRUST_REPRESENTATIVE),
        "medium" => Some(DEFAULT_TRUST),
        "high" => Some(HIGH_TRUST_REPRESENTATIVE),
        _ => None,
    }
}

fn canonical_evidence_trust(value: Option<&Value>, trust: f64) -> Value {
    match value.and_then(Value::as_str) {
        Some(bucket) => Value::String(bucket.trim().to_ascii_lowercase()),
        None => json!(trust),
    }
}

fn public_evidence_text(value: &str, max_bytes: usize) -> bool {
    let value = value.trim();
    !value.is_empty() && value.len() <= max_bytes && !value.chars().any(char::is_control)
}

fn value_as_i64(value: &Value) -> Option<i64> {
    value
        .as_i64()
        .or_else(|| value.as_u64().and_then(|number| i64::try_from(number).ok()))
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
        let value = item.as_str().and_then(normalized_non_empty)?;
        if value.len() > 4_096 || value.chars().any(char::is_control) {
            return None;
        }
        values.push(value);
    }
    Some(values)
}

fn quarantined_fact(proposal: &Value, reason: &str) -> FactCandidateValidation {
    quarantined_fact_with_validation(
        proposal,
        reason,
        &json!({
            "status": "quarantined",
            "reason": reason,
        }),
    )
}

fn quarantined_unavailable_exact_duplicate(
    proposal: &Value,
    fact_id: &str,
    payload_access: PayloadAccessState,
) -> FactCandidateValidation {
    let reason = "exact duplicate is unavailable for safe validation";
    quarantined_fact_with_validation(
        proposal,
        reason,
        &json!({
            "status": "quarantined",
            "reason": reason,
            "dedupe": {
                "exact_match": {
                    "canonical_fact_id": fact_id,
                    "payload_access": payload_access_label(payload_access),
                },
            },
        }),
    )
}

fn payload_access_label(value: PayloadAccessState) -> &'static str {
    match value {
        PayloadAccessState::Eligible => "eligible",
        PayloadAccessState::Redacted => "redacted",
        PayloadAccessState::Quarantined => "quarantined",
        PayloadAccessState::RetentionExpired => "retention_expired",
        PayloadAccessState::Deleted => "deleted",
        PayloadAccessState::Unavailable => "unavailable",
        PayloadAccessState::Ambiguous => "ambiguous",
    }
}

fn quarantined_fact_with_validation(
    proposal: &Value,
    reason: &str,
    validation: &Value,
) -> FactCandidateValidation {
    FactCandidateValidation::Quarantined(json!({
        "item": proposal,
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
