//! Generation-bound session-derived evidence spans and bursts.
//!
//! These contracts describe immutable, rebuildable projections over consecutive
//! message occurrences. They are not source authority, summaries, or carriers of
//! external GitHub/CI/diagnostic/Git/receipt/task payloads.

use std::fmt;

use serde::{Deserialize, Deserializer, Serialize};
use sha2::{Digest, Sha256};

use crate::research::{
    DataVersionDigest, MessageId, RetrievalAnchorId, SessionId, ThreadId, UtcMicros,
};
use crate::session::{
    MessageOccurrenceIdV1, SessionAuthorityClassV1, SessionContractError, SummarySourceHorizonV1,
};

const DERIVED_EVIDENCE_ID_DOMAIN: &[u8] = b"tracedecay.session.derived-evidence.v1\0";
const DERIVED_MEMBER_DIGEST_DOMAIN: &[u8] = b"tracedecay.session.derived-member-digest.v1\0";
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

    pub fn as_derived(&self) -> &DerivedEvidenceIdV1 {
        &self.0
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

/// Immutable generation-bound derived evidence record.
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
    members: Vec<DerivedEvidenceMemberV1>,
}

impl SessionDerivedEvidenceRecordV1 {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        evidence_kind: DerivedEvidenceKindV1,
        retrieval_anchor_id: RetrievalAnchorId,
        session_id: SessionId,
        thread_id: Option<ThreadId>,
        algorithm_version: impl Into<String>,
        configuration_digest: DataVersionDigest,
        source_horizon: SummarySourceHorizonV1,
        members: Vec<DerivedEvidenceMemberV1>,
    ) -> Result<Self, SessionContractError> {
        let algorithm_version = algorithm_version.into();
        if algorithm_version.is_empty() || algorithm_version.trim() != algorithm_version {
            return Err(SessionContractError::InvalidIdentity {
                field: "derived evidence algorithm_version",
            });
        }
        if members.is_empty() {
            return Err(SessionContractError::DerivedEvidenceMembersRequired);
        }
        source_horizon.validate()?;
        let mut seen = BTreeSetLite::default();
        for (index, member) in members.iter().enumerate() {
            member.validate()?;
            if member.ordinal as usize != index {
                return Err(SessionContractError::NoncontiguousDerivedEvidenceOrdinals);
            }
            if !seen.insert(member.occurrence_id.as_str().to_owned()) {
                return Err(SessionContractError::DuplicateDerivedEvidenceMember);
            }
        }
        let first = members
            .first()
            .expect("non-empty members")
            .occurrence_id
            .clone();
        let last = members
            .last()
            .expect("non-empty members")
            .occurrence_id
            .clone();
        let member_digest = member_digest(
            evidence_kind,
            &algorithm_version,
            &configuration_digest,
            &members,
        )?;
        let evidence_id = derive_evidence_id(
            evidence_kind,
            &algorithm_version,
            &configuration_digest,
            &members,
        )?;
        let member_count =
            u32::try_from(members.len()).map_err(|_| SessionContractError::InvalidIdentity {
                field: "derived evidence member_count",
            })?;
        Ok(Self {
            evidence_id,
            evidence_kind,
            retrieval_anchor_id,
            session_id,
            thread_id,
            first_occurrence_id: first,
            last_occurrence_id: last,
            algorithm_version,
            configuration_digest,
            member_count,
            member_digest,
            source_horizon,
            authority: SessionAuthorityClassV1::DerivedProjection,
            members,
        })
    }

    pub fn validate(&self) -> Result<(), SessionContractError> {
        if self.authority != SessionAuthorityClassV1::DerivedProjection {
            return Err(SessionContractError::DerivedEvidenceAuthorityMismatch);
        }
        if self.members.is_empty() {
            return Err(SessionContractError::DerivedEvidenceMembersRequired);
        }
        if self.member_count as usize != self.members.len() {
            return Err(SessionContractError::DerivedEvidenceMemberDigestMismatch);
        }
        self.source_horizon.validate()?;
        let expected_digest = member_digest(
            self.evidence_kind,
            &self.algorithm_version,
            &self.configuration_digest,
            &self.members,
        )?;
        if expected_digest != self.member_digest {
            return Err(SessionContractError::DerivedEvidenceMemberDigestMismatch);
        }
        let expected_id = derive_evidence_id(
            self.evidence_kind,
            &self.algorithm_version,
            &self.configuration_digest,
            &self.members,
        )?;
        if expected_id != self.evidence_id {
            return Err(SessionContractError::InvalidIdentity {
                field: "DerivedEvidenceIdV1",
            });
        }
        let first = &self.members.first().expect("non-empty").occurrence_id;
        let last = &self.members.last().expect("non-empty").occurrence_id;
        if first != &self.first_occurrence_id || last != &self.last_occurrence_id {
            return Err(SessionContractError::DerivedEvidenceEndpointMismatch);
        }
        for (index, member) in self.members.iter().enumerate() {
            if member.ordinal as usize != index {
                return Err(SessionContractError::NoncontiguousDerivedEvidenceOrdinals);
            }
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

    pub fn members(&self) -> &[DerivedEvidenceMemberV1] {
        &self.members
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
            members: Vec<DerivedEvidenceMemberV1>,
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
            members: wire.members,
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

/// Derive generation-bound spans and bursts from canonical occurrence order.
pub fn derive_session_evidence_from_occurrences(
    session_id: &SessionId,
    occurrences: &[DerivedEvidenceOccurrenceRefV1],
    policy: &SessionDerivedEvidencePolicyV1,
) -> Result<Vec<SessionDerivedEvidenceRecordV1>, SessionContractError> {
    if occurrences.is_empty() {
        return Ok(Vec::new());
    }
    for window in occurrences.windows(2) {
        let left = &window[0];
        let right = &window[1];
        let ordered = (left.observation_sequence, left.projection_output_ordinal)
            <= (right.observation_sequence, right.projection_output_ordinal);
        if !ordered {
            return Err(SessionContractError::NoncontiguousDerivedEvidenceOrdinals);
        }
    }
    let configuration_digest = policy.configuration_digest()?;
    let runs = contiguous_runs(occurrences);
    let mut derived = Vec::new();
    for run in runs {
        if run.is_empty() {
            continue;
        }
        let burst_members = members_for_run(run);
        let horizon = horizon_for_run(run)?;
        let burst_anchor = derive_derived_anchor_id(
            DerivedEvidenceKindV1::Burst,
            session_id,
            &burst_members,
            &configuration_digest,
        )?;
        derived.push(SessionDerivedEvidenceRecordV1::new(
            DerivedEvidenceKindV1::Burst,
            burst_anchor,
            session_id.clone(),
            run.first().and_then(|item| item.thread_id.clone()),
            DerivedEvidenceKindV1::Burst.algorithm_version(),
            configuration_digest.clone(),
            horizon,
            burst_members,
        )?);

        let mut span_start = 0usize;
        while span_start < run.len() {
            let end = (span_start + policy.span_max_members).min(run.len());
            let span_run = &run[span_start..end];
            let span_members = members_for_run(span_run);
            let span_horizon = horizon_for_run(span_run)?;
            let span_anchor = derive_derived_anchor_id(
                DerivedEvidenceKindV1::Span,
                session_id,
                &span_members,
                &configuration_digest,
            )?;
            derived.push(SessionDerivedEvidenceRecordV1::new(
                DerivedEvidenceKindV1::Span,
                span_anchor,
                session_id.clone(),
                span_run.first().and_then(|item| item.thread_id.clone()),
                DerivedEvidenceKindV1::Span.algorithm_version(),
                configuration_digest.clone(),
                span_horizon,
                span_members,
            )?);
            if end == run.len() {
                break;
            }
            span_start = end;
        }
    }
    Ok(derived)
}

fn contiguous_runs(
    occurrences: &[DerivedEvidenceOccurrenceRefV1],
) -> Vec<&[DerivedEvidenceOccurrenceRefV1]> {
    // Versioned adjacency: maximal consecutive runs share a thread identity.
    let mut runs = Vec::new();
    let mut start = 0usize;
    for index in 1..occurrences.len() {
        if occurrences[index - 1].thread_id != occurrences[index].thread_id {
            runs.push(&occurrences[start..index]);
            start = index;
        }
    }
    runs.push(&occurrences[start..]);
    runs
}

fn members_for_run(run: &[DerivedEvidenceOccurrenceRefV1]) -> Vec<DerivedEvidenceMemberV1> {
    run.iter()
        .enumerate()
        .map(|(ordinal, item)| {
            let role = if run.len() == 1 || ordinal == 0 {
                DerivedEvidenceMemberRoleV1::First
            } else if ordinal + 1 == run.len() {
                DerivedEvidenceMemberRoleV1::Last
            } else {
                DerivedEvidenceMemberRoleV1::Member
            };
            DerivedEvidenceMemberV1::new(ordinal as u32, item.occurrence_id.clone(), role)
        })
        .collect()
}

fn horizon_for_run(
    run: &[DerivedEvidenceOccurrenceRefV1],
) -> Result<SummarySourceHorizonV1, SessionContractError> {
    let knowledge_through = run
        .iter()
        .map(|item| item.knowledge_at)
        .max()
        .ok_or(SessionContractError::DerivedEvidenceMembersRequired)?;
    Ok(SummarySourceHorizonV1 {
        knowledge_through,
        valid_through: None,
    })
}

fn derive_evidence_id(
    kind: DerivedEvidenceKindV1,
    algorithm_version: &str,
    configuration_digest: &DataVersionDigest,
    members: &[DerivedEvidenceMemberV1],
) -> Result<DerivedEvidenceIdV1, SessionContractError> {
    let mut hasher = Sha256::new();
    hasher.update(DERIVED_EVIDENCE_ID_DOMAIN);
    hasher.update(kind.as_str().as_bytes());
    hasher.update(algorithm_version.as_bytes());
    hasher.update(configuration_digest.as_str().as_bytes());
    for member in members {
        hasher.update(member.ordinal.to_be_bytes());
        hasher.update(member.occurrence_id.as_str().as_bytes());
        hasher.update(member.member_role.as_str().as_bytes());
    }
    DerivedEvidenceIdV1::new(encode_sha256(hasher))
}

fn member_digest(
    kind: DerivedEvidenceKindV1,
    algorithm_version: &str,
    configuration_digest: &DataVersionDigest,
    members: &[DerivedEvidenceMemberV1],
) -> Result<DataVersionDigest, SessionContractError> {
    let mut hasher = Sha256::new();
    hasher.update(DERIVED_MEMBER_DIGEST_DOMAIN);
    hasher.update(kind.as_str().as_bytes());
    hasher.update(algorithm_version.as_bytes());
    hasher.update(configuration_digest.as_str().as_bytes());
    for member in members {
        hasher.update(member.ordinal.to_be_bytes());
        hasher.update(member.occurrence_id.as_str().as_bytes());
    }
    digest_from_hasher(hasher)
}

fn derive_derived_anchor_id(
    kind: DerivedEvidenceKindV1,
    session_id: &SessionId,
    members: &[DerivedEvidenceMemberV1],
    configuration_digest: &DataVersionDigest,
) -> Result<RetrievalAnchorId, SessionContractError> {
    let mut hasher = Sha256::new();
    hasher.update(b"tracedecay.session.derived-anchor.v1\0");
    hasher.update(kind.as_str().as_bytes());
    hasher.update(session_id.as_str().as_bytes());
    hasher.update(configuration_digest.as_str().as_bytes());
    for member in members {
        hasher.update(member.occurrence_id.as_str().as_bytes());
    }
    RetrievalAnchorId::new(encode_sha256(hasher)).map_err(|_| {
        SessionContractError::InvalidIdentity {
            field: "derived evidence retrieval_anchor_id",
        }
    })
}

fn digest_from_hasher(hasher: Sha256) -> Result<DataVersionDigest, SessionContractError> {
    DataVersionDigest::new(encode_sha256(hasher)).map_err(|_| {
        SessionContractError::InvalidIdentity {
            field: "DataVersionDigest",
        }
    })
}

fn encode_sha256(hasher: Sha256) -> String {
    crate::canonical_text::encode_tagged_lowercase_hex("sha256:", &hasher.finalize())
}

fn is_sha256_identity(value: &str) -> bool {
    crate::canonical_text::is_tagged_lowercase_hex(value, "sha256:", 64)
}

#[derive(Default)]
struct BTreeSetLite {
    values: Vec<String>,
}

impl BTreeSetLite {
    fn insert(&mut self, value: String) -> bool {
        match self.values.binary_search(&value) {
            Ok(_) => false,
            Err(index) => {
                self.values.insert(index, value);
                true
            }
        }
    }
}
