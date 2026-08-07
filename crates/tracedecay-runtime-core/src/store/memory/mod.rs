//! Database-backed authority for append-only facts, evidence, and provenance.

use std::sync::Arc;

use crate::db::Database;

use tracedecay_domain::{
    ActorId, FactId, FactLineageEventV1, FactOwnerV1, ProvenanceId, RetrievalAnchorRecordV2,
};
use tracedecay_store::{
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
    ProjectMemoryFactHistoryV1, ProjectMemoryFactInspectionV1, ProjectMemoryFactListQueryV1,
    ProjectMemoryFactMergeCommandV1, ProjectMemoryFactMergeOutcomeV1, ProjectMemoryFactPageV1,
    ProjectMemoryFactProjectionV1, ProjectMemoryFactProposalImportReceiptV1,
    ProjectMemoryFactProposalImportV1, ProjectMemoryFactProposalPageV1,
    ProjectMemoryFactProposalPromotionResultV1, ProjectMemoryFactProposalPromotionV1,
    ProjectMemoryFactProposalRecordV1, ProjectMemoryFactProposalRevisionV1,
    ProjectMemoryFactProposalStateV1, ProjectMemoryFactRemoveCommandV1,
    ProjectMemoryFactRemoveOutcomeV1, ProjectMemoryFactRetrievalCommandV1,
    ProjectMemoryFactSearchPageV1, ProjectMemoryFactSearchQuery, ProjectMemoryFactTargetV1,
    ProjectMemoryFactUpdateCommandV1, ProjectMemoryFactUpdateOutcomeV1,
    ProjectMemoryMemoryRepairCommandV1, ProjectMemoryMemoryRepairStatsV1,
    ProjectMemoryMemoryStatusV1, CurrentFactsQuery, FactAsOfQuery, FactAsOfResponseV1,
    FactCommitOutcome, FactCompatibilityResult, FactCompatibilityStore, FactCurrentQuery,
    FactCurrentResponseV1, FactLineageQuery, FactLineageResponseV1, FactProposalStore,
    FactProposalStoreError, FactStore, FactStoreResult, FactWriteBatch, LegacyFactQuery,
    PromoteFactProposal, PromoteFactProposalOutcome, RetrievalAnchorQuery, StoredFactV1,
};

use crud::{
    PROMOTE_OPERATION, add_compatibility_fact_tx, compatibility_fact_feedback_history_tx,
    compatibility_fact_history_tx, fact_response_metadata_tx,
    find_compatibility_fact_by_content_digest_tx, get_compatibility_fact_tx,
    get_retrieval_anchor_tx, inspect_compatibility_fact_tx, list_compatibility_facts_tx,
    promote_compatibility_fact_proposal_tx,
    promote_compatibility_fact_proposal_with_disposition_tx, promote_fact_proposal_tx,
    query_current_facts_tx, query_fact_as_of_response_tx, query_fact_as_of_tx,
    query_fact_current_response_tx, query_fact_current_tx, query_fact_lineage_response_tx,
    query_fact_lineage_tx, record_compatibility_fact_feedback_tx, remove_compatibility_fact_tx,
    update_compatibility_fact_tx,
};
use curation::{apply_compatibility_fact_curation_tx, merge_compatibility_facts_tx};
use dashboard::{
    dashboard_compatibility_fact_detail_tx, dashboard_compatibility_memory_oplog_tx,
    dashboard_compatibility_memory_overview_tx, dashboard_compatibility_vector_points_tx,
};
use envelope::finish_read_snapshot;
use primitives::{QUERY_OPERATION, authority_storage_error, storage_error};
use projection::resolve_legacy_fact_tx;
use proposals::{
    count_pending_compatibility_fact_proposals_tx, get_compatibility_fact_proposal_tx,
    import_legacy_compatibility_fact_proposals_tx, list_compatibility_fact_proposals_tx,
    reject_compatibility_fact_proposal_tx, submit_compatibility_fact_proposal_tx,
};
use repair::{compatibility_feedback_history_repair_progress_tx, repair_compatibility_memory_tx};
use search::{
    find_compatibility_contradictions_tx, probe_compatibility_facts_tx,
    reason_compatibility_facts_tx, record_compatibility_fact_retrieval_tx,
    related_compatibility_facts_tx, search_compatibility_facts_tx,
};
use status::compatibility_memory_status_tx;

mod crud;
mod curation;
mod dashboard;
mod envelope;
mod primitives;
mod projection;
mod proposals;
mod repair;
mod runtime;
mod scoring;
mod search;
mod status;

#[cfg(test)]
use crate::db::engine::params;
#[cfg(test)]
use primitives::OwnerKey;

/// Canonical fact authority over one already-open, authority-bound database.
///
/// This adapter never resolves a path or opens a database. All write and read
/// transactions are delegated to the retained [`Database`] authority.
pub struct DatabaseFactStore<'a> {
    db: &'a Database,
    write_control: Option<FactWriteControl>,
}

/// Transport-neutral arbitration for one externally controlled fact write.
#[derive(Clone)]
pub struct FactWriteControl {
    interrupted: Arc<dyn Fn() -> bool + Send + Sync>,
    try_begin_commit: Arc<dyn Fn() -> bool + Send + Sync>,
}

impl FactWriteControl {
    pub fn new(
        interrupted: Arc<dyn Fn() -> bool + Send + Sync>,
        try_begin_commit: Arc<dyn Fn() -> bool + Send + Sync>,
    ) -> Self {
        Self {
            interrupted,
            try_begin_commit,
        }
    }

    fn interrupted(&self) -> bool {
        (self.interrupted)()
    }

    fn try_begin_commit(&self) -> bool {
        (self.try_begin_commit)()
    }
}

impl<'a> DatabaseFactStore<'a> {
    pub const fn new(db: &'a Database) -> Self {
        Self {
            db,
            write_control: None,
        }
    }

    pub fn new_controlled(db: &'a Database, write_control: FactWriteControl) -> Self {
        Self {
            db,
            write_control: Some(write_control),
        }
    }
}

impl FactStore for DatabaseFactStore<'_> {
    async fn commit_fact(&self, batch: FactWriteBatch) -> FactStoreResult<FactCommitOutcome> {
        match runtime::retained_fact_runtime(self.db)? {
            Some(runtime) => {
                runtime::commit_fact(self.db, runtime, batch, self.write_control.clone()).await
            }
            None => self.commit_batch(&batch).await,
        }
    }

    async fn query_current_facts(
        &self,
        query: CurrentFactsQuery,
    ) -> FactStoreResult<Vec<StoredFactV1>> {
        let snapshot = self
            .db
            .begin_memory_read_transaction(QUERY_OPERATION)
            .await
            .map_err(|error| storage_error(QUERY_OPERATION, error))?;
        let result = query_current_facts_tx(&snapshot, &query).await;
        finish_read_snapshot(snapshot, result).await
    }

    async fn query_fact_current(
        &self,
        query: FactCurrentQuery,
    ) -> FactStoreResult<Option<StoredFactV1>> {
        if let Some(runtime) = runtime::retained_fact_runtime(self.db)? {
            return runtime::query_fact_current(runtime, query);
        }
        let snapshot = self
            .db
            .begin_memory_read_transaction(QUERY_OPERATION)
            .await
            .map_err(|error| storage_error(QUERY_OPERATION, error))?;
        let result = query_fact_current_tx(&snapshot, query.owner(), query.fact_id()).await;
        finish_read_snapshot(snapshot, result).await
    }

    async fn query_fact_current_response(
        &self,
        query: FactCurrentQuery,
    ) -> FactStoreResult<FactCurrentResponseV1> {
        if let Some(runtime) = runtime::retained_fact_runtime(self.db)? {
            // The runtime read port answers the fact itself. It admits no
            // response-shaped operation, so coverage and contradiction are
            // measured from the retained authority the runtime is mounted on —
            // `validate_mount` proves it is the identical SQLite file — instead
            // of being reported as constants that no read ever observed.
            let fact = runtime::query_fact_current(runtime, query.clone())?;
            let snapshot = self
                .db
                .begin_memory_read_transaction(QUERY_OPERATION)
                .await
                .map_err(|error| storage_error(QUERY_OPERATION, error))?;
            let metadata =
                fact_response_metadata_tx(&snapshot, query.owner(), query.fact_id(), fact.as_ref())
                    .await;
            let (coverage, contradiction) = finish_read_snapshot(snapshot, metadata).await?;
            return Ok(FactCurrentResponseV1::new(fact, coverage, contradiction));
        }
        let snapshot = self
            .db
            .begin_memory_read_transaction(QUERY_OPERATION)
            .await
            .map_err(|error| storage_error(QUERY_OPERATION, error))?;
        let result = query_fact_current_response_tx(&snapshot, &query).await;
        finish_read_snapshot(snapshot, result).await
    }

    async fn query_fact_as_of(
        &self,
        query: FactAsOfQuery,
    ) -> FactStoreResult<Option<StoredFactV1>> {
        let snapshot = self
            .db
            .begin_memory_read_transaction(QUERY_OPERATION)
            .await
            .map_err(|error| storage_error(QUERY_OPERATION, error))?;
        let result = query_fact_as_of_tx(&snapshot, &query).await;
        finish_read_snapshot(snapshot, result).await
    }

    async fn query_fact_as_of_response(
        &self,
        query: FactAsOfQuery,
    ) -> FactStoreResult<FactAsOfResponseV1> {
        let snapshot = self
            .db
            .begin_memory_read_transaction(QUERY_OPERATION)
            .await
            .map_err(|error| storage_error(QUERY_OPERATION, error))?;
        let result = query_fact_as_of_response_tx(&snapshot, &query).await;
        finish_read_snapshot(snapshot, result).await
    }

    async fn query_fact_lineage(
        &self,
        query: FactLineageQuery,
    ) -> FactStoreResult<Vec<FactLineageEventV1>> {
        if let Some(runtime) = runtime::retained_fact_runtime(self.db)? {
            return runtime::query_fact_lineage(runtime, query);
        }
        let snapshot = self
            .db
            .begin_memory_read_transaction(QUERY_OPERATION)
            .await
            .map_err(|error| storage_error(QUERY_OPERATION, error))?;
        let result = query_fact_lineage_tx(&snapshot, &query).await;
        finish_read_snapshot(snapshot, result).await
    }

    async fn query_fact_lineage_response(
        &self,
        query: FactLineageQuery,
    ) -> FactStoreResult<FactLineageResponseV1> {
        if let Some(runtime) = runtime::retained_fact_runtime(self.db)? {
            // As in `query_fact_current_response`: the runtime answers the
            // lineage page, and the accompanying coverage and contradiction are
            // measured from the retained authority rather than fabricated.
            let events = runtime::query_fact_lineage(runtime, query.clone())?;
            let snapshot = self
                .db
                .begin_memory_read_transaction(QUERY_OPERATION)
                .await
                .map_err(|error| storage_error(QUERY_OPERATION, error))?;
            let metadata = async {
                let current =
                    query_fact_current_tx(&snapshot, query.owner(), query.fact_id()).await?;
                fact_response_metadata_tx(
                    &snapshot,
                    query.owner(),
                    query.fact_id(),
                    current.as_ref(),
                )
                .await
            }
            .await;
            let (coverage, contradiction) = finish_read_snapshot(snapshot, metadata).await?;
            return Ok(FactLineageResponseV1::new(events, coverage, contradiction));
        }
        let snapshot = self
            .db
            .begin_memory_read_transaction(QUERY_OPERATION)
            .await
            .map_err(|error| storage_error(QUERY_OPERATION, error))?;
        let result = query_fact_lineage_response_tx(&snapshot, &query).await;
        finish_read_snapshot(snapshot, result).await
    }

    async fn resolve_legacy_fact(&self, query: LegacyFactQuery) -> FactStoreResult<Option<FactId>> {
        let snapshot = self
            .db
            .begin_memory_read_transaction(QUERY_OPERATION)
            .await
            .map_err(|error| storage_error(QUERY_OPERATION, error))?;
        let result = resolve_legacy_fact_tx(&snapshot, &query).await;
        finish_read_snapshot(snapshot, result).await
    }

    async fn get_retrieval_anchor(
        &self,
        query: RetrievalAnchorQuery,
    ) -> FactStoreResult<Option<RetrievalAnchorRecordV2>> {
        let snapshot = self
            .db
            .begin_memory_read_transaction(QUERY_OPERATION)
            .await
            .map_err(|error| storage_error(QUERY_OPERATION, error))?;
        let result = get_retrieval_anchor_tx(&snapshot, &query).await;
        finish_read_snapshot(snapshot, result).await
    }
}

impl FactProposalStore for DatabaseFactStore<'_> {
    async fn promote_fact_proposal(
        &self,
        promotion: PromoteFactProposal,
    ) -> Result<PromoteFactProposalOutcome, FactProposalStoreError> {
        let transaction = self
            .db
            .begin_memory_write_transaction(PROMOTE_OPERATION)
            .await
            .map_err(|error| authority_storage_error(PROMOTE_OPERATION, error))?;
        let outcome = match promote_fact_proposal_tx(&transaction, &promotion).await {
            Ok(outcome) => outcome,
            Err(error) => {
                return match transaction.rollback().await {
                    Ok(()) => Err(error),
                    Err(rollback) => Err(authority_storage_error(
                        PROMOTE_OPERATION,
                        std::io::Error::other(format!(
                            "{error}; transaction rollback also failed: {rollback}"
                        )),
                    )),
                };
            }
        };
        if outcome.wrote {
            transaction
                .commit()
                .await
                .map_err(|error| authority_storage_error(PROMOTE_OPERATION, error))?;
        } else {
            transaction
                .rollback()
                .await
                .map_err(|error| authority_storage_error(PROMOTE_OPERATION, error))?;
        }
        Ok(outcome.outcome)
    }
}

impl FactCompatibilityStore for DatabaseFactStore<'_> {
    async fn list_compatibility_facts(
        &self,
        query: ProjectMemoryFactListQueryV1,
    ) -> FactCompatibilityResult<ProjectMemoryFactPageV1> {
        self.compatibility_read(move |transaction| {
            Box::pin(async move { list_compatibility_facts_tx(transaction, &query).await })
        })
        .await
    }

    async fn search_compatibility_facts(
        &self,
        query: ProjectMemoryFactSearchQuery,
    ) -> FactCompatibilityResult<ProjectMemoryFactSearchPageV1> {
        self.compatibility_read(move |transaction| {
            Box::pin(async move { search_compatibility_facts_tx(transaction, &query).await })
        })
        .await
    }

    async fn probe_compatibility_facts(
        &self,
        query: ProjectMemoryFactSearchQuery,
    ) -> FactCompatibilityResult<ProjectMemoryFactSearchPageV1> {
        self.compatibility_read(move |transaction| {
            Box::pin(async move { probe_compatibility_facts_tx(transaction, &query).await })
        })
        .await
    }

    async fn related_compatibility_facts(
        &self,
        query: ProjectMemoryFactSearchQuery,
    ) -> FactCompatibilityResult<ProjectMemoryFactSearchPageV1> {
        self.compatibility_read(move |transaction| {
            Box::pin(async move { related_compatibility_facts_tx(transaction, &query).await })
        })
        .await
    }

    async fn reason_compatibility_facts(
        &self,
        query: ProjectMemoryFactSearchQuery,
    ) -> FactCompatibilityResult<ProjectMemoryFactSearchPageV1> {
        self.compatibility_read(move |transaction| {
            Box::pin(async move { reason_compatibility_facts_tx(transaction, &query).await })
        })
        .await
    }

    async fn find_compatibility_contradictions(
        &self,
        query: ProjectMemoryFactContradictionQueryV1,
    ) -> FactCompatibilityResult<ProjectMemoryFactContradictionPageV1> {
        self.compatibility_read(move |transaction| {
            Box::pin(async move { find_compatibility_contradictions_tx(transaction, &query).await })
        })
        .await
    }

    async fn get_compatibility_fact(
        &self,
        target: ProjectMemoryFactTargetV1,
    ) -> FactCompatibilityResult<Option<ProjectMemoryFactProjectionV1>> {
        self.compatibility_read(move |transaction| {
            Box::pin(async move { get_compatibility_fact_tx(transaction, &target).await })
        })
        .await
    }

    async fn compatibility_fact_history(
        &self,
        query: ProjectMemoryFactHistoryQueryV1,
    ) -> FactCompatibilityResult<ProjectMemoryFactHistoryV1> {
        self.compatibility_read(move |transaction| {
            Box::pin(async move { compatibility_fact_history_tx(transaction, &query).await })
        })
        .await
    }

    async fn compatibility_memory_status(
        &self,
        owner: FactOwnerV1,
    ) -> FactCompatibilityResult<ProjectMemoryMemoryStatusV1> {
        self.compatibility_read(move |transaction| {
            Box::pin(async move {
                let feedback_repair =
                    compatibility_feedback_history_repair_progress_tx(transaction, &owner).await?;
                compatibility_memory_status_tx(transaction, &owner, feedback_repair).await
            })
        })
        .await
    }

    async fn inspect_compatibility_fact(
        &self,
        target: ProjectMemoryFactTargetV1,
    ) -> FactCompatibilityResult<Option<ProjectMemoryFactInspectionV1>> {
        self.compatibility_read(move |transaction| {
            Box::pin(async move { inspect_compatibility_fact_tx(transaction, &target).await })
        })
        .await
    }

    async fn add_compatibility_fact(
        &self,
        request: ProjectMemoryFactAddCommandV1,
    ) -> FactCompatibilityResult<ProjectMemoryFactAddOutcomeV1> {
        let db = self.db.clone();
        self.compatibility_write(move |transaction| {
            Box::pin(async move { add_compatibility_fact_tx(&db, transaction, &request).await })
        })
        .await
    }

    async fn update_compatibility_fact(
        &self,
        request: ProjectMemoryFactUpdateCommandV1,
    ) -> FactCompatibilityResult<ProjectMemoryFactUpdateOutcomeV1> {
        let db = self.db.clone();
        self.compatibility_write(move |transaction| {
            Box::pin(async move { update_compatibility_fact_tx(&db, transaction, &request).await })
        })
        .await
    }

    async fn remove_compatibility_fact(
        &self,
        request: ProjectMemoryFactRemoveCommandV1,
    ) -> FactCompatibilityResult<ProjectMemoryFactRemoveOutcomeV1> {
        let db = self.db.clone();
        self.compatibility_write(move |transaction| {
            Box::pin(async move { remove_compatibility_fact_tx(&db, transaction, &request).await })
        })
        .await
    }

    async fn record_compatibility_fact_feedback(
        &self,
        request: ProjectMemoryFactFeedbackCommandV1,
    ) -> FactCompatibilityResult<ProjectMemoryFactFeedbackOutcomeV1> {
        self.compatibility_write(move |transaction| {
            Box::pin(
                async move { record_compatibility_fact_feedback_tx(transaction, &request).await },
            )
        })
        .await
    }

    async fn compatibility_fact_feedback_history(
        &self,
        query: ProjectMemoryFactFeedbackHistoryQueryV1,
    ) -> FactCompatibilityResult<ProjectMemoryFactFeedbackHistoryV1> {
        self.compatibility_read(move |transaction| {
            Box::pin(async move {
                let feedback_repair = compatibility_feedback_history_repair_progress_tx(
                    transaction,
                    query.target().owner(),
                )
                .await?;
                compatibility_fact_feedback_history_tx(transaction, &query, feedback_repair).await
            })
        })
        .await
    }

    async fn find_compatibility_fact_by_content_digest(
        &self,
        query: ProjectMemoryFactContentDigestQueryV1,
    ) -> FactCompatibilityResult<Option<ProjectMemoryFactProjectionV1>> {
        self.compatibility_read(move |transaction| {
            Box::pin(async move {
                find_compatibility_fact_by_content_digest_tx(transaction, &query).await
            })
        })
        .await
    }

    async fn apply_compatibility_fact_curation(
        &self,
        request: ProjectMemoryFactCurationBatchV1,
    ) -> FactCompatibilityResult<ProjectMemoryFactCurationReceiptV1> {
        let db = self.db.clone();
        self.compatibility_write(move |transaction| {
            Box::pin(async move {
                apply_compatibility_fact_curation_tx(&db, transaction, &request).await
            })
        })
        .await
    }

    async fn merge_compatibility_facts(
        &self,
        request: ProjectMemoryFactMergeCommandV1,
    ) -> FactCompatibilityResult<ProjectMemoryFactMergeOutcomeV1> {
        let db = self.db.clone();
        self.compatibility_write(move |transaction| {
            Box::pin(async move { merge_compatibility_facts_tx(&db, transaction, &request).await })
        })
        .await
    }

    async fn repair_compatibility_memory(
        &self,
        request: ProjectMemoryMemoryRepairCommandV1,
    ) -> FactCompatibilityResult<ProjectMemoryMemoryRepairStatsV1> {
        let db = self.db.clone();
        self.compatibility_write(move |transaction| {
            Box::pin(
                async move { repair_compatibility_memory_tx(&db, transaction, &request).await },
            )
        })
        .await
    }

    async fn dashboard_compatibility_memory_overview(
        &self,
        query: ProjectMemoryDashboardMemoryOverviewQueryV1,
    ) -> FactCompatibilityResult<ProjectMemoryDashboardMemoryOverviewV1> {
        self.compatibility_read(move |transaction| {
            Box::pin(async move {
                dashboard_compatibility_memory_overview_tx(transaction, &query).await
            })
        })
        .await
    }

    async fn dashboard_compatibility_fact_detail(
        &self,
        query: ProjectMemoryDashboardFactDetailQueryV1,
    ) -> FactCompatibilityResult<Option<ProjectMemoryDashboardFactDetailV1>> {
        self.compatibility_read(move |transaction| {
            Box::pin(
                async move { dashboard_compatibility_fact_detail_tx(transaction, &query).await },
            )
        })
        .await
    }

    async fn dashboard_compatibility_vector_points(
        &self,
        query: ProjectMemoryDashboardVectorPointsQueryV1,
    ) -> FactCompatibilityResult<Vec<ProjectMemoryDashboardVectorPointV1>> {
        self.compatibility_read(move |transaction| {
            Box::pin(
                async move { dashboard_compatibility_vector_points_tx(transaction, &query).await },
            )
        })
        .await
    }

    async fn dashboard_compatibility_memory_oplog(
        &self,
        query: ProjectMemoryDashboardOplogQueryV1,
    ) -> FactCompatibilityResult<Vec<ProjectMemoryDashboardOplogEntryV1>> {
        self.compatibility_read(move |transaction| {
            Box::pin(
                async move { dashboard_compatibility_memory_oplog_tx(transaction, &query).await },
            )
        })
        .await
    }

    async fn record_compatibility_fact_retrieval(
        &self,
        request: ProjectMemoryFactRetrievalCommandV1,
    ) -> FactCompatibilityResult<Vec<ProjectMemoryFactProjectionV1>> {
        self.compatibility_write(move |transaction| {
            Box::pin(
                async move { record_compatibility_fact_retrieval_tx(transaction, &request).await },
            )
        })
        .await
    }

    async fn submit_compatibility_fact_proposal(
        &self,
        proposal_id: ProvenanceId,
        request: ProjectMemoryFactAddCommandV1,
        submitter: Option<ActorId>,
    ) -> FactCompatibilityResult<ProjectMemoryFactProposalRecordV1> {
        self.compatibility_write(move |transaction| {
            Box::pin(async move {
                submit_compatibility_fact_proposal_tx(
                    transaction,
                    proposal_id,
                    &request,
                    submitter.as_ref(),
                )
                .await
            })
        })
        .await
    }

    async fn get_compatibility_fact_proposal(
        &self,
        owner: FactOwnerV1,
        proposal_id: ProvenanceId,
    ) -> FactCompatibilityResult<Option<ProjectMemoryFactProposalRecordV1>> {
        self.compatibility_read(move |transaction| {
            Box::pin(async move {
                get_compatibility_fact_proposal_tx(transaction, &owner, &proposal_id).await
            })
        })
        .await
    }

    async fn list_compatibility_fact_proposals(
        &self,
        owner: FactOwnerV1,
        state: Option<ProjectMemoryFactProposalStateV1>,
        after_proposal_id: Option<ProvenanceId>,
        limit: usize,
    ) -> FactCompatibilityResult<ProjectMemoryFactProposalPageV1> {
        self.compatibility_read(move |transaction| {
            Box::pin(async move {
                list_compatibility_fact_proposals_tx(
                    transaction,
                    &owner,
                    state,
                    after_proposal_id.as_ref(),
                    limit,
                )
                .await
            })
        })
        .await
    }

    async fn count_pending_compatibility_fact_proposals(
        &self,
        owner: FactOwnerV1,
    ) -> FactCompatibilityResult<u64> {
        self.compatibility_read(move |transaction| {
            Box::pin(async move {
                count_pending_compatibility_fact_proposals_tx(transaction, &owner).await
            })
        })
        .await
    }

    async fn reject_compatibility_fact_proposal(
        &self,
        owner: FactOwnerV1,
        proposal_id: ProvenanceId,
        expected_revision: ProjectMemoryFactProposalRevisionV1,
        reviewer: ActorId,
        reason: String,
    ) -> FactCompatibilityResult<ProjectMemoryFactProposalRecordV1> {
        self.compatibility_write(move |transaction| {
            Box::pin(async move {
                reject_compatibility_fact_proposal_tx(
                    transaction,
                    &owner,
                    &proposal_id,
                    expected_revision,
                    &reviewer,
                    &reason,
                )
                .await
            })
        })
        .await
    }

    async fn import_legacy_compatibility_fact_proposals(
        &self,
        request: ProjectMemoryFactProposalImportV1,
    ) -> FactCompatibilityResult<ProjectMemoryFactProposalImportReceiptV1> {
        self.compatibility_write(move |transaction| {
            Box::pin(async move {
                import_legacy_compatibility_fact_proposals_tx(transaction, &request).await
            })
        })
        .await
    }

    async fn promote_compatibility_fact_proposal(
        &self,
        request: ProjectMemoryFactProposalPromotionV1,
    ) -> FactCompatibilityResult<ProjectMemoryFactProposalRecordV1> {
        let db = self.db.clone();
        self.compatibility_write(move |transaction| {
            Box::pin(async move {
                promote_compatibility_fact_proposal_tx(&db, transaction, &request).await
            })
        })
        .await
    }

    async fn promote_compatibility_fact_proposal_with_disposition(
        &self,
        request: ProjectMemoryFactProposalPromotionV1,
    ) -> FactCompatibilityResult<ProjectMemoryFactProposalPromotionResultV1> {
        let db = self.db.clone();
        self.compatibility_write(move |transaction| {
            Box::pin(async move {
                promote_compatibility_fact_proposal_with_disposition_tx(&db, transaction, &request)
                    .await
            })
        })
        .await
    }
}

/// The single owned-or-borrowed handle shape for the shared project-memory
/// database. Every project-memory route — the core fact-store accessors in
/// [`crate::tracedecay::facts`] and the MCP memory handlers alike — resolves
/// through this one type and its `db_path() == graph_db_path` routing
/// predicate, instead of each maintaining its own near-duplicate enum kept in
/// sync only by hand.
pub enum ProjectMemoryDbHandle<'a> {
    /// The database this instance already serves, when it already is the
    /// shared project store rather than a branch shard.
    Active(&'a Database),
    /// A separately opened handle to the shared project store, owned by the
    /// resolution because the active database is a branch shard.
    Owned(Box<Database>),
}

impl<'a> ProjectMemoryDbHandle<'a> {
    /// Borrows the resolved database regardless of ownership.
    pub fn as_db(&self) -> &Database {
        match self {
            Self::Active(db) => db,
            Self::Owned(db) => db.as_ref(),
        }
    }

    /// Consumes the resolved handle into a fact store that owns it, so a
    /// single accessor can build a memory application whose authority
    /// outlives the resolving call.
    pub fn into_fact_store(self) -> ProjectFactStore<'a> {
        ProjectFactStore { db: self }
    }
}

/// Canonical fact authority that *owns* its resolved project-memory database.
///
/// Project-memory routes resolve the shared project store into either the
/// active database or a separately opened handle. Borrowing that resolution
/// into a [`DatabaseFactStore`] cannot outlive the resolving call, which forced
/// every route to re-resolve the owner and database inline. This adapter owns
/// the resolved handle so one accessor can build the whole memory application,
/// delegating each fact-store operation to a borrowed [`DatabaseFactStore`].
pub struct ProjectFactStore<'a> {
    db: ProjectMemoryDbHandle<'a>,
}

impl<'a> ProjectFactStore<'a> {
    /// Wraps the active database without taking ownership.
    pub const fn borrowed(db: &'a Database) -> Self {
        Self {
            db: ProjectMemoryDbHandle::Active(db),
        }
    }

    /// Takes ownership of a separately opened project-store handle.
    pub const fn owned(db: Box<Database>) -> Self {
        Self {
            db: ProjectMemoryDbHandle::Owned(db),
        }
    }

    fn store(&self) -> DatabaseFactStore<'_> {
        DatabaseFactStore::new(self.db.as_db())
    }
}

/// Delegates each fact-store trait method to the borrowed [`DatabaseFactStore`].
macro_rules! delegate_fact_store_methods {
    ( $( fn $name:ident ( $( $arg:ident : $ty:ty ),* $(,)? ) -> $ret:ty; )+ ) => {
        $(
            async fn $name(&self, $( $arg : $ty ),* ) -> $ret {
                self.store().$name( $( $arg ),* ).await
            }
        )+
    };
}

impl FactStore for ProjectFactStore<'_> {
    delegate_fact_store_methods! {
        fn commit_fact(batch: FactWriteBatch) -> FactStoreResult<FactCommitOutcome>;
        fn query_current_facts(query: CurrentFactsQuery) -> FactStoreResult<Vec<StoredFactV1>>;
        fn query_fact_current(query: FactCurrentQuery) -> FactStoreResult<Option<StoredFactV1>>;
        fn query_fact_current_response(
            query: FactCurrentQuery,
        ) -> FactStoreResult<FactCurrentResponseV1>;
        fn query_fact_as_of(query: FactAsOfQuery) -> FactStoreResult<Option<StoredFactV1>>;
        fn query_fact_as_of_response(query: FactAsOfQuery) -> FactStoreResult<FactAsOfResponseV1>;
        fn query_fact_lineage(query: FactLineageQuery) -> FactStoreResult<Vec<FactLineageEventV1>>;
        fn query_fact_lineage_response(
            query: FactLineageQuery,
        ) -> FactStoreResult<FactLineageResponseV1>;
        fn resolve_legacy_fact(query: LegacyFactQuery) -> FactStoreResult<Option<FactId>>;
        fn get_retrieval_anchor(
            query: RetrievalAnchorQuery,
        ) -> FactStoreResult<Option<RetrievalAnchorRecordV2>>;
    }
}

impl FactProposalStore for ProjectFactStore<'_> {
    delegate_fact_store_methods! {
        fn promote_fact_proposal(
            promotion: PromoteFactProposal,
        ) -> Result<PromoteFactProposalOutcome, FactProposalStoreError>;
    }
}

impl FactCompatibilityStore for ProjectFactStore<'_> {
    delegate_fact_store_methods! {
        fn list_compatibility_facts(
            query: ProjectMemoryFactListQueryV1,
        ) -> FactCompatibilityResult<ProjectMemoryFactPageV1>;
        fn search_compatibility_facts(
            query: ProjectMemoryFactSearchQuery,
        ) -> FactCompatibilityResult<ProjectMemoryFactSearchPageV1>;
        fn probe_compatibility_facts(
            query: ProjectMemoryFactSearchQuery,
        ) -> FactCompatibilityResult<ProjectMemoryFactSearchPageV1>;
        fn related_compatibility_facts(
            query: ProjectMemoryFactSearchQuery,
        ) -> FactCompatibilityResult<ProjectMemoryFactSearchPageV1>;
        fn reason_compatibility_facts(
            query: ProjectMemoryFactSearchQuery,
        ) -> FactCompatibilityResult<ProjectMemoryFactSearchPageV1>;
        fn find_compatibility_contradictions(
            query: ProjectMemoryFactContradictionQueryV1,
        ) -> FactCompatibilityResult<ProjectMemoryFactContradictionPageV1>;
        fn get_compatibility_fact(
            target: ProjectMemoryFactTargetV1,
        ) -> FactCompatibilityResult<Option<ProjectMemoryFactProjectionV1>>;
        fn compatibility_fact_history(
            query: ProjectMemoryFactHistoryQueryV1,
        ) -> FactCompatibilityResult<ProjectMemoryFactHistoryV1>;
        fn compatibility_memory_status(
            owner: FactOwnerV1,
        ) -> FactCompatibilityResult<ProjectMemoryMemoryStatusV1>;
        fn inspect_compatibility_fact(
            target: ProjectMemoryFactTargetV1,
        ) -> FactCompatibilityResult<Option<ProjectMemoryFactInspectionV1>>;
        fn add_compatibility_fact(
            request: ProjectMemoryFactAddCommandV1,
        ) -> FactCompatibilityResult<ProjectMemoryFactAddOutcomeV1>;
        fn update_compatibility_fact(
            request: ProjectMemoryFactUpdateCommandV1,
        ) -> FactCompatibilityResult<ProjectMemoryFactUpdateOutcomeV1>;
        fn remove_compatibility_fact(
            request: ProjectMemoryFactRemoveCommandV1,
        ) -> FactCompatibilityResult<ProjectMemoryFactRemoveOutcomeV1>;
        fn record_compatibility_fact_feedback(
            request: ProjectMemoryFactFeedbackCommandV1,
        ) -> FactCompatibilityResult<ProjectMemoryFactFeedbackOutcomeV1>;
        fn compatibility_fact_feedback_history(
            query: ProjectMemoryFactFeedbackHistoryQueryV1,
        ) -> FactCompatibilityResult<ProjectMemoryFactFeedbackHistoryV1>;
        fn find_compatibility_fact_by_content_digest(
            query: ProjectMemoryFactContentDigestQueryV1,
        ) -> FactCompatibilityResult<Option<ProjectMemoryFactProjectionV1>>;
        fn apply_compatibility_fact_curation(
            request: ProjectMemoryFactCurationBatchV1,
        ) -> FactCompatibilityResult<ProjectMemoryFactCurationReceiptV1>;
        fn merge_compatibility_facts(
            request: ProjectMemoryFactMergeCommandV1,
        ) -> FactCompatibilityResult<ProjectMemoryFactMergeOutcomeV1>;
        fn repair_compatibility_memory(
            request: ProjectMemoryMemoryRepairCommandV1,
        ) -> FactCompatibilityResult<ProjectMemoryMemoryRepairStatsV1>;
        fn dashboard_compatibility_memory_overview(
            query: ProjectMemoryDashboardMemoryOverviewQueryV1,
        ) -> FactCompatibilityResult<ProjectMemoryDashboardMemoryOverviewV1>;
        fn dashboard_compatibility_fact_detail(
            query: ProjectMemoryDashboardFactDetailQueryV1,
        ) -> FactCompatibilityResult<Option<ProjectMemoryDashboardFactDetailV1>>;
        fn dashboard_compatibility_vector_points(
            query: ProjectMemoryDashboardVectorPointsQueryV1,
        ) -> FactCompatibilityResult<Vec<ProjectMemoryDashboardVectorPointV1>>;
        fn dashboard_compatibility_memory_oplog(
            query: ProjectMemoryDashboardOplogQueryV1,
        ) -> FactCompatibilityResult<Vec<ProjectMemoryDashboardOplogEntryV1>>;
        fn record_compatibility_fact_retrieval(
            request: ProjectMemoryFactRetrievalCommandV1,
        ) -> FactCompatibilityResult<Vec<ProjectMemoryFactProjectionV1>>;
        fn submit_compatibility_fact_proposal(
            proposal_id: ProvenanceId,
            request: ProjectMemoryFactAddCommandV1,
            submitter: Option<ActorId>,
        ) -> FactCompatibilityResult<ProjectMemoryFactProposalRecordV1>;
        fn get_compatibility_fact_proposal(
            owner: FactOwnerV1,
            proposal_id: ProvenanceId,
        ) -> FactCompatibilityResult<Option<ProjectMemoryFactProposalRecordV1>>;
        fn list_compatibility_fact_proposals(
            owner: FactOwnerV1,
            state: Option<ProjectMemoryFactProposalStateV1>,
            after_proposal_id: Option<ProvenanceId>,
            limit: usize,
        ) -> FactCompatibilityResult<ProjectMemoryFactProposalPageV1>;
        fn count_pending_compatibility_fact_proposals(
            owner: FactOwnerV1,
        ) -> FactCompatibilityResult<u64>;
        fn reject_compatibility_fact_proposal(
            owner: FactOwnerV1,
            proposal_id: ProvenanceId,
            expected_revision: ProjectMemoryFactProposalRevisionV1,
            reviewer: ActorId,
            reason: String,
        ) -> FactCompatibilityResult<ProjectMemoryFactProposalRecordV1>;
        fn import_legacy_compatibility_fact_proposals(
            request: ProjectMemoryFactProposalImportV1,
        ) -> FactCompatibilityResult<ProjectMemoryFactProposalImportReceiptV1>;
        fn promote_compatibility_fact_proposal(
            request: ProjectMemoryFactProposalPromotionV1,
        ) -> FactCompatibilityResult<ProjectMemoryFactProposalRecordV1>;
        fn promote_compatibility_fact_proposal_with_disposition(
            request: ProjectMemoryFactProposalPromotionV1,
        ) -> FactCompatibilityResult<ProjectMemoryFactProposalPromotionResultV1>;
    }
}

#[cfg(test)]
#[path = "fact_response_metadata_test.rs"]
mod fact_response_metadata_test;
