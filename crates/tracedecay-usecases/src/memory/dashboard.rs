//! Dashboard-facing V1 memory operations.

use tracedecay_domain::Confidence;
use tracedecay_store::{
    ProjectMemoryDashboardFactDetailQueryV1, ProjectMemoryDashboardFactDetailV1,
    ProjectMemoryDashboardMemoryOverviewQueryV1, ProjectMemoryDashboardMemoryOverviewV1,
    ProjectMemoryDashboardOplogEntryV1, ProjectMemoryDashboardOplogQueryV1,
    ProjectMemoryDashboardVectorPointV1, ProjectMemoryDashboardVectorPointsQueryV1,
    ProjectMemoryFactAddAliasV1, ProjectMemoryFactCurationBatchV1,
    ProjectMemoryFactCurationOperationV1, ProjectMemoryFactCurationReceiptV1,
    ProjectMemoryFactFeedbackHistoryQueryV1, ProjectMemoryFactFeedbackHistoryV1,
    ProjectMemoryFactLinkV1, ProjectMemoryFactMergeCommandV1, ProjectMemoryFactMergeEntitiesV1,
    ProjectMemoryFactMergeOutcomeV1, ProjectMemoryFactNormalizeTagsV1,
    ProjectMemoryFactRepairVectorV1, ProjectMemoryLegacyEntityTargetV1,
    ProjectMemoryMemoryRepairCommandV1, ProjectMemoryMemoryRepairStatsV1,
    ProjectMemoryMemoryStatusV1, FactCompatibilityStore,
};

use tracedecay_runtime_core::memory::hygiene::detect_secret_like;
use tracedecay_runtime_core::memory::types::{
    MemoryGroomingOperation, MemoryGroomingReport, MemoryRepairStats,
};

use super::MemoryApplication;
use super::compatibility::{compatibility_relation, legacy_usize};
use super::context::MemoryOperationContext;
use super::error::MemoryApplicationError;
use super::sanitize::{
    sanitize_curation_metadata, sanitize_curation_text, sanitize_curation_texts,
};

impl<A: FactCompatibilityStore> MemoryApplication<A> {
    /// Finite dashboard overview; the dashboard never opens a memory database
    /// or constructs a store query itself.
    pub async fn dashboard_overview_v1(
        &self,
        fact_limit: usize,
        graph_limit: usize,
    ) -> Result<ProjectMemoryDashboardMemoryOverviewV1, MemoryApplicationError> {
        let overview = self
            .authority
            .dashboard_compatibility_memory_overview(
                ProjectMemoryDashboardMemoryOverviewQueryV1::new(
                    self.owner.clone(),
                    fact_limit,
                    graph_limit,
                )?,
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

    /// Legacy numeric detail wrapper. The fixed compatibility source and owner
    /// are resolved here, never by a dashboard handler.
    pub async fn dashboard_fact_detail_v1(
        &self,
        fact_id: i64,
    ) -> Result<Option<ProjectMemoryDashboardFactDetailV1>, MemoryApplicationError> {
        let target = self.legacy_compatibility_target(fact_id)?;
        let detail = self
            .authority
            .dashboard_compatibility_fact_detail(ProjectMemoryDashboardFactDetailQueryV1::new(
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
    /// the legacy lossy `fact_trust_history_v1` vector projection.
    pub async fn dashboard_feedback_history_v1(
        &self,
        fact_id: i64,
        limit: usize,
    ) -> Result<ProjectMemoryFactFeedbackHistoryV1, MemoryApplicationError> {
        self.get_compatibility_feedback_history(ProjectMemoryFactFeedbackHistoryQueryV1::new(
            self.legacy_compatibility_target(fact_id)?,
            None,
            limit,
        )?)
        .await
    }

    /// Typed dashboard status including feedback-history repair progress.
    pub async fn dashboard_memory_status_v1(
        &self,
    ) -> Result<ProjectMemoryMemoryStatusV1, MemoryApplicationError> {
        self.compatibility_memory_status().await
    }

    /// Capped vector inputs for dashboard-side PCA and similarity. Pair scoring
    /// remains client-side over this bounded response rather than a generic DB API.
    pub async fn dashboard_vector_points_v1(
        &self,
        search: Option<String>,
        limit: usize,
    ) -> Result<Vec<ProjectMemoryDashboardVectorPointV1>, MemoryApplicationError> {
        let points = self
            .authority
            .dashboard_compatibility_vector_points(ProjectMemoryDashboardVectorPointsQueryV1::new(
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

    pub async fn dashboard_oplog_v1(
        &self,
        limit: usize,
    ) -> Result<Vec<ProjectMemoryDashboardOplogEntryV1>, MemoryApplicationError> {
        let entries = self
            .authority
            .dashboard_compatibility_memory_oplog(ProjectMemoryDashboardOplogQueryV1::new(
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

    pub async fn dashboard_curation_v1(
        &self,
        request: ProjectMemoryFactCurationBatchV1,
    ) -> Result<ProjectMemoryFactCurationReceiptV1, MemoryApplicationError> {
        self.ensure_owner(request.owner())?;
        let receipt = self
            .authority
            .apply_compatibility_fact_curation(request)
            .await?;
        if receipt.owner() != &self.owner {
            return Err(MemoryApplicationError::InvalidAuthorityResult {
                invariant: "dashboard curation receipt owner",
            });
        }
        Ok(receipt)
    }

    /// Dashboard-facing finite curation adapter. Numeric V1 identifiers are
    /// resolved only through the fixed compatibility scope at this boundary.
    pub async fn dashboard_apply_grooming_v1(
        &self,
        operations: Vec<MemoryGroomingOperation>,
        min_confidence: f64,
        context: MemoryOperationContext,
    ) -> Result<MemoryGroomingReport, MemoryApplicationError> {
        let minimum = Confidence::new(min_confidence).map_err(|_| {
            MemoryApplicationError::InvalidCompatibilityInput {
                invariant: "dashboard curation minimum confidence",
            }
        })?;
        let operations = operations
            .into_iter()
            .map(|operation| self.dashboard_curation_operation(operation))
            .collect::<Result<Vec<_>, _>>()?;
        let receipt = self
            .dashboard_curation_v1(ProjectMemoryFactCurationBatchV1::new(
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
                .map(|fact_id| self.legacy_compatibility_target(fact_id))
                .collect::<Result<Vec<_>, _>>()
        };
        let confidence = |value: f64| {
            Confidence::new(value).map_err(|_| MemoryApplicationError::InvalidCompatibilityInput {
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
                    self.legacy_compatibility_target(fact_id)?,
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
                    self.legacy_compatibility_target(source_fact_id)?,
                    self.legacy_compatibility_target(target_fact_id)?,
                    compatibility_relation(relation),
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
                    self.legacy_compatibility_target(fact_id)?,
                    fact_targets(evidence_fact_ids)?,
                    confidence(value)?,
                ),
            )),
        }
    }

    pub async fn dashboard_merge_facts_v1(
        &self,
        request: ProjectMemoryFactMergeCommandV1,
    ) -> Result<ProjectMemoryFactMergeOutcomeV1, MemoryApplicationError> {
        self.ensure_owner(request.owner())?;
        let outcome = self.authority.merge_compatibility_facts(request).await?;
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
    pub async fn dashboard_merge_fact_ids_v1(
        &self,
        winner_id: i64,
        loser_ids: Vec<i64>,
        merged_content: Option<String>,
        context: MemoryOperationContext,
    ) -> Result<ProjectMemoryFactMergeOutcomeV1, MemoryApplicationError> {
        let merged_content = match merged_content {
            Some(content) => {
                if detect_secret_like(content.trim()).is_some() {
                    return Err(MemoryApplicationError::InvalidCompatibilityInput {
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
            .map(|fact_id| self.legacy_compatibility_target(fact_id))
            .collect::<Result<Vec<_>, _>>()?;
        self.dashboard_merge_facts_v1(ProjectMemoryFactMergeCommandV1::new(
            self.owner.clone(),
            context.operation_id().clone(),
            self.legacy_compatibility_target(winner_id)?,
            losers,
            merged_content,
            context.actor().cloned(),
        )?)
        .await
    }

    /// One authority repair step only. Any incomplete feedback-history repair is
    /// surfaced through `memory_status_v1`/feedback history while the daemon resumes it.
    pub async fn dashboard_repair_v1(
        &self,
        context: MemoryOperationContext,
    ) -> Result<ProjectMemoryMemoryRepairStatsV1, MemoryApplicationError> {
        self.authority
            .repair_compatibility_memory(ProjectMemoryMemoryRepairCommandV1::new(
                self.owner.clone(),
                context.operation_id().clone(),
                context.actor().cloned(),
            )?)
            .await
            .map_err(Into::into)
    }
}
