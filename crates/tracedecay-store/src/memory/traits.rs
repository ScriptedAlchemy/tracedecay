use std::future::Future;
use tracedecay_domain::{
    ActorId, FactId, FactLineageEventV1, FactOwnerV1, ProvenanceId, RetrievalAnchorRecordV2,
};

use super::{
    CurrentFactsQuery, DashboardFactDetail, DashboardFactDetailQuery, DashboardMemoryOverview,
    DashboardMemoryOverviewQuery, DashboardOplogEntry, DashboardOplogQuery, DashboardVectorPoint,
    DashboardVectorPointsQuery, FactAddCommand, FactAddOutcome, FactAsOfQuery, FactAsOfResponseV1,
    FactCommitOutcome, FactContentDigestQuery, FactContradictionPage, FactContradictionQuery,
    FactCurationBatch, FactCurationReceipt, FactCurrentQuery, FactCurrentResponseV1,
    FactFeedbackCommand, FactFeedbackHistory, FactFeedbackHistoryQuery, FactFeedbackOutcome,
    FactHistory, FactHistoryQuery, FactInspection, FactLineageQuery, FactLineageResponseV1,
    FactLineageResult, FactListQuery, FactMergeCommand, FactMergeOutcome, FactPage, FactProjection,
    FactProposalEvidence, FactProposalPage, FactProposalPromotion, FactProposalPromotionResult,
    FactProposalRecord, FactProposalRevision, FactProposalState, FactProposalStoreError,
    FactRemoveCommand, FactRemoveOutcome, FactRetrievalCommand, FactSearchPage, FactSearchQuery,
    FactStoreResult, FactTarget, FactUpdateCommand, FactUpdateOutcome, FactWriteBatch,
    LegacyFactQuery, MemoryRepairCommand, MemoryRepairStats, MemoryStatus, PromoteFactProposal,
    PromoteFactProposalOutcome, RetrievalAnchorQuery, StoredFactV1,
};

/// Authoritative persistence boundary for append-only facts and evidence.
pub trait FactLineageStore: Send + Sync {
    fn commit_fact(
        &self,
        batch: FactWriteBatch,
    ) -> impl Future<Output = FactLineageResult<FactCommitOutcome>> + Send;

    fn query_current_facts(
        &self,
        query: CurrentFactsQuery,
    ) -> impl Future<Output = FactLineageResult<Vec<StoredFactV1>>> + Send;

    fn query_fact_current(
        &self,
        query: FactCurrentQuery,
    ) -> impl Future<Output = FactLineageResult<Option<StoredFactV1>>> + Send;

    /// Required, never defaulted: a default body could only invent coverage
    /// counters and a contradiction state that no read observed, so every
    /// implementor must measure them against its own authority.
    fn query_fact_current_response(
        &self,
        query: FactCurrentQuery,
    ) -> impl Future<Output = FactLineageResult<FactCurrentResponseV1>> + Send;

    fn query_fact_as_of(
        &self,
        query: FactAsOfQuery,
    ) -> impl Future<Output = FactLineageResult<Option<StoredFactV1>>> + Send;

    /// Required for the same reason as [`FactLineageStore::query_fact_current_response`].
    fn query_fact_as_of_response(
        &self,
        query: FactAsOfQuery,
    ) -> impl Future<Output = FactLineageResult<FactAsOfResponseV1>> + Send;

    fn query_fact_lineage(
        &self,
        query: FactLineageQuery,
    ) -> impl Future<Output = FactLineageResult<Vec<FactLineageEventV1>>> + Send;

    /// Required for the same reason as [`FactLineageStore::query_fact_current_response`].
    fn query_fact_lineage_response(
        &self,
        query: FactLineageQuery,
    ) -> impl Future<Output = FactLineageResult<FactLineageResponseV1>> + Send;

    fn resolve_legacy_fact(
        &self,
        query: LegacyFactQuery,
    ) -> impl Future<Output = FactLineageResult<Option<FactId>>> + Send;

    fn get_retrieval_anchor(
        &self,
        query: RetrievalAnchorQuery,
    ) -> impl Future<Output = FactLineageResult<Option<RetrievalAnchorRecordV2>>> + Send;
}

/// Owner-bound compound authority for atomically promoting one proposal.
pub trait FactProposalStore: FactLineageStore {
    fn commit_fact_proposal(
        &self,
        promotion: PromoteFactProposal,
    ) -> impl Future<Output = Result<PromoteFactProposalOutcome, FactProposalStoreError>> + Send;
}

/// Single typed authority boundary for the V1 compatibility surface.
pub trait FactStore: FactProposalStore {
    fn list_facts(
        &self,
        query: FactListQuery,
    ) -> impl Future<Output = FactStoreResult<FactPage>> + Send;

    fn search_facts(
        &self,
        query: FactSearchQuery,
    ) -> impl Future<Output = FactStoreResult<FactSearchPage>> + Send;

    fn probe_facts(
        &self,
        query: FactSearchQuery,
    ) -> impl Future<Output = FactStoreResult<FactSearchPage>> + Send;

    fn related_facts(
        &self,
        query: FactSearchQuery,
    ) -> impl Future<Output = FactStoreResult<FactSearchPage>> + Send;

    fn reason_facts(
        &self,
        query: FactSearchQuery,
    ) -> impl Future<Output = FactStoreResult<FactSearchPage>> + Send;

    fn find_contradictions(
        &self,
        query: FactContradictionQuery,
    ) -> impl Future<Output = FactStoreResult<FactContradictionPage>> + Send;

    fn get_fact(
        &self,
        target: FactTarget,
    ) -> impl Future<Output = FactStoreResult<Option<FactProjection>>> + Send;

    fn fact_history(
        &self,
        query: FactHistoryQuery,
    ) -> impl Future<Output = FactStoreResult<FactHistory>> + Send;

    /// Pure snapshot read. Implementations must report repair state without
    /// advancing a repair batch or acquiring the writer lane.
    fn memory_status(
        &self,
        owner: FactOwnerV1,
    ) -> impl Future<Output = FactStoreResult<MemoryStatus>> + Send;

    fn inspect_fact(
        &self,
        target: FactTarget,
    ) -> impl Future<Output = FactStoreResult<Option<FactInspection>>> + Send;

    fn add_fact(
        &self,
        request: FactAddCommand,
    ) -> impl Future<Output = FactStoreResult<FactAddOutcome>> + Send;

    fn update_fact(
        &self,
        request: FactUpdateCommand,
    ) -> impl Future<Output = FactStoreResult<FactUpdateOutcome>> + Send;

    fn remove_fact(
        &self,
        request: FactRemoveCommand,
    ) -> impl Future<Output = FactStoreResult<FactRemoveOutcome>> + Send;

    fn record_fact_feedback(
        &self,
        request: FactFeedbackCommand,
    ) -> impl Future<Output = FactStoreResult<FactFeedbackOutcome>> + Send;

    /// Pure snapshot read. Implementations must report repair state without
    /// advancing a repair batch or acquiring the writer lane.
    fn fact_feedback_history(
        &self,
        query: FactFeedbackHistoryQuery,
    ) -> impl Future<Output = FactStoreResult<FactFeedbackHistory>> + Send;

    /// Owner-scoped exact lookup for deduplication. `content_digest` is opaque and
    /// must be derived by the application boundary; implementations never accept
    /// raw content for this read.
    fn find_fact_by_content_digest(
        &self,
        query: FactContentDigestQuery,
    ) -> impl Future<Output = FactStoreResult<Option<FactProjection>>> + Send;

    /// Applies the finite V1 grooming operation set atomically for one owner.
    fn apply_fact_curation(
        &self,
        request: FactCurationBatch,
    ) -> impl Future<Output = FactStoreResult<FactCurationReceipt>> + Send;

    /// Merges legacy fact records under a caller supplied, owner-bound operation id.
    fn merge_facts(
        &self,
        request: FactMergeCommand,
    ) -> impl Future<Output = FactStoreResult<FactMergeOutcome>> + Send;

    /// Repairs the finite V1 compatibility projection and returns measured
    /// results plus the exact feedback-history batch outcome from that same
    /// atomic command.
    fn repair_memory(
        &self,
        request: MemoryRepairCommand,
    ) -> impl Future<Output = FactStoreResult<MemoryRepairStats>> + Send;

    /// Bounded dashboard summary. Implementations return safe typed projections,
    /// never arbitrary SQL rows or raw payloads for unavailable records.
    fn dashboard_memory_overview(
        &self,
        query: DashboardMemoryOverviewQuery,
    ) -> impl Future<Output = FactStoreResult<DashboardMemoryOverview>> + Send;

    /// Owner-bound detail view for one legacy fact and its typed entity links.
    fn dashboard_fact_detail(
        &self,
        query: DashboardFactDetailQuery,
    ) -> impl Future<Output = FactStoreResult<Option<DashboardFactDetail>>> + Send;

    /// Bounded, finite vector points. Similarity pairs are deliberately derived
    /// from this capped output at the dashboard edge rather than by a generic query API.
    fn dashboard_vector_points(
        &self,
        query: DashboardVectorPointsQuery,
    ) -> impl Future<Output = FactStoreResult<Vec<DashboardVectorPoint>>> + Send;

    /// Bounded owner-scoped audit projection with availability-preserving details.
    fn dashboard_memory_oplog(
        &self,
        query: DashboardOplogQuery,
    ) -> impl Future<Output = FactStoreResult<Vec<DashboardOplogEntry>>> + Send;

    fn record_fact_retrieval(
        &self,
        request: FactRetrievalCommand,
    ) -> impl Future<Output = FactStoreResult<Vec<FactProjection>>> + Send;

    fn submit_fact_proposal(
        &self,
        proposal_id: ProvenanceId,
        request: FactAddCommand,
        submitter: Option<ActorId>,
        evidence: FactProposalEvidence,
    ) -> impl Future<Output = FactStoreResult<FactProposalRecord>> + Send;

    fn get_fact_proposal(
        &self,
        owner: FactOwnerV1,
        proposal_id: ProvenanceId,
    ) -> impl Future<Output = FactStoreResult<Option<FactProposalRecord>>> + Send;

    #[allow(clippy::too_many_arguments)]
    fn list_fact_proposals(
        &self,
        owner: FactOwnerV1,
        state: Option<FactProposalState>,
        after_proposal_id: Option<ProvenanceId>,
        limit: usize,
    ) -> impl Future<Output = FactStoreResult<FactProposalPage>> + Send;

    fn count_pending_fact_proposals(
        &self,
        owner: FactOwnerV1,
    ) -> impl Future<Output = FactStoreResult<u64>> + Send;

    #[allow(clippy::too_many_arguments)]
    fn reject_fact_proposal(
        &self,
        owner: FactOwnerV1,
        proposal_id: ProvenanceId,
        expected_revision: FactProposalRevision,
        reviewer: ActorId,
        reason: String,
    ) -> impl Future<Output = FactStoreResult<FactProposalRecord>> + Send;

    fn promote_fact_proposal(
        &self,
        request: FactProposalPromotion,
    ) -> impl Future<Output = FactStoreResult<FactProposalRecord>> + Send;

    /// Atomic promotion result for callers that must distinguish a new decision
    /// from an idempotent replay without a racy pre-read.
    fn promote_fact_proposal_with_disposition(
        &self,
        request: FactProposalPromotion,
    ) -> impl Future<Output = FactStoreResult<FactProposalPromotionResult>> + Send;
}
