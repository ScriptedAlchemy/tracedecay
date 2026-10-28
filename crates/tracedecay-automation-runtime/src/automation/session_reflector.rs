use std::collections::BTreeSet;
use std::future::Future;
use std::pin::Pin;

use serde_json::{Value, json};
use tracedecay_contracts::automation::{
    SessionFactProposal, SessionFactSourceSpan, SessionFactTrust,
};
use tracedecay_contracts::retained_surfaces::MemoryAutomationFactEvidenceTrustBucketV1;
use tracedecay_domain::{Confidence, PayloadAccessState};
use tracedecay_session_memory::memory::{
    MemoryApplicationError, ProjectMemoryFactAddRequest, memory_application_error,
};
use tracedecay_store::{
    ProjectMemoryFactProjectionV1, ProjectMemoryFactSearchFilterV1, ProjectMemoryFactSearchKindV1,
    ProjectMemoryFactSearchQuery, ProjectMemoryFactStore,
};

use super::normalized_non_empty;
use crate::automation::lifecycle::AutomationRunControl;
use tracedecay_domain::errors::{Result, TraceDecayError};
use tracedecay_session_memory::memory::MemoryApplication;
use tracedecay_session_memory::memory::trust::{
    DEFAULT_TRUST, HIGH_TRUST_REPRESENTATIVE, LOW_TRUST_REPRESENTATIVE,
};

fn map_session_reflector_memory_error(
    operation: &'static str,
    error: MemoryApplicationError,
) -> TraceDecayError {
    if error.is_cancellation() {
        return memory_application_error(error);
    }
    TraceDecayError::database_operation(operation, error)
}

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
        let validated: Pin<Box<dyn Future<Output = Result<FactCandidateValidation>> + Send + '_>> =
            Box::pin(validate_fact_candidate(
                memory,
                run_control,
                proposal,
                &citations,
            ));
        match validated.await? {
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

    fn contains(&self, source_span: &SessionFactSourceSpan) -> bool {
        match source_span {
            SessionFactSourceSpan::RawMessage {
                session_id,
                message_id,
            } => self
                .raw_messages
                .contains(&(session_id.clone(), message_id.clone())),
            SessionFactSourceSpan::RawStore { store_id } => self.raw_store_ids.contains(store_id),
            SessionFactSourceSpan::SummaryNode { node_id } => self.summary_nodes.contains(node_id),
        }
    }
}

async fn validate_fact_candidate<A: ProjectMemoryFactStore>(
    memory: &MemoryApplication<A>,
    run_control: &AutomationRunControl,
    proposal: &Value,
    citations: &EvidenceCitationSet,
) -> Result<FactCandidateValidation> {
    let candidate = match serde_json::from_value::<SessionFactProposal>(proposal.clone()) {
        Ok(candidate) => candidate,
        Err(_) => {
            return Ok(quarantined_fact(
                proposal,
                "fact proposal does not match the response contract",
            ));
        }
    };
    let Some(content) = normalized_non_empty(&candidate.content) else {
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
    let category = candidate.category;
    let Some(tags) = normalized_fact_terms(&candidate.tags) else {
        return Ok(quarantined_fact(
            proposal,
            "tags exceed the public evidence contract",
        ));
    };
    let Some(entities) = normalized_fact_terms(&candidate.entities) else {
        return Ok(quarantined_fact(
            proposal,
            "entities exceed the public evidence contract",
        ));
    };
    let trust = candidate_trust_score(&candidate.trust);
    let Some(reason) = normalized_non_empty(&candidate.reason) else {
        return Ok(quarantined_fact(proposal, "reason is required"));
    };
    if reason.len() > 4_096 || reason.chars().any(char::is_control) {
        return Ok(quarantined_fact(
            proposal,
            "reason exceeds the public evidence contract",
        ));
    }
    let source_span = &candidate.source_span;
    let valid_source_identity = match source_span {
        SessionFactSourceSpan::RawMessage {
            session_id,
            message_id,
        } => public_evidence_text(session_id, 4_096) && public_evidence_text(message_id, 4_096),
        SessionFactSourceSpan::RawStore { .. } => true,
        SessionFactSourceSpan::SummaryNode { node_id } => public_evidence_text(node_id, 4_096),
    };
    if !valid_source_identity {
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
            map_session_reflector_memory_error(
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
            map_session_reflector_memory_error(
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
        "trust": canonical_evidence_trust(&candidate.trust, trust.as_f64()),
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

/// Trust syntax is decoded by the proposal contract. Bucket policy remains
/// with the memory authority, including the existing automatic-apply scores.
fn candidate_trust_score(value: &SessionFactTrust) -> f64 {
    match value {
        SessionFactTrust::Numeric(trust) => *trust,
        SessionFactTrust::Bucket(MemoryAutomationFactEvidenceTrustBucketV1::Low) => {
            LOW_TRUST_REPRESENTATIVE
        }
        SessionFactTrust::Bucket(MemoryAutomationFactEvidenceTrustBucketV1::Medium) => {
            DEFAULT_TRUST
        }
        SessionFactTrust::Bucket(MemoryAutomationFactEvidenceTrustBucketV1::High) => {
            HIGH_TRUST_REPRESENTATIVE
        }
    }
}

fn canonical_evidence_trust(value: &SessionFactTrust, trust: f64) -> Value {
    match value {
        SessionFactTrust::Numeric(_) => json!(trust),
        SessionFactTrust::Bucket(bucket) => json!(bucket),
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

fn normalized_fact_terms(terms: &[String]) -> Option<Vec<String>> {
    if terms.len() > 20 {
        return None;
    }
    let mut values = Vec::new();
    for term in terms {
        let value = normalized_non_empty(term)?;
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
