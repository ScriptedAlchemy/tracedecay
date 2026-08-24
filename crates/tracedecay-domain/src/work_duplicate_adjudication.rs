//! Explicit, revisioned adjudication of duplicate Work effort.
//!
//! This contract deliberately cannot infer a verdict. A caller must name two
//! exact attempts and pin the mounted Work and topology generations reviewed
//! by an independent operator/runtime adjudicator.

use schemars::JsonSchema;
use serde::{Deserialize, Deserializer, Serialize};
use thiserror::Error;

use crate::{
    ActorId, CoverageStateV1, DuplicateEffectOutcomeV1, DuplicateEffortKindV1, ManifestDigest,
    ProjectionGenerationId, QuantityEvidenceClassV1, UtcMicros, WorkAttemptIdentityV1,
    WorkCommandId, WorkTopologyGenerationRefV1,
};

pub const MAX_WORK_DUPLICATE_REASON_BYTES_V1: usize = 4_096;

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum WorkDuplicateAdjudicationContractErrorV1 {
    #[error("duplicate Work adjudication revision must be non-zero")]
    InvalidRevision,
    #[error("duplicate Work adjudication must bind two distinct attempts")]
    SameAttempt,
    #[error("duplicate Work adjudication evidence is invalid")]
    InvalidEvidence,
    #[error("duplicate Work adjudication reason is invalid")]
    InvalidReason,
    #[error("duplicate Work adjudication quantities require known evidence")]
    InvalidQuantityEvidence,
    #[error("duplicate Work adjudication receipt is invalid")]
    InvalidReceipt,
}

#[derive(Clone, Copy, Debug, Serialize, JsonSchema, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(transparent)]
pub struct WorkDuplicateAdjudicationRevisionV1(u64);

impl WorkDuplicateAdjudicationRevisionV1 {
    pub fn new(value: u64) -> Result<Self, WorkDuplicateAdjudicationContractErrorV1> {
        if value == 0 {
            return Err(WorkDuplicateAdjudicationContractErrorV1::InvalidRevision);
        }
        Ok(Self(value))
    }

    pub const fn initial() -> Self {
        Self(1)
    }

    pub const fn get(self) -> u64 {
        self.0
    }

    pub fn next(self) -> Result<Self, WorkDuplicateAdjudicationContractErrorV1> {
        self.0
            .checked_add(1)
            .map(Self)
            .ok_or(WorkDuplicateAdjudicationContractErrorV1::InvalidRevision)
    }
}

impl<'de> Deserialize<'de> for WorkDuplicateAdjudicationRevisionV1 {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        Self::new(u64::deserialize(deserializer)?).map_err(serde::de::Error::custom)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorkDuplicateAdjudicationEvidenceV1 {
    pub work_generation: ProjectionGenerationId,
    pub topology_generation: WorkTopologyGenerationRefV1,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorkDuplicateAdjudicationQuantitiesV1 {
    pub wall_micros: Option<u64>,
    pub token_count: Option<u64>,
    pub cost_micros: Option<u64>,
    pub test_count: Option<u64>,
    pub effect_count: Option<u64>,
    pub evidence: QuantityEvidenceClassV1,
    pub effect_outcome: DuplicateEffectOutcomeV1,
    pub coverage: CoverageStateV1,
}

impl WorkDuplicateAdjudicationQuantitiesV1 {
    pub fn validate(&self) -> Result<(), WorkDuplicateAdjudicationContractErrorV1> {
        let has_quantity = self.wall_micros.is_some()
            || self.token_count.is_some()
            || self.cost_micros.is_some()
            || self.test_count.is_some()
            || self.effect_count.is_some();
        if has_quantity && self.evidence == QuantityEvidenceClassV1::Unknown {
            return Err(WorkDuplicateAdjudicationContractErrorV1::InvalidQuantityEvidence);
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorkDuplicateAdjudicationCommandV1 {
    pub expected_revision: Option<WorkDuplicateAdjudicationRevisionV1>,
    pub first_attempt: WorkAttemptIdentityV1,
    pub second_attempt: WorkAttemptIdentityV1,
    pub evidence: WorkDuplicateAdjudicationEvidenceV1,
    pub verdict: DuplicateEffortKindV1,
    pub quantities: WorkDuplicateAdjudicationQuantitiesV1,
    pub reason: String,
    pub command_id: WorkCommandId,
    pub occurred_at: UtcMicros,
}

impl WorkDuplicateAdjudicationCommandV1 {
    pub fn validate(&self) -> Result<(), WorkDuplicateAdjudicationContractErrorV1> {
        if self.first_attempt == self.second_attempt {
            return Err(WorkDuplicateAdjudicationContractErrorV1::SameAttempt);
        }
        self.quantities.validate()?;
        let coverage_matches_verdict = match self.verdict {
            DuplicateEffortKindV1::ExactDuplicate
            | DuplicateEffortKindV1::SupersededOverlap
            | DuplicateEffortKindV1::RepeatedInvestigation
            | DuplicateEffortKindV1::DuplicateEffect
            | DuplicateEffortKindV1::NotDuplicate => {
                self.quantities.coverage == CoverageStateV1::Known
            }
            DuplicateEffortKindV1::Censored => matches!(
                self.quantities.coverage,
                CoverageStateV1::Partial
                    | CoverageStateV1::Stale
                    | CoverageStateV1::Sampled
                    | CoverageStateV1::Capped
            ),
            DuplicateEffortKindV1::Unknown => self.quantities.coverage == CoverageStateV1::Unknown,
        };
        if !coverage_matches_verdict {
            return Err(WorkDuplicateAdjudicationContractErrorV1::InvalidEvidence);
        }
        if !crate::canonical_text::is_canonical_text_within(
            &self.reason,
            MAX_WORK_DUPLICATE_REASON_BYTES_V1,
        ) {
            return Err(WorkDuplicateAdjudicationContractErrorV1::InvalidReason);
        }
        Ok(())
    }

    /// Duplicate-work is an undirected relation. Canonical ordering makes the
    /// same two attempts one identity regardless of caller presentation.
    pub fn canonicalized(mut self) -> Self {
        if self.second_attempt < self.first_attempt {
            std::mem::swap(&mut self.first_attempt, &mut self.second_attempt);
        }
        self
    }

    pub fn canonical_input_digest(&self) -> Result<ManifestDigest, crate::research::DomainError> {
        crate::canonical_sha256(&("tracedecay.work-duplicate-adjudication-command.v1", self))
    }

    /// Stable relation identity owned by the exact Work authority and
    /// canonical attempt pair. Caller-selected adjudication labels and
    /// corrected revisions cannot create a second metric trace for the same
    /// relation, and identical text in another authority cannot coalesce.
    pub fn relation_ref(
        &self,
        authority: &crate::WorkAuthority,
    ) -> Result<ManifestDigest, crate::research::DomainError> {
        let canonical = self.clone().canonicalized();
        Self::relation_ref_for_pair(
            authority,
            &canonical.first_attempt,
            &canonical.second_attempt,
        )
    }

    pub fn relation_ref_for_pair(
        authority: &crate::WorkAuthority,
        first_attempt: &WorkAttemptIdentityV1,
        second_attempt: &WorkAttemptIdentityV1,
    ) -> Result<ManifestDigest, crate::research::DomainError> {
        let (first_attempt, second_attempt) = if first_attempt <= second_attempt {
            (first_attempt, second_attempt)
        } else {
            (second_attempt, first_attempt)
        };
        crate::canonical_sha256(&(
            "tracedecay.work-duplicate-relation.v1",
            authority,
            first_attempt,
            second_attempt,
        ))
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorkDuplicateAdjudicationReceiptV1 {
    command: WorkDuplicateAdjudicationCommandV1,
    revision: WorkDuplicateAdjudicationRevisionV1,
    actor_id: ActorId,
    canonical_input_digest: ManifestDigest,
    adjudication_ref: ManifestDigest,
}

impl WorkDuplicateAdjudicationReceiptV1 {
    pub fn new(
        authority: &crate::WorkAuthority,
        command: WorkDuplicateAdjudicationCommandV1,
        revision: WorkDuplicateAdjudicationRevisionV1,
        canonical_input_digest: ManifestDigest,
    ) -> Result<Self, WorkDuplicateAdjudicationContractErrorV1> {
        command.validate()?;
        if command.clone().canonicalized() != command {
            return Err(WorkDuplicateAdjudicationContractErrorV1::InvalidReceipt);
        }
        let expected = match command.expected_revision {
            None => WorkDuplicateAdjudicationRevisionV1::initial(),
            Some(current) => current.next()?,
        };
        if revision != expected
            || command.canonical_input_digest().as_ref() != Ok(&canonical_input_digest)
        {
            return Err(WorkDuplicateAdjudicationContractErrorV1::InvalidReceipt);
        }
        let adjudication_ref = command
            .relation_ref(authority)
            .map_err(|_| WorkDuplicateAdjudicationContractErrorV1::InvalidReceipt)?;
        Ok(Self {
            command,
            revision,
            actor_id: authority.actor_id().clone(),
            canonical_input_digest,
            adjudication_ref,
        })
    }

    pub const fn command(&self) -> &WorkDuplicateAdjudicationCommandV1 {
        &self.command
    }

    pub const fn revision(&self) -> WorkDuplicateAdjudicationRevisionV1 {
        self.revision
    }

    pub const fn actor_id(&self) -> &ActorId {
        &self.actor_id
    }

    pub const fn canonical_input_digest(&self) -> &ManifestDigest {
        &self.canonical_input_digest
    }

    pub const fn adjudication_ref(&self) -> &ManifestDigest {
        &self.adjudication_ref
    }

    pub fn observability_payload(&self) -> crate::WorkDuplicateEffortObservedV1 {
        let adjudication_ref = self.adjudication_ref.as_str().to_owned();
        crate::WorkDuplicateEffortObservedV1 {
            adjudication_ref: adjudication_ref.clone(),
            adjudication_revision: self.revision.get(),
            kind: self.command.verdict,
            wall_micros: self.command.quantities.wall_micros,
            token_count: self.command.quantities.token_count,
            cost_micros: self.command.quantities.cost_micros,
            test_count: self.command.quantities.test_count,
            effect_count: self.command.quantities.effect_count,
            evidence: self.command.quantities.evidence,
            effect_outcome: self.command.quantities.effect_outcome,
            coverage: self.command.quantities.coverage,
            local_anchor_refs: vec![adjudication_ref],
        }
    }
}
