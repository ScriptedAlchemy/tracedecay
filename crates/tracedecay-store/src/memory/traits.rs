use std::future::Future;
use tracedecay_domain::RunId;
use tracedecay_domain::{FactLineageEventV1, FactOwnerV1, ProvenanceId, RetrievalAnchorRecordV2};

use super::ProjectMemoryAutomationRunReceiptsV1;
use super::{
    CurrentFactsQuery, FactAsOfQuery, FactAsOfResponseV1, FactCommitOutcome, FactCurrentQuery,
    FactCurrentResponseV1, FactLineageQuery, FactLineageResponseV1, FactReadControl,
    FactStoreResult, FactWriteBatch, FactWriteControl, ProjectMemoryAutomaticFactApplyResultV1,
    ProjectMemoryAutomaticFactEvidenceV1, ProjectMemoryAutomaticFactReceiptPageV1,
    ProjectMemoryAutomaticFactReceiptV1, ProjectMemoryAutomaticFactStateV1,
    ProjectMemoryDashboardFactDetailQueryV1, ProjectMemoryDashboardFactDetailV1,
    ProjectMemoryDashboardMemoryOverviewQueryV1, ProjectMemoryDashboardMemoryOverviewV1,
    ProjectMemoryDashboardOplogEntryV1, ProjectMemoryDashboardOplogQueryV1,
    ProjectMemoryDashboardVectorPointV1, ProjectMemoryDashboardVectorPointsQueryV1,
    ProjectMemoryFactAddCommandV1, ProjectMemoryFactAddOutcomeV1,
    ProjectMemoryFactContentDigestQueryV1, ProjectMemoryFactContradictionPageV1,
    ProjectMemoryFactContradictionQueryV1, ProjectMemoryFactCurationBatchV1,
    ProjectMemoryFactCurationReceiptV1, ProjectMemoryFactFeedbackCommandV1,
    ProjectMemoryFactFeedbackHistoryQueryV1, ProjectMemoryFactFeedbackHistoryV1,
    ProjectMemoryFactFeedbackOutcomeV1, ProjectMemoryFactHistoryQueryV1,
    ProjectMemoryFactHistoryV1, ProjectMemoryFactIdV1, ProjectMemoryFactInspectionV1,
    ProjectMemoryFactListQueryV1, ProjectMemoryFactMergeCommandV1, ProjectMemoryFactMergeOutcomeV1,
    ProjectMemoryFactPageV1, ProjectMemoryFactProjectionV1, ProjectMemoryFactRemoveCommandV1,
    ProjectMemoryFactRemoveOutcomeV1, ProjectMemoryFactRetrievalCommandV1,
    ProjectMemoryFactRetrievalOutcomeV1, ProjectMemoryFactSearchPageV1,
    ProjectMemoryFactSearchQuery, ProjectMemoryFactUpdateCommandV1,
    ProjectMemoryFactUpdateOutcomeV1, ProjectMemoryMemoryStatusV1, RetrievalAnchorQuery,
    StoredFactV1,
};

/// Authoritative persistence boundary for append-only facts and evidence.
pub trait FactStore: Send + Sync {
    fn commit_fact(
        &self,
        batch: FactWriteBatch,
        write_control: &FactWriteControl,
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

    fn get_retrieval_anchor(
        &self,
        query: RetrievalAnchorQuery,
    ) -> impl Future<Output = FactStoreResult<Option<RetrievalAnchorRecordV2>>> + Send;
}

/// Single typed authority boundary for canonical project memory.
pub trait ProjectMemoryFactStore: FactStore {
    fn list_project_memory_facts(
        &self,
        query: ProjectMemoryFactListQueryV1,
        read_control: &FactReadControl,
    ) -> impl Future<Output = FactStoreResult<ProjectMemoryFactPageV1>> + Send;

    fn search_project_memory_facts(
        &self,
        query: ProjectMemoryFactSearchQuery,
        read_control: &FactReadControl,
    ) -> impl Future<Output = FactStoreResult<ProjectMemoryFactSearchPageV1>> + Send;

    fn probe_project_memory_facts(
        &self,
        query: ProjectMemoryFactSearchQuery,
        read_control: &FactReadControl,
    ) -> impl Future<Output = FactStoreResult<ProjectMemoryFactSearchPageV1>> + Send;

    fn related_project_memory_facts(
        &self,
        query: ProjectMemoryFactSearchQuery,
        read_control: &FactReadControl,
    ) -> impl Future<Output = FactStoreResult<ProjectMemoryFactSearchPageV1>> + Send;

    fn reason_project_memory_facts(
        &self,
        query: ProjectMemoryFactSearchQuery,
        read_control: &FactReadControl,
    ) -> impl Future<Output = FactStoreResult<ProjectMemoryFactSearchPageV1>> + Send;

    fn find_project_memory_contradictions(
        &self,
        query: ProjectMemoryFactContradictionQueryV1,
        read_control: &FactReadControl,
    ) -> impl Future<Output = FactStoreResult<ProjectMemoryFactContradictionPageV1>> + Send;

    fn get_project_memory_fact(
        &self,
        target: ProjectMemoryFactIdV1,
        read_control: &FactReadControl,
    ) -> impl Future<Output = FactStoreResult<Option<ProjectMemoryFactProjectionV1>>> + Send;

    fn project_memory_fact_history(
        &self,
        query: ProjectMemoryFactHistoryQueryV1,
        read_control: &FactReadControl,
    ) -> impl Future<Output = FactStoreResult<ProjectMemoryFactHistoryV1>> + Send;

    /// Pure owner-scoped snapshot read that never acquires the writer lane.
    fn project_memory_status(
        &self,
        owner: FactOwnerV1,
        read_control: &FactReadControl,
    ) -> impl Future<Output = FactStoreResult<ProjectMemoryMemoryStatusV1>> + Send;

    fn inspect_project_memory_fact(
        &self,
        target: ProjectMemoryFactIdV1,
        read_control: &FactReadControl,
    ) -> impl Future<Output = FactStoreResult<Option<ProjectMemoryFactInspectionV1>>> + Send;

    fn add_project_memory_fact(
        &self,
        request: ProjectMemoryFactAddCommandV1,
        write_control: &FactWriteControl,
    ) -> impl Future<Output = FactStoreResult<ProjectMemoryFactAddOutcomeV1>> + Send;

    fn update_project_memory_fact(
        &self,
        request: ProjectMemoryFactUpdateCommandV1,
        write_control: &FactWriteControl,
    ) -> impl Future<Output = FactStoreResult<ProjectMemoryFactUpdateOutcomeV1>> + Send;

    fn remove_project_memory_fact(
        &self,
        request: ProjectMemoryFactRemoveCommandV1,
        write_control: &FactWriteControl,
    ) -> impl Future<Output = FactStoreResult<ProjectMemoryFactRemoveOutcomeV1>> + Send;

    fn record_project_memory_fact_feedback(
        &self,
        request: ProjectMemoryFactFeedbackCommandV1,
        write_control: &FactWriteControl,
    ) -> impl Future<Output = FactStoreResult<ProjectMemoryFactFeedbackOutcomeV1>> + Send;

    /// Pure owner-scoped snapshot read that never acquires the writer lane.
    fn project_memory_fact_feedback_history(
        &self,
        query: ProjectMemoryFactFeedbackHistoryQueryV1,
        read_control: &FactReadControl,
    ) -> impl Future<Output = FactStoreResult<ProjectMemoryFactFeedbackHistoryV1>> + Send;

    /// Owner-scoped exact lookup for deduplication. `content_digest` is opaque and
    /// must be derived by the application boundary; implementations never accept
    /// raw content for this read.
    fn find_project_memory_fact_by_content_digest(
        &self,
        query: ProjectMemoryFactContentDigestQueryV1,
        read_control: &FactReadControl,
    ) -> impl Future<Output = FactStoreResult<Option<ProjectMemoryFactProjectionV1>>> + Send;

    /// Applies the finite curation operation set atomically for one owner.
    fn apply_project_memory_fact_curation(
        &self,
        request: ProjectMemoryFactCurationBatchV1,
        write_control: &FactWriteControl,
    ) -> impl Future<Output = FactStoreResult<ProjectMemoryFactCurationReceiptV1>> + Send;

    /// Merges canonical fact records under a caller supplied, owner-bound operation id.
    fn merge_project_memory_facts(
        &self,
        request: ProjectMemoryFactMergeCommandV1,
        write_control: &FactWriteControl,
    ) -> impl Future<Output = FactStoreResult<ProjectMemoryFactMergeOutcomeV1>> + Send;

    /// Bounded dashboard summary. Implementations return safe typed projections,
    /// never arbitrary SQL rows or raw payloads for unavailable records.
    fn dashboard_project_memory_overview(
        &self,
        query: ProjectMemoryDashboardMemoryOverviewQueryV1,
        read_control: &FactReadControl,
    ) -> impl Future<Output = FactStoreResult<ProjectMemoryDashboardMemoryOverviewV1>> + Send;

    /// Owner-bound detail view for one canonical fact and its typed entity links.
    fn dashboard_project_memory_fact_detail(
        &self,
        query: ProjectMemoryDashboardFactDetailQueryV1,
        read_control: &FactReadControl,
    ) -> impl Future<Output = FactStoreResult<Option<ProjectMemoryDashboardFactDetailV1>>> + Send;

    /// Bounded, finite vector points. Similarity pairs are deliberately derived
    /// from this capped output at the dashboard edge rather than by a generic query API.
    fn dashboard_project_memory_vector_points(
        &self,
        query: ProjectMemoryDashboardVectorPointsQueryV1,
        read_control: &FactReadControl,
    ) -> impl Future<Output = FactStoreResult<Vec<ProjectMemoryDashboardVectorPointV1>>> + Send;

    /// Bounded owner-scoped lineage audit projection.
    fn dashboard_project_memory_oplog(
        &self,
        query: ProjectMemoryDashboardOplogQueryV1,
        read_control: &FactReadControl,
    ) -> impl Future<Output = FactStoreResult<Vec<ProjectMemoryDashboardOplogEntryV1>>> + Send;

    fn record_project_memory_fact_retrieval(
        &self,
        request: ProjectMemoryFactRetrievalCommandV1,
        write_control: &FactWriteControl,
    ) -> impl Future<Output = FactStoreResult<ProjectMemoryFactRetrievalOutcomeV1>> + Send;

    /// Applies one validated automation fact and records only its terminal
    /// outcome in the same transaction as the fact write.
    fn apply_project_memory_automatic_fact(
        &self,
        apply_id: ProvenanceId,
        request: ProjectMemoryFactAddCommandV1,
        evidence: ProjectMemoryAutomaticFactEvidenceV1,
        write_control: &FactWriteControl,
    ) -> impl Future<Output = FactStoreResult<ProjectMemoryAutomaticFactApplyResultV1>> + Send;

    fn get_project_memory_automatic_fact_receipt(
        &self,
        owner: FactOwnerV1,
        apply_id: ProvenanceId,
        read_control: &FactReadControl,
    ) -> impl Future<Output = FactStoreResult<Option<ProjectMemoryAutomaticFactReceiptV1>>> + Send;

    #[allow(clippy::too_many_arguments)]
    fn list_project_memory_automatic_fact_receipts(
        &self,
        owner: FactOwnerV1,
        state: Option<ProjectMemoryAutomaticFactStateV1>,
        after_apply_id: Option<ProvenanceId>,
        limit: usize,
        read_control: &FactReadControl,
    ) -> impl Future<Output = FactStoreResult<ProjectMemoryAutomaticFactReceiptPageV1>> + Send;

    /// Reads the immutable receipt material committed for one exact
    /// owner-bound automation run. Implementations must reject overflow or
    /// ambiguous curation receipts rather than truncate or choose one.
    fn project_memory_automation_run_receipts(
        &self,
        owner: FactOwnerV1,
        run_id: RunId,
        read_control: &FactReadControl,
    ) -> impl Future<Output = FactStoreResult<ProjectMemoryAutomationRunReceiptsV1>> + Send;
}
