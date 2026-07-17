//! Database-backed authority for append-only facts, evidence, and provenance.

use crate::db::Database;

use tracedecay_domain::{
    ActorId, FactId, FactLineageEventV1, FactOwnerV1, ProvenanceId, RetrievalAnchorRecordV2,
};
use tracedecay_store::{
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
    CompatibilityLegacyMemoryCutoverCommandV1, CompatibilityLegacyMemoryCutoverProgressV1,
    CompatibilityMemoryRepairCommandV1, CompatibilityMemoryRepairStatsV1,
    CompatibilityMemoryStatusV1, CurrentFactsQuery, FactAsOfQuery, FactCommitOutcome,
    FactCompatibilityResult, FactCompatibilityStore, FactCurrentQuery, FactLineageQuery,
    FactProposalStore, FactProposalStoreError, FactStore, FactStoreResult, FactWriteBatch,
    LegacyFactQuery, PromoteFactProposal, PromoteFactProposalOutcome, RetrievalAnchorQuery,
    StoredFactV1,
};

use crud::{
    PROMOTE_OPERATION, add_compatibility_fact_tx, compatibility_fact_feedback_history_tx,
    compatibility_fact_history_tx, find_compatibility_fact_by_content_digest_tx,
    get_compatibility_fact_tx, get_retrieval_anchor_tx, inspect_compatibility_fact_tx,
    list_compatibility_facts_tx, promote_compatibility_fact_proposal_tx,
    promote_compatibility_fact_proposal_with_disposition_tx, promote_fact_proposal_tx,
    query_current_facts_tx, query_fact_as_of_tx, query_fact_current_tx, query_fact_lineage_tx,
    record_compatibility_fact_feedback_tx, remove_compatibility_fact_tx,
    update_compatibility_fact_tx,
};
use curation::{apply_compatibility_fact_curation_tx, merge_compatibility_facts_tx};
use cutover::{advance_compatibility_legacy_memory_cutover_tx, compatibility_memory_status_tx};
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

mod crud;
mod curation;
mod cutover;
mod dashboard;
mod envelope;
mod primitives;
mod projection;
mod proposals;
mod repair;
mod scoring;
mod search;

pub(crate) use repair::{COMPATIBILITY_REPAIR_BANK_BATCH, COMPATIBILITY_REPAIR_VECTOR_BATCH};

#[cfg(test)]
use crate::db::MemoryV2FeedbackHistoryRepairBatchOutcome;
#[cfg(test)]
use envelope::compatibility_lookup_operation_receipt_tx;
#[cfg(test)]
use libsql::params;
#[cfg(test)]
use primitives::{
    COMPATIBILITY_WRITE_OPERATION, OwnerKey, compatibility_source_store_id, storage_message,
};
#[cfg(test)]
use repair::{compatibility_receipt_feedback_history_repair, compatibility_repair_request_digest};
#[cfg(test)]
use tracedecay_domain::SourceStoreId;
#[cfg(test)]
use tracedecay_store::{
    CompatibilityFactMappingV1, CompatibilityFeedbackRepairProgressV1, FactCompatibilityStoreError,
};

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

impl FactStore for DatabaseFactStore<'_> {
    async fn commit_fact(&self, batch: FactWriteBatch) -> FactStoreResult<FactCommitOutcome> {
        self.commit_batch(&batch).await
    }

    async fn query_current_facts(
        &self,
        query: CurrentFactsQuery,
    ) -> FactStoreResult<Vec<StoredFactV1>> {
        let snapshot = self
            .db
            .begin_isolated_read_snapshot(QUERY_OPERATION)
            .await
            .map_err(|error| storage_error(QUERY_OPERATION, error))?;
        let result = query_current_facts_tx(&snapshot, &query).await;
        finish_read_snapshot(snapshot, result).await
    }

    async fn query_fact_current(
        &self,
        query: FactCurrentQuery,
    ) -> FactStoreResult<Option<StoredFactV1>> {
        let snapshot = self
            .db
            .begin_isolated_read_snapshot(QUERY_OPERATION)
            .await
            .map_err(|error| storage_error(QUERY_OPERATION, error))?;
        let result = query_fact_current_tx(&snapshot, query.owner(), query.fact_id()).await;
        finish_read_snapshot(snapshot, result).await
    }

    async fn query_fact_as_of(
        &self,
        query: FactAsOfQuery,
    ) -> FactStoreResult<Option<StoredFactV1>> {
        let snapshot = self
            .db
            .begin_isolated_read_snapshot(QUERY_OPERATION)
            .await
            .map_err(|error| storage_error(QUERY_OPERATION, error))?;
        let result = query_fact_as_of_tx(&snapshot, &query).await;
        finish_read_snapshot(snapshot, result).await
    }

    async fn query_fact_lineage(
        &self,
        query: FactLineageQuery,
    ) -> FactStoreResult<Vec<FactLineageEventV1>> {
        let snapshot = self
            .db
            .begin_isolated_read_snapshot(QUERY_OPERATION)
            .await
            .map_err(|error| storage_error(QUERY_OPERATION, error))?;
        let result = query_fact_lineage_tx(&snapshot, &query).await;
        finish_read_snapshot(snapshot, result).await
    }

    async fn resolve_legacy_fact(&self, query: LegacyFactQuery) -> FactStoreResult<Option<FactId>> {
        let snapshot = self
            .db
            .begin_isolated_read_snapshot(QUERY_OPERATION)
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
            .begin_isolated_read_snapshot(QUERY_OPERATION)
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
            .begin_write_transaction(PROMOTE_OPERATION)
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
        query: CompatibilityFactListQueryV1,
    ) -> FactCompatibilityResult<CompatibilityFactPageV1> {
        self.compatibility_read(move |transaction| {
            Box::pin(async move { list_compatibility_facts_tx(transaction, &query).await })
        })
        .await
    }

    async fn search_compatibility_facts(
        &self,
        query: CompatibilityFactSearchQuery,
    ) -> FactCompatibilityResult<CompatibilityFactSearchPageV1> {
        self.compatibility_read(move |transaction| {
            Box::pin(async move { search_compatibility_facts_tx(transaction, &query).await })
        })
        .await
    }

    async fn probe_compatibility_facts(
        &self,
        query: CompatibilityFactSearchQuery,
    ) -> FactCompatibilityResult<CompatibilityFactSearchPageV1> {
        self.compatibility_read(move |transaction| {
            Box::pin(async move { probe_compatibility_facts_tx(transaction, &query).await })
        })
        .await
    }

    async fn related_compatibility_facts(
        &self,
        query: CompatibilityFactSearchQuery,
    ) -> FactCompatibilityResult<CompatibilityFactSearchPageV1> {
        self.compatibility_read(move |transaction| {
            Box::pin(async move { related_compatibility_facts_tx(transaction, &query).await })
        })
        .await
    }

    async fn reason_compatibility_facts(
        &self,
        query: CompatibilityFactSearchQuery,
    ) -> FactCompatibilityResult<CompatibilityFactSearchPageV1> {
        self.compatibility_read(move |transaction| {
            Box::pin(async move { reason_compatibility_facts_tx(transaction, &query).await })
        })
        .await
    }

    async fn find_compatibility_contradictions(
        &self,
        query: CompatibilityFactContradictionQueryV1,
    ) -> FactCompatibilityResult<CompatibilityFactContradictionPageV1> {
        self.compatibility_read(move |transaction| {
            Box::pin(async move { find_compatibility_contradictions_tx(transaction, &query).await })
        })
        .await
    }

    async fn get_compatibility_fact(
        &self,
        target: CompatibilityFactTargetV1,
    ) -> FactCompatibilityResult<Option<CompatibilityFactProjectionV1>> {
        self.compatibility_read(move |transaction| {
            Box::pin(async move { get_compatibility_fact_tx(transaction, &target).await })
        })
        .await
    }

    async fn compatibility_fact_history(
        &self,
        query: CompatibilityFactHistoryQueryV1,
    ) -> FactCompatibilityResult<CompatibilityFactHistoryV1> {
        self.compatibility_read(move |transaction| {
            Box::pin(async move { compatibility_fact_history_tx(transaction, &query).await })
        })
        .await
    }

    async fn compatibility_memory_status(
        &self,
        owner: FactOwnerV1,
    ) -> FactCompatibilityResult<CompatibilityMemoryStatusV1> {
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
        target: CompatibilityFactTargetV1,
    ) -> FactCompatibilityResult<Option<CompatibilityFactInspectionV1>> {
        self.compatibility_read(move |transaction| {
            Box::pin(async move { inspect_compatibility_fact_tx(transaction, &target).await })
        })
        .await
    }

    async fn add_compatibility_fact(
        &self,
        request: CompatibilityFactAddCommandV1,
    ) -> FactCompatibilityResult<CompatibilityFactAddOutcomeV1> {
        let db = self.db.clone();
        self.compatibility_write(move |transaction| {
            Box::pin(async move { add_compatibility_fact_tx(&db, transaction, &request).await })
        })
        .await
    }

    async fn update_compatibility_fact(
        &self,
        request: CompatibilityFactUpdateCommandV1,
    ) -> FactCompatibilityResult<CompatibilityFactUpdateOutcomeV1> {
        let db = self.db.clone();
        self.compatibility_write(move |transaction| {
            Box::pin(async move { update_compatibility_fact_tx(&db, transaction, &request).await })
        })
        .await
    }

    async fn remove_compatibility_fact(
        &self,
        request: CompatibilityFactRemoveCommandV1,
    ) -> FactCompatibilityResult<CompatibilityFactRemoveOutcomeV1> {
        let db = self.db.clone();
        self.compatibility_write(move |transaction| {
            Box::pin(async move { remove_compatibility_fact_tx(&db, transaction, &request).await })
        })
        .await
    }

    async fn record_compatibility_fact_feedback(
        &self,
        request: CompatibilityFactFeedbackCommandV1,
    ) -> FactCompatibilityResult<CompatibilityFactFeedbackOutcomeV1> {
        self.compatibility_write(move |transaction| {
            Box::pin(
                async move { record_compatibility_fact_feedback_tx(transaction, &request).await },
            )
        })
        .await
    }

    async fn compatibility_fact_feedback_history(
        &self,
        query: CompatibilityFactFeedbackHistoryQueryV1,
    ) -> FactCompatibilityResult<CompatibilityFactFeedbackHistoryV1> {
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
        query: CompatibilityFactContentDigestQueryV1,
    ) -> FactCompatibilityResult<Option<CompatibilityFactProjectionV1>> {
        self.compatibility_read(move |transaction| {
            Box::pin(async move {
                find_compatibility_fact_by_content_digest_tx(transaction, &query).await
            })
        })
        .await
    }

    async fn apply_compatibility_fact_curation(
        &self,
        request: CompatibilityFactCurationBatchV1,
    ) -> FactCompatibilityResult<CompatibilityFactCurationReceiptV1> {
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
        request: CompatibilityFactMergeCommandV1,
    ) -> FactCompatibilityResult<CompatibilityFactMergeOutcomeV1> {
        let db = self.db.clone();
        self.compatibility_write(move |transaction| {
            Box::pin(async move { merge_compatibility_facts_tx(&db, transaction, &request).await })
        })
        .await
    }

    async fn repair_compatibility_memory(
        &self,
        request: CompatibilityMemoryRepairCommandV1,
    ) -> FactCompatibilityResult<CompatibilityMemoryRepairStatsV1> {
        let db = self.db.clone();
        self.compatibility_write(move |transaction| {
            Box::pin(
                async move { repair_compatibility_memory_tx(&db, transaction, &request).await },
            )
        })
        .await
    }

    async fn advance_compatibility_legacy_memory_cutover(
        &self,
        request: CompatibilityLegacyMemoryCutoverCommandV1,
    ) -> FactCompatibilityResult<CompatibilityLegacyMemoryCutoverProgressV1> {
        advance_compatibility_legacy_memory_cutover_tx(self.db, &request).await
    }

    async fn dashboard_compatibility_memory_overview(
        &self,
        query: CompatibilityDashboardMemoryOverviewQueryV1,
    ) -> FactCompatibilityResult<CompatibilityDashboardMemoryOverviewV1> {
        self.compatibility_read(move |transaction| {
            Box::pin(async move {
                dashboard_compatibility_memory_overview_tx(transaction, &query).await
            })
        })
        .await
    }

    async fn dashboard_compatibility_fact_detail(
        &self,
        query: CompatibilityDashboardFactDetailQueryV1,
    ) -> FactCompatibilityResult<Option<CompatibilityDashboardFactDetailV1>> {
        self.compatibility_read(move |transaction| {
            Box::pin(
                async move { dashboard_compatibility_fact_detail_tx(transaction, &query).await },
            )
        })
        .await
    }

    async fn dashboard_compatibility_vector_points(
        &self,
        query: CompatibilityDashboardVectorPointsQueryV1,
    ) -> FactCompatibilityResult<Vec<CompatibilityDashboardVectorPointV1>> {
        self.compatibility_read(move |transaction| {
            Box::pin(
                async move { dashboard_compatibility_vector_points_tx(transaction, &query).await },
            )
        })
        .await
    }

    async fn dashboard_compatibility_memory_oplog(
        &self,
        query: CompatibilityDashboardOplogQueryV1,
    ) -> FactCompatibilityResult<Vec<CompatibilityDashboardOplogEntryV1>> {
        self.compatibility_read(move |transaction| {
            Box::pin(
                async move { dashboard_compatibility_memory_oplog_tx(transaction, &query).await },
            )
        })
        .await
    }

    async fn record_compatibility_fact_retrieval(
        &self,
        request: CompatibilityFactRetrievalCommandV1,
    ) -> FactCompatibilityResult<Vec<CompatibilityFactProjectionV1>> {
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
        request: CompatibilityFactAddCommandV1,
        submitter: Option<ActorId>,
    ) -> FactCompatibilityResult<CompatibilityFactProposalRecordV1> {
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
    ) -> FactCompatibilityResult<Option<CompatibilityFactProposalRecordV1>> {
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
        state: Option<CompatibilityFactProposalStateV1>,
        after_proposal_id: Option<ProvenanceId>,
        limit: usize,
    ) -> FactCompatibilityResult<CompatibilityFactProposalPageV1> {
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
        expected_revision: CompatibilityFactProposalRevisionV1,
        reviewer: ActorId,
        reason: String,
    ) -> FactCompatibilityResult<CompatibilityFactProposalRecordV1> {
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
        request: CompatibilityFactProposalImportV1,
    ) -> FactCompatibilityResult<CompatibilityFactProposalImportReceiptV1> {
        self.compatibility_write(move |transaction| {
            Box::pin(async move {
                import_legacy_compatibility_fact_proposals_tx(transaction, &request).await
            })
        })
        .await
    }

    async fn promote_compatibility_fact_proposal(
        &self,
        request: CompatibilityFactProposalPromotionV1,
    ) -> FactCompatibilityResult<CompatibilityFactProposalRecordV1> {
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
        request: CompatibilityFactProposalPromotionV1,
    ) -> FactCompatibilityResult<CompatibilityFactProposalPromotionResultV1> {
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

#[cfg(test)]
#[path = "memory_repair_test.rs"]
mod memory_repair_test;

#[cfg(test)]
#[path = "memory_cutover_test.rs"]
mod memory_cutover_test;
