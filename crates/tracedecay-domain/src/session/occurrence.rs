//! Pure session and temporal-retrieval contracts.
//!
//! These values carry identity, temporal, authority, coverage, and compact
//! context metadata only. Persistence, policy, hydration, and query execution
//! remain outside the domain crate.

use std::fmt;

use serde::{Deserialize, Deserializer, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::observation::{CanonicalMessageRoleV1, CanonicalObservationIdV1};
use crate::research::{
    AgentInstanceId, ComponentVersion, EvidenceClass, MessageId, ObservationId, RetrievalAnchorId,
    SanitizationReceiptRefV1, SessionId, ThreadId, TurnId, UtcMicros,
};

const MESSAGE_OCCURRENCE_ID_DOMAIN: &[u8] = b"tracedecay.session.message-occurrence.v1\0";

/// Validation failures at the session-domain boundary.
#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum SessionContractError {
    #[error("{field} is not a canonical identity")]
    InvalidIdentity { field: &'static str },
    #[error("{field} must be non-zero")]
    ZeroValue { field: &'static str },
    #[error("a byte range must be non-empty and ordered")]
    InvalidByteRange,
    #[error("message occurrence identity does not match its observation and ordinal")]
    OccurrenceIdentityMismatch,
    #[error("a logical copy cannot reference itself")]
    CopySelfReference,
    #[error("copy proof does not identify the copied-from occurrence")]
    CopyProofSourceMismatch,
    #[error("a temporal assertion cannot relate an anchor to itself")]
    AssertionSelfReference,
    #[error("a session summary requires at least one exact source anchor")]
    SummarySourcesRequired,
    #[error("a session summary source anchor is duplicated")]
    DuplicateSummarySource,
    #[error("a session summary cannot predate its knowledge horizon")]
    InvalidSummaryHorizon,
    #[error("a session summary cannot name itself as predecessor")]
    SummarySelfPredecessor,
    #[error("{group} grouping provenance requires a corresponding identity")]
    GroupingProvenanceWithoutId { group: &'static str },
    #[error("{group} identity requires grouping provenance")]
    GroupingIdWithoutProvenance { group: &'static str },
    #[error("compact context contains a duplicate anchor")]
    DuplicateContextAnchor,
    #[error("compact context encoded bytes do not match its records")]
    CompactContextEncodedBytesMismatch,
    #[error("compact context record bytes overflow the aggregate")]
    CompactContextEncodedBytesOverflow,
    #[error("a temporal coverage interval requires at least one bound")]
    EmptyCoverageInterval,
    #[error("a temporal coverage interval has reversed bounds")]
    ReversedCoverageInterval,
    #[error("source coverage intervals are not canonical")]
    NonCanonicalCoverageIntervals,
    #[error("source coverage frontiers are inconsistent")]
    InvalidSourceCoverageFrontiers,
    #[error("source coverage state and reason disagree")]
    InvalidSourceCoverageState,
    #[error("a source coverage receipt requires at least one source")]
    SourceCoverageRequired,
    #[error("a source coverage receipt contains duplicate sources")]
    DuplicateSourceCoverage,
    #[error("source coverage does not match the receipt request")]
    SourceCoverageRequestMismatch,
    #[error("a refresh key requires at least one source target")]
    RefreshSourcesRequired,
    #[error("a refresh key contains duplicate source targets")]
    DuplicateRefreshSource,
    #[error("a refresh source target regresses its observed frontier")]
    InvalidRefreshSourceFrontier,
    #[error("derived evidence requires at least one ordered member")]
    DerivedEvidenceMembersRequired,
    #[error("derived evidence contains a duplicate occurrence member")]
    DuplicateDerivedEvidenceMember,
    #[error("derived evidence membership ordinals are not contiguous")]
    NoncontiguousDerivedEvidenceOrdinals,
    #[error("derived evidence endpoints do not match the ordered manifest")]
    DerivedEvidenceEndpointMismatch,
    #[error("derived evidence session identity mismatches its members")]
    DerivedEvidenceSessionMismatch,
    #[error("derived evidence authority must be derived_projection")]
    DerivedEvidenceAuthorityMismatch,
    #[error("derived evidence member digest does not match membership")]
    DerivedEvidenceMemberDigestMismatch,
}

macro_rules! session_string_id {
    ($($name:ident),+ $(,)?) => {$(
        #[derive(Clone, Debug, Serialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
        #[serde(transparent)]
        pub struct $name(String);

        impl $name {
            pub fn new(value: impl Into<String>) -> Result<Self, SessionContractError> {
                let value = value.into();
                if !$crate::canonical_text::is_canonical_text_within(
                    &value,
                    $crate::canonical_text::CANONICAL_TEXT_MAX_BYTES,
                ) {
                    return Err(SessionContractError::InvalidIdentity {
                        field: stringify!($name),
                    });
                }
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
                Self::new(String::deserialize(deserializer)?)
                    .map_err(serde::de::Error::custom)
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str(&self.0)
            }
        }
    )+};
}

session_string_id!(
    SessionSummaryIdV1,
    TemporalAssertionIdV1,
    SessionRefreshOperationIdV1,
    SessionCursorKeyIdV1,
    SessionSourceIdV1,
);

/// Stable identity of one projected output from a canonical observation.
#[derive(Clone, Debug, Serialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(transparent)]
pub struct MessageOccurrenceIdV1(String);

impl MessageOccurrenceIdV1 {
    pub fn derive(
        observation_id: &CanonicalObservationIdV1,
        output_ordinal: ProjectionOutputOrdinalV1,
    ) -> Self {
        let mut hasher = Sha256::new();
        hasher.update(MESSAGE_OCCURRENCE_ID_DOMAIN);
        hasher.update(observation_id.as_str().as_bytes());
        hasher.update(output_ordinal.value().to_be_bytes());
        Self(crate::canonical_text::encode_tagged_lowercase_hex(
            "sha256:",
            &hasher.finalize(),
        ))
    }

    pub fn new(value: impl Into<String>) -> Result<Self, SessionContractError> {
        let value = value.into();
        let valid = crate::canonical_text::is_tagged_lowercase_hex(&value, "sha256:", 64);
        if !valid {
            return Err(SessionContractError::InvalidIdentity {
                field: "MessageOccurrenceIdV1",
            });
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl<'de> Deserialize<'de> for MessageOccurrenceIdV1 {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        Self::new(String::deserialize(deserializer)?).map_err(serde::de::Error::custom)
    }
}

impl fmt::Display for MessageOccurrenceIdV1 {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// Zero-based output position within one canonical observation projection.
#[derive(
    Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash,
)]
#[serde(transparent)]
pub struct ProjectionOutputOrdinalV1(u32);

impl ProjectionOutputOrdinalV1 {
    pub const fn new(value: u32) -> Self {
        Self(value)
    }

    pub const fn value(self) -> u32 {
        self.0
    }
}

macro_rules! nonzero_numeric_value {
    ($name:ident, $integer:ty, $field:literal) => {
        #[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
        #[serde(transparent)]
        pub struct $name($integer);

        impl $name {
            pub fn new(value: $integer) -> Result<Self, SessionContractError> {
                if value == 0 {
                    return Err(SessionContractError::ZeroValue { field: $field });
                }
                Ok(Self(value))
            }

            pub const fn value(self) -> $integer {
                self.0
            }
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
            where
                D: Deserializer<'de>,
            {
                Self::new(<$integer>::deserialize(deserializer)?).map_err(serde::de::Error::custom)
            }
        }
    };
}

nonzero_numeric_value!(
    SessionProjectionGenerationV1,
    u64,
    "session projection generation"
);
nonzero_numeric_value!(SessionCursorVersionV1, u16, "session cursor version");

/// Persisted signing-key reference used by authenticated collection cursors.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(deny_unknown_fields)]
pub struct SignedCursorKeyRefV1 {
    pub key_id: SessionCursorKeyIdV1,
    pub version: SessionCursorVersionV1,
}

/// Requested temporal interpretation.
#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum TemporalModeV1 {
    Current,
    AsOf { cutoff: UtcMicros },
    Evolution,
    Forensic,
}

impl TemporalModeV1 {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Current => "current",
            Self::AsOf { .. } => "as_of",
            Self::Evolution => "evolution",
            Self::Forensic => "forensic",
        }
    }
}

impl<'de> Deserialize<'de> for TemporalModeV1 {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
        enum Wire {
            Current {},
            AsOf { cutoff: UtcMicros },
            Evolution {},
            Forensic {},
        }

        Ok(match Wire::deserialize(deserializer)? {
            Wire::Current {} => Self::Current,
            Wire::AsOf { cutoff } => Self::AsOf { cutoff },
            Wire::Evolution {} => Self::Evolution,
            Wire::Forensic {} => Self::Forensic,
        })
    }
}

/// Retrieval unit selected by the caller.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "snake_case")]
pub enum RetrievalGrainV1 {
    Occurrence,
    LogicalMessage,
    Turn,
    Session,
    Thread,
    Agent,
    Summary,
}

impl RetrievalGrainV1 {
    /// Every variant, so exhaustive callers do not hand-maintain a list.
    pub const ALL: [Self; 7] = [
        Self::Occurrence,
        Self::LogicalMessage,
        Self::Turn,
        Self::Session,
        Self::Thread,
        Self::Agent,
        Self::Summary,
    ];

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Occurrence => "occurrence",
            Self::LogicalMessage => "logical_message",
            Self::Turn => "turn",
            Self::Session => "session",
            Self::Thread => "thread",
            Self::Agent => "agent",
            Self::Summary => "summary",
        }
    }
}

/// Canonical half-open UTF-8 byte range retained by exact retrieval evidence.
#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ByteRangeV1 {
    start: u64,
    end: u64,
}

impl ByteRangeV1 {
    pub const fn new(start: u64, end: u64) -> Result<Self, SessionContractError> {
        if start >= end {
            return Err(SessionContractError::InvalidByteRange);
        }
        Ok(Self { start, end })
    }

    pub const fn start(self) -> u64 {
        self.start
    }

    pub const fn end(self) -> u64 {
        self.end
    }
}

impl<'de> Deserialize<'de> for ByteRangeV1 {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct SerializedByteRange {
            start: u64,
            end: u64,
        }

        let range = SerializedByteRange::deserialize(deserializer)?;
        Self::new(range.start, range.end).map_err(serde::de::Error::custom)
    }
}

/// Valid-time evidence for an occurrence or assertion.
#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum TemporalValidityV1 {
    Known { valid_at: UtcMicros },
    Unknown,
}

impl TemporalValidityV1 {
    /// Whether evidence may participate in a representative answer for `mode`.
    ///
    /// `as_of` is intentionally strict: both knowledge and valid time must be
    /// at or before the cutoff, and unknown valid time is excluded.
    pub const fn is_representative_at(self, knowledge_at: UtcMicros, mode: TemporalModeV1) -> bool {
        match mode {
            TemporalModeV1::AsOf { cutoff } => match self {
                Self::Known { valid_at } => knowledge_at.0 <= cutoff.0 && valid_at.0 <= cutoff.0,
                Self::Unknown => false,
            },
            TemporalModeV1::Current | TemporalModeV1::Evolution | TemporalModeV1::Forensic => true,
        }
    }
}

impl<'de> Deserialize<'de> for TemporalValidityV1 {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
        enum Wire {
            Known { valid_at: UtcMicros },
            Unknown {},
        }

        Ok(match Wire::deserialize(deserializer)? {
            Wire::Known { valid_at } => Self::Known { valid_at },
            Wire::Unknown {} => Self::Unknown,
        })
    }
}

/// Whether a Turn/thread identity was observed or deterministically projected.
#[derive(Clone, Debug, Serialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum GroupingProvenanceV1 {
    ProviderNative,
    DerivedRoleBoundary { projector_version: ComponentVersion },
}

impl GroupingProvenanceV1 {
    pub fn validate(&self) -> Result<(), SessionContractError> {
        match self {
            Self::ProviderNative => Ok(()),
            Self::DerivedRoleBoundary { projector_version } => projector_version
                .validate()
                .map_err(|_| SessionContractError::InvalidIdentity {
                    field: "grouping projector version",
                }),
        }
    }
}

impl<'de> Deserialize<'de> for GroupingProvenanceV1 {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
        enum Wire {
            ProviderNative {},
            DerivedRoleBoundary { projector_version: ComponentVersion },
        }

        Ok(match Wire::deserialize(deserializer)? {
            Wire::ProviderNative {} => Self::ProviderNative,
            Wire::DerivedRoleBoundary { projector_version } => {
                Self::DerivedRoleBoundary { projector_version }
            }
        })
    }
}

/// Authority class attached to session evidence.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "snake_case")]
pub enum SessionAuthorityClassV1 {
    ProviderNative,
    CanonicalObservation,
    ExplicitAnchorAssertion,
    DerivedProjection,
    ImmutableSummary,
}

impl SessionAuthorityClassV1 {
    /// Every variant, so exhaustive callers do not hand-maintain a list.
    pub const ALL: [Self; 5] = [
        Self::ProviderNative,
        Self::CanonicalObservation,
        Self::ExplicitAnchorAssertion,
        Self::DerivedProjection,
        Self::ImmutableSummary,
    ];

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ProviderNative => "provider_native",
            Self::CanonicalObservation => "canonical_observation",
            Self::ExplicitAnchorAssertion => "explicit_anchor_assertion",
            Self::DerivedProjection => "derived_projection",
            Self::ImmutableSummary => "immutable_summary",
        }
    }
}

/// Exact evidence and sanitization references behind one session-domain row.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SessionEvidenceMetadataV1 {
    pub authority: SessionAuthorityClassV1,
    pub evidence_class: EvidenceClass,
    pub source_anchor_id: RetrievalAnchorId,
    pub sanitization_receipt: SanitizationReceiptRefV1,
}

impl SessionEvidenceMetadataV1 {
    pub fn validate(&self) -> Result<(), SessionContractError> {
        self.source_anchor_id
            .validate()
            .map_err(|_| SessionContractError::InvalidIdentity {
                field: "session evidence source anchor",
            })?;
        self.sanitization_receipt
            .validate()
            .map_err(|_| SessionContractError::InvalidIdentity {
                field: "session evidence sanitization receipt",
            })
    }
}

/// Immutable projected occurrence of one message-like observation output.
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct MessageOccurrenceRecordV1 {
    pub occurrence_id: MessageOccurrenceIdV1,
    pub source_observation_id: CanonicalObservationIdV1,
    pub projection_output_ordinal: ProjectionOutputOrdinalV1,
    pub retrieval_anchor_id: RetrievalAnchorId,
    pub session_id: SessionId,
    pub thread_id: Option<ThreadId>,
    pub thread_grouping: Option<GroupingProvenanceV1>,
    pub turn_id: Option<TurnId>,
    pub turn_grouping: Option<GroupingProvenanceV1>,
    pub message_id: Option<MessageId>,
    pub agent_id: Option<AgentInstanceId>,
    pub role: CanonicalMessageRoleV1,
    pub knowledge_at: UtcMicros,
    pub valid_time: TemporalValidityV1,
    pub evidence: SessionEvidenceMetadataV1,
}

impl MessageOccurrenceRecordV1 {
    pub fn validate(&self) -> Result<(), SessionContractError> {
        if self.occurrence_id
            != MessageOccurrenceIdV1::derive(
                &self.source_observation_id,
                self.projection_output_ordinal,
            )
        {
            return Err(SessionContractError::OccurrenceIdentityMismatch);
        }
        self.retrieval_anchor_id
            .validate()
            .map_err(|_| SessionContractError::InvalidIdentity {
                field: "occurrence retrieval anchor",
            })?;
        self.session_id
            .validate()
            .map_err(|_| SessionContractError::InvalidIdentity {
                field: "occurrence session",
            })?;
        for (field, value) in [
            (
                "occurrence thread",
                self.thread_id.as_ref().map(ThreadId::validate),
            ),
            (
                "occurrence turn",
                self.turn_id.as_ref().map(TurnId::validate),
            ),
            (
                "occurrence message",
                self.message_id.as_ref().map(MessageId::validate),
            ),
            (
                "occurrence agent",
                self.agent_id.as_ref().map(AgentInstanceId::validate),
            ),
        ] {
            if value.is_some_and(|result| result.is_err()) {
                return Err(SessionContractError::InvalidIdentity { field });
            }
        }
        for (group, id_present, provenance) in [
            (
                "thread",
                self.thread_id.is_some(),
                self.thread_grouping.as_ref(),
            ),
            ("turn", self.turn_id.is_some(), self.turn_grouping.as_ref()),
        ] {
            match (id_present, provenance) {
                (false, Some(_)) => {
                    return Err(SessionContractError::GroupingProvenanceWithoutId { group });
                }
                (true, None) => {
                    return Err(SessionContractError::GroupingIdWithoutProvenance { group });
                }
                (true, Some(provenance)) => provenance.validate()?,
                (false, None) => {}
            }
        }
        self.evidence.validate()
    }
}

impl<'de> Deserialize<'de> for MessageOccurrenceRecordV1 {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Wire {
            occurrence_id: MessageOccurrenceIdV1,
            source_observation_id: CanonicalObservationIdV1,
            projection_output_ordinal: ProjectionOutputOrdinalV1,
            retrieval_anchor_id: RetrievalAnchorId,
            session_id: SessionId,
            thread_id: Option<ThreadId>,
            thread_grouping: Option<GroupingProvenanceV1>,
            turn_id: Option<TurnId>,
            turn_grouping: Option<GroupingProvenanceV1>,
            message_id: Option<MessageId>,
            agent_id: Option<AgentInstanceId>,
            role: CanonicalMessageRoleV1,
            knowledge_at: UtcMicros,
            valid_time: TemporalValidityV1,
            evidence: SessionEvidenceMetadataV1,
        }

        let wire = Wire::deserialize(deserializer)?;
        let record = Self {
            occurrence_id: wire.occurrence_id,
            source_observation_id: wire.source_observation_id,
            projection_output_ordinal: wire.projection_output_ordinal,
            retrieval_anchor_id: wire.retrieval_anchor_id,
            session_id: wire.session_id,
            thread_id: wire.thread_id,
            thread_grouping: wire.thread_grouping,
            turn_id: wire.turn_id,
            turn_grouping: wire.turn_grouping,
            message_id: wire.message_id,
            agent_id: wire.agent_id,
            role: wire.role,
            knowledge_at: wire.knowledge_at,
            valid_time: wire.valid_time,
            evidence: wire.evidence,
        };
        record.validate().map_err(serde::de::Error::custom)?;
        Ok(record)
    }
}

/// Evidence that can prove two occurrences are logical copies.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum CopyProofV1 {
    ProviderLinkage {
        source_occurrence_id: MessageOccurrenceIdV1,
        provider_record_id: ObservationId,
    },
    ParentMessageLinkage {
        source_occurrence_id: MessageOccurrenceIdV1,
        parent_message_id: MessageId,
    },
    ExplicitAnchorAssertion {
        source_occurrence_id: MessageOccurrenceIdV1,
        assertion_anchor_id: RetrievalAnchorId,
    },
}

impl CopyProofV1 {
    pub fn source_occurrence_id(&self) -> &MessageOccurrenceIdV1 {
        match self {
            Self::ProviderLinkage {
                source_occurrence_id,
                ..
            }
            | Self::ParentMessageLinkage {
                source_occurrence_id,
                ..
            }
            | Self::ExplicitAnchorAssertion {
                source_occurrence_id,
                ..
            } => source_occurrence_id,
        }
    }
}

/// Immutable evidence-backed logical-copy edge.
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct LogicalCopyRecordV1 {
    pub occurrence_id: MessageOccurrenceIdV1,
    pub copied_from_occurrence_id: MessageOccurrenceIdV1,
    pub proof: CopyProofV1,
    /// When the copy edge became visible to the authoritative store/projection.
    pub knowledge_at: UtcMicros,
    /// Independent represented-world validity; legacy rows default to unknown.
    pub valid_time: TemporalValidityV1,
}

impl LogicalCopyRecordV1 {
    pub fn validate(&self) -> Result<(), SessionContractError> {
        if self.occurrence_id == self.copied_from_occurrence_id {
            return Err(SessionContractError::CopySelfReference);
        }
        if self.proof.source_occurrence_id() != &self.copied_from_occurrence_id {
            return Err(SessionContractError::CopyProofSourceMismatch);
        }
        Ok(())
    }
}

impl<'de> Deserialize<'de> for LogicalCopyRecordV1 {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Wire {
            occurrence_id: MessageOccurrenceIdV1,
            copied_from_occurrence_id: MessageOccurrenceIdV1,
            proof: CopyProofV1,
            #[serde(default)]
            knowledge_at: Option<UtcMicros>,
            #[serde(default)]
            valid_time: Option<TemporalValidityV1>,
        }

        let wire = Wire::deserialize(deserializer)?;
        let record = Self {
            occurrence_id: wire.occurrence_id,
            copied_from_occurrence_id: wire.copied_from_occurrence_id,
            proof: wire.proof,
            // Legacy copy wires omit bitemporal fields; preserve unknown validity
            // and a zero knowledge watermark rather than inventing provider time.
            knowledge_at: wire.knowledge_at.unwrap_or(UtcMicros(0)),
            valid_time: wire.valid_time.unwrap_or(TemporalValidityV1::Unknown),
        };
        record.validate().map_err(serde::de::Error::custom)?;
        Ok(record)
    }
}

/// Temporal relationship asserted between two exact anchors.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "snake_case")]
pub enum TemporalAssertionKindV1 {
    Corrects,
    Supersedes,
    Contradicts,
    Supports,
}

impl TemporalAssertionKindV1 {
    /// Every variant, so exhaustive callers do not hand-maintain a list.
    pub const ALL: [Self; 4] = [
        Self::Corrects,
        Self::Supersedes,
        Self::Contradicts,
        Self::Supports,
    ];

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Corrects => "corrects",
            Self::Supersedes => "supersedes",
            Self::Contradicts => "contradicts",
            Self::Supports => "supports",
        }
    }
}

/// Immutable temporal assertion over exact evidence anchors.
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct TemporalAssertionRecordV1 {
    pub assertion_id: TemporalAssertionIdV1,
    pub kind: TemporalAssertionKindV1,
    pub subject_anchor_id: RetrievalAnchorId,
    pub object_anchor_id: RetrievalAnchorId,
    pub knowledge_at: UtcMicros,
    pub valid_time: TemporalValidityV1,
    pub evidence: SessionEvidenceMetadataV1,
}

impl TemporalAssertionRecordV1 {
    pub fn validate(&self) -> Result<(), SessionContractError> {
        if self.subject_anchor_id == self.object_anchor_id {
            return Err(SessionContractError::AssertionSelfReference);
        }
        self.subject_anchor_id
            .validate()
            .and_then(|_| self.object_anchor_id.validate())
            .map_err(|_| SessionContractError::InvalidIdentity {
                field: "temporal assertion anchor",
            })?;
        self.evidence.validate()
    }
}

impl<'de> Deserialize<'de> for TemporalAssertionRecordV1 {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Wire {
            assertion_id: TemporalAssertionIdV1,
            kind: TemporalAssertionKindV1,
            subject_anchor_id: RetrievalAnchorId,
            object_anchor_id: RetrievalAnchorId,
            knowledge_at: UtcMicros,
            valid_time: TemporalValidityV1,
            evidence: SessionEvidenceMetadataV1,
        }

        let wire = Wire::deserialize(deserializer)?;
        let record = Self {
            assertion_id: wire.assertion_id,
            kind: wire.kind,
            subject_anchor_id: wire.subject_anchor_id,
            object_anchor_id: wire.object_anchor_id,
            knowledge_at: wire.knowledge_at,
            valid_time: wire.valid_time,
            evidence: wire.evidence,
        };
        record.validate().map_err(serde::de::Error::custom)?;
        Ok(record)
    }
}
