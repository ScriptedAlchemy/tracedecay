use std::collections::BTreeSet;

use serde_json::Value;
use thiserror::Error;

use super::detect::{SanitizationFindingV1, redact_sensitive_values};
use tracedecay_capture::ParseLimits;

#[derive(Clone, Copy, Debug)]
pub(crate) struct StructuredSanitizationLimits {
    raw_bytes: usize,
    expanded_bytes: usize,
    depth: usize,
    items: usize,
}

impl StructuredSanitizationLimits {
    pub(crate) fn new(
        raw_bytes: usize,
        expanded_bytes: usize,
        depth: usize,
        items: usize,
    ) -> Result<Self, StructuredSanitizationError> {
        if raw_bytes == 0 || expanded_bytes == 0 || depth == 0 || items == 0 {
            return Err(StructuredSanitizationError::InvalidLimits);
        }
        Ok(Self {
            raw_bytes,
            expanded_bytes,
            depth,
            items,
        })
    }
}

#[derive(Debug)]
pub(crate) struct StructuredSanitizedPayload {
    payload: Value,
    findings: Vec<SanitizationFindingV1>,
    structurally_parsed: bool,
}

impl StructuredSanitizedPayload {
    pub(crate) fn payload(&self) -> &Value {
        &self.payload
    }

    pub(crate) const fn was_structurally_parsed(&self) -> bool {
        self.structurally_parsed
    }

    pub(crate) fn findings(&self) -> &[SanitizationFindingV1] {
        &self.findings
    }
}

#[derive(Clone, Copy, Debug, Error, PartialEq, Eq)]
pub(crate) enum StructuredSanitizationError {
    #[error("structured sanitizer limits are invalid")]
    InvalidLimits,
    #[error("structured payload exceeds the raw byte limit")]
    RawBytesExceeded,
    #[error("structured payload exceeds the expanded byte limit")]
    ExpandedBytesExceeded,
    #[error("structured payload exceeds the nesting-depth limit")]
    NestingDepthExceeded,
    #[error("structured payload exceeds the item-count limit")]
    ItemCountExceeded,
    #[error("structured payload is not UTF-8")]
    InvalidEncoding,
    #[error("structured sanitizer is unavailable")]
    SanitizerUnavailable,
}

pub(crate) fn sanitize_structured_payload(
    raw: &[u8],
    limits: StructuredSanitizationLimits,
) -> Result<StructuredSanitizedPayload, StructuredSanitizationError> {
    if raw.len() > limits.raw_bytes {
        return Err(StructuredSanitizationError::RawBytesExceeded);
    }
    let text =
        std::str::from_utf8(raw).map_err(|_| StructuredSanitizationError::InvalidEncoding)?;
    match serde_json::from_str(text) {
        Ok(value) => sanitize_parsed(value, limits),
        Err(_) => sanitize_malformed(text, limits),
    }
}

pub fn sanitize_provider_metadata_json(text: &str, max_bytes: u64) -> Option<Value> {
    let max_bytes = usize::try_from(max_bytes).ok()?;
    if text.len() > max_bytes {
        return None;
    }
    let policy = ParseLimits::default_policy();
    let limits =
        StructuredSanitizationLimits::new(max_bytes, max_bytes, policy.depth, policy.values)
            .ok()?;
    let sanitized = sanitize_structured_payload(text.as_bytes(), limits).ok()?;
    sanitized
        .was_structurally_parsed()
        .then_some(sanitized.payload)
        .filter(Value::is_object)
}

fn sanitize_parsed(
    value: Value,
    limits: StructuredSanitizationLimits,
) -> Result<StructuredSanitizedPayload, StructuredSanitizationError> {
    validate_expansion(&value, limits)?;
    let detected = redact_sensitive_values(value, &BTreeSet::new())
        .map_err(|_| StructuredSanitizationError::SanitizerUnavailable)?;
    if !detected.quarantine_findings.is_empty() {
        return Err(StructuredSanitizationError::SanitizerUnavailable);
    }
    validate_expansion(&detected.payload, limits)?;
    Ok(StructuredSanitizedPayload {
        payload: detected.payload,
        findings: detected.findings,
        structurally_parsed: true,
    })
}

fn sanitize_malformed(
    text: &str,
    limits: StructuredSanitizationLimits,
) -> Result<StructuredSanitizedPayload, StructuredSanitizationError> {
    let detected = redact_sensitive_values(Value::String(text.to_owned()), &BTreeSet::new())
        .map_err(|_| StructuredSanitizationError::SanitizerUnavailable)?;
    if !detected.quarantine_findings.is_empty() {
        return Err(StructuredSanitizationError::SanitizerUnavailable);
    }
    validate_expansion(&detected.payload, limits)?;
    Ok(StructuredSanitizedPayload {
        payload: detected.payload,
        findings: detected.findings,
        structurally_parsed: false,
    })
}

fn validate_expansion(
    value: &Value,
    limits: StructuredSanitizationLimits,
) -> Result<(), StructuredSanitizationError> {
    let expanded =
        serde_json::to_vec(value).map_err(|_| StructuredSanitizationError::SanitizerUnavailable)?;
    if expanded.len() > limits.expanded_bytes {
        return Err(StructuredSanitizationError::ExpandedBytesExceeded);
    }

    let mut stack = vec![(value, 1usize)];
    let mut items = 0usize;
    while let Some((current, depth)) = stack.pop() {
        items = items.saturating_add(1);
        if items > limits.items {
            return Err(StructuredSanitizationError::ItemCountExceeded);
        }
        if depth > limits.depth {
            return Err(StructuredSanitizationError::NestingDepthExceeded);
        }
        match current {
            Value::Object(fields) => stack.extend(
                fields
                    .values()
                    .map(|child| (child, depth.saturating_add(1))),
            ),
            Value::Array(values) => {
                stack.extend(values.iter().map(|child| (child, depth.saturating_add(1))));
            }
            Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => {}
        }
    }
    Ok(())
}
