//! Mandatory privacy boundary for V2 observation capture.
//!
//! Provider adapters hand complete records to this module before any durable
//! or externally visible sink. Only [`ObservationSanitizationOutcomeV1::Durable`]
//! carries payload bytes.

mod detect;
pub(crate) mod detector_kernel;
mod parse;
mod sanitize;

pub(crate) use detect::sanitize_provider_metadata_text;
pub use detect::{
    DetectionConfidenceV1, PrivacyDetectorV1, SanitizationActionV1, SanitizationFindingV1,
};
pub use parse::{
    ClaudeRecordParseErrorV1, MAX_OBSERVATION_RECORD_BYTES, ObservationRecordParseErrorV1,
    ParsedClaudeRecordV1, ParsedObservationRecordV1, parse_claude_record_v1,
    parse_normalized_observation_record_v1, parse_observation_record_v1,
};
pub use sanitize::{
    CLAUDE_SANITIZER_VERSION_V1, ClaudeRecordSanitizerV1, ClaudeSanitizationOutcomeV1,
    ClaudeSanitizerPolicyV1, OBSERVATION_SANITIZER_VERSION_V1, ObservationSanitizationOutcomeV1,
    PrivacySanitizerError, RecordSanitizerPolicyV1, RecordSanitizerV1, SanitizedClaudeRecordV1,
    SanitizedObservationRecordV1,
};

#[cfg(test)]
mod tests;
