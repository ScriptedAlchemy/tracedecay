//! Request sanitizers for legacy V1 memory payloads.

use tracedecay_runtime_core::memory::hygiene::detect_secret_like;
use tracedecay_runtime_core::memory::types::{AddFactRequest, UpdateFactRequest};
use tracedecay_runtime_core::privacy::{
    MemoryFactSanitizationV1, sanitize_memory_fact_payload, sanitize_provider_metadata_text,
};

use super::error::MemoryApplicationError;

pub(super) fn sanitize_add_fact_request(
    mut request: AddFactRequest,
) -> Result<Option<AddFactRequest>, MemoryApplicationError> {
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
    let wire = serde_json::to_value(&request).map_err(|_| {
        MemoryApplicationError::InvalidCompatibilityInput {
            invariant: "legacy add request serialization",
        }
    })?;
    let MemoryFactSanitizationV1::Durable { payload, .. } = sanitize_memory_fact_payload(wire)
        .map_err(|_| MemoryApplicationError::InvalidCompatibilityInput {
            invariant: "legacy add request privacy sanitizer",
        })?
    else {
        return Ok(None);
    };
    let mut sanitized = serde_json::from_value::<AddFactRequest>(payload).map_err(|_| {
        MemoryApplicationError::InvalidCompatibilityInput {
            invariant: "sanitized legacy add request",
        }
    })?;
    sanitized.source = source;
    Ok(Some(sanitized))
}

pub(super) fn sanitize_update_fact_request(
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
    let wire = serde_json::to_value(&request).map_err(|_| {
        MemoryApplicationError::InvalidCompatibilityInput {
            invariant: "legacy update request serialization",
        }
    })?;
    let MemoryFactSanitizationV1::Durable { payload, .. } = sanitize_memory_fact_payload(wire)
        .map_err(|_| MemoryApplicationError::InvalidCompatibilityInput {
            invariant: "legacy update request privacy sanitizer",
        })?
    else {
        return Ok(None);
    };
    let mut sanitized = serde_json::from_value::<UpdateFactRequest>(payload).map_err(|_| {
        MemoryApplicationError::InvalidCompatibilityInput {
            invariant: "sanitized legacy update request",
        }
    })?;
    sanitized.source = source;
    Ok(Some(sanitized))
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
) -> Result<serde_json::Value, MemoryApplicationError> {
    match sanitize_memory_fact_payload(value).map_err(|_| {
        MemoryApplicationError::InvalidCompatibilityInput {
            invariant: "dashboard curation metadata privacy sanitizer",
        }
    })? {
        MemoryFactSanitizationV1::Durable { payload, .. } => Ok(payload),
        MemoryFactSanitizationV1::Quarantined => {
            Err(MemoryApplicationError::InvalidCompatibilityInput {
                invariant: "dashboard curation metadata rejected by privacy sanitizer",
            })
        }
    }
}
