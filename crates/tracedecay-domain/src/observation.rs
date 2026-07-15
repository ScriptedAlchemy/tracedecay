//! Pure contracts for sanitized Claude transcript observations.
//!
//! These values deliberately exclude filesystem paths, ambient working
//! directories, database row identifiers, and provider display labels from
//! durable identity. Capture code resolves those runtime details before it
//! constructs this boundary.

use std::cmp::Ordering;

use serde::ser::SerializeStruct;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::Value;
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::research::{
    ComponentVersion, ProjectId, RetentionClass, SanitizationReceiptId, SanitizationReceiptRefV1,
    SessionId, canonical_json_bytes,
};

const OBSERVATION_ID_DOMAIN: &[u8] = b"tracedecay.claude.observation.v1\0";
const LEGACY_IDEMPOTENCY_KEY_DOMAIN: &[u8] = b"tracedecay.claude.idempotency.v1\0";
const CLAUDE_RECEIPT_ID_DOMAIN: &[u8] = b"tracedecay.privacy.claude.receipt.v1\0";
const CLAUDE_RECEIPT_ID_PREFIX: &str = "privacy.claude.v1.";

/// Pure validation failures at the observation contract boundary.
#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum ObservationContractError {
    #[error("Claude source identity is invalid")]
    InvalidSourceIdentity,
    #[error("project observation scope is invalid")]
    InvalidProjectScope,
    #[error("Claude file generation must be non-zero")]
    InvalidFileGeneration,
    #[error("Claude record byte range must be non-empty and increasing")]
    InvalidByteRange,
    #[error("{field} must be a canonical SHA-256 digest")]
    InvalidDigest { field: &'static str },
    #[error("canonical observation encoding failed")]
    CanonicalEncoding,
    #[error("source cursors belong to different Claude sessions")]
    CursorSourceMismatch,
    #[error("source cursors belong to different observation scopes")]
    CursorScopeMismatch,
    #[error("source cursors belong to different file generations")]
    CursorGenerationMismatch,
    #[error("sanitization receipt reference is invalid")]
    InvalidReceiptReference,
    #[error("unclassified content cannot cross the durable boundary")]
    UnclassifiedPayload,
    #[error("secret content cannot be accepted without redaction")]
    SecretPayloadAccepted,
    #[error("accepted or redacted content requires a payload reference")]
    ReceiptPayloadRequired,
    #[error("rejected or quarantined content cannot carry a payload reference")]
    ReceiptPayloadForbidden,
    #[error("sanitization receipt does not bind the durable payload")]
    ReceiptPayloadMismatch,
    #[error("serialized observation identity does not match its source evidence")]
    ObservationIdentityMismatch,
    #[error("serialized idempotency key does not match its source evidence")]
    IdempotencyKeyMismatch,
}

/// Stable logical identity of one Claude transcript.
///
/// The session identity is provider-native evidence. The physical file identity
/// is represented separately by [`ClaudeFileGenerationV1`].
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(deny_unknown_fields)]
pub struct ClaudeSourceIdentityV1 {
    session_id: SessionId,
}

impl ClaudeSourceIdentityV1 {
    pub fn new(session_id: SessionId) -> Result<Self, ObservationContractError> {
        session_id
            .validate()
            .map_err(|_| ObservationContractError::InvalidSourceIdentity)?;
        Ok(Self { session_id })
    }

    pub fn session_id(&self) -> &SessionId {
        &self.session_id
    }

    pub fn validate(&self) -> Result<(), ObservationContractError> {
        self.session_id
            .validate()
            .map_err(|_| ObservationContractError::InvalidSourceIdentity)
    }
}

/// Authoritative ownership scope selected before persistence.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ObservationScopeV1 {
    Profile,
    Project { project_id: ProjectId },
}

impl ObservationScopeV1 {
    pub fn validate(&self) -> Result<(), ObservationContractError> {
        match self {
            Self::Profile => Ok(()),
            Self::Project { project_id } => project_id
                .validate()
                .map_err(|_| ObservationContractError::InvalidProjectScope),
        }
    }
}

/// Native file generation identity produced by Claude JSONL framing.
#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(transparent)]
pub struct ClaudeFileGenerationV1(u64);

impl ClaudeFileGenerationV1 {
    pub fn new(file_id: u64) -> Result<Self, ObservationContractError> {
        if file_id == 0 {
            return Err(ObservationContractError::InvalidFileGeneration);
        }
        Ok(Self(file_id))
    }

    pub fn file_id(self) -> u64 {
        self.0
    }
}

impl<'de> Deserialize<'de> for ClaudeFileGenerationV1 {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        Self::new(u64::deserialize(deserializer)?).map_err(serde::de::Error::custom)
    }
}

/// Exact byte span of one complete Claude JSONL record.
#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ClaudeByteRangeV1 {
    start: u64,
    end: u64,
}

impl ClaudeByteRangeV1 {
    pub fn new(start: u64, end: u64) -> Result<Self, ObservationContractError> {
        if start >= end {
            return Err(ObservationContractError::InvalidByteRange);
        }
        Ok(Self { start, end })
    }

    pub fn start(self) -> u64 {
        self.start
    }

    pub fn end(self) -> u64 {
        self.end
    }
}

impl<'de> Deserialize<'de> for ClaudeByteRangeV1 {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Wire {
            start: u64,
            end: u64,
        }

        let wire = Wire::deserialize(deserializer)?;
        Self::new(wire.start, wire.end).map_err(serde::de::Error::custom)
    }
}

/// Stable source evidence used to derive one observation identity.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(deny_unknown_fields)]
pub struct ClaudeObservationIdentityMaterialV1 {
    source: ClaudeSourceIdentityV1,
    scope: ObservationScopeV1,
    generation: ClaudeFileGenerationV1,
    position: ClaudeByteRangeV1,
}

impl ClaudeObservationIdentityMaterialV1 {
    pub fn new(
        source: ClaudeSourceIdentityV1,
        scope: ObservationScopeV1,
        generation: ClaudeFileGenerationV1,
        position: ClaudeByteRangeV1,
    ) -> Result<Self, ObservationContractError> {
        source.validate()?;
        scope.validate()?;
        Ok(Self {
            source,
            scope,
            generation,
            position,
        })
    }

    pub fn source(&self) -> &ClaudeSourceIdentityV1 {
        &self.source
    }

    pub fn scope(&self) -> &ObservationScopeV1 {
        &self.scope
    }

    pub fn generation(&self) -> ClaudeFileGenerationV1 {
        self.generation
    }

    pub fn position(&self) -> ClaudeByteRangeV1 {
        self.position
    }

    pub fn validate(&self) -> Result<(), ObservationContractError> {
        self.source.validate()?;
        self.scope.validate()
    }
}

macro_rules! sha256_newtype {
    ($name:ident, $field:literal) => {
        #[derive(Clone, Debug, Serialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
        #[serde(transparent)]
        pub struct $name(String);

        impl $name {
            pub fn new(value: impl Into<String>) -> Result<Self, ObservationContractError> {
                let value = value.into();
                validate_sha256(&value, $field)?;
                Ok(Self(value))
            }

            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
            where
                D: Deserializer<'de>,
            {
                Self::new(String::deserialize(deserializer)?).map_err(serde::de::Error::custom)
            }
        }
    };
}

sha256_newtype!(CanonicalObservationIdV1, "observation identity");
sha256_newtype!(PayloadDigestV1, "payload digest");

/// Wire-compatible name for the canonical observation identity.
pub type IdempotencyKeyV1 = CanonicalObservationIdV1;

impl CanonicalObservationIdV1 {
    pub fn derive(
        material: &ClaudeObservationIdentityMaterialV1,
    ) -> Result<Self, ObservationContractError> {
        material.validate()?;
        Self::new(domain_digest(OBSERVATION_ID_DOMAIN, material)?)
    }
}

/// Durable Claude JSONL byte cursor tied to one source, owner, and file generation.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(deny_unknown_fields)]
pub struct ClaudeSourceCursorV1 {
    source: ClaudeSourceIdentityV1,
    scope: ObservationScopeV1,
    generation: ClaudeFileGenerationV1,
    byte_offset: u64,
}

impl ClaudeSourceCursorV1 {
    pub fn new(
        source: ClaudeSourceIdentityV1,
        scope: ObservationScopeV1,
        generation: ClaudeFileGenerationV1,
        byte_offset: u64,
    ) -> Result<Self, ObservationContractError> {
        source.validate()?;
        scope.validate()?;
        Ok(Self {
            source,
            scope,
            generation,
            byte_offset,
        })
    }

    pub fn source(&self) -> &ClaudeSourceIdentityV1 {
        &self.source
    }

    pub fn scope(&self) -> &ObservationScopeV1 {
        &self.scope
    }

    pub fn generation(&self) -> ClaudeFileGenerationV1 {
        self.generation
    }

    pub fn byte_offset(&self) -> u64 {
        self.byte_offset
    }

    /// Compares cursors only when their ordering authority is identical.
    pub fn checked_cmp(&self, other: &Self) -> Result<Ordering, ObservationContractError> {
        if self.source != other.source {
            return Err(ObservationContractError::CursorSourceMismatch);
        }
        if self.scope != other.scope {
            return Err(ObservationContractError::CursorScopeMismatch);
        }
        if self.generation != other.generation {
            return Err(ObservationContractError::CursorGenerationMismatch);
        }
        Ok(self.byte_offset.cmp(&other.byte_offset))
    }
}

/// Canonical content-addressed reference to a sanitized JSON payload.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(deny_unknown_fields)]
pub struct PayloadReferenceV1 {
    digest: PayloadDigestV1,
    byte_len: u64,
}

impl PayloadReferenceV1 {
    pub fn for_payload(payload: &Value) -> Result<Self, ObservationContractError> {
        let bytes = canonical_json_bytes(payload)
            .map_err(|_| ObservationContractError::CanonicalEncoding)?;
        Ok(Self {
            digest: PayloadDigestV1::new(sha256_digest(&bytes))?,
            byte_len: bytes.len() as u64,
        })
    }

    pub fn digest(&self) -> &PayloadDigestV1 {
        &self.digest
    }

    pub fn byte_len(&self) -> u64 {
        self.byte_len
    }
}

/// Result of mandatory capture sanitization.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "snake_case")]
pub enum SanitizerDispositionV1 {
    Accepted,
    Redacted,
    Rejected,
    Quarantined,
}

impl SanitizerDispositionV1 {
    pub fn permits_durable_payload(self) -> bool {
        matches!(self, Self::Accepted | Self::Redacted)
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Accepted => "accepted",
            Self::Redacted => "redacted",
            Self::Rejected => "rejected",
            Self::Quarantined => "quarantined",
        }
    }
}

/// Classification applied before content crosses a durable sink.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "snake_case")]
pub enum SensitivityV1 {
    Unclassified,
    NonSensitive,
    Sensitive,
    Secret,
}

/// Canonical inputs used to derive one Claude sanitization receipt reference.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CanonicalClaudeSanitizationReceiptMaterialV1 {
    sanitizer_version: ComponentVersion,
    observation_id: CanonicalObservationIdV1,
    disposition: SanitizerDispositionV1,
    evidence: Vec<u8>,
}

impl CanonicalClaudeSanitizationReceiptMaterialV1 {
    pub fn new(
        identity: &ClaudeObservationIdentityMaterialV1,
        sanitizer_version: ComponentVersion,
        disposition: SanitizerDispositionV1,
        evidence: impl AsRef<[u8]>,
    ) -> Result<Self, ObservationContractError> {
        let observation_id = CanonicalObservationIdV1::derive(identity)?;
        Ok(Self {
            sanitizer_version,
            observation_id,
            disposition,
            evidence: evidence.as_ref().to_vec(),
        })
    }

    pub fn derive_receipt_ref(&self) -> Result<SanitizationReceiptRefV1, ObservationContractError> {
        let mut hasher = Sha256::new();
        hasher.update(CLAUDE_RECEIPT_ID_DOMAIN);
        hasher.update(self.sanitizer_version.as_str().as_bytes());
        hasher.update(self.observation_id.as_str().as_bytes());
        hasher.update(self.disposition.as_str().as_bytes());
        hasher.update(&self.evidence);
        let receipt_id = SanitizationReceiptId::new(format!(
            "{CLAUDE_RECEIPT_ID_PREFIX}{}",
            format_hex(&hasher.finalize())
        ))
        .map_err(|_| ObservationContractError::InvalidReceiptReference)?;
        SanitizationReceiptRefV1::new(receipt_id, self.sanitizer_version.clone())
            .map_err(|_| ObservationContractError::InvalidReceiptReference)
    }
}

/// Receipt binding sanitizer version, disposition, classification, and payload.
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct SanitizationReceiptV1 {
    receipt: SanitizationReceiptRefV1,
    disposition: SanitizerDispositionV1,
    sensitivity: SensitivityV1,
    payload: Option<PayloadReferenceV1>,
}

impl SanitizationReceiptV1 {
    pub fn new(
        receipt: SanitizationReceiptRefV1,
        disposition: SanitizerDispositionV1,
        sensitivity: SensitivityV1,
        payload: Option<PayloadReferenceV1>,
    ) -> Result<Self, ObservationContractError> {
        receipt
            .validate()
            .map_err(|_| ObservationContractError::InvalidReceiptReference)?;
        if sensitivity == SensitivityV1::Unclassified {
            return Err(ObservationContractError::UnclassifiedPayload);
        }
        if disposition == SanitizerDispositionV1::Accepted && sensitivity == SensitivityV1::Secret {
            return Err(ObservationContractError::SecretPayloadAccepted);
        }
        match (disposition.permits_durable_payload(), payload.is_some()) {
            (true, false) => return Err(ObservationContractError::ReceiptPayloadRequired),
            (false, true) => return Err(ObservationContractError::ReceiptPayloadForbidden),
            _ => {}
        }
        Ok(Self {
            receipt,
            disposition,
            sensitivity,
            payload,
        })
    }

    pub fn receipt(&self) -> &SanitizationReceiptRefV1 {
        &self.receipt
    }

    pub fn disposition(&self) -> SanitizerDispositionV1 {
        self.disposition
    }

    pub fn sensitivity(&self) -> SensitivityV1 {
        self.sensitivity
    }

    pub fn payload(&self) -> Option<&PayloadReferenceV1> {
        self.payload.as_ref()
    }
}

impl<'de> Deserialize<'de> for SanitizationReceiptV1 {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Wire {
            receipt: SanitizationReceiptRefV1,
            disposition: SanitizerDispositionV1,
            sensitivity: SensitivityV1,
            payload: Option<PayloadReferenceV1>,
        }

        let wire = Wire::deserialize(deserializer)?;
        Self::new(
            wire.receipt,
            wire.disposition,
            wire.sensitivity,
            wire.payload,
        )
        .map_err(serde::de::Error::custom)
    }
}

/// Durable Claude observation that can only be built from receipt-bound content.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DurableClaudeObservationV1 {
    observation_id: CanonicalObservationIdV1,
    identity: ClaudeObservationIdentityMaterialV1,
    receipt: SanitizationReceiptV1,
    retention_class: RetentionClass,
    payload: Value,
}

impl DurableClaudeObservationV1 {
    pub fn new(
        identity: ClaudeObservationIdentityMaterialV1,
        receipt: SanitizationReceiptV1,
        retention_class: RetentionClass,
        payload: Value,
    ) -> Result<Self, ObservationContractError> {
        identity.validate()?;
        if !receipt.disposition.permits_durable_payload() {
            return Err(ObservationContractError::ReceiptPayloadForbidden);
        }
        let payload_reference = PayloadReferenceV1::for_payload(&payload)?;
        if receipt.payload.as_ref() != Some(&payload_reference) {
            return Err(ObservationContractError::ReceiptPayloadMismatch);
        }
        let observation_id = CanonicalObservationIdV1::derive(&identity)?;
        Ok(Self {
            observation_id,
            identity,
            receipt,
            retention_class,
            payload,
        })
    }

    pub fn observation_id(&self) -> &CanonicalObservationIdV1 {
        &self.observation_id
    }

    pub fn idempotency_key(&self) -> &IdempotencyKeyV1 {
        &self.observation_id
    }

    pub fn identity(&self) -> &ClaudeObservationIdentityMaterialV1 {
        &self.identity
    }

    pub fn source(&self) -> &ClaudeSourceIdentityV1 {
        self.identity.source()
    }

    pub fn scope(&self) -> &ObservationScopeV1 {
        self.identity.scope()
    }

    pub fn receipt(&self) -> &SanitizationReceiptV1 {
        &self.receipt
    }

    pub fn retention_class(&self) -> &RetentionClass {
        &self.retention_class
    }

    pub fn payload(&self) -> &Value {
        &self.payload
    }

    pub fn payload_reference(&self) -> &PayloadReferenceV1 {
        self.receipt
            .payload()
            .expect("durable observation constructor requires a payload reference")
    }

    pub fn canonical_payload_bytes(&self) -> Result<Vec<u8>, ObservationContractError> {
        canonical_json_bytes(&self.payload).map_err(|_| ObservationContractError::CanonicalEncoding)
    }
}

impl Serialize for DurableClaudeObservationV1 {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut wire = serializer.serialize_struct("DurableClaudeObservationV1", 6)?;
        wire.serialize_field("observation_id", &self.observation_id)?;
        wire.serialize_field("idempotency_key", self.idempotency_key())?;
        wire.serialize_field("identity", &self.identity)?;
        wire.serialize_field("receipt", &self.receipt)?;
        wire.serialize_field("retention_class", &self.retention_class)?;
        wire.serialize_field("payload", &self.payload)?;
        wire.end()
    }
}

impl<'de> Deserialize<'de> for DurableClaudeObservationV1 {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Wire {
            observation_id: CanonicalObservationIdV1,
            idempotency_key: IdempotencyKeyV1,
            identity: ClaudeObservationIdentityMaterialV1,
            receipt: SanitizationReceiptV1,
            retention_class: RetentionClass,
            payload: Value,
        }

        let wire = Wire::deserialize(deserializer)?;
        let expected_observation_id = wire.observation_id.clone();
        let expected_idempotency_key = wire.idempotency_key.clone();
        let observation = Self::new(
            wire.identity,
            wire.receipt,
            wire.retention_class,
            wire.payload,
        )
        .map_err(serde::de::Error::custom)?;
        if observation.observation_id != expected_observation_id {
            return Err(serde::de::Error::custom(
                ObservationContractError::ObservationIdentityMismatch,
            ));
        }
        let legacy_idempotency_key =
            legacy_idempotency_key(&observation.identity).map_err(serde::de::Error::custom)?;
        if expected_idempotency_key != *observation.idempotency_key()
            && expected_idempotency_key != legacy_idempotency_key
        {
            return Err(serde::de::Error::custom(
                ObservationContractError::IdempotencyKeyMismatch,
            ));
        }
        Ok(observation)
    }
}

/// Relationship between an existing record and a candidate retry.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "snake_case")]
pub enum ObservationCollisionOutcomeV1 {
    Distinct,
    ExactDuplicate,
    IdentityCollision,
}

pub fn classify_observation_collision(
    existing: &DurableClaudeObservationV1,
    candidate: &DurableClaudeObservationV1,
) -> ObservationCollisionOutcomeV1 {
    if existing.observation_id != candidate.observation_id {
        ObservationCollisionOutcomeV1::Distinct
    } else if existing.payload_reference() == candidate.payload_reference() {
        ObservationCollisionOutcomeV1::ExactDuplicate
    } else {
        ObservationCollisionOutcomeV1::IdentityCollision
    }
}

fn validate_sha256(value: &str, field: &'static str) -> Result<(), ObservationContractError> {
    let valid = value.strip_prefix("sha256:").is_some_and(|encoded| {
        encoded.len() == 64
            && encoded
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    });
    if valid {
        Ok(())
    } else {
        Err(ObservationContractError::InvalidDigest { field })
    }
}

fn domain_digest(
    domain: &[u8],
    value: &impl Serialize,
) -> Result<String, ObservationContractError> {
    let bytes =
        canonical_json_bytes(value).map_err(|_| ObservationContractError::CanonicalEncoding)?;
    let mut hasher = Sha256::new();
    hasher.update(domain);
    hasher.update(bytes);
    Ok(format_sha256(&hasher.finalize()))
}

fn legacy_idempotency_key(
    material: &ClaudeObservationIdentityMaterialV1,
) -> Result<IdempotencyKeyV1, ObservationContractError> {
    IdempotencyKeyV1::new(domain_digest(LEGACY_IDEMPOTENCY_KEY_DOMAIN, material)?)
}

fn sha256_digest(bytes: &[u8]) -> String {
    format_sha256(&Sha256::digest(bytes))
}

fn format_sha256(digest: &[u8]) -> String {
    format!("sha256:{}", format_hex(digest))
}

fn format_hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;

    let mut encoded = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        write!(&mut encoded, "{byte:02x}").expect("writing to a String cannot fail");
    }
    encoded
}
