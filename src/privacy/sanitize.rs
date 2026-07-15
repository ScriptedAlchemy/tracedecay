use std::collections::BTreeSet;
use std::fmt::Write as _;

use serde_json::Value;
use sha2::{Digest, Sha256};
use thiserror::Error;
use tracedecay_domain::{
    CanonicalClaudeSanitizationReceiptMaterialV1, ClaudeObservationIdentityMaterialV1,
    ComponentVersion, DurableClaudeObservationV1, ObservationContractError, PayloadReferenceV1,
    RetentionClass, SanitizationReceiptV1, SanitizerDispositionV1, SensitivityV1,
};

use super::detect::{
    DetectionConfidenceV1, DetectionError, PrivacyDetectorV1, SanitizationActionV1,
    SanitizationFindingV1, normalize_key, redact_sensitive_values,
};
use super::parse::{ParseLimits, ParsedClaudeRecordV1, ParsedPolicyLimitViolation};

pub const PR5_CLAUDE_SANITIZER_VERSION: &str = "privacy.claude-record.v1";
const PR5_POLICY_FINGERPRINT_DOMAIN: &[u8] = b"tracedecay.privacy.claude.policy.v1\0";

#[derive(Debug, Error)]
pub enum PrivacySanitizerError {
    #[error("privacy sanitizer policy is invalid")]
    InvalidPolicy,
    #[error("privacy detector is unavailable")]
    DetectorUnavailable,
    #[error("parsed Claude record range does not match observation identity")]
    SourceRangeMismatch,
    #[error("privacy domain contract rejected sanitizer output")]
    DomainContract(#[source] ObservationContractError),
}

impl From<ObservationContractError> for PrivacySanitizerError {
    fn from(error: ObservationContractError) -> Self {
        Self::DomainContract(error)
    }
}

impl From<DetectionError> for PrivacySanitizerError {
    fn from(_: DetectionError) -> Self {
        Self::DetectorUnavailable
    }
}

#[derive(Clone, Debug)]
pub struct ClaudeSanitizerPolicyV1 {
    version: ComponentVersion,
    max_record_bytes: usize,
    max_depth: usize,
    max_values: usize,
    sensitive_keys: BTreeSet<String>,
    valid: bool,
}

impl ClaudeSanitizerPolicyV1 {
    pub fn pr5() -> Result<Self, PrivacySanitizerError> {
        let version = ComponentVersion::new(PR5_CLAUDE_SANITIZER_VERSION)
            .map_err(|_| PrivacySanitizerError::InvalidPolicy)?;
        let sensitive_keys = pr5_sensitive_keys();
        let limits = ParseLimits::pr5();
        Ok(Self {
            version,
            max_record_bytes: limits.record_bytes,
            max_depth: limits.depth,
            max_values: limits.values,
            sensitive_keys,
            valid: true,
        })
    }

    #[must_use]
    pub fn with_sensitive_keys(mut self, keys: impl IntoIterator<Item = impl AsRef<str>>) -> Self {
        self.sensitive_keys
            .extend(keys.into_iter().map(|key| normalize_key(key.as_ref())));
        self.valid = self.refresh_version().is_ok();
        self
    }

    pub fn with_limits(
        mut self,
        max_record_bytes: usize,
        max_depth: usize,
        max_values: usize,
    ) -> Result<Self, PrivacySanitizerError> {
        if max_record_bytes == 0 || max_depth == 0 || max_values == 0 {
            return Err(PrivacySanitizerError::InvalidPolicy);
        }
        self.max_record_bytes = max_record_bytes;
        self.max_depth = max_depth;
        self.max_values = max_values;
        self.refresh_version()?;
        self.valid = true;
        Ok(self)
    }

    fn refresh_version(&mut self) -> Result<(), PrivacySanitizerError> {
        let limits = ParseLimits::pr5();
        if self.max_record_bytes == limits.record_bytes
            && self.max_depth == limits.depth
            && self.max_values == limits.values
            && self.sensitive_keys == pr5_sensitive_keys()
        {
            self.version = ComponentVersion::new(PR5_CLAUDE_SANITIZER_VERSION)
                .map_err(|_| PrivacySanitizerError::InvalidPolicy)?;
            return Ok(());
        }

        let mut hasher = Sha256::new();
        hasher.update(PR5_POLICY_FINGERPRINT_DOMAIN);
        for value in [self.max_record_bytes, self.max_depth, self.max_values] {
            let value = u64::try_from(value).map_err(|_| PrivacySanitizerError::InvalidPolicy)?;
            hasher.update(value.to_be_bytes());
        }
        for key in &self.sensitive_keys {
            let length =
                u64::try_from(key.len()).map_err(|_| PrivacySanitizerError::InvalidPolicy)?;
            hasher.update(length.to_be_bytes());
            hasher.update(key.as_bytes());
        }
        let mut fingerprint = String::with_capacity(64);
        for byte in hasher.finalize() {
            write!(&mut fingerprint, "{byte:02x}")
                .map_err(|_| PrivacySanitizerError::InvalidPolicy)?;
        }
        self.version = ComponentVersion::new(format!(
            "{PR5_CLAUDE_SANITIZER_VERSION}.policy.{fingerprint}"
        ))
        .map_err(|_| PrivacySanitizerError::InvalidPolicy)?;
        Ok(())
    }

    pub fn version(&self) -> &ComponentVersion {
        &self.version
    }
}

fn pr5_sensitive_keys() -> BTreeSet<String> {
    [
        "api_key",
        "api_token",
        "access_token",
        "authorization",
        "auth_token",
        "bearer_token",
        "client_secret",
        "credential",
        "id_token",
        "password",
        "passwd",
        "passphrase",
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
    .collect()
}

#[derive(Clone, Debug)]
pub struct ClaudeRecordSanitizerV1 {
    policy: ClaudeSanitizerPolicyV1,
}

impl ClaudeRecordSanitizerV1 {
    pub fn new(policy: ClaudeSanitizerPolicyV1) -> Self {
        Self { policy }
    }

    pub fn pr5() -> Result<Self, PrivacySanitizerError> {
        Ok(Self::new(ClaudeSanitizerPolicyV1::pr5()?))
    }

    pub fn policy(&self) -> &ClaudeSanitizerPolicyV1 {
        &self.policy
    }

    /// Sanitizes a parser-issued token without decoding or parsing the record again.
    pub fn sanitize_parsed(
        &self,
        parsed: ParsedClaudeRecordV1,
        identity: ClaudeObservationIdentityMaterialV1,
        retention_class: RetentionClass,
    ) -> Result<ClaudeSanitizationOutcomeV1, PrivacySanitizerError> {
        if !self.policy.valid {
            return Err(PrivacySanitizerError::InvalidPolicy);
        }
        if *parsed.source_range() != identity.position() {
            return Err(PrivacySanitizerError::SourceRangeMismatch);
        }
        if let Err(kind) = parsed.verify_limits(self.parse_limits()) {
            return self.non_durable_outcome_from_digest(kind, parsed.raw_digest(), &identity);
        }

        let raw_digest = *parsed.raw_digest();
        let detected = redact_sensitive_values(parsed.into_value(), &self.policy.sensitive_keys)?;
        if !detected.quarantine_findings.is_empty() {
            return self.quarantined_outcome_from_digest(
                &raw_digest,
                &identity,
                detected.quarantine_findings,
            );
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
        let payload_reference = PayloadReferenceV1::for_payload(&detected.payload)?;
        let receipt_ref =
            CanonicalClaudeSanitizationReceiptMaterialV1::for_durable_payload_with_sensitivity(
                &identity,
                self.policy.version.clone(),
                disposition,
                sensitivity,
                &raw_digest,
                &payload_reference,
            )?
            .derive_receipt_ref()?;
        let receipt = SanitizationReceiptV1::new(
            receipt_ref,
            disposition,
            sensitivity,
            Some(payload_reference),
        )?;
        let observation =
            DurableClaudeObservationV1::new(identity, receipt, retention_class, detected.payload)?;
        let sanitized_record = SanitizedClaudeRecordV1::issue(&observation);
        Ok(ClaudeSanitizationOutcomeV1::Durable {
            observation: Box::new(observation),
            sanitized_record,
            findings: detected.findings,
        })
    }

    fn non_durable_outcome_from_digest(
        &self,
        kind: ParsedPolicyLimitViolation,
        raw_digest: &[u8; 32],
        identity: &ClaudeObservationIdentityMaterialV1,
    ) -> Result<ClaudeSanitizationOutcomeV1, PrivacySanitizerError> {
        let (disposition, detector, action) = match kind {
            ParsedPolicyLimitViolation::NestingDepth | ParsedPolicyLimitViolation::ValueCount => (
                SanitizerDispositionV1::Quarantined,
                PrivacyDetectorV1::StructureLimit,
                SanitizationActionV1::Quarantined,
            ),
            ParsedPolicyLimitViolation::RecordSize => (
                SanitizerDispositionV1::Rejected,
                PrivacyDetectorV1::RecordSizeLimit,
                SanitizationActionV1::Rejected,
            ),
        };
        let sensitivity = SensitivityV1::Sensitive;
        let receipt_ref =
            CanonicalClaudeSanitizationReceiptMaterialV1::for_non_durable_with_sensitivity(
                identity,
                self.policy.version.clone(),
                disposition,
                sensitivity,
                raw_digest,
            )?
            .derive_receipt_ref()?;
        let receipt = SanitizationReceiptV1::new(receipt_ref, disposition, sensitivity, None)?;
        let finding =
            SanitizationFindingV1::new(detector, "$", DetectionConfidenceV1::Exact, action);
        Ok(match disposition {
            SanitizerDispositionV1::Rejected => ClaudeSanitizationOutcomeV1::Rejected {
                receipt,
                findings: vec![finding],
            },
            SanitizerDispositionV1::Quarantined => ClaudeSanitizationOutcomeV1::Quarantined {
                receipt,
                findings: vec![finding],
            },
            SanitizerDispositionV1::Accepted | SanitizerDispositionV1::Redacted => {
                return Err(PrivacySanitizerError::InvalidPolicy);
            }
        })
    }

    fn quarantined_outcome_from_digest(
        &self,
        raw_digest: &[u8; 32],
        identity: &ClaudeObservationIdentityMaterialV1,
        findings: Vec<SanitizationFindingV1>,
    ) -> Result<ClaudeSanitizationOutcomeV1, PrivacySanitizerError> {
        let disposition = SanitizerDispositionV1::Quarantined;
        let sensitivity = SensitivityV1::Sensitive;
        let receipt_ref =
            CanonicalClaudeSanitizationReceiptMaterialV1::for_non_durable_with_sensitivity(
                identity,
                self.policy.version.clone(),
                disposition,
                sensitivity,
                raw_digest,
            )?
            .derive_receipt_ref()?;
        let receipt = SanitizationReceiptV1::new(receipt_ref, disposition, sensitivity, None)?;
        Ok(ClaudeSanitizationOutcomeV1::Quarantined { receipt, findings })
    }

    fn parse_limits(&self) -> ParseLimits {
        ParseLimits {
            record_bytes: self.policy.max_record_bytes,
            depth: self.policy.max_depth,
            values: self.policy.max_values,
        }
    }
}

/// Sanitizer-issued, receipt-bound payload for downstream V1 frame folding.
///
/// Its constructor is private so a raw `serde_json::Value` cannot be relabeled
/// as sanitized by provider adapters.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SanitizedClaudeRecordV1(Box<DurableClaudeObservationV1>);

impl SanitizedClaudeRecordV1 {
    fn issue(observation: &DurableClaudeObservationV1) -> Self {
        Self(Box::new(observation.clone()))
    }

    pub fn payload(&self) -> &Value {
        self.0.payload()
    }

    pub fn receipt(&self) -> &SanitizationReceiptV1 {
        self.0.receipt()
    }
}

#[derive(Clone, Debug)]
pub enum ClaudeSanitizationOutcomeV1 {
    Durable {
        observation: Box<DurableClaudeObservationV1>,
        sanitized_record: SanitizedClaudeRecordV1,
        findings: Vec<SanitizationFindingV1>,
    },
    Rejected {
        receipt: SanitizationReceiptV1,
        findings: Vec<SanitizationFindingV1>,
    },
    Quarantined {
        receipt: SanitizationReceiptV1,
        findings: Vec<SanitizationFindingV1>,
    },
}

impl ClaudeSanitizationOutcomeV1 {
    pub fn durable_observation(&self) -> Option<&DurableClaudeObservationV1> {
        match self {
            Self::Durable { observation, .. } => Some(observation),
            Self::Rejected { .. } | Self::Quarantined { .. } => None,
        }
    }

    pub fn receipt(&self) -> &SanitizationReceiptV1 {
        match self {
            Self::Durable { observation, .. } => observation.receipt(),
            Self::Rejected { receipt, .. } | Self::Quarantined { receipt, .. } => receipt,
        }
    }

    pub fn findings(&self) -> &[SanitizationFindingV1] {
        match self {
            Self::Durable { findings, .. }
            | Self::Rejected { findings, .. }
            | Self::Quarantined { findings, .. } => findings,
        }
    }

    pub fn sanitized_record(&self) -> Option<&SanitizedClaudeRecordV1> {
        match self {
            Self::Durable {
                sanitized_record, ..
            } => Some(sanitized_record),
            Self::Rejected { .. } | Self::Quarantined { .. } => None,
        }
    }
}
