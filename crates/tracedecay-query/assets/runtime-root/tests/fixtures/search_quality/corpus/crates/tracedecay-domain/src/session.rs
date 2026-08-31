//! Pure session and temporal-retrieval contracts.
//!
//! These values carry identity, temporal, authority, coverage, and compact
//! context metadata only. Persistence, policy, hydration, and query execution
//! remain outside the domain crate.

use std::collections::BTreeSet;
use std::fmt::{self, Write};

use serde::{Deserialize, Deserializer, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::observation::{CanonicalMessageRoleV1, CanonicalObservationIdV1};
use crate::research::{
    AgentInstanceId, ComponentVersion, DataVersionDigest, EvidenceClass, MessageId, ObservationId,
    RetrievalAnchorId, SanitizationReceiptRefV1, SessionId, ThreadId, TurnId, UtcMicros,
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
                if value.is_empty()
                    || value.trim() != value
                    || value.len() > 512
                    || value.chars().any(char::is_control)
                {
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
        let digest = hasher.finalize();
        let mut encoded = String::with_capacity(71);
        encoded.push_str("sha256:");
        for byte in digest {
            write!(&mut encoded, "{byte:02x}").expect("writing to a String cannot fail");
        }
        Self(encoded)
    }

    pub fn new(value: impl Into<String>) -> Result<Self, SessionContractError> {
        let value = value.into();
        let valid = value.strip_prefix("sha256:").is_some_and(|encoded| {
            encoded.len() == 64
                && encoded
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        });
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
    EvidenceSpan,
    EvidenceBurst,
}

impl RetrievalGrainV1 {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Occurrence => "occurrence",
            Self::LogicalMessage => "logical_message",
            Self::Turn => "turn",
            Self::Session => "session",
            Self::Thread => "thread",
            Self::Agent => "agent",
            Self::Summary => "summary",
            Self::EvidenceSpan => "evidence_span",
            Self::EvidenceBurst => "evidence_burst",
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

/// Exact source-time horizon covered by an immutable summary.
#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(deny_unknown_fields)]
pub struct SummarySourceHorizonV1 {
    pub knowledge_through: UtcMicros,
    pub valid_through: Option<UtcMicros>,
}

impl SummarySourceHorizonV1 {
    pub fn validate(self) -> Result<(), SessionContractError> {
        Ok(())
    }
}

impl<'de> Deserialize<'de> for SummarySourceHorizonV1 {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Wire {
            knowledge_through: UtcMicros,
            #[serde(deserialize_with = "deserialize_required_option")]
            valid_through: Option<UtcMicros>,
        }

        fn deserialize_required_option<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
        where
            D: Deserializer<'de>,
            T: Deserialize<'de>,
        {
            Option::deserialize(deserializer)
        }

        let wire = Wire::deserialize(deserializer)?;
        let horizon = Self {
            knowledge_through: wire.knowledge_through,
            valid_through: wire.valid_through,
        };
        horizon.validate().map_err(serde::de::Error::custom)?;
        Ok(horizon)
    }
}

/// Publication metadata that binds a summary to its route and sanitization.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SummaryPublicationMetadataV1 {
    pub model_route: ComponentVersion,
    pub configuration_digest: DataVersionDigest,
    pub sanitization_receipt: SanitizationReceiptRefV1,
}

impl SummaryPublicationMetadataV1 {
    pub fn validate(&self) -> Result<(), SessionContractError> {
        self.model_route
            .validate()
            .and_then(|_| self.configuration_digest.validate())
            .and_then(|_| self.sanitization_receipt.validate())
            .map_err(|_| SessionContractError::InvalidIdentity {
                field: "summary publication metadata",
            })
    }
}

/// Immutable summary node with exact, identity-unique source anchors.
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SessionSummaryRecordV1 {
    summary_id: SessionSummaryIdV1,
    session_id: SessionId,
    summary_anchor_id: RetrievalAnchorId,
    source_anchors: Vec<RetrievalAnchorId>,
    source_horizon: SummarySourceHorizonV1,
    created_at: UtcMicros,
    predecessor_summary_id: Option<SessionSummaryIdV1>,
    publication: Option<SummaryPublicationMetadataV1>,
}

impl SessionSummaryRecordV1 {
    pub fn new(
        summary_id: SessionSummaryIdV1,
        session_id: SessionId,
        summary_anchor_id: RetrievalAnchorId,
        source_anchors: Vec<RetrievalAnchorId>,
        source_horizon: SummarySourceHorizonV1,
        created_at: UtcMicros,
    ) -> Result<Self, SessionContractError> {
        if source_anchors.is_empty() {
            return Err(SessionContractError::SummarySourcesRequired);
        }
        let mut unique = BTreeSet::new();
        if source_anchors
            .iter()
            .any(|source| !unique.insert(source.clone()))
        {
            return Err(SessionContractError::DuplicateSummarySource);
        }
        if created_at < source_horizon.knowledge_through {
            return Err(SessionContractError::InvalidSummaryHorizon);
        }
        source_horizon.validate()?;
        session_id
            .validate()
            .and_then(|_| summary_anchor_id.validate())
            .map_err(|_| SessionContractError::InvalidIdentity {
                field: "session summary",
            })?;
        for source in &source_anchors {
            source
                .validate()
                .map_err(|_| SessionContractError::InvalidIdentity {
                    field: "session summary source anchor",
                })?;
        }
        let source_anchors = unique.into_iter().collect();
        Ok(Self {
            summary_id,
            session_id,
            summary_anchor_id,
            source_anchors,
            source_horizon,
            created_at,
            predecessor_summary_id: None,
            publication: None,
        })
    }

    pub fn with_predecessor(
        mut self,
        predecessor: SessionSummaryIdV1,
    ) -> Result<Self, SessionContractError> {
        if self.summary_id == predecessor {
            return Err(SessionContractError::SummarySelfPredecessor);
        }
        self.predecessor_summary_id = Some(predecessor);
        Ok(self)
    }

    pub fn with_publication(
        mut self,
        publication: SummaryPublicationMetadataV1,
    ) -> Result<Self, SessionContractError> {
        publication.validate()?;
        self.publication = Some(publication);
        Ok(self)
    }

    pub fn summary_id(&self) -> &SessionSummaryIdV1 {
        &self.summary_id
    }

    pub fn session_id(&self) -> &SessionId {
        &self.session_id
    }

    pub fn summary_anchor_id(&self) -> &RetrievalAnchorId {
        &self.summary_anchor_id
    }

    pub fn source_anchors(&self) -> &[RetrievalAnchorId] {
        &self.source_anchors
    }

    pub fn source_horizon(&self) -> SummarySourceHorizonV1 {
        self.source_horizon
    }

    pub fn created_at(&self) -> UtcMicros {
        self.created_at
    }

    pub fn predecessor_summary_id(&self) -> Option<&SessionSummaryIdV1> {
        self.predecessor_summary_id.as_ref()
    }

    pub fn publication(&self) -> Option<&SummaryPublicationMetadataV1> {
        self.publication.as_ref()
    }
}

impl<'de> Deserialize<'de> for SessionSummaryRecordV1 {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Wire {
            summary_id: SessionSummaryIdV1,
            session_id: SessionId,
            summary_anchor_id: RetrievalAnchorId,
            source_anchors: Vec<RetrievalAnchorId>,
            source_horizon: SummarySourceHorizonV1,
            created_at: UtcMicros,
            predecessor_summary_id: Option<SessionSummaryIdV1>,
            publication: Option<SummaryPublicationMetadataV1>,
        }

        let wire = Wire::deserialize(deserializer)?;
        let mut summary = Self::new(
            wire.summary_id,
            wire.session_id,
            wire.summary_anchor_id,
            wire.source_anchors,
            wire.source_horizon,
            wire.created_at,
        )
        .map_err(serde::de::Error::custom)?;
        if let Some(predecessor) = wire.predecessor_summary_id {
            summary = summary
                .with_predecessor(predecessor)
                .map_err(serde::de::Error::custom)?;
        }
        if let Some(publication) = wire.publication {
            summary = summary
                .with_publication(publication)
                .map_err(serde::de::Error::custom)?;
        }
        Ok(summary)
    }
}

/// Current hydration eligibility after authorization and retention rechecks.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "snake_case")]
pub enum HydrationStateV1 {
    Available,
    RetainedButUnavailable,
    Redacted,
    Deleted,
    RetentionExpired,
    Unauthorized,
    Locked,
    UnverifiableLegacy,
}

impl HydrationStateV1 {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Available => "available",
            Self::RetainedButUnavailable => "retained_but_unavailable",
            Self::Redacted => "redacted",
            Self::Deleted => "deleted",
            Self::RetentionExpired => "retention_expired",
            Self::Unauthorized => "unauthorized",
            Self::Locked => "locked",
            Self::UnverifiableLegacy => "unverifiable_legacy",
        }
    }
}

/// Why an otherwise relevant item was omitted from compact context.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "snake_case")]
pub enum ContextOmissionReasonV1 {
    ByteBudget,
    TokenBudget,
    Unauthorized,
    Redacted,
    Deleted,
    RetentionExpired,
    Locked,
    Unavailable,
    SummaryHorizonMismatch,
    DuplicateRepresentative,
}

impl ContextOmissionReasonV1 {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ByteBudget => "byte_budget",
            Self::TokenBudget => "token_budget",
            Self::Unauthorized => "unauthorized",
            Self::Redacted => "redacted",
            Self::Deleted => "deleted",
            Self::RetentionExpired => "retention_expired",
            Self::Locked => "locked",
            Self::Unavailable => "unavailable",
            Self::SummaryHorizonMismatch => "summary_horizon_mismatch",
            Self::DuplicateRepresentative => "duplicate_representative",
        }
    }
}

/// One selected context item. Payload text remains behind the exact anchor.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CompactContextRecordV1 {
    pub anchor_id: RetrievalAnchorId,
    pub grain: RetrievalGrainV1,
    pub hydration: HydrationStateV1,
    pub encoded_bytes: u64,
}

impl CompactContextRecordV1 {
    pub fn validate(&self) -> Result<(), SessionContractError> {
        self.anchor_id
            .validate()
            .map_err(|_| SessionContractError::InvalidIdentity {
                field: "compact context record anchor",
            })
    }
}

/// One explicit compact-context omission.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CompactContextOmissionV1 {
    pub anchor_id: Option<RetrievalAnchorId>,
    pub reason: ContextOmissionReasonV1,
}

impl CompactContextOmissionV1 {
    pub fn validate(&self) -> Result<(), SessionContractError> {
        if let Some(anchor_id) = &self.anchor_id {
            anchor_id
                .validate()
                .map_err(|_| SessionContractError::InvalidIdentity {
                    field: "compact context omission anchor",
                })?;
        }
        Ok(())
    }
}

/// One conflict retained in compact context instead of silently selecting a side.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CompactContextConflictV1 {
    pub anchor_id: RetrievalAnchorId,
    pub supporting_anchor_ids: BTreeSet<RetrievalAnchorId>,
}

impl CompactContextConflictV1 {
    pub fn validate(&self) -> Result<(), SessionContractError> {
        self.anchor_id
            .validate()
            .map_err(|_| SessionContractError::InvalidIdentity {
                field: "compact context conflict anchor",
            })?;
        for anchor_id in &self.supporting_anchor_ids {
            anchor_id
                .validate()
                .map_err(|_| SessionContractError::InvalidIdentity {
                    field: "compact context conflict supporting anchor",
                })?;
        }
        Ok(())
    }
}

/// One typed temporal edge needed to interpret compact-context evolution.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CompactContextLineageEdgeV1 {
    pub kind: TemporalAssertionKindV1,
    pub subject_anchor_id: RetrievalAnchorId,
    pub object_anchor_id: RetrievalAnchorId,
    pub knowledge_at: UtcMicros,
    pub authority: SessionAuthorityClassV1,
    pub authorized: bool,
    pub supporting_anchor_ids: BTreeSet<RetrievalAnchorId>,
}

impl CompactContextLineageEdgeV1 {
    pub fn validate(&self) -> Result<(), SessionContractError> {
        if self.subject_anchor_id == self.object_anchor_id {
            return Err(SessionContractError::AssertionSelfReference);
        }
        for (field, anchor_id) in [
            ("compact context lineage subject", &self.subject_anchor_id),
            ("compact context lineage object", &self.object_anchor_id),
        ] {
            anchor_id
                .validate()
                .map_err(|_| SessionContractError::InvalidIdentity { field })?;
        }
        for anchor_id in &self.supporting_anchor_ids {
            anchor_id
                .validate()
                .map_err(|_| SessionContractError::InvalidIdentity {
                    field: "compact context lineage supporting anchor",
                })?;
        }
        Ok(())
    }
}

/// Anchor-only compact-context assembly result.
#[derive(Clone, Debug, Default, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CompactContextBundleV1 {
    pub records: Vec<CompactContextRecordV1>,
    pub omissions: Vec<CompactContextOmissionV1>,
    pub continuation_anchors: Vec<RetrievalAnchorId>,
    pub coverage: TemporalCoverageCountsV1,
    pub conflicts: Vec<CompactContextConflictV1>,
    pub lineage: Vec<CompactContextLineageEdgeV1>,
    pub encoded_bytes: u64,
}

impl CompactContextBundleV1 {
    pub fn validate(&self) -> Result<(), SessionContractError> {
        let mut anchors = BTreeSet::new();
        let mut encoded_bytes = 0_u64;
        for record in &self.records {
            record.validate()?;
            if !anchors.insert(record.anchor_id.clone()) {
                return Err(SessionContractError::DuplicateContextAnchor);
            }
            encoded_bytes = encoded_bytes
                .checked_add(record.encoded_bytes)
                .ok_or(SessionContractError::CompactContextEncodedBytesOverflow)?;
        }
        for anchor in &self.continuation_anchors {
            anchor
                .validate()
                .map_err(|_| SessionContractError::InvalidIdentity {
                    field: "compact context continuation anchor",
                })?;
            if !anchors.insert(anchor.clone()) {
                return Err(SessionContractError::DuplicateContextAnchor);
            }
        }
        for omission in &self.omissions {
            omission.validate()?;
            if let Some(anchor_id) = &omission.anchor_id
                && !anchors.insert(anchor_id.clone())
            {
                return Err(SessionContractError::DuplicateContextAnchor);
            }
        }
        for conflict in &self.conflicts {
            conflict.validate()?;
        }
        for edge in &self.lineage {
            edge.validate()?;
        }
        if self.encoded_bytes != encoded_bytes {
            return Err(SessionContractError::CompactContextEncodedBytesMismatch);
        }
        Ok(())
    }
}

impl<'de> Deserialize<'de> for CompactContextBundleV1 {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Wire {
            records: Vec<CompactContextRecordV1>,
            omissions: Vec<CompactContextOmissionV1>,
            continuation_anchors: Vec<RetrievalAnchorId>,
            #[serde(default)]
            coverage: TemporalCoverageCountsV1,
            #[serde(default)]
            conflicts: Vec<CompactContextConflictV1>,
            #[serde(default)]
            lineage: Vec<CompactContextLineageEdgeV1>,
            encoded_bytes: u64,
        }

        let wire = Wire::deserialize(deserializer)?;
        let bundle = Self {
            records: wire.records,
            omissions: wire.omissions,
            continuation_anchors: wire.continuation_anchors,
            coverage: wire.coverage,
            conflicts: wire.conflicts,
            lineage: wire.lineage,
            encoded_bytes: wire.encoded_bytes,
        };
        bundle.validate().map_err(serde::de::Error::custom)?;
        Ok(bundle)
    }
}

/// Representative-view counts that complement shard-level [`crate::CoverageReportV1`].
#[derive(
    Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash,
)]
#[serde(deny_unknown_fields)]
pub struct TemporalCoverageCountsV1 {
    pub visible: u64,
    pub hidden: u64,
    pub unknown: u64,
    pub redacted: u64,
}

impl TemporalCoverageCountsV1 {
    pub const fn total(self) -> Option<u64> {
        match self.visible.checked_add(self.hidden) {
            Some(total) => match total.checked_add(self.unknown) {
                Some(total) => total.checked_add(self.redacted),
                None => None,
            },
            None => None,
        }
    }

    pub const fn has_withheld_or_unknown(self) -> bool {
        self.hidden != 0 || self.unknown != 0 || self.redacted != 0
    }
}

/// Monotonic provider or projector position for one session source.
#[derive(
    Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash,
)]
#[serde(transparent)]
pub struct SessionSourceFrontierV1(u64);

impl SessionSourceFrontierV1 {
    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    pub const fn value(self) -> u64 {
        self.0
    }

    pub const fn lag_from(self, target: Self) -> u64 {
        target.0.saturating_sub(self.0)
    }
}

/// Closed time interval on one temporal axis.
#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(deny_unknown_fields)]
pub struct ClosedUtcIntervalV1 {
    from_inclusive: Option<UtcMicros>,
    through_inclusive: Option<UtcMicros>,
}

impl ClosedUtcIntervalV1 {
    pub fn new(
        from_inclusive: Option<UtcMicros>,
        through_inclusive: Option<UtcMicros>,
    ) -> Result<Self, SessionContractError> {
        if from_inclusive.is_none() && through_inclusive.is_none() {
            return Err(SessionContractError::EmptyCoverageInterval);
        }
        if matches!(
            (from_inclusive, through_inclusive),
            (Some(from), Some(through)) if from > through
        ) {
            return Err(SessionContractError::ReversedCoverageInterval);
        }
        Ok(Self {
            from_inclusive,
            through_inclusive,
        })
    }

    pub const fn from_inclusive(self) -> Option<UtcMicros> {
        self.from_inclusive
    }

    pub const fn through_inclusive(self) -> Option<UtcMicros> {
        self.through_inclusive
    }
}

impl<'de> Deserialize<'de> for ClosedUtcIntervalV1 {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Wire {
            from_inclusive: Option<UtcMicros>,
            through_inclusive: Option<UtcMicros>,
        }

        let wire = Wire::deserialize(deserializer)?;
        Self::new(wire.from_inclusive, wire.through_inclusive).map_err(serde::de::Error::custom)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(tag = "kind", content = "interval", rename_all = "snake_case")]
pub enum ValidCoverageIntervalV1 {
    Known(ClosedUtcIntervalV1),
    Unknown,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(deny_unknown_fields)]
pub struct SessionSourceCoverageIntervalV1 {
    pub knowledge: ClosedUtcIntervalV1,
    pub valid: ValidCoverageIntervalV1,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(deny_unknown_fields)]
pub struct SessionTemporalCoverageRequestV1 {
    mode: TemporalModeV1,
}

impl SessionTemporalCoverageRequestV1 {
    pub const fn new(mode: TemporalModeV1) -> Self {
        Self { mode }
    }

    pub const fn mode(&self) -> TemporalModeV1 {
        self.mode
    }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "snake_case")]
pub enum SessionSourceCoverageStateV1 {
    Fresh,
    Stale,
    Partial,
    Locked,
    Redacted,
    RetentionWithheld,
    Unavailable,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SessionSourceCoverageReasonV1 {
    CaughtUp,
    ProjectionBehindSource {
        lag: u64,
    },
    SourceBehindTarget {
        lag: u64,
    },
    ProjectionAndSourceBehind {
        projection_lag: u64,
        source_lag: u64,
    },
    Locked,
    Redacted,
    RetentionWithheld,
    Unavailable,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SessionSourceCoverageV1 {
    source_id: SessionSourceIdV1,
    observed_frontier: SessionSourceFrontierV1,
    committed_frontier: SessionSourceFrontierV1,
    target_watermark: SessionSourceFrontierV1,
    request: SessionTemporalCoverageRequestV1,
    covered_intervals: Vec<SessionSourceCoverageIntervalV1>,
    missing_intervals: Vec<SessionSourceCoverageIntervalV1>,
    state: SessionSourceCoverageStateV1,
    reason: SessionSourceCoverageReasonV1,
}

impl SessionSourceCoverageV1 {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        source_id: SessionSourceIdV1,
        observed_frontier: SessionSourceFrontierV1,
        committed_frontier: SessionSourceFrontierV1,
        target_watermark: SessionSourceFrontierV1,
        request: SessionTemporalCoverageRequestV1,
        mut covered_intervals: Vec<SessionSourceCoverageIntervalV1>,
        mut missing_intervals: Vec<SessionSourceCoverageIntervalV1>,
        state: SessionSourceCoverageStateV1,
        reason: SessionSourceCoverageReasonV1,
    ) -> Result<Self, SessionContractError> {
        if committed_frontier > observed_frontier {
            return Err(SessionContractError::InvalidSourceCoverageFrontiers);
        }
        covered_intervals.sort();
        missing_intervals.sort();
        if has_duplicate_intervals(&covered_intervals)
            || has_duplicate_intervals(&missing_intervals)
            || covered_intervals.iter().any(|covered| {
                missing_intervals
                    .iter()
                    .any(|missing| coverage_intervals_touch_or_overlap(covered, missing))
            })
            || !coverage_state_matches_reason(state, &reason)
        {
            return Err(if coverage_state_matches_reason(state, &reason) {
                SessionContractError::NonCanonicalCoverageIntervals
            } else {
                SessionContractError::InvalidSourceCoverageState
            });
        }
        Ok(Self {
            source_id,
            observed_frontier,
            committed_frontier,
            target_watermark,
            request,
            covered_intervals,
            missing_intervals,
            state,
            reason,
        })
    }

    pub fn from_frontiers(
        source_id: SessionSourceIdV1,
        observed_frontier: SessionSourceFrontierV1,
        committed_frontier: SessionSourceFrontierV1,
        target_watermark: SessionSourceFrontierV1,
        request: SessionTemporalCoverageRequestV1,
    ) -> Result<Self, SessionContractError> {
        let projection_lag = committed_frontier.lag_from(observed_frontier);
        let source_lag = observed_frontier.lag_from(target_watermark);
        let (state, reason) = match (projection_lag, source_lag) {
            (0, 0) => (
                SessionSourceCoverageStateV1::Fresh,
                SessionSourceCoverageReasonV1::CaughtUp,
            ),
            (0, lag) => (
                SessionSourceCoverageStateV1::Partial,
                SessionSourceCoverageReasonV1::SourceBehindTarget { lag },
            ),
            (lag, 0) => (
                SessionSourceCoverageStateV1::Stale,
                SessionSourceCoverageReasonV1::ProjectionBehindSource { lag },
            ),
            (projection_lag, source_lag) => (
                SessionSourceCoverageStateV1::Partial,
                SessionSourceCoverageReasonV1::ProjectionAndSourceBehind {
                    projection_lag,
                    source_lag,
                },
            ),
        };
        Self::new(
            source_id,
            observed_frontier,
            committed_frontier,
            target_watermark,
            request,
            Vec::new(),
            Vec::new(),
            state,
            reason,
        )
    }

    pub fn source_id(&self) -> &SessionSourceIdV1 {
        &self.source_id
    }

    pub const fn observed_frontier(&self) -> SessionSourceFrontierV1 {
        self.observed_frontier
    }

    pub const fn committed_frontier(&self) -> SessionSourceFrontierV1 {
        self.committed_frontier
    }

    pub const fn target_watermark(&self) -> SessionSourceFrontierV1 {
        self.target_watermark
    }

    pub fn request(&self) -> &SessionTemporalCoverageRequestV1 {
        &self.request
    }

    pub fn covered_intervals(&self) -> &[SessionSourceCoverageIntervalV1] {
        &self.covered_intervals
    }

    pub fn missing_intervals(&self) -> &[SessionSourceCoverageIntervalV1] {
        &self.missing_intervals
    }

    pub const fn state(&self) -> SessionSourceCoverageStateV1 {
        self.state
    }

    pub fn reason(&self) -> &SessionSourceCoverageReasonV1 {
        &self.reason
    }

    pub const fn frontier_lag(&self) -> u64 {
        self.target_watermark
            .0
            .saturating_sub(self.committed_frontier.0)
    }
}

impl<'de> Deserialize<'de> for SessionSourceCoverageV1 {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Wire {
            source_id: SessionSourceIdV1,
            observed_frontier: SessionSourceFrontierV1,
            committed_frontier: SessionSourceFrontierV1,
            target_watermark: SessionSourceFrontierV1,
            request: SessionTemporalCoverageRequestV1,
            covered_intervals: Vec<SessionSourceCoverageIntervalV1>,
            missing_intervals: Vec<SessionSourceCoverageIntervalV1>,
            state: SessionSourceCoverageStateV1,
            reason: SessionSourceCoverageReasonV1,
        }

        let wire = Wire::deserialize(deserializer)?;
        Self::new(
            wire.source_id,
            wire.observed_frontier,
            wire.committed_frontier,
            wire.target_watermark,
            wire.request,
            wire.covered_intervals,
            wire.missing_intervals,
            wire.state,
            wire.reason,
        )
        .map_err(serde::de::Error::custom)
    }
}

fn has_duplicate_intervals(intervals: &[SessionSourceCoverageIntervalV1]) -> bool {
    intervals.iter().enumerate().any(|(index, left)| {
        intervals[index + 1..]
            .iter()
            .any(|right| coverage_intervals_touch_or_overlap(left, right))
    })
}

fn coverage_intervals_touch_or_overlap(
    left: &SessionSourceCoverageIntervalV1,
    right: &SessionSourceCoverageIntervalV1,
) -> bool {
    intervals_touch_or_overlap(left.knowledge, right.knowledge)
        && valid_intervals_touch_or_overlap(&left.valid, &right.valid)
}

fn valid_intervals_touch_or_overlap(
    left: &ValidCoverageIntervalV1,
    right: &ValidCoverageIntervalV1,
) -> bool {
    match (left, right) {
        (ValidCoverageIntervalV1::Unknown, ValidCoverageIntervalV1::Unknown) => true,
        (ValidCoverageIntervalV1::Known(left), ValidCoverageIntervalV1::Known(right)) => {
            intervals_touch_or_overlap(*left, *right)
        }
        _ => false,
    }
}

fn intervals_touch_or_overlap(left: ClosedUtcIntervalV1, right: ClosedUtcIntervalV1) -> bool {
    let left_from = left.from_inclusive.map_or(i64::MIN, |value| value.0);
    let left_through = left.through_inclusive.map_or(i64::MAX, |value| value.0);
    let right_from = right.from_inclusive.map_or(i64::MIN, |value| value.0);
    let right_through = right.through_inclusive.map_or(i64::MAX, |value| value.0);
    left_from <= right_through.saturating_add(1) && right_from <= left_through.saturating_add(1)
}

fn coverage_state_matches_reason(
    state: SessionSourceCoverageStateV1,
    reason: &SessionSourceCoverageReasonV1,
) -> bool {
    matches!(
        (state, reason),
        (
            SessionSourceCoverageStateV1::Fresh,
            SessionSourceCoverageReasonV1::CaughtUp
        ) | (
            SessionSourceCoverageStateV1::Stale,
            SessionSourceCoverageReasonV1::ProjectionBehindSource { .. }
        ) | (
            SessionSourceCoverageStateV1::Partial,
            SessionSourceCoverageReasonV1::SourceBehindTarget { .. }
                | SessionSourceCoverageReasonV1::ProjectionAndSourceBehind { .. }
        ) | (
            SessionSourceCoverageStateV1::Locked,
            SessionSourceCoverageReasonV1::Locked
        ) | (
            SessionSourceCoverageStateV1::Redacted,
            SessionSourceCoverageReasonV1::Redacted
        ) | (
            SessionSourceCoverageStateV1::RetentionWithheld,
            SessionSourceCoverageReasonV1::RetentionWithheld
        ) | (
            SessionSourceCoverageStateV1::Unavailable,
            SessionSourceCoverageReasonV1::Unavailable
        )
    )
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "snake_case")]
pub enum SessionSourceCoverageAggregateStateV1 {
    Fresh,
    Stale,
    Partial,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SessionSourceCoverageReceiptV1 {
    request: SessionTemporalCoverageRequestV1,
    sources: Vec<SessionSourceCoverageV1>,
    aggregate_state: SessionSourceCoverageAggregateStateV1,
}

impl SessionSourceCoverageReceiptV1 {
    pub fn new(
        request: SessionTemporalCoverageRequestV1,
        mut sources: Vec<SessionSourceCoverageV1>,
    ) -> Result<Self, SessionContractError> {
        if sources.is_empty() {
            return Err(SessionContractError::SourceCoverageRequired);
        }
        sources.sort_by(|left, right| left.source_id.cmp(&right.source_id));
        if sources
            .windows(2)
            .any(|pair| pair[0].source_id == pair[1].source_id)
        {
            return Err(SessionContractError::DuplicateSourceCoverage);
        }
        if sources.iter().any(|source| source.request != request) {
            return Err(SessionContractError::SourceCoverageRequestMismatch);
        }
        let all_fresh = sources
            .iter()
            .all(|source| source.state == SessionSourceCoverageStateV1::Fresh);
        let all_stale = sources
            .iter()
            .all(|source| source.state == SessionSourceCoverageStateV1::Stale);
        let aggregate_state = if all_fresh {
            SessionSourceCoverageAggregateStateV1::Fresh
        } else if all_stale {
            SessionSourceCoverageAggregateStateV1::Stale
        } else {
            SessionSourceCoverageAggregateStateV1::Partial
        };
        Ok(Self {
            request,
            sources,
            aggregate_state,
        })
    }

    pub fn request(&self) -> &SessionTemporalCoverageRequestV1 {
        &self.request
    }

    pub fn sources(&self) -> &[SessionSourceCoverageV1] {
        &self.sources
    }

    pub const fn aggregate_state(&self) -> SessionSourceCoverageAggregateStateV1 {
        self.aggregate_state
    }

    pub fn max_frontier_lag(&self) -> u64 {
        self.sources
            .iter()
            .map(SessionSourceCoverageV1::frontier_lag)
            .max()
            .unwrap_or(0)
    }
}

impl<'de> Deserialize<'de> for SessionSourceCoverageReceiptV1 {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Wire {
            request: SessionTemporalCoverageRequestV1,
            sources: Vec<SessionSourceCoverageV1>,
            aggregate_state: SessionSourceCoverageAggregateStateV1,
        }

        let wire = Wire::deserialize(deserializer)?;
        let receipt = Self::new(wire.request, wire.sources).map_err(serde::de::Error::custom)?;
        if receipt.aggregate_state != wire.aggregate_state {
            return Err(serde::de::Error::custom(
                SessionContractError::InvalidSourceCoverageState,
            ));
        }
        Ok(receipt)
    }
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(deny_unknown_fields)]
pub struct SessionRefreshSourceTargetV1 {
    source_id: SessionSourceIdV1,
    observed_frontier: SessionSourceFrontierV1,
    target_watermark: SessionSourceFrontierV1,
}

impl SessionRefreshSourceTargetV1 {
    pub fn new(
        source_id: SessionSourceIdV1,
        observed_frontier: SessionSourceFrontierV1,
        target_watermark: SessionSourceFrontierV1,
    ) -> Result<Self, SessionContractError> {
        if target_watermark < observed_frontier {
            return Err(SessionContractError::InvalidRefreshSourceFrontier);
        }
        Ok(Self {
            source_id,
            observed_frontier,
            target_watermark,
        })
    }

    pub fn source_id(&self) -> &SessionSourceIdV1 {
        &self.source_id
    }

    pub const fn observed_frontier(&self) -> SessionSourceFrontierV1 {
        self.observed_frontier
    }

    pub const fn target_watermark(&self) -> SessionSourceFrontierV1 {
        self.target_watermark
    }
}

impl<'de> Deserialize<'de> for SessionRefreshSourceTargetV1 {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Wire {
            source_id: SessionSourceIdV1,
            observed_frontier: SessionSourceFrontierV1,
            target_watermark: SessionSourceFrontierV1,
        }

        let wire = Wire::deserialize(deserializer)?;
        Self::new(
            wire.source_id,
            wire.observed_frontier,
            wire.target_watermark,
        )
        .map_err(serde::de::Error::custom)
    }
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq, Hash)]
#[serde(deny_unknown_fields)]
pub struct SessionRefreshKeyV1 {
    store_root_id: String,
    session_id: SessionId,
    sources: Vec<SessionRefreshSourceTargetV1>,
    projector_version: String,
    configuration_digest: String,
}

impl SessionRefreshKeyV1 {
    pub fn new(
        store_root_id: impl Into<String>,
        session_id: SessionId,
        mut sources: Vec<SessionRefreshSourceTargetV1>,
        projector_version: impl Into<String>,
        configuration_digest: impl Into<String>,
    ) -> Result<Self, SessionContractError> {
        let store_root_id = canonical_component(store_root_id.into(), "store_root_id")?;
        let projector_version = canonical_component(projector_version.into(), "projector_version")?;
        let configuration_digest =
            canonical_component(configuration_digest.into(), "configuration_digest")?;
        if sources.is_empty() {
            return Err(SessionContractError::RefreshSourcesRequired);
        }
        sources.sort();
        if sources
            .windows(2)
            .any(|pair| pair[0].source_id == pair[1].source_id)
        {
            return Err(SessionContractError::DuplicateRefreshSource);
        }
        Ok(Self {
            store_root_id,
            session_id,
            sources,
            projector_version,
            configuration_digest,
        })
    }

    pub fn sources(&self) -> &[SessionRefreshSourceTargetV1] {
        &self.sources
    }

    pub fn store_root_id(&self) -> &str {
        &self.store_root_id
    }

    pub fn session_id(&self) -> &SessionId {
        &self.session_id
    }

    pub fn projector_version(&self) -> &str {
        &self.projector_version
    }

    pub fn configuration_digest(&self) -> &str {
        &self.configuration_digest
    }
}

impl<'de> Deserialize<'de> for SessionRefreshKeyV1 {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Wire {
            store_root_id: String,
            session_id: SessionId,
            sources: Vec<SessionRefreshSourceTargetV1>,
            projector_version: String,
            configuration_digest: String,
        }

        let wire = Wire::deserialize(deserializer)?;
        Self::new(
            wire.store_root_id,
            wire.session_id,
            wire.sources,
            wire.projector_version,
            wire.configuration_digest,
        )
        .map_err(serde::de::Error::custom)
    }
}

fn canonical_component(value: String, field: &'static str) -> Result<String, SessionContractError> {
    if value.is_empty()
        || value.trim() != value
        || value.len() > 512
        || value.chars().any(char::is_control)
    {
        return Err(SessionContractError::InvalidIdentity { field });
    }
    Ok(value)
}

#[cfg(test)]
mod source_freshness_tests {
    use super::*;

    fn source(value: &str) -> SessionSourceIdV1 {
        SessionSourceIdV1::new(value).unwrap()
    }

    #[test]
    fn validity_intervals_reject_empty_and_reversed_bounds() {
        assert_eq!(
            ClosedUtcIntervalV1::new(None, None),
            Err(SessionContractError::EmptyCoverageInterval)
        );
        assert_eq!(
            ClosedUtcIntervalV1::new(Some(UtcMicros(20)), Some(UtcMicros(10))),
            Err(SessionContractError::ReversedCoverageInterval)
        );

        let request = SessionTemporalCoverageRequestV1::new(TemporalModeV1::Current);
        let interval = |from, through| SessionSourceCoverageIntervalV1 {
            knowledge: ClosedUtcIntervalV1::new(Some(UtcMicros(from)), Some(UtcMicros(through)))
                .unwrap(),
            valid: ValidCoverageIntervalV1::Unknown,
        };
        assert_eq!(
            SessionSourceCoverageV1::new(
                source("cursor"),
                SessionSourceFrontierV1::new(10),
                SessionSourceFrontierV1::new(10),
                SessionSourceFrontierV1::new(10),
                request,
                vec![interval(1, 5), interval(6, 10)],
                Vec::new(),
                SessionSourceCoverageStateV1::Fresh,
                SessionSourceCoverageReasonV1::CaughtUp,
            ),
            Err(SessionContractError::NonCanonicalCoverageIntervals)
        );
    }

    #[test]
    fn source_freshness_is_derived_from_observed_projected_and_target_frontiers() {
        let request = SessionTemporalCoverageRequestV1::new(TemporalModeV1::Current);
        let stale = SessionSourceCoverageV1::from_frontiers(
            source("cursor"),
            SessionSourceFrontierV1::new(10),
            SessionSourceFrontierV1::new(8),
            SessionSourceFrontierV1::new(10),
            request.clone(),
        )
        .unwrap();
        assert_eq!(stale.state(), SessionSourceCoverageStateV1::Stale);
        assert_eq!(
            stale.reason(),
            &SessionSourceCoverageReasonV1::ProjectionBehindSource { lag: 2 }
        );

        let partial = SessionSourceCoverageV1::from_frontiers(
            source("claude"),
            SessionSourceFrontierV1::new(8),
            SessionSourceFrontierV1::new(8),
            SessionSourceFrontierV1::new(10),
            request,
        )
        .unwrap();
        assert_eq!(partial.state(), SessionSourceCoverageStateV1::Partial);
        assert_eq!(
            partial.reason(),
            &SessionSourceCoverageReasonV1::SourceBehindTarget { lag: 2 }
        );
    }

    #[test]
    fn aggregate_receipt_preserves_sources_and_mixed_freshness() {
        let request = SessionTemporalCoverageRequestV1::new(TemporalModeV1::Current);
        let receipt = SessionSourceCoverageReceiptV1::new(
            request.clone(),
            vec![
                SessionSourceCoverageV1::from_frontiers(
                    source("cursor"),
                    SessionSourceFrontierV1::new(10),
                    SessionSourceFrontierV1::new(10),
                    SessionSourceFrontierV1::new(10),
                    request.clone(),
                )
                .unwrap(),
                SessionSourceCoverageV1::from_frontiers(
                    source("claude"),
                    SessionSourceFrontierV1::new(10),
                    SessionSourceFrontierV1::new(7),
                    SessionSourceFrontierV1::new(10),
                    request,
                )
                .unwrap(),
            ],
        )
        .unwrap();

        assert_eq!(receipt.sources().len(), 2);
        assert_eq!(
            receipt.aggregate_state(),
            SessionSourceCoverageAggregateStateV1::Partial
        );
        assert_eq!(receipt.max_frontier_lag(), 3);
    }

    #[test]
    fn refresh_key_canonicalizes_sources_and_round_trips() {
        let target = |name: &str| {
            SessionRefreshSourceTargetV1::new(
                source(name),
                SessionSourceFrontierV1::new(8),
                SessionSourceFrontierV1::new(10),
            )
            .unwrap()
        };
        let key = SessionRefreshKeyV1::new(
            "root.1",
            SessionId::new("session.1").unwrap(),
            vec![target("cursor"), target("claude")],
            "projector.v1",
            "sha256:configuration",
        )
        .unwrap();
        assert_eq!(key.sources()[0].source_id().as_str(), "claude");
        let encoded = serde_json::to_string(&key).unwrap();
        assert_eq!(
            serde_json::from_str::<SessionRefreshKeyV1>(&encoded).unwrap(),
            key
        );
    }
}
