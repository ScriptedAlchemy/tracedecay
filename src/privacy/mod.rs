//! Mandatory privacy boundary for V2 observation capture.
//!
//! Provider adapters hand complete records to this module before any durable
//! or externally visible sink. Only [`ClaudeSanitizationOutcomeV1::Durable`]
//! carries payload bytes.

mod detect;
pub(crate) mod detector_kernel;
mod parse;
mod sanitize;

pub use detect::{
    DetectionConfidenceV1, PrivacyDetectorV1, SanitizationActionV1, SanitizationFindingV1,
};
pub use sanitize::{
    ClaudeRecordSanitizerV1, ClaudeSanitizationOutcomeV1, ClaudeSanitizerPolicyV1,
    PR5_CLAUDE_SANITIZER_VERSION, PR5_MAX_CLAUDE_RECORD_BYTES, PrivacySanitizerError,
};

#[cfg(test)]
mod tests;
