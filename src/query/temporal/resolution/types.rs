use std::collections::BTreeSet;
use std::ops::Deref;

use tracedecay_domain::{
    MessageOccurrenceIdV1, RetrievalAnchorId, SessionAuthorityClassV1, TemporalAssertionKindV1,
    TemporalAssertionRecordV1, TemporalValidityV1, UtcMicros,
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResolutionEvidence {
    pub authority: SessionAuthorityClassV1,
    authorized: bool,
    pub supporting_anchor_ids: BTreeSet<RetrievalAnchorId>,
}

impl ResolutionEvidence {
    pub fn new(authority: SessionAuthorityClassV1, authorization: ValidatedAuthorization) -> Self {
        Self {
            authority,
            authorized: authorization.is_authorized(),
            supporting_anchor_ids: BTreeSet::new(),
        }
    }

    pub const fn is_authorized(&self) -> bool {
        self.authorized
    }

    #[must_use]
    pub fn with_supporting_anchor(mut self, anchor_id: RetrievalAnchorId) -> Self {
        self.supporting_anchor_ids.insert(anchor_id);
        self
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ValidatedAuthorization {
    Authorized,
    Unauthorized,
}

impl ValidatedAuthorization {
    pub const fn is_authorized(self) -> bool {
        matches!(self, Self::Authorized)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ResolutionInputError {
    UnauthorizedAssertion,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResolutionOccurrence {
    pub occurrence_id: MessageOccurrenceIdV1,
    pub anchor_id: RetrievalAnchorId,
    pub knowledge_at: UtcMicros,
    pub valid_time: TemporalValidityV1,
    pub evidence: ResolutionEvidence,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResolutionAssertion {
    pub kind: TemporalAssertionKindV1,
    pub subject_anchor_id: RetrievalAnchorId,
    pub object_anchor_id: RetrievalAnchorId,
    pub knowledge_at: UtcMicros,
    pub valid_time: TemporalValidityV1,
    pub evidence: ResolutionEvidence,
}

impl ResolutionAssertion {
    pub fn from_record(
        assertion: &TemporalAssertionRecordV1,
        authorization: ValidatedAuthorization,
    ) -> Result<Self, ResolutionInputError> {
        if !authorization.is_authorized() {
            return Err(ResolutionInputError::UnauthorizedAssertion);
        }
        Ok(Self {
            kind: assertion.kind,
            subject_anchor_id: assertion.subject_anchor_id.clone(),
            object_anchor_id: assertion.object_anchor_id.clone(),
            knowledge_at: assertion.knowledge_at,
            valid_time: assertion.valid_time,
            evidence: ResolutionEvidence::new(assertion.evidence.authority, authorization)
                .with_supporting_anchor(assertion.evidence.source_anchor_id.clone()),
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResolvedOccurrence {
    pub occurrence: ResolutionOccurrence,
    pub representative_id: MessageOccurrenceIdV1,
    pub conflicted: bool,
    pub uncertain: bool,
    pub supporting_anchor_ids: BTreeSet<RetrievalAnchorId>,
}

impl ResolvedOccurrence {
    pub const fn certainty(&self) -> ResolutionCertainty {
        if self.uncertain {
            ResolutionCertainty::AuthorizedUnknown
        } else {
            ResolutionCertainty::Known
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ResolutionCertainty {
    Known,
    AuthorizedUnknown,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum ResolutionLineageEdgeKind {
    Correction,
    Contradiction,
    Supersession,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResolutionLineageEdge {
    pub kind: ResolutionLineageEdgeKind,
    pub subject_anchor_id: RetrievalAnchorId,
    pub object_anchor_id: RetrievalAnchorId,
    pub knowledge_at: UtcMicros,
    pub evidence: ResolutionEvidence,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TemporalResolution {
    pub occurrences: Vec<ResolvedOccurrence>,
    pub lineage_edges: Vec<ResolutionLineageEdge>,
}

impl Deref for TemporalResolution {
    type Target = [ResolvedOccurrence];

    fn deref(&self) -> &Self::Target {
        &self.occurrences
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ResolutionCheckpoint {
    Occurrence,
    Copy,
    Assertion,
    Relation,
    Materialization,
    Evolution,
}
