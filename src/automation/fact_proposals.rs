use std::path::{Path, PathBuf};

use libsql::Connection;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::errors::{Result, TraceDecayError};
use crate::memory::store::MemoryStore;
use crate::memory::trust::DEFAULT_TRUST;
use crate::memory::types::{AddFactOutcome, AddFactRequest};
use crate::tracedecay::current_timestamp;

const FACT_PROPOSALS_FILENAME: &str = "fact_proposals.json";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FactProposalState {
    PendingApproval,
    Applied,
    Rejected,
}

impl FactProposalState {
    pub fn parse(value: &str) -> Result<Self> {
        match value.trim().replace('-', "_").as_str() {
            "pending" | "pending_approval" => Ok(Self::PendingApproval),
            "applied" => Ok(Self::Applied),
            "rejected" | "rejected_validation" => Ok(Self::Rejected),
            other => Err(config_error(format!(
                "unknown fact proposal state '{other}'; expected pending_approval, applied, or rejected"
            ))),
        }
    }
}

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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub applied_fact_id: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub apply_outcome: Option<AddFactOutcome>,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FactProposalStore {
    pub schema_version: u32,
    #[serde(default)]
    pub proposals: Vec<FactProposalRecord>,
}

impl Default for FactProposalStore {
    fn default() -> Self {
        Self {
            schema_version: 1,
            proposals: Vec::new(),
        }
    }
}

pub fn fact_proposals_path(dashboard_root: &Path) -> PathBuf {
    dashboard_root.join(FACT_PROPOSALS_FILENAME)
}

pub async fn load_fact_proposal_store(dashboard_root: &Path) -> Result<FactProposalStore> {
    let path = fact_proposals_path(dashboard_root);
    let bytes = match tokio::fs::read(&path).await {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Ok(FactProposalStore::default());
        }
        Err(e) => {
            return Err(config_error(format!(
                "failed to read fact proposal store '{}': {e}",
                path.display()
            )));
        }
    };
    serde_json::from_slice(&bytes).map_err(|e| {
        config_error(format!(
            "failed to parse fact proposal store '{}': {e}",
            path.display()
        ))
    })
}

pub async fn save_fact_proposal_store(
    dashboard_root: &Path,
    store: &FactProposalStore,
) -> Result<()> {
    let path = fact_proposals_path(dashboard_root);
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent).await.map_err(|e| {
            config_error(format!(
                "failed to create fact proposal directory '{}': {e}",
                parent.display()
            ))
        })?;
    }
    let bytes = serde_json::to_vec_pretty(store).map_err(TraceDecayError::from)?;
    tokio::fs::write(&path, bytes).await.map_err(|e| {
        config_error(format!(
            "failed to write fact proposal store '{}': {e}",
            path.display()
        ))
    })
}

pub async fn list_fact_proposals(
    dashboard_root: &Path,
    state: Option<FactProposalState>,
    limit: usize,
) -> Result<Vec<FactProposalRecord>> {
    let mut proposals = load_fact_proposal_store(dashboard_root).await?.proposals;
    if let Some(state) = state {
        proposals.retain(|proposal| proposal.state == state);
    }
    proposals.sort_by_key(|proposal| std::cmp::Reverse(proposal.created_at));
    proposals.truncate(limit);
    Ok(proposals)
}

pub async fn load_fact_proposal(
    dashboard_root: &Path,
    proposal_id: &str,
) -> Result<Option<FactProposalRecord>> {
    Ok(load_fact_proposal_store(dashboard_root)
        .await?
        .proposals
        .into_iter()
        .find(|proposal| proposal.proposal_id == proposal_id))
}

pub async fn record_session_fact_proposals(
    dashboard_root: &Path,
    run_id: &str,
    evidence_hash: Option<&str>,
    accepted_facts: &[Value],
    rejected_facts: &[Value],
) -> Result<Vec<FactProposalRecord>> {
    let mut store = load_fact_proposal_store(dashboard_root).await?;
    let mut records = Vec::new();
    let now = current_timestamp();
    for (index, value) in accepted_facts.iter().enumerate() {
        let add_fact_request = value
            .get("add_fact_request")
            .cloned()
            .ok_or_else(|| config_error("accepted fact proposal missing add_fact_request"))?;
        let add_fact_request = serde_json::from_value::<AddFactRequest>(add_fact_request)
            .map_err(|e| config_error(format!("invalid accepted fact add_fact_request: {e}")))?;
        if pending_add_fact_request_exists(&store, &add_fact_request) {
            continue;
        }
        let proposal = value.get("proposal").cloned();
        let validation = value.get("validation").cloned();
        let record = FactProposalRecord {
            schema_version: 1,
            proposal_id: proposal_id(run_id, index, value),
            run_id: run_id.to_string(),
            evidence_hash: evidence_hash.map(ToOwned::to_owned),
            state: FactProposalState::PendingApproval,
            add_fact_request: Some(add_fact_request),
            proposal,
            validation_reason: None,
            validation,
            reviewer: None,
            applied_fact_id: None,
            apply_outcome: None,
            created_at: now,
            updated_at: now,
        };
        records.push(record.clone());
        store.proposals.push(record);
    }
    for (index, value) in rejected_facts.iter().enumerate() {
        let record = FactProposalRecord {
            schema_version: 1,
            proposal_id: proposal_id(run_id, accepted_facts.len() + index, value),
            run_id: run_id.to_string(),
            evidence_hash: evidence_hash.map(ToOwned::to_owned),
            state: FactProposalState::Rejected,
            add_fact_request: None,
            proposal: value.get("proposal").cloned(),
            validation_reason: value
                .get("reason")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned),
            validation: value.get("validation").cloned(),
            reviewer: Some("validator".to_string()),
            applied_fact_id: None,
            apply_outcome: None,
            created_at: now,
            updated_at: now,
        };
        records.push(record.clone());
        store.proposals.push(record);
    }
    save_fact_proposal_store(dashboard_root, &store).await?;
    Ok(records)
}

fn pending_add_fact_request_exists(store: &FactProposalStore, request: &AddFactRequest) -> bool {
    let content = normalize_fact_content(&request.content);
    store.proposals.iter().any(|proposal| {
        proposal.state == FactProposalState::PendingApproval
            && proposal.add_fact_request.as_ref().is_some_and(|existing| {
                existing.category == request.category
                    && normalize_fact_content(&existing.content) == content
            })
    })
}

fn normalize_fact_content(content: &str) -> String {
    content.split_whitespace().collect::<Vec<_>>().join(" ")
}

pub async fn apply_fact_proposal(
    dashboard_root: &Path,
    conn: &Connection,
    proposal_id: &str,
    reviewer: Option<String>,
) -> Result<FactProposalRecord> {
    let mut store = load_fact_proposal_store(dashboard_root).await?;
    let record = store
        .proposals
        .iter_mut()
        .find(|proposal| proposal.proposal_id == proposal_id)
        .ok_or_else(|| config_error(format!("fact proposal '{proposal_id}' not found")))?;
    if record.state != FactProposalState::PendingApproval {
        return Err(config_error(format!(
            "fact proposal '{proposal_id}' is not pending approval"
        )));
    }
    let Some(request) = record.add_fact_request.clone() else {
        return Err(config_error(format!(
            "fact proposal '{proposal_id}' has no add_fact_request"
        )));
    };
    let outcome = MemoryStore::new(conn)
        .add_fact(request, DEFAULT_TRUST)
        .await?;
    record.updated_at = current_timestamp();
    record.reviewer = reviewer;
    record.applied_fact_id = outcome.fact.as_ref().map(|fact| fact.fact_id);
    record.apply_outcome = Some(outcome.clone());
    if outcome.fact.is_some() {
        record.state = FactProposalState::Applied;
    } else {
        record.state = FactProposalState::Rejected;
        record.validation_reason.clone_from(&outcome.diff.reason);
    }
    let updated = record.clone();
    save_fact_proposal_store(dashboard_root, &store).await?;
    Ok(updated)
}

pub async fn reject_fact_proposal(
    dashboard_root: &Path,
    proposal_id: &str,
    reviewer: Option<String>,
    reason: Option<String>,
) -> Result<FactProposalRecord> {
    let mut store = load_fact_proposal_store(dashboard_root).await?;
    let record = store
        .proposals
        .iter_mut()
        .find(|proposal| proposal.proposal_id == proposal_id)
        .ok_or_else(|| config_error(format!("fact proposal '{proposal_id}' not found")))?;
    if record.state != FactProposalState::PendingApproval {
        return Err(config_error(format!(
            "fact proposal '{proposal_id}' is not pending approval"
        )));
    }
    record.state = FactProposalState::Rejected;
    record.reviewer = reviewer;
    record.validation_reason = reason;
    record.updated_at = current_timestamp();
    let updated = record.clone();
    save_fact_proposal_store(dashboard_root, &store).await?;
    Ok(updated)
}

fn proposal_id(run_id: &str, index: usize, value: &Value) -> String {
    let mut hasher = Sha256::new();
    hasher.update(run_id.as_bytes());
    hasher.update(b"\0");
    hasher.update(index.to_string().as_bytes());
    hasher.update(b"\0");
    hasher.update(value.to_string().as_bytes());
    let digest = hex::encode(hasher.finalize());
    format!("fact_{}", &digest[..16])
}

fn config_error(message: impl Into<String>) -> TraceDecayError {
    TraceDecayError::Config {
        message: message.into(),
    }
}
