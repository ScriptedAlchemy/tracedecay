//! Mandatory privacy boundary for V2 observation capture.
//!
//! Provider adapters hand complete records to this module before any durable
//! or externally visible sink. Only [`ObservationSanitizationOutcomeV1::Durable`]
//! carries payload bytes.

mod detect;
pub mod detector_kernel;
mod sanitize;
mod structural_id;

pub use detect::{
    CODE_SOURCE_SANITIZER_VERSION_V1, DetectionConfidenceV1, PrivacyDetectorV1,
    SanitizationActionV1, SanitizationDetectorOriginV1, SanitizationDetectorRevisionV1,
    SanitizationEvidenceAnchorV1, SanitizationFindingV1, SanitizationRemediationClassV1,
    SanitizationScanBoundaryV1, SanitizationScannedCoverageV1,
};
pub use detect::{
    CodeSourceSanitizationV1, MemoryFactSanitizationV1, sanitize_code_source_bytes,
    sanitize_memory_fact_payload, sanitize_provider_metadata_text,
};
pub use sanitize::{
    CLAUDE_SANITIZER_VERSION_V1, ClaudeRecordSanitizerV1, ClaudeSanitizationOutcomeV1,
    ClaudeSanitizerPolicyV1, OBSERVATION_SANITIZER_VERSION_V1, ObservationSanitizationOutcomeV1,
    PrivacySanitizerError, RecordSanitizerPolicyV1, RecordSanitizerV1, SanitizedClaudeRecordV1,
    SanitizedObservationRecordV1,
};
pub use structural_id::{
    protect_optional_sensitive_structural_id, protect_sensitive_structural_id,
};
pub use tracedecay_capture::{
    ClaudeRecordParseErrorV1, MAX_OBSERVATION_RECORD_BYTES, ObservationRecordParseErrorV1,
    ParsedClaudeRecordV1, ParsedObservationRecordV1, parse_claude_record_v1,
    parse_normalized_observation_record_v1, parse_observation_record_v1,
};
pub use tracedecay_capture::{ParseLimits, ParsedPolicyLimitViolation};

#[cfg(test)]
mod tests;
