use std::future::Future;
use tracedecay_domain::{
    ActorId, FactId, FactLineageEventV1, FactOwnerV1, ProvenanceId, RetrievalAnchorRecordV2,
};

use super::{
    CurrentFactsQuery, FactAsOfQuery, FactAsOfResponseV1, FactCommitOutcome, FactCurrentQuery,
    FactCurrentResponseV1, FactLineageQuery, FactLineageResponseV1, FactProposalStoreError,
    FactStoreResult, FactWriteBatch, LegacyFactQuery, ProjectMemoryDashboardFactDetailQueryV1,
    ProjectMemoryDashboardFactDetailV1, ProjectMemoryDashboardMemoryOverviewQueryV1,
    ProjectMemoryDashboardMemoryOverviewV1, ProjectMemoryDashboardOplogEntryV1,
    ProjectMemoryDashboardOplogQueryV1, ProjectMemoryDashboardVectorPointV1,
    ProjectMemoryDashboardVectorPointsQueryV1, ProjectMemoryFactAddCommandV1,
    ProjectMemoryFactAddOutcomeV1, ProjectMemoryFactContentDigestQueryV1,
    ProjectMemoryFactContradictionPageV1, ProjectMemoryFactContradictionQueryV1,
    ProjectMemoryFactCurationBatchV1, ProjectMemoryFactCurationReceiptV1,
    ProjectMemoryFactFeedbackCommandV1, ProjectMemoryFactFeedbackHistoryQueryV1,
    ProjectMemoryFactFeedbackHistoryV1, ProjectMemoryFactFeedbackOutcomeV1,
    ProjectMemoryFactHistoryQueryV1, ProjectMemoryFactHistoryV1, ProjectMemoryFactInspectionV1,
    ProjectMemoryFactListQueryV1, ProjectMemoryFactMergeCommandV1, ProjectMemoryFactMergeOutcomeV1,
    ProjectMemoryFactPageV1, ProjectMemoryFactProjectionV1, ProjectMemoryFactProposalEvidenceV1,
    ProjectMemoryFactProposalPageV1, ProjectMemoryFactProposalPromotionResultV1,
    ProjectMemoryFactProposalPromotionV1, ProjectMemoryFactProposalRecordV1,
    ProjectMemoryFactProposalRevisionV1, ProjectMemoryFactProposalStateV1,
    ProjectMemoryFactRemoveCommandV1, ProjectMemoryFactRemoveOutcomeV1,
    ProjectMemoryFactRetrievalCommandV1, ProjectMemoryFactSearchPageV1,
    ProjectMemoryFactSearchQuery, ProjectMemoryFactTargetV1, ProjectMemoryFactUpdateCommandV1,
    ProjectMemoryFactUpdateOutcomeV1, ProjectMemoryMemoryRepairCommandV1,
    ProjectMemoryMemoryRepairStatsV1, ProjectMemoryMemoryStatusV1, ProjectMemoryResult,
    PromoteFactProposal, PromoteFactProposalOutcome, RetrievalAnchorQuery, StoredFactV1,
};

/// Authoritative persistence boundary for append-only facts and evidence.
pub trait FactStore: Send + Sync {
    fn commit_fact(
        &self,
        batch: FactWriteBatch,
    ) -> impl Future<Output = FactStoreResult<FactCommitOutcome>> + Send;

    fn query_current_facts(
        &self,
        query: CurrentFactsQuery,
    ) -> impl Future<Output = FactStoreResult<Vec<StoredFactV1>>> + Send;

    fn query_fact_current(
        &self,
        query: FactCurrentQuery,
    ) -> impl Future<Output = FactStoreResult<Option<StoredFactV1>>> + Send;

    /// Required, never defaulted: a default body could only invent coverage
    /// counters and a contradiction state that no read observed, so every
    /// implementor must measure them against its own authority.
    fn query_fact_current_response(
        &self,
        query: FactCurrentQuery,
    ) -> impl Future<Output = FactStoreResult<FactCurrentResponseV1>> + Send;

    fn query_fact_as_of(
        &self,
        query: FactAsOfQuery,
    ) -> impl Future<Output = FactStoreResult<Option<StoredFactV1>>> + Send;

    /// Required for the same reason as [`FactStore::query_fact_current_response`].
    fn query_fact_as_of_response(
        &self,
        query: FactAsOfQuery,
    ) -> impl Future<Output = FactStoreResult<FactAsOfResponseV1>> + Send;

    fn query_fact_lineage(
        &self,
        query: FactLineageQuery,
    ) -> impl Future<Output = FactStoreResult<Vec<FactLineageEventV1>>> + Send;

    /// Required for the same reason as [`FactStore::query_fact_current_response`].
    fn query_fact_lineage_response(
        &self,
        query: FactLineageQuery,
    ) -> impl Future<Output = FactStoreResult<FactLineageResponseV1>> + Send;

    fn resolve_legacy_fact(
        &self,
        query: LegacyFactQuery,
    ) -> impl Future<Output = FactStoreResult<Option<FactId>>> + Send;

    fn get_retrieval_anchor(
        &self,
        query: RetrievalAnchorQuery,
    ) -> impl Future<Output = FactStoreResult<Option<RetrievalAnchorRecordV2>>> + Send;
}

/// Owner-bound compound authority for atomically promoting one proposal.
pub trait FactProposalStore: FactStore {
    fn promote_fact_proposal(
        &self,
        promotion: PromoteFactProposal,
    ) -> impl Future<Output = Result<PromoteFactProposalOutcome, FactProposalStoreError>> + Send;
}

/// Single typed authority boundary for the V1 compatibility surface.
pub trait ProjectMemoryFactStore: FactProposalStore {
    fn list_project_memory_facts(
        &self,
        query: ProjectMemoryFactListQueryV1,
    ) -> impl Future<Output = ProjectMemoryResult<ProjectMemoryFactPageV1>> + Send;

    fn search_project_memory_facts(
        &self,
        query: ProjectMemoryFactSearchQuery,
    ) -> impl Future<Output = ProjectMemoryResult<ProjectMemoryFactSearchPageV1>> + Send;

    fn probe_project_memory_facts(
        &self,
        query: ProjectMemoryFactSearchQuery,
    ) -> impl Future<Output = ProjectMemoryResult<ProjectMemoryFactSearchPageV1>> + Send;

    fn related_project_memory_facts(
        &self,
        query: ProjectMemoryFactSearchQuery,
    ) -> impl Future<Output = ProjectMemoryResult<ProjectMemoryFactSearchPageV1>> + Send;

    fn reason_project_memory_facts(
        &self,
        query: ProjectMemoryFactSearchQuery,
    ) -> impl Future<Output = ProjectMemoryResult<ProjectMemoryFactSearchPageV1>> + Send;

    fn find_project_memory_contradictions(
        &self,
        query: ProjectMemoryFactContradictionQueryV1,
    ) -> impl Future<Output = ProjectMemoryResult<ProjectMemoryFactContradictionPageV1>> + Send;

    fn get_project_memory_fact(
        &self,
        target: ProjectMemoryFactTargetV1,
    ) -> impl Future<Output = ProjectMemoryResult<Option<ProjectMemoryFactProjectionV1>>> + Send;

    fn project_memory_fact_history(
        &self,
        query: ProjectMemoryFactHistoryQueryV1,
    ) -> impl Future<Output = ProjectMemoryResult<ProjectMemoryFactHistoryV1>> + Send;

    /// Pure snapshot read. Implementations must report repair state without
    /// advancing a repair batch or acquiring the writer lane.
    fn project_memory_status(
        &self,
        owner: FactOwnerV1,
    ) -> impl Future<Output = ProjectMemoryResult<ProjectMemoryMemoryStatusV1>> + Send;

    fn inspect_project_memory_fact(
        &self,
        target: ProjectMemoryFactTargetV1,
    ) -> impl Future<Output = ProjectMemoryResult<Option<ProjectMemoryFactInspectionV1>>> + Send;

    fn add_project_memory_fact(
        &self,
        request: ProjectMemoryFactAddCommandV1,
    ) -> impl Future<Output = ProjectMemoryResult<ProjectMemoryFactAddOutcomeV1>> + Send;

    fn update_project_memory_fact(
        &self,
        request: ProjectMemoryFactUpdateCommandV1,
    ) -> impl Future<Output = ProjectMemoryResult<ProjectMemoryFactUpdateOutcomeV1>> + Send;

    fn remove_project_memory_fact(
        &self,
        request: ProjectMemoryFactRemoveCommandV1,
    ) -> impl Future<Output = ProjectMemoryResult<ProjectMemoryFactRemoveOutcomeV1>> + Send;

    fn record_project_memory_fact_feedback(
        &self,
        request: ProjectMemoryFactFeedbackCommandV1,
    ) -> impl Future<Output = ProjectMemoryResult<ProjectMemoryFactFeedbackOutcomeV1>> + Send;

    /// Pure snapshot read. Implementations must report repair state without
    /// advancing a repair batch or acquiring the writer lane.
    fn project_memory_fact_feedback_history(
        &self,
        query: ProjectMemoryFactFeedbackHistoryQueryV1,
    ) -> impl Future<Output = ProjectMemoryResult<ProjectMemoryFactFeedbackHistoryV1>> + Send;

    /// Owner-scoped exact lookup for deduplication. `content_digest` is opaque and
    /// must be derived by the application boundary; implementations never accept
    /// raw content for this read.
    fn find_project_memory_fact_by_content_digest(
        &self,
        query: ProjectMemoryFactContentDigestQueryV1,
    ) -> impl Future<Output = ProjectMemoryResult<Option<ProjectMemoryFactProjectionV1>>> + Send;

    /// Applies the finite V1 grooming operation set atomically for one owner.
    fn apply_project_memory_fact_curation(
        &self,
        request: ProjectMemoryFactCurationBatchV1,
    ) -> impl Future<Output = ProjectMemoryResult<ProjectMemoryFactCurationReceiptV1>> + Send;

    /// Merges legacy fact records under a caller supplied, owner-bound operation id.
    fn merge_project_memory_facts(
        &self,
        request: ProjectMemoryFactMergeCommandV1,
    ) -> impl Future<Output = ProjectMemoryResult<ProjectMemoryFactMergeOutcomeV1>> + Send;

    /// Repairs the finite V1 compatibility projection and returns measured
    /// results plus the exact feedback-history batch outcome from that same
    /// atomic command.
    fn repair_project_memory(
        &self,
        request: ProjectMemoryMemoryRepairCommandV1,
    ) -> impl Future<Output = ProjectMemoryResult<ProjectMemoryMemoryRepairStatsV1>> + Send;

    /// Bounded dashboard summary. Implementations return safe typed projections,
    /// never arbitrary SQL rows or raw payloads for unavailable records.
    fn dashboard_project_memory_overview(
        &self,
        query: ProjectMemoryDashboardMemoryOverviewQueryV1,
    ) -> impl Future<Output = ProjectMemoryResult<ProjectMemoryDashboardMemoryOverviewV1>> + Send;

    /// Owner-bound detail view for one legacy fact and its typed entity links.
    fn dashboard_project_memory_fact_detail(
        &self,
        query: ProjectMemoryDashboardFactDetailQueryV1,
    ) -> impl Future<Output = ProjectMemoryResult<Option<ProjectMemoryDashboardFactDetailV1>>> + Send;

    /// Bounded, finite vector points. Similarity pairs are deliberately derived
    /// from this capped output at the dashboard edge rather than by a generic query API.
    fn dashboard_project_memory_vector_points(
        &self,
        query: ProjectMemoryDashboardVectorPointsQueryV1,
    ) -> impl Future<Output = ProjectMemoryResult<Vec<ProjectMemoryDashboardVectorPointV1>>> + Send;

    /// Bounded owner-scoped audit projection with availability-preserving details.
    fn dashboard_project_memory_oplog(
        &self,
        query: ProjectMemoryDashboardOplogQueryV1,
    ) -> impl Future<Output = ProjectMemoryResult<Vec<ProjectMemoryDashboardOplogEntryV1>>> + Send;

    fn record_project_memory_fact_retrieval(
        &self,
        request: ProjectMemoryFactRetrievalCommandV1,
    ) -> impl Future<Output = ProjectMemoryResult<Vec<ProjectMemoryFactProjectionV1>>> + Send;

    fn submit_project_memory_fact_proposal(
        &self,
        proposal_id: ProvenanceId,
        request: ProjectMemoryFactAddCommandV1,
        submitter: Option<ActorId>,
        evidence: ProjectMemoryFactProposalEvidenceV1,
    ) -> impl Future<Output = ProjectMemoryResult<ProjectMemoryFactProposalRecordV1>> + Send;

    fn get_project_memory_fact_proposal(
        &self,
        owner: FactOwnerV1,
        proposal_id: ProvenanceId,
    ) -> impl Future<Output = ProjectMemoryResult<Option<ProjectMemoryFactProposalRecordV1>>> + Send;

    #[allow(clippy::too_many_arguments)]
    fn list_project_memory_fact_proposals(
        &self,
        owner: FactOwnerV1,
        state: Option<ProjectMemoryFactProposalStateV1>,
        after_proposal_id: Option<ProvenanceId>,
        limit: usize,
    ) -> impl Future<Output = ProjectMemoryResult<ProjectMemoryFactProposalPageV1>> + Send;

    fn count_pending_project_memory_fact_proposals(
        &self,
        owner: FactOwnerV1,
    ) -> impl Future<Output = ProjectMemoryResult<u64>> + Send;

    #[allow(clippy::too_many_arguments)]
    fn reject_project_memory_fact_proposal(
        &self,
        owner: FactOwnerV1,
        proposal_id: ProvenanceId,
        expected_revision: ProjectMemoryFactProposalRevisionV1,
        reviewer: ActorId,
        reason: String,
    ) -> impl Future<Output = ProjectMemoryResult<ProjectMemoryFactProposalRecordV1>> + Send;

    fn promote_project_memory_fact_proposal(
        &self,
        request: ProjectMemoryFactProposalPromotionV1,
    ) -> impl Future<Output = ProjectMemoryResult<ProjectMemoryFactProposalRecordV1>> + Send;

    /// Atomic promotion result for callers that must distinguish a new decision
    /// from an idempotent replay without a racy pre-read.
    fn promote_project_memory_fact_proposal_with_disposition(
        &self,
        request: ProjectMemoryFactProposalPromotionV1,
    ) -> impl Future<Output = ProjectMemoryResult<ProjectMemoryFactProposalPromotionResultV1>> + Send;
}
