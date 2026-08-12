//! Request sanitizers for canonical project-memory payloads.

use serde::Deserialize;
use serde_json::{Value, json};
use tracedecay_domain::{FactCategoryV1, FactRelationProvenanceV1, SanitizationReceiptV1};
use tracedecay_runtime_core::memory::hygiene::detect_secret_like;
use tracedecay_runtime_core::privacy::{
    MemoryFactSanitizationV1, sanitize_memory_fact_payload, sanitize_provider_metadata_text,
};

use super::error::MemoryApplicationError;
use super::project_memory::ProjectMemoryFactAddRequest;

pub(super) struct SanitizedAddFactRequest {
    request: ProjectMemoryFactAddRequest,
    receipt: SanitizationReceiptV1,
}

impl SanitizedAddFactRequest {
    pub(super) fn into_parts(self) -> (ProjectMemoryFactAddRequest, SanitizationReceiptV1) {
        (self.request, self.receipt)
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SanitizedFactPayloadWire {
    content: String,
    category: FactCategoryV1,
    tags: Vec<String>,
    entities: Vec<String>,
    metadata: Value,
    #[serde(default)]
    source_label: Option<String>,
}

pub(super) fn sanitize_add_fact_request(
    mut request: ProjectMemoryFactAddRequest,
) -> Result<Option<SanitizedAddFactRequest>, MemoryApplicationError> {
    strip_reserved_automation_run_id(&mut request.metadata);
    // The canonical payload sorts labels before hashing; the sanitizer receipt
    // is computed over this wire, so it must see the same canonical order.
    request.tags.sort_unstable();
    request.entities.sort_unstable();
    if detect_secret_like(request.content.trim()).is_some() {
        return Ok(None);
    }
    let Some(source_label) = sanitize_optional_memory_text(request.source_label.clone()) else {
        return Ok(None);
    };
    let mut wire = json!({
        "content": &request.content,
        "category": request.category,
        "tags": &request.tags,
        "entities": &request.entities,
        "metadata": &request.metadata,
    });
    if let Some(source_label) = &source_label
        && let Value::Object(wire) = &mut wire
    {
        wire.insert(
            "source_label".to_owned(),
            Value::String(source_label.clone()),
        );
    }
    let MemoryFactSanitizationV1::Durable { payload, receipt } = sanitize_memory_fact_payload(wire)
        .map_err(|_| MemoryApplicationError::InvalidInput {
            invariant: "project-memory add request privacy sanitizer",
        })?
    else {
        return Ok(None);
    };
    let sanitized = serde_json::from_value::<SanitizedFactPayloadWire>(payload).map_err(|_| {
        MemoryApplicationError::InvalidInput {
            invariant: "sanitized project-memory fact payload",
        }
    })?;
    request.content = sanitized.content;
    request.category = sanitized.category;
    request.tags = sanitized.tags;
    request.entities = sanitized.entities;
    request.metadata = sanitized.metadata;
    request.source_label = sanitized.source_label;
    Ok(Some(SanitizedAddFactRequest { request, receipt }))
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
        .ok_or(MemoryApplicationError::InvalidInput { invariant })
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

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SanitizedRelationProvenanceWire {
    source_label: String,
    metadata: Value,
}

pub(super) fn sanitize_curation_provenance(
    source_label: String,
    metadata: Value,
) -> Result<FactRelationProvenanceV1, MemoryApplicationError> {
    let Some(source_label) = sanitize_provider_metadata_text(&source_label) else {
        return Err(MemoryApplicationError::InvalidInput {
            invariant: "canonical relation source rejected by privacy sanitizer",
        });
    };
    let wire = json!({
        "source_label": source_label,
        "metadata": metadata,
    });
    let MemoryFactSanitizationV1::Durable { payload, receipt } = sanitize_memory_fact_payload(wire)
        .map_err(|_| MemoryApplicationError::InvalidInput {
            invariant: "canonical relation provenance privacy sanitizer",
        })?
    else {
        return Err(MemoryApplicationError::InvalidInput {
            invariant: "canonical relation provenance rejected by privacy sanitizer",
        });
    };
    let sanitized =
        serde_json::from_value::<SanitizedRelationProvenanceWire>(payload).map_err(|_| {
            MemoryApplicationError::InvalidInput {
                invariant: "sanitized canonical relation provenance",
            }
        })?;
    FactRelationProvenanceV1::new(sanitized.source_label, sanitized.metadata, receipt).map_err(
        |_| MemoryApplicationError::InvalidInput {
            invariant: "canonical relation provenance",
        },
    )
}
