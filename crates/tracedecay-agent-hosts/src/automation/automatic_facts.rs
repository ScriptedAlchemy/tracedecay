//! Authority-backed automatic fact apply receipts.
//!
//! Candidate discovery and validation belong to the automation run receipt.
//! This module records and reads only terminal applied or quarantined effects.

use std::collections::HashSet;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use tracedecay_domain::{ActorId, ProvenanceId};
use tracedecay_store::{
    MAX_PROJECT_MEMORY_AUTOMATIC_FACT_RECEIPTS, ProjectMemoryAutomaticFactEvidenceV1,
    ProjectMemoryAutomaticFactReceiptV1, ProjectMemoryAutomaticFactStateV1, ProjectMemoryFactStore,
};

use super::config_error;
use crate::application::memory::{
    MemoryApplication, automatic_fact_add_command, memory_application_error,
};
use crate::errors::{Result, TraceDecayError};
use crate::memory::types::{AddFactRequest, MemoryCategory};
use crate::privacy::sanitize_provider_metadata_text;

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
    pub add_fact_request: AddFactRequest,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub item: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub validation: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quarantine_reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub applied_canonical_fact_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub applied_fact_id: Option<i64>,
    pub recorded_at: i64,
}

pub struct AutomaticFactApplyBatch {
    pub receipts: Vec<AutomaticFactReceipt>,
    pub retry_error: Option<TraceDecayError>,
}

pub async fn record_session_automatic_facts<A: ProjectMemoryFactStore>(
    memory: &MemoryApplication<A>,
    run_id: &str,
    evidence_hash: Option<&str>,
    admitted_facts: &[Value],
) -> Result<AutomaticFactApplyBatch> {
    let mut receipts = Vec::with_capacity(admitted_facts.len());
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
                serde_json::from_value::<AddFactRequest>(request).map_err(|error| {
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
        let result = match memory
            .apply_project_memory_automatic_fact(authoritative_id, command, evidence)
            .await
        {
            Ok(result) => result,
            Err(error) => {
                return Ok(AutomaticFactApplyBatch {
                    receipts,
                    retry_error: Some(memory_application_error(error)),
                });
            }
        };
        let receipt = automatic_fact_receipt(result.receipt())?;
        if apply_ids.insert(receipt.apply_id.clone()) {
            receipts.push(receipt);
        }
    }

    Ok(AutomaticFactApplyBatch {
        receipts,
        retry_error: None,
    })
}

pub async fn list_automatic_fact_receipts<A: ProjectMemoryFactStore>(
    memory: &MemoryApplication<A>,
    state: Option<AutomaticFactState>,
    limit: usize,
) -> Result<Vec<AutomaticFactReceipt>> {
    if limit == 0 {
        return Ok(Vec::new());
    }
    let page = memory
        .list_project_memory_automatic_fact_receipts(
            state.map(authority_state),
            None,
            limit.min(MAX_PROJECT_MEMORY_AUTOMATIC_FACT_RECEIPTS),
        )
        .await
        .map_err(memory_application_error)?;
    page.receipts().iter().map(automatic_fact_receipt).collect()
}

pub async fn load_automatic_fact_receipt<A: ProjectMemoryFactStore>(
    memory: &MemoryApplication<A>,
    apply_id: &str,
) -> Result<Option<AutomaticFactReceipt>> {
    let apply_id = ProvenanceId::new(apply_id.to_string()).map_err(store_error)?;
    let receipt = memory
        .get_project_memory_automatic_fact_receipt(apply_id)
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
        applied_canonical_fact_id: receipt
            .applied_fact_id()
            .map(|fact_id| fact_id.as_str().to_string()),
        applied_fact_id: receipt
            .applied_mapping()
            .and_then(|mapping| mapping.legacy_fact_id()),
        recorded_at: receipt.recorded_at().0.div_euclid(1_000_000),
    })
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
