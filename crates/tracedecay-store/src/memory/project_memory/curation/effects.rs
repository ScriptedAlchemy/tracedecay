use serde::{Deserialize, Deserializer, Serialize};
use tracedecay_domain::{
    Confidence, DomainError, FactId, FactOwnerV1, FactRelationKindV1, FactRelationV1,
    PayloadReferenceV1, SanitizationReceiptRefV1, SanitizationReceiptV1, SanitizerDispositionV1,
    SensitivityV1, canonical_sha256,
};

use super::super::super::{FactCommitReceipt, FactStoreError, FactStoreResult};
use super::super::{ProjectMemoryFactIdV1, validate_project_memory_text};
use super::{
    MAX_PROJECT_MEMORY_CURATION_TARGETS, ProjectMemoryFactAddDispositionV1,
    ProjectMemoryFactAddOutcomeV1, ProjectMemoryFactMergeOutcomeV1,
    ProjectMemoryFactRemoveOutcomeV1, ProjectMemoryFactUpdateOutcomeV1,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProjectMemoryFactCurationRemoveDispositionV1 {
    Removed,
    AlreadyRemoved,
    NotFound,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProjectMemoryFactCurationLinkDispositionV1 {
    Linked,
    AlreadyLinked,
}

/// Exact durable effect emitted by one curation operation, in request order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ProjectMemoryFactCurationOperationEffectV1 {
    Add {
        fact: ProjectMemoryFactIdV1,
        disposition: ProjectMemoryFactAddDispositionV1,
        closest_fact: Option<ProjectMemoryFactIdV1>,
        similarity_millionths: Option<u32>,
        commit: Option<FactCommitReceipt>,
    },
    Update {
        fact: ProjectMemoryFactIdV1,
        trust_delta_millionths: i32,
        commit: FactCommitReceipt,
    },
    Merge {
        outcome: ProjectMemoryFactMergeOutcomeV1,
    },
    Remove {
        target: ProjectMemoryFactIdV1,
        disposition: ProjectMemoryFactCurationRemoveDispositionV1,
        remaining_fact_count: u64,
        commit: Option<FactCommitReceipt>,
    },
    NormalizeTags {
        fact: ProjectMemoryFactIdV1,
        commit: FactCommitReceipt,
    },
    LinkFacts {
        relation: ProjectMemoryFactCurationLinkEffectV1,
        disposition: ProjectMemoryFactCurationLinkDispositionV1,
        commit: Option<FactCommitReceipt>,
    },
}

impl ProjectMemoryFactCurationOperationEffectV1 {
    pub(in crate::memory::project_memory) fn durable_operation_identity(
        &self,
    ) -> FactStoreResult<Option<String>> {
        let digest = match self {
            Self::NormalizeTags { fact, .. } => Some(canonical_sha256(&(
                "tracedecay.project-memory.curation-normalize-identity.v1",
                fact.fact_id(),
            ))?),
            Self::LinkFacts { relation, .. } => Some(canonical_sha256(&(
                "tracedecay.project-memory.curation-link-identity.v1",
                relation.owner(),
                relation.source_fact_id(),
                relation.target_fact_id(),
                relation.relation(),
            ))?),
            Self::Add { .. } | Self::Update { .. } | Self::Merge { .. } | Self::Remove { .. } => {
                None
            }
        };
        Ok(digest.map(|digest| digest.as_str().to_owned()))
    }

    pub fn add(outcome: &ProjectMemoryFactAddOutcomeV1) -> FactStoreResult<Self> {
        Self::add_snapshot(
            ProjectMemoryFactIdV1::new(
                outcome.fact().owner().clone(),
                outcome.fact().fact_id().clone(),
            )?,
            outcome.disposition(),
            outcome.closest_fact_id().cloned(),
            outcome.similarity_millionths(),
            outcome.commit_receipt().cloned(),
        )
    }

    pub(in crate::memory::project_memory) fn add_snapshot(
        fact: ProjectMemoryFactIdV1,
        disposition: ProjectMemoryFactAddDispositionV1,
        closest_fact: Option<ProjectMemoryFactIdV1>,
        similarity_millionths: Option<u32>,
        commit: Option<FactCommitReceipt>,
    ) -> FactStoreResult<Self> {
        let commit_matches = commit.as_ref().is_some_and(|commit| {
            commit.owner() == fact.owner() && commit.fact_id() == fact.fact_id()
        });
        let comparison_matches = closest_fact.as_ref().is_some_and(|closest| {
            closest.owner() == fact.owner() && closest.fact_id() != fact.fact_id()
        }) && similarity_millionths
            .is_some_and(|value| value <= 1_000_000);
        let valid = match disposition {
            ProjectMemoryFactAddDispositionV1::Added => {
                commit_matches && closest_fact.is_none() && similarity_millionths.is_none()
            }
            ProjectMemoryFactAddDispositionV1::NearDuplicate => {
                (commit.is_none()
                    && closest_fact.as_ref() == Some(&fact)
                    && similarity_millionths == Some(1_000_000))
                    || (commit_matches && comparison_matches)
            }
            ProjectMemoryFactAddDispositionV1::PossibleConflict => {
                commit_matches && comparison_matches
            }
        };
        if !valid {
            return Err(FactStoreError::Contract(DomainError::SnapshotMismatch {
                field: "curation add effect",
            }));
        }
        Ok(Self::Add {
            fact,
            disposition,
            closest_fact,
            similarity_millionths,
            commit,
        })
    }

    pub fn update(outcome: &ProjectMemoryFactUpdateOutcomeV1) -> FactStoreResult<Self> {
        let fact = ProjectMemoryFactIdV1::new(
            outcome.fact().owner().clone(),
            outcome.fact().fact_id().clone(),
        )?;
        Self::update_snapshot(
            fact,
            outcome.trust_delta_millionths(),
            outcome.commit_receipt().clone(),
        )
    }

    pub(in crate::memory::project_memory) fn update_snapshot(
        fact: ProjectMemoryFactIdV1,
        trust_delta_millionths: i32,
        commit: FactCommitReceipt,
    ) -> FactStoreResult<Self> {
        if !(-1_000_000..=1_000_000).contains(&trust_delta_millionths)
            || commit.owner() != fact.owner()
            || commit.fact_id() != fact.fact_id()
        {
            return Err(FactStoreError::Contract(DomainError::SnapshotMismatch {
                field: "curation update effect",
            }));
        }
        Ok(Self::Update {
            fact,
            trust_delta_millionths,
            commit,
        })
    }

    pub fn merge(outcome: ProjectMemoryFactMergeOutcomeV1) -> Self {
        Self::Merge { outcome }
    }

    pub fn remove(
        target: ProjectMemoryFactIdV1,
        outcome: &ProjectMemoryFactRemoveOutcomeV1,
    ) -> FactStoreResult<Self> {
        let disposition = if outcome.was_removed() {
            ProjectMemoryFactCurationRemoveDispositionV1::Removed
        } else if outcome.fact().is_some() {
            ProjectMemoryFactCurationRemoveDispositionV1::AlreadyRemoved
        } else {
            ProjectMemoryFactCurationRemoveDispositionV1::NotFound
        };
        if outcome.fact().is_some_and(|fact| {
            fact.owner() != target.owner() || fact.fact_id() != target.fact_id()
        }) {
            return Err(FactStoreError::FactMismatch);
        }
        Self::remove_snapshot(
            target,
            disposition,
            outcome.remaining_fact_count(),
            outcome.commit_receipt().cloned(),
        )
    }

    pub(in crate::memory::project_memory) fn remove_snapshot(
        target: ProjectMemoryFactIdV1,
        disposition: ProjectMemoryFactCurationRemoveDispositionV1,
        remaining_fact_count: u64,
        commit: Option<FactCommitReceipt>,
    ) -> FactStoreResult<Self> {
        let receipt_matches = commit.as_ref().is_some_and(|commit| {
            commit.owner() == target.owner() && commit.fact_id() == target.fact_id()
        });
        let valid_commit = match disposition {
            ProjectMemoryFactCurationRemoveDispositionV1::Removed => receipt_matches,
            ProjectMemoryFactCurationRemoveDispositionV1::AlreadyRemoved
            | ProjectMemoryFactCurationRemoveDispositionV1::NotFound => commit.is_none(),
        };
        if !valid_commit {
            return Err(FactStoreError::Contract(DomainError::SnapshotMismatch {
                field: "curation remove effect",
            }));
        }
        Ok(Self::Remove {
            target,
            disposition,
            remaining_fact_count,
            commit,
        })
    }

    pub fn normalize_tags(
        fact: ProjectMemoryFactIdV1,
        commit: FactCommitReceipt,
    ) -> FactStoreResult<Self> {
        if commit.owner() != fact.owner() || commit.fact_id() != fact.fact_id() {
            return Err(FactStoreError::Contract(DomainError::SnapshotMismatch {
                field: "curation normalization effect commit",
            }));
        }
        Ok(Self::NormalizeTags { fact, commit })
    }

    pub fn link_facts(
        relation: FactRelationV1,
        commit: FactCommitReceipt,
    ) -> FactStoreResult<Self> {
        if commit.owner() != relation.owner() || commit.fact_id() != relation.source_fact_id() {
            return Err(FactStoreError::Contract(DomainError::SnapshotMismatch {
                field: "curation relation effect commit",
            }));
        }
        Self::link_facts_snapshot(
            ProjectMemoryFactCurationLinkEffectV1::from_relation(&relation)?,
            ProjectMemoryFactCurationLinkDispositionV1::Linked,
            Some(commit),
        )
    }

    pub fn already_linked(relation: FactRelationV1) -> FactStoreResult<Self> {
        Self::link_facts_snapshot(
            ProjectMemoryFactCurationLinkEffectV1::from_relation(&relation)?,
            ProjectMemoryFactCurationLinkDispositionV1::AlreadyLinked,
            None,
        )
    }

    pub(in crate::memory::project_memory) fn link_facts_snapshot(
        relation: ProjectMemoryFactCurationLinkEffectV1,
        disposition: ProjectMemoryFactCurationLinkDispositionV1,
        commit: Option<FactCommitReceipt>,
    ) -> FactStoreResult<Self> {
        relation.validate()?;
        let valid = match (&disposition, &commit) {
            (ProjectMemoryFactCurationLinkDispositionV1::Linked, Some(commit)) => {
                commit.owner() == relation.owner() && commit.fact_id() == relation.source_fact_id()
            }
            (ProjectMemoryFactCurationLinkDispositionV1::AlreadyLinked, None) => true,
            _ => false,
        };
        if !valid {
            return Err(FactStoreError::Contract(DomainError::SnapshotMismatch {
                field: "curation relation effect commit",
            }));
        }
        Ok(Self::LinkFacts {
            relation,
            disposition,
            commit,
        })
    }

    pub fn commit_receipts(&self) -> Vec<&FactCommitReceipt> {
        match self {
            Self::Add { commit, .. } | Self::Remove { commit, .. } => commit.iter().collect(),
            Self::Update { commit, .. } | Self::NormalizeTags { commit, .. } => vec![commit],
            Self::LinkFacts { commit, .. } => commit.iter().collect(),
            Self::Merge { outcome } => outcome.commit_receipts().iter().collect(),
        }
    }

    pub fn primary_commit(&self) -> Option<&FactCommitReceipt> {
        match self {
            Self::Add { commit, .. } | Self::Remove { commit, .. } => commit.as_ref(),
            Self::Update { commit, .. } | Self::NormalizeTags { commit, .. } => Some(commit),
            Self::LinkFacts { commit, .. } => commit.as_ref(),
            Self::Merge { outcome } => outcome.commit_receipts().first(),
        }
    }

    pub(in crate::memory::project_memory) fn changed_facts(
        &self,
    ) -> FactStoreResult<Vec<ProjectMemoryFactIdV1>> {
        Ok(match self {
            Self::Add { fact, commit, .. } => commit.iter().map(|_| fact.clone()).collect(),
            Self::Update { fact, .. } | Self::NormalizeTags { fact, .. } => vec![fact.clone()],
            Self::Merge { outcome } => {
                let mut facts = Vec::with_capacity(
                    outcome.deleted_losers().len() + usize::from(outcome.content_updated()),
                );
                if outcome.content_updated() {
                    facts.push(outcome.winner().clone());
                }
                facts.extend_from_slice(outcome.deleted_losers());
                facts
            }
            Self::Remove {
                target,
                disposition,
                ..
            } if *disposition == ProjectMemoryFactCurationRemoveDispositionV1::Removed => {
                vec![target.clone()]
            }
            Self::Remove { .. } => Vec::new(),
            Self::LinkFacts {
                relation, commit, ..
            } if commit.is_some() => vec![
                ProjectMemoryFactIdV1::new(
                    relation.owner().clone(),
                    relation.source_fact_id().clone(),
                )?,
                ProjectMemoryFactIdV1::new(
                    relation.owner().clone(),
                    relation.target_fact_id().clone(),
                )?,
            ],
            Self::LinkFacts { .. } => Vec::new(),
        })
    }

    pub fn matches_add_outcome(&self, outcome: &ProjectMemoryFactAddOutcomeV1) -> bool {
        matches!(
            self,
            Self::Add { fact, disposition, closest_fact, similarity_millionths, commit }
                if fact.owner() == outcome.fact().owner()
                    && fact.fact_id() == outcome.fact().fact_id()
                    && *disposition == outcome.disposition()
                    && closest_fact.as_ref() == outcome.closest_fact_id()
                    && *similarity_millionths == outcome.similarity_millionths()
                    && commit.as_ref() == outcome.commit_receipt()
        )
    }

    pub fn matches_update_outcome(&self, outcome: &ProjectMemoryFactUpdateOutcomeV1) -> bool {
        matches!(
            self,
            Self::Update { fact, trust_delta_millionths, commit }
                if fact.owner() == outcome.fact().owner()
                    && fact.fact_id() == outcome.fact().fact_id()
                    && *trust_delta_millionths == outcome.trust_delta_millionths()
                    && commit == outcome.commit_receipt()
        )
    }

    pub fn matches_merge_outcome(&self, outcome: &ProjectMemoryFactMergeOutcomeV1) -> bool {
        matches!(
            self,
            Self::Merge { outcome: expected }
                if expected.owner() == outcome.owner()
                    && expected.operation_id() == outcome.operation_id()
                    && expected.input_digest() == outcome.input_digest()
                    && expected.winner() == outcome.winner()
                    && expected.content_updated() == outcome.content_updated()
                    && expected.deleted_losers() == outcome.deleted_losers()
                    && expected.commit_receipts() == outcome.commit_receipts()
        )
    }

    pub fn matches_remove_outcome(
        &self,
        expected_target: &ProjectMemoryFactIdV1,
        outcome: &ProjectMemoryFactRemoveOutcomeV1,
    ) -> bool {
        matches!(
            self,
            Self::Remove { target, disposition, remaining_fact_count, commit }
                if target == expected_target
                    && *remaining_fact_count == outcome.remaining_fact_count()
                    && *disposition == if outcome.was_removed() {
                        ProjectMemoryFactCurationRemoveDispositionV1::Removed
                    } else if outcome.fact().is_some() {
                        ProjectMemoryFactCurationRemoveDispositionV1::AlreadyRemoved
                    } else {
                        ProjectMemoryFactCurationRemoveDispositionV1::NotFound
                    }
                    && outcome.fact().is_none_or(|fact| {
                        fact.owner() == target.owner() && fact.fact_id() == target.fact_id()
                    })
                    && commit.as_ref() == outcome.commit_receipt()
        )
    }
}

/// Payload-free, sanitizer-bound snapshot of one durable relation effect.
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ProjectMemoryFactCurationLinkEffectV1 {
    owner: FactOwnerV1,
    source_fact_id: FactId,
    target_fact_id: FactId,
    relation: FactRelationKindV1,
    evidence_fact_ids: Vec<FactId>,
    confidence: Confidence,
    source_label: String,
    provenance_reference: PayloadReferenceV1,
    sanitization_receipt: SanitizationReceiptRefV1,
    sanitization_disposition: SanitizerDispositionV1,
    sanitization_sensitivity: SensitivityV1,
}

impl<'de> Deserialize<'de> for ProjectMemoryFactCurationLinkEffectV1 {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Wire {
            owner: FactOwnerV1,
            source_fact_id: FactId,
            target_fact_id: FactId,
            relation: FactRelationKindV1,
            evidence_fact_ids: Vec<FactId>,
            confidence: Confidence,
            source_label: String,
            provenance_reference: PayloadReferenceV1,
            sanitization_receipt: SanitizationReceiptRefV1,
            sanitization_disposition: SanitizerDispositionV1,
            sanitization_sensitivity: SensitivityV1,
        }

        let wire = Wire::deserialize(deserializer)?;
        let snapshot = Self {
            owner: wire.owner,
            source_fact_id: wire.source_fact_id,
            target_fact_id: wire.target_fact_id,
            relation: wire.relation,
            evidence_fact_ids: wire.evidence_fact_ids,
            confidence: wire.confidence,
            source_label: wire.source_label,
            provenance_reference: wire.provenance_reference,
            sanitization_receipt: wire.sanitization_receipt,
            sanitization_disposition: wire.sanitization_disposition,
            sanitization_sensitivity: wire.sanitization_sensitivity,
        };
        snapshot.validate().map_err(serde::de::Error::custom)?;
        Ok(snapshot)
    }
}

impl ProjectMemoryFactCurationLinkEffectV1 {
    fn from_relation(relation: &FactRelationV1) -> FactStoreResult<Self> {
        let sanitization = relation.provenance().sanitization_receipt();
        let provenance_reference = sanitization.payload().cloned().ok_or_else(|| {
            FactStoreError::Contract(DomainError::NonCanonical {
                field: "curation relation provenance reference",
            })
        })?;
        Ok(Self {
            owner: relation.owner().clone(),
            source_fact_id: relation.source_fact_id().clone(),
            target_fact_id: relation.target_fact_id().clone(),
            relation: relation.kind(),
            evidence_fact_ids: relation.evidence_fact_ids().to_vec(),
            confidence: relation.confidence(),
            source_label: relation.source_label().to_owned(),
            provenance_reference,
            sanitization_receipt: sanitization.receipt().clone(),
            sanitization_disposition: sanitization.disposition(),
            sanitization_sensitivity: sanitization.sensitivity(),
        })
    }

    pub fn owner(&self) -> &FactOwnerV1 {
        &self.owner
    }

    pub fn source_fact_id(&self) -> &FactId {
        &self.source_fact_id
    }

    pub fn target_fact_id(&self) -> &FactId {
        &self.target_fact_id
    }

    pub fn relation(&self) -> FactRelationKindV1 {
        self.relation
    }

    pub fn evidence_fact_ids(&self) -> &[FactId] {
        &self.evidence_fact_ids
    }

    pub fn confidence(&self) -> Confidence {
        self.confidence
    }

    pub fn source_label(&self) -> &str {
        &self.source_label
    }

    pub fn provenance_reference(&self) -> &PayloadReferenceV1 {
        &self.provenance_reference
    }

    pub fn sanitization_receipt(&self) -> &SanitizationReceiptRefV1 {
        &self.sanitization_receipt
    }

    pub fn sanitization_disposition(&self) -> SanitizerDispositionV1 {
        self.sanitization_disposition
    }

    pub fn sanitization_sensitivity(&self) -> SensitivityV1 {
        self.sanitization_sensitivity
    }

    pub fn sanitization_receipt_value(&self) -> FactStoreResult<SanitizationReceiptV1> {
        SanitizationReceiptV1::new(
            self.sanitization_receipt.clone(),
            self.sanitization_disposition,
            self.sanitization_sensitivity,
            Some(self.provenance_reference.clone()),
        )
        .map_err(|_| {
            FactStoreError::Contract(DomainError::SnapshotMismatch {
                field: "curation relation sanitization receipt",
            })
        })
    }

    fn validate(&self) -> FactStoreResult<()> {
        self.owner.validate()?;
        self.source_fact_id.validate_owner(&self.owner)?;
        self.target_fact_id.validate_owner(&self.owner)?;
        if self.source_fact_id == self.target_fact_id {
            return Err(FactStoreError::Contract(DomainError::NonCanonical {
                field: "curation relation effect endpoints",
            }));
        }
        if self.evidence_fact_ids.is_empty()
            || self.evidence_fact_ids.len() > MAX_PROJECT_MEMORY_CURATION_TARGETS
            || self
                .evidence_fact_ids
                .iter()
                .any(|fact_id| fact_id.validate_owner(&self.owner).is_err())
            || self
                .evidence_fact_ids
                .windows(2)
                .any(|pair| pair[0] >= pair[1])
            || !self.sanitization_disposition.permits_durable_payload()
            || self.sanitization_sensitivity == SensitivityV1::Unclassified
            || self.provenance_reference.byte_len() == 0
            || self.sanitization_receipt_value().is_err()
        {
            return Err(FactStoreError::Contract(DomainError::NonCanonical {
                field: "curation relation effect provenance",
            }));
        }
        validate_project_memory_text(&self.source_label, "curation relation source label")
    }

    pub fn matches_relation(&self, relation: &FactRelationV1) -> bool {
        matches!(Self::from_relation(relation), Ok(snapshot) if snapshot == *self)
    }
}
