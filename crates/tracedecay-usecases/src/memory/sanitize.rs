//! Request sanitizers for legacy V1 memory payloads.

use serde::Deserialize;
use serde_json::{Value, json};
use tracedecay_domain::{FactCategoryV1, SanitizationReceiptV1};
use tracedecay_runtime_core::memory::hygiene::detect_secret_like;
use tracedecay_runtime_core::memory::types::{AddFactRequest, UpdateFactRequest};
use tracedecay_runtime_core::privacy::{
    MemoryFactSanitizationV1, sanitize_memory_fact_payload, sanitize_provider_metadata_text,
};
use tracedecay_store::CompatibilityRelationProvenanceV1;

use super::error::MemoryApplicationError;

pub(super) struct SanitizedAddFactRequestV1 {
    request: AddFactRequest,
    receipt: SanitizationReceiptV1,
}

impl SanitizedAddFactRequestV1 {
    pub(super) fn into_parts(self) -> (AddFactRequest, SanitizationReceiptV1) {
        (self.request, self.receipt)
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SanitizedFactPayloadWireV1 {
    content: String,
    category: FactCategoryV1,
    tags: Vec<String>,
    entities: Vec<String>,
    metadata: Value,
}

pub(super) fn sanitize_add_fact_request(
    mut request: AddFactRequest,
) -> Result<Option<SanitizedAddFactRequestV1>, MemoryApplicationError> {
    strip_reserved_automation_run_id(&mut request.metadata);
    // The canonical payload sorts labels before hashing; the sanitizer receipt
    // is computed over this wire, so it must see the same canonical order.
    request.tags.sort_unstable();
    request.entities.sort_unstable();
    if detect_secret_like(request.content.trim()).is_some() {
        return Ok(None);
    }
    let Some(source) = sanitize_optional_memory_text(request.source.clone()) else {
        return Ok(None);
    };
    let wire = json!({
        "content": &request.content,
        "category": FactCategoryV1::from(request.category),
        "tags": &request.tags,
        "entities": &request.entities,
        "metadata": &request.metadata,
    });
    let MemoryFactSanitizationV1::Durable { payload, receipt } = sanitize_memory_fact_payload(wire)
        .map_err(|_| MemoryApplicationError::InvalidCompatibilityInput {
            invariant: "legacy add request privacy sanitizer",
        })?
    else {
        return Ok(None);
    };
    let sanitized =
        serde_json::from_value::<SanitizedFactPayloadWireV1>(payload).map_err(|_| {
            MemoryApplicationError::InvalidCompatibilityInput {
                invariant: "sanitized legacy fact payload",
            }
        })?;
    request.content = sanitized.content;
    request.category = sanitized.category.into();
    request.tags = sanitized.tags;
    request.entities = sanitized.entities;
    request.metadata = sanitized.metadata;
    request.source = source;
    Ok(Some(SanitizedAddFactRequestV1 { request, receipt }))
}

/// Prepares a typed compatibility patch without claiming it is durable-safe.
///
/// The exact durable fact payload does not exist until the authority merges
/// this patch with the current assertion. The authority therefore sanitizes
/// that merged value once and persists the resulting receipt; pre-sanitizing
/// this partial patch would create an unrelated receipt and then discard it.
pub(super) fn prepare_tainted_update_fact_request(
    mut request: UpdateFactRequest,
) -> Result<Option<UpdateFactRequest>, MemoryApplicationError> {
    if let Some(metadata) = request.metadata.as_mut() {
        strip_reserved_automation_run_id(metadata);
    }
    // Match the canonical payload's sorted label order (see the add path).
    if let Some(tags) = request.tags.as_mut() {
        tags.sort_unstable();
    }
    if let Some(entities) = request.entities.as_mut() {
        entities.sort_unstable();
    }
    if request
        .content
        .as_deref()
        .is_some_and(|content| detect_secret_like(content.trim()).is_some())
    {
        return Ok(None);
    }
    let Some(source) = sanitize_optional_memory_text(request.source.clone()) else {
        return Ok(None);
    };
    request.source = source;
    Ok(Some(request))
}

/// `automation_run_id` is typed command metadata. Never permit a caller to
/// smuggle it through a payload that will be persisted and privacy-scanned as
/// ordinary fact metadata.
fn strip_reserved_automation_run_id(metadata: &mut serde_json::Value) {
    if let serde_json::Value::Object(metadata) = metadata {
        metadata.remove("automation_run_id");
    }
}

pub(super) fn sanitize_optional_memory_text(value: Option<String>) -> Option<Option<String>> {
    match value {
        Some(value) => sanitize_provider_metadata_text(&value).map(Some),
        None => Some(None),
    }
}

pub(super) fn sanitize_curation_text(
    value: String,
    invariant: &'static str,
) -> Result<String, MemoryApplicationError> {
    sanitize_provider_metadata_text(&value)
        .ok_or(MemoryApplicationError::InvalidCompatibilityInput { invariant })
}

pub(super) fn sanitize_curation_texts(
    values: Vec<String>,
    invariant: &'static str,
) -> Result<Vec<String>, MemoryApplicationError> {
    values
        .into_iter()
        .map(|value| sanitize_curation_text(value, invariant))
        .collect()
}

pub(super) fn sanitize_curation_metadata(
    value: serde_json::Value,
) -> Result<CompatibilityRelationProvenanceV1, MemoryApplicationError> {
    match sanitize_memory_fact_payload(value).map_err(|_| {
        MemoryApplicationError::InvalidCompatibilityInput {
            invariant: "dashboard curation metadata privacy sanitizer",
        }
    })? {
        MemoryFactSanitizationV1::Durable { payload, receipt } => {
            CompatibilityRelationProvenanceV1::new(payload, receipt)
                .map_err(MemoryApplicationError::Store)
        }
        MemoryFactSanitizationV1::Quarantined => {
            Err(MemoryApplicationError::InvalidCompatibilityInput {
                invariant: "dashboard curation metadata rejected by privacy sanitizer",
            })
        }
    }
}
