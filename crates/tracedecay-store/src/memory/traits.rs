use std::future::Future;
use tracedecay_domain::{
    ActorId, FactId, FactLineageEventV1, FactOwnerV1, ProvenanceId, RetrievalAnchorRecordV2,
};

use super::{
    CompatibilityDashboardFactDetailQueryV1, CompatibilityDashboardFactDetailV1,
    CompatibilityDashboardMemoryOverviewQueryV1, CompatibilityDashboardMemoryOverviewV1,
    CompatibilityDashboardOplogEntryV1, CompatibilityDashboardOplogQueryV1,
    CompatibilityDashboardVectorPointV1, CompatibilityDashboardVectorPointsQueryV1,
    CompatibilityFactAddCommandV1, CompatibilityFactAddOutcomeV1,
    CompatibilityFactContentDigestQueryV1, CompatibilityFactContradictionPageV1,
    CompatibilityFactContradictionQueryV1, CompatibilityFactCurationBatchV1,
    CompatibilityFactCurationReceiptV1, CompatibilityFactFeedbackCommandV1,
    CompatibilityFactFeedbackHistoryQueryV1, CompatibilityFactFeedbackHistoryV1,
    CompatibilityFactFeedbackOutcomeV1, CompatibilityFactHistoryQueryV1,
    CompatibilityFactHistoryV1, CompatibilityFactInspectionV1, CompatibilityFactListQueryV1,
    CompatibilityFactMergeCommandV1, CompatibilityFactMergeOutcomeV1, CompatibilityFactPageV1,
    CompatibilityFactProjectionV1, CompatibilityFactProposalImportReceiptV1,
    CompatibilityFactProposalImportV1, CompatibilityFactProposalPageV1,
    CompatibilityFactProposalPromotionResultV1, CompatibilityFactProposalPromotionV1,
    CompatibilityFactProposalRecordV1, CompatibilityFactProposalRevisionV1,
    CompatibilityFactProposalStateV1, CompatibilityFactRemoveCommandV1,
    CompatibilityFactRemoveOutcomeV1, CompatibilityFactRetrievalCommandV1,
    CompatibilityFactSearchPageV1, CompatibilityFactSearchQuery, CompatibilityFactTargetV1,
    CompatibilityFactUpdateCommandV1, CompatibilityFactUpdateOutcomeV1,
    CompatibilityMemoryRepairCommandV1, CompatibilityMemoryRepairStatsV1,
    CompatibilityMemoryStatusV1, CurrentFactsQuery, FactAsOfQuery, FactAsOfResponseV1,
    FactCommitOutcome, FactCompatibilityResult, FactCurrentQuery, FactCurrentResponseV1,
    FactLineageQuery, FactLineageResponseV1, FactProposalStoreError, FactStoreResult,
    FactWriteBatch, LegacyFactQuery, PromoteFactProposal, PromoteFactProposalOutcome,
    RetrievalAnchorQuery, StoredFactV1,
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
pub trait FactCompatibilityStore: FactProposalStore {
    fn list_compatibility_facts(
        &self,
        query: CompatibilityFactListQueryV1,
    ) -> impl Future<Output = FactCompatibilityResult<CompatibilityFactPageV1>> + Send;

    fn search_compatibility_facts(
        &self,
        query: CompatibilityFactSearchQuery,
    ) -> impl Future<Output = FactCompatibilityResult<CompatibilityFactSearchPageV1>> + Send;

    fn probe_compatibility_facts(
        &self,
        query: CompatibilityFactSearchQuery,
    ) -> impl Future<Output = FactCompatibilityResult<CompatibilityFactSearchPageV1>> + Send;

    fn related_compatibility_facts(
        &self,
        query: CompatibilityFactSearchQuery,
    ) -> impl Future<Output = FactCompatibilityResult<CompatibilityFactSearchPageV1>> + Send;

    fn reason_compatibility_facts(
        &self,
        query: CompatibilityFactSearchQuery,
    ) -> impl Future<Output = FactCompatibilityResult<CompatibilityFactSearchPageV1>> + Send;

    fn find_compatibility_contradictions(
        &self,
        query: CompatibilityFactContradictionQueryV1,
    ) -> impl Future<Output = FactCompatibilityResult<CompatibilityFactContradictionPageV1>> + Send;

    fn get_compatibility_fact(
        &self,
        target: CompatibilityFactTargetV1,
    ) -> impl Future<Output = FactCompatibilityResult<Option<CompatibilityFactProjectionV1>>> + Send;

    fn compatibility_fact_history(
        &self,
        query: CompatibilityFactHistoryQueryV1,
    ) -> impl Future<Output = FactCompatibilityResult<CompatibilityFactHistoryV1>> + Send;

    /// Pure snapshot read. Implementations must report repair state without
    /// advancing a repair batch or acquiring the writer lane.
    fn compatibility_memory_status(
        &self,
        owner: FactOwnerV1,
    ) -> impl Future<Output = FactCompatibilityResult<CompatibilityMemoryStatusV1>> + Send;

    fn inspect_compatibility_fact(
        &self,
        target: CompatibilityFactTargetV1,
    ) -> impl Future<Output = FactCompatibilityResult<Option<CompatibilityFactInspectionV1>>> + Send;

    fn add_compatibility_fact(
        &self,
        request: CompatibilityFactAddCommandV1,
    ) -> impl Future<Output = FactCompatibilityResult<CompatibilityFactAddOutcomeV1>> + Send;

    fn update_compatibility_fact(
        &self,
        request: CompatibilityFactUpdateCommandV1,
    ) -> impl Future<Output = FactCompatibilityResult<CompatibilityFactUpdateOutcomeV1>> + Send;

    fn remove_compatibility_fact(
        &self,
        request: CompatibilityFactRemoveCommandV1,
    ) -> impl Future<Output = FactCompatibilityResult<CompatibilityFactRemoveOutcomeV1>> + Send;

    fn record_compatibility_fact_feedback(
        &self,
        request: CompatibilityFactFeedbackCommandV1,
    ) -> impl Future<Output = FactCompatibilityResult<CompatibilityFactFeedbackOutcomeV1>> + Send;

    /// Pure snapshot read. Implementations must report repair state without
    /// advancing a repair batch or acquiring the writer lane.
    fn compatibility_fact_feedback_history(
        &self,
        query: CompatibilityFactFeedbackHistoryQueryV1,
    ) -> impl Future<Output = FactCompatibilityResult<CompatibilityFactFeedbackHistoryV1>> + Send;

    /// Owner-scoped exact lookup for deduplication. `content_digest` is opaque and
    /// must be derived by the application boundary; implementations never accept
    /// raw content for this read.
    fn find_compatibility_fact_by_content_digest(
        &self,
        query: CompatibilityFactContentDigestQueryV1,
    ) -> impl Future<Output = FactCompatibilityResult<Option<CompatibilityFactProjectionV1>>> + Send;

    /// Applies the finite V1 grooming operation set atomically for one owner.
    fn apply_compatibility_fact_curation(
        &self,
        request: CompatibilityFactCurationBatchV1,
    ) -> impl Future<Output = FactCompatibilityResult<CompatibilityFactCurationReceiptV1>> + Send;

    /// Merges legacy fact records under a caller supplied, owner-bound operation id.
    fn merge_compatibility_facts(
        &self,
        request: CompatibilityFactMergeCommandV1,
    ) -> impl Future<Output = FactCompatibilityResult<CompatibilityFactMergeOutcomeV1>> + Send;

    /// Repairs the finite V1 compatibility projection and returns measured
    /// results plus the exact feedback-history batch outcome from that same
    /// atomic command.
    fn repair_compatibility_memory(
        &self,
        request: CompatibilityMemoryRepairCommandV1,
    ) -> impl Future<Output = FactCompatibilityResult<CompatibilityMemoryRepairStatsV1>> + Send;

    /// Bounded dashboard summary. Implementations return safe typed projections,
    /// never arbitrary SQL rows or raw payloads for unavailable records.
    fn dashboard_compatibility_memory_overview(
        &self,
        query: CompatibilityDashboardMemoryOverviewQueryV1,
    ) -> impl Future<Output = FactCompatibilityResult<CompatibilityDashboardMemoryOverviewV1>> + Send;

    /// Owner-bound detail view for one legacy fact and its typed entity links.
    fn dashboard_compatibility_fact_detail(
        &self,
        query: CompatibilityDashboardFactDetailQueryV1,
    ) -> impl Future<Output = FactCompatibilityResult<Option<CompatibilityDashboardFactDetailV1>>> + Send;

    /// Bounded, finite vector points. Similarity pairs are deliberately derived
    /// from this capped output at the dashboard edge rather than by a generic query API.
    fn dashboard_compatibility_vector_points(
        &self,
        query: CompatibilityDashboardVectorPointsQueryV1,
    ) -> impl Future<Output = FactCompatibilityResult<Vec<CompatibilityDashboardVectorPointV1>>> + Send;

    /// Bounded owner-scoped audit projection with availability-preserving details.
    fn dashboard_compatibility_memory_oplog(
        &self,
        query: CompatibilityDashboardOplogQueryV1,
    ) -> impl Future<Output = FactCompatibilityResult<Vec<CompatibilityDashboardOplogEntryV1>>> + Send;

    fn record_compatibility_fact_retrieval(
        &self,
        request: CompatibilityFactRetrievalCommandV1,
    ) -> impl Future<Output = FactCompatibilityResult<Vec<CompatibilityFactProjectionV1>>> + Send;

    fn submit_compatibility_fact_proposal(
        &self,
        proposal_id: ProvenanceId,
        request: CompatibilityFactAddCommandV1,
        submitter: Option<ActorId>,
    ) -> impl Future<Output = FactCompatibilityResult<CompatibilityFactProposalRecordV1>> + Send;

    fn get_compatibility_fact_proposal(
        &self,
        owner: FactOwnerV1,
        proposal_id: ProvenanceId,
    ) -> impl Future<Output = FactCompatibilityResult<Option<CompatibilityFactProposalRecordV1>>> + Send;

    #[allow(clippy::too_many_arguments)]
    fn list_compatibility_fact_proposals(
        &self,
        owner: FactOwnerV1,
        state: Option<CompatibilityFactProposalStateV1>,
        after_proposal_id: Option<ProvenanceId>,
        limit: usize,
    ) -> impl Future<Output = FactCompatibilityResult<CompatibilityFactProposalPageV1>> + Send;

    fn count_pending_compatibility_fact_proposals(
        &self,
        owner: FactOwnerV1,
    ) -> impl Future<Output = FactCompatibilityResult<u64>> + Send;

    #[allow(clippy::too_many_arguments)]
    fn reject_compatibility_fact_proposal(
        &self,
        owner: FactOwnerV1,
        proposal_id: ProvenanceId,
        expected_revision: CompatibilityFactProposalRevisionV1,
        reviewer: ActorId,
        reason: String,
    ) -> impl Future<Output = FactCompatibilityResult<CompatibilityFactProposalRecordV1>> + Send;

    fn import_legacy_compatibility_fact_proposals(
        &self,
        request: CompatibilityFactProposalImportV1,
    ) -> impl Future<Output = FactCompatibilityResult<CompatibilityFactProposalImportReceiptV1>> + Send;

    fn promote_compatibility_fact_proposal(
        &self,
        request: CompatibilityFactProposalPromotionV1,
    ) -> impl Future<Output = FactCompatibilityResult<CompatibilityFactProposalRecordV1>> + Send;

    /// Atomic promotion result for callers that must distinguish a new decision
    /// from an idempotent replay without a racy pre-read.
    fn promote_compatibility_fact_proposal_with_disposition(
        &self,
        request: CompatibilityFactProposalPromotionV1,
    ) -> impl Future<Output = FactCompatibilityResult<CompatibilityFactProposalPromotionResultV1>> + Send;
}
