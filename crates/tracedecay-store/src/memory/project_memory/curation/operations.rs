use std::collections::BTreeSet;

use serde_json::{Value, json};
use tracedecay_domain::canonical_text::sha256_hex;
use tracedecay_domain::{
    ActorId, Confidence, DomainError, FactEventId, FactOwnerV1, ProvenanceId, RunId,
    canonical_sha256,
};

use super::super::super::queries::validate_limit;
use super::super::super::{FactStoreError, FactStoreResult};
use super::super::{
    ProjectMemoryFactIdV1, validate_project_memory_entity, validate_project_memory_text,
};
use super::validate::{
    validate_curation_confidence, validate_curation_evidence, validate_curation_fact_target,
};
use super::{
    MAX_PROJECT_MEMORY_CURATION_OPERATIONS, MAX_PROJECT_MEMORY_CURATION_TARGETS,
    ProjectMemoryFactCurationAddV1, ProjectMemoryFactCurationMergeV1,
    ProjectMemoryFactCurationRemoveV1, ProjectMemoryFactCurationUpdateV1,
};

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

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct ProjectMemoryFactCurationReviewRefV1 {
    fact: ProjectMemoryFactIdV1,
    expected_last_event_id: FactEventId,
}

impl ProjectMemoryFactCurationReviewRefV1 {
    pub fn new(fact: ProjectMemoryFactIdV1, expected_last_event_id: FactEventId) -> Self {
        Self {
            fact,
            expected_last_event_id,
        }
    }

    pub fn fact(&self) -> &ProjectMemoryFactIdV1 {
        &self.fact
    }

    pub fn expected_last_event_id(&self) -> &FactEventId {
        &self.expected_last_event_id
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProjectMemoryFactNormalizeTagsV1 {
    fact: ProjectMemoryFactCurationReviewRefV1,
    tags: Vec<String>,
    evidence_facts: Vec<ProjectMemoryFactCurationReviewRefV1>,
    confidence: Confidence,
}

impl ProjectMemoryFactNormalizeTagsV1 {
    pub fn new(
        fact: ProjectMemoryFactCurationReviewRefV1,
        tags: Vec<String>,
        evidence_facts: Vec<ProjectMemoryFactCurationReviewRefV1>,
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

    pub fn fact(&self) -> &ProjectMemoryFactCurationReviewRefV1 {
        &self.fact
    }

    pub fn tags(&self) -> &[String] {
        &self.tags
    }

    pub fn evidence_facts(&self) -> &[ProjectMemoryFactCurationReviewRefV1] {
        &self.evidence_facts
    }

    pub fn confidence(&self) -> Confidence {
        self.confidence
    }
}

/// Thin curation input over immutable, receipt-bound domain relation material.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProjectMemoryFactLinkV1 {
    relation: tracedecay_domain::FactRelationV1,
    source: ProjectMemoryFactCurationReviewRefV1,
    target: ProjectMemoryFactCurationReviewRefV1,
    evidence_facts: Vec<ProjectMemoryFactCurationReviewRefV1>,
}

impl ProjectMemoryFactLinkV1 {
    pub fn new(
        relation: tracedecay_domain::FactRelationV1,
        source: ProjectMemoryFactCurationReviewRefV1,
        target: ProjectMemoryFactCurationReviewRefV1,
        evidence_facts: Vec<ProjectMemoryFactCurationReviewRefV1>,
    ) -> FactStoreResult<Self> {
        relation.owner().validate()?;
        if source.fact().owner() != relation.owner()
            || target.fact().owner() != relation.owner()
            || source.fact().fact_id() != relation.source_fact_id()
            || target.fact().fact_id() != relation.target_fact_id()
            || evidence_facts
                .iter()
                .map(|fact| fact.fact().fact_id())
                .collect::<Vec<_>>()
                != relation.evidence_fact_ids().iter().collect::<Vec<_>>()
        {
            return Err(FactStoreError::Contract(DomainError::SnapshotMismatch {
                field: "curation relation reviewed facts",
            }));
        }
        Ok(Self {
            relation,
            source,
            target,
            evidence_facts,
        })
    }

    pub fn relation(&self) -> &tracedecay_domain::FactRelationV1 {
        &self.relation
    }

    pub fn source(&self) -> &ProjectMemoryFactCurationReviewRefV1 {
        &self.source
    }
    pub fn target(&self) -> &ProjectMemoryFactCurationReviewRefV1 {
        &self.target
    }
    pub fn evidence_facts(&self) -> &[ProjectMemoryFactCurationReviewRefV1] {
        &self.evidence_facts
    }
}

/// Finite set of canonical curation operations executed in one outer write.
#[derive(Clone, Debug, PartialEq)]
pub enum ProjectMemoryFactCurationOperationV1 {
    Add(ProjectMemoryFactCurationAddV1),
    Update(ProjectMemoryFactCurationUpdateV1),
    Merge(ProjectMemoryFactCurationMergeV1),
    Remove(ProjectMemoryFactCurationRemoveV1),
    NormalizeTags(ProjectMemoryFactNormalizeTagsV1),
    LinkFacts(ProjectMemoryFactLinkV1),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProjectMemoryFactCurationMutationKindV1 {
    Add,
    Update,
    Merge,
    Remove,
}

impl ProjectMemoryFactCurationMutationKindV1 {
    fn as_str(self) -> &'static str {
        match self {
            Self::Add => "add",
            Self::Update => "update",
            Self::Merge => "merge",
            Self::Remove => "remove",
        }
    }
}

pub fn derive_project_memory_fact_curation_child_operation_id(
    outer_operation_id: &ProvenanceId,
    operation_index: usize,
    kind: ProjectMemoryFactCurationMutationKindV1,
) -> FactStoreResult<ProvenanceId> {
    outer_operation_id.validate()?;
    let operation_index = u64::try_from(operation_index).map_err(|_| {
        FactStoreError::Contract(DomainError::NonCanonical {
            field: "curation child operation index",
        })
    })?;
    let digest = canonical_sha256(&(
        "tracedecay.project-memory.curation-child.v1",
        outer_operation_id,
        operation_index,
        kind.as_str(),
    ))?;
    ProvenanceId::new(format!("memory-curation-child.{digest}")).map_err(FactStoreError::Contract)
}

impl ProjectMemoryFactCurationOperationV1 {
    pub fn child_operation_id(&self) -> Option<&ProvenanceId> {
        match self {
            Self::Add(operation) => Some(operation.command().operation_id()),
            Self::Update(operation) => Some(operation.command().operation_id()),
            Self::Merge(operation) => Some(operation.command().operation_id()),
            Self::Remove(operation) => Some(operation.command().operation_id()),
            Self::NormalizeTags(_) | Self::LinkFacts(_) => None,
        }
    }

    fn mutation_kind(&self) -> Option<ProjectMemoryFactCurationMutationKindV1> {
        match self {
            Self::Add(_) => Some(ProjectMemoryFactCurationMutationKindV1::Add),
            Self::Update(_) => Some(ProjectMemoryFactCurationMutationKindV1::Update),
            Self::Merge(_) => Some(ProjectMemoryFactCurationMutationKindV1::Merge),
            Self::Remove(_) => Some(ProjectMemoryFactCurationMutationKindV1::Remove),
            Self::NormalizeTags(_) | Self::LinkFacts(_) => None,
        }
    }

    fn operation_identity(&self) -> FactStoreResult<String> {
        match self {
            Self::Add(operation) => Ok(operation.command().operation_id().as_str().to_owned()),
            Self::Update(operation) => Ok(operation.command().operation_id().as_str().to_owned()),
            Self::Merge(operation) => Ok(operation.command().operation_id().as_str().to_owned()),
            Self::Remove(operation) => Ok(operation.command().operation_id().as_str().to_owned()),
            Self::NormalizeTags(operation) => canonical_sha256(&(
                "tracedecay.project-memory.curation-normalize-identity.v1",
                operation.fact().fact().fact_id(),
            ))
            .map(|digest| digest.as_str().to_owned())
            .map_err(FactStoreError::from),
            Self::LinkFacts(operation) => canonical_sha256(&(
                "tracedecay.project-memory.curation-link-identity.v1",
                operation.relation().owner(),
                operation.relation().source_fact_id(),
                operation.relation().target_fact_id(),
                operation.relation().kind(),
            ))
            .map(|digest| digest.as_str().to_owned())
            .map_err(FactStoreError::from),
        }
    }

    fn validate_for(
        &self,
        owner: &FactOwnerV1,
        actor: Option<&ActorId>,
        automation_run_id: Option<&RunId>,
        min_confidence: Confidence,
    ) -> FactStoreResult<()> {
        match self {
            Self::Add(operation) => {
                if operation.command().owner() != owner
                    || operation.command().actor() != actor
                    || automation_run_id.is_some()
                        && operation.command().automation_run_id()
                            != automation_run_id.map(RunId::as_str)
                {
                    return Err(FactStoreError::Contract(DomainError::SnapshotMismatch {
                        field: "curation add authority",
                    }));
                }
                validate_curation_evidence(owner, operation.evidence().facts())?;
                validate_curation_confidence(operation.evidence().confidence(), min_confidence)
            }
            Self::Update(operation) => {
                validate_curation_fact_target(owner, operation.command().target())?;
                operation.validate_review_cas()?;
                validate_mutation_authority(
                    owner,
                    actor,
                    operation.command().target().owner(),
                    operation.command().actor(),
                    operation.evidence().facts(),
                    operation.evidence().confidence(),
                    min_confidence,
                )
            }
            Self::Merge(operation) => {
                if operation.command().owner() != owner || operation.command().actor() != actor {
                    return Err(FactStoreError::Contract(DomainError::SnapshotMismatch {
                        field: "curation merge authority",
                    }));
                }
                validate_curation_evidence(owner, operation.evidence().facts())?;
                validate_curation_confidence(operation.evidence().confidence(), min_confidence)
            }
            Self::Remove(operation) => {
                validate_curation_fact_target(owner, operation.command().target())?;
                operation.validate_review_cas()?;
                validate_mutation_authority(
                    owner,
                    actor,
                    operation.command().target().owner(),
                    operation.command().actor(),
                    operation.evidence().facts(),
                    operation.evidence().confidence(),
                    min_confidence,
                )
            }
            Self::NormalizeTags(operation) => {
                validate_curation_fact_target(owner, operation.fact().fact())?;
                validate_curation_evidence(owner, operation.evidence_facts())?;
                validate_curation_confidence(operation.confidence(), min_confidence)
            }
            Self::LinkFacts(operation) => {
                if operation.relation().owner() != owner {
                    return Err(FactStoreError::OwnerMismatch);
                }
                for reviewed in std::iter::once(operation.source())
                    .chain(std::iter::once(operation.target()))
                    .chain(operation.evidence_facts())
                {
                    validate_curation_fact_target(owner, reviewed.fact())?;
                }
                validate_curation_confidence(operation.relation().confidence(), min_confidence)
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn validate_mutation_authority(
    owner: &FactOwnerV1,
    actor: Option<&ActorId>,
    command_owner: &FactOwnerV1,
    command_actor: Option<&ActorId>,
    evidence: &[ProjectMemoryFactCurationReviewRefV1],
    confidence: Confidence,
    min_confidence: Confidence,
) -> FactStoreResult<()> {
    if command_owner != owner || command_actor != actor {
        return Err(FactStoreError::Contract(DomainError::SnapshotMismatch {
            field: "curation mutation authority",
        }));
    }
    validate_curation_evidence(owner, evidence)?;
    validate_curation_confidence(confidence, min_confidence)
}

#[derive(Clone, Debug, PartialEq)]
pub struct ProjectMemoryFactCurationBatchV1 {
    owner: FactOwnerV1,
    operation_id: ProvenanceId,
    actor: Option<ActorId>,
    automation_run_id: Option<RunId>,
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
        validate_changed_fact_capacity(&operations)?;
        validate_child_operation_ids(&operation_id, &operations)?;
        for operation in &operations {
            operation.validate_for(&owner, actor.as_ref(), None, min_confidence)?;
        }
        Ok(Self {
            owner,
            operation_id,
            actor,
            automation_run_id: None,
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

    pub fn with_automation_run_id(mut self, run_id: RunId) -> FactStoreResult<Self> {
        run_id.validate()?;
        for operation in &self.operations {
            operation.validate_for(
                &self.owner,
                self.actor.as_ref(),
                Some(&run_id),
                self.min_confidence,
            )?;
        }
        self.automation_run_id = Some(run_id);
        Ok(self)
    }

    pub fn automation_run_id(&self) -> Option<&RunId> {
        self.automation_run_id.as_ref()
    }

    pub fn min_confidence(&self) -> Confidence {
        self.min_confidence
    }

    pub fn operations(&self) -> &[ProjectMemoryFactCurationOperationV1] {
        &self.operations
    }

    pub fn input_digest(&self) -> FactStoreResult<String> {
        if self.operations.iter().any(|operation| {
            matches!(
                operation,
                ProjectMemoryFactCurationOperationV1::Add(operation)
                    if operation.command().automation_run_id()
                        != self.automation_run_id.as_ref().map(RunId::as_str)
            )
        }) {
            return Err(FactStoreError::Contract(DomainError::SnapshotMismatch {
                field: "curation add automation run",
            }));
        }
        for operation in &self.operations {
            operation.validate_for(
                &self.owner,
                self.actor.as_ref(),
                self.automation_run_id.as_ref(),
                self.min_confidence,
            )?;
        }
        let operations = self
            .operations
            .iter()
            .map(curation_operation_digest)
            .collect::<FactStoreResult<Vec<_>>>()?;
        let material = json!({
            "owner": self.owner(),
            "actor": self.actor().map(ActorId::as_str),
            "automation_run_id": self.automation_run_id().map(RunId::as_str),
            "min_confidence": self.min_confidence().as_f64(),
            "operations": operations,
        });
        let encoded = serde_json::to_string(&material).map_err(|_| {
            FactStoreError::Contract(DomainError::NonCanonical {
                field: "curation request digest material",
            })
        })?;
        Ok(sha256_hex(encoded.as_bytes()))
    }
}

fn validate_changed_fact_capacity(
    operations: &[ProjectMemoryFactCurationOperationV1],
) -> FactStoreResult<()> {
    let changed_fact_capacity = operations.iter().try_fold(0_usize, |total, operation| {
        let operation_capacity = match operation {
            ProjectMemoryFactCurationOperationV1::Merge(operation) => {
                operation.command().loser_targets().len()
                    + usize::from(operation.command().merged_content().is_some())
            }
            ProjectMemoryFactCurationOperationV1::LinkFacts(_) => 2,
            ProjectMemoryFactCurationOperationV1::Add(_)
            | ProjectMemoryFactCurationOperationV1::Update(_)
            | ProjectMemoryFactCurationOperationV1::Remove(_)
            | ProjectMemoryFactCurationOperationV1::NormalizeTags(_) => 1,
        };
        total.checked_add(operation_capacity).ok_or_else(|| {
            FactStoreError::Contract(DomainError::NonCanonical {
                field: "curation changed fact capacity",
            })
        })
    })?;
    validate_limit(changed_fact_capacity, MAX_PROJECT_MEMORY_CURATION_TARGETS)
}

fn validate_child_operation_ids(
    outer_operation_id: &ProvenanceId,
    operations: &[ProjectMemoryFactCurationOperationV1],
) -> FactStoreResult<()> {
    let mut seen = BTreeSet::new();
    for (index, operation) in operations.iter().enumerate() {
        if let (Some(child_operation_id), Some(kind)) =
            (operation.child_operation_id(), operation.mutation_kind())
        {
            let expected = derive_project_memory_fact_curation_child_operation_id(
                outer_operation_id,
                index,
                kind,
            )?;
            if child_operation_id != &expected {
                return Err(FactStoreError::Contract(DomainError::DuplicateId {
                    field: "curation child operation identity",
                }));
            }
        }
        if !seen.insert(operation.operation_identity()?) {
            return Err(FactStoreError::Contract(DomainError::DuplicateId {
                field: "curation operation identity",
            }));
        }
    }
    Ok(())
}

fn curation_fact_identity(target: &ProjectMemoryFactIdV1) -> Value {
    json!({ "fact_id": target.fact_id().as_str() })
}

fn curation_review_identity(target: &ProjectMemoryFactCurationReviewRefV1) -> Value {
    json!({
        "fact_id": target.fact().fact_id().as_str(),
        "expected_last_event_id": target.expected_last_event_id().as_str(),
    })
}

fn curation_evidence_digest(evidence: &super::ProjectMemoryFactCurationEvidenceV1) -> Value {
    json!({
        "facts": evidence
            .facts()
            .iter()
            .map(curation_review_identity)
            .collect::<Vec<_>>(),
        "confidence": evidence.confidence().as_f64(),
        "reason": evidence.reason(),
    })
}

fn curation_operation_digest(
    operation: &ProjectMemoryFactCurationOperationV1,
) -> FactStoreResult<Value> {
    Ok(match operation {
        ProjectMemoryFactCurationOperationV1::Add(operation) => json!({
            "kind": "add",
            "operation_id": operation.command().operation_id().as_str(),
            "input_digest": operation.command().input_digest(),
            "evidence": curation_evidence_digest(operation.evidence()),
        }),
        ProjectMemoryFactCurationOperationV1::Update(operation) => json!({
            "kind": "update",
            "operation_id": operation.command().operation_id().as_str(),
            "target": curation_fact_identity(operation.command().target()),
            "expected_last_event_id": operation.command().expected_last_event_id(),
            "content": operation.command().patch().content(),
            "category": operation.command().patch().category(),
            "source_label": operation.command().patch().source_label(),
            "tags": operation.command().patch().tags(),
            "entities": operation.command().patch().entities(),
            "metadata": operation.command().patch().metadata(),
            "trust": operation.command().patch().trust().map(Confidence::as_f64),
            "evidence": curation_evidence_digest(operation.evidence()),
        }),
        ProjectMemoryFactCurationOperationV1::Merge(operation) => json!({
            "kind": "merge",
            "operation_id": operation.command().operation_id().as_str(),
            "input_digest": operation.command().input_digest()?,
            "evidence": curation_evidence_digest(operation.evidence()),
        }),
        ProjectMemoryFactCurationOperationV1::Remove(operation) => json!({
            "kind": "remove",
            "operation_id": operation.command().operation_id().as_str(),
            "target": curation_fact_identity(operation.command().target()),
            "expected_last_event_id": operation.command().expected_last_event_id(),
            "evidence": curation_evidence_digest(operation.evidence()),
        }),
        ProjectMemoryFactCurationOperationV1::NormalizeTags(operation) => json!({
            "kind": "normalize_tags",
            "fact": curation_review_identity(operation.fact()),
            "tags": operation.tags(),
            "evidence": operation
                .evidence_facts()
                .iter()
                .map(curation_review_identity)
                .collect::<Vec<_>>(),
            "confidence": operation.confidence().as_f64(),
        }),
        ProjectMemoryFactCurationOperationV1::LinkFacts(operation) => json!({
            "kind": "link_facts",
            "relation": operation.relation(),
            "source": curation_review_identity(operation.source()),
            "target": curation_review_identity(operation.target()),
            "evidence": operation.evidence_facts().iter().map(curation_review_identity).collect::<Vec<_>>(),
        }),
    })
}
