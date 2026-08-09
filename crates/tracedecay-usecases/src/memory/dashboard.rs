//! Dashboard-facing memory operations.

use serde_json::Value;
use tracedecay_domain::{Confidence, FactId};
use tracedecay_store::{
    ProjectMemoryDashboardFactDetailQueryV1, ProjectMemoryDashboardFactDetailV1,
    ProjectMemoryDashboardMemoryOverviewQueryV1, ProjectMemoryDashboardMemoryOverviewV1,
    ProjectMemoryDashboardOplogEntryV1, ProjectMemoryDashboardOplogQueryV1,
    ProjectMemoryDashboardVectorPointV1, ProjectMemoryDashboardVectorPointsQueryV1,
    ProjectMemoryFactAddAliasV1, ProjectMemoryFactCurationBatchV1,
    ProjectMemoryFactCurationOperationV1, ProjectMemoryFactCurationReceiptV1,
    ProjectMemoryFactFeedbackHistoryQueryV1, ProjectMemoryFactFeedbackHistoryV1,
    ProjectMemoryFactIdV1, ProjectMemoryFactLinkV1, ProjectMemoryFactMergeCommandV1,
    ProjectMemoryFactMergeEntitiesV1, ProjectMemoryFactMergeOutcomeV1,
    ProjectMemoryFactNormalizeTagsV1, ProjectMemoryFactRelationV1,
    ProjectMemoryFactRemoveCommandV1, ProjectMemoryFactRemoveOutcomeV1,
    ProjectMemoryFactRepairVectorV1, ProjectMemoryFactStore, ProjectMemoryFactTargetV1,
    ProjectMemoryLegacyEntityTargetV1, ProjectMemoryMemoryRepairCommandV1,
    ProjectMemoryMemoryRepairStatsV1, ProjectMemoryMemoryStatusV1,
};

use tracedecay_runtime_core::memory::hygiene::detect_secret_like;
use tracedecay_runtime_core::memory::types::{
    MemoryGroomingOperation, MemoryGroomingReport, MemoryRepairStats,
};

use super::MemoryApplication;
use super::context::MemoryOperationContext;
use super::error::MemoryApplicationError;
use super::project_memory::{legacy_usize, memory_relation};
use super::sanitize::{
    sanitize_curation_metadata, sanitize_curation_text, sanitize_curation_texts,
};

/// Exact-identity grooming command used by canonical fact callers.
///
/// Numeric identifiers remain only for legacy entity rows. Fact and evidence
/// identities are canonical [`FactId`] values and never pass through the
/// persisted numeric compatibility resolver.
#[derive(Clone, Debug, PartialEq)]
pub enum CanonicalMemoryGroomingOperation {
    NormalizeTags {
        fact_id: FactId,
        tags: Vec<String>,
        evidence_fact_ids: Vec<FactId>,
        confidence: Confidence,
    },
    MergeEntities {
        winner_entity_id: i64,
        loser_entity_ids: Vec<i64>,
        evidence_fact_ids: Vec<FactId>,
        confidence: Confidence,
    },
    AddAlias {
        entity_id: i64,
        alias: String,
        evidence_fact_ids: Vec<FactId>,
        confidence: Confidence,
    },
    LinkFacts {
        source_fact_id: FactId,
        target_fact_id: FactId,
        relation: ProjectMemoryFactRelationV1,
        evidence_fact_ids: Vec<FactId>,
        confidence: Confidence,
        source: String,
        metadata: Value,
    },
}

impl<A: ProjectMemoryFactStore> MemoryApplication<A> {
    fn canonical_fact_target(
        &self,
        fact_id: FactId,
    ) -> Result<ProjectMemoryFactTargetV1, MemoryApplicationError> {
        Ok(ProjectMemoryFactTargetV1::Canonical(
            ProjectMemoryFactIdV1::new(self.owner.clone(), fact_id)?,
        ))
    }

    /// Finite dashboard overview; the dashboard never opens a memory database
    /// or constructs a store query itself.
    pub async fn dashboard_overview(
        &self,
        fact_limit: usize,
        graph_limit: usize,
    ) -> Result<ProjectMemoryDashboardMemoryOverviewV1, MemoryApplicationError> {
        let overview = self
            .authority
            .dashboard_project_memory_overview(ProjectMemoryDashboardMemoryOverviewQueryV1::new(
                self.owner.clone(),
                fact_limit,
                graph_limit,
            )?)
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

    /// Persisted numeric detail wrapper. The fixed fact-id source and owner
    /// are resolved here, never by a dashboard handler.
    pub async fn dashboard_fact_detail(
        &self,
        fact_id: i64,
    ) -> Result<Option<ProjectMemoryDashboardFactDetailV1>, MemoryApplicationError> {
        let target = self.persisted_fact_id_target(fact_id)?;
        let detail = self
            .authority
            .dashboard_project_memory_fact_detail(ProjectMemoryDashboardFactDetailQueryV1::new(
                target.clone(),
            )?)
            .await?;
        if let Some(detail) = &detail
            && (detail.fact.owner() != &self.owner
                || detail
                    .entities
                    .iter()
                    .any(|entity| entity.target.owner() != &self.owner)
                || detail
                    .history
                    .as_ref()
                    .is_some_and(|history| history.owner() != &self.owner))
        {
            return Err(MemoryApplicationError::InvalidAuthorityResult {
                invariant: "dashboard detail owner",
            });
        }
        Ok(detail)
    }

    /// Numeric dashboard trust-history route retaining typed repair progress.
    /// Callers that need an honest incomplete state must use this rather than
    /// the legacy lossy `fact_trust_history` vector projection.
    pub async fn dashboard_feedback_history(
        &self,
        fact_id: i64,
        limit: usize,
    ) -> Result<ProjectMemoryFactFeedbackHistoryV1, MemoryApplicationError> {
        self.get_project_memory_feedback_history(ProjectMemoryFactFeedbackHistoryQueryV1::new(
            self.persisted_fact_id_target(fact_id)?,
            None,
            limit,
        )?)
        .await
    }

    /// Typed dashboard status including feedback-history repair progress.
    pub async fn dashboard_memory_status(
        &self,
    ) -> Result<ProjectMemoryMemoryStatusV1, MemoryApplicationError> {
        self.project_memory_status().await
    }

    /// Capped vector inputs for dashboard-side PCA and similarity. Pair scoring
    /// remains client-side over this bounded response rather than a generic DB API.
    pub async fn dashboard_vector_points(
        &self,
        search: Option<String>,
        limit: usize,
    ) -> Result<Vec<ProjectMemoryDashboardVectorPointV1>, MemoryApplicationError> {
        let points = self
            .authority
            .dashboard_project_memory_vector_points(ProjectMemoryDashboardVectorPointsQueryV1::new(
                self.owner.clone(),
                search,
                limit,
            )?)
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
    ) -> Result<Vec<ProjectMemoryDashboardOplogEntryV1>, MemoryApplicationError> {
        let entries = self
            .authority
            .dashboard_project_memory_oplog(ProjectMemoryDashboardOplogQueryV1::new(
                self.owner.clone(),
                limit,
            )?)
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
    ) -> Result<ProjectMemoryFactCurationReceiptV1, MemoryApplicationError> {
        self.ensure_owner(request.owner())?;
        let receipt = self
            .authority
            .apply_project_memory_fact_curation(request)
            .await?;
        if receipt.owner() != &self.owner {
            return Err(MemoryApplicationError::InvalidAuthorityResult {
                invariant: "dashboard curation receipt owner",
            });
        }
        Ok(receipt)
    }

    /// Applies exact-identity grooming without entering the numeric dashboard
    /// compatibility boundary.
    pub async fn apply_canonical_grooming(
        &self,
        operations: Vec<CanonicalMemoryGroomingOperation>,
        min_confidence: Confidence,
        context: MemoryOperationContext,
    ) -> Result<ProjectMemoryFactCurationReceiptV1, MemoryApplicationError> {
        let operations = operations
            .into_iter()
            .map(|operation| self.canonical_curation_operation(operation))
            .collect::<Result<Vec<_>, _>>()?;
        self.dashboard_curation(ProjectMemoryFactCurationBatchV1::new(
            self.owner.clone(),
            context.operation_id().clone(),
            context.actor().cloned(),
            min_confidence,
            operations,
        )?)
        .await
    }

    fn canonical_curation_operation(
        &self,
        operation: CanonicalMemoryGroomingOperation,
    ) -> Result<ProjectMemoryFactCurationOperationV1, MemoryApplicationError> {
        let fact_targets = |fact_ids: Vec<FactId>| {
            fact_ids
                .into_iter()
                .map(|fact_id| self.canonical_fact_target(fact_id))
                .collect::<Result<Vec<_>, _>>()
        };
        match operation {
            CanonicalMemoryGroomingOperation::NormalizeTags {
                fact_id,
                tags,
                evidence_fact_ids,
                confidence,
            } => Ok(ProjectMemoryFactCurationOperationV1::NormalizeTags(
                ProjectMemoryFactNormalizeTagsV1::new(
                    self.canonical_fact_target(fact_id)?,
                    sanitize_curation_texts(tags, "canonical curation tags")?,
                    fact_targets(evidence_fact_ids)?,
                    confidence,
                )?,
            )),
            CanonicalMemoryGroomingOperation::MergeEntities {
                winner_entity_id,
                loser_entity_ids,
                evidence_fact_ids,
                confidence,
            } => Ok(ProjectMemoryFactCurationOperationV1::MergeEntities(
                ProjectMemoryFactMergeEntitiesV1::new(
                    ProjectMemoryLegacyEntityTargetV1::new(self.owner.clone(), winner_entity_id)?,
                    loser_entity_ids
                        .into_iter()
                        .map(|entity_id| {
                            ProjectMemoryLegacyEntityTargetV1::new(self.owner.clone(), entity_id)
                        })
                        .collect::<Result<Vec<_>, _>>()?,
                    fact_targets(evidence_fact_ids)?,
                    confidence,
                )?,
            )),
            CanonicalMemoryGroomingOperation::AddAlias {
                entity_id,
                alias,
                evidence_fact_ids,
                confidence,
            } => Ok(ProjectMemoryFactCurationOperationV1::AddAlias(
                ProjectMemoryFactAddAliasV1::new(
                    ProjectMemoryLegacyEntityTargetV1::new(self.owner.clone(), entity_id)?,
                    sanitize_curation_text(alias, "canonical curation alias")?,
                    fact_targets(evidence_fact_ids)?,
                    confidence,
                )?,
            )),
            CanonicalMemoryGroomingOperation::LinkFacts {
                source_fact_id,
                target_fact_id,
                relation,
                evidence_fact_ids,
                confidence,
                source,
                metadata,
            } => Ok(ProjectMemoryFactCurationOperationV1::LinkFacts(
                ProjectMemoryFactLinkV1::new(
                    self.canonical_fact_target(source_fact_id)?,
                    self.canonical_fact_target(target_fact_id)?,
                    relation,
                    fact_targets(evidence_fact_ids)?,
                    confidence,
                    sanitize_curation_text(source, "canonical curation relation source")?,
                    sanitize_curation_metadata(metadata)?,
                )?,
            )),
        }
    }

    /// Removes one canonical fact identity without resolving a legacy row id.
    pub async fn remove_canonical_fact(
        &self,
        fact_id: FactId,
        context: MemoryOperationContext,
    ) -> Result<ProjectMemoryFactRemoveOutcomeV1, MemoryApplicationError> {
        self.remove_project_memory_fact(ProjectMemoryFactRemoveCommandV1::new(
            self.canonical_fact_target(fact_id)?,
            context.operation_id().clone(),
            None,
            context.actor().cloned(),
        )?)
        .await
    }

    /// Merges canonical fact identities without a numeric compatibility hop.
    pub async fn merge_canonical_facts(
        &self,
        winner_id: FactId,
        loser_ids: Vec<FactId>,
        merged_content: Option<String>,
        context: MemoryOperationContext,
    ) -> Result<ProjectMemoryFactMergeOutcomeV1, MemoryApplicationError> {
        let merged_content = match merged_content {
            Some(content) => {
                if detect_secret_like(content.trim()).is_some() {
                    return Err(MemoryApplicationError::InvalidInput {
                        invariant: "canonical merge content rejected by privacy sanitizer",
                    });
                }
                Some(sanitize_curation_text(
                    content,
                    "canonical merge content rejected by privacy sanitizer",
                )?)
            }
            None => None,
        };
        let winner = self.canonical_fact_target(winner_id)?;
        let losers = loser_ids
            .into_iter()
            .map(|fact_id| self.canonical_fact_target(fact_id))
            .collect::<Result<Vec<_>, _>>()?;
        self.dashboard_merge_facts(ProjectMemoryFactMergeCommandV1::new(
            self.owner.clone(),
            context.operation_id().clone(),
            winner,
            losers,
            merged_content,
            context.actor().cloned(),
        )?)
        .await
    }

    /// Dashboard-facing finite curation adapter. Persisted numeric identifiers are
    /// resolved only through the fixed persisted fact-id scope at this boundary.
    pub async fn dashboard_apply_grooming(
        &self,
        operations: Vec<MemoryGroomingOperation>,
        min_confidence: f64,
        context: MemoryOperationContext,
    ) -> Result<MemoryGroomingReport, MemoryApplicationError> {
        let minimum =
            Confidence::new(min_confidence).map_err(|_| MemoryApplicationError::InvalidInput {
                invariant: "dashboard curation minimum confidence",
            })?;
        let operations = operations
            .into_iter()
            .map(|operation| self.dashboard_curation_operation(operation))
            .collect::<Result<Vec<_>, _>>()?;
        let receipt = self
            .dashboard_curation(ProjectMemoryFactCurationBatchV1::new(
                self.owner.clone(),
                context.operation_id().clone(),
                context.actor().cloned(),
                minimum,
                operations,
            )?)
            .await?;
        Ok(MemoryGroomingReport {
            normalized_tags: legacy_usize(receipt.normalized_tags(), "dashboard normalized tags")?,
            merged_entities: legacy_usize(receipt.merged_entities(), "dashboard merged entities")?,
            aliases_added: legacy_usize(receipt.aliases_added(), "dashboard aliases added")?,
            facts_linked: legacy_usize(receipt.facts_linked(), "dashboard facts linked")?,
            vectors_repaired: legacy_usize(
                receipt.vectors_repaired(),
                "dashboard vectors repaired",
            )?,
            derived_repair: MemoryRepairStats {
                missing_vectors_repaired: legacy_usize(
                    receipt.derived_repair().missing_vectors_repaired(),
                    "dashboard derived vectors repaired",
                )?,
                banks_rebuilt: legacy_usize(
                    receipt.derived_repair().banks_rebuilt(),
                    "dashboard derived banks rebuilt",
                )?,
            },
        })
    }

    fn dashboard_curation_operation(
        &self,
        operation: MemoryGroomingOperation,
    ) -> Result<ProjectMemoryFactCurationOperationV1, MemoryApplicationError> {
        let fact_targets = |fact_ids: Vec<i64>| {
            fact_ids
                .into_iter()
                .map(|fact_id| self.persisted_fact_id_target(fact_id))
                .collect::<Result<Vec<_>, _>>()
        };
        let confidence = |value: f64| {
            Confidence::new(value).map_err(|_| MemoryApplicationError::InvalidInput {
                invariant: "dashboard curation confidence",
            })
        };
        match operation {
            MemoryGroomingOperation::NormalizeTags {
                fact_id,
                tags,
                evidence_fact_ids,
                confidence: value,
            } => Ok(ProjectMemoryFactCurationOperationV1::NormalizeTags(
                ProjectMemoryFactNormalizeTagsV1::new(
                    self.persisted_fact_id_target(fact_id)?,
                    sanitize_curation_texts(tags, "dashboard curation tags")?,
                    fact_targets(evidence_fact_ids)?,
                    confidence(value)?,
                )?,
            )),
            MemoryGroomingOperation::MergeEntities {
                winner_entity_id,
                loser_entity_ids,
                evidence_fact_ids,
                confidence: value,
            } => Ok(ProjectMemoryFactCurationOperationV1::MergeEntities(
                ProjectMemoryFactMergeEntitiesV1::new(
                    ProjectMemoryLegacyEntityTargetV1::new(self.owner.clone(), winner_entity_id)?,
                    loser_entity_ids
                        .into_iter()
                        .map(|entity_id| {
                            ProjectMemoryLegacyEntityTargetV1::new(self.owner.clone(), entity_id)
                        })
                        .collect::<Result<Vec<_>, _>>()?,
                    fact_targets(evidence_fact_ids)?,
                    confidence(value)?,
                )?,
            )),
            MemoryGroomingOperation::AddAlias {
                entity_id,
                alias,
                evidence_fact_ids,
                confidence: value,
            } => Ok(ProjectMemoryFactCurationOperationV1::AddAlias(
                ProjectMemoryFactAddAliasV1::new(
                    ProjectMemoryLegacyEntityTargetV1::new(self.owner.clone(), entity_id)?,
                    sanitize_curation_text(alias, "dashboard curation alias")?,
                    fact_targets(evidence_fact_ids)?,
                    confidence(value)?,
                )?,
            )),
            MemoryGroomingOperation::LinkFacts {
                source_fact_id,
                target_fact_id,
                relation,
                evidence_fact_ids,
                confidence: value,
                source,
                metadata,
            } => Ok(ProjectMemoryFactCurationOperationV1::LinkFacts(
                ProjectMemoryFactLinkV1::new(
                    self.persisted_fact_id_target(source_fact_id)?,
                    self.persisted_fact_id_target(target_fact_id)?,
                    memory_relation(relation),
                    fact_targets(evidence_fact_ids)?,
                    confidence(value)?,
                    sanitize_curation_text(source, "dashboard curation relation source")?,
                    sanitize_curation_metadata(metadata)?,
                )?,
            )),
            MemoryGroomingOperation::RepairVector {
                fact_id,
                evidence_fact_ids,
                confidence: value,
            } => Ok(ProjectMemoryFactCurationOperationV1::RepairVector(
                ProjectMemoryFactRepairVectorV1::new(
                    self.persisted_fact_id_target(fact_id)?,
                    fact_targets(evidence_fact_ids)?,
                    confidence(value)?,
                ),
            )),
        }
    }

    pub async fn dashboard_merge_facts(
        &self,
        request: ProjectMemoryFactMergeCommandV1,
    ) -> Result<ProjectMemoryFactMergeOutcomeV1, MemoryApplicationError> {
        self.ensure_owner(request.owner())?;
        let outcome = self.authority.merge_project_memory_facts(request).await?;
        if outcome.owner() != &self.owner {
            return Err(MemoryApplicationError::InvalidAuthorityResult {
                invariant: "dashboard merge outcome owner",
            });
        }
        Ok(outcome)
    }

    /// Legacy numeric merge route for the dashboard. The handler supplies only
    /// IDs and a trusted operation context; fixed source/owner resolution and
    /// content privacy gating stay in the application layer.
    pub async fn dashboard_merge_fact_ids(
        &self,
        winner_id: i64,
        loser_ids: Vec<i64>,
        merged_content: Option<String>,
        context: MemoryOperationContext,
    ) -> Result<ProjectMemoryFactMergeOutcomeV1, MemoryApplicationError> {
        let merged_content = match merged_content {
            Some(content) => {
                if detect_secret_like(content.trim()).is_some() {
                    return Err(MemoryApplicationError::InvalidInput {
                        invariant: "dashboard merge content rejected by privacy sanitizer",
                    });
                }
                Some(sanitize_curation_text(
                    content,
                    "dashboard merge content rejected by privacy sanitizer",
                )?)
            }
            None => None,
        };
        let losers = loser_ids
            .into_iter()
            .map(|fact_id| self.persisted_fact_id_target(fact_id))
            .collect::<Result<Vec<_>, _>>()?;
        self.dashboard_merge_facts(ProjectMemoryFactMergeCommandV1::new(
            self.owner.clone(),
            context.operation_id().clone(),
            self.persisted_fact_id_target(winner_id)?,
            losers,
            merged_content,
            context.actor().cloned(),
        )?)
        .await
    }

    /// One authority repair step only. Any incomplete feedback-history repair is
    /// surfaced through `memory_status`/feedback history while the daemon resumes it.
    pub async fn dashboard_repair(
        &self,
        context: MemoryOperationContext,
    ) -> Result<ProjectMemoryMemoryRepairStatsV1, MemoryApplicationError> {
        self.authority
            .repair_project_memory(ProjectMemoryMemoryRepairCommandV1::new(
                self.owner.clone(),
                context.operation_id().clone(),
                context.actor().cloned(),
            )?)
            .await
            .map_err(Into::into)
    }
}
