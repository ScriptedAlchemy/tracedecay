//! Authority-backed automation fact proposals.
//!
//! Proposal state, CAS, applied facts, and presentation metadata (evidence
//! payloads, timestamps) all come from [`MemoryApplication`]; there is no
//! sidecar projection store.

use std::collections::HashSet;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use tracedecay_domain::{ActorId, ProvenanceId};
use tracedecay_store::{
    ProjectMemoryFactProposalEvidenceV1, ProjectMemoryFactProposalPromotionDispositionV1,
    ProjectMemoryFactProposalPromotionV1, ProjectMemoryFactProposalRecordV1,
    ProjectMemoryFactProposalStateV1, ProjectMemoryFactStore,
};

use super::config_error;
use crate::application::memory::{
    MemoryApplication, MemoryApplicationError, automation_fact_proposal_add_command,
};
use crate::errors::{Result, TraceDecayError};
use crate::memory::types::{AddFactRequest, MemoryCategory};
use crate::privacy::sanitize_provider_metadata_text;
use crate::tracedecay::current_timestamp;

const MAX_FACT_PROPOSAL_PAGE_SIZE: usize = 1_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FactProposalState {
    PendingApproval,
    /// Display-only input state. The durable fact authority never persists an
    /// applying state.
    Applying,
    Applied,
    Rejected,
    Quarantined,
}

impl FactProposalState {
    pub fn parse(value: &str) -> Result<Self> {
        match value.trim().replace('-', "_").as_str() {
            "pending" | "pending_approval" => Ok(Self::PendingApproval),
            "applying" => Ok(Self::Applying),
            "applied" => Ok(Self::Applied),
            "rejected" | "rejected_validation" => Ok(Self::Rejected),
            "quarantined" => Ok(Self::Quarantined),
            other => Err(config_error(format!(
                "unknown fact proposal state '{other}'; expected pending_approval, applying, applied, rejected, or quarantined"
            ))),
        }
    }
}

/// Display shape retained for dashboard and run-ledger JSON. It is rendered
/// from the authoritative proposal record on every read.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FactProposalRecord {
    pub schema_version: u32,
    pub proposal_id: String,
    pub run_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub evidence_hash: Option<String>,
    pub state: FactProposalState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub add_fact_request: Option<AddFactRequest>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proposal: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub validation_reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub validation: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reviewer: Option<String>,
    /// Canonical durable fact identity. Never coerce this into a numeric mapping.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub applied_canonical_fact_id: Option<String>,
    /// Legacy numeric mapping, populated only when the authority has one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub applied_fact_id: Option<i64>,
    pub created_at: i64,
    pub updated_at: i64,
    #[serde(default, skip_serializing_if = "crate::serde_util::is_default")]
    pub duplicate_count: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_duplicate_run_id: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub folded_contents: Vec<String>,
}

pub async fn record_session_fact_proposals<A: ProjectMemoryFactStore>(
    memory: &MemoryApplication<A>,
    run_id: &str,
    evidence_hash: Option<&str>,
    accepted_facts: &[Value],
    rejected_facts: &[Value],
) -> Result<Vec<FactProposalRecord>> {
    let mut records = Vec::with_capacity(accepted_facts.len() + rejected_facts.len());
    let observed_at = current_timestamp();
    let evidence_hash = bounded_metadata_text(evidence_hash, 160);
    let submitter = proposal_actor("automation:session-reflector")?;
    let mut submitted_semantic_keys = HashSet::new();
    let mut submitted_proposal_ids = HashSet::new();

    for (index, value) in accepted_facts.iter().enumerate() {
        let proposal_id = proposal_id(run_id, index, value);
        let request = value
            .get("add_fact_request")
            .cloned()
            .ok_or_else(|| config_error("accepted fact proposal missing add_fact_request"))
            .and_then(|request| {
                serde_json::from_value::<AddFactRequest>(request).map_err(|error| {
                    config_error(format!("invalid accepted fact add_fact_request: {error}"))
                })
            });
        let Ok(request) = request else {
            records.push(rejected_projection(
                &proposal_id,
                run_id,
                evidence_hash.as_deref(),
                FactProposalState::Quarantined,
                "automation proposal could not be reconstructed",
                observed_at,
            ));
            continue;
        };
        let Ok(command) = automation_fact_proposal_add_command(
            memory.owner().clone(),
            request,
            run_id,
            &proposal_id,
            Some(submitter.clone()),
        ) else {
            records.push(rejected_projection(
                &proposal_id,
                run_id,
                evidence_hash.as_deref(),
                FactProposalState::Quarantined,
                "automation proposal was rejected by memory privacy validation",
                observed_at,
            ));
            continue;
        };
        let semantic_key = (
            command.category(),
            normalize_fact_content(command.content()),
        );
        if !submitted_semantic_keys.insert(semantic_key) {
            // Preserve the exact-duplicate contract: different evidence
            // annotations for the same fact assertion are a partial no-op,
            // not a second proposal or promotion.
            continue;
        }
        let authoritative_id = ProvenanceId::new(proposal_id.clone()).map_err(store_error)?;
        let evidence = ProjectMemoryFactProposalEvidenceV1::new(
            evidence_hash.clone(),
            value.get("proposal").cloned(),
            value.get("validation").cloned(),
        )
        .map_err(store_error)?;
        let proposal = memory
            .submit_project_memory_fact_proposal(
                authoritative_id,
                command,
                Some(submitter.clone()),
                evidence,
            )
            .await
            .map_err(memory_error)?;
        if !submitted_proposal_ids.insert(proposal.proposal_id().as_str().to_string()) {
            // The authority collapsed this exact canonical command/digest into
            // an earlier proposal. Keep one display record so a duplicate
            // model item remains a partial no-op.
            continue;
        }
        records.push(record_from_authority(&proposal)?);
    }

    for (index, _) in rejected_facts.iter().enumerate() {
        records.push(rejected_projection(
            &proposal_id(run_id, accepted_facts.len() + index, &rejected_facts[index]),
            run_id,
            evidence_hash.as_deref(),
            FactProposalState::Rejected,
            "automation proposal was rejected before authority submission",
            observed_at,
        ));
    }
    Ok(records)
}

pub async fn list_fact_proposals<A: ProjectMemoryFactStore>(
    memory: &MemoryApplication<A>,
    state: Option<FactProposalState>,
    limit: usize,
) -> Result<Vec<FactProposalRecord>> {
    if limit == 0 || state == Some(FactProposalState::Applying) {
        return Ok(Vec::new());
    }
    let limit = limit.min(MAX_FACT_PROPOSAL_PAGE_SIZE);
    let page = memory
        .list_project_memory_fact_proposals(state.map(compatibility_state), None, limit)
        .await
        .map_err(memory_error)?;
    page.proposals().iter().map(record_from_authority).collect()
}

pub async fn load_fact_proposal<A: ProjectMemoryFactStore>(
    memory: &MemoryApplication<A>,
    proposal_id: &str,
) -> Result<Option<FactProposalRecord>> {
    let proposal_id = ProvenanceId::new(proposal_id.to_string()).map_err(store_error)?;
    let proposal = memory
        .get_project_memory_fact_proposal(proposal_id)
        .await
        .map_err(memory_error)?;
    proposal.as_ref().map(record_from_authority).transpose()
}

/// There is deliberately no authoritative `Applying` state.
pub async fn list_applying_fact_proposals<A: ProjectMemoryFactStore>(
    _memory: &MemoryApplication<A>,
) -> Result<Vec<FactProposalRecord>> {
    Ok(Vec::new())
}

pub async fn apply_fact_proposal<A: ProjectMemoryFactStore>(
    memory: &MemoryApplication<A>,
    proposal_id: &str,
    reviewer: Option<String>,
) -> Result<FactProposalRecord> {
    Ok(
        apply_fact_proposal_with_result(memory, proposal_id, reviewer)
            .await?
            .record,
    )
}

/// Authority-backed apply result. `newly_promoted` is an atomic store receipt,
/// not an inference from the final proposal state.
#[derive(Debug, Clone, PartialEq)]
pub struct FactProposalApplyResult {
    pub record: FactProposalRecord,
    pub newly_promoted: bool,
}

pub async fn apply_fact_proposal_with_result<A: ProjectMemoryFactStore>(
    memory: &MemoryApplication<A>,
    proposal_id: &str,
    reviewer: Option<String>,
) -> Result<FactProposalApplyResult> {
    let proposal_id = ProvenanceId::new(proposal_id.to_string()).map_err(store_error)?;
    let current = memory
        .get_project_memory_fact_proposal(proposal_id.clone())
        .await
        .map_err(memory_error)?
        .ok_or_else(|| config_error(format!("fact proposal '{proposal_id}' not found")))?;
    if current.state() == ProjectMemoryFactProposalStateV1::Applied {
        return Ok(FactProposalApplyResult {
            record: record_from_authority(&current)?,
            newly_promoted: false,
        });
    }
    if current.state() != ProjectMemoryFactProposalStateV1::PendingApproval {
        return Err(config_error(format!(
            "fact proposal '{proposal_id}' is not pending approval"
        )));
    }
    let reviewer_actor = proposal_reviewer(reviewer.as_deref())?;
    let request = ProjectMemoryFactProposalPromotionV1::new(
        memory.owner().clone(),
        proposal_id,
        current.revision(),
        Some(reviewer_actor),
    )
    .map_err(store_error)?;
    let promotion = memory
        .promote_project_memory_fact_proposal_with_disposition(request)
        .await
        .map_err(memory_error)?;
    let record = record_from_authority(promotion.proposal())?;
    Ok(FactProposalApplyResult {
        record,
        newly_promoted: matches!(
            promotion.disposition(),
            ProjectMemoryFactProposalPromotionDispositionV1::NewlyPromoted
        ),
    })
}

pub async fn reject_fact_proposal<A: ProjectMemoryFactStore>(
    memory: &MemoryApplication<A>,
    proposal_id: &str,
    reviewer: Option<String>,
    reason: Option<String>,
) -> Result<FactProposalRecord> {
    let proposal_id = ProvenanceId::new(proposal_id.to_string()).map_err(store_error)?;
    let current = memory
        .get_project_memory_fact_proposal(proposal_id.clone())
        .await
        .map_err(memory_error)?
        .ok_or_else(|| config_error(format!("fact proposal '{proposal_id}' not found")))?;
    if current.state() == ProjectMemoryFactProposalStateV1::Rejected {
        return record_from_authority(&current);
    }
    if current.state() != ProjectMemoryFactProposalStateV1::PendingApproval {
        return Err(config_error(format!(
            "fact proposal '{proposal_id}' is not pending approval"
        )));
    }
    let reviewer_actor = proposal_reviewer(reviewer.as_deref())?;
    let reason = sanitized_reason(reason);
    let proposal = memory
        .reject_project_memory_fact_proposal(
            proposal_id,
            current.revision(),
            reviewer_actor,
            reason,
        )
        .await
        .map_err(memory_error)?;
    record_from_authority(&proposal)
}

fn record_from_authority(
    proposal: &ProjectMemoryFactProposalRecordV1,
) -> Result<FactProposalRecord> {
    let run_id = proposal.automation_run_id().ok_or_else(|| {
        config_error(format!(
            "fact proposal '{}' is missing its automation run identity",
            proposal.proposal_id()
        ))
    })?;
    Ok(FactProposalRecord {
        schema_version: 2,
        proposal_id: proposal.proposal_id().as_str().to_string(),
        run_id: run_id.to_string(),
        evidence_hash: proposal.evidence().evidence_hash().map(ToOwned::to_owned),
        state: display_state(proposal.state()),
        add_fact_request: Some(add_request_from_command(proposal.request())),
        proposal: proposal.evidence().proposal().cloned(),
        validation_reason: proposal.reason().map(ToOwned::to_owned),
        validation: proposal.evidence().validation().cloned(),
        reviewer: proposal.reviewer().map(|actor| actor.as_str().to_string()),
        applied_canonical_fact_id: proposal
            .applied_fact_id()
            .map(|fact_id| fact_id.as_str().to_string()),
        applied_fact_id: proposal.legacy_fact_id(),
        created_at: proposal.submitted_at().0.div_euclid(1_000_000),
        updated_at: proposal.updated_at().0.div_euclid(1_000_000),
        duplicate_count: 0,
        last_duplicate_run_id: None,
        folded_contents: Vec::new(),
    })
}

fn rejected_projection(
    proposal_id: &str,
    run_id: &str,
    evidence_hash: Option<&str>,
    state: FactProposalState,
    reason: &str,
    observed_at: i64,
) -> FactProposalRecord {
    FactProposalRecord {
        schema_version: 2,
        proposal_id: proposal_id.to_string(),
        run_id: run_id.to_string(),
        evidence_hash: bounded_metadata_text(evidence_hash, 160),
        state,
        add_fact_request: None,
        proposal: None,
        validation_reason: Some(reason.to_string()),
        validation: None,
        reviewer: Some("automation:session-reflector".to_string()),
        applied_canonical_fact_id: None,
        applied_fact_id: None,
        created_at: observed_at,
        updated_at: observed_at,
        duplicate_count: 0,
        last_duplicate_run_id: None,
        folded_contents: Vec::new(),
    }
}

fn add_request_from_command(
    command: &tracedecay_store::ProjectMemoryFactAddCommandV1,
) -> AddFactRequest {
    AddFactRequest {
        content: command.content().to_string(),
        category: MemoryCategory::from(command.category()),
        source: command.source().map(ToOwned::to_owned),
        tags: command.tags().to_vec(),
        entities: command.entities().to_vec(),
        trust: Some(command.default_trust().as_f64()),
        metadata: command.metadata().clone(),
    }
}

const fn compatibility_state(state: FactProposalState) -> ProjectMemoryFactProposalStateV1 {
    match state {
        FactProposalState::PendingApproval | FactProposalState::Applying => {
            ProjectMemoryFactProposalStateV1::PendingApproval
        }
        FactProposalState::Applied => ProjectMemoryFactProposalStateV1::Applied,
        FactProposalState::Rejected => ProjectMemoryFactProposalStateV1::Rejected,
        FactProposalState::Quarantined => ProjectMemoryFactProposalStateV1::Quarantined,
    }
}

const fn display_state(state: ProjectMemoryFactProposalStateV1) -> FactProposalState {
    match state {
        ProjectMemoryFactProposalStateV1::PendingApproval
        | ProjectMemoryFactProposalStateV1::Applying => FactProposalState::PendingApproval,
        ProjectMemoryFactProposalStateV1::Applied => FactProposalState::Applied,
        ProjectMemoryFactProposalStateV1::Rejected => FactProposalState::Rejected,
        ProjectMemoryFactProposalStateV1::Quarantined => FactProposalState::Quarantined,
    }
}

fn proposal_actor(value: &str) -> Result<ActorId> {
    ActorId::new(value.to_string()).map_err(store_error)
}

/// The authority records the actual reviewer identity when it is a valid
/// bounded metadata string; the fixed automation actor is only the fallback.
fn proposal_reviewer(value: Option<&str>) -> Result<ActorId> {
    match bounded_metadata_text(value, 160) {
        Some(value) => ActorId::new(value).map_err(store_error),
        None => proposal_actor("automation:proposal-review"),
    }
}

fn sanitized_reason(reason: Option<String>) -> String {
    bounded_metadata_text(reason.as_deref(), 512)
        .unwrap_or_else(|| "rejected by reviewer".to_string())
}

fn bounded_metadata_text(value: Option<&str>, maximum: usize) -> Option<String> {
    let value = value?.trim();
    if value.is_empty() || value.len() > maximum || value.chars().any(char::is_control) {
        return None;
    }
    sanitize_provider_metadata_text(value)
        .filter(|sanitized| !sanitized.trim().is_empty() && sanitized.len() <= maximum)
}

fn proposal_id(run_id: &str, index: usize, value: &Value) -> String {
    let mut hasher = Sha256::new();
    let index = index.to_string();
    let value = value.to_string();
    for component in [run_id.as_bytes(), index.as_bytes(), value.as_bytes()] {
        hasher.update((component.len() as u64).to_be_bytes());
        hasher.update(component);
    }
    format!("fact_{}", &hex::encode(hasher.finalize())[..16])
}

fn normalize_fact_content(content: &str) -> String {
    content.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn memory_error(error: MemoryApplicationError) -> TraceDecayError {
    config_error(format!("fact proposal authority failed: {error}"))
}

fn store_error(error: impl std::fmt::Display) -> TraceDecayError {
    config_error(format!("fact proposal contract is invalid: {error}"))
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
#[path = "fact_proposals_test.rs"]
mod fact_proposals_test;
