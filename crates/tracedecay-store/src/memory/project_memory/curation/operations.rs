use std::collections::BTreeSet;

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use tracedecay_domain::{
    ActorId, Confidence, DomainError, FactEventId, FactId, FactOwnerV1, FactRelationV1,
    ProvenanceId,
};

use super::super::super::queries::validate_limit;
use super::super::super::{FactCommitReceipt, FactStoreError, FactStoreResult};
use super::super::{
    ProjectMemoryFactIdV1, validate_project_memory_entity, validate_project_memory_text,
};
use super::validate::{
    validate_curation_confidence, validate_curation_evidence, validate_curation_fact_target,
};
use super::{MAX_PROJECT_MEMORY_CURATION_OPERATIONS, MAX_PROJECT_MEMORY_CURATION_TARGETS};

/// Stable owner-scoped identity for a canonical entity projection.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct ProjectMemoryEntityIdV1 {
    owner: FactOwnerV1,
    entity: String,
}

impl ProjectMemoryEntityIdV1 {
    pub fn new(owner: FactOwnerV1, entity: String) -> FactStoreResult<Self> {
        owner.validate()?;
        validate_project_memory_entity(&entity)?;
        Ok(Self { owner, entity })
    }

    pub fn owner(&self) -> &FactOwnerV1 {
        &self.owner
    }

    pub fn entity(&self) -> &str {
        &self.entity
    }

    pub(in crate::memory::project_memory) fn validate(&self) -> FactStoreResult<()> {
        self.owner.validate()?;
        validate_project_memory_entity(&self.entity)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProjectMemoryFactNormalizeTagsV1 {
    fact: ProjectMemoryFactIdV1,
    tags: Vec<String>,
    evidence_facts: Vec<ProjectMemoryFactIdV1>,
    confidence: Confidence,
}

impl ProjectMemoryFactNormalizeTagsV1 {
    pub fn new(
        fact: ProjectMemoryFactIdV1,
        tags: Vec<String>,
        evidence_facts: Vec<ProjectMemoryFactIdV1>,
        confidence: Confidence,
    ) -> FactStoreResult<Self> {
        if tags.len() > MAX_PROJECT_MEMORY_CURATION_TARGETS {
            return Err(FactStoreError::InvalidQueryLimit {
                limit: tags.len(),
                max: MAX_PROJECT_MEMORY_CURATION_TARGETS,
            });
        }
        for tag in &tags {
            validate_project_memory_text(tag, "curation tag")?;
        }
        Ok(Self {
            fact,
            tags,
            evidence_facts,
            confidence,
        })
    }

    pub fn fact(&self) -> &ProjectMemoryFactIdV1 {
        &self.fact
    }

    pub fn tags(&self) -> &[String] {
        &self.tags
    }

    pub fn evidence_facts(&self) -> &[ProjectMemoryFactIdV1] {
        &self.evidence_facts
    }

    pub fn confidence(&self) -> Confidence {
        self.confidence
    }
}

/// Thin curation input over immutable, receipt-bound domain relation material.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProjectMemoryFactLinkV1 {
    relation: FactRelationV1,
}

impl ProjectMemoryFactLinkV1 {
    pub fn new(relation: FactRelationV1) -> FactStoreResult<Self> {
        relation.owner().validate()?;
        Ok(Self { relation })
    }

    pub fn relation(&self) -> &FactRelationV1 {
        &self.relation
    }
}

/// Finite set of curation operations; this is intentionally not a generic
/// command dispatcher.
#[derive(Clone, Debug, PartialEq)]
pub enum ProjectMemoryFactCurationOperationV1 {
    NormalizeTags(ProjectMemoryFactNormalizeTagsV1),
    LinkFacts(ProjectMemoryFactLinkV1),
}

impl ProjectMemoryFactCurationOperationV1 {
    fn validate_for(&self, owner: &FactOwnerV1, min_confidence: Confidence) -> FactStoreResult<()> {
        match self {
            Self::NormalizeTags(operation) => {
                validate_curation_fact_target(owner, operation.fact())?;
                validate_curation_evidence(owner, operation.evidence_facts())?;
                validate_curation_confidence(operation.confidence(), min_confidence)
            }
            Self::LinkFacts(operation) => {
                if operation.relation().owner() != owner {
                    return Err(FactStoreError::OwnerMismatch);
                }
                validate_curation_confidence(operation.relation().confidence(), min_confidence)
            }
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct ProjectMemoryFactCurationBatchV1 {
    owner: FactOwnerV1,
    operation_id: ProvenanceId,
    actor: Option<ActorId>,
    min_confidence: Confidence,
    operations: Vec<ProjectMemoryFactCurationOperationV1>,
}

impl ProjectMemoryFactCurationBatchV1 {
    pub fn new(
        owner: FactOwnerV1,
        operation_id: ProvenanceId,
        actor: Option<ActorId>,
        min_confidence: Confidence,
        operations: Vec<ProjectMemoryFactCurationOperationV1>,
    ) -> FactStoreResult<Self> {
        owner.validate()?;
        operation_id.validate()?;
        if let Some(actor) = &actor {
            actor.validate()?;
        }
        validate_limit(operations.len(), MAX_PROJECT_MEMORY_CURATION_OPERATIONS)?;
        for operation in &operations {
            operation.validate_for(&owner, min_confidence)?;
        }
        Ok(Self {
            owner,
            operation_id,
            actor,
            min_confidence,
            operations,
        })
    }

    pub fn owner(&self) -> &FactOwnerV1 {
        &self.owner
    }

    pub fn operation_id(&self) -> &ProvenanceId {
        &self.operation_id
    }

    pub fn actor(&self) -> Option<&ActorId> {
        self.actor.as_ref()
    }

    pub fn min_confidence(&self) -> Confidence {
        self.min_confidence
    }

    pub fn operations(&self) -> &[ProjectMemoryFactCurationOperationV1] {
        &self.operations
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProjectMemoryFactCurationReceiptV1 {
    owner: FactOwnerV1,
    operation_id: ProvenanceId,
    input_digest: String,
    commit_receipts: Vec<FactCommitReceipt>,
    replay_fact_id: FactId,
    replay_event_id: FactEventId,
    changed_facts: Vec<ProjectMemoryFactIdV1>,
    normalized_tags: u64,
    facts_linked: u64,
    // Delivery disposition is derived at the operation boundary and is not
    // part of the durable receipt identity serialized below.
    replayed: bool,
}

#[derive(Serialize)]
#[serde(deny_unknown_fields)]
struct ProjectMemoryFactCurationReceiptRef<'a> {
    owner: &'a FactOwnerV1,
    operation_id: &'a ProvenanceId,
    input_digest: &'a str,
    commit_receipts: &'a [FactCommitReceipt],
    replay_fact_id: &'a FactId,
    replay_event_id: &'a FactEventId,
    changed_fact_ids: Vec<&'a FactId>,
    normalized_tags: u64,
    facts_linked: u64,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ProjectMemoryFactCurationReceiptWire {
    owner: FactOwnerV1,
    operation_id: ProvenanceId,
    input_digest: String,
    commit_receipts: Vec<FactCommitReceipt>,
    replay_fact_id: FactId,
    replay_event_id: FactEventId,
    changed_fact_ids: Vec<FactId>,
    normalized_tags: u64,
    facts_linked: u64,
}

impl Serialize for ProjectMemoryFactCurationReceiptV1 {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        ProjectMemoryFactCurationReceiptRef {
            owner: self.owner(),
            operation_id: self.operation_id(),
            input_digest: self.input_digest(),
            commit_receipts: self.commit_receipts(),
            replay_fact_id: self.replay_fact_id(),
            replay_event_id: self.replay_event_id(),
            changed_fact_ids: self
                .changed_facts()
                .iter()
                .map(ProjectMemoryFactIdV1::fact_id)
                .collect(),
            normalized_tags: self.normalized_tags(),
            facts_linked: self.facts_linked(),
        }
        .serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for ProjectMemoryFactCurationReceiptV1 {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let wire = ProjectMemoryFactCurationReceiptWire::deserialize(deserializer)?;
        let changed_facts = wire
            .changed_fact_ids
            .into_iter()
            .map(|fact_id| ProjectMemoryFactIdV1::new(wire.owner.clone(), fact_id))
            .collect::<FactStoreResult<Vec<_>>>()
            .map_err(serde::de::Error::custom)?;
        let receipt = Self::new(
            wire.owner,
            wire.operation_id,
            wire.input_digest,
            wire.commit_receipts,
            changed_facts,
            wire.normalized_tags,
            wire.facts_linked,
        )
        .map_err(serde::de::Error::custom)?;
        if receipt.replay_fact_id() != &wire.replay_fact_id
            || receipt.replay_event_id() != &wire.replay_event_id
        {
            return Err(serde::de::Error::custom(
                "curation receipt replay pointers do not match its first commit",
            ));
        }
        Ok(receipt)
    }
}

impl ProjectMemoryFactCurationReceiptV1 {
    pub fn new(
        owner: FactOwnerV1,
        operation_id: ProvenanceId,
        input_digest: String,
        commit_receipts: Vec<FactCommitReceipt>,
        changed_facts: Vec<ProjectMemoryFactIdV1>,
        normalized_tags: u64,
        facts_linked: u64,
    ) -> FactStoreResult<Self> {
        owner.validate()?;
        operation_id.validate()?;
        if input_digest.len() != 64
            || !input_digest
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            return Err(FactStoreError::Contract(DomainError::NonCanonical {
                field: "curation receipt input digest",
            }));
        }
        let first_commit = commit_receipts.first().ok_or_else(|| {
            FactStoreError::Contract(DomainError::Empty {
                field: "curation receipt commit receipts",
            })
        })?;
        if commit_receipts.len() > MAX_PROJECT_MEMORY_CURATION_OPERATIONS
            || commit_receipts
                .iter()
                .any(|receipt| receipt.owner() != &owner)
        {
            return Err(FactStoreError::Contract(DomainError::NonCanonical {
                field: "curation receipt commit receipts",
            }));
        }
        let mut committed_event_ids = BTreeSet::new();
        for receipt in &commit_receipts {
            for event_id in receipt.committed_event_ids() {
                if !committed_event_ids.insert(event_id) {
                    return Err(FactStoreError::Contract(DomainError::DuplicateId {
                        field: "curation receipt committed events",
                    }));
                }
            }
        }
        if changed_facts.len() > MAX_PROJECT_MEMORY_CURATION_TARGETS
            || changed_facts
                .iter()
                .any(|mapping| mapping.owner() != &owner)
            || changed_facts.iter().enumerate().any(|(index, mapping)| {
                changed_facts[..index]
                    .iter()
                    .any(|previous| previous.fact_id() == mapping.fact_id())
            })
        {
            return Err(FactStoreError::Contract(DomainError::NonCanonical {
                field: "curation receipt fact identities",
            }));
        }
        let replay_fact_id = first_commit.fact_id().clone();
        let replay_event_id = first_commit.last_event_id().clone();
        Ok(Self {
            owner,
            operation_id,
            input_digest,
            commit_receipts,
            replay_fact_id,
            replay_event_id,
            changed_facts,
            normalized_tags,
            facts_linked,
            replayed: false,
        })
    }

    pub fn into_replayed(mut self) -> Self {
        self.replayed = true;
        self
    }

    pub fn owner(&self) -> &FactOwnerV1 {
        &self.owner
    }

    pub fn operation_id(&self) -> &ProvenanceId {
        &self.operation_id
    }

    pub fn input_digest(&self) -> &str {
        &self.input_digest
    }

    pub fn commit_receipts(&self) -> &[FactCommitReceipt] {
        &self.commit_receipts
    }

    pub fn replay_fact_id(&self) -> &FactId {
        &self.replay_fact_id
    }

    pub fn replay_event_id(&self) -> &FactEventId {
        &self.replay_event_id
    }

    pub fn changed_facts(&self) -> &[ProjectMemoryFactIdV1] {
        &self.changed_facts
    }

    pub fn normalized_tags(&self) -> u64 {
        self.normalized_tags
    }

    pub fn facts_linked(&self) -> u64 {
        self.facts_linked
    }

    pub fn replayed(&self) -> bool {
        self.replayed
    }
}
