use std::collections::BTreeSet;
use std::sync::OnceLock;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;
use tracedecay_domain::{
    ComponentVersion, PayloadReferenceV1, SanitizationReceiptId, SanitizationReceiptRefV1,
    SanitizationReceiptV1, SanitizerDispositionV1, SensitivityV1,
};

use super::assessment::{
    SanitizationAssessmentV1, SanitizationHeuristicScaleV1, SanitizationScaleRevisionV1,
    validate_assessment,
};
use super::detector_kernel::{
    CredentialPattern, CredentialPatternKind, CredentialPatternProfile, CredentialRuleSetError,
    JsonPathSegment, JsonVisitMut, NormalizedSensitiveKey, SensitiveKeyPolicy,
    compile_credential_patterns, entropy_bits_per_mille, high_entropy_ranges,
    visit_json_object_keys, visit_sensitive_json_mut,
};
use super::length_prefixed_sha256_hex;

const REDACTED_EXACT: &str = "[TraceDecay redacted: exact credential]";
const REDACTED_BEARER: &str = "[TraceDecay redacted: bearer token]";
const REDACTED_ASSIGNMENT: &str = "[TraceDecay redacted: credential assignment]";
const REDACTED_PRIVATE_KEY: &str = "[TraceDecay redacted: private key]";
const REDACTED_ENTROPY: &str = "[TraceDecay redacted: high-entropy token]";
const REDACTED_SENSITIVE_FIELD: &str = "[TraceDecay redacted: sensitive field]";
pub const MEMORY_FACT_SANITIZER_VERSION_V1: &str = "privacy.memory-fact.v1";
const MEMORY_FACT_RECEIPT_DOMAIN_V1: &[u8] = b"tracedecay.privacy.memory-fact.receipt.v1\0";
const MAX_FINDING_LOCATION_BYTES: usize = 256;
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PrivacyDetectorV1 {
    ExactCredential,
    BearerToken,
    CredentialAssignment,
    PrivateKey,
    SensitiveField,
    HighEntropyToken,
    /// Reserved for public V1 compatibility; malformed input is reported by
    /// `ClaudeRecordParseErrorV1` before detector findings are constructed.
    MalformedRecord,
    RecordSizeLimit,
    StructureLimit,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DetectionConfidenceV1 {
    Exact,
    Contextual,
    Heuristic,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SanitizationActionV1 {
    Redacted,
    Rejected,
    Quarantined,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum SanitizationDetectorOriginV1 {
    BuiltInDetectorKernel,
    SanitizerPolicy,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum SanitizationDetectorRevisionV1 {
    V1,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum SanitizationScanBoundaryV1 {
    RecordBytes,
    NestingDepth,
    ValueCount,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub(crate) enum SanitizationScannedCoverageV1 {
    Complete,
    Incomplete {
        boundary: SanitizationScanBoundaryV1,
    },
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum SanitizationRemediationClassV1 {
    RotateOrRevokeCredential,
    RemoveSensitiveValue,
    CorrectMalformedRecord,
    ReduceInputAndRetry,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SanitizationEvidenceAnchorV1 {
    structural_location: String,
}

impl SanitizationEvidenceAnchorV1 {
    fn structural(location: impl Into<String>) -> Self {
        Self {
            structural_location: bounded_location(location.into()),
        }
    }

    #[cfg(test)]
    pub(crate) fn structural_location(&self) -> &str {
        &self.structural_location
    }
}

/// Safe diagnostic evidence. It intentionally has no field for matched text.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(try_from = "SanitizationFindingWireV1")]
pub struct SanitizationFindingV1 {
    detector: PrivacyDetectorV1,
    detector_origin: SanitizationDetectorOriginV1,
    detector_revision: SanitizationDetectorRevisionV1,
    location: String,
    confidence: DetectionConfidenceV1,
    action: SanitizationActionV1,
    remediation_class: SanitizationRemediationClassV1,
    evidence_anchors: Vec<SanitizationEvidenceAnchorV1>,
    scanned_coverage: SanitizationScannedCoverageV1,
    /// Omitted from the wire when absent, so a finding that abstains keeps its
    /// established serialized shape.
    #[serde(skip_serializing_if = "Option::is_none")]
    assessment: Option<SanitizationAssessmentV1>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SanitizationFindingWireV1 {
    detector: PrivacyDetectorV1,
    detector_origin: SanitizationDetectorOriginV1,
    detector_revision: SanitizationDetectorRevisionV1,
    location: String,
    confidence: DetectionConfidenceV1,
    action: SanitizationActionV1,
    remediation_class: SanitizationRemediationClassV1,
    evidence_anchors: Vec<SanitizationEvidenceAnchorWireV1>,
    scanned_coverage: SanitizationScannedCoverageV1,
    #[serde(default)]
    assessment: Option<SanitizationAssessmentV1>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SanitizationEvidenceAnchorWireV1 {
    structural_location: String,
}

impl TryFrom<SanitizationFindingWireV1> for SanitizationFindingV1 {
    type Error = &'static str;

    fn try_from(wire: SanitizationFindingWireV1) -> Result<Self, Self::Error> {
        if wire.location.len() > MAX_FINDING_LOCATION_BYTES
            || !is_safe_structural_location(&wire.location)
            || wire.evidence_anchors.len() != 1
            || wire.evidence_anchors.iter().any(|anchor| {
                anchor.structural_location.len() > MAX_FINDING_LOCATION_BYTES
                    || anchor.structural_location != wire.location
                    || !is_safe_structural_location(&anchor.structural_location)
            })
        {
            return Err("sanitization finding evidence anchors are invalid");
        }
        if wire.remediation_class != remediation_class(wire.detector) {
            return Err("sanitization finding remediation class is invalid");
        }
        match (wire.detector, wire.action, wire.scanned_coverage) {
            (
                PrivacyDetectorV1::RecordSizeLimit,
                SanitizationActionV1::Rejected,
                SanitizationScannedCoverageV1::Incomplete {
                    boundary: SanitizationScanBoundaryV1::RecordBytes,
                },
            )
            | (
                PrivacyDetectorV1::StructureLimit,
                SanitizationActionV1::Quarantined,
                SanitizationScannedCoverageV1::Incomplete {
                    boundary:
                        SanitizationScanBoundaryV1::NestingDepth
                        | SanitizationScanBoundaryV1::ValueCount,
                },
            ) if wire.detector_origin == SanitizationDetectorOriginV1::SanitizerPolicy => {}
            (PrivacyDetectorV1::RecordSizeLimit | PrivacyDetectorV1::StructureLimit, _, _) => {
                return Err("sanitization finding scanned coverage is invalid");
            }
            (_, _, SanitizationScannedCoverageV1::Complete) => {}
            (_, _, SanitizationScannedCoverageV1::Incomplete { .. }) => {
                return Err("sanitization finding scanned coverage is invalid");
            }
        }
        if wire.detector == PrivacyDetectorV1::MalformedRecord
            && wire.detector_origin != SanitizationDetectorOriginV1::SanitizerPolicy
        {
            return Err("sanitization finding detector origin is invalid");
        }
        if let Some(assessment) = wire.assessment.as_ref() {
            validate_assessment(wire.confidence, assessment)?;
        }
        Ok(Self {
            detector: wire.detector,
            detector_origin: wire.detector_origin,
            detector_revision: wire.detector_revision,
            location: wire.location,
            confidence: wire.confidence,
            action: wire.action,
            remediation_class: wire.remediation_class,
            evidence_anchors: wire
                .evidence_anchors
                .into_iter()
                .map(|anchor| SanitizationEvidenceAnchorV1 {
                    structural_location: anchor.structural_location,
                })
                .collect(),
            scanned_coverage: wire.scanned_coverage,
            assessment: wire.assessment,
        })
    }
}

impl SanitizationFindingV1 {
    pub fn new(
        detector: PrivacyDetectorV1,
        location: impl Into<String>,
        confidence: DetectionConfidenceV1,
        action: SanitizationActionV1,
    ) -> Self {
        let origin = match detector {
            PrivacyDetectorV1::MalformedRecord
            | PrivacyDetectorV1::RecordSizeLimit
            | PrivacyDetectorV1::StructureLimit => SanitizationDetectorOriginV1::SanitizerPolicy,
            PrivacyDetectorV1::ExactCredential
            | PrivacyDetectorV1::BearerToken
            | PrivacyDetectorV1::CredentialAssignment
            | PrivacyDetectorV1::PrivateKey
            | PrivacyDetectorV1::SensitiveField
            | PrivacyDetectorV1::HighEntropyToken => {
                SanitizationDetectorOriginV1::BuiltInDetectorKernel
            }
        };
        Self::new_with_origin(detector, origin, location, confidence, action)
    }

    pub(crate) fn new_with_origin(
        detector: PrivacyDetectorV1,
        detector_origin: SanitizationDetectorOriginV1,
        location: impl Into<String>,
        confidence: DetectionConfidenceV1,
        action: SanitizationActionV1,
    ) -> Self {
        let remediation_class = remediation_class(detector);
        let location = bounded_location(location.into());
        Self {
            detector,
            detector_origin,
            detector_revision: SanitizationDetectorRevisionV1::V1,
            evidence_anchors: vec![SanitizationEvidenceAnchorV1::structural(location.clone())],
            location,
            confidence,
            action,
            remediation_class,
            scanned_coverage: SanitizationScannedCoverageV1::Complete,
            assessment: None,
        }
    }

    /// Attaches a typed assessment, or abstains when the assessment claims more
    /// than its evidence supports.
    ///
    /// Abstention is the specified degradation: a stale, shifted, or
    /// under-supported calibration must not weaken the finding, and the finding
    /// itself — origin, anchors, coverage, remediation class — is unaffected.
    pub(crate) fn with_assessment(mut self, assessment: SanitizationAssessmentV1) -> Self {
        if validate_assessment(self.confidence, &assessment).is_ok() {
            self.assessment = Some(assessment);
        }
        self
    }

    pub(crate) fn new_with_incomplete_coverage(
        detector: PrivacyDetectorV1,
        location: impl Into<String>,
        confidence: DetectionConfidenceV1,
        action: SanitizationActionV1,
        boundary: SanitizationScanBoundaryV1,
    ) -> Self {
        let mut finding = Self::new(detector, location, confidence, action);
        finding.scanned_coverage = SanitizationScannedCoverageV1::Incomplete { boundary };
        finding
    }

    pub fn detector(&self) -> PrivacyDetectorV1 {
        self.detector
    }

    #[cfg(test)]
    pub(crate) fn detector_origin(&self) -> SanitizationDetectorOriginV1 {
        self.detector_origin
    }

    #[cfg(test)]
    pub(crate) fn detector_revision(&self) -> SanitizationDetectorRevisionV1 {
        self.detector_revision
    }

    pub fn location(&self) -> &str {
        &self.location
    }

    pub fn confidence(&self) -> DetectionConfidenceV1 {
        self.confidence
    }

    pub fn action(&self) -> SanitizationActionV1 {
        self.action
    }

    #[cfg(test)]
    pub(crate) fn remediation_class(&self) -> SanitizationRemediationClassV1 {
        self.remediation_class
    }

    pub fn evidence_anchors(&self) -> &[SanitizationEvidenceAnchorV1] {
        &self.evidence_anchors
    }

    #[cfg(test)]
    pub(crate) fn scanned_coverage(&self) -> SanitizationScannedCoverageV1 {
        self.scanned_coverage
    }

    pub fn assessment(&self) -> Option<&SanitizationAssessmentV1> {
        self.assessment.as_ref()
    }
}

fn remediation_class(detector: PrivacyDetectorV1) -> SanitizationRemediationClassV1 {
    match detector {
        PrivacyDetectorV1::ExactCredential
        | PrivacyDetectorV1::BearerToken
        | PrivacyDetectorV1::CredentialAssignment
        | PrivacyDetectorV1::PrivateKey => SanitizationRemediationClassV1::RotateOrRevokeCredential,
        PrivacyDetectorV1::SensitiveField | PrivacyDetectorV1::HighEntropyToken => {
            SanitizationRemediationClassV1::RemoveSensitiveValue
        }
        PrivacyDetectorV1::MalformedRecord => {
            SanitizationRemediationClassV1::CorrectMalformedRecord
        }
        PrivacyDetectorV1::RecordSizeLimit | PrivacyDetectorV1::StructureLimit => {
            SanitizationRemediationClassV1::ReduceInputAndRetry
        }
    }
}

fn is_safe_structural_location(location: &str) -> bool {
    if matches!(
        location,
        "$" | "$/structural-identity" | "$/<bounded-location>"
    ) {
        return true;
    }
    let Some(mut remaining) = location.strip_prefix("$/") else {
        return false;
    };
    while !remaining.is_empty() {
        let (segment, rest) = remaining
            .split_once('/')
            .map_or((remaining, ""), |(segment, rest)| (segment, rest));
        let valid_field = segment
            .strip_prefix("field[")
            .and_then(|value| value.strip_suffix(']'))
            .is_some_and(|value| {
                !value.is_empty() && value.bytes().all(|byte| byte.is_ascii_digit())
            });
        let valid_index = !segment.is_empty() && segment.bytes().all(|byte| byte.is_ascii_digit());
        if !valid_field && !valid_index {
            return false;
        }
        remaining = rest;
    }
    true
}

#[derive(Debug, Error)]
pub enum DetectionError {
    #[error("privacy detector initialization failed")]
    Initialization,
    #[error("privacy detector input exceeds the bounded scan limit")]
    ScanLimitExceeded,
    #[error("privacy sanitizer receipt construction failed")]
    Receipt,
    /// The sanitizer refused a structured document fail-closed: either it
    /// declared a structured data format but could not be parsed without
    /// ambiguity, or it parsed but carries credential material where redaction
    /// cannot rewrite it (an object key). Field semantics cannot be proven
    /// safely scanned either way. This is the sanitizer's own fail-closed
    /// refusal, not a construction fault.
    #[error("privacy sanitizer quarantined an ambiguous structured document")]
    StructuredQuarantine,
}

pub enum MemoryFactSanitizationV1 {
    Durable {
        payload: Value,
        receipt: SanitizationReceiptV1,
    },
    Quarantined,
}

#[derive(Clone, Copy, Debug, Error, PartialEq, Eq)]
pub enum SanitizedPayloadVerificationError {
    #[error("sanitized payload receipt has a stale sanitizer revision")]
    StaleRevision,
    #[error("sanitized payload receipt has a non-durable disposition")]
    NonDurableDisposition,
    #[error("sanitized payload does not match its exact receipt reference")]
    PayloadMismatch,
    #[error("sanitized payload receipt was not issued by the expected sanitizer")]
    ReceiptAuthorityMismatch,
    #[error("sanitized payload cannot be canonically encoded")]
    CanonicalEncoding,
}

pub(crate) struct DetectionResult {
    pub payload: Value,
    pub findings: Vec<SanitizationFindingV1>,
    pub quarantine_findings: Vec<SanitizationFindingV1>,
}

pub(super) struct ConfiguredSensitiveKeyPolicy<'a>(pub(super) &'a BTreeSet<String>);

impl SensitiveKeyPolicy for ConfiguredSensitiveKeyPolicy<'_> {
    type Match = SanitizationDetectorOriginV1;

    fn classify(&self, key: &NormalizedSensitiveKey) -> Option<Self::Match> {
        if self.0.contains(key.ascii_compact()) {
            Some(SanitizationDetectorOriginV1::SanitizerPolicy)
        } else {
            is_semantically_sensitive_key(key)
                .then_some(SanitizationDetectorOriginV1::BuiltInDetectorKernel)
        }
    }
}

fn is_semantically_sensitive_key(key: &NormalizedSensitiveKey) -> bool {
    const SAFE_METADATA_KEYS: &[&str] = &[
        "api_key_hint",
        "credential_type",
        "password_policy",
        "token_budget",
        "token_count",
        "token_counts",
        "token_limit",
        "token_type",
        "token_usage",
    ];

    let separated = key.separated();
    if SAFE_METADATA_KEYS.contains(&separated) {
        return false;
    }

    let suffix = separated.rsplit('_').next().unwrap_or(separated);
    matches!(
        suffix,
        "credential" | "passphrase" | "passwd" | "password" | "secret" | "token"
    ) || matches!(
        separated,
        "access_key" | "api_key" | "private_key" | "secret_key"
    ) || ["_access_key", "_api_key", "_private_key", "_secret_key"]
        .iter()
        .any(|compound| separated.ends_with(compound))
}

pub(crate) fn redact_sensitive_values(
    mut payload: Value,
    sensitive_keys: &BTreeSet<String>,
) -> Result<DetectionResult, DetectionError> {
    let patterns = credential_patterns()?;
    let mut findings = Vec::new();
    let mut quarantine_findings = Vec::new();
    let policy = ConfiguredSensitiveKeyPolicy(sensitive_keys);
    visit_json_object_keys(&payload, &policy, |key, path| {
        let mut key_evidence = key.to_string();
        redact_text(
            &mut key_evidence,
            &structural_location(path),
            patterns,
            &mut quarantine_findings,
            SanitizationActionV1::Quarantined,
        )
    });
    if quarantine_findings.is_empty() {
        visit_sensitive_json_mut(&mut payload, &policy, |value, path| match value {
            JsonVisitMut::SensitiveValue(child, origin) if !child.is_null() => {
                *child = Value::String(REDACTED_SENSITIVE_FIELD.to_string());
                findings.push(SanitizationFindingV1::new_with_origin(
                    PrivacyDetectorV1::SensitiveField,
                    origin,
                    structural_location(path),
                    DetectionConfidenceV1::Contextual,
                    SanitizationActionV1::Redacted,
                ));
                true
            }
            JsonVisitMut::SensitiveValue(_, _) => false,
            JsonVisitMut::String(text) => redact_text(
                text,
                &structural_location(path),
                patterns,
                &mut findings,
                SanitizationActionV1::Redacted,
            ),
        });
    }
    findings.sort();
    findings.dedup();
    quarantine_findings.sort();
    quarantine_findings.dedup();
    Ok(DetectionResult {
        payload,
        findings,
        quarantine_findings,
    })
}

pub fn verify_sanitized_json_payload(
    payload: &Value,
    receipt: &SanitizationReceiptV1,
    expected_revision: &ComponentVersion,
) -> Result<(), SanitizedPayloadVerificationError> {
    if receipt.receipt().sanitizer_version() != expected_revision {
        return Err(SanitizedPayloadVerificationError::StaleRevision);
    }
    if !matches!(
        receipt.disposition(),
        SanitizerDispositionV1::Accepted | SanitizerDispositionV1::Redacted
    ) {
        return Err(SanitizedPayloadVerificationError::NonDurableDisposition);
    }
    let payload_reference = PayloadReferenceV1::for_payload(payload)
        .map_err(|_| SanitizedPayloadVerificationError::CanonicalEncoding)?;
    if receipt.payload() != Some(&payload_reference) {
        return Err(SanitizedPayloadVerificationError::PayloadMismatch);
    }
    Ok(())
}

pub fn serialize_verified_json_payload(
    payload: &Value,
    receipt: &SanitizationReceiptV1,
    expected_revision: &ComponentVersion,
) -> Result<Vec<u8>, SanitizedPayloadVerificationError> {
    verify_sanitized_json_payload(payload, receipt, expected_revision)?;
    serde_json::to_vec(payload).map_err(|_| SanitizedPayloadVerificationError::CanonicalEncoding)
}

pub fn verify_memory_fact_sanitization(
    payload: &Value,
    receipt: &SanitizationReceiptV1,
) -> Result<(), SanitizedPayloadVerificationError> {
    let revision = ComponentVersion::new(MEMORY_FACT_SANITIZER_VERSION_V1)
        .map_err(|_| SanitizedPayloadVerificationError::CanonicalEncoding)?;
    verify_sanitized_json_payload(payload, receipt, &revision)?;
    let expected = memory_fact_receipt(payload, receipt.disposition(), receipt.sensitivity())
        .map_err(|_| SanitizedPayloadVerificationError::CanonicalEncoding)?;
    if receipt != &expected {
        return Err(SanitizedPayloadVerificationError::ReceiptAuthorityMismatch);
    }
    Ok(())
}

/// Sanitizes one structured legacy fact payload and binds durable output to
/// an exact content reference. Raw input is never included in errors or the
/// receipt identifier. Quarantine deliberately carries no payload or receipt.
pub fn sanitize_memory_fact_payload(
    payload: Value,
) -> Result<MemoryFactSanitizationV1, DetectionError> {
    let sensitive_keys = [
        "access_token",
        "api_key",
        "api_token",
        "authorization",
        "auth_token",
        "bearer_token",
        "client_secret",
        "credential",
        "id_token",
        "password",
        "passphrase",
        "passwd",
        "private_key",
        "refresh_token",
        "secret",
        "secret_key",
        "session_token",
        "token",
        "x_api_key",
    ]
    .into_iter()
    .map(normalize_key)
    .collect();
    let detected = redact_sensitive_values(payload, &sensitive_keys)?;
    if !detected.quarantine_findings.is_empty() {
        return Ok(MemoryFactSanitizationV1::Quarantined);
    }

    let disposition = if detected.findings.is_empty() {
        SanitizerDispositionV1::Accepted
    } else {
        SanitizerDispositionV1::Redacted
    };
    let sensitivity = if detected.findings.is_empty() {
        SensitivityV1::NonSensitive
    } else {
        SensitivityV1::Secret
    };
    let receipt = memory_fact_receipt(&detected.payload, disposition, sensitivity)?;
    Ok(MemoryFactSanitizationV1::Durable {
        payload: detected.payload,
        receipt,
    })
}

fn memory_fact_receipt(
    payload: &Value,
    disposition: SanitizerDispositionV1,
    sensitivity: SensitivityV1,
) -> Result<SanitizationReceiptV1, DetectionError> {
    let payload_reference =
        PayloadReferenceV1::for_payload(payload).map_err(|_| DetectionError::Receipt)?;
    let sanitizer_version = ComponentVersion::new(MEMORY_FACT_SANITIZER_VERSION_V1)
        .map_err(|_| DetectionError::Receipt)?;
    let receipt_id = SanitizationReceiptId::new(format!(
        "memory-fact-receipt.v1.{}",
        length_prefixed_sha256_hex(&[
            MEMORY_FACT_RECEIPT_DOMAIN_V1,
            sanitizer_version.as_str().as_bytes(),
            disposition.as_str().as_bytes(),
            sensitivity.as_str().as_bytes(),
            payload_reference.digest().as_str().as_bytes(),
            &payload_reference.byte_len().to_be_bytes(),
        ])
    ))
    .map_err(|_| DetectionError::Receipt)?;
    let receipt_ref = SanitizationReceiptRefV1::new(receipt_id, sanitizer_version)
        .map_err(|_| DetectionError::Receipt)?;
    SanitizationReceiptV1::new(
        receipt_ref,
        disposition,
        sensitivity,
        Some(payload_reference),
    )
    .map_err(|_| DetectionError::Receipt)
}

pub(super) fn redact_text(
    text: &mut String,
    path: &str,
    patterns: &[CredentialPattern],
    findings: &mut Vec<SanitizationFindingV1>,
    action: SanitizationActionV1,
) -> bool {
    let mut candidates = Vec::new();
    for pattern in patterns {
        let (detector, confidence, replacement) = pattern_metadata(pattern.kind());
        let priority = match pattern.kind() {
            CredentialPatternKind::PrivateKey => 4,
            CredentialPatternKind::BearerToken | CredentialPatternKind::CredentialAssignment => 3,
            CredentialPatternKind::KnownCredential => 2,
        };
        candidates.extend(
            pattern
                .ranges(text)
                .into_iter()
                .map(|range| (range, detector, confidence, replacement, priority)),
        );
    }
    candidates.sort_by(|left, right| {
        left.0
            .start
            .cmp(&right.0.start)
            .then_with(|| right.4.cmp(&left.4))
            .then_with(|| right.0.end.cmp(&left.0.end))
    });

    let mut replacements: Vec<(
        std::ops::Range<usize>,
        PrivacyDetectorV1,
        DetectionConfidenceV1,
        &'static str,
        u8,
    )> = Vec::new();
    for candidate in candidates {
        if let Some(previous) = replacements.last_mut()
            && candidate.0.start < previous.0.end
        {
            previous.0.end = previous.0.end.max(candidate.0.end);
            if candidate.4 > previous.4 {
                previous.1 = candidate.1;
                previous.2 = candidate.2;
                previous.3 = candidate.3;
                previous.4 = candidate.4;
            }
            continue;
        }
        replacements.push(candidate);
    }

    let mut changed = !replacements.is_empty();
    for (range, detector, confidence, replacement, _) in replacements.into_iter().rev() {
        text.replace_range(range, replacement);
        findings.push(SanitizationFindingV1::new(
            detector, path, confidence, action,
        ));
    }

    let ranges = high_entropy_ranges(text);
    if !ranges.is_empty() {
        changed = true;
        // The score is read before the tokens are replaced, and reports the
        // strongest evidence in this text rather than an average that a single
        // long low-entropy run could dilute.
        let score_per_mille = ranges.first().and_then(|first| {
            let initial = entropy_bits_per_mille(&text[first.clone()])?;
            ranges.iter().skip(1).try_fold(initial, |strongest, range| {
                entropy_bits_per_mille(&text[range.clone()]).map(|score| strongest.max(score))
            })
        });
        for range in ranges.into_iter().rev() {
            text.replace_range(range, REDACTED_ENTROPY);
        }
        let mut finding = SanitizationFindingV1::new(
            PrivacyDetectorV1::HighEntropyToken,
            path,
            DetectionConfidenceV1::Heuristic,
            action,
        );
        if let Some(score_per_mille) = score_per_mille {
            finding = finding.with_assessment(SanitizationAssessmentV1::HeuristicScore {
                scale: SanitizationHeuristicScaleV1::ShannonEntropyBitsPerCharacter,
                scale_revision: SanitizationScaleRevisionV1::V1,
                score_per_mille,
            });
        }
        findings.push(finding);
    }
    changed
}

pub(super) fn credential_patterns() -> Result<&'static [CredentialPattern], DetectionError> {
    static PATTERNS: OnceLock<Result<Vec<CredentialPattern>, CredentialRuleSetError>> =
        OnceLock::new();
    PATTERNS
        .get_or_init(|| compile_credential_patterns(CredentialPatternProfile::Observation))
        .as_deref()
        .map_err(|_| DetectionError::Initialization)
}

fn pattern_metadata(
    kind: CredentialPatternKind,
) -> (PrivacyDetectorV1, DetectionConfidenceV1, &'static str) {
    match kind {
        CredentialPatternKind::PrivateKey => (
            PrivacyDetectorV1::PrivateKey,
            DetectionConfidenceV1::Exact,
            REDACTED_PRIVATE_KEY,
        ),
        CredentialPatternKind::BearerToken => (
            PrivacyDetectorV1::BearerToken,
            DetectionConfidenceV1::Exact,
            REDACTED_BEARER,
        ),
        CredentialPatternKind::KnownCredential => (
            PrivacyDetectorV1::ExactCredential,
            DetectionConfidenceV1::Exact,
            REDACTED_EXACT,
        ),
        CredentialPatternKind::CredentialAssignment => (
            PrivacyDetectorV1::CredentialAssignment,
            DetectionConfidenceV1::Contextual,
            REDACTED_ASSIGNMENT,
        ),
    }
}

pub(crate) fn normalize_key(key: &str) -> String {
    NormalizedSensitiveKey::new(key).ascii_compact().to_string()
}

fn structural_location(path: &[JsonPathSegment]) -> String {
    let mut location = String::from("$");
    for segment in path {
        match segment {
            JsonPathSegment::Field(index) => {
                location.push_str("/field[");
                location.push_str(&index.to_string());
                location.push(']');
            }
            JsonPathSegment::Index(index) => {
                location.push('/');
                location.push_str(&index.to_string());
            }
        }
    }
    location
}

fn bounded_location(location: String) -> String {
    if location.len() <= MAX_FINDING_LOCATION_BYTES {
        location
    } else {
        "$/<bounded-location>".to_string()
    }
}
