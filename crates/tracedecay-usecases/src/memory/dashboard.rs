//! Dashboard-facing operations over canonical project-memory identities.

use serde_json::Value;
use tracedecay_domain::{Confidence, FactId, FactRelationKindV1, FactRelationV1};
use tracedecay_runtime_core::memory::hygiene::detect_secret_like;
use tracedecay_store::{
    FactReadControl, FactWriteControl, ProjectMemoryDashboardFactDetailQueryV1,
    ProjectMemoryDashboardFactDetailV1, ProjectMemoryDashboardMemoryOverviewQueryV1,
    ProjectMemoryDashboardMemoryOverviewV1, ProjectMemoryDashboardOplogEntryV1,
    ProjectMemoryDashboardOplogQueryV1, ProjectMemoryDashboardVectorPointV1,
    ProjectMemoryDashboardVectorPointsQueryV1, ProjectMemoryFactCurationBatchV1,
    ProjectMemoryFactCurationOperationV1, ProjectMemoryFactCurationReceiptV1,
    ProjectMemoryFactFeedbackHistoryQueryV1, ProjectMemoryFactFeedbackHistoryV1,
    ProjectMemoryFactIdV1, ProjectMemoryFactLinkV1, ProjectMemoryFactMergeCommandV1,
    ProjectMemoryFactMergeOutcomeV1, ProjectMemoryFactNormalizeTagsV1,
    ProjectMemoryFactRemoveCommandV1, ProjectMemoryFactRemoveOutcomeV1, ProjectMemoryFactStore,
    ProjectMemoryMemoryStatusV1,
};

use super::MemoryApplication;
use super::context::MemoryOperationContext;
use super::error::{MemoryApplicationError, MemoryMutationError, settle_authority_result};
use super::sanitize::{
    sanitize_curation_provenance, sanitize_curation_text, sanitize_curation_texts,
};

/// Finite, exact-identity curation command accepted by use cases.
#[derive(Clone, Debug, PartialEq)]
pub enum ProjectMemoryCurationOperation {
    NormalizeTags {
        fact_id: FactId,
        tags: Vec<String>,
        evidence_fact_ids: Vec<FactId>,
        confidence: Confidence,
    },
    LinkFacts {
        source_fact_id: FactId,
        target_fact_id: FactId,
        relation: FactRelationKindV1,
        evidence_fact_ids: Vec<FactId>,
        confidence: Confidence,
        source_label: String,
        metadata: Value,
    },
}

impl<A: ProjectMemoryFactStore> MemoryApplication<A> {
    fn fact_identity(
        &self,
        fact_id: FactId,
    ) -> Result<ProjectMemoryFactIdV1, MemoryApplicationError> {
        ProjectMemoryFactIdV1::new(self.owner.clone(), fact_id).map_err(Into::into)
    }

    /// Finite dashboard overview; the dashboard never opens a memory database
    /// or constructs an unbounded store query itself.
    pub async fn dashboard_overview(
        &self,
        fact_limit: usize,
        graph_limit: usize,
        read_control: &FactReadControl,
    ) -> Result<ProjectMemoryDashboardMemoryOverviewV1, MemoryApplicationError> {
        let overview = self
            .authority
            .dashboard_project_memory_overview(
                ProjectMemoryDashboardMemoryOverviewQueryV1::new(
                    self.owner.clone(),
                    fact_limit,
                    graph_limit,
                )?,
                read_control,
            )
            .await?;
        if overview.owner != self.owner
            || overview.facts.len() > fact_limit
            || overview.entities.len() > graph_limit
            || overview.fact_entity_links.len() > graph_limit
            || overview
                .facts
                .iter()
                .any(|fact| fact.fact.owner() != &self.owner)
            || overview
                .entities
                .iter()
                .any(|entity| entity.target.owner() != &self.owner)
            || overview
                .fact_entity_links
                .iter()
                .any(|link| link.fact.owner() != &self.owner || link.entity.owner() != &self.owner)
        {
            return Err(MemoryApplicationError::InvalidAuthorityResult {
                invariant: "dashboard overview owner and bounds",
            });
        }
        Ok(overview)
    }

    pub async fn dashboard_fact_detail(
        &self,
        fact_id: FactId,
        read_control: &FactReadControl,
    ) -> Result<Option<ProjectMemoryDashboardFactDetailV1>, MemoryApplicationError> {
        let target = self.fact_identity(fact_id)?;
        let detail = self
            .authority
            .dashboard_project_memory_fact_detail(
                ProjectMemoryDashboardFactDetailQueryV1::new(target.clone())?,
                read_control,
            )
            .await?;
        if let Some(detail) = &detail
            && (detail.fact.owner() != &self.owner
                || detail.fact.fact_id() != target.fact_id()
                || detail
                    .entities
                    .iter()
                    .any(|entity| entity.target.owner() != &self.owner)
                || detail.history.as_ref().is_some_and(|history| {
                    history.owner() != &self.owner || history.fact_id() != target.fact_id()
                }))
        {
            return Err(MemoryApplicationError::InvalidAuthorityResult {
                invariant: "dashboard detail owner and identity",
            });
        }
        Ok(detail)
    }

    pub async fn dashboard_feedback_history(
        &self,
        fact_id: FactId,
        limit: usize,
        read_control: &FactReadControl,
    ) -> Result<ProjectMemoryFactFeedbackHistoryV1, MemoryApplicationError> {
        self.get_project_memory_feedback_history(
            ProjectMemoryFactFeedbackHistoryQueryV1::new(
                self.fact_identity(fact_id)?,
                None,
                limit,
            )?,
            read_control,
        )
        .await
    }

    pub async fn dashboard_memory_status(
        &self,
        read_control: &FactReadControl,
    ) -> Result<ProjectMemoryMemoryStatusV1, MemoryApplicationError> {
        self.project_memory_status(read_control).await
    }

    /// Capped vector inputs for dashboard-side PCA and similarity.
    pub async fn dashboard_vector_points(
        &self,
        search: Option<String>,
        limit: usize,
        read_control: &FactReadControl,
    ) -> Result<Vec<ProjectMemoryDashboardVectorPointV1>, MemoryApplicationError> {
        let points = self
            .authority
            .dashboard_project_memory_vector_points(
                ProjectMemoryDashboardVectorPointsQueryV1::new(self.owner.clone(), search, limit)?,
                read_control,
            )
            .await?;
        if points.len() > limit
            || points
                .iter()
                .any(|point| point.fact.fact.owner() != &self.owner)
        {
            return Err(MemoryApplicationError::InvalidAuthorityResult {
                invariant: "dashboard vector point owner and bounds",
            });
        }
        Ok(points)
    }

    pub async fn dashboard_oplog(
        &self,
        limit: usize,
        read_control: &FactReadControl,
    ) -> Result<Vec<ProjectMemoryDashboardOplogEntryV1>, MemoryApplicationError> {
        let entries = self
            .authority
            .dashboard_project_memory_oplog(
                ProjectMemoryDashboardOplogQueryV1::new(self.owner.clone(), limit)?,
                read_control,
            )
            .await?;
        if entries.len() > limit
            || entries.iter().any(|entry| {
                entry
                    .fact
                    .as_ref()
                    .is_some_and(|target| target.owner() != &self.owner)
            })
        {
            return Err(MemoryApplicationError::InvalidAuthorityResult {
                invariant: "dashboard oplog owner and bounds",
            });
        }
        Ok(entries)
    }

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
        let receipt = self
            .authority
            .apply_project_memory_fact_curation(request, write_control)
            .await
            .map_err(MemoryApplicationError::from)?;
        settle_authority_result(receipt, |receipt| {
            if receipt.owner() != &self.owner
                || receipt.operation_id() != &operation_id
                || receipt
                    .changed_facts()
                    .iter()
                    .any(|fact| fact.owner() != &self.owner)
            {
                return Err(MemoryApplicationError::InvalidAuthorityResult {
                    invariant: "dashboard curation receipt owner",
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
        write_control: &FactWriteControl,
    ) -> Result<
        ProjectMemoryFactCurationReceiptV1,
        MemoryMutationError<ProjectMemoryFactCurationReceiptV1>,
    > {
        let operations = operations
            .into_iter()
            .map(|operation| self.curation_operation(operation))
            .collect::<Result<Vec<_>, _>>()?;
        self.dashboard_curation(
            ProjectMemoryFactCurationBatchV1::new(
                self.owner.clone(),
                context.operation_id().clone(),
                context.actor().cloned(),
                min_confidence,
                operations,
            )
            .map_err(MemoryApplicationError::from)?,
            write_control,
        )
        .await
    }

    fn curation_operation(
        &self,
        operation: ProjectMemoryCurationOperation,
    ) -> Result<ProjectMemoryFactCurationOperationV1, MemoryApplicationError> {
        let fact_targets = |fact_ids: Vec<FactId>| {
            fact_ids
                .into_iter()
                .map(|fact_id| self.fact_identity(fact_id))
                .collect::<Result<Vec<_>, _>>()
        };
        match operation {
            ProjectMemoryCurationOperation::NormalizeTags {
                fact_id,
                tags,
                evidence_fact_ids,
                confidence,
            } => Ok(ProjectMemoryFactCurationOperationV1::NormalizeTags(
                ProjectMemoryFactNormalizeTagsV1::new(
                    self.fact_identity(fact_id)?,
                    sanitize_curation_texts(tags, "canonical curation tags")?,
                    fact_targets(evidence_fact_ids)?,
                    confidence,
                )?,
            )),
            ProjectMemoryCurationOperation::LinkFacts {
                source_fact_id,
                target_fact_id,
                relation,
                mut evidence_fact_ids,
                confidence,
                source_label,
                metadata,
            } => {
                canonicalize_relation_evidence(&self.owner, &mut evidence_fact_ids)?;
                let provenance = sanitize_curation_provenance(source_label, metadata)?;
                let relation = FactRelationV1::new(
                    self.owner.clone(),
                    source_fact_id,
                    target_fact_id,
                    relation,
                    evidence_fact_ids,
                    confidence,
                    provenance,
                )
                .map_err(|_| MemoryApplicationError::InvalidInput {
                    invariant: "canonical fact relation",
                })?;
                Ok(ProjectMemoryFactCurationOperationV1::LinkFacts(
                    ProjectMemoryFactLinkV1::new(relation)?,
                ))
            }
        }
    }

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
            ProjectMemoryFactRemoveCommandV1::new(
                self.fact_identity(fact_id)?,
                context.operation_id().clone(),
                None,
                context.actor().cloned(),
            )
            .map_err(MemoryApplicationError::from)?,
            write_control,
        )
        .await
    }

    pub async fn merge_canonical_facts(
        &self,
        winner_id: FactId,
        loser_ids: Vec<FactId>,
        merged_content: Option<String>,
        context: MemoryOperationContext,
        write_control: &FactWriteControl,
    ) -> Result<ProjectMemoryFactMergeOutcomeV1, MemoryMutationError<ProjectMemoryFactMergeOutcomeV1>>
    {
        let merged_content = match merged_content {
            Some(content) => {
                if detect_secret_like(content.trim()).is_some() {
                    return Err(MemoryMutationError::Application(
                        MemoryApplicationError::InvalidInput {
                            invariant: "canonical merge content rejected by privacy sanitizer",
                        },
                    ));
                }
                Some(sanitize_curation_text(
                    content,
                    "canonical merge content rejected by privacy sanitizer",
                )?)
            }
            None => None,
        };
        let winner = self.fact_identity(winner_id)?;
        let losers = loser_ids
            .into_iter()
            .map(|fact_id| self.fact_identity(fact_id))
            .collect::<Result<Vec<_>, _>>()?;
        self.dashboard_merge_facts(
            ProjectMemoryFactMergeCommandV1::new(
                self.owner.clone(),
                context.operation_id().clone(),
                winner,
                losers,
                merged_content,
                context.actor().cloned(),
            )
            .map_err(MemoryApplicationError::from)?,
            write_control,
        )
        .await
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
        let losers = request.losers().to_vec();
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
                    invariant: "dashboard merge outcome exact command and receipt identity",
                });
            }
            Ok(())
        })
    }
}

pub(super) fn canonicalize_relation_evidence(
    owner: &tracedecay_domain::FactOwnerV1,
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
