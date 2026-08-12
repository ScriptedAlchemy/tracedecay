//! Authority-backed automatic fact apply receipts.
//!
//! Candidate discovery and validation belong to the automation run receipt.
//! This module records and reads only terminal applied or quarantined effects.
//! It also recognizes the independently shipped v1 proposal sidecar for a
//! explicit retirement boundary. This crate only classifies the exact shipped
//! bytes. The daemon journals terminal-history retirement before archive and
//! removal; unresolved records are never approved or imported.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use tracedecay_domain::{ActorId, Confidence, FactCategoryV1, ProvenanceId, RunId};
use tracedecay_store::{
    FactReadControl, MAX_PROJECT_MEMORY_AUTOMATIC_FACT_RECEIPTS,
    ProjectMemoryAutomaticFactApplyResultV1, ProjectMemoryAutomaticFactEvidenceV1,
    ProjectMemoryAutomaticFactReceiptV1, ProjectMemoryAutomaticFactStateV1, ProjectMemoryFactStore,
};

use super::{config_error, lifecycle::AutomationRunControl};
use crate::application::memory::{
    MemoryApplication, automatic_fact_add_command, memory_application_error,
};
use crate::errors::{Result, TraceDecayError};
use crate::privacy::sanitize_provider_metadata_text;
use tracedecay_usecases::memory::{MemoryMutationError, ProjectMemoryFactAddRequest};

const SHIPPED_FACT_PROPOSALS_FILENAME: &str = "fact_proposals.json";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
enum ShippedFactProposalStateV1 {
    PendingApproval,
    Applied,
    Rejected,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
struct ShippedAddFactRequestV1 {
    content: String,
    category: FactCategoryV1,
    #[serde(rename = "source", alias = "source_label")]
    source_label: Option<String>,
    tags: Vec<String>,
    entities: Vec<String>,
    trust: Option<Confidence>,
    metadata: Value,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
struct ShippedFactProposalRecordV1 {
    schema_version: u32,
    proposal_id: String,
    run_id: String,
    #[serde(default)]
    evidence_hash: Option<String>,
    state: ShippedFactProposalStateV1,
    #[serde(default)]
    add_fact_request: Option<ShippedAddFactRequestV1>,
    #[serde(default)]
    proposal: Option<Value>,
    #[serde(default)]
    validation_reason: Option<String>,
    #[serde(default)]
    validation: Option<Value>,
    #[serde(default)]
    reviewer: Option<String>,
    #[serde(default)]
    applied_fact_id: Option<i64>,
    #[serde(default)]
    apply_outcome: Option<Value>,
    created_at: i64,
    updated_at: i64,
    #[serde(default)]
    duplicate_count: u32,
    #[serde(default)]
    last_duplicate_run_id: Option<String>,
    #[serde(default)]
    folded_contents: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
struct ShippedFactProposalStoreV1 {
    schema_version: u32,
    #[serde(default)]
    proposals: Vec<ShippedFactProposalRecordV1>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AutomaticFactState {
    Applied,
    Quarantined,
}

impl AutomaticFactState {
    pub fn parse(value: &str) -> Result<Self> {
        match value.trim().replace('-', "_").as_str() {
            "applied" => Ok(Self::Applied),
            "quarantined" => Ok(Self::Quarantined),
            other => Err(config_error(format!(
                "unknown automatic fact state '{other}'; expected applied or quarantined"
            ))),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AutomaticFactReceipt {
    pub schema_version: u32,
    pub apply_id: String,
    pub run_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub evidence_hash: Option<String>,
    pub state: AutomaticFactState,
    pub add_fact_request: ProjectMemoryFactAddRequest,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub item: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub validation: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quarantine_reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub applied_fact_id: Option<String>,
    pub recorded_at_micros: i64,
}

pub struct AutomaticFactApplyBatch {
    pub receipts: Vec<AutomaticFactReceipt>,
    pub retry_error: Option<TraceDecayError>,
    pub(crate) settled_receipts: Vec<SettledAutomaticFactReceipt>,
}

#[derive(Debug, Clone)]
pub(crate) enum SettledAutomaticFactReceipt {
    Projected {
        receipt: AutomaticFactReceipt,
        authority_result: ProjectMemoryAutomaticFactApplyResultV1,
    },
    InvalidAuthority(ProjectMemoryAutomaticFactApplyResultV1),
}

enum AutomaticFactApplySettlement {
    Terminal {
        receipt: SettledAutomaticFactReceipt,
        validation_error: Option<TraceDecayError>,
    },
    ApplicationError(TraceDecayError),
}

pub async fn record_session_automatic_facts<A: ProjectMemoryFactStore>(
    memory: &MemoryApplication<A>,
    run_control: &AutomationRunControl,
    run_id: &str,
    evidence_hash: Option<&str>,
    admitted_facts: &[Value],
) -> Result<AutomaticFactApplyBatch> {
    let outer_run_id = RunId::new(run_id.to_owned()).map_err(store_error)?;
    let mut receipts = Vec::with_capacity(admitted_facts.len());
    let mut settled_receipts = Vec::with_capacity(admitted_facts.len());
    let evidence_hash = bounded_metadata_text(evidence_hash, 160);
    let actor = automatic_fact_actor("automation:session-reflector")?;
    let mut semantic_keys = HashSet::new();
    let mut apply_ids = HashSet::new();

    for (index, value) in admitted_facts.iter().enumerate() {
        let apply_id = automatic_fact_apply_id(run_id, index, value);
        let request = value
            .get("add_fact_request")
            .cloned()
            .ok_or_else(|| config_error("admitted automatic fact is missing add_fact_request"))
            .and_then(|request| {
                serde_json::from_value::<ProjectMemoryFactAddRequest>(request).map_err(|error| {
                    config_error(format!("invalid admitted automatic fact request: {error}"))
                })
            })?;
        let command = automatic_fact_add_command(
            memory.owner().clone(),
            request,
            run_id,
            &apply_id,
            Some(actor.clone()),
        )
        .map_err(memory_application_error)?;
        if command.automation_run_id() != Some(outer_run_id.as_str()) {
            return Err(config_error(
                "automatic fact command is not bound to the admitted outer run",
            ));
        }
        let semantic_key = (
            command.category(),
            normalize_fact_content(command.content()),
        );
        if !semantic_keys.insert(semantic_key) {
            continue;
        }
        let authoritative_id = ProvenanceId::new(apply_id).map_err(store_error)?;
        let evidence = ProjectMemoryAutomaticFactEvidenceV1::new(
            evidence_hash.clone(),
            value.get("item").cloned(),
            value.get("validation").cloned(),
        )
        .map_err(store_error)?;
        let write_control = run_control.write_control();
        let settlement = automatic_fact_apply_settlement(
            memory
                .apply_project_memory_automatic_fact(
                    authoritative_id,
                    command,
                    evidence,
                    &write_control,
                )
                .await,
        )?;
        let (receipt, validation_error) = match settlement {
            AutomaticFactApplySettlement::Terminal {
                receipt,
                validation_error,
            } => (receipt, validation_error),
            AutomaticFactApplySettlement::ApplicationError(error) => {
                return Ok(AutomaticFactApplyBatch {
                    receipts,
                    retry_error: Some(error),
                    settled_receipts,
                });
            }
        };
        if apply_ids.insert(receipt.apply_id().to_owned()) {
            if let Some(projected) = receipt.projected() {
                receipts.push(projected.clone());
            }
            settled_receipts.push(receipt);
        }
        if validation_error.is_some() {
            return Ok(AutomaticFactApplyBatch {
                receipts,
                retry_error: validation_error,
                settled_receipts,
            });
        }
    }

    Ok(AutomaticFactApplyBatch {
        receipts,
        retry_error: None,
        settled_receipts,
    })
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ShippedFactProposalDisposition {
    Absent,
    TerminalHistory {
        source_path: PathBuf,
        source_digest: String,
        source_bytes: Vec<u8>,
    },
    ResetRequired {
        source_path: PathBuf,
        source_digest: String,
        reason: String,
    },
}

pub async fn inspect_shipped_fact_proposals(
    dashboard_root: &Path,
) -> Result<ShippedFactProposalDisposition> {
    let source_path = dashboard_root.join(SHIPPED_FACT_PROPOSALS_FILENAME);
    let bytes = match tokio::fs::read(&source_path).await {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(ShippedFactProposalDisposition::Absent);
        }
        Err(error) => {
            return Err(config_error(format!(
                "failed to read shipped fact proposal sidecar '{}': {error}",
                source_path.display()
            )));
        }
    };
    let source_digest = format!("sha256:{}", hex::encode(Sha256::digest(&bytes)));
    let source_metadata = tokio::fs::symlink_metadata(&source_path)
        .await
        .map_err(|error| {
            config_error(format!(
                "failed to inspect shipped fact proposal sidecar '{}': {error}",
                source_path.display()
            ))
        })?;
    if !source_metadata.file_type().is_file() {
        return Ok(shipped_fact_proposal_reset_required(
            source_path,
            source_digest,
            "the shipped v1 sidecar is not a regular file",
        ));
    }
    let store = match serde_json::from_slice::<ShippedFactProposalStoreV1>(&bytes) {
        Ok(store) => store,
        Err(error) => {
            return Ok(shipped_fact_proposal_reset_required(
                source_path,
                source_digest,
                format!("the shipped v1 JSON is malformed: {error}"),
            ));
        }
    };
    if store.schema_version != 1 {
        return Ok(shipped_fact_proposal_reset_required(
            source_path,
            source_digest,
            format!(
                "root schema version {} is not the shipped version 1",
                store.schema_version
            ),
        ));
    }
    if let Some(record) = store
        .proposals
        .iter()
        .find(|record| record.schema_version != 1)
    {
        return Ok(shipped_fact_proposal_reset_required(
            source_path,
            source_digest,
            format!(
                "proposal '{}' has unsupported schema version {}",
                record.proposal_id, record.schema_version
            ),
        ));
    }

    let mut proposal_ids = HashSet::new();
    for record in &store.proposals {
        if !proposal_ids.insert(record.proposal_id.as_str()) {
            return Ok(shipped_fact_proposal_reset_required(
                source_path,
                source_digest,
                format!(
                    "proposal identity '{}' occurs more than once",
                    record.proposal_id
                ),
            ));
        }
        if record.state == ShippedFactProposalStateV1::PendingApproval {
            return Ok(shipped_fact_proposal_reset_required(
                source_path,
                source_digest,
                format!(
                    "unresolved proposal '{}' cannot be imported because final-V2 has no fact approval authority",
                    record.proposal_id
                ),
            ));
        }
    }
    Ok(ShippedFactProposalDisposition::TerminalHistory {
        source_path,
        source_digest,
        source_bytes: bytes,
    })
}

fn shipped_fact_proposal_reset_required(
    source_path: PathBuf,
    source_digest: String,
    reason: impl Into<String>,
) -> ShippedFactProposalDisposition {
    ShippedFactProposalDisposition::ResetRequired {
        source_path,
        source_digest,
        reason: reason.into(),
    }
}

pub async fn list_automatic_fact_receipts<A: ProjectMemoryFactStore>(
    memory: &MemoryApplication<A>,
    state: Option<AutomaticFactState>,
    limit: usize,
    read_control: &FactReadControl,
) -> Result<Vec<AutomaticFactReceipt>> {
    if limit == 0 {
        return Ok(Vec::new());
    }
    let page = memory
        .list_project_memory_automatic_fact_receipts(
            state.map(authority_state),
            None,
            limit.min(MAX_PROJECT_MEMORY_AUTOMATIC_FACT_RECEIPTS),
            read_control,
        )
        .await
        .map_err(memory_application_error)?;
    page.receipts().iter().map(automatic_fact_receipt).collect()
}

pub async fn load_automatic_fact_receipt<A: ProjectMemoryFactStore>(
    memory: &MemoryApplication<A>,
    apply_id: &str,
    read_control: &FactReadControl,
) -> Result<Option<AutomaticFactReceipt>> {
    let apply_id = ProvenanceId::new(apply_id.to_string()).map_err(store_error)?;
    let receipt = memory
        .get_project_memory_automatic_fact_receipt(apply_id, read_control)
        .await
        .map_err(memory_application_error)?;
    receipt.as_ref().map(automatic_fact_receipt).transpose()
}

fn automatic_fact_receipt(
    receipt: &ProjectMemoryAutomaticFactReceiptV1,
) -> Result<AutomaticFactReceipt> {
    let run_id = receipt.automation_run_id().ok_or_else(|| {
        config_error(format!(
            "automatic fact receipt '{}' is missing its automation run identity",
            receipt.apply_id()
        ))
    })?;
    Ok(AutomaticFactReceipt {
        schema_version: 1,
        apply_id: receipt.apply_id().as_str().to_string(),
        run_id: run_id.to_string(),
        evidence_hash: receipt.evidence().evidence_hash().map(ToOwned::to_owned),
        state: display_state(receipt.state()),
        add_fact_request: add_request_from_command(receipt.request()),
        item: receipt.evidence().item().cloned(),
        validation: receipt.evidence().validation().cloned(),
        quarantine_reason: receipt.quarantine_reason().map(ToOwned::to_owned),
        applied_fact_id: receipt
            .applied_fact_id()
            .map(|fact_id| fact_id.as_str().to_string()),
        recorded_at_micros: receipt.recorded_at().0,
    })
}

fn automatic_fact_apply_settlement(
    settlement: std::result::Result<
        ProjectMemoryAutomaticFactApplyResultV1,
        MemoryMutationError<ProjectMemoryAutomaticFactApplyResultV1>,
    >,
) -> Result<AutomaticFactApplySettlement> {
    let (authority_result, validation_error, invalid_authority) = match settlement {
        Ok(authority_result) => (authority_result, None, false),
        Err(MemoryMutationError::Application(error)) => {
            return Ok(AutomaticFactApplySettlement::ApplicationError(
                memory_application_error(error),
            ));
        }
        Err(MemoryMutationError::InvalidAuthorityResult {
            error,
            authority_result,
        }) => (
            authority_result,
            Some(memory_application_error(error)),
            true,
        ),
    };
    let receipt = match automatic_fact_receipt(authority_result.receipt()) {
        Ok(receipt) => SettledAutomaticFactReceipt::Projected {
            receipt,
            authority_result,
        },
        Err(_) if invalid_authority => {
            SettledAutomaticFactReceipt::InvalidAuthority(authority_result)
        }
        Err(error) => return Err(error),
    };
    Ok(AutomaticFactApplySettlement::Terminal {
        receipt,
        validation_error,
    })
}

impl SettledAutomaticFactReceipt {
    pub(crate) fn into_authority_result(self) -> ProjectMemoryAutomaticFactApplyResultV1 {
        match self {
            Self::Projected {
                authority_result, ..
            }
            | Self::InvalidAuthority(authority_result) => authority_result,
        }
    }

    pub(crate) fn projected(&self) -> Option<&AutomaticFactReceipt> {
        match self {
            Self::Projected { receipt, .. } => Some(receipt),
            Self::InvalidAuthority(_) => None,
        }
    }

    fn authority_result(&self) -> &ProjectMemoryAutomaticFactApplyResultV1 {
        match self {
            Self::Projected {
                authority_result, ..
            }
            | Self::InvalidAuthority(authority_result) => authority_result,
        }
    }

    pub(crate) fn apply_id(&self) -> &str {
        self.authority_result().receipt().apply_id().as_str()
    }

    pub(crate) fn state(&self) -> AutomaticFactState {
        display_state(self.authority_result().receipt().state())
    }

    pub(crate) fn applied_fact_id(&self) -> Option<&str> {
        self.authority_result()
            .receipt()
            .applied_fact_id()
            .map(tracedecay_domain::FactId::as_str)
    }

    #[cfg(test)]
    pub(crate) fn ledger_value(&self) -> Value {
        let result = self.authority_result();
        let receipt = result.receipt();
        let request = receipt.request();
        let applied_target = receipt.applied_target().map(|target| {
            serde_json::json!({
                "owner": target.owner(),
                "fact_id": target.fact_id(),
            })
        });
        let disposition = match result.disposition() {
            tracedecay_store::ProjectMemoryAutomaticFactApplyDispositionV1::Applied => "applied",
            tracedecay_store::ProjectMemoryAutomaticFactApplyDispositionV1::AlreadyApplied => {
                "already_applied"
            }
            tracedecay_store::ProjectMemoryAutomaticFactApplyDispositionV1::Quarantined => {
                "quarantined"
            }
        };
        serde_json::json!({
            "schema_version": 1,
            "apply_id": receipt.apply_id(),
            "owner": receipt.owner(),
            "run_id": receipt.automation_run_id(),
            "state": display_state(receipt.state()),
            "disposition": disposition,
            "request": {
                "operation_id": request.operation_id(),
                "input_digest": request.input_digest(),
                "automation_run_id": request.automation_run_id(),
                "actor": request.actor(),
                "add_fact_request": add_request_from_command(request),
                "sanitization_receipt": request.sanitization_receipt(),
            },
            "evidence": receipt.evidence(),
            "effect": {
                "applied_fact_id": receipt.applied_fact_id(),
                "applied_target": applied_target,
                "applied_assertion_id": receipt.applied_assertion_id(),
                "applied_event_id": receipt.applied_event_id(),
                "quarantine_reason": receipt.quarantine_reason(),
            },
            "recorded_at_micros": receipt.recorded_at().0,
        })
    }
}

fn add_request_from_command(
    command: &tracedecay_store::ProjectMemoryFactAddCommandV1,
) -> ProjectMemoryFactAddRequest {
    ProjectMemoryFactAddRequest {
        content: command.content().to_string(),
        category: command.category(),
        source_label: command.source_label().map(ToOwned::to_owned),
        tags: command.tags().to_vec(),
        entities: command.entities().to_vec(),
        trust: Some(command.default_trust()),
        metadata: command.metadata().clone(),
    }
}

const fn authority_state(state: AutomaticFactState) -> ProjectMemoryAutomaticFactStateV1 {
    match state {
        AutomaticFactState::Applied => ProjectMemoryAutomaticFactStateV1::Applied,
        AutomaticFactState::Quarantined => ProjectMemoryAutomaticFactStateV1::Quarantined,
    }
}

const fn display_state(state: ProjectMemoryAutomaticFactStateV1) -> AutomaticFactState {
    match state {
        ProjectMemoryAutomaticFactStateV1::Applied => AutomaticFactState::Applied,
        ProjectMemoryAutomaticFactStateV1::Quarantined => AutomaticFactState::Quarantined,
    }
}

fn automatic_fact_actor(value: &str) -> Result<ActorId> {
    ActorId::new(value.to_string()).map_err(store_error)
}

fn bounded_metadata_text(value: Option<&str>, maximum: usize) -> Option<String> {
    let value = value?.trim();
    if value.is_empty() || value.len() > maximum || value.chars().any(char::is_control) {
        return None;
    }
    sanitize_provider_metadata_text(value)
        .filter(|sanitized| !sanitized.trim().is_empty() && sanitized.len() <= maximum)
}

fn automatic_fact_apply_id(run_id: &str, index: usize, value: &Value) -> String {
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
    content
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

fn store_error(error: impl std::fmt::Display) -> TraceDecayError {
    config_error(format!("automatic fact contract is invalid: {error}"))
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
#[path = "automatic_facts_test.rs"]
mod automatic_facts_test;
