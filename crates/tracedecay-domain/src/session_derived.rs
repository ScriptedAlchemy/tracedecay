//! Generation-bound session-derived evidence spans and bursts.
//!
//! These contracts describe immutable, rebuildable projections over consecutive
//! message occurrences. They are not source authority, summaries, or carriers of
//! external GitHub/CI/diagnostic/Git/receipt/task payloads.

use std::collections::BTreeMap;
use std::fmt;

use serde::{Deserialize, Deserializer, Serialize};
use sha2::{Digest, Sha256};

use crate::canonical_text::{canonical_framed_sha256_bytes, encode_tagged_lowercase_hex};
use crate::research::{
    DataVersionDigest, MessageId, RetrievalAnchorId, SessionId, ThreadId, UtcMicros,
};
use crate::session::{
    MessageOccurrenceIdV1, SessionAuthorityClassV1, SessionContractError, SummarySourceHorizonV1,
};

const DERIVED_EVIDENCE_ID_DOMAIN: &[u8] = b"tracedecay.session.derived-evidence.v2";
const DERIVED_MEMBER_DIGEST_DOMAIN: &[u8] = b"tracedecay.session.derived-member-chain.v2";
const DERIVED_ANCHOR_DOMAIN: &[u8] = b"tracedecay.session.derived-anchor.v2";
const DERIVED_CONFIGURATION_DOMAIN: &[u8] = b"tracedecay.session.derived-configuration.v1\0";

/// Default versioned span window used by the generation projector.
pub const SESSION_DERIVED_SPAN_ALGORITHM_V1: &str = "session-derived-span-v1";
/// Default versioned burst adjacency policy used by the generation projector.
pub const SESSION_DERIVED_BURST_ALGORITHM_V1: &str = "session-derived-burst-v1";
/// Maximum members admitted into one actionable span under the default policy.
pub const SESSION_DERIVED_SPAN_MAX_MEMBERS_V1: usize = 32;

/// Kind of generation-bound derived evidence projected over occurrences.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "snake_case")]
pub enum DerivedEvidenceKindV1 {
    Span,
    Burst,
}

impl DerivedEvidenceKindV1 {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Span => "span",
            Self::Burst => "burst",
        }
    }

    pub const fn algorithm_version(self) -> &'static str {
        match self {
            Self::Span => SESSION_DERIVED_SPAN_ALGORITHM_V1,
            Self::Burst => SESSION_DERIVED_BURST_ALGORITHM_V1,
        }
    }
}

/// Opaque typed identity for one derived evidence record.
#[derive(Clone, Debug, Serialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(transparent)]
pub struct DerivedEvidenceIdV1(String);

impl DerivedEvidenceIdV1 {
    pub fn new(value: impl Into<String>) -> Result<Self, SessionContractError> {
        let value = value.into();
        if !is_sha256_identity(&value) {
            return Err(SessionContractError::InvalidIdentity {
                field: "DerivedEvidenceIdV1",
            });
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl<'de> Deserialize<'de> for DerivedEvidenceIdV1 {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        Self::new(String::deserialize(deserializer)?).map_err(serde::de::Error::custom)
    }
}

impl fmt::Display for DerivedEvidenceIdV1 {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// Span-specific identity wrapper over [`DerivedEvidenceIdV1`].
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(transparent)]
pub struct EvidenceSpanIdV1(DerivedEvidenceIdV1);

impl EvidenceSpanIdV1 {
    pub fn new(value: impl Into<String>) -> Result<Self, SessionContractError> {
        Ok(Self(DerivedEvidenceIdV1::new(value)?))
    }

    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }
}

/// One ordered member of a derived span or burst.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(deny_unknown_fields)]
pub struct DerivedEvidenceMemberV1 {
    pub ordinal: u32,
    pub occurrence_id: MessageOccurrenceIdV1,
    pub member_role: DerivedEvidenceMemberRoleV1,
}

impl DerivedEvidenceMemberV1 {
    pub fn new(
        ordinal: u32,
        occurrence_id: MessageOccurrenceIdV1,
        member_role: DerivedEvidenceMemberRoleV1,
    ) -> Self {
        Self {
            ordinal,
            occurrence_id,
            member_role,
        }
    }

    pub fn validate(&self) -> Result<(), SessionContractError> {
        Ok(())
    }
}

/// Member role within a derived span or burst.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "snake_case")]
pub enum DerivedEvidenceMemberRoleV1 {
    Member,
    First,
    Last,
}

impl DerivedEvidenceMemberRoleV1 {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Member => "member",
            Self::First => "first",
            Self::Last => "last",
        }
    }
}

/// One immutable version of a derived span or burst.
///
/// Members are stored beside the record, keyed by its kind and first member,
/// so extending a live run appends members instead of restating them.
/// `member_digest` chains every member in ordinal order and `evidence_id`
/// binds that chain to the member count, so a version is reproducible from
/// its ordered members alone.
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SessionDerivedEvidenceRecordV1 {
    evidence_id: DerivedEvidenceIdV1,
    evidence_kind: DerivedEvidenceKindV1,
    retrieval_anchor_id: RetrievalAnchorId,
    session_id: SessionId,
    thread_id: Option<ThreadId>,
    first_occurrence_id: MessageOccurrenceIdV1,
    last_occurrence_id: MessageOccurrenceIdV1,
    algorithm_version: String,
    configuration_digest: DataVersionDigest,
    member_count: u32,
    member_digest: DataVersionDigest,
    source_horizon: SummarySourceHorizonV1,
    authority: SessionAuthorityClassV1,
}

impl SessionDerivedEvidenceRecordV1 {
    pub fn validate(&self) -> Result<(), SessionContractError> {
        if self.authority != SessionAuthorityClassV1::DerivedProjection {
            return Err(SessionContractError::DerivedEvidenceAuthorityMismatch);
        }
        if self.member_count == 0 {
            return Err(SessionContractError::DerivedEvidenceMembersRequired);
        }
        if (self.member_count == 1) != (self.first_occurrence_id == self.last_occurrence_id) {
            return Err(SessionContractError::DerivedEvidenceEndpointMismatch);
        }
        self.source_horizon.validate()?;
        if self.algorithm_version != self.evidence_kind.algorithm_version() {
            return Err(SessionContractError::InvalidIdentity {
                field: "derived evidence algorithm_version",
            });
        }
        if derive_evidence_id(
            self.evidence_kind,
            &self.algorithm_version,
            &self.configuration_digest,
            &self.member_digest,
            self.member_count,
        )? != self.evidence_id
        {
            return Err(SessionContractError::InvalidIdentity {
                field: "DerivedEvidenceIdV1",
            });
        }
        if derive_derived_anchor_id(
            self.evidence_kind,
            &self.session_id,
            &self.configuration_digest,
            &self.member_digest,
        )? != self.retrieval_anchor_id
        {
            return Err(SessionContractError::InvalidIdentity {
                field: "derived evidence retrieval_anchor_id",
            });
        }
        Ok(())
    }

    pub fn evidence_id(&self) -> &DerivedEvidenceIdV1 {
        &self.evidence_id
    }

    pub const fn evidence_kind(&self) -> DerivedEvidenceKindV1 {
        self.evidence_kind
    }

    pub fn retrieval_anchor_id(&self) -> &RetrievalAnchorId {
        &self.retrieval_anchor_id
    }

    pub fn session_id(&self) -> &SessionId {
        &self.session_id
    }

    pub fn thread_id(&self) -> Option<&ThreadId> {
        self.thread_id.as_ref()
    }

    pub fn first_occurrence_id(&self) -> &MessageOccurrenceIdV1 {
        &self.first_occurrence_id
    }

    pub fn last_occurrence_id(&self) -> &MessageOccurrenceIdV1 {
        &self.last_occurrence_id
    }

    pub fn algorithm_version(&self) -> &str {
        &self.algorithm_version
    }

    pub fn configuration_digest(&self) -> &DataVersionDigest {
        &self.configuration_digest
    }

    pub const fn member_count(&self) -> u32 {
        self.member_count
    }

    pub fn member_digest(&self) -> &DataVersionDigest {
        &self.member_digest
    }

    pub fn source_horizon(&self) -> &SummarySourceHorizonV1 {
        &self.source_horizon
    }

    pub const fn authority(&self) -> SessionAuthorityClassV1 {
        self.authority
    }

    pub fn span_id(&self) -> Result<EvidenceSpanIdV1, SessionContractError> {
        if self.evidence_kind != DerivedEvidenceKindV1::Span {
            return Err(SessionContractError::InvalidIdentity {
                field: "EvidenceSpanIdV1",
            });
        }
        EvidenceSpanIdV1::new(self.evidence_id.as_str())
    }
}

impl<'de> Deserialize<'de> for SessionDerivedEvidenceRecordV1 {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Wire {
            evidence_id: DerivedEvidenceIdV1,
            evidence_kind: DerivedEvidenceKindV1,
            retrieval_anchor_id: RetrievalAnchorId,
            session_id: SessionId,
            thread_id: Option<ThreadId>,
            first_occurrence_id: MessageOccurrenceIdV1,
            last_occurrence_id: MessageOccurrenceIdV1,
            algorithm_version: String,
            configuration_digest: DataVersionDigest,
            member_count: u32,
            member_digest: DataVersionDigest,
            source_horizon: SummarySourceHorizonV1,
            authority: SessionAuthorityClassV1,
        }

        let wire = Wire::deserialize(deserializer)?;
        let record = Self {
            evidence_id: wire.evidence_id,
            evidence_kind: wire.evidence_kind,
            retrieval_anchor_id: wire.retrieval_anchor_id,
            session_id: wire.session_id,
            thread_id: wire.thread_id,
            first_occurrence_id: wire.first_occurrence_id,
            last_occurrence_id: wire.last_occurrence_id,
            algorithm_version: wire.algorithm_version,
            configuration_digest: wire.configuration_digest,
            member_count: wire.member_count,
            member_digest: wire.member_digest,
            source_horizon: wire.source_horizon,
            authority: wire.authority,
        };
        record.validate().map_err(serde::de::Error::custom)?;
        Ok(record)
    }
}

/// Ordered occurrence identity used while deriving spans and bursts.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DerivedEvidenceOccurrenceRefV1 {
    pub occurrence_id: MessageOccurrenceIdV1,
    pub retrieval_anchor_id: RetrievalAnchorId,
    pub thread_id: Option<ThreadId>,
    pub message_id: Option<MessageId>,
    pub knowledge_at: UtcMicros,
    pub observation_sequence: u64,
    pub projection_output_ordinal: u32,
}

/// Versioned configuration for the default span/burst projector.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionDerivedEvidencePolicyV1 {
    pub span_max_members: usize,
}

impl Default for SessionDerivedEvidencePolicyV1 {
    fn default() -> Self {
        Self {
            span_max_members: SESSION_DERIVED_SPAN_MAX_MEMBERS_V1,
        }
    }
}

impl SessionDerivedEvidencePolicyV1 {
    pub fn configuration_digest(&self) -> Result<DataVersionDigest, SessionContractError> {
        let mut hasher = Sha256::new();
        hasher.update(DERIVED_CONFIGURATION_DOMAIN);
        hasher.update(self.span_max_members.to_be_bytes());
        digest_from_hasher(hasher)
    }
}

/// The latest burst and span of a session's last run, the only derived
/// evidence a later occurrence can extend.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DerivedEvidenceTailV1 {
    pub burst: SessionDerivedEvidenceRecordV1,
    pub span: SessionDerivedEvidenceRecordV1,
}

/// Derived evidence written by one extension: the final version of every
/// span or burst it touched, and every member it added or re-roled, keyed by
/// the owning evidence kind and first member.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DerivedEvidenceDeltaV1 {
    pub records: Vec<SessionDerivedEvidenceRecordV1>,
    pub members: Vec<(
        DerivedEvidenceKindV1,
        MessageOccurrenceIdV1,
        DerivedEvidenceMemberV1,
    )>,
}

/// Extends a session's spans and bursts with occurrences appended after
/// `tail` in canonical occurrence order.
///
/// Runs are maximal consecutive occurrences sharing a thread; a burst covers
/// one run and spans partition it into windows of the policy's size. Only the
/// tail run can grow, so an extension touches at most that run and the runs
/// it starts. Folding every occurrence from an empty tail derives the same
/// records and members.
pub fn extend_session_evidence(
    session_id: &SessionId,
    tail: Option<&DerivedEvidenceTailV1>,
    occurrences: &[DerivedEvidenceOccurrenceRefV1],
    policy: &SessionDerivedEvidencePolicyV1,
) -> Result<DerivedEvidenceDeltaV1, SessionContractError> {
    for window in occurrences.windows(2) {
        let left = &window[0];
        let right = &window[1];
        if (left.observation_sequence, left.projection_output_ordinal)
            > (right.observation_sequence, right.projection_output_ordinal)
        {
            return Err(SessionContractError::NoncontiguousDerivedEvidenceOrdinals);
        }
    }
    let configuration_digest = policy.configuration_digest()?;
    let span_max_members = u32::try_from(policy.span_max_members.max(1)).map_err(|_| {
        SessionContractError::InvalidIdentity {
            field: "derived evidence span_max_members",
        }
    })?;
    let mut burst = tail
        .map(|tail| DerivedEvidenceBuilder::resume(&tail.burst))
        .transpose()?;
    let mut span = tail
        .map(|tail| DerivedEvidenceBuilder::resume(&tail.span))
        .transpose()?;
    let mut touched = BTreeMap::new();
    let mut members = BTreeMap::new();
    for occurrence in occurrences {
        let continues_run = burst
            .as_ref()
            .is_some_and(|burst| burst.thread_id == occurrence.thread_id);
        let (next_burst, next_span) = match (burst.take(), span.take()) {
            (Some(mut run), Some(mut window)) if continues_run => {
                run.push(occurrence, &mut members)?;
                if window.member_count < span_max_members {
                    window.push(occurrence, &mut members)?;
                } else {
                    window = DerivedEvidenceBuilder::start(
                        DerivedEvidenceKindV1::Span,
                        session_id,
                        &configuration_digest,
                        occurrence,
                        &mut members,
                    )?;
                }
                (run, window)
            }
            _ => (
                DerivedEvidenceBuilder::start(
                    DerivedEvidenceKindV1::Burst,
                    session_id,
                    &configuration_digest,
                    occurrence,
                    &mut members,
                )?,
                DerivedEvidenceBuilder::start(
                    DerivedEvidenceKindV1::Span,
                    session_id,
                    &configuration_digest,
                    occurrence,
                    &mut members,
                )?,
            ),
        };
        touched.insert(next_burst.key(), next_burst.record()?);
        touched.insert(next_span.key(), next_span.record()?);
        burst = Some(next_burst);
        span = Some(next_span);
    }
    Ok(DerivedEvidenceDeltaV1 {
        records: touched.into_values().collect(),
        members: members
            .into_iter()
            .map(|((kind, first, _), member)| (kind, first, member))
            .collect(),
    })
}

/// Derives every span and burst of `occurrences` from scratch.
pub fn derive_session_evidence_from_occurrences(
    session_id: &SessionId,
    occurrences: &[DerivedEvidenceOccurrenceRefV1],
    policy: &SessionDerivedEvidencePolicyV1,
) -> Result<DerivedEvidenceDeltaV1, SessionContractError> {
    extend_session_evidence(session_id, None, occurrences, policy)
}

type DerivedEvidenceKey = (DerivedEvidenceKindV1, MessageOccurrenceIdV1);
type DerivedMemberKey = (DerivedEvidenceKindV1, MessageOccurrenceIdV1, u32);

struct DerivedEvidenceBuilder {
    kind: DerivedEvidenceKindV1,
    session_id: SessionId,
    thread_id: Option<ThreadId>,
    first_occurrence_id: MessageOccurrenceIdV1,
    last_occurrence_id: MessageOccurrenceIdV1,
    configuration_digest: DataVersionDigest,
    member_count: u32,
    member_digest: DataVersionDigest,
    knowledge_through: UtcMicros,
}

impl DerivedEvidenceBuilder {
    fn start(
        kind: DerivedEvidenceKindV1,
        session_id: &SessionId,
        configuration_digest: &DataVersionDigest,
        first: &DerivedEvidenceOccurrenceRefV1,
        members: &mut BTreeMap<DerivedMemberKey, DerivedEvidenceMemberV1>,
    ) -> Result<Self, SessionContractError> {
        let mut builder = Self {
            kind,
            session_id: session_id.clone(),
            thread_id: first.thread_id.clone(),
            first_occurrence_id: first.occurrence_id.clone(),
            last_occurrence_id: first.occurrence_id.clone(),
            configuration_digest: configuration_digest.clone(),
            member_count: 0,
            member_digest: member_digest_seed(
                kind,
                kind.algorithm_version(),
                configuration_digest,
            )?,
            knowledge_through: first.knowledge_at,
        };
        builder.push(first, members)?;
        Ok(builder)
    }

    fn resume(record: &SessionDerivedEvidenceRecordV1) -> Result<Self, SessionContractError> {
        record.validate()?;
        Ok(Self {
            kind: record.evidence_kind,
            session_id: record.session_id.clone(),
            thread_id: record.thread_id.clone(),
            first_occurrence_id: record.first_occurrence_id.clone(),
            last_occurrence_id: record.last_occurrence_id.clone(),
            configuration_digest: record.configuration_digest.clone(),
            member_count: record.member_count,
            member_digest: record.member_digest.clone(),
            knowledge_through: record.source_horizon.knowledge_through,
        })
    }

    fn key(&self) -> DerivedEvidenceKey {
        (self.kind, self.first_occurrence_id.clone())
    }

    fn push(
        &mut self,
        occurrence: &DerivedEvidenceOccurrenceRefV1,
        members: &mut BTreeMap<DerivedMemberKey, DerivedEvidenceMemberV1>,
    ) -> Result<(), SessionContractError> {
        let ordinal = self.member_count;
        if ordinal > 0 && occurrence.occurrence_id == self.last_occurrence_id {
            return Err(SessionContractError::DuplicateDerivedEvidenceMember);
        }
        if ordinal >= 2 {
            let previous = ordinal - 1;
            members.insert(
                (self.kind, self.first_occurrence_id.clone(), previous),
                DerivedEvidenceMemberV1::new(
                    previous,
                    self.last_occurrence_id.clone(),
                    DerivedEvidenceMemberRoleV1::Member,
                ),
            );
        }
        let role = if ordinal == 0 {
            DerivedEvidenceMemberRoleV1::First
        } else {
            DerivedEvidenceMemberRoleV1::Last
        };
        members.insert(
            (self.kind, self.first_occurrence_id.clone(), ordinal),
            DerivedEvidenceMemberV1::new(ordinal, occurrence.occurrence_id.clone(), role),
        );
        self.member_digest =
            extend_member_digest(&self.member_digest, ordinal, &occurrence.occurrence_id)?;
        self.member_count =
            ordinal
                .checked_add(1)
                .ok_or(SessionContractError::InvalidIdentity {
                    field: "derived evidence member_count",
                })?;
        self.last_occurrence_id = occurrence.occurrence_id.clone();
        self.knowledge_through = self.knowledge_through.max(occurrence.knowledge_at);
        Ok(())
    }

    fn record(&self) -> Result<SessionDerivedEvidenceRecordV1, SessionContractError> {
        let algorithm_version = self.kind.algorithm_version();
        let record = SessionDerivedEvidenceRecordV1 {
            evidence_id: derive_evidence_id(
                self.kind,
                algorithm_version,
                &self.configuration_digest,
                &self.member_digest,
                self.member_count,
            )?,
            evidence_kind: self.kind,
            retrieval_anchor_id: derive_derived_anchor_id(
                self.kind,
                &self.session_id,
                &self.configuration_digest,
                &self.member_digest,
            )?,
            session_id: self.session_id.clone(),
            thread_id: self.thread_id.clone(),
            first_occurrence_id: self.first_occurrence_id.clone(),
            last_occurrence_id: self.last_occurrence_id.clone(),
            algorithm_version: algorithm_version.to_owned(),
            configuration_digest: self.configuration_digest.clone(),
            member_count: self.member_count,
            member_digest: self.member_digest.clone(),
            source_horizon: SummarySourceHorizonV1 {
                knowledge_through: self.knowledge_through,
                valid_through: None,
            },
            authority: SessionAuthorityClassV1::DerivedProjection,
        };
        record.validate()?;
        Ok(record)
    }
}

fn member_digest_seed(
    kind: DerivedEvidenceKindV1,
    algorithm_version: &str,
    configuration_digest: &DataVersionDigest,
) -> Result<DataVersionDigest, SessionContractError> {
    tagged_digest(canonical_framed_sha256_bytes(
        DERIVED_MEMBER_DIGEST_DOMAIN,
        &[
            kind.as_str().as_bytes(),
            algorithm_version.as_bytes(),
            configuration_digest.as_str().as_bytes(),
        ],
    ))
}

fn extend_member_digest(
    previous: &DataVersionDigest,
    ordinal: u32,
    occurrence_id: &MessageOccurrenceIdV1,
) -> Result<DataVersionDigest, SessionContractError> {
    tagged_digest(canonical_framed_sha256_bytes(
        DERIVED_MEMBER_DIGEST_DOMAIN,
        &[
            previous.as_str().as_bytes(),
            &ordinal.to_be_bytes(),
            occurrence_id.as_str().as_bytes(),
        ],
    ))
}

fn derive_evidence_id(
    kind: DerivedEvidenceKindV1,
    algorithm_version: &str,
    configuration_digest: &DataVersionDigest,
    member_digest: &DataVersionDigest,
    member_count: u32,
) -> Result<DerivedEvidenceIdV1, SessionContractError> {
    DerivedEvidenceIdV1::new(encode_tagged_lowercase_hex(
        "sha256:",
        &canonical_framed_sha256_bytes(
            DERIVED_EVIDENCE_ID_DOMAIN,
            &[
                kind.as_str().as_bytes(),
                algorithm_version.as_bytes(),
                configuration_digest.as_str().as_bytes(),
                member_digest.as_str().as_bytes(),
                &member_count.to_be_bytes(),
            ],
        ),
    ))
}

fn derive_derived_anchor_id(
    kind: DerivedEvidenceKindV1,
    session_id: &SessionId,
    configuration_digest: &DataVersionDigest,
    member_digest: &DataVersionDigest,
) -> Result<RetrievalAnchorId, SessionContractError> {
    RetrievalAnchorId::new(encode_tagged_lowercase_hex(
        "sha256:",
        &canonical_framed_sha256_bytes(
            DERIVED_ANCHOR_DOMAIN,
            &[
                kind.as_str().as_bytes(),
                session_id.as_str().as_bytes(),
                configuration_digest.as_str().as_bytes(),
                member_digest.as_str().as_bytes(),
            ],
        ),
    ))
    .map_err(|_| SessionContractError::InvalidIdentity {
        field: "derived evidence retrieval_anchor_id",
    })
}

fn digest_from_hasher(hasher: Sha256) -> Result<DataVersionDigest, SessionContractError> {
    tagged_digest(hasher.finalize().into())
}

fn tagged_digest(bytes: [u8; 32]) -> Result<DataVersionDigest, SessionContractError> {
    DataVersionDigest::new(encode_tagged_lowercase_hex("sha256:", &bytes)).map_err(|_| {
        SessionContractError::InvalidIdentity {
            field: "DataVersionDigest",
        }
    })
}

fn is_sha256_identity(value: &str) -> bool {
    crate::canonical_text::is_tagged_lowercase_hex(value, "sha256:", 64)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sha_id(label: &str) -> String {
        encode_tagged_lowercase_hex("sha256:", &Sha256::digest(label.as_bytes()))
    }

    fn occurrence(index: u64, thread: &str) -> DerivedEvidenceOccurrenceRefV1 {
        DerivedEvidenceOccurrenceRefV1 {
            occurrence_id: MessageOccurrenceIdV1::new(sha_id(&format!("occurrence-{index}")))
                .unwrap(),
            retrieval_anchor_id: RetrievalAnchorId::new(sha_id(&format!("anchor-{index}")))
                .unwrap(),
            thread_id: Some(ThreadId::new(thread).unwrap()),
            message_id: Some(MessageId::new(format!("message.{index}")).unwrap()),
            knowledge_at: UtcMicros(i64::try_from(index).unwrap()),
            observation_sequence: index + 1,
            projection_output_ordinal: 0,
        }
    }

    #[test]
    fn singleton_occurrence_derives_valid_burst_and_span_evidence() {
        let session_id = SessionId::new("session.derived.singleton").unwrap();
        let only = occurrence(0, "thread.derived.singleton");

        let derived = derive_session_evidence_from_occurrences(
            &session_id,
            std::slice::from_ref(&only),
            &SessionDerivedEvidencePolicyV1::default(),
        )
        .unwrap();

        assert_eq!(derived.records.len(), 2);
        assert_eq!(derived.members.len(), 2);
        for (_, first, member) in &derived.members {
            assert_eq!(first, &only.occurrence_id);
            assert_eq!(member.ordinal, 0);
            assert_eq!(member.member_role, DerivedEvidenceMemberRoleV1::First);
        }
        for record in derived.records {
            assert_eq!(record.member_count(), 1);
            assert_eq!(record.first_occurrence_id(), &only.occurrence_id);
            assert_eq!(record.last_occurrence_id(), &only.occurrence_id);
            let encoded = serde_json::to_value(&record).unwrap();
            assert_eq!(
                serde_json::from_value::<SessionDerivedEvidenceRecordV1>(encoded).unwrap(),
                record
            );
        }
    }

    /// Replays `extension` onto a record/member map the way the store does:
    /// a later version of a key replaces the earlier one.
    fn apply(
        records: &mut BTreeMap<DerivedEvidenceKey, SessionDerivedEvidenceRecordV1>,
        members: &mut BTreeMap<DerivedMemberKey, DerivedEvidenceMemberV1>,
        extension: DerivedEvidenceDeltaV1,
    ) {
        for record in extension.records {
            records.insert(
                (record.evidence_kind(), record.first_occurrence_id().clone()),
                record,
            );
        }
        for (kind, first, member) in extension.members {
            members.insert((kind, first, member.ordinal), member);
        }
    }

    fn tail_of(
        records: &BTreeMap<DerivedEvidenceKey, SessionDerivedEvidenceRecordV1>,
        last: &MessageOccurrenceIdV1,
    ) -> DerivedEvidenceTailV1 {
        let find = |kind| {
            records
                .values()
                .find(|record| {
                    record.evidence_kind() == kind && record.last_occurrence_id() == last
                })
                .unwrap()
                .clone()
        };
        DerivedEvidenceTailV1 {
            burst: find(DerivedEvidenceKindV1::Burst),
            span: find(DerivedEvidenceKindV1::Span),
        }
    }

    #[test]
    fn extending_a_tail_one_occurrence_at_a_time_matches_the_from_scratch_derivation() {
        let session_id = SessionId::new("session.derived.extension").unwrap();
        let policy = SessionDerivedEvidencePolicyV1 {
            span_max_members: 3,
        };
        let threads = [
            "a", "a", "a", "a", "a", "b", "b", "a", "c", "c", "c", "c", "c", "c", "c",
        ];
        let occurrences = threads
            .iter()
            .enumerate()
            .map(|(index, thread)| occurrence(u64::try_from(index).unwrap(), thread))
            .collect::<Vec<_>>();

        let mut expected_records = BTreeMap::new();
        let mut expected_members = BTreeMap::new();
        apply(
            &mut expected_records,
            &mut expected_members,
            derive_session_evidence_from_occurrences(&session_id, &occurrences, &policy).unwrap(),
        );

        let mut records = BTreeMap::new();
        let mut members = BTreeMap::new();
        for (index, next) in occurrences.iter().enumerate() {
            let tail = index
                .checked_sub(1)
                .map(|previous| tail_of(&records, &occurrences[previous].occurrence_id));
            let extension = extend_session_evidence(
                &session_id,
                tail.as_ref(),
                std::slice::from_ref(next),
                &policy,
            )
            .unwrap();
            assert!(
                extension.records.len() <= 2 && extension.members.len() <= 4,
                "one appended occurrence rewrites only the tail burst and span"
            );
            apply(&mut records, &mut members, extension);
        }

        assert_eq!(records, expected_records);
        assert_eq!(members, expected_members);
        let mut bursts = expected_records
            .values()
            .filter(|record| record.evidence_kind() == DerivedEvidenceKindV1::Burst)
            .map(SessionDerivedEvidenceRecordV1::member_count)
            .collect::<Vec<_>>();
        bursts.sort_unstable();
        assert_eq!(bursts, [1, 2, 5, 7]);
        let spans = expected_records
            .values()
            .filter(|record| record.evidence_kind() == DerivedEvidenceKindV1::Span)
            .count();
        assert_eq!(spans, 7);
        assert_eq!(
            members
                .values()
                .filter(|member| member.member_role == DerivedEvidenceMemberRoleV1::Last)
                .count(),
            8
        );
    }
}
