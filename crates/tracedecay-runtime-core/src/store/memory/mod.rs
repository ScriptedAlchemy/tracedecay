//! Database-backed authority for append-only facts, evidence, and provenance.

use crate::db::Database;

use tracedecay_domain::RunId;
use tracedecay_domain::{FactLineageEventV1, FactOwnerV1, ProvenanceId, RetrievalAnchorRecordV2};
use tracedecay_store::ProjectMemoryAutomationRunReceiptsV1;
use tracedecay_store::{
    CurrentFactsQuery, FactAsOfQuery, FactAsOfResponseV1, FactCommitOutcome, FactCurrentQuery,
    FactCurrentResponseV1, FactLineageQuery, FactLineageResponseV1, FactReadControl, FactStore,
    FactStoreResult, FactWriteBatch, FactWriteControl,
    ProjectMemoryAutomaticFactApplyDispositionV1, ProjectMemoryAutomaticFactApplyResultV1,
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
    ProjectMemoryFactSearchQuery, ProjectMemoryFactStore, ProjectMemoryFactUpdateCommandV1,
    ProjectMemoryFactUpdateOutcomeV1, ProjectMemoryGraphPageV1, ProjectMemoryGraphQueryV1,
    ProjectMemoryGraphStore, ProjectMemoryMemoryStatusV1, RetrievalAnchorQuery, StoredFactV1,
};

use automatic_facts::{
    get_project_memory_automatic_fact_receipt_tx, list_project_memory_automatic_fact_receipts_tx,
};
use automation_run_receipts::project_memory_automation_run_receipts_tx;
use crud::{
    add_project_memory_fact_tx, apply_project_memory_automatic_fact_tx, fact_response_metadata_tx,
    find_project_memory_fact_by_content_digest_controlled_tx,
    get_project_memory_fact_controlled_tx, get_retrieval_anchor_tx,
    inspect_project_memory_fact_controlled_tx, list_project_memory_facts_controlled_tx,
    project_memory_fact_feedback_history_tx, project_memory_fact_history_controlled_tx,
    query_current_facts_tx, query_fact_as_of_response_tx, query_fact_as_of_tx,
    query_fact_current_response_tx, query_fact_current_tx, query_fact_lineage_response_tx,
    query_fact_lineage_tx, record_project_memory_fact_feedback_tx, remove_project_memory_fact_tx,
    update_project_memory_fact_tx,
};
use curation::{apply_project_memory_fact_curation_tx, merge_project_memory_facts_tx};
use dashboard::{
    dashboard_project_memory_fact_detail_tx, dashboard_project_memory_oplog_tx,
    dashboard_project_memory_overview_tx, dashboard_project_memory_vector_points_tx,
};
use envelope::finish_read_snapshot;
use primitives::{COMMIT_OPERATION, QUERY_OPERATION, storage_error};
use search::{
    ensure_project_memory_search_not_cancelled, find_project_memory_contradictions_tx,
    probe_project_memory_facts_tx, reason_project_memory_facts_tx,
    record_project_memory_fact_retrieval_tx, related_project_memory_facts,
    search_project_memory_facts,
};
use status::project_memory_status_tx;

mod automatic_facts;
mod automation_run_receipts;
mod candidates;
#[cfg(feature = "test-transport")]
mod commit_barrier;
mod crud;
mod curation;
mod dashboard;
#[cfg(test)]
mod dashboard_tests;
mod envelope;
mod graph;
mod graph_manifest;
#[cfg(test)]
mod graph_reconciliation_tests;
#[cfg(test)]
mod graph_tests;
mod primitives;
mod projection;
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[doc(hidden)]
pub enum ProjectMemoryGraphReconciliationScheduleV1 {
    NotMounted,
    Scheduled,
    AlreadyScheduled,
    Retiring,
    LifecycleClosed,
}

/// Internal daemon mount/write hook for derived verified-graph catch-up.
///
/// Product callers mutate canonical facts through [`ProjectMemoryFactStore`];
/// they never reconcile topology directly.
#[doc(hidden)]
pub fn schedule_project_memory_graph_reconciliation(
    db: Database,
) -> ProjectMemoryGraphReconciliationScheduleV1 {
    graph::schedule_project_memory_graph_reconciliation(db)
}

impl<'a> DatabaseFactStore<'a> {
    pub const fn new(db: &'a Database) -> Self {
        Self { db }
    }
}

impl FactStore for DatabaseFactStore<'_> {
    async fn commit_fact(
        &self,
        batch: FactWriteBatch,
        write_control: &FactWriteControl,
    ) -> FactStoreResult<FactCommitOutcome> {
        match runtime::retained_fact_runtime(self.db)? {
            Some(_) => {
                let db = (*self.db).clone();
                let write_control = write_control.clone();
                // The task owns the retained-dispatch receipt path so caller
                // cancellation cannot strand a durable commit before its
                // derived graph reconciliation trigger is recorded.
                tokio::spawn(async move {
                    let retained = db.retained_runtime();
                    let outcome =
                        runtime::commit_fact(&db, retained, batch, &write_control).await?;
                    if matches!(&outcome, FactCommitOutcome::Committed(_)) {
                        graph::publish_project_memory_graph_after_write(db.clone()).await;
                    }
                    Ok(outcome)
                })
                .await
                .map_err(|error| storage_error(COMMIT_OPERATION, error))?
            }
            None => self.commit_batch(&batch, write_control).await,
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

impl ProjectMemoryFactStore for DatabaseFactStore<'_> {
    async fn list_project_memory_facts(
        &self,
        query: ProjectMemoryFactListQueryV1,
        read_control: &FactReadControl,
    ) -> FactStoreResult<ProjectMemoryFactPageV1> {
        let read_control = read_control.clone();
        self.project_memory_read(move |transaction| {
            Box::pin(async move {
                list_project_memory_facts_controlled_tx(transaction, &query, &read_control).await
            })
        })
        .await
    }

    async fn search_project_memory_facts(
        &self,
        query: ProjectMemoryFactSearchQuery,
        read_control: &FactReadControl,
    ) -> FactStoreResult<ProjectMemoryFactSearchPageV1> {
        search_project_memory_facts(self.db, &query, read_control).await
    }

    async fn probe_project_memory_facts(
        &self,
        query: ProjectMemoryFactSearchQuery,
        read_control: &FactReadControl,
    ) -> FactStoreResult<ProjectMemoryFactSearchPageV1> {
        ensure_project_memory_search_not_cancelled(read_control)?;
        let owned_read_control = read_control.clone();
        let page = self
            .project_memory_read(move |transaction| {
                Box::pin(async move {
                    probe_project_memory_facts_tx(transaction, &query, &owned_read_control).await
                })
            })
            .await?;
        ensure_project_memory_search_not_cancelled(read_control)?;
        Ok(page)
    }

    async fn related_project_memory_facts(
        &self,
        query: ProjectMemoryFactSearchQuery,
        read_control: &FactReadControl,
    ) -> FactStoreResult<ProjectMemoryFactSearchPageV1> {
        related_project_memory_facts(self.db, &query, read_control).await
    }

    async fn reason_project_memory_facts(
        &self,
        query: ProjectMemoryFactSearchQuery,
        read_control: &FactReadControl,
    ) -> FactStoreResult<ProjectMemoryFactSearchPageV1> {
        ensure_project_memory_search_not_cancelled(read_control)?;
        let owned_read_control = read_control.clone();
        let page = self
            .project_memory_read(move |transaction| {
                Box::pin(async move {
                    reason_project_memory_facts_tx(transaction, &query, &owned_read_control).await
                })
            })
            .await?;
        ensure_project_memory_search_not_cancelled(read_control)?;
        Ok(page)
    }

    async fn find_project_memory_contradictions(
        &self,
        query: ProjectMemoryFactContradictionQueryV1,
        read_control: &FactReadControl,
    ) -> FactStoreResult<ProjectMemoryFactContradictionPageV1> {
        let read_control = read_control.clone();
        self.project_memory_read(move |transaction| {
            Box::pin(async move {
                find_project_memory_contradictions_tx(transaction, &query, &read_control).await
            })
        })
        .await
    }

    async fn get_project_memory_fact(
        &self,
        target: ProjectMemoryFactIdV1,
        read_control: &FactReadControl,
    ) -> FactStoreResult<Option<ProjectMemoryFactProjectionV1>> {
        let read_control = read_control.clone();
        self.project_memory_read(move |transaction| {
            Box::pin(async move {
                get_project_memory_fact_controlled_tx(transaction, &target, &read_control).await
            })
        })
        .await
    }

    async fn project_memory_fact_history(
        &self,
        query: ProjectMemoryFactHistoryQueryV1,
        read_control: &FactReadControl,
    ) -> FactStoreResult<ProjectMemoryFactHistoryV1> {
        let read_control = read_control.clone();
        self.project_memory_read(move |transaction| {
            Box::pin(async move {
                project_memory_fact_history_controlled_tx(transaction, &query, &read_control).await
            })
        })
        .await
    }

    async fn project_memory_status(
        &self,
        owner: FactOwnerV1,
        read_control: &FactReadControl,
    ) -> FactStoreResult<ProjectMemoryMemoryStatusV1> {
        let read_control = read_control.clone();
        self.project_memory_read(move |transaction| {
            Box::pin(
                async move { project_memory_status_tx(transaction, &owner, &read_control).await },
            )
        })
        .await
    }

    async fn inspect_project_memory_fact(
        &self,
        target: ProjectMemoryFactIdV1,
        read_control: &FactReadControl,
    ) -> FactStoreResult<Option<ProjectMemoryFactInspectionV1>> {
        let read_control = read_control.clone();
        self.project_memory_read(move |transaction| {
            Box::pin(async move {
                inspect_project_memory_fact_controlled_tx(transaction, &target, &read_control).await
            })
        })
        .await
    }

    async fn add_project_memory_fact(
        &self,
        request: ProjectMemoryFactAddCommandV1,
        write_control: &FactWriteControl,
    ) -> FactStoreResult<ProjectMemoryFactAddOutcomeV1> {
        self.project_memory_write(
            write_control,
            |outcome: &ProjectMemoryFactAddOutcomeV1| {
                outcome.commit_receipt().is_some() && !outcome.commit_replayed()
            },
            move |transaction| {
                Box::pin(async move { add_project_memory_fact_tx(transaction, &request).await })
            },
        )
        .await
    }

    async fn update_project_memory_fact(
        &self,
        request: ProjectMemoryFactUpdateCommandV1,
        write_control: &FactWriteControl,
    ) -> FactStoreResult<ProjectMemoryFactUpdateOutcomeV1> {
        self.project_memory_write(
            write_control,
            |outcome: &ProjectMemoryFactUpdateOutcomeV1| !outcome.commit_replayed(),
            move |transaction| {
                Box::pin(async move { update_project_memory_fact_tx(transaction, &request).await })
            },
        )
        .await
    }

    async fn remove_project_memory_fact(
        &self,
        request: ProjectMemoryFactRemoveCommandV1,
        write_control: &FactWriteControl,
    ) -> FactStoreResult<ProjectMemoryFactRemoveOutcomeV1> {
        self.project_memory_write(
            write_control,
            |outcome: &ProjectMemoryFactRemoveOutcomeV1| {
                outcome.was_removed() && !outcome.commit_replayed()
            },
            move |transaction| {
                Box::pin(async move { remove_project_memory_fact_tx(transaction, &request).await })
            },
        )
        .await
    }

    async fn record_project_memory_fact_feedback(
        &self,
        request: ProjectMemoryFactFeedbackCommandV1,
        write_control: &FactWriteControl,
    ) -> FactStoreResult<ProjectMemoryFactFeedbackOutcomeV1> {
        self.project_memory_write(
            write_control,
            |_| false,
            move |transaction| {
                Box::pin(async move {
                    record_project_memory_fact_feedback_tx(transaction, &request).await
                })
            },
        )
        .await
    }

    async fn project_memory_fact_feedback_history(
        &self,
        query: ProjectMemoryFactFeedbackHistoryQueryV1,
        read_control: &FactReadControl,
    ) -> FactStoreResult<ProjectMemoryFactFeedbackHistoryV1> {
        let read_control = read_control.clone();
        self.project_memory_read(move |transaction| {
            Box::pin(async move {
                project_memory_fact_feedback_history_tx(transaction, &query, &read_control).await
            })
        })
        .await
    }

    async fn find_project_memory_fact_by_content_digest(
        &self,
        query: ProjectMemoryFactContentDigestQueryV1,
        read_control: &FactReadControl,
    ) -> FactStoreResult<Option<ProjectMemoryFactProjectionV1>> {
        let read_control = read_control.clone();
        self.project_memory_read(move |transaction| {
            Box::pin(async move {
                find_project_memory_fact_by_content_digest_controlled_tx(
                    transaction,
                    &query,
                    &read_control,
                )
                .await
            })
        })
        .await
    }

    async fn apply_project_memory_fact_curation(
        &self,
        request: ProjectMemoryFactCurationBatchV1,
        write_control: &FactWriteControl,
    ) -> FactStoreResult<ProjectMemoryFactCurationReceiptV1> {
        self.project_memory_write(
            write_control,
            |receipt: &ProjectMemoryFactCurationReceiptV1| {
                !receipt.replayed() && !receipt.changed_facts().is_empty()
            },
            move |transaction| {
                Box::pin(async move {
                    apply_project_memory_fact_curation_tx(transaction, &request).await
                })
            },
        )
        .await
    }

    async fn merge_project_memory_facts(
        &self,
        request: ProjectMemoryFactMergeCommandV1,
        write_control: &FactWriteControl,
    ) -> FactStoreResult<ProjectMemoryFactMergeOutcomeV1> {
        self.project_memory_write(
            write_control,
            |outcome: &ProjectMemoryFactMergeOutcomeV1| !outcome.replayed(),
            move |transaction| {
                Box::pin(async move { merge_project_memory_facts_tx(transaction, &request).await })
            },
        )
        .await
    }

    async fn dashboard_project_memory_overview(
        &self,
        query: ProjectMemoryDashboardMemoryOverviewQueryV1,
        read_control: &FactReadControl,
    ) -> FactStoreResult<ProjectMemoryDashboardMemoryOverviewV1> {
        let read_control = read_control.clone();
        self.project_memory_read(move |transaction| {
            Box::pin(async move {
                dashboard_project_memory_overview_tx(transaction, &query, &read_control).await
            })
        })
        .await
    }

    async fn dashboard_project_memory_fact_detail(
        &self,
        query: ProjectMemoryDashboardFactDetailQueryV1,
        read_control: &FactReadControl,
    ) -> FactStoreResult<Option<ProjectMemoryDashboardFactDetailV1>> {
        let read_control = read_control.clone();
        self.project_memory_read(move |transaction| {
            Box::pin(async move {
                dashboard_project_memory_fact_detail_tx(transaction, &query, &read_control).await
            })
        })
        .await
    }

    async fn dashboard_project_memory_vector_points(
        &self,
        query: ProjectMemoryDashboardVectorPointsQueryV1,
        read_control: &FactReadControl,
    ) -> FactStoreResult<Vec<ProjectMemoryDashboardVectorPointV1>> {
        let read_control = read_control.clone();
        self.project_memory_read(move |transaction| {
            Box::pin(async move {
                dashboard_project_memory_vector_points_tx(transaction, &query, &read_control).await
            })
        })
        .await
    }

    async fn dashboard_project_memory_oplog(
        &self,
        query: ProjectMemoryDashboardOplogQueryV1,
        read_control: &FactReadControl,
    ) -> FactStoreResult<Vec<ProjectMemoryDashboardOplogEntryV1>> {
        let read_control = read_control.clone();
        self.project_memory_read(move |transaction| {
            Box::pin(async move {
                dashboard_project_memory_oplog_tx(transaction, &query, &read_control).await
            })
        })
        .await
    }

    async fn record_project_memory_fact_retrieval(
        &self,
        request: ProjectMemoryFactRetrievalCommandV1,
        write_control: &FactWriteControl,
    ) -> FactStoreResult<ProjectMemoryFactRetrievalOutcomeV1> {
        self.project_memory_write(
            write_control,
            |_| false,
            move |transaction| {
                Box::pin(async move {
                    record_project_memory_fact_retrieval_tx(transaction, &request).await
                })
            },
        )
        .await
    }

    async fn apply_project_memory_automatic_fact(
        &self,
        apply_id: ProvenanceId,
        request: ProjectMemoryFactAddCommandV1,
        evidence: ProjectMemoryAutomaticFactEvidenceV1,
        write_control: &FactWriteControl,
    ) -> FactStoreResult<ProjectMemoryAutomaticFactApplyResultV1> {
        self.project_memory_write(
            write_control,
            |outcome: &ProjectMemoryAutomaticFactApplyResultV1| {
                outcome.disposition() == ProjectMemoryAutomaticFactApplyDispositionV1::Applied
            },
            move |transaction| {
                Box::pin(async move {
                    apply_project_memory_automatic_fact_tx(
                        transaction,
                        apply_id,
                        &request,
                        &evidence,
                    )
                    .await
                })
            },
        )
        .await
    }

    async fn get_project_memory_automatic_fact_receipt(
        &self,
        owner: FactOwnerV1,
        apply_id: ProvenanceId,
        read_control: &FactReadControl,
    ) -> FactStoreResult<Option<ProjectMemoryAutomaticFactReceiptV1>> {
        let read_control = read_control.clone();
        self.project_memory_read(move |transaction| {
            Box::pin(async move {
                get_project_memory_automatic_fact_receipt_tx(
                    transaction,
                    &owner,
                    &apply_id,
                    &read_control,
                )
                .await
            })
        })
        .await
    }

    async fn list_project_memory_automatic_fact_receipts(
        &self,
        owner: FactOwnerV1,
        state: Option<ProjectMemoryAutomaticFactStateV1>,
        after_apply_id: Option<ProvenanceId>,
        limit: usize,
        read_control: &FactReadControl,
    ) -> FactStoreResult<ProjectMemoryAutomaticFactReceiptPageV1> {
        let read_control = read_control.clone();
        self.project_memory_read(move |transaction| {
            Box::pin(async move {
                list_project_memory_automatic_fact_receipts_tx(
                    transaction,
                    &owner,
                    state,
                    after_apply_id.as_ref(),
                    limit,
                    &read_control,
                )
                .await
            })
        })
        .await
    }

    async fn project_memory_automation_run_receipts(
        &self,
        owner: FactOwnerV1,
        run_id: RunId,
        read_control: &FactReadControl,
    ) -> FactStoreResult<ProjectMemoryAutomationRunReceiptsV1> {
        if let Some(runtime) = runtime::retained_fact_runtime(self.db)? {
            runtime::validate_owner_binding(
                runtime.binding(),
                &owner,
                "recover memory automation receipts",
            )?;
        }
        let read_control = read_control.clone();
        self.project_memory_read(move |transaction| {
            Box::pin(async move {
                project_memory_automation_run_receipts_tx(
                    transaction,
                    &owner,
                    &run_id,
                    &read_control,
                )
                .await
            })
        })
        .await
    }
}

impl ProjectMemoryGraphStore for DatabaseFactStore<'_> {
    async fn project_memory_graph(
        &self,
        query: ProjectMemoryGraphQueryV1,
        read_control: &FactReadControl,
    ) -> FactStoreResult<ProjectMemoryGraphPageV1> {
        graph::project_memory_graph(self.db, query, read_control).await
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
        fn commit_fact(
            batch: FactWriteBatch,
            write_control: &FactWriteControl,
        ) -> FactStoreResult<FactCommitOutcome>;
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
        fn get_retrieval_anchor(
            query: RetrievalAnchorQuery,
        ) -> FactStoreResult<Option<RetrievalAnchorRecordV2>>;
    }
}

impl ProjectMemoryFactStore for ProjectFactStore<'_> {
    delegate_fact_store_methods! {
        fn list_project_memory_facts(
            query: ProjectMemoryFactListQueryV1,
            read_control: &FactReadControl,
        ) -> FactStoreResult<ProjectMemoryFactPageV1>;
        fn search_project_memory_facts(
            query: ProjectMemoryFactSearchQuery,
            read_control: &FactReadControl,
        ) -> FactStoreResult<ProjectMemoryFactSearchPageV1>;
        fn probe_project_memory_facts(
            query: ProjectMemoryFactSearchQuery,
            read_control: &FactReadControl,
        ) -> FactStoreResult<ProjectMemoryFactSearchPageV1>;
        fn related_project_memory_facts(
            query: ProjectMemoryFactSearchQuery,
            read_control: &FactReadControl,
        ) -> FactStoreResult<ProjectMemoryFactSearchPageV1>;
        fn reason_project_memory_facts(
            query: ProjectMemoryFactSearchQuery,
            read_control: &FactReadControl,
        ) -> FactStoreResult<ProjectMemoryFactSearchPageV1>;
        fn find_project_memory_contradictions(
            query: ProjectMemoryFactContradictionQueryV1,
            read_control: &FactReadControl,
        ) -> FactStoreResult<ProjectMemoryFactContradictionPageV1>;
        fn get_project_memory_fact(
            target: ProjectMemoryFactIdV1,
            read_control: &FactReadControl,
        ) -> FactStoreResult<Option<ProjectMemoryFactProjectionV1>>;
        fn project_memory_fact_history(
            query: ProjectMemoryFactHistoryQueryV1,
            read_control: &FactReadControl,
        ) -> FactStoreResult<ProjectMemoryFactHistoryV1>;
        fn project_memory_status(
            owner: FactOwnerV1,
            read_control: &FactReadControl,
        ) -> FactStoreResult<ProjectMemoryMemoryStatusV1>;
        fn inspect_project_memory_fact(
            target: ProjectMemoryFactIdV1,
            read_control: &FactReadControl,
        ) -> FactStoreResult<Option<ProjectMemoryFactInspectionV1>>;
        fn add_project_memory_fact(
            request: ProjectMemoryFactAddCommandV1,
            write_control: &FactWriteControl,
        ) -> FactStoreResult<ProjectMemoryFactAddOutcomeV1>;
        fn update_project_memory_fact(
            request: ProjectMemoryFactUpdateCommandV1,
            write_control: &FactWriteControl,
        ) -> FactStoreResult<ProjectMemoryFactUpdateOutcomeV1>;
        fn remove_project_memory_fact(
            request: ProjectMemoryFactRemoveCommandV1,
            write_control: &FactWriteControl,
        ) -> FactStoreResult<ProjectMemoryFactRemoveOutcomeV1>;
        fn record_project_memory_fact_feedback(
            request: ProjectMemoryFactFeedbackCommandV1,
            write_control: &FactWriteControl,
        ) -> FactStoreResult<ProjectMemoryFactFeedbackOutcomeV1>;
        fn project_memory_fact_feedback_history(
            query: ProjectMemoryFactFeedbackHistoryQueryV1,
            read_control: &FactReadControl,
        ) -> FactStoreResult<ProjectMemoryFactFeedbackHistoryV1>;
        fn find_project_memory_fact_by_content_digest(
            query: ProjectMemoryFactContentDigestQueryV1,
            read_control: &FactReadControl,
        ) -> FactStoreResult<Option<ProjectMemoryFactProjectionV1>>;
        fn apply_project_memory_fact_curation(
            request: ProjectMemoryFactCurationBatchV1,
            write_control: &FactWriteControl,
        ) -> FactStoreResult<ProjectMemoryFactCurationReceiptV1>;
        fn merge_project_memory_facts(
            request: ProjectMemoryFactMergeCommandV1,
            write_control: &FactWriteControl,
        ) -> FactStoreResult<ProjectMemoryFactMergeOutcomeV1>;
        fn dashboard_project_memory_overview(
            query: ProjectMemoryDashboardMemoryOverviewQueryV1,
            read_control: &FactReadControl,
        ) -> FactStoreResult<ProjectMemoryDashboardMemoryOverviewV1>;
        fn dashboard_project_memory_fact_detail(
            query: ProjectMemoryDashboardFactDetailQueryV1,
            read_control: &FactReadControl,
        ) -> FactStoreResult<Option<ProjectMemoryDashboardFactDetailV1>>;
        fn dashboard_project_memory_vector_points(
            query: ProjectMemoryDashboardVectorPointsQueryV1,
            read_control: &FactReadControl,
        ) -> FactStoreResult<Vec<ProjectMemoryDashboardVectorPointV1>>;
        fn dashboard_project_memory_oplog(
            query: ProjectMemoryDashboardOplogQueryV1,
            read_control: &FactReadControl,
        ) -> FactStoreResult<Vec<ProjectMemoryDashboardOplogEntryV1>>;
        fn record_project_memory_fact_retrieval(
            request: ProjectMemoryFactRetrievalCommandV1,
            write_control: &FactWriteControl,
        ) -> FactStoreResult<ProjectMemoryFactRetrievalOutcomeV1>;
        fn apply_project_memory_automatic_fact(
            apply_id: ProvenanceId,
            request: ProjectMemoryFactAddCommandV1,
            evidence: ProjectMemoryAutomaticFactEvidenceV1,
            write_control: &FactWriteControl,
        ) -> FactStoreResult<ProjectMemoryAutomaticFactApplyResultV1>;
        fn get_project_memory_automatic_fact_receipt(
            owner: FactOwnerV1,
            apply_id: ProvenanceId,
            read_control: &FactReadControl,
        ) -> FactStoreResult<Option<ProjectMemoryAutomaticFactReceiptV1>>;
        fn list_project_memory_automatic_fact_receipts(
            owner: FactOwnerV1,
            state: Option<ProjectMemoryAutomaticFactStateV1>,
            after_apply_id: Option<ProvenanceId>,
            limit: usize,
            read_control: &FactReadControl,
        ) -> FactStoreResult<ProjectMemoryAutomaticFactReceiptPageV1>;
        fn project_memory_automation_run_receipts(
            owner: FactOwnerV1,
            run_id: RunId,
            read_control: &FactReadControl,
        ) -> FactStoreResult<ProjectMemoryAutomationRunReceiptsV1>;
    }
}

impl ProjectMemoryGraphStore for ProjectFactStore<'_> {
    delegate_fact_store_methods! {
        fn project_memory_graph(
            query: ProjectMemoryGraphQueryV1,
            read_control: &FactReadControl,
        ) -> FactStoreResult<ProjectMemoryGraphPageV1>;
    }
}

#[cfg(test)]
#[path = "fact_response_metadata_test.rs"]
mod fact_response_metadata_test;

#[cfg(test)]
mod search_tests;
