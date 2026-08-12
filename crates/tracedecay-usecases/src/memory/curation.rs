//! Owner-bound construction and settlement for canonical memory curation.

use serde_json::Value;
use tracedecay_domain::{
    ActorId, Confidence, FactEventId, FactId, FactIdentityMaterialV1, FactIdentitySourceV1,
    FactOwnerV1, FactRelationKindV1, FactRelationV1, ProvenanceId, RunId,
};
use tracedecay_runtime_core::memory::hygiene::detect_secret_like;
use tracedecay_store::{
    FactWriteControl, ProjectMemoryFactAddCommandV1, ProjectMemoryFactAddMaterialV1,
    ProjectMemoryFactCurationAddV1, ProjectMemoryFactCurationBatchV1,
    ProjectMemoryFactCurationEvidenceV1, ProjectMemoryFactCurationMergeV1,
    ProjectMemoryFactCurationMutationKindV1, ProjectMemoryFactCurationOperationEffectV1,
    ProjectMemoryFactCurationOperationV1, ProjectMemoryFactCurationReceiptV1,
    ProjectMemoryFactCurationRemoveV1, ProjectMemoryFactCurationReviewRefV1,
    ProjectMemoryFactCurationUpdateV1, ProjectMemoryFactIdV1, ProjectMemoryFactLinkV1,
    ProjectMemoryFactMergeCommandV1, ProjectMemoryFactMergeOutcomeV1,
    ProjectMemoryFactMergeTargetV1, ProjectMemoryFactNormalizeTagsV1,
    ProjectMemoryFactRemoveCommandV1, ProjectMemoryFactRemoveOutcomeV1, ProjectMemoryFactStore,
    ProjectMemoryFactUpdateCommandV1, ProjectMemoryFactUpdateOutcomeV1,
    ProjectMemoryFactUpdatePatchV1, derive_project_memory_fact_curation_child_operation_id,
};

use super::MemoryApplication;
use super::context::MemoryOperationContext;
use super::error::{MemoryApplicationError, MemoryMutationError, settle_authority_result};
use super::project_memory::{ProjectMemoryFactAddPreflight, ProjectMemoryFactAddRequest};
use super::sanitize::{
    sanitize_curation_provenance, sanitize_curation_text, sanitize_curation_texts,
};

/// Finite, exact-identity operation accepted by automatic curation.
#[derive(Clone, Debug, PartialEq)]
pub enum ProjectMemoryCurationOperation {
    Add {
        request: ProjectMemoryFactAddRequest,
        evidence_facts: Vec<ProjectMemoryCurationMutationTarget>,
        confidence: Confidence,
        reason: String,
    },
    Update {
        target: ProjectMemoryCurationMutationTarget,
        patch: ProjectMemoryFactUpdatePatchV1,
        evidence_facts: Vec<ProjectMemoryCurationMutationTarget>,
        confidence: Confidence,
        reason: String,
    },
    Merge {
        winner: ProjectMemoryCurationMutationTarget,
        losers: Vec<ProjectMemoryCurationMutationTarget>,
        merged_content: Option<String>,
        evidence_facts: Vec<ProjectMemoryCurationMutationTarget>,
        confidence: Confidence,
        reason: String,
    },
    Remove {
        target: ProjectMemoryCurationMutationTarget,
        evidence_facts: Vec<ProjectMemoryCurationMutationTarget>,
        confidence: Confidence,
        reason: String,
    },
    NormalizeTags {
        target: ProjectMemoryCurationMutationTarget,
        tags: Vec<String>,
        evidence_facts: Vec<ProjectMemoryCurationMutationTarget>,
        confidence: Confidence,
    },
    LinkFacts {
        source: ProjectMemoryCurationMutationTarget,
        target: ProjectMemoryCurationMutationTarget,
        relation: FactRelationKindV1,
        evidence_facts: Vec<ProjectMemoryCurationMutationTarget>,
        confidence: Confidence,
        source_label: String,
        metadata: Value,
    },
}

/// Exact fact snapshot required by an automatic destructive curation review.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProjectMemoryCurationMutationTarget {
    fact_id: FactId,
    expected_last_event_id: FactEventId,
}

impl ProjectMemoryCurationMutationTarget {
    pub fn new(fact_id: FactId, expected_last_event_id: FactEventId) -> Self {
        Self {
            fact_id,
            expected_last_event_id,
        }
    }

    pub fn fact_id(&self) -> &FactId {
        &self.fact_id
    }

    pub fn expected_last_event_id(&self) -> &FactEventId {
        &self.expected_last_event_id
    }
}

impl From<ProjectMemoryCurationMutationTarget> for ProjectMemoryFactMutationTarget {
    fn from(target: ProjectMemoryCurationMutationTarget) -> Self {
        Self::exact(target.fact_id, target.expected_last_event_id)
    }
}

/// One canonical fact admitted for an administrative mutation.
///
/// The use-case layer binds the fact to its owner. Update and removal may be
/// unconditional; merge construction requires an exact event snapshot.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProjectMemoryFactMutationTarget {
    fact_id: FactId,
    expected_last_event_id: Option<FactEventId>,
}

impl ProjectMemoryFactMutationTarget {
    pub fn new(fact_id: FactId, expected_last_event_id: Option<FactEventId>) -> Self {
        Self {
            fact_id,
            expected_last_event_id,
        }
    }

    pub fn exact(fact_id: FactId, expected_last_event_id: FactEventId) -> Self {
        Self::new(fact_id, Some(expected_last_event_id))
    }

    pub fn fact_id(&self) -> &FactId {
        &self.fact_id
    }

    pub fn expected_last_event_id(&self) -> Option<&FactEventId> {
        self.expected_last_event_id.as_ref()
    }
}

impl<A: ProjectMemoryFactStore> MemoryApplication<A> {
    /// Settles one already-canonical curation batch against its exact receipt.
    pub async fn dashboard_curation(
        &self,
        request: ProjectMemoryFactCurationBatchV1,
        write_control: &FactWriteControl,
    ) -> Result<
        ProjectMemoryFactCurationReceiptV1,
        MemoryMutationError<ProjectMemoryFactCurationReceiptV1>,
    > {
        self.ensure_owner(request.owner())?;
        let operation_id = request.operation_id().clone();
        let automation_run_id = request.automation_run_id().cloned();
        let input_digest = request
            .input_digest()
            .map_err(MemoryApplicationError::from)?;
        let operations = request.operations().to_vec();
        let receipt = self
            .authority
            .apply_project_memory_fact_curation(request, write_control)
            .await
            .map_err(MemoryApplicationError::from)?;
        settle_authority_result(receipt, |receipt| {
            if receipt.owner() != &self.owner
                || receipt.operation_id() != &operation_id
                || receipt.automation_run_id() != automation_run_id.as_ref()
                || receipt.input_digest() != input_digest
                || receipt.operation_effects().len() != operations.len()
                || usize::try_from(receipt.accepted_operations()).ok() != Some(operations.len())
                || operations
                    .iter()
                    .zip(receipt.operation_effects())
                    .any(|(operation, effect)| match (operation, effect) {
                        (
                            ProjectMemoryFactCurationOperationV1::Add(operation),
                            ProjectMemoryFactCurationOperationEffectV1::Add { fact, .. },
                        ) => {
                            operation.command().owner() != fact.owner()
                                || effect.primary_commit().is_some()
                                    && committed_add_fact_id(operation.command())
                                        .as_ref()
                                        .map_or(true, |expected| expected != fact.fact_id())
                        }
                        (
                            ProjectMemoryFactCurationOperationV1::Update(operation),
                            ProjectMemoryFactCurationOperationEffectV1::Update { fact, .. },
                        ) => operation.command().target() != fact,
                        (
                            ProjectMemoryFactCurationOperationV1::Merge(operation),
                            ProjectMemoryFactCurationOperationEffectV1::Merge { outcome },
                        ) => !merge_outcome_matches_command(operation.command(), outcome),
                        (
                            ProjectMemoryFactCurationOperationV1::Remove(operation),
                            ProjectMemoryFactCurationOperationEffectV1::Remove { target, .. },
                        ) => operation.command().target() != target,
                        (
                            ProjectMemoryFactCurationOperationV1::NormalizeTags(operation),
                            ProjectMemoryFactCurationOperationEffectV1::NormalizeTags {
                                fact, ..
                            },
                        ) => operation.fact().fact() != fact,
                        (
                            ProjectMemoryFactCurationOperationV1::LinkFacts(operation),
                            ProjectMemoryFactCurationOperationEffectV1::LinkFacts {
                                relation, ..
                            },
                        ) => !relation.matches_relation(operation.relation()),
                        _ => true,
                    })
                || receipt
                    .changed_facts()
                    .iter()
                    .any(|fact| fact.owner() != &self.owner)
            {
                return Err(MemoryApplicationError::InvalidAuthorityResult {
                    invariant: "canonical curation receipt exact request identity",
                });
            }
            Ok(())
        })
    }

    pub async fn apply_project_memory_curation(
        &self,
        operations: Vec<ProjectMemoryCurationOperation>,
        min_confidence: Confidence,
        context: MemoryOperationContext,
        automation_run_id: Option<RunId>,
        write_control: &FactWriteControl,
    ) -> Result<
        ProjectMemoryFactCurationReceiptV1,
        MemoryMutationError<ProjectMemoryFactCurationReceiptV1>,
    > {
        let operations = operations
            .into_iter()
            .enumerate()
            .map(|(index, operation)| {
                self.curation_operation(
                    operation,
                    context.operation_id(),
                    index,
                    context.actor(),
                    automation_run_id.as_ref(),
                )
            })
            .collect::<Result<Vec<_>, _>>()?;
        let mut batch = ProjectMemoryFactCurationBatchV1::new(
            self.owner.clone(),
            context.operation_id().clone(),
            context.actor().cloned(),
            min_confidence,
            operations,
        )
        .map_err(MemoryApplicationError::from)?;
        if let Some(run_id) = automation_run_id {
            batch = batch
                .with_automation_run_id(run_id)
                .map_err(MemoryApplicationError::from)?;
        }
        self.dashboard_curation(batch, write_control).await
    }

    fn curation_operation(
        &self,
        operation: ProjectMemoryCurationOperation,
        outer_operation_id: &ProvenanceId,
        operation_index: usize,
        actor: Option<&ActorId>,
        automation_run_id: Option<&RunId>,
    ) -> Result<ProjectMemoryFactCurationOperationV1, MemoryApplicationError> {
        let review_refs = |facts: Vec<ProjectMemoryCurationMutationTarget>| {
            facts
                .into_iter()
                .map(|fact| {
                    Ok::<_, MemoryApplicationError>(ProjectMemoryFactCurationReviewRefV1::new(
                        fact_identity(&self.owner, fact.fact_id)?,
                        fact.expected_last_event_id,
                    ))
                })
                .collect::<Result<Vec<_>, _>>()
        };
        match operation {
            ProjectMemoryCurationOperation::Add {
                request,
                evidence_facts,
                confidence,
                reason,
            } => {
                let child_operation_id = curation_child_operation_id(
                    outer_operation_id,
                    operation_index,
                    ProjectMemoryFactCurationMutationKindV1::Add,
                )?;
                let command = self.curation_add_command(
                    request,
                    child_operation_id,
                    actor.cloned(),
                    automation_run_id,
                )?;
                Ok(ProjectMemoryFactCurationOperationV1::Add(
                    ProjectMemoryFactCurationAddV1::new(
                        command,
                        curation_evidence(&self.owner, evidence_facts, confidence, reason)?,
                    )?,
                ))
            }
            ProjectMemoryCurationOperation::Update {
                target,
                patch,
                evidence_facts,
                confidence,
                reason,
            } => {
                let child_operation_id = curation_child_operation_id(
                    outer_operation_id,
                    operation_index,
                    ProjectMemoryFactCurationMutationKindV1::Update,
                )?;
                Ok(ProjectMemoryFactCurationOperationV1::Update(
                    ProjectMemoryFactCurationUpdateV1::new(
                        update_command(
                            &self.owner,
                            target.into(),
                            patch,
                            child_operation_id,
                            actor.cloned(),
                        )?,
                        curation_evidence(&self.owner, evidence_facts, confidence, reason)?,
                    )?,
                ))
            }
            ProjectMemoryCurationOperation::Merge {
                winner,
                losers,
                merged_content,
                evidence_facts,
                confidence,
                reason,
            } => {
                let child_operation_id = curation_child_operation_id(
                    outer_operation_id,
                    operation_index,
                    ProjectMemoryFactCurationMutationKindV1::Merge,
                )?;
                Ok(ProjectMemoryFactCurationOperationV1::Merge(
                    ProjectMemoryFactCurationMergeV1::new(
                        merge_command(
                            &self.owner,
                            winner.into(),
                            losers.into_iter().map(Into::into).collect(),
                            merged_content,
                            child_operation_id,
                            actor.cloned(),
                        )?,
                        curation_evidence(&self.owner, evidence_facts, confidence, reason)?,
                    )?,
                ))
            }
            ProjectMemoryCurationOperation::Remove {
                target,
                evidence_facts,
                confidence,
                reason,
            } => {
                let child_operation_id = curation_child_operation_id(
                    outer_operation_id,
                    operation_index,
                    ProjectMemoryFactCurationMutationKindV1::Remove,
                )?;
                Ok(ProjectMemoryFactCurationOperationV1::Remove(
                    ProjectMemoryFactCurationRemoveV1::new(
                        remove_command(
                            &self.owner,
                            target.into(),
                            child_operation_id,
                            actor.cloned(),
                        )?,
                        curation_evidence(&self.owner, evidence_facts, confidence, reason)?,
                    )?,
                ))
            }
            ProjectMemoryCurationOperation::NormalizeTags {
                target,
                tags,
                evidence_facts,
                confidence,
            } => Ok(ProjectMemoryFactCurationOperationV1::NormalizeTags(
                ProjectMemoryFactNormalizeTagsV1::new(
                    ProjectMemoryFactCurationReviewRefV1::new(
                        fact_identity(&self.owner, target.fact_id)?,
                        target.expected_last_event_id,
                    ),
                    sanitize_curation_texts(tags, "canonical curation tags")?,
                    review_refs(evidence_facts)?,
                    confidence,
                )?,
            )),
            ProjectMemoryCurationOperation::LinkFacts {
                source,
                target,
                relation,
                mut evidence_facts,
                confidence,
                source_label,
                metadata,
            } => {
                canonicalize_review_evidence(&self.owner, &mut evidence_facts)?;
                let provenance = sanitize_curation_provenance(source_label, metadata)?;
                let relation = FactRelationV1::new(
                    self.owner.clone(),
                    source.fact_id.clone(),
                    target.fact_id.clone(),
                    relation,
                    evidence_facts
                        .iter()
                        .map(|fact| fact.fact_id.clone())
                        .collect(),
                    confidence,
                    provenance,
                )
                .map_err(|_| MemoryApplicationError::InvalidInput {
                    invariant: "canonical fact relation",
                })?;
                Ok(ProjectMemoryFactCurationOperationV1::LinkFacts(
                    ProjectMemoryFactLinkV1::new(
                        relation,
                        ProjectMemoryFactCurationReviewRefV1::new(
                            fact_identity(&self.owner, source.fact_id)?,
                            source.expected_last_event_id,
                        ),
                        ProjectMemoryFactCurationReviewRefV1::new(
                            fact_identity(&self.owner, target.fact_id)?,
                            target.expected_last_event_id,
                        ),
                        review_refs(evidence_facts)?,
                    )?,
                ))
            }
        }
    }

    fn curation_add_command(
        &self,
        request: ProjectMemoryFactAddRequest,
        operation_id: ProvenanceId,
        actor: Option<ActorId>,
        automation_run_id: Option<&RunId>,
    ) -> Result<ProjectMemoryFactAddCommandV1, MemoryApplicationError> {
        let preflight = self.preflight_project_memory_fact_add(request, actor.clone())?;
        let ProjectMemoryFactAddPreflight::Ready { command, .. } = preflight else {
            return Err(MemoryApplicationError::InvalidInput {
                invariant: "curation add declined by memory privacy sanitizer",
            });
        };
        ProjectMemoryFactAddMaterialV1::new(
            command.owner().clone(),
            command.content().to_owned(),
            command.category(),
            command.source_label().map(ToOwned::to_owned),
            command.tags().to_vec(),
            command.entities().to_vec(),
            command.metadata().clone(),
            command.sanitization_receipt().clone(),
            automation_run_id.map(|run_id| run_id.as_str().to_owned()),
            command.default_trust(),
            actor,
        )
        .map_err(MemoryApplicationError::from)?
        .into_command(operation_id)
        .map_err(MemoryApplicationError::from)
    }

    /// Constructs an owner-bound update command from the canonical store patch.
    pub fn canonical_fact_update_command(
        &self,
        target: ProjectMemoryFactMutationTarget,
        patch: ProjectMemoryFactUpdatePatchV1,
        context: &MemoryOperationContext,
    ) -> Result<ProjectMemoryFactUpdateCommandV1, MemoryApplicationError> {
        update_command(
            &self.owner,
            target,
            patch,
            context.operation_id().clone(),
            context.actor().cloned(),
        )
    }

    pub async fn update_canonical_fact(
        &self,
        target: ProjectMemoryFactMutationTarget,
        patch: ProjectMemoryFactUpdatePatchV1,
        context: MemoryOperationContext,
        write_control: &FactWriteControl,
    ) -> Result<
        ProjectMemoryFactUpdateOutcomeV1,
        MemoryMutationError<ProjectMemoryFactUpdateOutcomeV1>,
    > {
        let command = self.canonical_fact_update_command(target, patch, &context)?;
        self.update_project_memory_fact(command, write_control)
            .await
    }

    /// Constructs an owner-bound compare-and-set remove command.
    pub fn canonical_fact_remove_command(
        &self,
        target: ProjectMemoryFactMutationTarget,
        context: &MemoryOperationContext,
    ) -> Result<ProjectMemoryFactRemoveCommandV1, MemoryApplicationError> {
        remove_command(
            &self.owner,
            target,
            context.operation_id().clone(),
            context.actor().cloned(),
        )
    }

    /// Removes an owner-bound fact when the caller has no read snapshot.
    pub async fn remove_canonical_fact(
        &self,
        fact_id: FactId,
        context: MemoryOperationContext,
        write_control: &FactWriteControl,
    ) -> Result<
        ProjectMemoryFactRemoveOutcomeV1,
        MemoryMutationError<ProjectMemoryFactRemoveOutcomeV1>,
    > {
        self.remove_project_memory_fact(
            self.canonical_fact_remove_command(
                ProjectMemoryFactMutationTarget::new(fact_id, None),
                &context,
            )?,
            write_control,
        )
        .await
    }

    pub async fn remove_canonical_fact_at(
        &self,
        target: ProjectMemoryFactMutationTarget,
        context: MemoryOperationContext,
        write_control: &FactWriteControl,
    ) -> Result<
        ProjectMemoryFactRemoveOutcomeV1,
        MemoryMutationError<ProjectMemoryFactRemoveOutcomeV1>,
    > {
        let command = self.canonical_fact_remove_command(target, &context)?;
        self.remove_project_memory_fact(command, write_control)
            .await
    }

    /// Constructs an owner-bound, compare-and-set merge command.
    pub fn canonical_fact_merge_command(
        &self,
        winner: ProjectMemoryFactMutationTarget,
        losers: Vec<ProjectMemoryFactMutationTarget>,
        merged_content: Option<String>,
        context: &MemoryOperationContext,
    ) -> Result<ProjectMemoryFactMergeCommandV1, MemoryApplicationError> {
        merge_command(
            &self.owner,
            winner,
            losers,
            merged_content,
            context.operation_id().clone(),
            context.actor().cloned(),
        )
    }

    pub async fn merge_canonical_facts(
        &self,
        winner: ProjectMemoryFactMutationTarget,
        losers: Vec<ProjectMemoryFactMutationTarget>,
        merged_content: Option<String>,
        context: MemoryOperationContext,
        write_control: &FactWriteControl,
    ) -> Result<ProjectMemoryFactMergeOutcomeV1, MemoryMutationError<ProjectMemoryFactMergeOutcomeV1>>
    {
        let command =
            self.canonical_fact_merge_command(winner, losers, merged_content, &context)?;
        self.dashboard_merge_facts(command, write_control).await
    }

    pub async fn dashboard_merge_facts(
        &self,
        request: ProjectMemoryFactMergeCommandV1,
        write_control: &FactWriteControl,
    ) -> Result<ProjectMemoryFactMergeOutcomeV1, MemoryMutationError<ProjectMemoryFactMergeOutcomeV1>>
    {
        self.ensure_owner(request.owner())?;
        let operation_id = request.operation_id().clone();
        let input_digest = request
            .input_digest()
            .map_err(MemoryApplicationError::from)?;
        let winner = request.winner().clone();
        let losers = request.loser_facts().cloned().collect::<Vec<_>>();
        let content_updated = request.merged_content().is_some();
        let outcome = self
            .authority
            .merge_project_memory_facts(request, write_control)
            .await
            .map_err(MemoryApplicationError::from)?;
        settle_authority_result(outcome, |outcome| {
            if outcome.owner() != &self.owner
                || outcome.operation_id() != &operation_id
                || outcome.input_digest() != input_digest.as_str()
                || outcome.winner() != &winner
                || outcome.deleted_losers() != losers.as_slice()
                || outcome.content_updated() != content_updated
            {
                return Err(MemoryApplicationError::InvalidAuthorityResult {
                    invariant: "canonical merge outcome exact command and receipt identity",
                });
            }
            Ok(())
        })
    }
}

fn committed_add_fact_id(
    command: &ProjectMemoryFactAddCommandV1,
) -> Result<FactId, MemoryApplicationError> {
    FactId::derive(&FactIdentityMaterialV1::new(
        command.owner().clone(),
        FactIdentitySourceV1::Application {
            operation_id: command.operation_id().clone(),
        },
    )?)
    .map_err(Into::into)
}

fn curation_child_operation_id(
    outer_operation_id: &ProvenanceId,
    operation_index: usize,
    kind: ProjectMemoryFactCurationMutationKindV1,
) -> Result<ProvenanceId, MemoryApplicationError> {
    derive_project_memory_fact_curation_child_operation_id(
        outer_operation_id,
        operation_index,
        kind,
    )
    .map_err(MemoryApplicationError::from)
}

fn update_command(
    owner: &FactOwnerV1,
    target: ProjectMemoryFactMutationTarget,
    patch: ProjectMemoryFactUpdatePatchV1,
    operation_id: ProvenanceId,
    actor: Option<ActorId>,
) -> Result<ProjectMemoryFactUpdateCommandV1, MemoryApplicationError> {
    ProjectMemoryFactUpdateCommandV1::new(
        fact_identity(owner, target.fact_id)?,
        operation_id,
        target.expected_last_event_id,
        patch,
        actor,
    )
    .map_err(MemoryApplicationError::from)
}

fn remove_command(
    owner: &FactOwnerV1,
    target: ProjectMemoryFactMutationTarget,
    operation_id: ProvenanceId,
    actor: Option<ActorId>,
) -> Result<ProjectMemoryFactRemoveCommandV1, MemoryApplicationError> {
    ProjectMemoryFactRemoveCommandV1::new(
        fact_identity(owner, target.fact_id)?,
        operation_id,
        target.expected_last_event_id,
        actor,
    )
    .map_err(MemoryApplicationError::from)
}

fn merge_command(
    owner: &FactOwnerV1,
    winner: ProjectMemoryFactMutationTarget,
    losers: Vec<ProjectMemoryFactMutationTarget>,
    merged_content: Option<String>,
    operation_id: ProvenanceId,
    actor: Option<ActorId>,
) -> Result<ProjectMemoryFactMergeCommandV1, MemoryApplicationError> {
    let merged_content = sanitize_merge_content(merged_content)?;
    let winner = merge_target(owner, winner)?;
    let losers = losers
        .into_iter()
        .map(|target| merge_target(owner, target))
        .collect::<Result<Vec<_>, _>>()?;
    ProjectMemoryFactMergeCommandV1::new(
        owner.clone(),
        operation_id,
        winner,
        losers,
        merged_content,
        actor,
    )
    .map_err(MemoryApplicationError::from)
}

fn fact_identity(
    owner: &FactOwnerV1,
    fact_id: FactId,
) -> Result<ProjectMemoryFactIdV1, MemoryApplicationError> {
    ProjectMemoryFactIdV1::new(owner.clone(), fact_id).map_err(Into::into)
}

fn merge_target(
    owner: &FactOwnerV1,
    target: ProjectMemoryFactMutationTarget,
) -> Result<ProjectMemoryFactMergeTargetV1, MemoryApplicationError> {
    let expected_last_event_id =
        target
            .expected_last_event_id
            .ok_or(MemoryApplicationError::InvalidInput {
                invariant: "canonical merge target requires exact event identity",
            })?;
    ProjectMemoryFactMergeTargetV1::new(
        fact_identity(owner, target.fact_id)?,
        expected_last_event_id,
    )
    .map_err(MemoryApplicationError::from)
}

fn sanitize_merge_content(
    merged_content: Option<String>,
) -> Result<Option<String>, MemoryApplicationError> {
    match merged_content {
        Some(content) => {
            if detect_secret_like(content.trim()).is_some() {
                return Err(MemoryApplicationError::InvalidInput {
                    invariant: "canonical merge content rejected by privacy sanitizer",
                });
            }
            sanitize_curation_text(
                content,
                "canonical merge content rejected by privacy sanitizer",
            )
            .map(Some)
        }
        None => Ok(None),
    }
}

fn curation_evidence(
    owner: &FactOwnerV1,
    mut evidence_facts: Vec<ProjectMemoryCurationMutationTarget>,
    confidence: Confidence,
    reason: String,
) -> Result<ProjectMemoryFactCurationEvidenceV1, MemoryApplicationError> {
    canonicalize_review_evidence(owner, &mut evidence_facts)?;
    let facts = evidence_facts
        .into_iter()
        .map(|fact| {
            Ok::<_, MemoryApplicationError>(ProjectMemoryFactCurationReviewRefV1::new(
                fact_identity(owner, fact.fact_id)?,
                fact.expected_last_event_id,
            ))
        })
        .collect::<Result<Vec<_>, _>>()?;
    ProjectMemoryFactCurationEvidenceV1::new(
        owner,
        facts,
        confidence,
        sanitize_curation_text(reason, "canonical curation reason")?,
    )
    .map_err(MemoryApplicationError::from)
}

fn merge_outcome_matches_command(
    command: &ProjectMemoryFactMergeCommandV1,
    outcome: &ProjectMemoryFactMergeOutcomeV1,
) -> bool {
    let Ok(input_digest) = command.input_digest() else {
        return false;
    };
    outcome.owner() == command.owner()
        && outcome.operation_id() == command.operation_id()
        && outcome.input_digest() == input_digest
        && outcome.winner() == command.winner()
        && outcome.deleted_losers().iter().eq(command.loser_facts())
        && outcome.content_updated() == command.merged_content().is_some()
}

pub(super) fn canonicalize_relation_evidence(
    owner: &FactOwnerV1,
    evidence_fact_ids: &mut [FactId],
) -> Result<(), MemoryApplicationError> {
    if evidence_fact_ids
        .iter()
        .any(|fact_id| fact_id.validate_owner(owner).is_err())
    {
        return Err(MemoryApplicationError::InvalidInput {
            invariant: "canonical relation evidence owner",
        });
    }
    evidence_fact_ids.sort_unstable();
    if evidence_fact_ids.windows(2).any(|pair| pair[0] == pair[1]) {
        return Err(MemoryApplicationError::InvalidInput {
            invariant: "canonical relation evidence must be unique",
        });
    }
    Ok(())
}

fn canonicalize_review_evidence(
    owner: &FactOwnerV1,
    evidence: &mut [ProjectMemoryCurationMutationTarget],
) -> Result<(), MemoryApplicationError> {
    if evidence
        .iter()
        .any(|fact| fact.fact_id.validate_owner(owner).is_err())
    {
        return Err(MemoryApplicationError::InvalidInput {
            invariant: "canonical reviewed evidence owner",
        });
    }
    evidence.sort_unstable_by(|left, right| left.fact_id.cmp(&right.fact_id));
    if evidence
        .windows(2)
        .any(|pair| pair[0].fact_id == pair[1].fact_id)
    {
        return Err(MemoryApplicationError::InvalidInput {
            invariant: "canonical reviewed evidence must be unique",
        });
    }
    Ok(())
}
