//! Pure contracts for sanitized provider observations.
//!
//! These values deliberately exclude filesystem paths, ambient working
//! directories, database row identifiers, and provider display labels from
//! durable identity. Capture code resolves those runtime details before it
//! constructs this boundary. Claude compatibility aliases preserve the legacy
//! wire format while later providers retain typed native ordering evidence.

use std::cmp::Ordering;
use std::collections::BTreeSet;
use std::fmt;
use std::io::{self, Write};

use schemars::JsonSchema;
use serde::ser::SerializeStruct;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::Value;
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::research::{
    ComponentVersion, ObservationId, ProjectId, ProviderId, RetentionClass, SanitizationReceiptId,
    SanitizationReceiptRefV1, SessionId, canonical_json_bytes,
};

const CLAUDE_OBSERVATION_ID_DOMAIN: &[u8] = b"tracedecay.claude.observation.v1\0";
const OBSERVATION_ID_DOMAIN: &[u8] = b"tracedecay.observation.v1\0";
const LEGACY_IDEMPOTENCY_KEY_DOMAIN: &[u8] = b"tracedecay.claude.idempotency.v1\0";
const CLAUDE_RECEIPT_ID_DOMAIN: &[u8] = b"tracedecay.privacy.claude.receipt.v1\0";
const OBSERVATION_RECEIPT_ID_DOMAIN: &[u8] = b"tracedecay.privacy.observation.receipt.v1\0";
const CLAUDE_RECEIPT_SENSITIVITY_DOMAIN: &[u8] = b"sensitivity\0";
const CLAUDE_RECEIPT_RAW_DIGEST_DOMAIN: &[u8] = b"raw-record-sha256\0";
const CLAUDE_RECEIPT_SANITIZED_PAYLOAD_DOMAIN: &[u8] = b"sanitized-payload-digest\0";
const CLAUDE_RECEIPT_NO_PAYLOAD_DOMAIN: &[u8] = b"no-durable-payload\0";
const CLAUDE_RECEIPT_ID_PREFIX: &str = "privacy.claude.v1.";
const OBSERVATION_RECEIPT_ID_PREFIX: &str = "privacy.observation.v1.";

/// Shared parse and canonical-envelope limits for one observation record.
pub const MAX_OBSERVATION_RECORD_BYTES: usize = 1024 * 1024;
pub const MAX_OBSERVATION_STRUCTURE_DEPTH: usize = 96;
pub const MAX_OBSERVATION_STRUCTURE_VALUES: usize = 50_000;
pub const MAX_CANONICAL_OBSERVATION_FACTS_V1: usize = MAX_OBSERVATION_STRUCTURE_VALUES;

/// Pure validation failures at the observation contract boundary.
#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum ObservationContractError {
    #[error("observation source identity is invalid")]
    InvalidSourceIdentity,
    #[error("native observation record identity is invalid")]
    InvalidNativeRecordIdentity,
    #[error("project observation scope is invalid")]
    InvalidProjectScope,
    #[error("observation source generation must be non-zero")]
    InvalidFileGeneration,
    #[error("observation source range must be non-empty and increasing")]
    InvalidByteRange,
    #[error("{field} must be a canonical SHA-256 digest")]
    InvalidDigest { field: &'static str },
    #[error("canonical observation encoding failed")]
    CanonicalEncoding,
    #[error("source cursors belong to different provider sources")]
    CursorSourceMismatch,
    #[error("source cursors belong to different observation scopes")]
    CursorScopeMismatch,
    #[error("source cursors belong to different source generations")]
    CursorGenerationMismatch,
    #[error("source cursors use different ordering domains")]
    CursorOrderingDomainMismatch,
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
    #[error("canonical observation envelope version is unsupported")]
    UnsupportedCanonicalEnvelopeVersion,
    #[error("canonical observation record kind is invalid")]
    InvalidCanonicalRecordKind,
    #[error("canonical observation envelope must contain at least one fact")]
    CanonicalFactsRequired,
    #[error("canonical observation envelope exceeds the fact-count limit")]
    CanonicalFactsTooMany,
    #[error("canonical observation envelope exceeds the byte limit")]
    CanonicalEnvelopeTooLarge,
    #[error("canonical observation envelope exceeds the nesting limit")]
    CanonicalEnvelopeTooDeep,
    #[error("canonical observation envelope exceeds the value-count limit")]
    CanonicalEnvelopeTooManyValues,
    #[error("durable canonical observation payload is invalid")]
    InvalidCanonicalPayload,
    #[error("canonical observation ordering evidence is invalid")]
    InvalidCanonicalOrderingEvidence,
    #[error("canonical reasoning visibility disagrees with its content")]
    InvalidReasoningVisibility,
}

/// Stable logical identity of one provider observation source.
///
/// The session identity is provider-native evidence. The physical file identity
/// is represented separately by [`ObservationSourceGenerationV1`].
#[derive(
    Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq, PartialOrd, Ord, Hash,
)]
#[serde(deny_unknown_fields)]
pub struct ObservationSourceIdentityV1 {
    #[serde(
        default = "default_observation_provider",
        skip_serializing_if = "is_default_observation_provider"
    )]
    provider: ProviderId,
    session_id: SessionId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    source_key: Option<SessionId>,
}

impl ObservationSourceIdentityV1 {
    pub fn new(session_id: SessionId) -> Result<Self, ObservationContractError> {
        Self::for_provider(default_observation_provider(), session_id)
    }

    pub fn for_provider(
        provider: ProviderId,
        session_id: SessionId,
    ) -> Result<Self, ObservationContractError> {
        provider
            .validate()
            .map_err(|_| ObservationContractError::InvalidSourceIdentity)?;
        session_id
            .validate()
            .map_err(|_| ObservationContractError::InvalidSourceIdentity)?;
        Ok(Self {
            provider,
            session_id,
            source_key: None,
        })
    }

    pub fn for_source(
        session_id: SessionId,
        source_key: SessionId,
    ) -> Result<Self, ObservationContractError> {
        Self::for_provider_source(default_observation_provider(), session_id, source_key)
    }

    pub fn for_provider_source(
        provider: ProviderId,
        session_id: SessionId,
        source_key: SessionId,
    ) -> Result<Self, ObservationContractError> {
        provider
            .validate()
            .map_err(|_| ObservationContractError::InvalidSourceIdentity)?;
        session_id
            .validate()
            .map_err(|_| ObservationContractError::InvalidSourceIdentity)?;
        source_key
            .validate()
            .map_err(|_| ObservationContractError::InvalidSourceIdentity)?;
        Ok(Self {
            provider,
            session_id,
            source_key: Some(source_key),
        })
    }

    pub fn provider(&self) -> &ProviderId {
        &self.provider
    }

    pub fn session_id(&self) -> &SessionId {
        &self.session_id
    }

    pub fn source_key(&self) -> &SessionId {
        self.source_key.as_ref().unwrap_or(&self.session_id)
    }

    pub fn validate(&self) -> Result<(), ObservationContractError> {
        self.provider
            .validate()
            .map_err(|_| ObservationContractError::InvalidSourceIdentity)?;
        self.session_id
            .validate()
            .map_err(|_| ObservationContractError::InvalidSourceIdentity)?;
        if let Some(source_key) = &self.source_key {
            source_key
                .validate()
                .map_err(|_| ObservationContractError::InvalidSourceIdentity)?;
        }
        Ok(())
    }
}

fn default_observation_provider() -> ProviderId {
    ProviderId::new("claude").expect("the built-in Claude provider id is valid")
}

fn is_default_observation_provider(provider: &ProviderId) -> bool {
    provider.as_str() == "claude"
}

/// Compatibility name for the first observation source adapter.
pub type ClaudeSourceIdentityV1 = ObservationSourceIdentityV1;

/// Authoritative ownership scope selected before persistence.
#[derive(
    Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq, PartialOrd, Ord, Hash,
)]
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

/// Native ordering authority for one provider source.
///
/// Numeric positions are comparable only within the same source, scope,
/// generation, and ordering domain.
#[derive(
    Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash,
)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum ObservationOrderingDomainV1 {
    #[default]
    FileBytes,
    SqliteRowId,
    SnapshotOrder,
    DaemonSequence,
}

impl ObservationOrderingDomainV1 {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::FileBytes => "file_bytes",
            Self::SqliteRowId => "sqlite_row_id",
            Self::SnapshotOrder => "snapshot_order",
            Self::DaemonSequence => "daemon_sequence",
        }
    }
}

fn is_file_bytes_ordering(domain: &ObservationOrderingDomainV1) -> bool {
    *domain == ObservationOrderingDomainV1::FileBytes
}

/// Native source generation or incarnation identity.
#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(transparent)]
pub struct ObservationSourceGenerationV1(u64);

impl ObservationSourceGenerationV1 {
    pub fn new(file_id: u64) -> Result<Self, ObservationContractError> {
        if file_id == 0 {
            return Err(ObservationContractError::InvalidFileGeneration);
        }
        Ok(Self(file_id))
    }

    pub fn file_id(self) -> u64 {
        self.0
    }

    pub fn generation_id(self) -> u64 {
        self.0
    }
}

impl<'de> Deserialize<'de> for ObservationSourceGenerationV1 {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        Self::new(u64::deserialize(deserializer)?).map_err(serde::de::Error::custom)
    }
}

/// Compatibility name for Claude JSONL file generations.
pub type ClaudeFileGenerationV1 = ObservationSourceGenerationV1;

/// Exact byte span of one complete Claude JSONL record.
#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ObservationSourceRangeV1 {
    start: u64,
    end: u64,
}

impl ObservationSourceRangeV1 {
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

impl<'de> Deserialize<'de> for ObservationSourceRangeV1 {
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

/// Compatibility name for Claude JSONL byte ranges.
pub type ClaudeByteRangeV1 = ObservationSourceRangeV1;

/// Stable source evidence used to derive one observation identity.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(deny_unknown_fields)]
pub struct ObservationIdentityMaterialV1 {
    source: ObservationSourceIdentityV1,
    scope: ObservationScopeV1,
    generation: ObservationSourceGenerationV1,
    position: ObservationSourceRangeV1,
    #[serde(default, skip_serializing_if = "is_file_bytes_ordering")]
    ordering_domain: ObservationOrderingDomainV1,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    native_record_id: Option<ObservationId>,
}

impl ObservationIdentityMaterialV1 {
    /// Constructs legacy file-byte identity material.
    pub fn new(
        source: ObservationSourceIdentityV1,
        scope: ObservationScopeV1,
        generation: ObservationSourceGenerationV1,
        position: ObservationSourceRangeV1,
    ) -> Result<Self, ObservationContractError> {
        Self::for_ordered_record(
            source,
            scope,
            generation,
            position,
            ObservationOrderingDomainV1::FileBytes,
            None,
        )
    }

    /// Constructs provider identity with an explicit ordering domain and stable
    /// native record key. The key may itself be a canonical content digest when
    /// the provider exposes no immutable identifier.
    pub fn for_native_record(
        source: ObservationSourceIdentityV1,
        scope: ObservationScopeV1,
        generation: ObservationSourceGenerationV1,
        position: ObservationSourceRangeV1,
        ordering_domain: ObservationOrderingDomainV1,
        native_record_id: ObservationId,
    ) -> Result<Self, ObservationContractError> {
        Self::for_ordered_record(
            source,
            scope,
            generation,
            position,
            ordering_domain,
            Some(native_record_id),
        )
    }

    fn for_ordered_record(
        source: ObservationSourceIdentityV1,
        scope: ObservationScopeV1,
        generation: ObservationSourceGenerationV1,
        position: ObservationSourceRangeV1,
        ordering_domain: ObservationOrderingDomainV1,
        native_record_id: Option<ObservationId>,
    ) -> Result<Self, ObservationContractError> {
        source.validate()?;
        scope.validate()?;
        if let Some(record_id) = &native_record_id {
            record_id
                .validate()
                .map_err(|_| ObservationContractError::InvalidNativeRecordIdentity)?;
        }
        Ok(Self {
            source,
            scope,
            generation,
            position,
            ordering_domain,
            native_record_id,
        })
    }

    pub fn source(&self) -> &ObservationSourceIdentityV1 {
        &self.source
    }

    pub fn scope(&self) -> &ObservationScopeV1 {
        &self.scope
    }

    pub fn generation(&self) -> ObservationSourceGenerationV1 {
        self.generation
    }

    pub fn position(&self) -> ObservationSourceRangeV1 {
        self.position
    }

    pub fn ordering_domain(&self) -> ObservationOrderingDomainV1 {
        self.ordering_domain
    }

    pub fn native_record_id(&self) -> Option<&ObservationId> {
        self.native_record_id.as_ref()
    }

    pub fn validate(&self) -> Result<(), ObservationContractError> {
        self.source.validate()?;
        self.scope.validate()?;
        if let Some(record_id) = &self.native_record_id {
            record_id
                .validate()
                .map_err(|_| ObservationContractError::InvalidNativeRecordIdentity)?;
        }
        Ok(())
    }
}

pub type ClaudeObservationIdentityMaterialV1 = ObservationIdentityMaterialV1;

crate::canonical_text::validated_string_newtype!(
    schema,
    ObservationContractError,
    validate_sha256;
    CanonicalObservationIdV1 => "observation identity",
    PayloadDigestV1 => "payload digest",
);

pub type IdempotencyKeyV1 = CanonicalObservationIdV1;

impl CanonicalObservationIdV1 {
    pub fn derive(
        material: &ObservationIdentityMaterialV1,
    ) -> Result<Self, ObservationContractError> {
        material.validate()?;
        if is_default_observation_provider(material.source().provider()) {
            if let Some(native_record_id) = material.native_record_id() {
                #[derive(Serialize)]
                struct ClaudeNativeIdentity<'a> {
                    provider: &'a ProviderId,
                    session_id: &'a SessionId,
                    scope: &'a ObservationScopeV1,
                    native_record_id: &'a ObservationId,
                }

                return Self::new(domain_digest(
                    CLAUDE_OBSERVATION_ID_DOMAIN,
                    &ClaudeNativeIdentity {
                        provider: material.source().provider(),
                        session_id: material.source().session_id(),
                        scope: material.scope(),
                        native_record_id,
                    },
                )?);
            }
            return Self::new(domain_digest(CLAUDE_OBSERVATION_ID_DOMAIN, material)?);
        }
        if let Some(native_record_id) = material.native_record_id() {
            #[derive(Serialize)]
            struct NativeIdentity<'a> {
                source: &'a ObservationSourceIdentityV1,
                scope: &'a ObservationScopeV1,
                native_record_id: &'a ObservationId,
            }

            return Self::new(domain_digest(
                OBSERVATION_ID_DOMAIN,
                &NativeIdentity {
                    source: material.source(),
                    scope: material.scope(),
                    native_record_id,
                },
            )?);
        }
        Self::new(domain_digest(OBSERVATION_ID_DOMAIN, material)?)
    }
}

/// Durable cursor tied to one provider source, owner, generation, and ordering domain.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(deny_unknown_fields)]
pub struct ObservationSourceCursorV1 {
    source: ObservationSourceIdentityV1,
    scope: ObservationScopeV1,
    generation: ObservationSourceGenerationV1,
    byte_offset: u64,
    #[serde(default, skip_serializing_if = "is_file_bytes_ordering")]
    ordering_domain: ObservationOrderingDomainV1,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    file_identity: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    resume_fingerprint: Option<u64>,
}

impl ObservationSourceCursorV1 {
    /// Constructs the legacy-compatible file-byte cursor.
    pub fn new(
        source: ObservationSourceIdentityV1,
        scope: ObservationScopeV1,
        generation: ObservationSourceGenerationV1,
        byte_offset: u64,
    ) -> Result<Self, ObservationContractError> {
        Self::for_ordering(
            source,
            scope,
            generation,
            ObservationOrderingDomainV1::FileBytes,
            byte_offset,
        )
    }

    pub fn for_ordering(
        source: ObservationSourceIdentityV1,
        scope: ObservationScopeV1,
        generation: ObservationSourceGenerationV1,
        ordering_domain: ObservationOrderingDomainV1,
        position: u64,
    ) -> Result<Self, ObservationContractError> {
        source.validate()?;
        scope.validate()?;
        Ok(Self {
            source,
            scope,
            generation,
            byte_offset: position,
            ordering_domain,
            file_identity: None,
            resume_fingerprint: None,
        })
    }

    #[must_use]
    pub fn with_resume_checkpoint(mut self, file_identity: u64, resume_fingerprint: u64) -> Self {
        self.file_identity = Some(file_identity);
        self.resume_fingerprint = Some(resume_fingerprint);
        self
    }

    pub fn source(&self) -> &ObservationSourceIdentityV1 {
        &self.source
    }

    pub fn scope(&self) -> &ObservationScopeV1 {
        &self.scope
    }

    pub fn generation(&self) -> ObservationSourceGenerationV1 {
        self.generation
    }

    pub fn byte_offset(&self) -> u64 {
        self.byte_offset
    }

    pub fn position(&self) -> u64 {
        self.byte_offset
    }

    pub fn ordering_domain(&self) -> ObservationOrderingDomainV1 {
        self.ordering_domain
    }

    pub fn file_identity(&self) -> Option<u64> {
        self.file_identity
    }

    pub fn resume_fingerprint(&self) -> Option<u64> {
        self.resume_fingerprint
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
        if self.ordering_domain != other.ordering_domain {
            return Err(ObservationContractError::CursorOrderingDomainMismatch);
        }
        Ok(self.byte_offset.cmp(&other.byte_offset))
    }
}

/// Compatibility name for Claude JSONL source cursors.
pub type ClaudeSourceCursorV1 = ObservationSourceCursorV1;

pub const CANONICAL_OBSERVATION_ENVELOPE_VERSION_V1: u16 = 1;

/// Provider-neutral semantic payload produced from one decoded native record.
///
/// This value is transient until the privacy boundary sanitizes its serialized
/// form. It is not a second persistence authority or a provider metadata bag.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CanonicalObservationEnvelopeV1 {
    version: u16,
    provider: ProviderId,
    native_record_kind: String,
    stable_record_id: ObservationId,
    relations: CanonicalObservationRelationsV1,
    facts: Vec<CanonicalObservationFactV1>,
    evidence: CanonicalObservationEvidenceV1,
}

impl CanonicalObservationEnvelopeV1 {
    pub fn new(
        provider: ProviderId,
        native_record_kind: impl Into<String>,
        stable_record_id: ObservationId,
        relations: CanonicalObservationRelationsV1,
        facts: Vec<CanonicalObservationFactV1>,
        evidence: CanonicalObservationEvidenceV1,
    ) -> Result<Self, ObservationContractError> {
        let envelope = Self {
            version: CANONICAL_OBSERVATION_ENVELOPE_VERSION_V1,
            provider,
            native_record_kind: native_record_kind.into(),
            stable_record_id,
            relations,
            facts,
            evidence,
        };
        envelope.validate()?;
        Ok(envelope)
    }

    pub fn version(&self) -> u16 {
        self.version
    }

    pub fn provider(&self) -> &ProviderId {
        &self.provider
    }

    pub fn native_record_kind(&self) -> &str {
        &self.native_record_kind
    }

    pub fn stable_record_id(&self) -> &ObservationId {
        &self.stable_record_id
    }

    pub fn relations(&self) -> &CanonicalObservationRelationsV1 {
        &self.relations
    }

    pub fn facts(&self) -> &[CanonicalObservationFactV1] {
        &self.facts
    }

    pub fn evidence(&self) -> &CanonicalObservationEvidenceV1 {
        &self.evidence
    }

    pub fn validate(&self) -> Result<(), ObservationContractError> {
        if self.version != CANONICAL_OBSERVATION_ENVELOPE_VERSION_V1 {
            return Err(ObservationContractError::UnsupportedCanonicalEnvelopeVersion);
        }
        self.provider
            .validate()
            .map_err(|_| ObservationContractError::InvalidSourceIdentity)?;
        self.stable_record_id
            .validate()
            .map_err(|_| ObservationContractError::InvalidNativeRecordIdentity)?;
        validate_canonical_label(&self.native_record_kind)?;
        self.relations.validate()?;
        self.evidence.validate()?;
        if self.facts.is_empty() {
            return Err(ObservationContractError::CanonicalFactsRequired);
        }
        if self.facts.len() > MAX_CANONICAL_OBSERVATION_FACTS_V1 {
            return Err(ObservationContractError::CanonicalFactsTooMany);
        }
        for fact in &self.facts {
            fact.validate()?;
        }
        validate_canonical_envelope_limits(self)?;
        Ok(())
    }
}

fn validate_canonical_envelope_limits(
    envelope: &CanonicalObservationEnvelopeV1,
) -> Result<(), ObservationContractError> {
    let mut content_values = 0usize;
    for fact in &envelope.facts {
        if let Some(content) = fact.content() {
            validate_value_structure(content, 4, &mut content_values)?;
        }
    }

    let mut writer = ByteLimitWriter::new(MAX_OBSERVATION_RECORD_BYTES);
    match serde_json::to_writer(&mut writer, envelope) {
        Err(_) if writer.exceeded => {
            return Err(ObservationContractError::CanonicalEnvelopeTooLarge);
        }
        Err(_) => return Err(ObservationContractError::CanonicalEncoding),
        Ok(()) => {}
    }

    let value =
        serde_json::to_value(envelope).map_err(|_| ObservationContractError::CanonicalEncoding)?;
    let mut values = 0usize;
    validate_value_structure(&value, 1, &mut values)
}

fn validate_value_structure(
    value: &Value,
    initial_depth: usize,
    values: &mut usize,
) -> Result<(), ObservationContractError> {
    let mut stack = vec![(value, initial_depth)];
    while let Some((current, depth)) = stack.pop() {
        *values = values.saturating_add(1);
        if *values > MAX_OBSERVATION_STRUCTURE_VALUES {
            return Err(ObservationContractError::CanonicalEnvelopeTooManyValues);
        }
        if depth > MAX_OBSERVATION_STRUCTURE_DEPTH {
            return Err(ObservationContractError::CanonicalEnvelopeTooDeep);
        }
        match current {
            Value::Object(fields) => stack.extend(
                fields
                    .values()
                    .map(|child| (child, depth.saturating_add(1))),
            ),
            Value::Array(items) => {
                stack.extend(items.iter().map(|child| (child, depth.saturating_add(1))))
            }
            _ => {}
        }
    }
    Ok(())
}

struct ByteLimitWriter {
    written: usize,
    limit: usize,
    exceeded: bool,
}

impl ByteLimitWriter {
    fn new(limit: usize) -> Self {
        Self {
            written: 0,
            limit,
            exceeded: false,
        }
    }
}

impl Write for ByteLimitWriter {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        let remaining = self.limit.saturating_sub(self.written);
        if buffer.len() > remaining {
            self.exceeded = true;
            return Err(io::Error::other(
                "canonical observation byte limit exceeded",
            ));
        }
        self.written += buffer.len();
        Ok(buffer.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CanonicalObservationRelationsV1 {
    session_id: SessionId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    thread_id: Option<ObservationId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    turn_id: Option<ObservationId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    message_id: Option<ObservationId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    parent_session_id: Option<SessionId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    parent_message_id: Option<ObservationId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    agent_id: Option<ObservationId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    parent_agent_id: Option<ObservationId>,
}

impl CanonicalObservationRelationsV1 {
    pub fn new(session_id: SessionId) -> Self {
        Self {
            session_id,
            thread_id: None,
            turn_id: None,
            message_id: None,
            parent_session_id: None,
            parent_message_id: None,
            agent_id: None,
            parent_agent_id: None,
        }
    }

    #[must_use]
    pub fn with_thread_id(mut self, thread_id: ObservationId) -> Self {
        self.thread_id = Some(thread_id);
        self
    }

    #[must_use]
    pub fn with_turn_id(mut self, turn_id: ObservationId) -> Self {
        self.turn_id = Some(turn_id);
        self
    }

    #[must_use]
    pub fn with_message_id(mut self, message_id: ObservationId) -> Self {
        self.message_id = Some(message_id);
        self
    }

    #[must_use]
    pub fn with_parent_session_id(mut self, parent_session_id: SessionId) -> Self {
        self.parent_session_id = Some(parent_session_id);
        self
    }

    #[must_use]
    pub fn with_parent_message_id(mut self, parent_message_id: ObservationId) -> Self {
        self.parent_message_id = Some(parent_message_id);
        self
    }

    #[must_use]
    pub fn with_agent_id(mut self, agent_id: ObservationId) -> Self {
        self.agent_id = Some(agent_id);
        self
    }

    #[must_use]
    pub fn with_parent_agent_id(mut self, parent_agent_id: ObservationId) -> Self {
        self.parent_agent_id = Some(parent_agent_id);
        self
    }

    pub fn session_id(&self) -> &SessionId {
        &self.session_id
    }

    pub fn thread_id(&self) -> Option<&ObservationId> {
        self.thread_id.as_ref()
    }

    pub fn turn_id(&self) -> Option<&ObservationId> {
        self.turn_id.as_ref()
    }

    pub fn message_id(&self) -> Option<&ObservationId> {
        self.message_id.as_ref()
    }

    pub fn parent_session_id(&self) -> Option<&SessionId> {
        self.parent_session_id.as_ref()
    }

    pub fn parent_message_id(&self) -> Option<&ObservationId> {
        self.parent_message_id.as_ref()
    }

    pub fn agent_id(&self) -> Option<&ObservationId> {
        self.agent_id.as_ref()
    }

    pub fn parent_agent_id(&self) -> Option<&ObservationId> {
        self.parent_agent_id.as_ref()
    }

    fn validate(&self) -> Result<(), ObservationContractError> {
        self.session_id
            .validate()
            .map_err(|_| ObservationContractError::InvalidSourceIdentity)?;
        if let Some(parent_session_id) = &self.parent_session_id {
            parent_session_id
                .validate()
                .map_err(|_| ObservationContractError::InvalidSourceIdentity)?;
        }
        for id in [
            self.thread_id.as_ref(),
            self.turn_id.as_ref(),
            self.message_id.as_ref(),
            self.parent_message_id.as_ref(),
            self.agent_id.as_ref(),
            self.parent_agent_id.as_ref(),
        ]
        .into_iter()
        .flatten()
        {
            id.validate()
                .map_err(|_| ObservationContractError::InvalidNativeRecordIdentity)?;
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CanonicalObservationEvidenceV1 {
    ordering_domain: ObservationOrderingDomainV1,
    range: ObservationSourceRangeV1,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    native_sequence: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    native_timestamp: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    revision: Option<String>,
}

impl CanonicalObservationEvidenceV1 {
    pub fn new(
        ordering_domain: ObservationOrderingDomainV1,
        range: ObservationSourceRangeV1,
    ) -> Self {
        Self {
            ordering_domain,
            range,
            native_sequence: None,
            native_timestamp: None,
            revision: None,
        }
    }

    #[must_use]
    pub fn with_native_sequence(mut self, native_sequence: u64) -> Self {
        self.native_sequence = Some(native_sequence);
        self
    }

    #[must_use]
    pub fn with_native_timestamp(mut self, native_timestamp: i64) -> Self {
        self.native_timestamp = Some(native_timestamp);
        self
    }

    pub fn with_revision(
        mut self,
        revision: impl Into<String>,
    ) -> Result<Self, ObservationContractError> {
        let revision = revision.into();
        validate_canonical_label(&revision)?;
        self.revision = Some(revision);
        Ok(self)
    }

    pub fn ordering_domain(&self) -> ObservationOrderingDomainV1 {
        self.ordering_domain
    }

    pub fn range(&self) -> ObservationSourceRangeV1 {
        self.range
    }

    pub fn native_sequence(&self) -> Option<u64> {
        self.native_sequence
    }

    pub fn native_timestamp(&self) -> Option<i64> {
        self.native_timestamp
    }

    pub fn revision(&self) -> Option<&str> {
        self.revision.as_deref()
    }

    fn validate(&self) -> Result<(), ObservationContractError> {
        if let Some(revision) = &self.revision {
            validate_canonical_label(revision)
                .map_err(|_| ObservationContractError::InvalidCanonicalOrderingEvidence)?;
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CanonicalMessageRoleV1 {
    User,
    Assistant,
    System,
    Tool,
    Unknown,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CanonicalReasoningVisibilityV1 {
    Visible,
    Redacted,
    Unavailable,
    NotApplicable,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CanonicalGitEvidenceKindV1 {
    Diff,
    FileEdit,
    Commit,
    Branch,
    PullRequest,
    Unknown,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CanonicalWorkflowEvidenceKindV1 {
    Plan,
    Task,
    Subagent,
    ModelFallback,
    Attribution,
    PullRequest,
    Unknown,
}

/// Provider-neutral meaning of one native workflow lifecycle fact.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CanonicalWorkflowSemanticKindV1 {
    Goal,
    Plan,
    TodoList,
    TodoItem,
    Task,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CanonicalBoundaryKindV1 {
    SessionStart,
    SessionEnd,
    TurnStart,
    TurnEnd,
    CompactionBoundary,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CanonicalUnknownStateV1 {
    Absent,
    Null,
    Unsupported,
    Redacted,
    Unrecoverable,
    Malformed,
}

/// Native grain to which a provider's usage counters apply.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ProviderUsageScopeV1 {
    Request,
    Message,
    Turn,
    Session,
    Unknown,
    Unavailable,
}

impl ProviderUsageScopeV1 {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Request => "request",
            Self::Message => "message",
            Self::Turn => "turn",
            Self::Session => "session",
            Self::Unknown => "unknown",
            Self::Unavailable => "unavailable",
        }
    }

    pub fn from_durable_str(value: &str) -> Option<Self> {
        match value {
            "request" => Some(Self::Request),
            "message" => Some(Self::Message),
            "turn" => Some(Self::Turn),
            "session" => Some(Self::Session),
            "unknown" => Some(Self::Unknown),
            "unavailable" => Some(Self::Unavailable),
            _ => None,
        }
    }
}

/// Whether counters are additive for this record or a provider running total.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ProviderUsageCounterSemanticsV1 {
    Delta,
    Cumulative,
    Unknown,
    Unavailable,
}

impl ProviderUsageCounterSemanticsV1 {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Delta => "delta",
            Self::Cumulative => "cumulative",
            Self::Unknown => "unknown",
            Self::Unavailable => "unavailable",
        }
    }

    pub fn from_durable_str(value: &str) -> Option<Self> {
        match value {
            "delta" => Some(Self::Delta),
            "cumulative" => Some(Self::Cumulative),
            "unknown" => Some(Self::Unknown),
            "unavailable" => Some(Self::Unavailable),
            _ => None,
        }
    }
}

/// Provider-usage contract dimensions absent from otherwise trustworthy
/// native counters. A fact remains uncorrelated until every missing dimension
/// is supplied by native evidence; neighboring observations are not evidence.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum ProviderUsageContractDimensionV1 {
    Model,
    Scope,
    CounterSemantics,
    Correlation,
}

/// Provider model identity is never inferred from a neighboring message.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
pub enum ProviderUsageModelV1 {
    Known { model: String },
    Unknown { reason: CanonicalUnknownStateV1 },
    Unavailable { reason: CanonicalUnknownStateV1 },
}

/// Counters retain missing fields and unavailable evidence without zero filling.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
pub enum ProviderUsageCountersV1 {
    Known {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        input_tokens: Option<u64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        output_tokens: Option<u64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cache_read_tokens: Option<u64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cache_write_tokens: Option<u64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reasoning_tokens: Option<u64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        total_tokens: Option<u64>,
    },
    Unknown {
        reason: CanonicalUnknownStateV1,
    },
    Unavailable {
        reason: CanonicalUnknownStateV1,
    },
}

/// Immutable read model for one exactly-once provider usage projection.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ProviderUsageObservationV1 {
    pub observation_id: CanonicalObservationIdV1,
    pub usage_ordinal: u32,
    pub receipt_id: String,
    pub observation_sequence: u64,
    pub scope: ObservationScopeV1,
    pub provider: ProviderId,
    pub model: ProviderUsageModelV1,
    pub native_scope: ProviderUsageScopeV1,
    pub counter_semantics: ProviderUsageCounterSemanticsV1,
    pub counters: ProviderUsageCountersV1,
    pub session_id: SessionId,
    pub turn_id: Option<ObservationId>,
    pub message_id: Option<ObservationId>,
    pub request_id: Option<ObservationId>,
    pub native_kind: String,
    pub native_field: String,
    pub ordering_domain: ObservationOrderingDomainV1,
    pub source_range: ObservationSourceRangeV1,
    pub native_timestamp: Option<i64>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ProviderUsageCursorV1 {
    pub observation_sequence: u64,
    pub usage_ordinal: u32,
    pub upper_observation_sequence: u64,
    pub scope: ObservationScopeV1,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
pub enum ProviderUsageReadV1 {
    Known {
        observations: Vec<ProviderUsageObservationV1>,
        upper_observation_sequence: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        next_cursor: Option<ProviderUsageCursorV1>,
    },
    Unknown {
        reason: CanonicalUnknownStateV1,
        upper_observation_sequence: u64,
    },
    Unavailable {
        reason: CanonicalUnknownStateV1,
        upper_observation_sequence: u64,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum CanonicalObservationFactV1 {
    Session {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        project_path: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        location_path: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        transcript_path: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        title: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        started_at: Option<i64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        ended_at: Option<i64>,
        source: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        native_source: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        profile: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        location_provenance: Option<String>,
    },
    Message {
        role: CanonicalMessageRoleV1,
        content: Value,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        model: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        timestamp: Option<i64>,
    },
    ToolInvocation {
        invocation_id: ObservationId,
        name: String,
        arguments: Value,
    },
    ToolResult {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        invocation_id: Option<ObservationId>,
        content: Value,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        success: Option<bool>,
    },
    ProviderUsage {
        model: ProviderUsageModelV1,
        native_scope: ProviderUsageScopeV1,
        counter_semantics: ProviderUsageCounterSemanticsV1,
        counters: ProviderUsageCountersV1,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        request_id: Option<ObservationId>,
        native_kind: String,
        native_field: String,
    },
    /// Counters captured without enough native evidence to establish a
    /// provider/model/scope/correlation contract. These remain observation
    /// evidence only and are never projected into billing or messages.
    UncorrelatedUsage {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        input_tokens: Option<u64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        output_tokens: Option<u64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cache_read_tokens: Option<u64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cache_write_tokens: Option<u64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reasoning_tokens: Option<u64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        total_tokens: Option<u64>,
        native_kind: String,
        native_field: String,
        missing_dimensions: BTreeSet<ProviderUsageContractDimensionV1>,
    },
    Compaction {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        summary: Option<Value>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        input_tokens: Option<u64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        output_tokens: Option<u64>,
    },
    Reasoning {
        visibility: CanonicalReasoningVisibilityV1,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        content: Option<Value>,
    },
    Git {
        evidence_kind: CanonicalGitEvidenceKindV1,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reference: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        content: Option<Value>,
    },
    Workflow {
        evidence_kind: CanonicalWorkflowEvidenceKindV1,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reference: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        content: Option<Value>,
    },
    WorkflowLifecycle {
        semantic_kind: CanonicalWorkflowSemanticKindV1,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        provider_reference: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        item_id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        parent_reference: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        list_reference: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        state: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        status: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        item_order: Option<u64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        revision: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        event_sequence: Option<u64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        content: Option<Value>,
    },
    Boundary {
        boundary_kind: CanonicalBoundaryKindV1,
    },
    Unknown {
        native_kind: String,
        state: CanonicalUnknownStateV1,
    },
}

impl CanonicalObservationFactV1 {
    /// Returns the structured payload carried by facts that own one.
    pub fn content(&self) -> Option<&Value> {
        match self {
            Self::Message { content, .. }
            | Self::ToolResult { content, .. }
            | Self::ToolInvocation {
                arguments: content, ..
            } => Some(content),
            Self::Compaction { summary, .. }
            | Self::Reasoning {
                content: summary, ..
            }
            | Self::Git {
                content: summary, ..
            }
            | Self::Workflow {
                content: summary, ..
            }
            | Self::WorkflowLifecycle {
                content: summary, ..
            } => summary.as_ref(),
            Self::Session { .. }
            | Self::ProviderUsage { .. }
            | Self::UncorrelatedUsage { .. }
            | Self::Boundary { .. }
            | Self::Unknown { .. } => None,
        }
    }

    fn validate(&self) -> Result<(), ObservationContractError> {
        match self {
            Self::Session {
                started_at,
                ended_at,
                ..
            } => {
                if (*started_at)
                    .zip(*ended_at)
                    .is_some_and(|(start, end)| end < start)
                {
                    return Err(ObservationContractError::InvalidCanonicalOrderingEvidence);
                }
            }
            Self::ToolInvocation {
                invocation_id,
                name,
                ..
            } => {
                invocation_id
                    .validate()
                    .map_err(|_| ObservationContractError::InvalidNativeRecordIdentity)?;
                validate_canonical_label(name)?;
            }
            Self::ToolResult {
                invocation_id: Some(invocation_id),
                ..
            } => invocation_id
                .validate()
                .map_err(|_| ObservationContractError::InvalidNativeRecordIdentity)?,
            Self::ProviderUsage {
                model,
                counters,
                request_id,
                native_kind,
                native_field,
                ..
            } => {
                if let ProviderUsageModelV1::Known { model } = model {
                    validate_canonical_label(model)?;
                }
                if let Some(request_id) = request_id {
                    request_id
                        .validate()
                        .map_err(|_| ObservationContractError::InvalidNativeRecordIdentity)?;
                }
                validate_canonical_label(native_kind)?;
                validate_canonical_label(native_field)?;
                if let ProviderUsageCountersV1::Known {
                    input_tokens,
                    output_tokens,
                    cache_read_tokens,
                    cache_write_tokens,
                    reasoning_tokens,
                    total_tokens,
                } = counters
                    && [
                        input_tokens,
                        output_tokens,
                        cache_read_tokens,
                        cache_write_tokens,
                        reasoning_tokens,
                        total_tokens,
                    ]
                    .into_iter()
                    .all(Option::is_none)
                {
                    return Err(ObservationContractError::InvalidCanonicalPayload);
                }
            }
            Self::UncorrelatedUsage {
                native_kind,
                native_field,
                missing_dimensions,
                ..
            } => {
                validate_canonical_label(native_kind)?;
                validate_canonical_label(native_field)?;
                if missing_dimensions.is_empty() {
                    return Err(ObservationContractError::InvalidCanonicalPayload);
                }
            }
            Self::Reasoning {
                visibility,
                content,
            } if (*visibility == CanonicalReasoningVisibilityV1::Visible) != content.is_some() => {
                return Err(ObservationContractError::InvalidReasoningVisibility);
            }
            Self::Git { reference, .. } | Self::Workflow { reference, .. } => {
                if let Some(reference) = reference {
                    validate_canonical_label(reference)?;
                }
            }
            Self::WorkflowLifecycle {
                provider_reference,
                item_id,
                parent_reference,
                list_reference,
                state,
                status,
                revision,
                ..
            } => {
                for value in [
                    provider_reference,
                    item_id,
                    parent_reference,
                    list_reference,
                    state,
                    status,
                    revision,
                ]
                .into_iter()
                .flatten()
                {
                    validate_canonical_label(value)?;
                }
            }
            Self::Unknown { native_kind, .. } => validate_canonical_label(native_kind)?,
            Self::Message { .. }
            | Self::ToolResult { .. }
            | Self::Compaction { .. }
            | Self::Reasoning { .. }
            | Self::Boundary { .. } => {}
        }
        Ok(())
    }
}

fn validate_canonical_label(value: &str) -> Result<(), ObservationContractError> {
    if value.trim().is_empty() || value.len() > 256 || value.chars().any(char::is_control) {
        return Err(ObservationContractError::InvalidCanonicalRecordKind);
    }
    Ok(())
}

#[derive(
    Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq, PartialOrd, Ord, Hash,
)]
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

#[derive(
    JsonSchema, Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash,
)]
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

#[derive(
    JsonSchema, Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash,
)]
#[serde(rename_all = "snake_case")]
pub enum SensitivityV1 {
    Unclassified,
    NonSensitive,
    Sensitive,
    Secret,
}

impl SensitivityV1 {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Unclassified => "unclassified",
            Self::NonSensitive => "non_sensitive",
            Self::Sensitive => "sensitive",
            Self::Secret => "secret",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ReceiptDomainV1 {
    Claude,
    Observation,
}

impl ReceiptDomainV1 {
    fn for_identity(identity: &ObservationIdentityMaterialV1) -> Self {
        if is_default_observation_provider(identity.source().provider()) {
            Self::Claude
        } else {
            Self::Observation
        }
    }

    fn digest_domain(self) -> &'static [u8] {
        match self {
            Self::Claude => CLAUDE_RECEIPT_ID_DOMAIN,
            Self::Observation => OBSERVATION_RECEIPT_ID_DOMAIN,
        }
    }

    fn id_prefix(self) -> &'static str {
        match self {
            Self::Claude => CLAUDE_RECEIPT_ID_PREFIX,
            Self::Observation => OBSERVATION_RECEIPT_ID_PREFIX,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CanonicalClaudeSanitizationReceiptMaterialV1 {
    receipt_domain: ReceiptDomainV1,
    sanitizer_version: ComponentVersion,
    observation_id: CanonicalObservationIdV1,
    disposition: SanitizerDispositionV1,
    sensitivity: SensitivityV1,
    raw_digest: [u8; 32],
    sanitized_payload_digest: Option<PayloadDigestV1>,
}

impl CanonicalClaudeSanitizationReceiptMaterialV1 {
    pub fn for_durable_payload(
        identity: &ClaudeObservationIdentityMaterialV1,
        sanitizer_version: ComponentVersion,
        disposition: SanitizerDispositionV1,
        raw_digest: &[u8; 32],
        sanitized_payload: &PayloadReferenceV1,
    ) -> Result<Self, ObservationContractError> {
        let sensitivity = match disposition {
            SanitizerDispositionV1::Accepted => SensitivityV1::NonSensitive,
            SanitizerDispositionV1::Redacted => SensitivityV1::Secret,
            SanitizerDispositionV1::Rejected | SanitizerDispositionV1::Quarantined => {
                return Err(ObservationContractError::ReceiptPayloadForbidden);
            }
        };
        Self::for_durable_payload_with_sensitivity(
            identity,
            sanitizer_version,
            disposition,
            sensitivity,
            raw_digest,
            sanitized_payload,
        )
    }

    pub fn for_durable_payload_with_sensitivity(
        identity: &ClaudeObservationIdentityMaterialV1,
        sanitizer_version: ComponentVersion,
        disposition: SanitizerDispositionV1,
        sensitivity: SensitivityV1,
        raw_digest: &[u8; 32],
        sanitized_payload: &PayloadReferenceV1,
    ) -> Result<Self, ObservationContractError> {
        if !disposition.permits_durable_payload() {
            return Err(ObservationContractError::ReceiptPayloadForbidden);
        }
        validate_receipt_sensitivity(disposition, sensitivity)?;
        let observation_id = CanonicalObservationIdV1::derive(identity)?;
        Ok(Self {
            receipt_domain: ReceiptDomainV1::for_identity(identity),
            sanitizer_version,
            observation_id,
            disposition,
            sensitivity,
            raw_digest: *raw_digest,
            sanitized_payload_digest: Some(sanitized_payload.digest().clone()),
        })
    }

    pub fn for_non_durable(
        identity: &ClaudeObservationIdentityMaterialV1,
        sanitizer_version: ComponentVersion,
        disposition: SanitizerDispositionV1,
        raw_digest: &[u8; 32],
    ) -> Result<Self, ObservationContractError> {
        Self::for_non_durable_with_sensitivity(
            identity,
            sanitizer_version,
            disposition,
            SensitivityV1::Sensitive,
            raw_digest,
        )
    }

    pub fn for_non_durable_with_sensitivity(
        identity: &ClaudeObservationIdentityMaterialV1,
        sanitizer_version: ComponentVersion,
        disposition: SanitizerDispositionV1,
        sensitivity: SensitivityV1,
        raw_digest: &[u8; 32],
    ) -> Result<Self, ObservationContractError> {
        if disposition.permits_durable_payload() {
            return Err(ObservationContractError::ReceiptPayloadRequired);
        }
        validate_receipt_sensitivity(disposition, sensitivity)?;
        let observation_id = CanonicalObservationIdV1::derive(identity)?;
        Ok(Self {
            receipt_domain: ReceiptDomainV1::for_identity(identity),
            sanitizer_version,
            observation_id,
            disposition,
            sensitivity,
            raw_digest: *raw_digest,
            sanitized_payload_digest: None,
        })
    }

    pub fn derive_receipt_ref(&self) -> Result<SanitizationReceiptRefV1, ObservationContractError> {
        let mut hasher = Sha256::new();
        update_hash_frame(&mut hasher, self.receipt_domain.digest_domain());
        update_hash_frame(&mut hasher, self.sanitizer_version.as_str().as_bytes());
        update_hash_frame(&mut hasher, self.observation_id.as_str().as_bytes());
        update_hash_frame(&mut hasher, self.disposition.as_str().as_bytes());
        update_hash_frame(&mut hasher, CLAUDE_RECEIPT_SENSITIVITY_DOMAIN);
        update_hash_frame(&mut hasher, self.sensitivity.as_str().as_bytes());
        update_hash_frame(&mut hasher, CLAUDE_RECEIPT_RAW_DIGEST_DOMAIN);
        update_hash_frame(&mut hasher, &self.raw_digest);
        if let Some(payload_digest) = &self.sanitized_payload_digest {
            update_hash_frame(&mut hasher, CLAUDE_RECEIPT_SANITIZED_PAYLOAD_DOMAIN);
            update_hash_frame(&mut hasher, payload_digest.as_str().as_bytes());
        } else {
            update_hash_frame(&mut hasher, CLAUDE_RECEIPT_NO_PAYLOAD_DOMAIN);
        }
        let receipt_id = SanitizationReceiptId::new(format!(
            "{}{}",
            self.receipt_domain.id_prefix(),
            crate::canonical_text::encode_lowercase_hex(&hasher.finalize())
        ))
        .map_err(|_| ObservationContractError::InvalidReceiptReference)?;
        SanitizationReceiptRefV1::new(receipt_id, self.sanitizer_version.clone())
            .map_err(|_| ObservationContractError::InvalidReceiptReference)
    }
}

fn validate_receipt_sensitivity(
    disposition: SanitizerDispositionV1,
    sensitivity: SensitivityV1,
) -> Result<(), ObservationContractError> {
    if sensitivity == SensitivityV1::Unclassified {
        return Err(ObservationContractError::UnclassifiedPayload);
    }
    if disposition == SanitizerDispositionV1::Accepted && sensitivity == SensitivityV1::Secret {
        return Err(ObservationContractError::SecretPayloadAccepted);
    }
    Ok(())
}

fn update_hash_frame(hasher: &mut Sha256, value: &[u8]) {
    hasher.update((value.len() as u64).to_be_bytes());
    hasher.update(value);
}

#[derive(Clone, Debug, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
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
        validate_receipt_sensitivity(disposition, sensitivity)?;
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

/// Durable provider observation that can only be built from receipt-bound content.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DurableObservationV1 {
    observation_id: CanonicalObservationIdV1,
    identity: ObservationIdentityMaterialV1,
    receipt: SanitizationReceiptV1,
    retention_class: RetentionClass,
    payload: Value,
}

impl DurableObservationV1 {
    pub fn new(
        identity: ObservationIdentityMaterialV1,
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

    pub fn identity(&self) -> &ObservationIdentityMaterialV1 {
        &self.identity
    }

    pub fn source(&self) -> &ObservationSourceIdentityV1 {
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

impl Serialize for DurableObservationV1 {
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

impl<'de> Deserialize<'de> for DurableObservationV1 {
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
        let mut observation = Self::new(
            wire.identity,
            wire.receipt,
            wire.retention_class,
            wire.payload,
        )
        .map_err(serde::de::Error::custom)?;
        let accepted =
            accepted_identity_digests(&observation.observation_id, &observation.identity)
                .map_err(serde::de::Error::custom)?;
        if !accepted.contains(&expected_observation_id) {
            return Err(serde::de::Error::custom(
                ObservationContractError::ObservationIdentityMismatch,
            ));
        }
        if !accepted.contains(&expected_idempotency_key) {
            return Err(serde::de::Error::custom(
                ObservationContractError::IdempotencyKeyMismatch,
            ));
        }
        // Carry the id the row actually stores, not the one just re-derived.
        //
        // `new` derives the current form, which is right for a fresh
        // observation and wrong for a decoded one: a row written under an
        // earlier derivation is keyed by that earlier digest, in its own
        // `observation_id` column and in every row that joins to it. Handing
        // callers a different id than the row is keyed by makes each of them
        // responsible for knowing the derivation history, and the storage
        // audit's column-versus-JSON comparison failed for exactly that
        // reason. Keeping the accepted digest here also makes decode/encode
        // round-trip, so re-serializing a legacy row cannot silently restate
        // its identity.
        observation.observation_id = expected_observation_id;
        Ok(observation)
    }
}

/// Compatibility name for durable Claude observations.
pub type DurableClaudeObservationV1 = DurableObservationV1;

/// Relationship between an existing record and a candidate retry.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "snake_case")]
pub enum ObservationCollisionOutcomeV1 {
    Distinct,
    ExactDuplicate,
    IdentityCollision,
}

pub fn classify_observation_collision(
    existing: &DurableObservationV1,
    candidate: &DurableObservationV1,
) -> ObservationCollisionOutcomeV1 {
    if existing.observation_id != candidate.observation_id {
        ObservationCollisionOutcomeV1::Distinct
    } else if existing.payload_reference() == candidate.payload_reference() {
        ObservationCollisionOutcomeV1::ExactDuplicate
    } else {
        ObservationCollisionOutcomeV1::IdentityCollision
    }
}

/// Whether `candidate` is the current canonical form of a retained Codex or
/// Cursor record written before route-only source context was removed.
///
/// The compatibility is deliberately directional and field-bounded. It first
/// binds both payloads to the same native record identity, then permits only
/// the exact source-context fields those providers formerly synthesized. Any
/// authored fact, role, content, native timestamp, or unrelated relation still
/// differs after normalization and therefore remains an identity collision.
pub fn is_canonical_payload_revision_replay(
    existing: &DurableObservationV1,
    candidate: &DurableObservationV1,
) -> bool {
    let existing_identity = existing.identity();
    let candidate_identity = candidate.identity();
    let Some(native_record_id) = existing_identity.native_record_id() else {
        return false;
    };
    if existing.observation_id() != candidate.observation_id()
        || existing_identity.source() != candidate_identity.source()
        || existing_identity.scope() != candidate_identity.scope()
        || existing_identity.ordering_domain() != candidate_identity.ordering_domain()
        || candidate_identity.native_record_id() != Some(native_record_id)
        || existing.retention_class() != candidate.retention_class()
        || existing.receipt().disposition() != candidate.receipt().disposition()
        || existing.receipt().sensitivity() != candidate.receipt().sensitivity()
        || existing.receipt().receipt().sanitizer_version()
            != candidate.receipt().receipt().sanitizer_version()
    {
        return false;
    }

    let Ok(existing_envelope) =
        serde_json::from_value::<CanonicalObservationEnvelopeV1>(existing.payload().clone())
    else {
        return false;
    };
    let Ok(candidate_envelope) =
        serde_json::from_value::<CanonicalObservationEnvelopeV1>(candidate.payload().clone())
    else {
        return false;
    };
    if serde_json::to_value(&existing_envelope).ok().as_ref() != Some(existing.payload())
        || serde_json::to_value(&candidate_envelope).ok().as_ref() != Some(candidate.payload())
        || existing_envelope.provider() != existing_identity.source().provider()
        || candidate_envelope.provider() != candidate_identity.source().provider()
        || existing_envelope.stable_record_id() != native_record_id
        || candidate_envelope.stable_record_id() != native_record_id
        || existing_envelope.provider() != candidate_envelope.provider()
    {
        return false;
    }

    let mut normalized = existing.payload().clone();
    let current = candidate.payload();
    let legacy_context_changed = match candidate_envelope.provider().as_str() {
        "codex" => normalize_codex_payload_revision(&mut normalized, current),
        "cursor" => normalize_cursor_payload_revision(&mut normalized, current),
        _ => false,
    };
    legacy_context_changed && normalized == *current
}

fn normalize_codex_payload_revision(existing: &mut Value, current: &Value) -> bool {
    let (Some(existing_object), Some(current_object)) =
        (existing.as_object_mut(), current.as_object())
    else {
        return false;
    };
    let mut changed =
        remove_legacy_relation_when_current_absent(existing_object, current_object, "turn_id");
    let (Some(existing_facts), Some(current_facts)) = (
        existing_object
            .get_mut("facts")
            .and_then(Value::as_array_mut),
        current_object.get("facts").and_then(Value::as_array),
    ) else {
        return false;
    };
    if existing_facts.len() != current_facts.len() {
        return false;
    }
    for (existing_fact, current_fact) in existing_facts.iter_mut().zip(current_facts) {
        let (Some(existing_fact), Some(current_fact)) =
            (existing_fact.as_object_mut(), current_fact.as_object())
        else {
            return false;
        };
        if existing_fact.get("kind") != current_fact.get("kind") {
            return false;
        }
        match current_fact.get("kind").and_then(Value::as_str) {
            Some("session") => {
                if current_fact.contains_key("transcript_path") {
                    return false;
                }
                for key in ["project_path", "location_path", "transcript_path"] {
                    changed |= replace_with_current_field(existing_fact, current_fact, key);
                }
            }
            Some("provider_usage") if is_unknown_absent_model(current_fact.get("model")) => {
                changed |= replace_with_current_field(existing_fact, current_fact, "model");
            }
            Some("uncorrelated_usage")
                if current_adds_only_missing_model(existing_fact, current_fact) =>
            {
                changed |=
                    replace_with_current_field(existing_fact, current_fact, "missing_dimensions");
            }
            _ => {}
        }
    }
    changed
}

fn normalize_cursor_payload_revision(existing: &mut Value, current: &Value) -> bool {
    let (Some(existing_object), Some(current_object)) =
        (existing.as_object_mut(), current.as_object())
    else {
        return false;
    };
    let mut changed =
        remove_legacy_relation_when_current_absent(existing_object, current_object, "thread_id");
    let (Some(existing_facts), Some(current_facts)) = (
        existing_object
            .get_mut("facts")
            .and_then(Value::as_array_mut),
        current_object.get("facts").and_then(Value::as_array),
    ) else {
        return false;
    };
    if existing_facts.len() != current_facts.len() {
        return false;
    }
    for (existing_fact, current_fact) in existing_facts.iter_mut().zip(current_facts) {
        let (Some(existing_fact), Some(current_fact)) =
            (existing_fact.as_object_mut(), current_fact.as_object())
        else {
            return false;
        };
        if existing_fact.get("kind") != current_fact.get("kind") {
            return false;
        }
        if current_fact.get("kind").and_then(Value::as_str) == Some("message") {
            for key in ["model", "timestamp"] {
                if !current_fact.contains_key(key) {
                    changed |= existing_fact.remove(key).is_some();
                }
            }
        }
    }
    let (Some(existing_evidence), Some(current_evidence)) = (
        existing_object
            .get_mut("evidence")
            .and_then(Value::as_object_mut),
        current_object.get("evidence").and_then(Value::as_object),
    ) else {
        return false;
    };
    if !current_evidence.contains_key("native_timestamp") {
        changed |= existing_evidence.remove("native_timestamp").is_some();
    }
    changed
}

fn remove_legacy_relation_when_current_absent(
    existing: &mut serde_json::Map<String, Value>,
    current: &serde_json::Map<String, Value>,
    relation: &str,
) -> bool {
    let Some(existing_relations) = existing.get_mut("relations").and_then(Value::as_object_mut)
    else {
        return false;
    };
    let Some(current_relations) = current.get("relations").and_then(Value::as_object) else {
        return false;
    };
    !current_relations.contains_key(relation) && existing_relations.remove(relation).is_some()
}

fn replace_with_current_field(
    existing: &mut serde_json::Map<String, Value>,
    current: &serde_json::Map<String, Value>,
    key: &str,
) -> bool {
    let current_value = current.get(key).cloned();
    if existing.get(key) == current_value.as_ref() {
        return false;
    }
    match current_value {
        Some(value) => {
            existing.insert(key.to_owned(), value);
        }
        None => {
            existing.remove(key);
        }
    }
    true
}

fn is_unknown_absent_model(model: Option<&Value>) -> bool {
    model.is_some_and(|model| {
        model.get("state").and_then(Value::as_str) == Some("unknown")
            && model.get("reason").and_then(Value::as_str) == Some("absent")
    })
}

fn current_adds_only_missing_model(
    existing: &serde_json::Map<String, Value>,
    current: &serde_json::Map<String, Value>,
) -> bool {
    let (Some(existing_dimensions), Some(current_dimensions)) = (
        existing.get("missing_dimensions").and_then(Value::as_array),
        current.get("missing_dimensions").and_then(Value::as_array),
    ) else {
        return false;
    };
    let mut expected = existing_dimensions.clone();
    expected.push(Value::String("model".to_owned()));
    expected.sort_by(|left, right| left.as_str().cmp(&right.as_str()));
    let mut actual = current_dimensions.clone();
    actual.sort_by(|left, right| left.as_str().cmp(&right.as_str()));
    expected == actual
}

fn validate_sha256(value: &str, field: &'static str) -> Result<(), ObservationContractError> {
    let valid = crate::canonical_text::is_tagged_lowercase_hex(value, "sha256:", 64);
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
    tracing::trace!(
        target: "tracedecay::observation_admission_work",
        work = "identity_derivation",
        "derive canonical observation identity"
    );
    let bytes =
        canonical_json_bytes(value).map_err(|_| ObservationContractError::CanonicalEncoding)?;
    let mut hasher = Sha256::new();
    hasher.update(domain);
    hasher.update(bytes);
    Ok(format_sha256(&hasher.finalize()))
}

/// Every digest this identity material has legitimately produced, newest
/// first. Element zero is the only one ever written; the rest exist so rows
/// committed under an earlier derivation stay decodable.
///
/// A stored row carries this digest under two names — `observation_id` and its
/// `idempotency_key` alias, see [`DurableObservationV1::idempotency_key`] — so
/// the two fields must accept exactly the same set. Accepting an older entry
/// grants nothing: every one digests the same identity material under a domain
/// separator, so a row still binds to its own evidence. Rejecting them makes
/// committed rows permanently undecodable, and nothing downstream can
/// quarantine a row that will not decode.
///
/// A new derivation goes at the front of this list and nowhere else.
///
/// `current` is the caller's already-derived id rather than a re-derivation,
/// because the warm-up authority audit runs this once per row over the whole
/// `observations` table.
fn accepted_identity_digests(
    current: &CanonicalObservationIdV1,
    material: &ClaudeObservationIdentityMaterialV1,
) -> Result<[CanonicalObservationIdV1; 3], ObservationContractError> {
    let provider_domain = if is_default_observation_provider(material.source().provider()) {
        CLAUDE_OBSERVATION_ID_DOMAIN
    } else {
        OBSERVATION_ID_DOMAIN
    };
    Ok([
        current.clone(),
        CanonicalObservationIdV1::new(domain_digest(provider_domain, material)?)?,
        CanonicalObservationIdV1::new(domain_digest(LEGACY_IDEMPOTENCY_KEY_DOMAIN, material)?)?,
    ])
}

fn sha256_digest(bytes: &[u8]) -> String {
    tracing::trace!(
        target: "tracedecay::observation_admission_work",
        work = "payload_digest",
        "digest canonical observation payload"
    );
    format_sha256(&Sha256::digest(bytes))
}

fn format_sha256(digest: &[u8]) -> String {
    crate::canonical_text::encode_tagged_lowercase_hex("sha256:", digest)
}
