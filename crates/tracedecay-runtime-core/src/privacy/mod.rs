//! Mandatory privacy boundary for V2 observation capture.
//!
//! Provider adapters hand complete records to this module before any durable
//! or externally visible sink. Only [`ObservationSanitizationOutcomeV1::Durable`]
//! carries payload bytes.

mod assessment;
mod detect;
pub mod detector_kernel;
mod lcm;
mod rules;
mod sanitize;
mod structural_id;
mod structured;
mod structured_text;

/// Lowercase-hex SHA-256 over `parts`, each prefixed with its big-endian
/// `u64` length.
///
/// The length prefix is what makes the concatenation unambiguous, so every
/// receipt and protected identifier in this module derives its digest through
/// this one function rather than re-spelling the loop: a copy that dropped or
/// reordered the prefix would silently mint colliding ids.
pub(crate) fn length_prefixed_sha256_hex(parts: &[&[u8]]) -> String {
    use sha2::{Digest, Sha256};

    let mut hasher = Sha256::new();
    for part in parts {
        hasher.update((part.len() as u64).to_be_bytes());
        hasher.update(part);
    }
    hex::encode(hasher.finalize())
}

pub use assessment::{
    SanitizationAssessmentV1, SanitizationCalibrationDriftV1, SanitizationCalibrationProfileV1,
    SanitizationComparisonSetV1, SanitizationDetectorCohortV1, SanitizationHeuristicScaleV1,
    SanitizationRankComponentV1, SanitizationScaleRevisionV1,
};
pub use detect::{
    DetectionConfidenceV1, MEMORY_FACT_SANITIZER_VERSION_V1, MemoryFactSanitizationV1,
    PrivacyDetectorV1, SanitizationActionV1, SanitizationEvidenceAnchorV1, SanitizationFindingV1,
    SanitizedPayloadVerificationError, sanitize_memory_fact_payload,
    serialize_verified_json_payload, verify_memory_fact_sanitization,
    verify_sanitized_json_payload,
};
pub use lcm::{
    LcmSensitiveRedactionPolicyV1, LcmSensitiveRedactionV1, redact_lcm_sensitive_payload,
};
pub use sanitize::{
    ClaudeRecordSanitizerV1, ClaudeSanitizationOutcomeV1, ClaudeSanitizerPolicyV1,
    ObservationSanitizationOutcomeV1, PrivacySanitizerError, RecordSanitizerV1,
    SanitizedClaudeRecordV1, SanitizedObservationRecordV1,
};
pub use structural_id::{
    protect_optional_sensitive_structural_id, protect_sensitive_structural_id,
};
pub use structured::{StructuredTextFormatV1, sanitize_provider_metadata_json};
pub use structured_text::{
    CODE_SOURCE_SANITIZER_VERSION_V1, CodeSourceSanitizationV1, CodeSourceShapeV1,
    LCM_PAYLOAD_SANITIZER_VERSION_V1, LcmPayloadSanitizationV1, bind_sanitized_lcm_payload_text,
    lcm_payload_detector_revision, quarantine_lcm_payload_text, sanitize_code_source_bytes,
    sanitize_lcm_payload_text, sanitize_provider_metadata_text,
};
pub use tracedecay_capture::{
    ClaudeRecordParseErrorV1, MAX_OBSERVATION_RECORD_BYTES, ObservationRecordParseErrorV1,
    ParsedClaudeRecordV1, ParsedObservationRecordV1, parse_claude_record_v1,
    parse_normalized_observation_record_v1, parse_observation_record_v1,
};
pub use tracedecay_capture::{ParseLimits, ParsedPolicyLimitViolation};

#[cfg(test)]
mod structured_tests;
#[cfg(test)]
mod structured_text_tests;
#[cfg(test)]
mod tests;
