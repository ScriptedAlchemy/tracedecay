use std::collections::BTreeSet;

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
use super::parse::{ParseFailureKind, ParseLimits, parse_claude_record};

pub const PR5_CLAUDE_SANITIZER_VERSION: &str = "privacy.claude-record.v1";
const DEFAULT_MAX_RECORD_BYTES: usize = 1024 * 1024;
const DEFAULT_MAX_DEPTH: usize = 96;
const DEFAULT_MAX_VALUES: usize = 50_000;

#[derive(Debug, Error)]
pub enum PrivacySanitizerError {
    #[error("privacy sanitizer policy is invalid")]
    InvalidPolicy,
    #[error("privacy detector is unavailable")]
    DetectorUnavailable,
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
}

impl ClaudeSanitizerPolicyV1 {
    pub fn pr5() -> Result<Self, PrivacySanitizerError> {
        let version = ComponentVersion::new(PR5_CLAUDE_SANITIZER_VERSION)
            .map_err(|_| PrivacySanitizerError::InvalidPolicy)?;
        let sensitive_keys = [
            "api_key",
            "api_token",
            "access_token",
            "authorization",
            "auth_token",
            "bearer_token",
            "client_secret",
            "credential",
            "password",
            "passwd",
            "passphrase",
            "private_key",
            "secret",
            "secret_key",
            "token",
        ]
        .into_iter()
        .map(normalize_key)
        .collect();
        Ok(Self {
            version,
            max_record_bytes: DEFAULT_MAX_RECORD_BYTES,
            max_depth: DEFAULT_MAX_DEPTH,
            max_values: DEFAULT_MAX_VALUES,
            sensitive_keys,
        })
    }

    pub fn with_sensitive_keys(mut self, keys: impl IntoIterator<Item = impl AsRef<str>>) -> Self {
        self.sensitive_keys
            .extend(keys.into_iter().map(|key| normalize_key(key.as_ref())));
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
        Ok(self)
    }

    pub fn version(&self) -> &ComponentVersion {
        &self.version
    }
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

    /// Mandatory parse-before-scan boundary for one complete Claude JSONL record.
    pub fn sanitize(
        &self,
        record: &[u8],
        identity: ClaudeObservationIdentityMaterialV1,
        retention_class: RetentionClass,
    ) -> Result<ClaudeSanitizationOutcomeV1, PrivacySanitizerError> {
        let limits = ParseLimits {
            max_record_bytes: self.policy.max_record_bytes,
            max_depth: self.policy.max_depth,
            max_values: self.policy.max_values,
        };
        let parsed = match parse_claude_record(record, limits) {
            Ok(parsed) => parsed,
            Err(kind) => return self.non_durable_outcome(kind, record, &identity),
        };

        let detected = redact_sensitive_values(parsed, &self.policy.sensitive_keys)?;
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
        let receipt_ref = CanonicalClaudeSanitizationReceiptMaterialV1::new(
            &identity,
            self.policy.version.clone(),
            disposition,
            payload_reference.digest().as_str().as_bytes(),
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
        Ok(ClaudeSanitizationOutcomeV1::Durable {
            observation,
            findings: detected.findings,
        })
    }

    fn non_durable_outcome(
        &self,
        kind: ParseFailureKind,
        record: &[u8],
        identity: &ClaudeObservationIdentityMaterialV1,
    ) -> Result<ClaudeSanitizationOutcomeV1, PrivacySanitizerError> {
        let (disposition, detector, action) = match kind {
            ParseFailureKind::TooDeep | ParseFailureKind::TooManyValues => (
                SanitizerDispositionV1::Quarantined,
                PrivacyDetectorV1::StructureLimit,
                SanitizationActionV1::Quarantined,
            ),
            ParseFailureKind::TooLarge => (
                SanitizerDispositionV1::Rejected,
                PrivacyDetectorV1::RecordSizeLimit,
                SanitizationActionV1::Rejected,
            ),
            ParseFailureKind::Empty | ParseFailureKind::Malformed | ParseFailureKind::NonObject => {
                (
                    SanitizerDispositionV1::Rejected,
                    PrivacyDetectorV1::MalformedRecord,
                    SanitizationActionV1::Rejected,
                )
            }
        };
        let raw_digest = Sha256::digest(record);
        let receipt_ref = CanonicalClaudeSanitizationReceiptMaterialV1::new(
            identity,
            self.policy.version.clone(),
            disposition,
            &raw_digest,
        )?
        .derive_receipt_ref()?;
        let receipt =
            SanitizationReceiptV1::new(receipt_ref, disposition, SensitivityV1::Sensitive, None)?;
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
}

#[derive(Clone, Debug)]
pub enum ClaudeSanitizationOutcomeV1 {
    Durable {
        observation: DurableClaudeObservationV1,
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
}
