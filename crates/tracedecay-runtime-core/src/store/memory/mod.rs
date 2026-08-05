//! Database-backed authority for append-only facts, evidence, and provenance.

use crate::db::Database;

use tracedecay_domain::{
    ActorId, FactId, FactLineageEventV1, FactOwnerV1, ProvenanceId, RetrievalAnchorRecordV2,
};
use tracedecay_store::{
    CurrentFactsQuery, DashboardFactDetail, DashboardFactDetailQuery, DashboardMemoryOverview,
    DashboardMemoryOverviewQuery, DashboardOplogEntry, DashboardOplogQuery, DashboardVectorPoint,
    DashboardVectorPointsQuery, FactAddCommand, FactAddOutcome, FactAsOfQuery, FactAsOfResponseV1,
    FactCommitOutcome, FactContentDigestQuery, FactContradictionPage, FactContradictionQuery,
    FactCurationBatch, FactCurationReceipt, FactCurrentQuery, FactCurrentResponseV1,
    FactFeedbackCommand, FactFeedbackHistory, FactFeedbackHistoryQuery, FactFeedbackOutcome,
    FactHistory, FactHistoryQuery, FactInspection, FactLineageQuery, FactLineageResponseV1,
    FactLineageResult, FactLineageStore, FactListQuery, FactMergeCommand, FactMergeOutcome,
    FactPage, FactProjection, FactProposalEvidence, FactProposalPage, FactProposalPromotion,
    FactProposalPromotionResult, FactProposalRecord, FactProposalRevision, FactProposalState,
    FactProposalStore, FactProposalStoreError, FactRemoveCommand, FactRemoveOutcome,
    FactRetrievalCommand, FactSearchPage, FactSearchQuery, FactStore, FactStoreResult, FactTarget,
    FactUpdateCommand, FactUpdateOutcome, FactWriteBatch, LegacyFactQuery, MemoryRepairCommand,
    MemoryRepairStats, MemoryStatus, PromoteFactProposal, PromoteFactProposalOutcome,
    RetrievalAnchorQuery, StoredFactV1,
};

use crud::{
    PROMOTE_OPERATION, add_fact_tx, commit_fact_proposal_tx, fact_feedback_history_tx,
    fact_history_tx, fact_response_metadata_tx, find_fact_by_content_digest_tx, get_fact_tx,
    get_retrieval_anchor_tx, inspect_fact_tx, list_facts_tx, promote_fact_proposal_tx,
    promote_fact_proposal_with_disposition_tx, query_current_facts_tx,
    query_fact_as_of_response_tx, query_fact_as_of_tx, query_fact_current_response_tx,
    query_fact_current_tx, query_fact_lineage_response_tx, query_fact_lineage_tx,
    record_fact_feedback_tx, remove_fact_tx, update_fact_tx,
};
use curation::{apply_fact_curation_tx, merge_facts_tx};
use dashboard::{
    dashboard_fact_detail_tx, dashboard_memory_oplog_tx, dashboard_memory_overview_tx,
    dashboard_vector_points_tx,
};
use envelope::finish_read_snapshot;
use primitives::{QUERY_OPERATION, authority_storage_error, storage_error};
use projection::resolve_legacy_fact_tx;
use proposals::{
    count_pending_fact_proposals_tx, get_fact_proposal_tx, list_fact_proposals_tx,
    reject_fact_proposal_tx, submit_fact_proposal_tx,
};
use repair::{feedback_history_repair_progress_tx, repair_memory_tx};
use search::{
    find_contradictions_tx, probe_facts_tx, reason_facts_tx, record_fact_retrieval_tx,
    related_facts_tx, search_facts_tx,
};
use status::memory_status_tx;

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
}

impl<'a> DatabaseFactStore<'a> {
    pub const fn new(db: &'a Database) -> Self {
        Self { db }
    }
}

impl FactLineageStore for DatabaseFactStore<'_> {
    async fn commit_fact(&self, batch: FactWriteBatch) -> FactLineageResult<FactCommitOutcome> {
        match runtime::retained_fact_runtime(self.db)? {
            Some(runtime) => runtime::commit_fact(self.db, runtime, batch).await,
            None => self.commit_batch(&batch).await,
        }
    }

    async fn query_current_facts(
        &self,
        query: CurrentFactsQuery,
    ) -> FactLineageResult<Vec<StoredFactV1>> {
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
    ) -> FactLineageResult<Option<StoredFactV1>> {
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
    ) -> FactLineageResult<FactCurrentResponseV1> {
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
    ) -> FactLineageResult<Option<StoredFactV1>> {
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
    ) -> FactLineageResult<FactAsOfResponseV1> {
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
    ) -> FactLineageResult<Vec<FactLineageEventV1>> {
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
    ) -> FactLineageResult<FactLineageResponseV1> {
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

    async fn resolve_legacy_fact(
        &self,
        query: LegacyFactQuery,
    ) -> FactLineageResult<Option<FactId>> {
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
    ) -> FactLineageResult<Option<RetrievalAnchorRecordV2>> {
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
    async fn commit_fact_proposal(
        &self,
        promotion: PromoteFactProposal,
    ) -> Result<PromoteFactProposalOutcome, FactProposalStoreError> {
        let transaction = self
            .db
            .begin_memory_write_transaction(PROMOTE_OPERATION)
            .await
            .map_err(|error| authority_storage_error(PROMOTE_OPERATION, error))?;
        let outcome = match commit_fact_proposal_tx(&transaction, &promotion).await {
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

impl FactStore for DatabaseFactStore<'_> {
    async fn list_facts(&self, query: FactListQuery) -> FactStoreResult<FactPage> {
        self.read(move |transaction| {
            Box::pin(async move { list_facts_tx(transaction, &query).await })
        })
        .await
    }

    async fn search_facts(&self, query: FactSearchQuery) -> FactStoreResult<FactSearchPage> {
        self.read(move |transaction| {
            Box::pin(async move { search_facts_tx(transaction, &query).await })
        })
        .await
    }

    async fn probe_facts(&self, query: FactSearchQuery) -> FactStoreResult<FactSearchPage> {
        self.read(move |transaction| {
            Box::pin(async move { probe_facts_tx(transaction, &query).await })
        })
        .await
    }

    async fn related_facts(&self, query: FactSearchQuery) -> FactStoreResult<FactSearchPage> {
        self.read(move |transaction| {
            Box::pin(async move { related_facts_tx(transaction, &query).await })
        })
        .await
    }

    async fn reason_facts(&self, query: FactSearchQuery) -> FactStoreResult<FactSearchPage> {
        self.read(move |transaction| {
            Box::pin(async move { reason_facts_tx(transaction, &query).await })
        })
        .await
    }

    async fn find_contradictions(
        &self,
        query: FactContradictionQuery,
    ) -> FactStoreResult<FactContradictionPage> {
        self.read(move |transaction| {
            Box::pin(async move { find_contradictions_tx(transaction, &query).await })
        })
        .await
    }

    async fn get_fact(&self, target: FactTarget) -> FactStoreResult<Option<FactProjection>> {
        self.read(move |transaction| {
            Box::pin(async move { get_fact_tx(transaction, &target).await })
        })
        .await
    }

    async fn fact_history(&self, query: FactHistoryQuery) -> FactStoreResult<FactHistory> {
        self.read(move |transaction| {
            Box::pin(async move { fact_history_tx(transaction, &query).await })
        })
        .await
    }

    async fn memory_status(&self, owner: FactOwnerV1) -> FactStoreResult<MemoryStatus> {
        self.read(move |transaction| {
            Box::pin(async move {
                let feedback_repair =
                    feedback_history_repair_progress_tx(transaction, &owner).await?;
                memory_status_tx(transaction, &owner, feedback_repair).await
            })
        })
        .await
    }

    async fn inspect_fact(&self, target: FactTarget) -> FactStoreResult<Option<FactInspection>> {
        self.read(move |transaction| {
            Box::pin(async move { inspect_fact_tx(transaction, &target).await })
        })
        .await
    }

    async fn add_fact(&self, request: FactAddCommand) -> FactStoreResult<FactAddOutcome> {
        let db = self.db.clone();
        self.write(move |transaction| {
            Box::pin(async move { add_fact_tx(&db, transaction, &request).await })
        })
        .await
    }

    async fn update_fact(&self, request: FactUpdateCommand) -> FactStoreResult<FactUpdateOutcome> {
        let db = self.db.clone();
        self.write(move |transaction| {
            Box::pin(async move { update_fact_tx(&db, transaction, &request).await })
        })
        .await
    }

    async fn remove_fact(&self, request: FactRemoveCommand) -> FactStoreResult<FactRemoveOutcome> {
        let db = self.db.clone();
        self.write(move |transaction| {
            Box::pin(async move { remove_fact_tx(&db, transaction, &request).await })
        })
        .await
    }

    async fn record_fact_feedback(
        &self,
        request: FactFeedbackCommand,
    ) -> FactStoreResult<FactFeedbackOutcome> {
        self.write(move |transaction| {
            Box::pin(async move { record_fact_feedback_tx(transaction, &request).await })
        })
        .await
    }

    async fn fact_feedback_history(
        &self,
        query: FactFeedbackHistoryQuery,
    ) -> FactStoreResult<FactFeedbackHistory> {
        self.read(move |transaction| {
            Box::pin(async move {
                let feedback_repair =
                    feedback_history_repair_progress_tx(transaction, query.target().owner())
                        .await?;
                fact_feedback_history_tx(transaction, &query, feedback_repair).await
            })
        })
        .await
    }

    async fn find_fact_by_content_digest(
        &self,
        query: FactContentDigestQuery,
    ) -> FactStoreResult<Option<FactProjection>> {
        self.read(move |transaction| {
            Box::pin(async move { find_fact_by_content_digest_tx(transaction, &query).await })
        })
        .await
    }

    async fn apply_fact_curation(
        &self,
        request: FactCurationBatch,
    ) -> FactStoreResult<FactCurationReceipt> {
        let db = self.db.clone();
        self.write(move |transaction| {
            Box::pin(async move { apply_fact_curation_tx(&db, transaction, &request).await })
        })
        .await
    }

    async fn merge_facts(&self, request: FactMergeCommand) -> FactStoreResult<FactMergeOutcome> {
        let db = self.db.clone();
        self.write(move |transaction| {
            Box::pin(async move { merge_facts_tx(&db, transaction, &request).await })
        })
        .await
    }

    async fn repair_memory(
        &self,
        request: MemoryRepairCommand,
    ) -> FactStoreResult<MemoryRepairStats> {
        let db = self.db.clone();
        self.write(move |transaction| {
            Box::pin(async move { repair_memory_tx(&db, transaction, &request).await })
        })
        .await
    }

    async fn dashboard_memory_overview(
        &self,
        query: DashboardMemoryOverviewQuery,
    ) -> FactStoreResult<DashboardMemoryOverview> {
        self.read(move |transaction| {
            Box::pin(async move { dashboard_memory_overview_tx(transaction, &query).await })
        })
        .await
    }

    async fn dashboard_fact_detail(
        &self,
        query: DashboardFactDetailQuery,
    ) -> FactStoreResult<Option<DashboardFactDetail>> {
        self.read(move |transaction| {
            Box::pin(async move { dashboard_fact_detail_tx(transaction, &query).await })
        })
        .await
    }

    async fn dashboard_vector_points(
        &self,
        query: DashboardVectorPointsQuery,
    ) -> FactStoreResult<Vec<DashboardVectorPoint>> {
        self.read(move |transaction| {
            Box::pin(async move { dashboard_vector_points_tx(transaction, &query).await })
        })
        .await
    }

    async fn dashboard_memory_oplog(
        &self,
        query: DashboardOplogQuery,
    ) -> FactStoreResult<Vec<DashboardOplogEntry>> {
        self.read(move |transaction| {
            Box::pin(async move { dashboard_memory_oplog_tx(transaction, &query).await })
        })
        .await
    }

    async fn record_fact_retrieval(
        &self,
        request: FactRetrievalCommand,
    ) -> FactStoreResult<Vec<FactProjection>> {
        self.write(move |transaction| {
            Box::pin(async move { record_fact_retrieval_tx(transaction, &request).await })
        })
        .await
    }

    async fn submit_fact_proposal(
        &self,
        proposal_id: ProvenanceId,
        request: FactAddCommand,
        submitter: Option<ActorId>,
        evidence: FactProposalEvidence,
    ) -> FactStoreResult<FactProposalRecord> {
        self.write(move |transaction| {
            Box::pin(async move {
                submit_fact_proposal_tx(
                    transaction,
                    proposal_id,
                    &request,
                    submitter.as_ref(),
                    &evidence,
                )
                .await
            })
        })
        .await
    }

    async fn get_fact_proposal(
        &self,
        owner: FactOwnerV1,
        proposal_id: ProvenanceId,
    ) -> FactStoreResult<Option<FactProposalRecord>> {
        self.read(move |transaction| {
            Box::pin(async move { get_fact_proposal_tx(transaction, &owner, &proposal_id).await })
        })
        .await
    }

    async fn list_fact_proposals(
        &self,
        owner: FactOwnerV1,
        state: Option<FactProposalState>,
        after_proposal_id: Option<ProvenanceId>,
        limit: usize,
    ) -> FactStoreResult<FactProposalPage> {
        self.read(move |transaction| {
            Box::pin(async move {
                list_fact_proposals_tx(
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

    async fn count_pending_fact_proposals(&self, owner: FactOwnerV1) -> FactStoreResult<u64> {
        self.read(move |transaction| {
            Box::pin(async move { count_pending_fact_proposals_tx(transaction, &owner).await })
        })
        .await
    }

    async fn reject_fact_proposal(
        &self,
        owner: FactOwnerV1,
        proposal_id: ProvenanceId,
        expected_revision: FactProposalRevision,
        reviewer: ActorId,
        reason: String,
    ) -> FactStoreResult<FactProposalRecord> {
        self.write(move |transaction| {
            Box::pin(async move {
                reject_fact_proposal_tx(
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

    async fn promote_fact_proposal(
        &self,
        request: FactProposalPromotion,
    ) -> FactStoreResult<FactProposalRecord> {
        let db = self.db.clone();
        self.write(move |transaction| {
            Box::pin(async move { promote_fact_proposal_tx(&db, transaction, &request).await })
        })
        .await
    }

    async fn promote_fact_proposal_with_disposition(
        &self,
        request: FactProposalPromotion,
    ) -> FactStoreResult<FactProposalPromotionResult> {
        let db = self.db.clone();
        self.write(move |transaction| {
            Box::pin(async move {
                promote_fact_proposal_with_disposition_tx(&db, transaction, &request).await
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

impl FactLineageStore for ProjectFactStore<'_> {
    delegate_fact_store_methods! {
        fn commit_fact(batch: FactWriteBatch) -> FactLineageResult<FactCommitOutcome>;
        fn query_current_facts(query: CurrentFactsQuery) -> FactLineageResult<Vec<StoredFactV1>>;
        fn query_fact_current(query: FactCurrentQuery) -> FactLineageResult<Option<StoredFactV1>>;
        fn query_fact_current_response(
            query: FactCurrentQuery,
        ) -> FactLineageResult<FactCurrentResponseV1>;
        fn query_fact_as_of(query: FactAsOfQuery) -> FactLineageResult<Option<StoredFactV1>>;
        fn query_fact_as_of_response(query: FactAsOfQuery) -> FactLineageResult<FactAsOfResponseV1>;
        fn query_fact_lineage(query: FactLineageQuery) -> FactLineageResult<Vec<FactLineageEventV1>>;
        fn query_fact_lineage_response(
            query: FactLineageQuery,
        ) -> FactLineageResult<FactLineageResponseV1>;
        fn resolve_legacy_fact(query: LegacyFactQuery) -> FactLineageResult<Option<FactId>>;
        fn get_retrieval_anchor(
            query: RetrievalAnchorQuery,
        ) -> FactLineageResult<Option<RetrievalAnchorRecordV2>>;
    }
}

impl FactProposalStore for ProjectFactStore<'_> {
    delegate_fact_store_methods! {
        fn commit_fact_proposal(
            promotion: PromoteFactProposal,
        ) -> Result<PromoteFactProposalOutcome, FactProposalStoreError>;
    }
}

impl FactStore for ProjectFactStore<'_> {
    delegate_fact_store_methods! {
        fn list_facts(
            query: FactListQuery,
        ) -> FactStoreResult<FactPage>;
        fn search_facts(
            query: FactSearchQuery,
        ) -> FactStoreResult<FactSearchPage>;
        fn probe_facts(
            query: FactSearchQuery,
        ) -> FactStoreResult<FactSearchPage>;
        fn related_facts(
            query: FactSearchQuery,
        ) -> FactStoreResult<FactSearchPage>;
        fn reason_facts(
            query: FactSearchQuery,
        ) -> FactStoreResult<FactSearchPage>;
        fn find_contradictions(
            query: FactContradictionQuery,
        ) -> FactStoreResult<FactContradictionPage>;
        fn get_fact(
            target: FactTarget,
        ) -> FactStoreResult<Option<FactProjection>>;
        fn fact_history(
            query: FactHistoryQuery,
        ) -> FactStoreResult<FactHistory>;
        fn memory_status(
            owner: FactOwnerV1,
        ) -> FactStoreResult<MemoryStatus>;
        fn inspect_fact(
            target: FactTarget,
        ) -> FactStoreResult<Option<FactInspection>>;
        fn add_fact(
            request: FactAddCommand,
        ) -> FactStoreResult<FactAddOutcome>;
        fn update_fact(
            request: FactUpdateCommand,
        ) -> FactStoreResult<FactUpdateOutcome>;
        fn remove_fact(
            request: FactRemoveCommand,
        ) -> FactStoreResult<FactRemoveOutcome>;
        fn record_fact_feedback(
            request: FactFeedbackCommand,
        ) -> FactStoreResult<FactFeedbackOutcome>;
        fn fact_feedback_history(
            query: FactFeedbackHistoryQuery,
        ) -> FactStoreResult<FactFeedbackHistory>;
        fn find_fact_by_content_digest(
            query: FactContentDigestQuery,
        ) -> FactStoreResult<Option<FactProjection>>;
        fn apply_fact_curation(
            request: FactCurationBatch,
        ) -> FactStoreResult<FactCurationReceipt>;
        fn merge_facts(
            request: FactMergeCommand,
        ) -> FactStoreResult<FactMergeOutcome>;
        fn repair_memory(
            request: MemoryRepairCommand,
        ) -> FactStoreResult<MemoryRepairStats>;
        fn dashboard_memory_overview(
            query: DashboardMemoryOverviewQuery,
        ) -> FactStoreResult<DashboardMemoryOverview>;
        fn dashboard_fact_detail(
            query: DashboardFactDetailQuery,
        ) -> FactStoreResult<Option<DashboardFactDetail>>;
        fn dashboard_vector_points(
            query: DashboardVectorPointsQuery,
        ) -> FactStoreResult<Vec<DashboardVectorPoint>>;
        fn dashboard_memory_oplog(
            query: DashboardOplogQuery,
        ) -> FactStoreResult<Vec<DashboardOplogEntry>>;
        fn record_fact_retrieval(
            request: FactRetrievalCommand,
        ) -> FactStoreResult<Vec<FactProjection>>;
        fn submit_fact_proposal(
            proposal_id: ProvenanceId,
            request: FactAddCommand,
            submitter: Option<ActorId>,
            evidence: FactProposalEvidence,
        ) -> FactStoreResult<FactProposalRecord>;
        fn get_fact_proposal(
            owner: FactOwnerV1,
            proposal_id: ProvenanceId,
        ) -> FactStoreResult<Option<FactProposalRecord>>;
        fn list_fact_proposals(
            owner: FactOwnerV1,
            state: Option<FactProposalState>,
            after_proposal_id: Option<ProvenanceId>,
            limit: usize,
        ) -> FactStoreResult<FactProposalPage>;
        fn count_pending_fact_proposals(
            owner: FactOwnerV1,
        ) -> FactStoreResult<u64>;
        fn reject_fact_proposal(
            owner: FactOwnerV1,
            proposal_id: ProvenanceId,
            expected_revision: FactProposalRevision,
            reviewer: ActorId,
            reason: String,
        ) -> FactStoreResult<FactProposalRecord>;
        fn promote_fact_proposal(
            request: FactProposalPromotion,
        ) -> FactStoreResult<FactProposalRecord>;
        fn promote_fact_proposal_with_disposition(
            request: FactProposalPromotion,
        ) -> FactStoreResult<FactProposalPromotionResult>;
    }
}

#[cfg(test)]
#[path = "fact_response_metadata_test.rs"]
mod fact_response_metadata_test;
