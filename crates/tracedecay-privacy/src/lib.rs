//! Mandatory privacy boundary for V2 observation capture.
//!
//! Provider adapters hand complete records to this crate before any durable
//! or externally visible sink. Only [`ObservationSanitizationOutcomeV1::Durable`]
//! carries payload bytes.
//!
//! This crate is the single owner of credential detection, record and payload
//! sanitization, and protected structural identifiers. It sits directly above
//! `tracedecay-domain` and `tracedecay-capture` so every store, session, and
//! host layer can sanitize through it without a dependency on the runtime
//! kernel.

#![cfg_attr(not(test), deny(clippy::unwrap_used))]
#![cfg_attr(not(test), deny(clippy::expect_used))]

mod assessment;
mod detect;
pub mod detector_kernel;
mod lcm;
mod privacy_remediation;
mod rules;
mod sanitize;
mod structural_id;
mod structured;
mod structured_text;

use tracedecay_domain::canonical_text::{canonical_framed_sha256, sha256_hex};

/// Lowercase-hex SHA-256 over `parts`, each prefixed with its big-endian
/// `u64` length.
///
/// The length prefix is what makes the concatenation unambiguous, so every
/// receipt and protected identifier in this module derives its digest through
/// this one function: a copy that dropped or reordered the prefix would
/// silently mint colliding ids. The framing is the domain's
/// [`canonical_framed_sha256`] with the first part as the domain separator,
/// so derived ids already on disk are unchanged.
pub(crate) fn length_prefixed_sha256_hex(parts: &[&[u8]]) -> String {
    match parts.split_first() {
        Some((domain, rest)) => canonical_framed_sha256(domain, rest),
        // No parts means no framed input: the digest of zero bytes, exactly
        // as the previous inline loop produced.
        None => sha256_hex(&[]),
    }
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
pub use privacy_remediation::{
    AdmittedPrivacyProjectV1, PrivacyLcmRemediationOutcomeV1, PrivacyMemoryRemediationOutcomeV1,
    PrivacyRemediationDeniedV1, PrivacyRemediationGrantV1, granted_remediation_read_control,
    granted_remediation_write_control, remediation_read_control, remediation_write_control,
    run_at_rest_privacy_remediation, spawn_at_rest_privacy_remediation,
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
    ParsedClaudeRecordV1, ParsedObservationRecordV1, PreparedObservationRecordV1,
    normalize_prepared_observation_record_v1, parse_claude_record_v1,
    parse_normalized_observation_record_v1, parse_observation_record_v1,
    prepare_observation_record_v1,
};
pub use tracedecay_capture::{ParseLimits, ParsedPolicyLimitViolation};

#[cfg(test)]
mod structured_tests;
#[cfg(test)]
mod structured_text_tests;
#[cfg(test)]
mod tests;
