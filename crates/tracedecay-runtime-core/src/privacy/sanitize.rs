use std::collections::BTreeSet;
use std::fmt::Write as _;

use serde_json::Value;
use sha2::{Digest, Sha256};
use thiserror::Error;
use tracedecay_domain::{
    CanonicalClaudeSanitizationReceiptMaterialV1, ClaudeObservationIdentityMaterialV1,
    ComponentVersion, DurableClaudeObservationV1, ObservationContractError, ObservationId,
    ObservationOrderingDomainV1, ObservationSourceIdentityV1, PayloadReferenceV1, RetentionClass,
    SanitizationReceiptV1, SanitizerDispositionV1, SensitivityV1, SessionId,
};

use super::detect::{
    DetectionConfidenceV1, DetectionError, PrivacyDetectorV1, SanitizationActionV1,
    SanitizationDetectorOriginV1, SanitizationFindingV1, SanitizationScanBoundaryV1, normalize_key,
    redact_sensitive_values,
};
use super::structural_id::{StructuralIdProtectionError, protect_sensitive_structural_id};
use super::{ParseLimits, ParsedClaudeRecordV1, ParsedPolicyLimitViolation};

pub(crate) const CLAUDE_SANITIZER_VERSION_V1: &str = "privacy.claude-record.v1";
pub(crate) const OBSERVATION_SANITIZER_VERSION_V1: &str = "privacy.observation-record.v1";
const CLAUDE_POLICY_FINGERPRINT_DOMAIN: &[u8] = b"tracedecay.privacy.claude.policy.v1\0";
const OBSERVATION_POLICY_FINGERPRINT_DOMAIN: &[u8] = b"tracedecay.privacy.observation.policy.v1\0";

#[derive(Debug, Error)]
pub enum PrivacySanitizerError {
    #[error("privacy sanitizer policy is invalid")]
    InvalidPolicy,
    #[error("privacy detector is unavailable")]
    DetectorUnavailable,
    #[error("parsed observation record range does not match observation identity")]
    SourceRangeMismatch,
    #[error("parsed observation ordering domain does not match observation identity")]
    OrderingDomainMismatch,
    #[error("provider observation did not cross the canonical normalization boundary")]
    CanonicalEnvelopeRequired,
    #[error("canonical observation provider does not match observation identity")]
    CanonicalProviderMismatch,
    #[error("canonical observation structural identity protection failed")]
    StructuralIdentityProtection,
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

impl From<StructuralIdProtectionError> for PrivacySanitizerError {
    fn from(_: StructuralIdProtectionError) -> Self {
        Self::StructuralIdentityProtection
    }
}

#[derive(Clone, Debug)]
pub struct ClaudeSanitizerPolicyV1 {
    version: ComponentVersion,
    max_record_bytes: usize,
    max_depth: usize,
    max_values: usize,
    sensitive_keys: BTreeSet<String>,
    provider_neutral: bool,
    valid: bool,
}

impl ClaudeSanitizerPolicyV1 {
    pub fn claude_v1() -> Result<Self, PrivacySanitizerError> {
        let version = ComponentVersion::new(CLAUDE_SANITIZER_VERSION_V1)
            .map_err(|_| PrivacySanitizerError::InvalidPolicy)?;
        let sensitive_keys = default_sensitive_keys();
        let limits = ParseLimits::default_policy();
        Ok(Self {
            version,
            max_record_bytes: limits.record_bytes,
            max_depth: limits.depth,
            max_values: limits.values,
            sensitive_keys,
            provider_neutral: false,
            valid: true,
        })
    }

    pub fn observation_v1() -> Result<Self, PrivacySanitizerError> {
        let mut policy = Self::claude_v1()?;
        policy.provider_neutral = true;
        policy.version = ComponentVersion::new(OBSERVATION_SANITIZER_VERSION_V1)
            .map_err(|_| PrivacySanitizerError::InvalidPolicy)?;
        Ok(policy)
    }

    #[must_use]
    #[cfg(test)]
    pub(crate) fn with_sensitive_keys(
        mut self,
        keys: impl IntoIterator<Item = impl AsRef<str>>,
    ) -> Self {
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
        let limits = ParseLimits::default_policy();
        if self.max_record_bytes == limits.record_bytes
            && self.max_depth == limits.depth
            && self.max_values == limits.values
            && self.sensitive_keys == default_sensitive_keys()
        {
            let version = if self.provider_neutral {
                OBSERVATION_SANITIZER_VERSION_V1
            } else {
                CLAUDE_SANITIZER_VERSION_V1
            };
            self.version =
                ComponentVersion::new(version).map_err(|_| PrivacySanitizerError::InvalidPolicy)?;
            return Ok(());
        }

        let mut hasher = Sha256::new();
        let fingerprint_domain = if self.provider_neutral {
            OBSERVATION_POLICY_FINGERPRINT_DOMAIN
        } else {
            CLAUDE_POLICY_FINGERPRINT_DOMAIN
        };
        hasher.update(fingerprint_domain);
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
        let base_version = if self.provider_neutral {
            OBSERVATION_SANITIZER_VERSION_V1
        } else {
            CLAUDE_SANITIZER_VERSION_V1
        };
        self.version = ComponentVersion::new(format!("{base_version}.policy.{fingerprint}"))
            .map_err(|_| PrivacySanitizerError::InvalidPolicy)?;
        Ok(())
    }

    pub fn version(&self) -> &ComponentVersion {
        &self.version
    }
}

fn default_sensitive_keys() -> BTreeSet<String> {
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

    pub fn claude_v1() -> Result<Self, PrivacySanitizerError> {
        Ok(Self::new(ClaudeSanitizerPolicyV1::claude_v1()?))
    }

    pub fn observation_v1() -> Result<Self, PrivacySanitizerError> {
        Ok(Self::new(ClaudeSanitizerPolicyV1::observation_v1()?))
    }

    pub fn policy(&self) -> &ClaudeSanitizerPolicyV1 {
        &self.policy
    }

    /// Sanitizes a parser-issued token without decoding or parsing the record again.
    pub fn sanitize_parsed(
        &self,
        parsed: ParsedClaudeRecordV1,
        mut identity: ClaudeObservationIdentityMaterialV1,
        retention_class: RetentionClass,
    ) -> Result<ClaudeSanitizationOutcomeV1, PrivacySanitizerError> {
        if !self.policy.valid {
            return Err(PrivacySanitizerError::InvalidPolicy);
        }
        if *parsed.source_range() != identity.position() {
            return Err(PrivacySanitizerError::SourceRangeMismatch);
        }
        if parsed.ordering_domain() != identity.ordering_domain() {
            return Err(PrivacySanitizerError::OrderingDomainMismatch);
        }
        let mut protected_identity_changed = false;
        if self.policy.provider_neutral {
            let canonical_provider = parsed
                .canonical_provider()
                .ok_or(PrivacySanitizerError::CanonicalEnvelopeRequired)?;
            if canonical_provider != identity.source().provider() {
                return Err(PrivacySanitizerError::CanonicalProviderMismatch);
            }
            let invalid = || {
                PrivacySanitizerError::DomainContract(
                    ObservationContractError::InvalidCanonicalPayload,
                )
            };
            if parsed
                .value()
                .pointer("/relations/session_id")
                .and_then(Value::as_str)
                != Some(identity.source().session_id().as_str())
            {
                return Err(invalid());
            }
            let stable_record_id = parsed
                .value()
                .get("stable_record_id")
                .and_then(Value::as_str)
                .ok_or_else(invalid)?;
            match identity.native_record_id() {
                Some(native_record_id) if stable_record_id == native_record_id.as_str() => {}
                None if canonical_provider.as_str() == "claude" => {}
                Some(_) | None => return Err(invalid()),
            }
            let (protected_identity, identity_changed) = protect_observation_identity(&identity)?;
            identity = protected_identity;
            protected_identity_changed = identity_changed;
        }
        if let Err(kind) = parsed.verify_limits(self.parse_limits()) {
            return self.non_durable_outcome_from_digest(kind, parsed.raw_digest(), &identity);
        }

        let raw_digest = *parsed.raw_digest();
        let mut payload = parsed.into_value();
        let structural_identity_protected = if self.policy.provider_neutral {
            let changed = protect_canonical_payload_structural_ids(&mut payload)?
                || protected_identity_changed;
            validate_canonical_structural_identity(&payload, &identity)?;
            changed
        } else {
            false
        };
        let mut detected = redact_sensitive_values(payload, &self.policy.sensitive_keys)?;
        if structural_identity_protected {
            detected
                .findings
                .push(SanitizationFindingV1::new_with_origin(
                    PrivacyDetectorV1::ExactCredential,
                    SanitizationDetectorOriginV1::SanitizerPolicy,
                    "$/structural-identity",
                    DetectionConfidenceV1::Exact,
                    SanitizationActionV1::Redacted,
                ));
        }
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
        let (disposition, detector, action, boundary) = match kind {
            ParsedPolicyLimitViolation::NestingDepth => (
                SanitizerDispositionV1::Quarantined,
                PrivacyDetectorV1::StructureLimit,
                SanitizationActionV1::Quarantined,
                SanitizationScanBoundaryV1::NestingDepth,
            ),
            ParsedPolicyLimitViolation::ValueCount => (
                SanitizerDispositionV1::Quarantined,
                PrivacyDetectorV1::StructureLimit,
                SanitizationActionV1::Quarantined,
                SanitizationScanBoundaryV1::ValueCount,
            ),
            ParsedPolicyLimitViolation::RecordSize => (
                SanitizerDispositionV1::Rejected,
                PrivacyDetectorV1::RecordSizeLimit,
                SanitizationActionV1::Rejected,
                SanitizationScanBoundaryV1::RecordBytes,
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
        let finding = SanitizationFindingV1::new_with_incomplete_coverage(
            detector,
            "$",
            DetectionConfidenceV1::Exact,
            action,
            boundary,
        );
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

fn protect_observation_identity(
    identity: &ClaudeObservationIdentityMaterialV1,
) -> Result<(ClaudeObservationIdentityMaterialV1, bool), PrivacySanitizerError> {
    let provider = identity.source().provider().clone();
    let (session_id, session_changed) =
        protected_session_id(identity.source().session_id().as_str())?;
    let source_has_explicit_key = serde_json::to_value(identity.source())
        .map_err(|_| PrivacySanitizerError::StructuralIdentityProtection)?
        .get("source_key")
        .is_some();
    let (source_key, source_key_changed) =
        protected_session_id(identity.source().source_key().as_str())?;
    let source = if source_has_explicit_key {
        ObservationSourceIdentityV1::for_provider_source(provider, session_id, source_key)
    } else {
        ObservationSourceIdentityV1::for_provider(provider, session_id)
    }?;

    let scope = identity.scope().clone();
    let generation = identity.generation();
    let position = identity.position();
    let ordering_domain = identity.ordering_domain();
    let (identity, native_changed) = match identity.native_record_id() {
        Some(native_record_id) => {
            let (native_record_id, changed) = protected_observation_id(native_record_id.as_str())?;
            (
                ClaudeObservationIdentityMaterialV1::for_native_record(
                    source,
                    scope,
                    generation,
                    position,
                    ordering_domain,
                    native_record_id,
                )?,
                changed,
            )
        }
        None if ordering_domain == ObservationOrderingDomainV1::FileBytes => (
            ClaudeObservationIdentityMaterialV1::new(source, scope, generation, position)?,
            false,
        ),
        None => {
            return Err(PrivacySanitizerError::DomainContract(
                ObservationContractError::InvalidNativeRecordIdentity,
            ));
        }
    };
    Ok((
        identity,
        session_changed || source_key_changed || native_changed,
    ))
}

fn protected_session_id(value: &str) -> Result<(SessionId, bool), PrivacySanitizerError> {
    let protected = protect_sensitive_structural_id(value)?;
    let changed = protected != value;
    SessionId::new(protected)
        .map(|value| (value, changed))
        .map_err(|_| {
            PrivacySanitizerError::DomainContract(ObservationContractError::InvalidSourceIdentity)
        })
}

fn protected_observation_id(value: &str) -> Result<(ObservationId, bool), PrivacySanitizerError> {
    let protected = protect_sensitive_structural_id(value)?;
    let changed = protected != value;
    ObservationId::new(protected)
        .map(|value| (value, changed))
        .map_err(|_| {
            PrivacySanitizerError::DomainContract(
                ObservationContractError::InvalidNativeRecordIdentity,
            )
        })
}

fn protect_string_field(
    object: &mut serde_json::Map<String, Value>,
    field: &str,
) -> Result<bool, PrivacySanitizerError> {
    let Some(Value::String(value)) = object.get_mut(field) else {
        return Ok(false);
    };
    let protected = protect_sensitive_structural_id(value)?;
    let changed = protected != *value;
    *value = protected;
    Ok(changed)
}

fn protect_canonical_payload_structural_ids(
    payload: &mut Value,
) -> Result<bool, PrivacySanitizerError> {
    let object = payload
        .as_object_mut()
        .ok_or(PrivacySanitizerError::StructuralIdentityProtection)?;
    let mut changed = protect_string_field(object, "stable_record_id")?;
    if let Some(Value::Object(relations)) = object.get_mut("relations") {
        for field in [
            "session_id",
            "thread_id",
            "turn_id",
            "message_id",
            "parent_session_id",
            "parent_message_id",
            "agent_id",
            "parent_agent_id",
        ] {
            changed |= protect_string_field(relations, field)?;
        }
    }
    if let Some(Value::Array(facts)) = object.get_mut("facts") {
        for fact in facts {
            let fact = fact
                .as_object_mut()
                .ok_or(PrivacySanitizerError::StructuralIdentityProtection)?;
            for field in [
                "invocation_id",
                "parent_session_id",
                "provider_reference",
                "item_id",
                "parent_reference",
                "list_reference",
            ] {
                changed |= protect_string_field(fact, field)?;
            }
        }
    }
    Ok(changed)
}

fn validate_canonical_structural_identity(
    payload: &Value,
    identity: &ClaudeObservationIdentityMaterialV1,
) -> Result<(), PrivacySanitizerError> {
    let invalid =
        || PrivacySanitizerError::DomainContract(ObservationContractError::InvalidCanonicalPayload);
    let relation_session_id = payload
        .pointer("/relations/session_id")
        .and_then(Value::as_str)
        .ok_or_else(invalid)?;
    if relation_session_id != identity.source().session_id().as_str() {
        return Err(invalid());
    }

    let stable_record_id = payload
        .get("stable_record_id")
        .and_then(Value::as_str)
        .ok_or_else(invalid)?;
    match identity.native_record_id() {
        Some(native_record_id) if stable_record_id == native_record_id.as_str() => Ok(()),
        None if identity.source().provider().as_str() == "claude" => Ok(()),
        Some(_) | None => Err(invalid()),
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

pub type RecordSanitizerV1 = ClaudeRecordSanitizerV1;
pub type SanitizedObservationRecordV1 = SanitizedClaudeRecordV1;
pub type ObservationSanitizationOutcomeV1 = ClaudeSanitizationOutcomeV1;

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
