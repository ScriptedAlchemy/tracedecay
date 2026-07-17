//! Database-backed authority for append-only facts, evidence, and provenance.

use std::collections::BTreeSet;
use std::error::Error;
use std::future::Future;
use std::pin::Pin;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::db::{
    Database, MemoryV2BackfillBatchOutcome, MemoryV2CutoverOutcome, MemoryV2CutoverReceipt,
    MemoryV2FeedbackHistoryRepairBatchOutcome,
};
use crate::memory::encoding::HolographicEncoder;
use crate::memory::entities::normalize_entity;
use crate::privacy::{
    MemoryFactSanitizationV1, sanitize_memory_fact_payload, sanitize_provider_metadata_text,
};
use libsql::{Transaction, params};
use serde::{Serialize, de::DeserializeOwned};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tracedecay_domain::{
    ActorId, Confidence, FactAssertionId, FactAssertionKindV1, FactAssertionV1, FactCategoryV1,
    FactCurationActionV1, FactEventId, FactEvidenceId, FactId, FactIdentityMaterialV1,
    FactIdentitySourceV1, FactLineageEventKindV1, FactLineageEventV1, FactOwnerV1, FactPayloadV1,
    LegacyFactMappingV1, LocatorDigest, PayloadAccessState, ProvenanceId, RetentionClass,
    RetrievalAnchorId, RetrievalAnchorRecordV2, SanitizerDispositionV1, SourceStoreId, UtcMicros,
    VectorWatermark,
};
use tracedecay_store::{
    CompatibilityDashboardFactDetailQueryV1, CompatibilityDashboardFactDetailV1,
    CompatibilityDashboardMemoryOverviewQueryV1, CompatibilityDashboardMemoryOverviewV1,
    CompatibilityDashboardOplogEntryV1, CompatibilityDashboardOplogQueryV1,
    CompatibilityDashboardVectorPointV1, CompatibilityDashboardVectorPointsQueryV1,
    CompatibilityFactAddAliasV1, CompatibilityFactAddCommandV1, CompatibilityFactAddDispositionV1,
    CompatibilityFactAddOutcomeV1, CompatibilityFactAvailabilityV1,
    CompatibilityFactContentDigestQueryV1, CompatibilityFactContradictionPageV1,
    CompatibilityFactContradictionQueryV1, CompatibilityFactContradictionV1,
    CompatibilityFactCurationBatchV1, CompatibilityFactCurationOperationV1,
    CompatibilityFactCurationReceiptV1, CompatibilityFactFeedbackActionV1,
    CompatibilityFactFeedbackCommandV1, CompatibilityFactFeedbackDetailsAvailabilityV1,
    CompatibilityFactFeedbackHistoryEntryV1, CompatibilityFactFeedbackHistoryQueryV1,
    CompatibilityFactFeedbackHistoryV1, CompatibilityFactFeedbackOutcomeV1,
    CompatibilityFactHistoryQueryV1, CompatibilityFactHistoryV1, CompatibilityFactIdV1,
    CompatibilityFactInspectionV1, CompatibilityFactLinkV1, CompatibilityFactListQueryV1,
    CompatibilityFactMappingV1, CompatibilityFactMergeCommandV1, CompatibilityFactMergeEntitiesV1,
    CompatibilityFactMergeOutcomeV1, CompatibilityFactNormalizeTagsV1, CompatibilityFactPageV1,
    CompatibilityFactProjectionV1, CompatibilityFactProposalImportReceiptV1,
    CompatibilityFactProposalImportV1, CompatibilityFactProposalPageV1,
    CompatibilityFactProposalPromotionDispositionV1, CompatibilityFactProposalPromotionResultV1,
    CompatibilityFactProposalPromotionV1, CompatibilityFactProposalRecordV1,
    CompatibilityFactProposalRevisionV1, CompatibilityFactProposalStateV1,
    CompatibilityFactRelationV1, CompatibilityFactRemoveCommandV1,
    CompatibilityFactRemoveOutcomeV1, CompatibilityFactRepairVectorV1,
    CompatibilityFactRetrievalCommandV1, CompatibilityFactSearchCursorV1,
    CompatibilityFactSearchHitV1, CompatibilityFactSearchKindV1, CompatibilityFactSearchPageV1,
    CompatibilityFactSearchQuery, CompatibilityFactSearchScoresV1, CompatibilityFactSourceV1,
    CompatibilityFactStatusV1, CompatibilityFactTargetV1, CompatibilityFactTelemetryV1,
    CompatibilityFactUnavailableV1, CompatibilityFactUpdateCommandV1,
    CompatibilityFactUpdateOutcomeV1, CompatibilityFactV1, CompatibilityFeedbackRepairProgressV1,
    CompatibilityLegacyMemoryCutoverCommandV1, CompatibilityLegacyMemoryCutoverProgressV1,
    CompatibilityMemoryAlgebraV1, CompatibilityMemoryFeedbackFunnelV1,
    CompatibilityMemoryRepairCommandV1, CompatibilityMemoryRepairStatsV1,
    CompatibilityMemoryStatusV1, CompatibilityProjectionStateV1, CurrentFactsQuery, FactAsOfQuery,
    FactCommitConflict, FactCommitOutcome, FactCommitReceipt, FactCompatibilityResult,
    FactCompatibilityStore, FactCompatibilityStoreError, FactCurrentQuery, FactLineageCursor,
    FactLineageQuery, FactProposalPromotionStateV1, FactProposalStore, FactProposalStoreError,
    FactStore, FactStoreError, FactStoreResult, FactWriteBatch, LegacyFactQuery,
    PromoteFactProposal, PromoteFactProposalOutcome, RetrievalAnchorQuery, StoredFactV1,
};

const COMMIT_OPERATION: &str = "commit canonical memory fact";
const QUERY_OPERATION: &str = "query canonical memory facts";
const PROMOTE_OPERATION: &str = "promote canonical memory proposal";
const COMPATIBILITY_READ_OPERATION: &str = "read compatibility memory facts";
const COMPATIBILITY_WRITE_OPERATION: &str = "write compatibility memory facts";
const DEFAULT_TRUST: f64 = 0.5;
const COMPATIBILITY_RETENTION_CLASS: &str = "compatibility-runtime-v1";
const COMPATIBILITY_SOURCE_STORE: &str = "legacy-memory-v1";
const COMPATIBILITY_LEGACY_CUTOVER_BATCH_SIZE: i64 = 500;
/// Per-repair-pass batch caps. The daemon scheduler treats a pass that hits
/// either cap as incomplete and keeps ticking rather than going idle with a
/// converging backlog.
pub(crate) const COMPATIBILITY_REPAIR_VECTOR_BATCH: i64 = 512;
pub(crate) const COMPATIBILITY_REPAIR_BANK_BATCH: i64 = 32;

/// Upper bound on empty backfill-phase transitions drained inside one cutover
/// pass. The phase walk is feedback → oplog → facts → awaiting_cutover, so a
/// small bound comfortably covers draining every empty phase in a single tick
/// while still guaranteeing the loop terminates.
const COMPATIBILITY_LEGACY_CUTOVER_MAX_EMPTY_PHASE_DRAIN: usize = 8;

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

    async fn commit_batch(&self, batch: &FactWriteBatch) -> FactStoreResult<FactCommitOutcome> {
        let transaction = self
            .db
            .begin_write_transaction(COMMIT_OPERATION)
            .await
            .map_err(|error| storage_error(COMMIT_OPERATION, error))?;
        let attempt = match commit_fact_tx(&transaction, batch).await {
            Ok(attempt) => attempt,
            Err(error) => {
                return match transaction.rollback().await {
                    Ok(()) => Err(error),
                    Err(rollback) => Err(storage_error(
                        COMMIT_OPERATION,
                        std::io::Error::other(format!(
                            "{error}; transaction rollback also failed and writer connection was retired: {rollback}"
                        )),
                    )),
                };
            }
        };
        if attempt.wrote {
            transaction
                .commit()
                .await
                .map_err(|error| storage_error(COMMIT_OPERATION, error))?;
        } else {
            transaction
                .rollback()
                .await
                .map_err(|error| storage_error(COMMIT_OPERATION, error))?;
        }
        Ok(attempt.outcome)
    }

    async fn compatibility_read<T>(
        &self,
        work: impl for<'tx> FnOnce(
            &'tx Transaction,
        ) -> Pin<
            Box<dyn Future<Output = FactCompatibilityResult<T>> + Send + 'tx>,
        >,
    ) -> FactCompatibilityResult<T> {
        let snapshot = self
            .db
            .begin_isolated_read_snapshot(COMPATIBILITY_READ_OPERATION)
            .await
            .map_err(|error| {
                FactCompatibilityStoreError::Store(storage_error(
                    COMPATIBILITY_READ_OPERATION,
                    error,
                ))
            })?;
        let result = work(&snapshot).await;
        match result {
            Ok(value) => {
                snapshot.commit().await.map_err(|error| {
                    FactCompatibilityStoreError::Store(storage_error(
                        COMPATIBILITY_READ_OPERATION,
                        error,
                    ))
                })?;
                Ok(value)
            }
            Err(error) => match snapshot.rollback().await {
                Ok(()) => Err(error),
                Err(rollback) => Err(FactCompatibilityStoreError::Store(storage_error(
                    COMPATIBILITY_READ_OPERATION,
                    std::io::Error::other(format!(
                        "{error}; read snapshot rollback also failed: {rollback}"
                    )),
                ))),
            },
        }
    }

    async fn compatibility_write<T>(
        &self,
        work: impl for<'tx> FnOnce(
            &'tx Transaction,
        ) -> Pin<
            Box<dyn Future<Output = FactCompatibilityResult<T>> + Send + 'tx>,
        >,
    ) -> FactCompatibilityResult<T> {
        let transaction = self
            .db
            .begin_write_transaction(COMPATIBILITY_WRITE_OPERATION)
            .await
            .map_err(|error| {
                FactCompatibilityStoreError::Store(storage_error(
                    COMPATIBILITY_WRITE_OPERATION,
                    error,
                ))
            })?;
        let result = work(&transaction).await;
        match result {
            Ok(value) => {
                transaction.commit().await.map_err(|error| {
                    FactCompatibilityStoreError::Store(storage_error(
                        COMPATIBILITY_WRITE_OPERATION,
                        error,
                    ))
                })?;
                Ok(value)
            }
            Err(error) => match transaction.rollback().await {
                Ok(()) => Err(error),
                Err(rollback) => Err(FactCompatibilityStoreError::Store(storage_error(
                    COMPATIBILITY_WRITE_OPERATION,
                    std::io::Error::other(format!(
                        "{error}; transaction rollback also failed: {rollback}"
                    )),
                ))),
            },
        }
    }
}

async fn advance_compatibility_feedback_history_repair_tx(
    db: &Database,
    transaction: &Transaction,
    owner: &FactOwnerV1,
) -> FactCompatibilityResult<CompatibilityFeedbackRepairProgressV1> {
    let source_store_id = compatibility_source_store_id()?;
    let outcome = db
        .repair_memory_v2_feedback_history_batch_in_transaction(
            transaction,
            owner,
            &source_store_id,
            512,
        )
        .await
        .map_err(|error| {
            FactCompatibilityStoreError::Store(storage_error(COMPATIBILITY_WRITE_OPERATION, error))
        })?;
    match outcome {
        MemoryV2FeedbackHistoryRepairBatchOutcome::NotRequired => {
            Ok(CompatibilityFeedbackRepairProgressV1::NotRequired)
        }
        MemoryV2FeedbackHistoryRepairBatchOutcome::Advanced { processed } => {
            let progress = db
                .feedback_history_repair_progress_in_transaction(
                    transaction,
                    owner,
                    &source_store_id,
                )
                .await
                .map_err(|error| {
                    FactCompatibilityStoreError::Store(storage_error(
                        COMPATIBILITY_WRITE_OPERATION,
                        error,
                    ))
                })?
                .ok_or_else(|| {
                    storage_message(
                        COMPATIBILITY_WRITE_OPERATION,
                        "feedback history repair progress disappeared after advancement",
                    )
                })?;
            if progress.complete {
                return Err(storage_message(
                    COMPATIBILITY_WRITE_OPERATION,
                    "feedback history repair advanced but reported completion",
                )
                .into());
            }
            let remaining = u64::try_from(
                progress
                    .feedback_frontier
                    .saturating_sub(progress.feedback_cursor),
            )
            .unwrap_or(0);
            Ok(CompatibilityFeedbackRepairProgressV1::Incomplete {
                processed: processed as u64,
                remaining: Some(remaining),
            })
        }
        MemoryV2FeedbackHistoryRepairBatchOutcome::Complete { processed } => {
            Ok(CompatibilityFeedbackRepairProgressV1::Complete {
                processed: processed as u64,
            })
        }
    }
}

async fn finish_read_snapshot<T>(
    snapshot: Transaction,
    result: FactStoreResult<T>,
) -> FactStoreResult<T> {
    match result {
        Ok(value) => {
            snapshot
                .commit()
                .await
                .map_err(|error| storage_error(QUERY_OPERATION, error))?;
            Ok(value)
        }
        Err(error) => match snapshot.rollback().await {
            Ok(()) => Err(error),
            Err(rollback) => Err(storage_error(
                QUERY_OPERATION,
                std::io::Error::other(format!(
                    "{error}; read snapshot rollback also failed: {rollback}"
                )),
            )),
        },
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
        let source_store_id = compatibility_source_store_id()?;
        let frontiers = self
            .db
            .load_or_capture_memory_v2_frontiers(request.owner(), &source_store_id)
            .await
            .map_err(|error| {
                FactCompatibilityStoreError::Store(storage_error(
                    COMPATIBILITY_WRITE_OPERATION,
                    error,
                ))
            })?;
        // Drain empty backfill phases within a single cutover pass so a fresh
        // (or fully imported) owner reaches finalization on one tick instead of
        // spending an idle tick per empty phase. The bounded feedback → oplog →
        // facts → awaiting_cutover walk means at most a handful of empty-phase
        // transitions before a batch does real work or the frontier is drained;
        // real work still commits exactly one bounded batch per pass.
        let mut total_processed = 0_u64;
        for _ in 0..COMPATIBILITY_LEGACY_CUTOVER_MAX_EMPTY_PHASE_DRAIN {
            match self
                .db
                .backfill_memory_v2_batch(
                    request.owner(),
                    &source_store_id,
                    frontiers,
                    COMPATIBILITY_LEGACY_CUTOVER_BATCH_SIZE,
                )
                .await
                .map_err(|error| {
                    FactCompatibilityStoreError::Store(storage_error(
                        COMPATIBILITY_WRITE_OPERATION,
                        error,
                    ))
                })? {
                MemoryV2BackfillBatchOutcome::Advanced { processed } => {
                    total_processed = total_processed.saturating_add(processed as u64);
                    if processed > 0 {
                        return Ok(CompatibilityLegacyMemoryCutoverProgressV1::Incomplete {
                            processed: total_processed,
                        });
                    }
                    // Empty phase transition; keep draining within this pass.
                }
                MemoryV2BackfillBatchOutcome::AwaitingCutover => {
                    let receipt = MemoryV2CutoverReceipt::new(
                        request.receipt_id().clone(),
                        request.owner().clone(),
                        source_store_id,
                        frontiers,
                        compatibility_now()?,
                    )
                    .map_err(|error| {
                        FactCompatibilityStoreError::Store(storage_error(
                            COMPATIBILITY_WRITE_OPERATION,
                            error,
                        ))
                    })?;
                    return match self.db.finalize_memory_v2_cutover(&receipt).await.map_err(
                        |error| {
                            FactCompatibilityStoreError::Store(storage_error(
                                COMPATIBILITY_WRITE_OPERATION,
                                error,
                            ))
                        },
                    )? {
                        MemoryV2CutoverOutcome::TailPending(_) => {
                            Ok(CompatibilityLegacyMemoryCutoverProgressV1::Incomplete {
                                processed: total_processed,
                            })
                        }
                        MemoryV2CutoverOutcome::Complete => {
                            Ok(CompatibilityLegacyMemoryCutoverProgressV1::Complete)
                        }
                    };
                }
            }
        }
        // The bounded phase walk did not settle this pass; report incomplete so
        // the daemon retries rather than spinning here unbounded.
        Ok(CompatibilityLegacyMemoryCutoverProgressV1::Incomplete {
            processed: total_processed,
        })
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

fn compatibility_projection_state(value: &str) -> FactStoreResult<CompatibilityProjectionStateV1> {
    match value {
        "ready" => Ok(CompatibilityProjectionStateV1::Ready),
        "rebuilding" => Ok(CompatibilityProjectionStateV1::Rebuilding),
        "stale" => Ok(CompatibilityProjectionStateV1::Stale),
        "unavailable" => Ok(CompatibilityProjectionStateV1::Unavailable),
        _ => Err(storage_message(
            QUERY_OPERATION,
            format!("unknown compatibility projection state {value:?}"),
        )),
    }
}

fn compatibility_unavailable(
    access: Option<PayloadAccessState>,
) -> CompatibilityFactAvailabilityV1 {
    match access {
        Some(PayloadAccessState::Deleted) => CompatibilityFactAvailabilityV1::Deleted,
        Some(PayloadAccessState::Quarantined) => CompatibilityFactAvailabilityV1::Quarantined,
        _ => CompatibilityFactAvailabilityV1::Unavailable,
    }
}

fn nonnegative_u64(value: i64, field: &'static str) -> FactStoreResult<u64> {
    u64::try_from(value).map_err(|_| {
        storage_message(
            QUERY_OPERATION,
            format!("compatibility {field} must be non-negative"),
        )
    })
}

fn compatibility_category_label(category: FactCategoryV1) -> &'static str {
    match category {
        FactCategoryV1::General => "general",
        FactCategoryV1::UserPref => "user_pref",
        FactCategoryV1::Project => "project",
        FactCategoryV1::Tool => "tool",
        FactCategoryV1::Decision => "decision",
        FactCategoryV1::CodeArea => "code_area",
    }
}

fn compatibility_now() -> FactStoreResult<UtcMicros> {
    let elapsed = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| storage_error(COMPATIBILITY_WRITE_OPERATION, error))?;
    let micros = i64::try_from(elapsed.as_micros()).map_err(|_| {
        storage_message(
            COMPATIBILITY_WRITE_OPERATION,
            "compatibility clock exceeds supported timestamp range",
        )
    })?;
    Ok(UtcMicros(micros))
}

fn compatibility_source_store_id() -> FactStoreResult<SourceStoreId> {
    SourceStoreId::new(COMPATIBILITY_SOURCE_STORE.to_owned()).map_err(FactStoreError::from)
}

async fn compatibility_fact_status_tx(
    transaction: &Transaction,
    owner: &FactOwnerV1,
    fact_id: &FactId,
) -> FactStoreResult<Option<CompatibilityFactStatusV1>> {
    let key = OwnerKey::new(owner)?;
    let mut rows = transaction
        .query(
            "SELECT current_facts.payload_access, current_facts.projection_state,
                    current_facts.updated_at, current_facts.vector_watermark_json
             FROM memory_v2_current_facts AS current_facts
             JOIN memory_v2_facts AS facts
               ON facts.fact_id = current_facts.fact_id
              AND facts.owner_kind = current_facts.owner_kind
              AND facts.project_id = current_facts.project_id
             WHERE current_facts.fact_id = ?1
               AND current_facts.owner_kind = ?2
               AND current_facts.project_id = ?3
               AND facts.owner_json = ?4",
            params![
                fact_id.as_str(),
                key.kind,
                key.project_id.as_str(),
                key.json.as_str(),
            ],
        )
        .await
        .map_err(|error| storage_error(QUERY_OPERATION, error))?;
    let Some(row) = rows
        .next()
        .await
        .map_err(|error| storage_error(QUERY_OPERATION, error))?
    else {
        return Ok(None);
    };
    let access = parse_payload_access(&row_string(&row, 0, QUERY_OPERATION)?)?;
    let state = compatibility_projection_state(&row_string(&row, 1, QUERY_OPERATION)?)?;
    let watermark = row_optional_string(&row, 3, QUERY_OPERATION)?
        .as_deref()
        .map(|value| from_json::<VectorWatermark>(value, QUERY_OPERATION))
        .transpose()?;
    CompatibilityFactStatusV1::new(
        owner.clone(),
        Some(fact_id.clone()),
        Some(access),
        state,
        Some(UtcMicros(row_i64(&row, 2, QUERY_OPERATION)?)),
        watermark,
    )
    .map(Some)
}

async fn compatibility_legacy_mapping_tx(
    transaction: &Transaction,
    owner: &FactOwnerV1,
    fact_id: &FactId,
) -> FactStoreResult<Option<LegacyFactMappingV1>> {
    let key = OwnerKey::new(owner)?;
    let source_store_id = compatibility_source_store_id()?;
    let mut rows = transaction
        .query(
            "SELECT mapping_json, owner_json FROM memory_v2_legacy_map
             WHERE owner_kind = ?1 AND project_id = ?2 AND fact_id = ?3
               AND source_store_id = ?4",
            params![
                key.kind,
                key.project_id.as_str(),
                fact_id.as_str(),
                source_store_id.as_str(),
            ],
        )
        .await
        .map_err(|error| storage_error(QUERY_OPERATION, error))?;
    let Some(row) = rows
        .next()
        .await
        .map_err(|error| storage_error(QUERY_OPERATION, error))?
    else {
        return Ok(None);
    };
    if row_string(&row, 1, QUERY_OPERATION)? != key.json {
        return Err(FactStoreError::OwnerMismatch);
    }
    let mapping =
        from_json::<LegacyFactMappingV1>(&row_string(&row, 0, QUERY_OPERATION)?, QUERY_OPERATION)?;
    if mapping.owner() != owner || mapping.fact_id() != fact_id {
        return Err(storage_message(
            QUERY_OPERATION,
            "compatibility legacy mapping identity mismatch",
        ));
    }
    Ok(Some(mapping))
}

async fn compatibility_projection_metadata_tx(
    transaction: &Transaction,
    owner: &FactOwnerV1,
    fact_id: &FactId,
    mapping: Option<&LegacyFactMappingV1>,
) -> FactStoreResult<(
    CompatibilityFactSourceV1,
    Option<String>,
    CompatibilityFactTelemetryV1,
)> {
    let key = OwnerKey::new(owner)?;
    let mut rows = transaction
        .query(
            "SELECT facts.identity_json, facts.created_at,
                    current_facts.retrieval_count, current_facts.access_count,
                    current_facts.helpful_count, current_facts.unhelpful_count,
                    current_facts.updated_at, current_facts.last_retrieved_at,
                    current_facts.last_recalled_at, current_facts.last_feedback_at
             FROM memory_v2_facts AS facts
             JOIN memory_v2_current_facts AS current_facts
               ON current_facts.fact_id = facts.fact_id
              AND current_facts.owner_kind = facts.owner_kind
              AND current_facts.project_id = facts.project_id
             WHERE facts.fact_id = ?1 AND facts.owner_kind = ?2
               AND facts.project_id = ?3 AND facts.owner_json = ?4",
            params![
                fact_id.as_str(),
                key.kind,
                key.project_id.as_str(),
                key.json.as_str(),
            ],
        )
        .await
        .map_err(|error| storage_error(QUERY_OPERATION, error))?;
    let row = rows
        .next()
        .await
        .map_err(|error| storage_error(QUERY_OPERATION, error))?
        .ok_or_else(|| {
            storage_message(QUERY_OPERATION, "compatibility fact metadata is missing")
        })?;
    let identity = from_json::<FactIdentityMaterialV1>(
        &row_string(&row, 0, QUERY_OPERATION)?,
        QUERY_OPERATION,
    )?;
    if identity.owner() != owner || FactId::derive(&identity)? != *fact_id {
        return Err(storage_message(
            QUERY_OPERATION,
            "compatibility fact identity material mismatch",
        ));
    }
    let source_label = match mapping {
        Some(mapping) => {
            let mut source_rows = transaction
                .query(
                    "SELECT source FROM memory_facts WHERE fact_id = ?1",
                    params![mapping.legacy_fact_id()],
                )
                .await
                .map_err(|error| storage_error(QUERY_OPERATION, error))?;
            source_rows
                .next()
                .await
                .map_err(|error| storage_error(QUERY_OPERATION, error))?
                .map(|row| row_optional_string(&row, 0, QUERY_OPERATION))
                .transpose()?
                .flatten()
        }
        None => None,
    };
    let telemetry = CompatibilityFactTelemetryV1::new(
        nonnegative_u64(row_i64(&row, 2, QUERY_OPERATION)?, "retrieval count")?,
        nonnegative_u64(row_i64(&row, 3, QUERY_OPERATION)?, "access count")?,
        nonnegative_u64(row_i64(&row, 4, QUERY_OPERATION)?, "helpful count")?,
        nonnegative_u64(row_i64(&row, 5, QUERY_OPERATION)?, "unhelpful count")?,
        UtcMicros(row_i64(&row, 1, QUERY_OPERATION)?),
        UtcMicros(row_i64(&row, 6, QUERY_OPERATION)?),
        row_optional_i64(&row, 7, QUERY_OPERATION)?.map(UtcMicros),
        row_optional_i64(&row, 8, QUERY_OPERATION)?.map(UtcMicros),
        row_optional_i64(&row, 9, QUERY_OPERATION)?.map(UtcMicros),
    )?;
    Ok((
        CompatibilityFactSourceV1::Canonical(identity.source().clone()),
        source_label,
        telemetry,
    ))
}

async fn load_compatibility_projection_tx(
    transaction: &Transaction,
    owner: &FactOwnerV1,
    fact_id: &FactId,
) -> FactStoreResult<Option<CompatibilityFactProjectionV1>> {
    let Some(status) = compatibility_fact_status_tx(transaction, owner, fact_id).await? else {
        return Ok(None);
    };
    let mapping = compatibility_legacy_mapping_tx(transaction, owner, fact_id).await?;
    let compatibility_id = CompatibilityFactIdV1::new(owner.clone(), fact_id.clone())?;
    let mapping = CompatibilityFactMappingV1::new(compatibility_id.clone(), mapping)?;
    let key = OwnerKey::new(owner)?;
    let Some(stored) = load_current_fact_tx(transaction, &key, owner, fact_id).await? else {
        return CompatibilityFactUnavailableV1::new(
            compatibility_id,
            compatibility_unavailable(status.payload_access()),
            status,
        )
        .map(CompatibilityFactProjectionV1::Unavailable)
        .map(Some);
    };
    if stored.payload().is_none() {
        return CompatibilityFactUnavailableV1::new(
            compatibility_id,
            compatibility_unavailable(status.payload_access()),
            status,
        )
        .map(CompatibilityFactProjectionV1::Unavailable)
        .map(Some);
    }
    let (source, source_label, telemetry) =
        compatibility_projection_metadata_tx(transaction, owner, fact_id, mapping.legacy_mapping())
            .await?;
    CompatibilityFactV1::new(stored, mapping, source, telemetry)?
        .with_source_label(source_label)
        .map(Box::new)
        .map(CompatibilityFactProjectionV1::Available)
        .map(Some)
}

async fn resolve_compatibility_target_tx(
    transaction: &Transaction,
    target: &CompatibilityFactTargetV1,
) -> FactStoreResult<Option<FactId>> {
    match target {
        CompatibilityFactTargetV1::Canonical(target) => Ok(Some(target.fact_id().clone())),
        CompatibilityFactTargetV1::Legacy(query) => {
            resolve_legacy_fact_tx(transaction, query).await
        }
    }
}

async fn list_compatibility_facts_tx(
    transaction: &Transaction,
    query: &CompatibilityFactListQueryV1,
) -> FactCompatibilityResult<CompatibilityFactPageV1> {
    let key = OwnerKey::new(query.owner())?;
    let category = query.category().map(compatibility_category_label);
    let min_trust = query.min_trust().map(Confidence::as_f64);
    let fetch_limit = i64::try_from(query.limit().saturating_add(1)).map_err(|_| {
        FactStoreError::InvalidQueryLimit {
            limit: query.limit(),
            max: usize::MAX,
        }
    })?;
    let mut rows = match (query.after_fact_id(), category) {
        (Some(after), Some(category)) => {
            transaction
                .query(
                    "SELECT current_facts.fact_id
                 FROM memory_v2_current_facts AS current_facts
                 JOIN memory_v2_facts AS facts
                   ON facts.fact_id = current_facts.fact_id
                  AND facts.owner_kind = current_facts.owner_kind
                  AND facts.project_id = current_facts.project_id
                 JOIN memory_v2_assertion_payloads AS payloads
                   ON payloads.assertion_id = current_facts.active_assertion_id
                  AND payloads.fact_id = current_facts.fact_id
                  AND payloads.owner_kind = current_facts.owner_kind
                  AND payloads.project_id = current_facts.project_id
                 WHERE current_facts.owner_kind = ?1 AND current_facts.project_id = ?2
                   AND facts.owner_json = ?3 AND current_facts.fact_id > ?4
                   AND current_facts.active_assertion_id IS NOT NULL
                   AND current_facts.trust_score >= ?5
                   AND json_extract(payloads.payload_json, '$.category') = ?6
                 ORDER BY current_facts.fact_id ASC LIMIT ?7",
                    params![
                        key.kind,
                        key.project_id.as_str(),
                        key.json.as_str(),
                        after.as_str(),
                        min_trust.unwrap_or(0.0),
                        category,
                        fetch_limit,
                    ],
                )
                .await
        }
        (Some(after), None) => {
            transaction
                .query(
                    "SELECT current_facts.fact_id
                 FROM memory_v2_current_facts AS current_facts
                 JOIN memory_v2_facts AS facts
                   ON facts.fact_id = current_facts.fact_id
                  AND facts.owner_kind = current_facts.owner_kind
                  AND facts.project_id = current_facts.project_id
                 WHERE current_facts.owner_kind = ?1 AND current_facts.project_id = ?2
                   AND facts.owner_json = ?3 AND current_facts.fact_id > ?4
                   AND current_facts.active_assertion_id IS NOT NULL
                   AND current_facts.trust_score >= ?5
                 ORDER BY current_facts.fact_id ASC LIMIT ?6",
                    params![
                        key.kind,
                        key.project_id.as_str(),
                        key.json.as_str(),
                        after.as_str(),
                        min_trust.unwrap_or(0.0),
                        fetch_limit,
                    ],
                )
                .await
        }
        (None, Some(category)) => {
            transaction
                .query(
                    "SELECT current_facts.fact_id
                 FROM memory_v2_current_facts AS current_facts
                 JOIN memory_v2_facts AS facts
                   ON facts.fact_id = current_facts.fact_id
                  AND facts.owner_kind = current_facts.owner_kind
                  AND facts.project_id = current_facts.project_id
                 JOIN memory_v2_assertion_payloads AS payloads
                   ON payloads.assertion_id = current_facts.active_assertion_id
                  AND payloads.fact_id = current_facts.fact_id
                  AND payloads.owner_kind = current_facts.owner_kind
                  AND payloads.project_id = current_facts.project_id
                 WHERE current_facts.owner_kind = ?1 AND current_facts.project_id = ?2
                   AND facts.owner_json = ?3 AND current_facts.active_assertion_id IS NOT NULL
                   AND current_facts.trust_score >= ?4
                   AND json_extract(payloads.payload_json, '$.category') = ?5
                 ORDER BY current_facts.fact_id ASC LIMIT ?6",
                    params![
                        key.kind,
                        key.project_id.as_str(),
                        key.json.as_str(),
                        min_trust.unwrap_or(0.0),
                        category,
                        fetch_limit,
                    ],
                )
                .await
        }
        (None, None) => {
            transaction
                .query(
                    "SELECT current_facts.fact_id
                 FROM memory_v2_current_facts AS current_facts
                 JOIN memory_v2_facts AS facts
                   ON facts.fact_id = current_facts.fact_id
                  AND facts.owner_kind = current_facts.owner_kind
                  AND facts.project_id = current_facts.project_id
                 WHERE current_facts.owner_kind = ?1 AND current_facts.project_id = ?2
                   AND facts.owner_json = ?3 AND current_facts.active_assertion_id IS NOT NULL
                   AND current_facts.trust_score >= ?4
                 ORDER BY current_facts.fact_id ASC LIMIT ?5",
                    params![
                        key.kind,
                        key.project_id.as_str(),
                        key.json.as_str(),
                        min_trust.unwrap_or(0.0),
                        fetch_limit,
                    ],
                )
                .await
        }
    }
    .map_err(|error| storage_error(QUERY_OPERATION, error))?;
    let mut fact_ids = Vec::new();
    while let Some(row) = rows
        .next()
        .await
        .map_err(|error| storage_error(QUERY_OPERATION, error))?
    {
        fact_ids.push(
            FactId::new(row_string(&row, 0, QUERY_OPERATION)?).map_err(FactStoreError::from)?,
        );
    }
    drop(rows);
    let has_more = fact_ids.len() > query.limit();
    fact_ids.truncate(query.limit());
    let mut facts = Vec::with_capacity(fact_ids.len());
    for fact_id in fact_ids {
        if let Some(fact) =
            load_compatibility_projection_tx(transaction, query.owner(), &fact_id).await?
        {
            facts.push(fact);
        }
    }
    let next = has_more
        .then(|| facts.last().map(|fact| fact.fact_id().clone()))
        .flatten();
    CompatibilityFactPageV1::new(query.owner().clone(), facts, next).map_err(Into::into)
}

async fn get_compatibility_fact_tx(
    transaction: &Transaction,
    target: &CompatibilityFactTargetV1,
) -> FactCompatibilityResult<Option<CompatibilityFactProjectionV1>> {
    let Some(fact_id) = resolve_compatibility_target_tx(transaction, target).await? else {
        return Ok(None);
    };
    load_compatibility_projection_tx(transaction, target.owner(), &fact_id)
        .await
        .map_err(Into::into)
}

fn compatibility_content_digest(content: &str) -> FactStoreResult<LocatorDigest> {
    LocatorDigest::new(format!(
        "sha256:{}",
        hex::encode(Sha256::digest(content.as_bytes()))
    ))
    .map_err(FactStoreError::from)
}

async fn find_compatibility_fact_by_content_digest_tx(
    transaction: &Transaction,
    query: &CompatibilityFactContentDigestQueryV1,
) -> FactCompatibilityResult<Option<CompatibilityFactProjectionV1>> {
    let key = OwnerKey::new(query.owner())?;
    let mut rows = transaction
        .query(
            "SELECT current_facts.fact_id, payloads.payload_json
             FROM memory_v2_current_facts AS current_facts
             JOIN memory_v2_facts AS facts
               ON facts.fact_id = current_facts.fact_id
              AND facts.owner_kind = current_facts.owner_kind
              AND facts.project_id = current_facts.project_id
             JOIN memory_v2_assertion_payloads AS payloads
               ON payloads.assertion_id = current_facts.active_assertion_id
              AND payloads.fact_id = current_facts.fact_id
              AND payloads.owner_kind = current_facts.owner_kind
              AND payloads.project_id = current_facts.project_id
             WHERE current_facts.owner_kind = ?1
               AND current_facts.project_id = ?2
               AND facts.owner_json = ?3
               AND current_facts.payload_access = 'eligible'
               AND current_facts.active_assertion_id IS NOT NULL
             ORDER BY current_facts.fact_id ASC",
            params![key.kind, key.project_id.as_str(), key.json.as_str()],
        )
        .await
        .map_err(|error| storage_error(COMPATIBILITY_READ_OPERATION, error))?;
    let mut matching_fact_id = None;
    while let Some(row) = rows
        .next()
        .await
        .map_err(|error| storage_error(COMPATIBILITY_READ_OPERATION, error))?
    {
        let payload = from_json::<FactPayloadV1>(
            &row_string(&row, 1, COMPATIBILITY_READ_OPERATION)?,
            COMPATIBILITY_READ_OPERATION,
        )?;
        if compatibility_content_digest(payload.content())? == *query.content_digest() {
            matching_fact_id = Some(
                FactId::new(row_string(&row, 0, COMPATIBILITY_READ_OPERATION)?)
                    .map_err(FactStoreError::from)?,
            );
            break;
        }
    }
    drop(rows);
    match matching_fact_id {
        Some(fact_id) => load_compatibility_projection_tx(transaction, query.owner(), &fact_id)
            .await
            .map_err(Into::into),
        None => Ok(None),
    }
}

async fn compatibility_fact_history_tx(
    transaction: &Transaction,
    query: &CompatibilityFactHistoryQueryV1,
) -> FactCompatibilityResult<CompatibilityFactHistoryV1> {
    let fact_id = resolve_compatibility_target_tx(transaction, query.target())
        .await?
        .ok_or_else(|| storage_message(QUERY_OPERATION, "compatibility fact target is missing"))?;
    let lineage = FactLineageQuery::new(
        query.target().owner().clone(),
        fact_id.clone(),
        query.after().cloned(),
        query.limit(),
    )?;
    let events = query_fact_lineage_tx(transaction, &lineage).await?;
    CompatibilityFactHistoryV1::new(query.target().owner().clone(), fact_id, events, None)
        .map_err(Into::into)
}

struct CompatibilitySanitizedPayload {
    payload: FactPayloadV1,
    access: PayloadAccessState,
}

fn compatibility_value_strings(value: &Value, field: &'static str) -> FactStoreResult<Vec<String>> {
    let values = value.as_array().ok_or_else(|| {
        storage_message(
            COMPATIBILITY_WRITE_OPERATION,
            format!("sanitized compatibility {field} is not an array"),
        )
    })?;
    values
        .iter()
        .map(|value| {
            value.as_str().map(ToOwned::to_owned).ok_or_else(|| {
                storage_message(
                    COMPATIBILITY_WRITE_OPERATION,
                    format!("sanitized compatibility {field} contains a non-string"),
                )
            })
        })
        .collect()
}

fn compatibility_payload_metadata(metadata: &Value) -> Value {
    let mut metadata = metadata.clone();
    if let Some(object) = metadata.as_object_mut() {
        object.remove("automation_run_id");
    }
    metadata
}

fn compatibility_sanitize_payload(
    content: &str,
    category: FactCategoryV1,
    tags: &[String],
    entities: &[String],
    metadata: &Value,
) -> FactStoreResult<Option<CompatibilitySanitizedPayload>> {
    let metadata = compatibility_payload_metadata(metadata);
    let sanitized = sanitize_memory_fact_payload(json!({
        "content": content,
        "category": compatibility_category_label(category),
        "tags": tags,
        "entities": entities,
        "metadata": metadata,
    }))
    .map_err(|error| storage_error(COMPATIBILITY_WRITE_OPERATION, error))?;
    let MemoryFactSanitizationV1::Durable { payload, receipt } = sanitized else {
        return Ok(None);
    };
    let content = payload
        .get("content")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            storage_message(
                COMPATIBILITY_WRITE_OPERATION,
                "sanitized compatibility content is missing",
            )
        })?
        .to_owned();
    let tags = compatibility_value_strings(
        payload.get("tags").ok_or_else(|| {
            storage_message(
                COMPATIBILITY_WRITE_OPERATION,
                "sanitized compatibility tags are missing",
            )
        })?,
        "tags",
    )?;
    let entities = compatibility_value_strings(
        payload.get("entities").ok_or_else(|| {
            storage_message(
                COMPATIBILITY_WRITE_OPERATION,
                "sanitized compatibility entities are missing",
            )
        })?,
        "entities",
    )?;
    let metadata = payload.get("metadata").cloned().ok_or_else(|| {
        storage_message(
            COMPATIBILITY_WRITE_OPERATION,
            "sanitized compatibility metadata is missing",
        )
    })?;
    let retention = RetentionClass::new(COMPATIBILITY_RETENTION_CLASS.to_owned())
        .map_err(FactStoreError::from)?;
    let fact_payload = FactPayloadV1::new(
        content, category, tags, entities, metadata, receipt, retention,
    )
    .map_err(FactStoreError::from)?;
    let access = match fact_payload.receipt().disposition() {
        SanitizerDispositionV1::Accepted => PayloadAccessState::Eligible,
        SanitizerDispositionV1::Redacted => PayloadAccessState::Redacted,
        SanitizerDispositionV1::Rejected | SanitizerDispositionV1::Quarantined => {
            return Err(storage_message(
                COMPATIBILITY_WRITE_OPERATION,
                "durable compatibility payload has a non-durable receipt disposition",
            ));
        }
    };
    Ok(Some(CompatibilitySanitizedPayload {
        payload: fact_payload,
        access,
    }))
}

fn compatibility_source_label(source: Option<&str>) -> FactStoreResult<String> {
    let source = source.unwrap_or("manual");
    sanitize_provider_metadata_text(source).ok_or_else(|| {
        storage_message(
            COMPATIBILITY_WRITE_OPERATION,
            "compatibility source is not eligible for persistence",
        )
    })
}

fn compatibility_legacy_timestamp(now: UtcMicros) -> i64 {
    now.0.div_euclid(1_000_000)
}

fn compatibility_mirror_vector(payload: &FactPayloadV1) -> FactStoreResult<Vec<u8>> {
    let encoder = HolographicEncoder::new();
    HolographicEncoder::serialize(&encoder.encode_fact(payload.content(), payload.entities()))
        .map_err(|error| storage_error(COMPATIBILITY_WRITE_OPERATION, error))
}

async fn compatibility_last_insert_rowid_tx(transaction: &Transaction) -> FactStoreResult<i64> {
    let mut rows = transaction
        .query("SELECT last_insert_rowid()", ())
        .await
        .map_err(|error| storage_error(COMPATIBILITY_WRITE_OPERATION, error))?;
    let row = rows
        .next()
        .await
        .map_err(|error| storage_error(COMPATIBILITY_WRITE_OPERATION, error))?
        .ok_or_else(|| {
            storage_message(
                COMPATIBILITY_WRITE_OPERATION,
                "compatibility last_insert_rowid returned no row",
            )
        })?;
    row_i64(&row, 0, COMPATIBILITY_WRITE_OPERATION)
}

async fn compatibility_mark_owner_banks_dirty_tx(
    db: &Database,
    transaction: &Transaction,
    owner: &FactOwnerV1,
    category: FactCategoryV1,
    updated_at: UtcMicros,
) -> FactStoreResult<()> {
    let source_store_id = compatibility_source_store_id()?;
    for bank_name in ["all", compatibility_category_label(category)] {
        db.mark_memory_v2_compatibility_bank_dirty_in_transaction(
            transaction,
            owner,
            &source_store_id,
            bank_name,
            updated_at,
        )
        .await
        .map_err(|error| storage_error(COMPATIBILITY_WRITE_OPERATION, error))?;
    }
    Ok(())
}

async fn compatibility_mirror_replace_entities_tx(
    transaction: &Transaction,
    legacy_fact_id: i64,
    entities: &[String],
    timestamp: i64,
) -> FactStoreResult<()> {
    let mut rows = transaction
        .query(
            "SELECT entity_id FROM memory_fact_entities WHERE fact_id = ?1",
            params![legacy_fact_id],
        )
        .await
        .map_err(|error| storage_error(COMPATIBILITY_WRITE_OPERATION, error))?;
    let mut old_entity_ids = Vec::new();
    while let Some(row) = rows
        .next()
        .await
        .map_err(|error| storage_error(COMPATIBILITY_WRITE_OPERATION, error))?
    {
        old_entity_ids.push(row_i64(&row, 0, COMPATIBILITY_WRITE_OPERATION)?);
    }
    drop(rows);
    transaction
        .execute(
            "DELETE FROM memory_fact_entities WHERE fact_id = ?1",
            params![legacy_fact_id],
        )
        .await
        .map_err(|error| storage_error(COMPATIBILITY_WRITE_OPERATION, error))?;
    let mut normalized = BTreeSet::new();
    for entity in entities {
        let name = normalize_entity(entity);
        let key = name.to_ascii_lowercase();
        if name.is_empty() || !normalized.insert(key.clone()) {
            continue;
        }
        let mut existing = transaction
            .query(
                "SELECT entity_id FROM memory_entities WHERE normalized_name = ?1",
                params![key.as_str()],
            )
            .await
            .map_err(|error| storage_error(COMPATIBILITY_WRITE_OPERATION, error))?;
        let entity_id = match existing
            .next()
            .await
            .map_err(|error| storage_error(COMPATIBILITY_WRITE_OPERATION, error))?
        {
            Some(row) => row_i64(&row, 0, COMPATIBILITY_WRITE_OPERATION)?,
            None => {
                drop(existing);
                transaction
                    .execute(
                        "INSERT INTO memory_entities(
                            name, normalized_name, entity_type, aliases, created_at, updated_at
                         ) VALUES(?1, ?2, 'unknown', '[]', ?3, ?3)",
                        params![name.as_str(), key.as_str(), timestamp],
                    )
                    .await
                    .map_err(|error| storage_error(COMPATIBILITY_WRITE_OPERATION, error))?;
                compatibility_last_insert_rowid_tx(transaction).await?
            }
        };
        transaction
            .execute(
                "INSERT OR IGNORE INTO memory_fact_entities(fact_id, entity_id)
                 VALUES(?1, ?2)",
                params![legacy_fact_id, entity_id],
            )
            .await
            .map_err(|error| storage_error(COMPATIBILITY_WRITE_OPERATION, error))?;
    }
    for entity_id in old_entity_ids {
        transaction
            .execute(
                "DELETE FROM memory_entities
                 WHERE entity_id = ?1
                   AND NOT EXISTS(
                     SELECT 1 FROM memory_fact_entities WHERE entity_id = ?1
                   )",
                params![entity_id],
            )
            .await
            .map_err(|error| storage_error(COMPATIBILITY_WRITE_OPERATION, error))?;
    }
    Ok(())
}

enum CompatibilityMirrorInsertV1 {
    Inserted(i64),
    Existing { fact_id: FactId },
}

async fn compatibility_mirror_insert_tx(
    db: &Database,
    transaction: &Transaction,
    owner: &FactOwnerV1,
    payload: &FactPayloadV1,
    source: &str,
    trust: Confidence,
    now: UtcMicros,
) -> FactStoreResult<CompatibilityMirrorInsertV1> {
    let timestamp = compatibility_legacy_timestamp(now);
    let mut existing = transaction
        .query(
            "SELECT fact_id FROM memory_facts WHERE content = ?1",
            params![payload.content()],
        )
        .await
        .map_err(|error| storage_error(COMPATIBILITY_WRITE_OPERATION, error))?;
    if let Some(row) = existing
        .next()
        .await
        .map_err(|error| storage_error(COMPATIBILITY_WRITE_OPERATION, error))?
    {
        let legacy_fact_id = row_i64(&row, 0, COMPATIBILITY_WRITE_OPERATION)?;
        let Some(fact_id) =
            compatibility_fact_for_legacy_id_tx(transaction, owner, legacy_fact_id).await?
        else {
            return Err(storage_message(
                COMPATIBILITY_WRITE_OPERATION,
                "compatibility mirror content is already bound to another owner or an unmigrated row",
            ));
        };
        return Ok(CompatibilityMirrorInsertV1::Existing { fact_id });
    }
    drop(existing);
    let vector = compatibility_mirror_vector(payload)?;
    transaction
        .execute(
            "INSERT INTO memory_facts(
                content, category, tags, trust_score, created_at, updated_at, source,
                metadata, hrr_vector, hrr_algebra, hrr_dim, hrr_precision
             ) VALUES(?1, ?2, ?3, ?4, ?5, ?5, ?6, ?7, ?8, 'amari_fhrr', ?9, 'f32')",
            params![
                payload.content(),
                compatibility_category_label(payload.category()),
                to_json(payload.tags(), "serialize compatibility mirror tags")?,
                trust.as_f64(),
                timestamp,
                source,
                to_json(
                    payload.metadata(),
                    "serialize compatibility mirror metadata"
                )?,
                vector,
                HolographicEncoder::DIMENSIONS as i64,
            ],
        )
        .await
        .map_err(|error| storage_error(COMPATIBILITY_WRITE_OPERATION, error))?;
    let legacy_fact_id = compatibility_last_insert_rowid_tx(transaction).await?;
    compatibility_mirror_replace_entities_tx(
        transaction,
        legacy_fact_id,
        payload.entities(),
        timestamp,
    )
    .await?;
    compatibility_mark_owner_banks_dirty_tx(db, transaction, owner, payload.category(), now)
        .await?;
    Ok(CompatibilityMirrorInsertV1::Inserted(legacy_fact_id))
}

#[allow(clippy::too_many_arguments)]
async fn compatibility_mirror_update_tx(
    db: &Database,
    transaction: &Transaction,
    owner: &FactOwnerV1,
    legacy_fact_id: i64,
    payload: &FactPayloadV1,
    source: &str,
    trust: Confidence,
    now: UtcMicros,
) -> FactStoreResult<()> {
    let timestamp = compatibility_legacy_timestamp(now);
    let vector = compatibility_mirror_vector(payload)?;
    transaction
        .execute(
            "UPDATE memory_facts SET
                content = ?1, category = ?2, tags = ?3, trust_score = ?4,
                source = ?5, metadata = ?6, hrr_vector = ?7, hrr_algebra = 'amari_fhrr',
                hrr_dim = ?8, hrr_precision = 'f32', updated_at = ?9
             WHERE fact_id = ?10",
            params![
                payload.content(),
                compatibility_category_label(payload.category()),
                to_json(payload.tags(), "serialize compatibility mirror tags")?,
                trust.as_f64(),
                source,
                to_json(
                    payload.metadata(),
                    "serialize compatibility mirror metadata"
                )?,
                vector,
                HolographicEncoder::DIMENSIONS as i64,
                timestamp,
                legacy_fact_id,
            ],
        )
        .await
        .map_err(|error| storage_error(COMPATIBILITY_WRITE_OPERATION, error))?;
    compatibility_mirror_replace_entities_tx(
        transaction,
        legacy_fact_id,
        payload.entities(),
        timestamp,
    )
    .await?;
    compatibility_mark_owner_banks_dirty_tx(db, transaction, owner, payload.category(), now).await
}

async fn compatibility_fact_for_legacy_id_tx(
    transaction: &Transaction,
    owner: &FactOwnerV1,
    legacy_fact_id: i64,
) -> FactStoreResult<Option<FactId>> {
    let key = OwnerKey::new(owner)?;
    let source_store_id = compatibility_source_store_id()?;
    let mut rows = transaction
        .query(
            "SELECT fact_id, owner_json FROM memory_v2_legacy_map
             WHERE owner_kind = ?1 AND project_id = ?2 AND source_store_id = ?3
               AND legacy_fact_id = ?4",
            params![
                key.kind,
                key.project_id.as_str(),
                source_store_id.as_str(),
                legacy_fact_id,
            ],
        )
        .await
        .map_err(|error| storage_error(QUERY_OPERATION, error))?;
    let Some(row) = rows
        .next()
        .await
        .map_err(|error| storage_error(QUERY_OPERATION, error))?
    else {
        return Ok(None);
    };
    if row_string(&row, 1, QUERY_OPERATION)? != key.json {
        return Err(FactStoreError::OwnerMismatch);
    }
    FactId::new(row_string(&row, 0, QUERY_OPERATION)?)
        .map(Some)
        .map_err(FactStoreError::from)
}

#[derive(Clone)]
struct CompatibilityOperationReceiptV1 {
    fact_id: Option<FactId>,
    event_id: Option<FactEventId>,
    receipt: Value,
}

fn compatibility_digest(material: Value) -> FactStoreResult<String> {
    let encoded = to_json(&material, "serialize compatibility request digest")?;
    let digest = Sha256::digest(encoded.as_bytes());
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut value = String::with_capacity(digest.len() * 2);
    for byte in digest {
        value.push(char::from(HEX[usize::from(byte >> 4)]));
        value.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    Ok(value)
}

async fn compatibility_lookup_operation_receipt_tx(
    transaction: &Transaction,
    owner: &FactOwnerV1,
    operation_id: &ProvenanceId,
    expected_kind: &'static str,
    request_digest: &str,
) -> FactStoreResult<Option<CompatibilityOperationReceiptV1>> {
    let key = OwnerKey::new(owner)?;
    let mut rows = transaction
        .query(
            "SELECT operation_kind, request_digest, fact_id, event_id, receipt_json
             FROM memory_v2_compatibility_operation_receipts
             WHERE owner_kind = ?1 AND project_id = ?2 AND operation_id = ?3",
            params![key.kind, key.project_id.as_str(), operation_id.as_str()],
        )
        .await
        .map_err(|error| storage_error(COMPATIBILITY_WRITE_OPERATION, error))?;
    let Some(row) = rows
        .next()
        .await
        .map_err(|error| storage_error(COMPATIBILITY_WRITE_OPERATION, error))?
    else {
        return Ok(None);
    };
    let operation_kind = row_string(&row, 0, COMPATIBILITY_WRITE_OPERATION)?;
    let stored_digest = row_string(&row, 1, COMPATIBILITY_WRITE_OPERATION)?;
    if operation_kind != expected_kind || stored_digest != request_digest {
        return Err(storage_message(
            COMPATIBILITY_WRITE_OPERATION,
            "compatibility operation id was reused with a different request",
        ));
    }
    let fact_id = row_optional_string(&row, 2, COMPATIBILITY_WRITE_OPERATION)?
        .map(FactId::new)
        .transpose()
        .map_err(FactStoreError::from)?;
    let event_id = row_optional_string(&row, 3, COMPATIBILITY_WRITE_OPERATION)?
        .map(FactEventId::new)
        .transpose()
        .map_err(FactStoreError::from)?;
    if event_id.is_some() && fact_id.is_none() {
        return Err(storage_message(
            COMPATIBILITY_WRITE_OPERATION,
            "compatibility receipt has an event without a fact",
        ));
    }
    let receipt = from_json::<Value>(
        &row_string(&row, 4, COMPATIBILITY_WRITE_OPERATION)?,
        COMPATIBILITY_WRITE_OPERATION,
    )?;
    Ok(Some(CompatibilityOperationReceiptV1 {
        fact_id,
        event_id,
        receipt,
    }))
}

#[allow(clippy::too_many_arguments)]
async fn compatibility_record_operation_receipt_tx(
    transaction: &Transaction,
    owner: &FactOwnerV1,
    operation_id: &ProvenanceId,
    operation_kind: &'static str,
    request_digest: &str,
    fact_id: Option<&FactId>,
    event_id: Option<&FactEventId>,
    receipt: &Value,
    recorded_at: UtcMicros,
) -> FactStoreResult<()> {
    if event_id.is_some() && fact_id.is_none() {
        return Err(storage_message(
            COMPATIBILITY_WRITE_OPERATION,
            "compatibility receipt cannot reference an event without a fact",
        ));
    }
    let key = OwnerKey::new(owner)?;
    transaction
        .execute(
            "INSERT INTO memory_v2_compatibility_operation_receipts(
                owner_kind, project_id, operation_id, operation_kind, request_digest,
                fact_id, event_id, receipt_json, recorded_at
             ) VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                key.kind,
                key.project_id.as_str(),
                operation_id.as_str(),
                operation_kind,
                request_digest,
                fact_id.map(FactId::as_str),
                event_id.map(FactEventId::as_str),
                to_json(receipt, "serialize compatibility operation receipt")?,
                recorded_at.0,
            ],
        )
        .await
        .map_err(|error| storage_error(COMPATIBILITY_WRITE_OPERATION, error))?;
    Ok(())
}

fn compatibility_event_time(now: UtcMicros, offset: i64) -> FactStoreResult<UtcMicros> {
    now.0.checked_add(offset).map(UtcMicros).ok_or_else(|| {
        storage_message(
            COMPATIBILITY_WRITE_OPERATION,
            "compatibility event timestamp overflow",
        )
    })
}

fn compatibility_legacy_mapping_for_new_fact(
    owner: &FactOwnerV1,
    legacy_fact_id: i64,
    now: UtcMicros,
) -> FactStoreResult<(FactIdentityMaterialV1, LegacyFactMappingV1)> {
    let source_store_id = compatibility_source_store_id()?;
    let identity = FactIdentityMaterialV1::new(
        owner.clone(),
        FactIdentitySourceV1::Legacy {
            source_store_id: source_store_id.clone(),
            legacy_fact_id,
        },
    )?;
    let fact_id = FactId::derive(&identity)?;
    let mapping = LegacyFactMappingV1::new(
        owner.clone(),
        source_store_id,
        legacy_fact_id,
        fact_id,
        tracedecay_domain::LegacyHistoryCoverageV1::Complete,
        now,
    )?;
    Ok((identity, mapping))
}

#[allow(clippy::too_many_arguments)]
fn compatibility_initial_batch(
    owner: &FactOwnerV1,
    identity: FactIdentityMaterialV1,
    mapping: LegacyFactMappingV1,
    payload: FactPayloadV1,
    access: PayloadAccessState,
    trust: Confidence,
    actor: Option<ActorId>,
    now: UtcMicros,
) -> FactStoreResult<FactWriteBatch> {
    let fact_id = mapping.fact_id().clone();
    let imported_at = compatibility_event_time(now, 0)?;
    let asserted_at = compatibility_event_time(now, 1)?;
    let assertion = FactAssertionV1::new(
        fact_id.clone(),
        owner.clone(),
        FactAssertionKindV1::Initial,
        payload,
        Vec::new(),
        asserted_at,
        actor.clone(),
    )?;
    let mut events = vec![
        FactLineageEventV1::new(
            fact_id.clone(),
            owner.clone(),
            FactLineageEventKindV1::LegacyImported {
                mapping: mapping.clone(),
            },
            imported_at,
            actor.clone(),
        )?,
        FactLineageEventV1::new(
            fact_id.clone(),
            owner.clone(),
            FactLineageEventKindV1::AssertionRecorded {
                assertion_id: assertion.assertion_id().clone(),
            },
            asserted_at,
            actor.clone(),
        )?,
    ];
    let mut next_offset = 2;
    if access != PayloadAccessState::Eligible {
        events.push(FactLineageEventV1::new(
            fact_id.clone(),
            owner.clone(),
            FactLineageEventKindV1::PayloadAccessChanged {
                previous: PayloadAccessState::Eligible,
                current: access,
            },
            compatibility_event_time(now, next_offset)?,
            actor.clone(),
        )?);
        next_offset += 1;
    }
    let default_trust = Confidence::new(DEFAULT_TRUST)?;
    if trust != default_trust {
        events.push(FactLineageEventV1::new(
            fact_id.clone(),
            owner.clone(),
            FactLineageEventKindV1::TrustChanged {
                previous: default_trust,
                current: trust,
                evidence_ids: Vec::new(),
            },
            compatibility_event_time(now, next_offset)?,
            actor.clone(),
        )?);
    }
    FactWriteBatch::new(
        fact_id,
        owner.clone(),
        Some(assertion),
        events,
        Vec::new(),
        Vec::new(),
        Some(mapping),
        None,
    )?
    .with_identity_material(identity)
}

async fn compatibility_commit_batch_tx(
    transaction: &Transaction,
    batch: &FactWriteBatch,
) -> FactStoreResult<(FactCommitReceipt, bool)> {
    let attempt = commit_fact_tx(transaction, batch).await?;
    match attempt.outcome {
        FactCommitOutcome::Committed(receipt) | FactCommitOutcome::IdempotentReplay(receipt) => {
            Ok((receipt, attempt.wrote))
        }
        FactCommitOutcome::Conflict(conflict) => Err(storage_message(
            COMPATIBILITY_WRITE_OPERATION,
            format!("compatibility canonical write conflict: {conflict:?}"),
        )),
        _ => Err(storage_message(
            COMPATIBILITY_WRITE_OPERATION,
            "compatibility canonical write returned an unsupported outcome",
        )),
    }
}

fn compatibility_tokens(text: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    for character in text.chars() {
        if character.is_ascii_alphanumeric() || matches!(character, '_' | '/' | ':' | '.') {
            current.push(character.to_ascii_lowercase());
        } else if !current.is_empty() {
            if current.len() >= 2 {
                tokens.push(std::mem::take(&mut current));
            } else {
                current.clear();
            }
        }
    }
    if current.len() >= 2 {
        tokens.push(current);
    }
    tokens.sort_unstable();
    tokens.dedup();
    tokens
}

fn compatibility_fact_tokens(fact: &CompatibilityFactV1) -> Vec<String> {
    let mut tokens = fact.content().map(compatibility_tokens).unwrap_or_default();
    if let Some(tags) = fact.tags() {
        for tag in tags {
            tokens.extend(compatibility_tokens(tag));
        }
    }
    if let Some(entities) = fact.entities() {
        for entity in entities {
            tokens.extend(compatibility_tokens(entity));
        }
    }
    tokens.sort_unstable();
    tokens.dedup();
    tokens
}

fn compatibility_term_coverage(query: &[String], fact: &[String]) -> f64 {
    if query.is_empty() {
        return 0.0;
    }
    let matched = query
        .iter()
        .filter(|query_token| {
            fact.iter().any(|fact_token| {
                fact_token == *query_token
                    || (query_token.len() >= 4 && fact_token.starts_with(query_token.as_str()))
            })
        })
        .count();
    matched as f64 / query.len() as f64
}

fn compatibility_jaccard(left: &[String], right: &[String]) -> f64 {
    if left.is_empty() || right.is_empty() {
        return 0.0;
    }
    let left = left.iter().map(String::as_str).collect::<BTreeSet<_>>();
    let right = right.iter().map(String::as_str).collect::<BTreeSet<_>>();
    let union = left.union(&right).count();
    if union == 0 {
        0.0
    } else {
        left.intersection(&right).count() as f64 / union as f64
    }
}

fn compatibility_holographic_score(query: &str, fact: &CompatibilityFactV1) -> f64 {
    let Some(content) = fact.content() else {
        return 0.0;
    };
    let encoder = HolographicEncoder::new();
    let query_vector = encoder.encode_fact(query, &compatibility_tokens(query));
    let fact_vector = encoder.encode_fact(content, fact.entities().unwrap_or_default());
    f64::midpoint(encoder.similarity(&query_vector, &fact_vector), 1.0).clamp(0.0, 1.0)
}

fn compatibility_millionths(value: f64) -> u32 {
    (value.clamp(0.0, 1.0) * 1_000_000.0).round() as u32
}

fn compatibility_temporal_decay(updated_at: UtcMicros, now: UtcMicros) -> f64 {
    let age_micros = now.0.saturating_sub(updated_at.0).max(0) as f64;
    let age_days = age_micros / 86_400_000_000.0;
    0.5_f64.powf(age_days / 365.0).clamp(0.10, 1.0)
}

fn compatibility_search_scores(
    query: &str,
    query_tokens: &[String],
    fact: &CompatibilityFactV1,
    now: UtcMicros,
) -> FactStoreResult<(CompatibilityFactSearchScoresV1, String)> {
    let fact_tokens = compatibility_fact_tokens(fact);
    let coverage = compatibility_term_coverage(query_tokens, &fact_tokens);
    let fts = coverage;
    let jaccard = compatibility_jaccard(query_tokens, &fact_tokens);
    let holographic = compatibility_holographic_score(query, fact);
    let trust = fact.fact().trust().as_f64();
    let temporal_decay = compatibility_temporal_decay(fact.telemetry().updated_at(), now);
    let usage_boost = 1.0 + (0.02 * (fact.telemetry().retrieval_count() as f64).ln_1p()).min(0.5);
    let score =
        (fts * 0.40 + jaccard * 0.30 + holographic * 0.30) * trust * temporal_decay * usage_boost;
    Ok((
        CompatibilityFactSearchScoresV1::new(
            compatibility_millionths(score),
            compatibility_millionths(fts),
            compatibility_millionths(jaccard),
            compatibility_millionths(holographic),
            compatibility_millionths(trust),
        )?,
        format!(
            "fts={fts:.3}, coverage={coverage:.3}, jaccard={jaccard:.3}, holographic={holographic:.3}, trust={trust:.3}, temporal_decay={temporal_decay:.3}, retrieval_count={}",
            fact.telemetry().retrieval_count(),
        ),
    ))
}

async fn compatibility_available_facts_tx(
    transaction: &Transaction,
    owner: &FactOwnerV1,
    category: Option<FactCategoryV1>,
    min_trust: Option<Confidence>,
) -> FactCompatibilityResult<Vec<CompatibilityFactV1>> {
    let query = CompatibilityFactListQueryV1::new(owner.clone(), category, min_trust, None, 1_000)?;
    let page = list_compatibility_facts_tx(transaction, &query).await?;
    Ok(page
        .facts()
        .iter()
        .filter_map(|projection| match projection {
            CompatibilityFactProjectionV1::Available(fact) => Some(fact.as_ref().clone()),
            CompatibilityFactProjectionV1::Unavailable(_) => None,
        })
        .collect())
}

fn compatibility_matches_entity(fact: &CompatibilityFactV1, entity: &str) -> bool {
    let normalized = normalize_entity(entity).to_ascii_lowercase();
    !normalized.is_empty()
        && fact.entities().is_some_and(|entities| {
            entities.iter().any(|candidate| {
                normalize_entity(candidate).eq_ignore_ascii_case(normalized.as_str())
            })
        })
}

fn compatibility_matches_all_entities(fact: &CompatibilityFactV1, entities: &[String]) -> bool {
    entities
        .iter()
        .all(|entity| compatibility_matches_entity(fact, entity))
}

async fn compatibility_rank_facts_tx(
    transaction: &Transaction,
    query: &CompatibilityFactSearchQuery,
) -> FactCompatibilityResult<CompatibilityFactSearchPageV1> {
    let min_trust = query
        .filter()
        .min_trust()
        .unwrap_or(Confidence::new(0.3).map_err(FactStoreError::from)?);
    let mut facts = compatibility_available_facts_tx(
        transaction,
        query.owner(),
        query.filter().category(),
        Some(min_trust),
    )
    .await?;
    let now = compatibility_now()?;
    let mut ranked = Vec::with_capacity(facts.len());
    match query.kind() {
        CompatibilityFactSearchKindV1::Search => {
            let text = query.query().ok_or_else(|| {
                storage_message(
                    COMPATIBILITY_READ_OPERATION,
                    "compatibility search query is missing",
                )
            })?;
            let tokens = compatibility_tokens(text);
            for fact in facts.drain(..) {
                let (scores, why) = compatibility_search_scores(text, &tokens, &fact, now)?;
                // Mirror the legacy retriever's relevance floor: a non-empty
                // query only returns facts with a real textual signal (FTS/term
                // overlap). Facts surfaced solely by the dense holographic
                // baseline or trust are never relevant matches, so scoring them
                // above zero must not pull unrelated facts into the results (or
                // bump their access counts).
                if !tokens.is_empty()
                    && scores.fts_score_millionths() == 0
                    && scores.jaccard_score_millionths() == 0
                {
                    continue;
                }
                if query
                    .filter()
                    .threshold_millionths()
                    .is_some_and(|threshold| scores.score_millionths() < threshold)
                {
                    continue;
                }
                ranked.push((
                    CompatibilityFactSearchHitV1::new(fact.clone(), scores, Some(why))?,
                    fact.telemetry().updated_at(),
                ));
            }
        }
        CompatibilityFactSearchKindV1::Probe => {
            let entity = query.query().ok_or_else(|| {
                storage_message(
                    COMPATIBILITY_READ_OPERATION,
                    "compatibility probe query is missing",
                )
            })?;
            for fact in facts
                .drain(..)
                .filter(|fact| compatibility_matches_entity(fact, entity))
            {
                let trust = compatibility_millionths(fact.fact().trust().as_f64());
                let scores = CompatibilityFactSearchScoresV1::new(trust, 0, 0, 1_000_000, trust)?;
                ranked.push((
                    CompatibilityFactSearchHitV1::new(
                        fact.clone(),
                        scores,
                        Some("entity probe".to_owned()),
                    )?,
                    fact.telemetry().updated_at(),
                ));
            }
        }
        CompatibilityFactSearchKindV1::Related { entity } => {
            for fact in facts
                .drain(..)
                .filter(|fact| compatibility_matches_entity(fact, &entity))
            {
                let trust = compatibility_millionths(fact.fact().trust().as_f64());
                let scores = CompatibilityFactSearchScoresV1::new(trust, 0, 0, 1_000_000, trust)?;
                ranked.push((
                    CompatibilityFactSearchHitV1::new(
                        fact.clone(),
                        scores,
                        Some("entity related".to_owned()),
                    )?,
                    fact.telemetry().updated_at(),
                ));
            }
        }
        CompatibilityFactSearchKindV1::Reason { entities } => {
            for fact in facts
                .drain(..)
                .filter(|fact| compatibility_matches_all_entities(fact, &entities))
            {
                let trust = compatibility_millionths(fact.fact().trust().as_f64());
                let scores = CompatibilityFactSearchScoresV1::new(trust, 0, 0, 1_000_000, trust)?;
                ranked.push((
                    CompatibilityFactSearchHitV1::new(
                        fact.clone(),
                        scores,
                        Some("entity reasoning".to_owned()),
                    )?,
                    fact.telemetry().updated_at(),
                ));
            }
        }
    }
    ranked.sort_by(|(left, left_updated), (right, right_updated)| {
        right
            .score_millionths()
            .cmp(&left.score_millionths())
            .then_with(|| right_updated.cmp(left_updated))
            .then_with(|| left.fact().fact_id().cmp(right.fact().fact_id()))
    });
    if let Some(after) = query.after() {
        ranked.retain(|(hit, updated_at)| {
            hit.score_millionths() < after.score_millionths()
                || (hit.score_millionths() == after.score_millionths()
                    && (*updated_at < after.updated_at()
                        || (*updated_at == after.updated_at()
                            && hit.fact().fact_id() > after.fact_id())))
        });
    }
    let has_more = ranked.len() > query.limit();
    ranked.truncate(query.limit());
    let next_after = if has_more {
        ranked.last().map(|(hit, updated_at)| {
            CompatibilityFactSearchCursorV1::new(
                hit.score_millionths(),
                *updated_at,
                hit.fact().fact_id().clone(),
            )
        })
    } else {
        None
    }
    .transpose()?;
    CompatibilityFactSearchPageV1::new(
        query.owner().clone(),
        ranked.into_iter().map(|(hit, _)| hit).collect(),
        next_after,
    )
    .map_err(Into::into)
}

async fn search_compatibility_facts_tx(
    transaction: &Transaction,
    query: &CompatibilityFactSearchQuery,
) -> FactCompatibilityResult<CompatibilityFactSearchPageV1> {
    compatibility_rank_facts_tx(transaction, query).await
}

async fn probe_compatibility_facts_tx(
    transaction: &Transaction,
    query: &CompatibilityFactSearchQuery,
) -> FactCompatibilityResult<CompatibilityFactSearchPageV1> {
    compatibility_rank_facts_tx(transaction, query).await
}

async fn related_compatibility_facts_tx(
    transaction: &Transaction,
    query: &CompatibilityFactSearchQuery,
) -> FactCompatibilityResult<CompatibilityFactSearchPageV1> {
    let CompatibilityFactSearchKindV1::Related { entity } = query.kind() else {
        return Err(storage_message(
            COMPATIBILITY_READ_OPERATION,
            "compatibility related query has the wrong kind",
        )
        .into());
    };
    let key = OwnerKey::new(query.owner())?;
    let source_store_id = compatibility_source_store_id()?;
    let normalized = normalize_entity(&entity).to_ascii_lowercase();
    let mut entity_rows = transaction
        .query(
            "SELECT DISTINCT entities.entity_id
             FROM memory_entities AS entities
             JOIN memory_fact_entities AS links ON links.entity_id = entities.entity_id
             JOIN memory_v2_legacy_map AS mappings ON mappings.legacy_fact_id = links.fact_id
             WHERE mappings.owner_kind = ?1 AND mappings.project_id = ?2
               AND mappings.owner_json = ?3 AND mappings.source_store_id = ?4
               AND (
                    entities.normalized_name = ?5
                    OR (
                        json_valid(entities.aliases)
                        AND EXISTS(
                            SELECT 1 FROM json_each(entities.aliases) AS aliases
                            WHERE lower(trim(CAST(aliases.value AS TEXT))) = ?5
                        )
                    )
               )
              ORDER BY entities.name ASC, entities.entity_id ASC
              LIMIT 256",
            params![
                key.kind,
                key.project_id.as_str(),
                key.json.as_str(),
                source_store_id.as_str(),
                normalized,
            ],
        )
        .await
        .map_err(|error| storage_error(COMPATIBILITY_READ_OPERATION, error))?;
    let mut source_entity_ids = Vec::new();
    while let Some(row) = entity_rows
        .next()
        .await
        .map_err(|error| storage_error(COMPATIBILITY_READ_OPERATION, error))?
    {
        source_entity_ids.push(row_i64(&row, 0, COMPATIBILITY_READ_OPERATION)?);
    }
    drop(entity_rows);
    if source_entity_ids.is_empty() {
        return CompatibilityFactSearchPageV1::new(query.owner().clone(), Vec::new(), None)
            .map_err(Into::into);
    }

    let placeholders = std::iter::repeat_n("?", source_entity_ids.len())
        .collect::<Vec<_>>()
        .join(",");
    let mut values = Vec::with_capacity(source_entity_ids.len() + 4);
    values.push(libsql::Value::Text(key.kind.to_string()));
    values.push(libsql::Value::Text(key.project_id.clone()));
    values.push(libsql::Value::Text(key.json.clone()));
    values.push(libsql::Value::Text(source_store_id.as_str().to_owned()));
    values.extend(
        source_entity_ids
            .iter()
            .copied()
            .map(libsql::Value::Integer),
    );
    let sql = format!(
        "SELECT DISTINCT co_entities.entity_id, co_entities.name
         FROM memory_fact_entities AS source_links
         JOIN memory_fact_entities AS co_links ON co_links.fact_id = source_links.fact_id
         JOIN memory_entities AS co_entities ON co_entities.entity_id = co_links.entity_id
         JOIN memory_v2_legacy_map AS mappings ON mappings.legacy_fact_id = source_links.fact_id
         WHERE mappings.owner_kind = ? AND mappings.project_id = ?
           AND mappings.owner_json = ? AND mappings.source_store_id = ?
           AND source_links.entity_id IN ({placeholders})
           AND co_links.entity_id NOT IN ({placeholders})
         ORDER BY co_entities.name ASC, co_entities.entity_id ASC
         LIMIT ?",
    );
    // The source-id list appears twice. Bind a separate, fixed-width value
    // list so this remains parameterized rather than interpolating identifiers.
    let mut co_values = values.clone();
    co_values.extend(
        source_entity_ids
            .iter()
            .copied()
            .map(libsql::Value::Integer),
    );
    co_values.push(libsql::Value::Integer(query.limit() as i64));
    let mut co_rows = transaction
        .query(&sql, co_values)
        .await
        .map_err(|error| storage_error(COMPATIBILITY_READ_OPERATION, error))?;
    let mut co_entities = Vec::new();
    while let Some(row) = co_rows
        .next()
        .await
        .map_err(|error| storage_error(COMPATIBILITY_READ_OPERATION, error))?
    {
        co_entities.push((
            row_i64(&row, 0, COMPATIBILITY_READ_OPERATION)?,
            row_string(&row, 1, COMPATIBILITY_READ_OPERATION)?,
        ));
    }
    drop(co_rows);

    let per_entity_limit = query.limit().saturating_mul(2).max(1);
    let mut encountered = Vec::new();
    let mut seen = BTreeSet::new();
    for (entity_id, _) in co_entities {
        let category = query.filter().category().map(compatibility_category_label);
        let min_trust = query
            .filter()
            .min_trust()
            .unwrap_or(Confidence::new(0.3).map_err(FactStoreError::from)?)
            .as_f64();
        let mut rows = match category {
            Some(category) => transaction
                .query(
                    "SELECT mappings.fact_id
                     FROM memory_fact_entities AS links
                     JOIN memory_v2_legacy_map AS mappings ON mappings.legacy_fact_id = links.fact_id
                     JOIN memory_facts AS legacy_facts ON legacy_facts.fact_id = links.fact_id
                     WHERE links.entity_id = ?1
                       AND mappings.owner_kind = ?2 AND mappings.project_id = ?3
                       AND mappings.owner_json = ?4 AND mappings.source_store_id = ?5
                       AND legacy_facts.category = ?6 AND legacy_facts.trust_score >= ?7
                     ORDER BY legacy_facts.updated_at DESC, mappings.fact_id ASC LIMIT ?8",
                    params![
                        entity_id,
                        key.kind,
                        key.project_id.as_str(),
                        key.json.as_str(),
                        source_store_id.as_str(),
                        category,
                        min_trust,
                        per_entity_limit as i64,
                    ],
                )
                .await,
            None => transaction
                .query(
                    "SELECT mappings.fact_id
                     FROM memory_fact_entities AS links
                     JOIN memory_v2_legacy_map AS mappings ON mappings.legacy_fact_id = links.fact_id
                     JOIN memory_facts AS legacy_facts ON legacy_facts.fact_id = links.fact_id
                     WHERE links.entity_id = ?1
                       AND mappings.owner_kind = ?2 AND mappings.project_id = ?3
                       AND mappings.owner_json = ?4 AND mappings.source_store_id = ?5
                       AND legacy_facts.trust_score >= ?6
                     ORDER BY legacy_facts.updated_at DESC, mappings.fact_id ASC LIMIT ?7",
                    params![
                        entity_id,
                        key.kind,
                        key.project_id.as_str(),
                        key.json.as_str(),
                        source_store_id.as_str(),
                        min_trust,
                        per_entity_limit as i64,
                    ],
                )
                .await,
        }
        .map_err(|error| storage_error(COMPATIBILITY_READ_OPERATION, error))?;
        while let Some(row) = rows
            .next()
            .await
            .map_err(|error| storage_error(COMPATIBILITY_READ_OPERATION, error))?
        {
            let fact_id = FactId::new(row_string(&row, 0, COMPATIBILITY_READ_OPERATION)?)
                .map_err(FactStoreError::from)?;
            if seen.insert(fact_id.clone()) {
                encountered.push(fact_id);
            }
        }
    }
    let mut ranked = Vec::new();
    for fact_id in encountered {
        let Some(CompatibilityFactProjectionV1::Available(fact)) =
            load_compatibility_projection_tx(transaction, query.owner(), &fact_id).await?
        else {
            continue;
        };
        let trust = compatibility_millionths(fact.fact().trust().as_f64());
        let scores = CompatibilityFactSearchScoresV1::new(trust, 0, 0, 1_000_000, trust)?;
        let updated_at = fact.telemetry().updated_at();
        ranked.push((
            CompatibilityFactSearchHitV1::new(
                *fact,
                scores,
                Some("co-occurring entity".to_owned()),
            )?,
            updated_at,
        ));
    }
    ranked.sort_by(|(left, left_updated), (right, right_updated)| {
        right
            .score_millionths()
            .cmp(&left.score_millionths())
            .then_with(|| right_updated.cmp(left_updated))
            .then_with(|| left.fact().fact_id().cmp(right.fact().fact_id()))
    });
    if let Some(after) = query.after() {
        ranked.retain(|(hit, updated_at)| {
            hit.score_millionths() < after.score_millionths()
                || (hit.score_millionths() == after.score_millionths()
                    && (*updated_at < after.updated_at()
                        || (*updated_at == after.updated_at()
                            && hit.fact().fact_id() > after.fact_id())))
        });
    }
    ranked.truncate(query.limit());
    // V1 related-fact traversal is one bounded, name-ordered co-occurrence
    // expansion rather than a cursorable global search. Exposing a cursor
    // here would falsely imply coverage beyond the intentionally capped
    // co-entity frontier.
    let next_after = None;
    CompatibilityFactSearchPageV1::new(
        query.owner().clone(),
        ranked.into_iter().map(|(hit, _)| hit).collect(),
        next_after,
    )
    .map_err(Into::into)
}

async fn reason_compatibility_facts_tx(
    transaction: &Transaction,
    query: &CompatibilityFactSearchQuery,
) -> FactCompatibilityResult<CompatibilityFactSearchPageV1> {
    compatibility_rank_facts_tx(transaction, query).await
}

async fn find_compatibility_contradictions_tx(
    transaction: &Transaction,
    query: &CompatibilityFactContradictionQueryV1,
) -> FactCompatibilityResult<CompatibilityFactContradictionPageV1> {
    let mut facts = compatibility_available_facts_tx(
        transaction,
        query.owner(),
        query.category(),
        Some(Confidence::new(0.0).map_err(FactStoreError::from)?),
    )
    .await?;
    facts.sort_by(|left, right| left.fact_id().cmp(right.fact_id()));
    let mut contradictions = Vec::new();
    'outer: for (index, left) in facts.iter().enumerate() {
        for right in facts.iter().skip(index + 1) {
            let Some(left_content) = left.content() else {
                continue;
            };
            let Some(right_content) = right.content() else {
                continue;
            };
            let left_entities = left
                .entities()
                .unwrap_or_default()
                .iter()
                .map(|entity| normalize_entity(entity).to_ascii_lowercase())
                .collect::<BTreeSet<_>>();
            let right_entities = right
                .entities()
                .unwrap_or_default()
                .iter()
                .map(|entity| normalize_entity(entity).to_ascii_lowercase())
                .collect::<BTreeSet<_>>();
            if left_entities.is_disjoint(&right_entities) {
                continue;
            }
            let left_tokens = compatibility_fact_tokens(left);
            let right_tokens = compatibility_fact_tokens(right);
            let similarity = compatibility_jaccard(&left_tokens, &right_tokens);
            let divergence = 1.0 - similarity;
            let left_negative = left_tokens.iter().any(|token| {
                matches!(
                    token.as_str(),
                    "not" | "no" | "never" | "avoid" | "dont" | "don't"
                )
            });
            let right_negative = right_tokens.iter().any(|token| {
                matches!(
                    token.as_str(),
                    "not" | "no" | "never" | "avoid" | "dont" | "don't"
                )
            });
            let score = compatibility_millionths(divergence);
            if score < query.threshold_millionths() && left_negative == right_negative {
                continue;
            }
            let (existing, new_content) = if left_negative {
                (right.clone(), left_content)
            } else {
                (left.clone(), right_content)
            };
            contradictions.push(CompatibilityFactContradictionV1::new(
                existing,
                new_content.to_owned(),
                score,
                Some(format!(
                    "shared entities with content divergence={divergence:.3}"
                )),
            )?);
            if contradictions.len() >= query.limit() {
                break 'outer;
            }
        }
    }
    CompatibilityFactContradictionPageV1::new(query.owner().clone(), contradictions)
        .map_err(Into::into)
}

fn compatibility_target_digest(target: &CompatibilityFactTargetV1) -> FactStoreResult<Value> {
    match target {
        CompatibilityFactTargetV1::Canonical(target) => Ok(json!({
            "canonical_fact_id": target.fact_id().as_str(),
        })),
        CompatibilityFactTargetV1::Legacy(query) => Ok(json!({
            "legacy_source_store_id": query.source_store_id().as_str(),
            "legacy_fact_id": query.legacy_fact_id(),
        })),
    }
}

async fn compatibility_required_mapping_tx(
    transaction: &Transaction,
    owner: &FactOwnerV1,
    fact_id: &FactId,
) -> FactStoreResult<LegacyFactMappingV1> {
    compatibility_legacy_mapping_tx(transaction, owner, fact_id)
        .await?
        .ok_or_else(|| {
            storage_message(
                COMPATIBILITY_WRITE_OPERATION,
                "compatibility fact has no fixed legacy-memory-v1 mapping",
            )
        })
}

async fn compatibility_source_for_fact_tx(
    transaction: &Transaction,
    mapping: &LegacyFactMappingV1,
) -> FactStoreResult<String> {
    let mut rows = transaction
        .query(
            "SELECT source FROM memory_facts WHERE fact_id = ?1",
            params![mapping.legacy_fact_id()],
        )
        .await
        .map_err(|error| storage_error(COMPATIBILITY_WRITE_OPERATION, error))?;
    let source = rows
        .next()
        .await
        .map_err(|error| storage_error(COMPATIBILITY_WRITE_OPERATION, error))?
        .map(|row| row_optional_string(&row, 0, COMPATIBILITY_WRITE_OPERATION))
        .transpose()?
        .flatten()
        .unwrap_or_else(|| "manual".to_owned());
    compatibility_source_label(Some(source.as_str()))
}

async fn compatibility_active_fact_count_tx(
    transaction: &Transaction,
    owner: &FactOwnerV1,
) -> FactStoreResult<u64> {
    let key = OwnerKey::new(owner)?;
    let mut rows = transaction
        .query(
            "SELECT COUNT(*) FROM memory_v2_current_facts AS current_facts
             JOIN memory_v2_facts AS facts
               ON facts.fact_id = current_facts.fact_id
              AND facts.owner_kind = current_facts.owner_kind
              AND facts.project_id = current_facts.project_id
             WHERE current_facts.owner_kind = ?1 AND current_facts.project_id = ?2
               AND facts.owner_json = ?3 AND current_facts.active_assertion_id IS NOT NULL",
            params![key.kind, key.project_id.as_str(), key.json.as_str()],
        )
        .await
        .map_err(|error| storage_error(COMPATIBILITY_WRITE_OPERATION, error))?;
    let row = rows
        .next()
        .await
        .map_err(|error| storage_error(COMPATIBILITY_WRITE_OPERATION, error))?
        .ok_or_else(|| {
            storage_message(
                COMPATIBILITY_WRITE_OPERATION,
                "compatibility count is missing",
            )
        })?;
    nonnegative_u64(
        row_i64(&row, 0, COMPATIBILITY_WRITE_OPERATION)?,
        "active fact count",
    )
}

async fn compatibility_mirror_delete_tx(
    db: &Database,
    transaction: &Transaction,
    owner: &FactOwnerV1,
    legacy_fact_id: i64,
    category: FactCategoryV1,
    now: UtcMicros,
) -> FactStoreResult<()> {
    let mut rows = transaction
        .query(
            "SELECT entity_id FROM memory_fact_entities WHERE fact_id = ?1",
            params![legacy_fact_id],
        )
        .await
        .map_err(|error| storage_error(COMPATIBILITY_WRITE_OPERATION, error))?;
    let mut entity_ids = Vec::new();
    while let Some(row) = rows
        .next()
        .await
        .map_err(|error| storage_error(COMPATIBILITY_WRITE_OPERATION, error))?
    {
        entity_ids.push(row_i64(&row, 0, COMPATIBILITY_WRITE_OPERATION)?);
    }
    drop(rows);
    transaction
        .execute(
            "DELETE FROM memory_fact_entities WHERE fact_id = ?1",
            params![legacy_fact_id],
        )
        .await
        .map_err(|error| storage_error(COMPATIBILITY_WRITE_OPERATION, error))?;
    transaction
        .execute(
            "DELETE FROM memory_facts WHERE fact_id = ?1",
            params![legacy_fact_id],
        )
        .await
        .map_err(|error| storage_error(COMPATIBILITY_WRITE_OPERATION, error))?;
    for entity_id in entity_ids {
        transaction
            .execute(
                "DELETE FROM memory_entities
                 WHERE entity_id = ?1
                   AND NOT EXISTS(
                     SELECT 1 FROM memory_fact_entities WHERE entity_id = ?1
                   )",
                params![entity_id],
            )
            .await
            .map_err(|error| storage_error(COMPATIBILITY_WRITE_OPERATION, error))?;
    }
    compatibility_mark_owner_banks_dirty_tx(db, transaction, owner, category, now).await
}

fn compatibility_feedback_action_label(action: CompatibilityFactFeedbackActionV1) -> &'static str {
    match action {
        CompatibilityFactFeedbackActionV1::Helpful => "helpful",
        CompatibilityFactFeedbackActionV1::Unhelpful => "unhelpful",
    }
}

fn compatibility_feedback_delta(action: CompatibilityFactFeedbackActionV1) -> f64 {
    match action {
        CompatibilityFactFeedbackActionV1::Helpful => 0.05,
        CompatibilityFactFeedbackActionV1::Unhelpful => -0.10,
    }
}

#[allow(clippy::too_many_arguments)]
async fn compatibility_mirror_feedback_tx(
    transaction: &Transaction,
    legacy_fact_id: i64,
    action: CompatibilityFactFeedbackActionV1,
    old_trust: Confidence,
    new_trust: Confidence,
    timestamp: i64,
    source: &str,
    note: Option<&str>,
) -> FactStoreResult<i64> {
    let changed = transaction
        .execute(
            "UPDATE memory_facts SET
                trust_score = ?1,
                helpful_count = helpful_count + ?2,
                unhelpful_count = unhelpful_count + ?3,
                last_feedback_at = ?4,
                updated_at = ?4
             WHERE fact_id = ?5",
            params![
                new_trust.as_f64(),
                i64::from(matches!(action, CompatibilityFactFeedbackActionV1::Helpful)),
                i64::from(matches!(
                    action,
                    CompatibilityFactFeedbackActionV1::Unhelpful
                )),
                timestamp,
                legacy_fact_id,
            ],
        )
        .await
        .map_err(|error| storage_error(COMPATIBILITY_WRITE_OPERATION, error))?;
    if changed != 1 {
        return Err(storage_message(
            COMPATIBILITY_WRITE_OPERATION,
            "compatibility feedback target is missing from the legacy mirror",
        ));
    }
    transaction
        .execute(
            "INSERT INTO memory_feedback_events (
                fact_id, action, trust_delta, old_trust, new_trust,
                created_at, source, note
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                legacy_fact_id,
                compatibility_feedback_action_label(action),
                new_trust.as_f64() - old_trust.as_f64(),
                old_trust.as_f64(),
                new_trust.as_f64(),
                timestamp,
                source,
                note,
            ],
        )
        .await
        .map_err(|error| storage_error(COMPATIBILITY_WRITE_OPERATION, error))?;
    compatibility_last_insert_rowid_tx(transaction).await
}

async fn compatibility_update_feedback_projection_tx(
    transaction: &Transaction,
    owner: &FactOwnerV1,
    fact_id: &FactId,
    action: CompatibilityFactFeedbackActionV1,
    timestamp: UtcMicros,
) -> FactStoreResult<()> {
    let key = OwnerKey::new(owner)?;
    let changed = transaction
        .execute(
            "UPDATE memory_v2_current_facts SET
                helpful_count = helpful_count + ?1,
                unhelpful_count = unhelpful_count + ?2,
                last_feedback_at = ?3
             WHERE fact_id = ?4 AND owner_kind = ?5 AND project_id = ?6",
            params![
                i64::from(matches!(action, CompatibilityFactFeedbackActionV1::Helpful)),
                i64::from(matches!(
                    action,
                    CompatibilityFactFeedbackActionV1::Unhelpful
                )),
                timestamp.0,
                fact_id.as_str(),
                key.kind,
                key.project_id.as_str(),
            ],
        )
        .await
        .map_err(|error| storage_error(COMPATIBILITY_WRITE_OPERATION, error))?;
    if changed != 1 {
        return Err(storage_message(
            COMPATIBILITY_WRITE_OPERATION,
            "compatibility feedback target has no current projection",
        ));
    }
    Ok(())
}

async fn compatibility_update_retrieval_projection_tx(
    transaction: &Transaction,
    owner: &FactOwnerV1,
    fact_id: &FactId,
    recall: bool,
    timestamp: UtcMicros,
) -> FactStoreResult<()> {
    let key = OwnerKey::new(owner)?;
    let changed = transaction
        .execute(
            "UPDATE memory_v2_current_facts SET
                retrieval_count = retrieval_count + 1,
                access_count = access_count + ?1,
                last_retrieved_at = ?2,
                last_recalled_at = CASE WHEN ?1 = 1 THEN ?2 ELSE last_recalled_at END
             WHERE fact_id = ?3 AND owner_kind = ?4 AND project_id = ?5",
            params![
                i64::from(recall),
                timestamp.0,
                fact_id.as_str(),
                key.kind,
                key.project_id.as_str(),
            ],
        )
        .await
        .map_err(|error| storage_error(COMPATIBILITY_WRITE_OPERATION, error))?;
    if changed != 1 {
        return Err(storage_message(
            COMPATIBILITY_WRITE_OPERATION,
            "compatibility retrieval target has no current projection",
        ));
    }
    let mapping = compatibility_required_mapping_tx(transaction, owner, fact_id).await?;
    let changed = transaction
        .execute(
            "UPDATE memory_facts SET
                retrieval_count = retrieval_count + 1,
                access_count = access_count + ?1,
                last_retrieved_at = ?2,
                last_recalled_at = CASE WHEN ?1 = 1 THEN ?2 ELSE last_recalled_at END
             WHERE fact_id = ?3",
            params![
                i64::from(recall),
                compatibility_legacy_timestamp(timestamp),
                mapping.legacy_fact_id()
            ],
        )
        .await
        .map_err(|error| storage_error(COMPATIBILITY_WRITE_OPERATION, error))?;
    if changed != 1 {
        return Err(storage_message(
            COMPATIBILITY_WRITE_OPERATION,
            "compatibility retrieval target is missing from the legacy mirror",
        ));
    }
    Ok(())
}

fn compatibility_correction_batch(
    fact: &StoredFactV1,
    payload: FactPayloadV1,
    access: PayloadAccessState,
    trust: Confidence,
    expected_last_event_id: Option<FactEventId>,
    actor: Option<ActorId>,
    now: UtcMicros,
) -> FactStoreResult<FactWriteBatch> {
    let assertion = FactAssertionV1::new(
        fact.fact_id().clone(),
        fact.owner().clone(),
        FactAssertionKindV1::Correction {
            supersedes: fact.active_assertion_id().clone(),
        },
        payload,
        Vec::new(),
        now,
        actor.clone(),
    )?;
    let mut events = vec![FactLineageEventV1::new(
        fact.fact_id().clone(),
        fact.owner().clone(),
        FactLineageEventKindV1::AssertionRecorded {
            assertion_id: assertion.assertion_id().clone(),
        },
        now,
        actor.clone(),
    )?];
    let mut offset = 1;
    if access != fact.payload_access() {
        events.push(FactLineageEventV1::new(
            fact.fact_id().clone(),
            fact.owner().clone(),
            FactLineageEventKindV1::PayloadAccessChanged {
                previous: fact.payload_access(),
                current: access,
            },
            compatibility_event_time(now, offset)?,
            actor.clone(),
        )?);
        offset += 1;
    }
    if trust != fact.trust() {
        events.push(FactLineageEventV1::new(
            fact.fact_id().clone(),
            fact.owner().clone(),
            FactLineageEventKindV1::TrustChanged {
                previous: fact.trust(),
                current: trust,
                evidence_ids: Vec::new(),
            },
            compatibility_event_time(now, offset)?,
            actor,
        )?);
    }
    FactWriteBatch::new(
        fact.fact_id().clone(),
        fact.owner().clone(),
        Some(assertion),
        events,
        Vec::new(),
        Vec::new(),
        None,
        expected_last_event_id,
    )
}

fn compatibility_removal_batch(
    owner: &FactOwnerV1,
    fact_id: &FactId,
    previous: PayloadAccessState,
    expected_last_event_id: Option<FactEventId>,
    actor: Option<ActorId>,
    now: UtcMicros,
) -> FactStoreResult<FactWriteBatch> {
    let event = FactLineageEventV1::new(
        fact_id.clone(),
        owner.clone(),
        FactLineageEventKindV1::PayloadAccessChanged {
            previous,
            current: PayloadAccessState::Deleted,
        },
        now,
        actor,
    )?;
    FactWriteBatch::new(
        fact_id.clone(),
        owner.clone(),
        None,
        vec![event],
        Vec::new(),
        Vec::new(),
        None,
        expected_last_event_id,
    )
}

async fn compatibility_replay_add_tx(
    transaction: &Transaction,
    owner: &FactOwnerV1,
    receipt: &CompatibilityOperationReceiptV1,
) -> FactCompatibilityResult<CompatibilityFactAddOutcomeV1> {
    let outcome = receipt
        .receipt
        .get("outcome")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            storage_message(
                COMPATIBILITY_WRITE_OPERATION,
                "compatibility add receipt is malformed",
            )
        })?;
    match outcome {
        "rejected_secret_like" => CompatibilityFactAddOutcomeV1::new(
            None,
            CompatibilityFactAddDispositionV1::RejectedSecretLike,
            None,
            None,
            receipt
                .receipt
                .get("reason")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned),
        )
        .map_err(Into::into),
        "added" | "near_duplicate" => {
            let fact_id = receipt.fact_id.as_ref().ok_or_else(|| {
                storage_message(
                    COMPATIBILITY_WRITE_OPERATION,
                    "compatibility add receipt fact is missing",
                )
            })?;
            let fact = load_compatibility_projection_tx(transaction, owner, fact_id)
                .await?
                .ok_or_else(|| {
                    storage_message(
                        COMPATIBILITY_WRITE_OPERATION,
                        "compatibility replay fact is missing",
                    )
                })?;
            let closest = if outcome == "near_duplicate" {
                Some(CompatibilityFactIdV1::new(owner.clone(), fact_id.clone())?)
            } else {
                None
            };
            CompatibilityFactAddOutcomeV1::new(
                Some(fact),
                if outcome == "added" {
                    CompatibilityFactAddDispositionV1::Added
                } else {
                    CompatibilityFactAddDispositionV1::NearDuplicate
                },
                closest,
                None,
                None,
            )
            .map_err(Into::into)
        }
        _ => Err(storage_message(
            COMPATIBILITY_WRITE_OPERATION,
            "unknown compatibility add receipt outcome",
        )
        .into()),
    }
}

async fn add_compatibility_fact_tx(
    db: &Database,
    transaction: &Transaction,
    request: &CompatibilityFactAddCommandV1,
) -> FactCompatibilityResult<CompatibilityFactAddOutcomeV1> {
    let payload_metadata = compatibility_payload_metadata(request.metadata());
    let request_digest = compatibility_digest(json!({
        "owner": request.owner(),
        "content": request.content(),
        "category": compatibility_category_label(request.category()),
        "source": request.source(),
        "tags": request.tags(),
        "entities": request.entities(),
        "metadata": &payload_metadata,
        "automation_run_id": request.automation_run_id(),
        "default_trust": request.default_trust().as_f64(),
        "actor": request.actor().map(ActorId::as_str),
    }))?;
    if let Some(receipt) = compatibility_lookup_operation_receipt_tx(
        transaction,
        request.owner(),
        request.operation_id(),
        "add",
        &request_digest,
    )
    .await?
    {
        return compatibility_replay_add_tx(transaction, request.owner(), &receipt).await;
    }
    let now = compatibility_now()?;
    let Some(sanitized) = compatibility_sanitize_payload(
        request.content(),
        request.category(),
        request.tags(),
        request.entities(),
        &payload_metadata,
    )?
    else {
        let receipt = json!({
            "outcome": "rejected_secret_like",
            "reason": "content rejected by privacy sanitizer",
        });
        compatibility_record_operation_receipt_tx(
            transaction,
            request.owner(),
            request.operation_id(),
            "add",
            &request_digest,
            None,
            None,
            &receipt,
            now,
        )
        .await?;
        return CompatibilityFactAddOutcomeV1::new(
            None,
            CompatibilityFactAddDispositionV1::RejectedSecretLike,
            None,
            None,
            Some("content rejected by privacy sanitizer".to_owned()),
        )
        .map_err(Into::into);
    };
    let source = compatibility_source_label(request.source())?;
    match compatibility_mirror_insert_tx(
        db,
        transaction,
        request.owner(),
        &sanitized.payload,
        &source,
        request.default_trust(),
        now,
    )
    .await?
    {
        CompatibilityMirrorInsertV1::Existing { fact_id, .. } => {
            let fact = load_compatibility_projection_tx(transaction, request.owner(), &fact_id)
                .await?
                .ok_or_else(|| {
                    storage_message(
                        COMPATIBILITY_WRITE_OPERATION,
                        "duplicate compatibility fact projection is missing",
                    )
                })?;
            let closest = CompatibilityFactIdV1::new(request.owner().clone(), fact_id.clone())?;
            let receipt = json!({ "outcome": "near_duplicate" });
            compatibility_record_operation_receipt_tx(
                transaction,
                request.owner(),
                request.operation_id(),
                "add",
                &request_digest,
                Some(&fact_id),
                None,
                &receipt,
                now,
            )
            .await?;
            CompatibilityFactAddOutcomeV1::new(
                Some(fact),
                CompatibilityFactAddDispositionV1::NearDuplicate,
                Some(closest),
                None,
                None,
            )
            .map_err(Into::into)
        }
        CompatibilityMirrorInsertV1::Inserted(legacy_fact_id) => {
            let (identity, mapping) =
                compatibility_legacy_mapping_for_new_fact(request.owner(), legacy_fact_id, now)?;
            let batch = compatibility_initial_batch(
                request.owner(),
                identity,
                mapping.clone(),
                sanitized.payload,
                sanitized.access,
                request.default_trust(),
                request.actor().cloned(),
                now,
            )?;
            let (canonical_receipt, _) = compatibility_commit_batch_tx(transaction, &batch).await?;
            let fact =
                load_compatibility_projection_tx(transaction, request.owner(), mapping.fact_id())
                    .await?
                    .ok_or_else(|| {
                        storage_message(
                            COMPATIBILITY_WRITE_OPERATION,
                            "added compatibility fact projection is missing",
                        )
                    })?;
            let receipt = json!({ "outcome": "added" });
            compatibility_record_operation_receipt_tx(
                transaction,
                request.owner(),
                request.operation_id(),
                "add",
                &request_digest,
                Some(mapping.fact_id()),
                Some(canonical_receipt.last_event_id()),
                &receipt,
                now,
            )
            .await?;
            CompatibilityFactAddOutcomeV1::new(
                Some(fact),
                CompatibilityFactAddDispositionV1::Added,
                None,
                None,
                None,
            )
            .map_err(Into::into)
        }
    }
}

async fn compatibility_replay_update_tx(
    transaction: &Transaction,
    owner: &FactOwnerV1,
    receipt: &CompatibilityOperationReceiptV1,
) -> FactCompatibilityResult<CompatibilityFactUpdateOutcomeV1> {
    let fact_id = receipt.fact_id.as_ref().ok_or_else(|| {
        storage_message(
            COMPATIBILITY_WRITE_OPERATION,
            "compatibility update receipt fact is missing",
        )
    })?;
    let fact = load_compatibility_projection_tx(transaction, owner, fact_id)
        .await?
        .ok_or_else(|| {
            storage_message(
                COMPATIBILITY_WRITE_OPERATION,
                "compatibility update replay fact is missing",
            )
        })?;
    let trust_delta_millionths = receipt
        .receipt
        .get("trust_delta_millionths")
        .and_then(Value::as_i64)
        .and_then(|value| i32::try_from(value).ok())
        .ok_or_else(|| {
            storage_message(
                COMPATIBILITY_WRITE_OPERATION,
                "compatibility update receipt is malformed",
            )
        })?;
    CompatibilityFactUpdateOutcomeV1::new(fact, trust_delta_millionths).map_err(Into::into)
}

async fn update_compatibility_fact_tx(
    db: &Database,
    transaction: &Transaction,
    request: &CompatibilityFactUpdateCommandV1,
) -> FactCompatibilityResult<CompatibilityFactUpdateOutcomeV1> {
    let request_digest = compatibility_digest(json!({
        "target": compatibility_target_digest(request.target())?,
        "expected_last_event_id": request.expected_last_event_id().map(FactEventId::as_str),
        "content": request.patch().content(),
        "category": request.patch().category().map(compatibility_category_label),
        "source": match request.patch().source() {
            None => json!({"changed": false}),
            Some(value) => json!({"changed": true, "value": value}),
        },
        "tags": request.patch().tags(),
        "entities": request.patch().entities(),
        "metadata": request.patch().metadata(),
        "trust": request.patch().trust().map(Confidence::as_f64),
        "actor": request.actor().map(ActorId::as_str),
    }))?;
    if let Some(receipt) = compatibility_lookup_operation_receipt_tx(
        transaction,
        request.target().owner(),
        request.operation_id(),
        "update",
        &request_digest,
    )
    .await?
    {
        return compatibility_replay_update_tx(transaction, request.target().owner(), &receipt)
            .await;
    }
    let fact_id = resolve_compatibility_target_tx(transaction, request.target())
        .await?
        .ok_or_else(|| {
            storage_message(
                COMPATIBILITY_WRITE_OPERATION,
                "compatibility update target is missing",
            )
        })?;
    let owner_key = OwnerKey::new(request.target().owner())?;
    let current = load_current_fact_tx(transaction, &owner_key, request.target().owner(), &fact_id)
        .await?
        .ok_or_else(|| {
            storage_message(
                COMPATIBILITY_WRITE_OPERATION,
                "compatibility update target is unavailable",
            )
        })?;
    let previous_payload = current
        .payload()
        .ok_or(FactStoreError::PayloadAccessMismatch)?;
    let content = request
        .patch()
        .content()
        .unwrap_or(previous_payload.content());
    let category = request
        .patch()
        .category()
        .unwrap_or(previous_payload.category());
    let tags = request.patch().tags().unwrap_or(previous_payload.tags());
    let entities = request
        .patch()
        .entities()
        .unwrap_or(previous_payload.entities());
    let metadata = request
        .patch()
        .metadata()
        .unwrap_or(previous_payload.metadata());
    let source = match request.patch().source() {
        Some(Some(source)) => compatibility_source_label(Some(source))?,
        Some(None) => "manual".to_owned(),
        None => {
            let mapping =
                compatibility_required_mapping_tx(transaction, request.target().owner(), &fact_id)
                    .await?;
            compatibility_source_for_fact_tx(transaction, &mapping).await?
        }
    };
    let Some(sanitized) =
        compatibility_sanitize_payload(content, category, tags, entities, metadata)?
    else {
        return Err(storage_message(
            COMPATIBILITY_WRITE_OPERATION,
            "compatibility update payload was rejected by the privacy sanitizer",
        )
        .into());
    };
    let new_trust = request.patch().trust().unwrap_or(current.trust());
    let now = compatibility_now()?;
    let batch = compatibility_correction_batch(
        &current,
        sanitized.payload.clone(),
        sanitized.access,
        new_trust,
        request
            .expected_last_event_id()
            .cloned()
            .or_else(|| Some(current.last_event_id().clone())),
        request.actor().cloned(),
        now,
    )?;
    let (canonical_receipt, _) = compatibility_commit_batch_tx(transaction, &batch).await?;
    let mapping =
        compatibility_required_mapping_tx(transaction, request.target().owner(), &fact_id).await?;
    compatibility_mirror_update_tx(
        db,
        transaction,
        request.target().owner(),
        mapping.legacy_fact_id(),
        &sanitized.payload,
        &source,
        new_trust,
        now,
    )
    .await?;
    let fact = load_compatibility_projection_tx(transaction, request.target().owner(), &fact_id)
        .await?
        .ok_or_else(|| {
            storage_message(
                COMPATIBILITY_WRITE_OPERATION,
                "updated compatibility projection is missing",
            )
        })?;
    let trust_delta_millionths =
        ((new_trust.as_f64() - current.trust().as_f64()) * 1_000_000.0).round() as i32;
    let receipt = json!({ "trust_delta_millionths": trust_delta_millionths });
    compatibility_record_operation_receipt_tx(
        transaction,
        request.target().owner(),
        request.operation_id(),
        "update",
        &request_digest,
        Some(&fact_id),
        Some(canonical_receipt.last_event_id()),
        &receipt,
        now,
    )
    .await?;
    CompatibilityFactUpdateOutcomeV1::new(fact, trust_delta_millionths).map_err(Into::into)
}

async fn compatibility_replay_remove_tx(
    transaction: &Transaction,
    owner: &FactOwnerV1,
    receipt: &CompatibilityOperationReceiptV1,
) -> FactCompatibilityResult<CompatibilityFactRemoveOutcomeV1> {
    let fact_id = receipt.fact_id.as_ref().ok_or_else(|| {
        storage_message(
            COMPATIBILITY_WRITE_OPERATION,
            "compatibility remove receipt fact is missing",
        )
    })?;
    let fact = load_compatibility_projection_tx(transaction, owner, fact_id)
        .await?
        .ok_or_else(|| {
            storage_message(
                COMPATIBILITY_WRITE_OPERATION,
                "compatibility remove replay fact is missing",
            )
        })?;
    let removed = receipt
        .receipt
        .get("removed")
        .and_then(Value::as_bool)
        .ok_or_else(|| {
            storage_message(
                COMPATIBILITY_WRITE_OPERATION,
                "compatibility remove receipt is malformed",
            )
        })?;
    let remaining_fact_count = compatibility_active_fact_count_tx(transaction, owner).await?;
    Ok(CompatibilityFactRemoveOutcomeV1::new(
        fact,
        removed,
        remaining_fact_count,
    ))
}

async fn remove_compatibility_fact_tx(
    db: &Database,
    transaction: &Transaction,
    request: &CompatibilityFactRemoveCommandV1,
) -> FactCompatibilityResult<CompatibilityFactRemoveOutcomeV1> {
    let request_digest = compatibility_digest(json!({
        "target": compatibility_target_digest(request.target())?,
        "expected_last_event_id": request.expected_last_event_id().map(FactEventId::as_str),
        "actor": request.actor().map(ActorId::as_str),
    }))?;
    if let Some(receipt) = compatibility_lookup_operation_receipt_tx(
        transaction,
        request.target().owner(),
        request.operation_id(),
        "remove",
        &request_digest,
    )
    .await?
    {
        return compatibility_replay_remove_tx(transaction, request.target().owner(), &receipt)
            .await;
    }
    let now = compatibility_now()?;
    let fact_id = resolve_compatibility_target_tx(transaction, request.target())
        .await?
        .ok_or_else(|| {
            storage_message(
                COMPATIBILITY_WRITE_OPERATION,
                "compatibility remove target is missing",
            )
        })?;
    let owner_key = OwnerKey::new(request.target().owner())?;
    let current = load_current_projection(transaction, &owner_key, &fact_id)
        .await?
        .ok_or_else(|| {
            storage_message(
                COMPATIBILITY_WRITE_OPERATION,
                "compatibility remove projection is missing",
            )
        })?;
    let removed = current.access != PayloadAccessState::Deleted;
    let event_id = if removed {
        let stored =
            load_current_fact_tx(transaction, &owner_key, request.target().owner(), &fact_id)
                .await?
                .ok_or_else(|| {
                    storage_message(
                        COMPATIBILITY_WRITE_OPERATION,
                        "compatibility remove target is unavailable",
                    )
                })?;
        let category = stored
            .payload()
            .ok_or(FactStoreError::PayloadAccessMismatch)?
            .category();
        let mapping =
            compatibility_required_mapping_tx(transaction, request.target().owner(), &fact_id)
                .await?;
        let batch = compatibility_removal_batch(
            request.target().owner(),
            &fact_id,
            current.access,
            request
                .expected_last_event_id()
                .cloned()
                .or_else(|| current.last_event_id.clone()),
            request.actor().cloned(),
            now,
        )?;
        let (canonical_receipt, _) = compatibility_commit_batch_tx(transaction, &batch).await?;
        compatibility_mirror_delete_tx(
            db,
            transaction,
            request.target().owner(),
            mapping.legacy_fact_id(),
            category,
            now,
        )
        .await?;
        Some(canonical_receipt.last_event_id().clone())
    } else {
        None
    };
    let fact = load_compatibility_projection_tx(transaction, request.target().owner(), &fact_id)
        .await?
        .ok_or_else(|| {
            storage_message(
                COMPATIBILITY_WRITE_OPERATION,
                "removed compatibility projection is missing",
            )
        })?;
    let remaining_fact_count =
        compatibility_active_fact_count_tx(transaction, request.target().owner()).await?;
    let receipt = json!({ "removed": removed });
    compatibility_record_operation_receipt_tx(
        transaction,
        request.target().owner(),
        request.operation_id(),
        "remove",
        &request_digest,
        Some(&fact_id),
        event_id.as_ref(),
        &receipt,
        now,
    )
    .await?;
    Ok(CompatibilityFactRemoveOutcomeV1::new(
        fact,
        removed,
        remaining_fact_count,
    ))
}

fn compatibility_receipt_u64(receipt: &Value, field: &'static str) -> FactStoreResult<u64> {
    receipt.get(field).and_then(Value::as_u64).ok_or_else(|| {
        storage_message(
            COMPATIBILITY_WRITE_OPERATION,
            format!("compatibility receipt {field} is malformed"),
        )
    })
}

fn compatibility_feedback_history_repair_receipt(
    progress: CompatibilityFeedbackRepairProgressV1,
) -> Value {
    match progress {
        CompatibilityFeedbackRepairProgressV1::Unknown => json!({ "state": "unknown" }),
        CompatibilityFeedbackRepairProgressV1::NotRequired => {
            json!({ "state": "not_required" })
        }
        CompatibilityFeedbackRepairProgressV1::Complete { processed } => {
            json!({ "state": "complete", "processed": processed })
        }
        CompatibilityFeedbackRepairProgressV1::Incomplete {
            processed,
            remaining,
        } => json!({
            "state": "incomplete",
            "processed": processed,
            "remaining": remaining,
        }),
    }
}

fn compatibility_receipt_feedback_history_repair(
    receipt: &Value,
) -> FactStoreResult<CompatibilityFeedbackRepairProgressV1> {
    let progress = receipt
        .get("feedback_history_repair")
        .and_then(Value::as_object)
        .ok_or_else(|| {
            storage_message(
                COMPATIBILITY_WRITE_OPERATION,
                "compatibility repair receipt feedback history progress is malformed",
            )
        })?;
    let state = progress
        .get("state")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            storage_message(
                COMPATIBILITY_WRITE_OPERATION,
                "compatibility repair receipt feedback history state is malformed",
            )
        })?;
    let required_u64 = |field| {
        progress.get(field).and_then(Value::as_u64).ok_or_else(|| {
            storage_message(
                COMPATIBILITY_WRITE_OPERATION,
                format!("compatibility repair receipt feedback history {field} is malformed"),
            )
        })
    };
    match state {
        "unknown" => Ok(CompatibilityFeedbackRepairProgressV1::Unknown),
        "not_required" => Ok(CompatibilityFeedbackRepairProgressV1::NotRequired),
        "complete" => Ok(CompatibilityFeedbackRepairProgressV1::Complete {
            processed: required_u64("processed")?,
        }),
        "incomplete" => {
            let remaining = match progress.get("remaining") {
                Some(Value::Null) => None,
                Some(value) => Some(value.as_u64().ok_or_else(|| {
                    storage_message(
                        COMPATIBILITY_WRITE_OPERATION,
                        "compatibility repair receipt feedback history remaining is malformed",
                    )
                })?),
                None => {
                    return Err(storage_message(
                        COMPATIBILITY_WRITE_OPERATION,
                        "compatibility repair receipt feedback history remaining is missing",
                    ));
                }
            };
            Ok(CompatibilityFeedbackRepairProgressV1::Incomplete {
                processed: required_u64("processed")?,
                remaining,
            })
        }
        _ => Err(storage_message(
            COMPATIBILITY_WRITE_OPERATION,
            "compatibility repair receipt feedback history state is unsupported",
        )),
    }
}

fn compatibility_receipt_i32(receipt: &Value, field: &'static str) -> FactStoreResult<i32> {
    receipt
        .get(field)
        .and_then(Value::as_i64)
        .and_then(|value| i32::try_from(value).ok())
        .ok_or_else(|| {
            storage_message(
                COMPATIBILITY_WRITE_OPERATION,
                format!("compatibility receipt {field} is malformed"),
            )
        })
}

fn compatibility_receipt_confidence(
    receipt: &Value,
    field: &'static str,
) -> FactStoreResult<Confidence> {
    let millionths = compatibility_receipt_u64(receipt, field)?;
    if millionths > 1_000_000 {
        return Err(storage_message(
            COMPATIBILITY_WRITE_OPERATION,
            format!("compatibility receipt {field} is out of range"),
        ));
    }
    Confidence::new(millionths as f64 / 1_000_000.0).map_err(FactStoreError::from)
}

fn compatibility_feedback_detail(value: Option<&str>) -> Option<String> {
    value
        .and_then(sanitize_provider_metadata_text)
        .filter(|value| !value.trim().is_empty())
}

fn compatibility_feedback_details(
    source: Option<&str>,
    reason: Option<&str>,
) -> (
    String,
    Option<String>,
    Option<String>,
    CompatibilityFactFeedbackDetailsAvailabilityV1,
) {
    let persisted_source = match source {
        Some(source) => compatibility_feedback_detail(Some(source)),
        None => Some("mcp".to_owned()),
    };
    let persisted_note = compatibility_feedback_detail(reason);
    let details_available = reason.is_none() || persisted_note.is_some();
    if let Some(source) = persisted_source
        && details_available
    {
        (
            source.clone(),
            Some(source),
            persisted_note,
            CompatibilityFactFeedbackDetailsAvailabilityV1::Available,
        )
    } else {
        (
            "mcp".to_owned(),
            None,
            None,
            CompatibilityFactFeedbackDetailsAvailabilityV1::Unknown,
        )
    }
}

fn compatibility_feedback_batch(
    fact: &StoredFactV1,
    new_trust: Confidence,
    expected_last_event_id: Option<FactEventId>,
    actor: Option<ActorId>,
    now: UtcMicros,
) -> FactStoreResult<FactWriteBatch> {
    let kind = if new_trust != fact.trust() {
        FactLineageEventKindV1::TrustChanged {
            previous: fact.trust(),
            current: new_trust,
            evidence_ids: Vec::new(),
        }
    } else {
        FactLineageEventKindV1::Curated {
            action: FactCurationActionV1::Retained,
            evidence_ids: Vec::new(),
        }
    };
    let event = FactLineageEventV1::new(
        fact.fact_id().clone(),
        fact.owner().clone(),
        kind,
        now,
        actor,
    )?;
    FactWriteBatch::new(
        fact.fact_id().clone(),
        fact.owner().clone(),
        None,
        vec![event],
        Vec::new(),
        Vec::new(),
        None,
        expected_last_event_id,
    )
}

fn compatibility_feedback_details_label(
    availability: CompatibilityFactFeedbackDetailsAvailabilityV1,
) -> &'static str {
    match availability {
        CompatibilityFactFeedbackDetailsAvailabilityV1::Available => "available",
        CompatibilityFactFeedbackDetailsAvailabilityV1::LegacyRedacted => "legacy_redacted",
        CompatibilityFactFeedbackDetailsAvailabilityV1::Unknown => "unknown",
    }
}

fn compatibility_feedback_details_availability(
    value: &str,
) -> FactStoreResult<CompatibilityFactFeedbackDetailsAvailabilityV1> {
    match value {
        "available" => Ok(CompatibilityFactFeedbackDetailsAvailabilityV1::Available),
        "legacy_redacted" => Ok(CompatibilityFactFeedbackDetailsAvailabilityV1::LegacyRedacted),
        "unknown" => Ok(CompatibilityFactFeedbackDetailsAvailabilityV1::Unknown),
        _ => Err(storage_message(
            COMPATIBILITY_READ_OPERATION,
            format!("unknown compatibility feedback detail availability {value:?}"),
        )),
    }
}

fn compatibility_feedback_action(
    value: &str,
) -> FactStoreResult<CompatibilityFactFeedbackActionV1> {
    match value {
        "helpful" => Ok(CompatibilityFactFeedbackActionV1::Helpful),
        "unhelpful" => Ok(CompatibilityFactFeedbackActionV1::Unhelpful),
        _ => Err(storage_message(
            COMPATIBILITY_READ_OPERATION,
            format!("unknown compatibility feedback action {value:?}"),
        )),
    }
}

#[allow(clippy::too_many_arguments)]
async fn compatibility_record_feedback_history_tx(
    transaction: &Transaction,
    owner: &FactOwnerV1,
    fact_id: &FactId,
    event_id: &FactEventId,
    legacy_feedback_event_id: i64,
    action: CompatibilityFactFeedbackActionV1,
    old_trust: Confidence,
    new_trust: Confidence,
    occurred_at: UtcMicros,
    source: Option<&str>,
    note: Option<&str>,
    availability: CompatibilityFactFeedbackDetailsAvailabilityV1,
) -> FactStoreResult<()> {
    if legacy_feedback_event_id <= 0 {
        return Err(storage_message(
            COMPATIBILITY_WRITE_OPERATION,
            "compatibility legacy feedback event id must be positive",
        ));
    }
    let key = OwnerKey::new(owner)?;
    let source_store_id = compatibility_source_store_id()?;
    transaction
        .execute(
            "INSERT INTO memory_v2_legacy_feedback_event_map(
                owner_kind, project_id, source_store_id, legacy_feedback_event_id, fact_id, event_id
             ) VALUES(?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                key.kind,
                key.project_id.as_str(),
                source_store_id.as_str(),
                legacy_feedback_event_id,
                fact_id.as_str(),
                event_id.as_str(),
            ],
        )
        .await
        .map_err(|error| storage_error(COMPATIBILITY_WRITE_OPERATION, error))?;
    transaction
        .execute(
            "INSERT INTO memory_v2_feedback_history(
                owner_kind, project_id, fact_id, event_id, action, old_trust, new_trust,
                occurred_at, source, note, details_availability
             ) VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            params![
                key.kind,
                key.project_id.as_str(),
                fact_id.as_str(),
                event_id.as_str(),
                compatibility_feedback_action_label(action),
                old_trust.as_f64(),
                new_trust.as_f64(),
                occurred_at.0,
                source,
                note,
                compatibility_feedback_details_label(availability),
            ],
        )
        .await
        .map_err(|error| storage_error(COMPATIBILITY_WRITE_OPERATION, error))?;
    Ok(())
}

async fn compatibility_replay_feedback_tx(
    transaction: &Transaction,
    owner: &FactOwnerV1,
    receipt: &CompatibilityOperationReceiptV1,
) -> FactCompatibilityResult<CompatibilityFactFeedbackOutcomeV1> {
    let fact_id = receipt.fact_id.as_ref().ok_or_else(|| {
        storage_message(
            COMPATIBILITY_WRITE_OPERATION,
            "compatibility feedback receipt fact is missing",
        )
    })?;
    let event_id = receipt.event_id.as_ref().ok_or_else(|| {
        storage_message(
            COMPATIBILITY_WRITE_OPERATION,
            "compatibility feedback receipt event is missing",
        )
    })?;
    let fact = load_compatibility_projection_tx(transaction, owner, fact_id)
        .await?
        .ok_or_else(|| {
            storage_message(
                COMPATIBILITY_WRITE_OPERATION,
                "compatibility feedback replay fact is missing",
            )
        })?;
    let legacy_feedback_event_id = i64::try_from(compatibility_receipt_u64(
        &receipt.receipt,
        "legacy_feedback_event_id",
    )?)
    .map_err(|_| {
        storage_message(
            COMPATIBILITY_WRITE_OPERATION,
            "compatibility feedback receipt legacy event id is out of range",
        )
    })?;
    CompatibilityFactFeedbackOutcomeV1::new(
        fact,
        event_id.clone(),
        Some(legacy_feedback_event_id),
        compatibility_receipt_confidence(&receipt.receipt, "old_trust_millionths")?,
        compatibility_receipt_confidence(&receipt.receipt, "new_trust_millionths")?,
        compatibility_receipt_i32(&receipt.receipt, "trust_delta_millionths")?,
        compatibility_receipt_u64(&receipt.receipt, "helpful_count")?,
        compatibility_receipt_u64(&receipt.receipt, "unhelpful_count")?,
    )
    .map_err(Into::into)
}

async fn record_compatibility_fact_feedback_tx(
    transaction: &Transaction,
    request: &CompatibilityFactFeedbackCommandV1,
) -> FactCompatibilityResult<CompatibilityFactFeedbackOutcomeV1> {
    let request_digest = compatibility_digest(json!({
        "target": compatibility_target_digest(request.target())?,
        "expected_last_event_id": request.expected_last_event_id().map(FactEventId::as_str),
        "action": compatibility_feedback_action_label(request.action()),
        "actor": request.actor().map(ActorId::as_str),
        "source": request.source(),
        "reason": request.reason(),
    }))?;
    if let Some(receipt) = compatibility_lookup_operation_receipt_tx(
        transaction,
        request.target().owner(),
        request.operation_id(),
        "feedback",
        &request_digest,
    )
    .await?
    {
        return compatibility_replay_feedback_tx(transaction, request.target().owner(), &receipt)
            .await;
    }
    let fact_id = resolve_compatibility_target_tx(transaction, request.target())
        .await?
        .ok_or_else(|| {
            storage_message(
                COMPATIBILITY_WRITE_OPERATION,
                "compatibility feedback target is missing",
            )
        })?;
    let owner_key = OwnerKey::new(request.target().owner())?;
    let current = load_current_fact_tx(transaction, &owner_key, request.target().owner(), &fact_id)
        .await?
        .ok_or_else(|| {
            storage_message(
                COMPATIBILITY_WRITE_OPERATION,
                "compatibility feedback target is unavailable",
            )
        })?;
    let old_trust = current.trust();
    let new_trust = Confidence::new(
        (old_trust.as_f64() + compatibility_feedback_delta(request.action())).clamp(0.0, 1.0),
    )
    .map_err(FactStoreError::from)?;
    let now = compatibility_now()?;
    let batch = compatibility_feedback_batch(
        &current,
        new_trust,
        request
            .expected_last_event_id()
            .cloned()
            .or_else(|| Some(current.last_event_id().clone())),
        request.actor().cloned(),
        now,
    )?;
    let (canonical_receipt, _) = compatibility_commit_batch_tx(transaction, &batch).await?;
    let event_id = canonical_receipt.last_event_id().clone();
    let mapping =
        compatibility_required_mapping_tx(transaction, request.target().owner(), &fact_id).await?;
    let (mirror_source, history_source, history_note, availability) =
        compatibility_feedback_details(request.source(), request.reason());
    let legacy_feedback_event_id = compatibility_mirror_feedback_tx(
        transaction,
        mapping.legacy_fact_id(),
        request.action(),
        old_trust,
        new_trust,
        compatibility_legacy_timestamp(now),
        &mirror_source,
        history_note.as_deref(),
    )
    .await?;
    compatibility_record_feedback_history_tx(
        transaction,
        request.target().owner(),
        &fact_id,
        &event_id,
        legacy_feedback_event_id,
        request.action(),
        old_trust,
        new_trust,
        now,
        history_source.as_deref(),
        history_note.as_deref(),
        availability,
    )
    .await?;
    compatibility_update_feedback_projection_tx(
        transaction,
        request.target().owner(),
        &fact_id,
        request.action(),
        now,
    )
    .await?;
    let fact = load_compatibility_projection_tx(transaction, request.target().owner(), &fact_id)
        .await?
        .ok_or_else(|| {
            storage_message(
                COMPATIBILITY_WRITE_OPERATION,
                "compatibility feedback projection is missing",
            )
        })?;
    let (_, _, telemetry) = compatibility_projection_metadata_tx(
        transaction,
        request.target().owner(),
        &fact_id,
        Some(&mapping),
    )
    .await?;
    let trust_delta_millionths =
        ((new_trust.as_f64() - old_trust.as_f64()) * 1_000_000.0).round() as i32;
    let receipt = json!({
        "old_trust_millionths": compatibility_millionths(old_trust.as_f64()),
        "new_trust_millionths": compatibility_millionths(new_trust.as_f64()),
        "trust_delta_millionths": trust_delta_millionths,
        "helpful_count": telemetry.helpful_count(),
        "unhelpful_count": telemetry.unhelpful_count(),
        "legacy_feedback_event_id": legacy_feedback_event_id,
    });
    compatibility_record_operation_receipt_tx(
        transaction,
        request.target().owner(),
        request.operation_id(),
        "feedback",
        &request_digest,
        Some(&fact_id),
        Some(&event_id),
        &receipt,
        now,
    )
    .await?;
    CompatibilityFactFeedbackOutcomeV1::new(
        fact,
        event_id,
        Some(legacy_feedback_event_id),
        old_trust,
        new_trust,
        trust_delta_millionths,
        telemetry.helpful_count(),
        telemetry.unhelpful_count(),
    )
    .map_err(Into::into)
}

async fn compatibility_replay_retrieval_tx(
    transaction: &Transaction,
    owner: &FactOwnerV1,
    receipt: &CompatibilityOperationReceiptV1,
) -> FactCompatibilityResult<Vec<CompatibilityFactProjectionV1>> {
    let fact_ids = receipt
        .receipt
        .get("fact_ids")
        .and_then(Value::as_array)
        .ok_or_else(|| {
            storage_message(
                COMPATIBILITY_WRITE_OPERATION,
                "compatibility retrieval receipt fact ids are missing",
            )
        })?;
    let mut facts = Vec::with_capacity(fact_ids.len());
    for value in fact_ids {
        let fact_id = FactId::new(value.as_str().ok_or_else(|| {
            storage_message(
                COMPATIBILITY_WRITE_OPERATION,
                "compatibility retrieval receipt fact id is malformed",
            )
        })?)
        .map_err(FactStoreError::from)?;
        let fact = load_compatibility_projection_tx(transaction, owner, &fact_id)
            .await?
            .ok_or_else(|| {
                storage_message(
                    COMPATIBILITY_WRITE_OPERATION,
                    "compatibility retrieval replay fact is missing",
                )
            })?;
        facts.push(fact);
    }
    Ok(facts)
}

async fn record_compatibility_fact_retrieval_tx(
    transaction: &Transaction,
    request: &CompatibilityFactRetrievalCommandV1,
) -> FactCompatibilityResult<Vec<CompatibilityFactProjectionV1>> {
    let request_digest = compatibility_digest(json!({
        "targets": request
            .targets()
            .iter()
            .map(compatibility_target_digest)
            .collect::<FactStoreResult<Vec<_>>>()?,
        "recall": request.recall(),
    }))?;
    if let Some(receipt) = compatibility_lookup_operation_receipt_tx(
        transaction,
        request.owner(),
        request.operation_id(),
        "retrieval",
        &request_digest,
    )
    .await?
    {
        return compatibility_replay_retrieval_tx(transaction, request.owner(), &receipt).await;
    }
    let mut fact_ids = Vec::with_capacity(request.targets().len());
    let mut seen = BTreeSet::new();
    for target in request.targets() {
        let fact_id = resolve_compatibility_target_tx(transaction, target)
            .await?
            .ok_or_else(|| {
                storage_message(
                    COMPATIBILITY_WRITE_OPERATION,
                    "compatibility retrieval target is missing",
                )
            })?;
        if seen.insert(fact_id.clone()) {
            fact_ids.push(fact_id);
        }
    }
    let owner_key = OwnerKey::new(request.owner())?;
    for fact_id in &fact_ids {
        if load_current_fact_tx(transaction, &owner_key, request.owner(), fact_id)
            .await?
            .is_none()
        {
            return Err(storage_message(
                COMPATIBILITY_WRITE_OPERATION,
                "compatibility retrieval target is unavailable",
            )
            .into());
        }
    }
    let now = compatibility_now()?;
    for fact_id in &fact_ids {
        compatibility_update_retrieval_projection_tx(
            transaction,
            request.owner(),
            fact_id,
            request.recall(),
            now,
        )
        .await?;
    }
    let mut facts = Vec::with_capacity(fact_ids.len());
    for fact_id in &fact_ids {
        facts.push(
            load_compatibility_projection_tx(transaction, request.owner(), fact_id)
                .await?
                .ok_or_else(|| {
                    storage_message(
                        COMPATIBILITY_WRITE_OPERATION,
                        "compatibility retrieval projection is missing",
                    )
                })?,
        );
    }
    let receipt = json!({
        "fact_ids": fact_ids.iter().map(FactId::as_str).collect::<Vec<_>>(),
    });
    compatibility_record_operation_receipt_tx(
        transaction,
        request.owner(),
        request.operation_id(),
        "retrieval",
        &request_digest,
        None,
        None,
        &receipt,
        now,
    )
    .await?;
    Ok(facts)
}

async fn compatibility_fact_feedback_history_tx(
    transaction: &Transaction,
    query: &CompatibilityFactFeedbackHistoryQueryV1,
    repair_progress: CompatibilityFeedbackRepairProgressV1,
) -> FactCompatibilityResult<CompatibilityFactFeedbackHistoryV1> {
    let fact_id = resolve_compatibility_target_tx(transaction, query.target())
        .await?
        .ok_or_else(|| {
            storage_message(
                COMPATIBILITY_READ_OPERATION,
                "compatibility feedback history target is missing",
            )
        })?;
    let key = OwnerKey::new(query.target().owner())?;
    let fetch_limit = i64::try_from(query.limit().saturating_add(1)).map_err(|_| {
        FactStoreError::InvalidQueryLimit {
            limit: query.limit(),
            max: usize::MAX,
        }
    })?;
    let after_time = query
        .after()
        .map(FactLineageCursor::occurred_at)
        .map(|time| time.0);
    let after_event = query.after().map(|cursor| cursor.event_id().as_str());
    let mut rows = transaction
        .query(
            "SELECT event_id, occurred_at, action, old_trust, new_trust,
                    source, note, details_availability
             FROM memory_v2_feedback_history
             WHERE owner_kind = ?1 AND project_id = ?2 AND fact_id = ?3
               AND (
                    ?4 IS NULL
                    OR occurred_at > ?4
                    OR (occurred_at = ?4 AND event_id > ?5)
               )
             ORDER BY occurred_at ASC, event_id ASC
             LIMIT ?6",
            params![
                key.kind,
                key.project_id.as_str(),
                fact_id.as_str(),
                after_time,
                after_event,
                fetch_limit,
            ],
        )
        .await
        .map_err(|error| storage_error(COMPATIBILITY_READ_OPERATION, error))?;
    let mut events = Vec::with_capacity(query.limit().saturating_add(1));
    while let Some(row) = rows
        .next()
        .await
        .map_err(|error| storage_error(COMPATIBILITY_READ_OPERATION, error))?
    {
        events.push(CompatibilityFactFeedbackHistoryEntryV1::new(
            FactEventId::new(row_string(&row, 0, COMPATIBILITY_READ_OPERATION)?)
                .map_err(FactStoreError::from)?,
            UtcMicros(row_i64(&row, 1, COMPATIBILITY_READ_OPERATION)?),
            compatibility_feedback_action(&row_string(&row, 2, COMPATIBILITY_READ_OPERATION)?)?,
            Confidence::new(row_f64(&row, 3, COMPATIBILITY_READ_OPERATION)?)
                .map_err(FactStoreError::from)?,
            Confidence::new(row_f64(&row, 4, COMPATIBILITY_READ_OPERATION)?)
                .map_err(FactStoreError::from)?,
            row_optional_string(&row, 5, COMPATIBILITY_READ_OPERATION)?,
            row_optional_string(&row, 6, COMPATIBILITY_READ_OPERATION)?,
            compatibility_feedback_details_availability(&row_string(
                &row,
                7,
                COMPATIBILITY_READ_OPERATION,
            )?)?,
        )?);
    }
    let has_more = events.len() > query.limit();
    events.truncate(query.limit());
    let next_after = has_more
        .then(|| {
            events
                .last()
                .map(|event| FactLineageCursor::new(event.occurred_at(), event.event_id().clone()))
        })
        .flatten()
        .transpose()?;
    CompatibilityFactFeedbackHistoryV1::new_with_repair_progress(
        query.target().owner().clone(),
        events,
        next_after,
        repair_progress,
    )
    .map_err(Into::into)
}

fn compatibility_relation_label(relation: CompatibilityFactRelationV1) -> &'static str {
    match relation {
        CompatibilityFactRelationV1::Supports => "supports",
        CompatibilityFactRelationV1::Contradicts => "contradicts",
        CompatibilityFactRelationV1::Supersedes => "supersedes",
        CompatibilityFactRelationV1::DerivedFrom => "derived_from",
    }
}

fn compatibility_relations_conflict(
    left: CompatibilityFactRelationV1,
    right: CompatibilityFactRelationV1,
) -> bool {
    matches!(
        (left, right),
        (
            CompatibilityFactRelationV1::Supports,
            CompatibilityFactRelationV1::Contradicts
        ) | (
            CompatibilityFactRelationV1::Contradicts,
            CompatibilityFactRelationV1::Supports
        )
    )
}

fn compatibility_normalize_tags(tags: &[String]) -> Vec<String> {
    tags.iter()
        .map(|tag| {
            tag.trim()
                .to_ascii_lowercase()
                .split_whitespace()
                .collect::<Vec<_>>()
                .join("_")
                .replace('-', "_")
        })
        .filter(|tag| !tag.is_empty())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

async fn compatibility_available_curation_fact_tx(
    transaction: &Transaction,
    target: &CompatibilityFactTargetV1,
) -> FactStoreResult<(FactId, StoredFactV1, CompatibilityFactMappingV1)> {
    let fact_id = resolve_compatibility_target_tx(transaction, target)
        .await?
        .ok_or_else(|| {
            storage_message(
                COMPATIBILITY_WRITE_OPERATION,
                "compatibility curation target is missing",
            )
        })?;
    let owner_key = OwnerKey::new(target.owner())?;
    let fact = load_current_fact_tx(transaction, &owner_key, target.owner(), &fact_id)
        .await?
        .ok_or_else(|| {
            storage_message(
                COMPATIBILITY_WRITE_OPERATION,
                "compatibility curation target is unavailable",
            )
        })?;
    if fact.payload().is_none() {
        return Err(FactStoreError::PayloadAccessMismatch);
    }
    let mapping = compatibility_required_mapping_tx(transaction, target.owner(), &fact_id).await?;
    let mapping = CompatibilityFactMappingV1::new(
        CompatibilityFactIdV1::new(target.owner().clone(), fact_id.clone())?,
        Some(mapping),
    )?;
    Ok((fact_id, fact, mapping))
}

async fn compatibility_curation_evidence_ids_tx(
    transaction: &Transaction,
    owner: &FactOwnerV1,
    evidence: &[CompatibilityFactTargetV1],
) -> FactStoreResult<Vec<FactId>> {
    let mut ids = Vec::with_capacity(evidence.len());
    let mut seen = BTreeSet::new();
    for target in evidence {
        if target.owner() != owner {
            return Err(FactStoreError::OwnerMismatch);
        }
        let (fact_id, _, _) = compatibility_available_curation_fact_tx(transaction, target).await?;
        if !seen.insert(fact_id.clone()) {
            return Err(storage_message(
                COMPATIBILITY_WRITE_OPERATION,
                "compatibility curation evidence resolved to duplicate facts",
            ));
        }
        ids.push(fact_id);
    }
    Ok(ids)
}

async fn compatibility_curation_mappings_from_ids_tx(
    transaction: &Transaction,
    owner: &FactOwnerV1,
    ids: &[FactId],
) -> FactStoreResult<Vec<CompatibilityFactMappingV1>> {
    let mut mappings = Vec::with_capacity(ids.len());
    let mut seen = BTreeSet::new();
    for fact_id in ids {
        if !seen.insert(fact_id.clone()) {
            continue;
        }
        let legacy_mapping = compatibility_required_mapping_tx(transaction, owner, fact_id).await?;
        mappings.push(CompatibilityFactMappingV1::new(
            CompatibilityFactIdV1::new(owner.clone(), fact_id.clone())?,
            Some(legacy_mapping),
        )?);
    }
    Ok(mappings)
}

async fn compatibility_sanitized_relation_metadata(metadata: &Value) -> FactStoreResult<Value> {
    match sanitize_memory_fact_payload(metadata.clone())
        .map_err(|error| storage_error(COMPATIBILITY_WRITE_OPERATION, error))?
    {
        MemoryFactSanitizationV1::Durable { payload, .. } => Ok(payload),
        MemoryFactSanitizationV1::Quarantined => Err(storage_message(
            COMPATIBILITY_WRITE_OPERATION,
            "compatibility relation metadata was rejected by the privacy sanitizer",
        )),
    }
}

#[allow(clippy::too_many_arguments)]
async fn compatibility_upsert_legacy_relation_tx(
    transaction: &Transaction,
    source_legacy_fact_id: i64,
    target_legacy_fact_id: i64,
    relation: CompatibilityFactRelationV1,
    confidence: Confidence,
    source_label: &str,
    metadata: &Value,
    timestamp: i64,
) -> FactStoreResult<()> {
    let mut rows = transaction
        .query(
            "SELECT relation FROM memory_fact_relations
             WHERE source_fact_id = ?1 AND target_fact_id = ?2",
            params![source_legacy_fact_id, target_legacy_fact_id],
        )
        .await
        .map_err(|error| storage_error(COMPATIBILITY_WRITE_OPERATION, error))?;
    while let Some(row) = rows
        .next()
        .await
        .map_err(|error| storage_error(COMPATIBILITY_WRITE_OPERATION, error))?
    {
        let stored = match row_string(&row, 0, COMPATIBILITY_WRITE_OPERATION)?.as_str() {
            "supports" => CompatibilityFactRelationV1::Supports,
            "contradicts" => CompatibilityFactRelationV1::Contradicts,
            "supersedes" => CompatibilityFactRelationV1::Supersedes,
            "derived_from" => CompatibilityFactRelationV1::DerivedFrom,
            _ => {
                return Err(storage_message(
                    COMPATIBILITY_WRITE_OPERATION,
                    "legacy compatibility relation has an unsupported kind",
                ));
            }
        };
        if compatibility_relations_conflict(stored, relation) {
            return Err(storage_message(
                COMPATIBILITY_WRITE_OPERATION,
                "compatibility relation conflicts with an existing relation",
            ));
        }
    }
    drop(rows);
    transaction
        .execute(
            "INSERT INTO memory_fact_relations(
                source_fact_id, target_fact_id, relation, confidence, source, metadata, created_at, updated_at
             ) VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?7)
             ON CONFLICT(source_fact_id, target_fact_id, relation) DO UPDATE SET
                confidence = excluded.confidence,
                source = excluded.source,
                metadata = excluded.metadata,
                updated_at = excluded.updated_at",
            params![
                source_legacy_fact_id,
                target_legacy_fact_id,
                compatibility_relation_label(relation),
                confidence.as_f64(),
                source_label,
                to_json(metadata, "serialize compatibility relation metadata")?,
                timestamp,
            ],
        )
        .await
        .map_err(|error| storage_error(COMPATIBILITY_WRITE_OPERATION, error))?;
    Ok(())
}

async fn compatibility_link_facts_tx(
    transaction: &Transaction,
    owner: &FactOwnerV1,
    actor: Option<&ActorId>,
    operation: &CompatibilityFactLinkV1,
    now: UtcMicros,
) -> FactStoreResult<(Vec<FactId>, Option<FactEventId>)> {
    let (source_fact_id, source_fact, source_mapping) =
        compatibility_available_curation_fact_tx(transaction, operation.source()).await?;
    let (target_fact_id, _, target_mapping) =
        compatibility_available_curation_fact_tx(transaction, operation.target()).await?;
    if source_fact_id == target_fact_id {
        return Err(storage_message(
            COMPATIBILITY_WRITE_OPERATION,
            "compatibility curation relation cannot target itself",
        ));
    }
    let evidence_fact_ids =
        compatibility_curation_evidence_ids_tx(transaction, owner, operation.evidence_facts())
            .await?;
    let source_label = compatibility_source_label(Some(operation.source_label()))?;
    let metadata = compatibility_sanitized_relation_metadata(operation.metadata()).await?;
    let key = OwnerKey::new(owner)?;
    let evidence_fact_ids_json = to_json(
        &evidence_fact_ids
            .iter()
            .map(FactId::as_str)
            .collect::<Vec<_>>(),
        "serialize compatibility relation evidence",
    )?;
    let provenance_json = to_json(&metadata, "serialize compatibility relation provenance")?;
    transaction
        .execute(
            "INSERT INTO memory_v2_fact_relations(
                owner_kind, project_id, source_fact_id, target_fact_id, relation,
                confidence, source_label, provenance_json, evidence_fact_ids_json,
                occurred_at, updated_at
             ) VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?10)
             ON CONFLICT(owner_kind, project_id, source_fact_id, target_fact_id, relation)
             DO UPDATE SET confidence = excluded.confidence,
                           source_label = excluded.source_label,
                           provenance_json = excluded.provenance_json,
                           evidence_fact_ids_json = excluded.evidence_fact_ids_json,
                           updated_at = excluded.updated_at",
            params![
                key.kind,
                key.project_id.as_str(),
                source_fact_id.as_str(),
                target_fact_id.as_str(),
                compatibility_relation_label(operation.relation()),
                operation.confidence().as_f64(),
                source_label.clone(),
                provenance_json,
                evidence_fact_ids_json,
                now.0,
            ],
        )
        .await
        .map_err(|error| storage_error(COMPATIBILITY_WRITE_OPERATION, error))?;
    let event_id = match operation.relation() {
        CompatibilityFactRelationV1::Supports | CompatibilityFactRelationV1::DerivedFrom => None,
        CompatibilityFactRelationV1::Contradicts | CompatibilityFactRelationV1::Supersedes => {
            let action = match operation.relation() {
                CompatibilityFactRelationV1::Contradicts => FactCurationActionV1::ContradictedBy {
                    fact_id: target_fact_id.clone(),
                },
                CompatibilityFactRelationV1::Supersedes => FactCurationActionV1::SupersededBy {
                    fact_id: target_fact_id.clone(),
                },
                _ => unreachable!("handled typed relation variants above"),
            };
            let event = FactLineageEventV1::new(
                source_fact_id.clone(),
                owner.clone(),
                FactLineageEventKindV1::Curated {
                    action,
                    // LinkFacts provenance is owner-scoped FactId data above. This V1 lineage
                    // field accepts only source-owned FactEvidenceId values.
                    evidence_ids: Vec::new(),
                },
                now,
                actor.cloned(),
            )?;
            let batch = FactWriteBatch::new(
                source_fact_id.clone(),
                owner.clone(),
                None,
                vec![event],
                Vec::new(),
                Vec::new(),
                None,
                Some(source_fact.last_event_id().clone()),
            )?;
            let (receipt, _) = compatibility_commit_batch_tx(transaction, &batch).await?;
            Some(receipt.last_event_id().clone())
        }
    };
    compatibility_upsert_legacy_relation_tx(
        transaction,
        source_mapping
            .legacy_fact_id()
            .ok_or(FactStoreError::FactMismatch)?,
        target_mapping
            .legacy_fact_id()
            .ok_or(FactStoreError::FactMismatch)?,
        operation.relation(),
        operation.confidence(),
        &source_label,
        &metadata,
        compatibility_legacy_timestamp(now),
    )
    .await?;
    Ok((vec![source_fact_id, target_fact_id], event_id))
}

fn compatibility_curated_correction_batch(
    fact: &StoredFactV1,
    payload: FactPayloadV1,
    actor: Option<ActorId>,
    now: UtcMicros,
) -> FactStoreResult<FactWriteBatch> {
    let assertion = FactAssertionV1::new(
        fact.fact_id().clone(),
        fact.owner().clone(),
        FactAssertionKindV1::Correction {
            supersedes: fact.active_assertion_id().clone(),
        },
        payload,
        Vec::new(),
        now,
        actor.clone(),
    )?;
    let recorded = FactLineageEventV1::new(
        fact.fact_id().clone(),
        fact.owner().clone(),
        FactLineageEventKindV1::AssertionRecorded {
            assertion_id: assertion.assertion_id().clone(),
        },
        now,
        actor.clone(),
    )?;
    let curated = FactLineageEventV1::new(
        fact.fact_id().clone(),
        fact.owner().clone(),
        FactLineageEventKindV1::Curated {
            action: FactCurationActionV1::Retained,
            evidence_ids: Vec::new(),
        },
        compatibility_event_time(now, 1)?,
        actor,
    )?;
    FactWriteBatch::new(
        fact.fact_id().clone(),
        fact.owner().clone(),
        Some(assertion),
        vec![recorded, curated],
        Vec::new(),
        Vec::new(),
        None,
        Some(fact.last_event_id().clone()),
    )
}

async fn compatibility_normalize_tags_tx(
    db: &Database,
    transaction: &Transaction,
    owner: &FactOwnerV1,
    actor: Option<&ActorId>,
    operation: &CompatibilityFactNormalizeTagsV1,
    now: UtcMicros,
) -> FactStoreResult<FactId> {
    let _evidence =
        compatibility_curation_evidence_ids_tx(transaction, owner, operation.evidence_facts())
            .await?;
    let (fact_id, fact, mapping) =
        compatibility_available_curation_fact_tx(transaction, operation.fact()).await?;
    let payload = fact
        .payload()
        .ok_or(FactStoreError::PayloadAccessMismatch)?;
    let tags = compatibility_normalize_tags(operation.tags());
    let Some(sanitized) = compatibility_sanitize_payload(
        payload.content(),
        payload.category(),
        &tags,
        payload.entities(),
        payload.metadata(),
    )?
    else {
        return Err(storage_message(
            COMPATIBILITY_WRITE_OPERATION,
            "compatibility normalized tags were rejected by the privacy sanitizer",
        ));
    };
    let source = compatibility_source_for_fact_tx(
        transaction,
        mapping
            .legacy_mapping()
            .ok_or(FactStoreError::FactMismatch)?,
    )
    .await?;
    let batch = compatibility_curated_correction_batch(
        &fact,
        sanitized.payload.clone(),
        actor.cloned(),
        now,
    )?;
    compatibility_commit_batch_tx(transaction, &batch).await?;
    compatibility_mirror_update_tx(
        db,
        transaction,
        owner,
        mapping
            .legacy_fact_id()
            .ok_or(FactStoreError::FactMismatch)?,
        &sanitized.payload,
        &source,
        fact.trust(),
        now,
    )
    .await?;
    Ok(fact_id)
}

async fn compatibility_repair_vector_for_fact_tx(
    db: &Database,
    transaction: &Transaction,
    owner: &FactOwnerV1,
    operation: &CompatibilityFactRepairVectorV1,
    now: UtcMicros,
) -> FactStoreResult<FactId> {
    let _evidence =
        compatibility_curation_evidence_ids_tx(transaction, owner, operation.evidence_facts())
            .await?;
    let (fact_id, fact, mapping) =
        compatibility_available_curation_fact_tx(transaction, operation.fact()).await?;
    let payload = fact
        .payload()
        .ok_or(FactStoreError::PayloadAccessMismatch)?;
    let changed = transaction
        .execute(
            "UPDATE memory_facts SET
                hrr_vector = ?1, hrr_algebra = 'amari_fhrr', hrr_dim = ?2, hrr_precision = ?3,
                updated_at = ?4
             WHERE fact_id = ?5",
            params![
                compatibility_mirror_vector(payload)?,
                HolographicEncoder::DIMENSIONS as i64,
                HolographicEncoder::HRR_PRECISION,
                compatibility_legacy_timestamp(now),
                mapping
                    .legacy_fact_id()
                    .ok_or(FactStoreError::FactMismatch)?,
            ],
        )
        .await
        .map_err(|error| storage_error(COMPATIBILITY_WRITE_OPERATION, error))?;
    if changed != 1 {
        return Err(storage_message(
            COMPATIBILITY_WRITE_OPERATION,
            "compatibility vector target is missing from the legacy mirror",
        ));
    }
    compatibility_mark_owner_banks_dirty_tx(db, transaction, owner, payload.category(), now)
        .await?;
    Ok(fact_id)
}

async fn compatibility_owner_entity_tx(
    transaction: &Transaction,
    owner: &FactOwnerV1,
    entity_id: i64,
) -> FactStoreResult<(String, Vec<String>)> {
    let key = OwnerKey::new(owner)?;
    let source_store_id = compatibility_source_store_id()?;
    let foreign_links = transaction
        .query(
            "SELECT COUNT(*)
             FROM memory_fact_entities AS links
             LEFT JOIN memory_v2_legacy_map AS mappings
               ON mappings.legacy_fact_id = links.fact_id
             WHERE links.entity_id = ?1
               AND (
                    mappings.legacy_fact_id IS NULL
                    OR mappings.owner_kind <> ?2
                    OR mappings.project_id <> ?3
                    OR mappings.owner_json <> ?4
                    OR mappings.source_store_id <> ?5
               )",
            params![
                entity_id,
                key.kind,
                key.project_id.as_str(),
                key.json.as_str(),
                source_store_id.as_str(),
            ],
        )
        .await
        .map_err(|error| storage_error(COMPATIBILITY_WRITE_OPERATION, error))?;
    let mut foreign_links = foreign_links;
    let row = foreign_links
        .next()
        .await
        .map_err(|error| storage_error(COMPATIBILITY_WRITE_OPERATION, error))?
        .ok_or_else(|| {
            storage_message(
                COMPATIBILITY_WRITE_OPERATION,
                "compatibility entity ownership count is missing",
            )
        })?;
    if row_i64(&row, 0, COMPATIBILITY_WRITE_OPERATION)? != 0 {
        return Err(storage_message(
            COMPATIBILITY_WRITE_OPERATION,
            "compatibility curation entity is shared outside this owner",
        ));
    }
    drop(foreign_links);
    let mut rows = transaction
        .query(
            "SELECT name, aliases FROM memory_entities WHERE entity_id = ?1",
            params![entity_id],
        )
        .await
        .map_err(|error| storage_error(COMPATIBILITY_WRITE_OPERATION, error))?;
    let row = rows
        .next()
        .await
        .map_err(|error| storage_error(COMPATIBILITY_WRITE_OPERATION, error))?
        .ok_or_else(|| {
            storage_message(
                COMPATIBILITY_WRITE_OPERATION,
                "compatibility curation entity is missing",
            )
        })?;
    Ok((
        row_string(&row, 0, COMPATIBILITY_WRITE_OPERATION)?,
        from_json::<Vec<String>>(
            &row_string(&row, 1, COMPATIBILITY_WRITE_OPERATION)?,
            COMPATIBILITY_WRITE_OPERATION,
        )?,
    ))
}

async fn compatibility_entity_linked_to_evidence_tx(
    transaction: &Transaction,
    owner: &FactOwnerV1,
    entity_id: i64,
    evidence_ids: &[FactId],
) -> FactStoreResult<()> {
    let key = OwnerKey::new(owner)?;
    let source_store_id = compatibility_source_store_id()?;
    let placeholders = std::iter::repeat_n("?", evidence_ids.len())
        .collect::<Vec<_>>()
        .join(",");
    let sql = format!(
        "SELECT 1
         FROM memory_fact_entities AS links
         JOIN memory_v2_legacy_map AS mappings ON mappings.legacy_fact_id = links.fact_id
         WHERE links.entity_id = ?
           AND mappings.owner_kind = ? AND mappings.project_id = ?
           AND mappings.owner_json = ? AND mappings.source_store_id = ?
           AND mappings.fact_id IN ({placeholders})
         LIMIT 1"
    );
    let mut values = Vec::with_capacity(evidence_ids.len() + 5);
    values.push(libsql::Value::Integer(entity_id));
    values.push(libsql::Value::Text(key.kind.to_string()));
    values.push(libsql::Value::Text(key.project_id.clone()));
    values.push(libsql::Value::Text(key.json.clone()));
    values.push(libsql::Value::Text(source_store_id.as_str().to_owned()));
    values.extend(
        evidence_ids
            .iter()
            .map(|fact_id| libsql::Value::Text(fact_id.as_str().to_owned())),
    );
    let mut rows = transaction
        .query(&sql, values)
        .await
        .map_err(|error| storage_error(COMPATIBILITY_WRITE_OPERATION, error))?;
    if rows
        .next()
        .await
        .map_err(|error| storage_error(COMPATIBILITY_WRITE_OPERATION, error))?
        .is_none()
    {
        return Err(storage_message(
            COMPATIBILITY_WRITE_OPERATION,
            "compatibility curation entity is not linked to supplied evidence",
        ));
    }
    Ok(())
}

async fn compatibility_owner_entity_fact_ids_tx(
    transaction: &Transaction,
    owner: &FactOwnerV1,
    entity_ids: &[i64],
) -> FactStoreResult<Vec<FactId>> {
    let key = OwnerKey::new(owner)?;
    let source_store_id = compatibility_source_store_id()?;
    let placeholders = std::iter::repeat_n("?", entity_ids.len())
        .collect::<Vec<_>>()
        .join(",");
    let sql = format!(
        "SELECT DISTINCT mappings.fact_id
         FROM memory_fact_entities AS links
         JOIN memory_v2_legacy_map AS mappings ON mappings.legacy_fact_id = links.fact_id
         WHERE mappings.owner_kind = ? AND mappings.project_id = ?
           AND mappings.owner_json = ? AND mappings.source_store_id = ?
           AND links.entity_id IN ({placeholders})
         ORDER BY mappings.fact_id ASC LIMIT 257"
    );
    let mut values = Vec::with_capacity(entity_ids.len() + 4);
    values.push(libsql::Value::Text(key.kind.to_string()));
    values.push(libsql::Value::Text(key.project_id.clone()));
    values.push(libsql::Value::Text(key.json.clone()));
    values.push(libsql::Value::Text(source_store_id.as_str().to_owned()));
    values.extend(entity_ids.iter().copied().map(libsql::Value::Integer));
    let mut rows = transaction
        .query(&sql, values)
        .await
        .map_err(|error| storage_error(COMPATIBILITY_WRITE_OPERATION, error))?;
    let mut fact_ids = Vec::new();
    while let Some(row) = rows
        .next()
        .await
        .map_err(|error| storage_error(COMPATIBILITY_WRITE_OPERATION, error))?
    {
        fact_ids.push(
            FactId::new(row_string(&row, 0, COMPATIBILITY_WRITE_OPERATION)?)
                .map_err(FactStoreError::from)?,
        );
    }
    if fact_ids.len() > 256 {
        return Err(storage_message(
            COMPATIBILITY_WRITE_OPERATION,
            "compatibility entity curation exceeds the fixed 256-fact bound",
        ));
    }
    Ok(fact_ids)
}

async fn compatibility_fact_entities_tx(
    transaction: &Transaction,
    legacy_fact_id: i64,
) -> FactStoreResult<Vec<String>> {
    let mut rows = transaction
        .query(
            "SELECT entities.name
             FROM memory_fact_entities AS links
             JOIN memory_entities AS entities ON entities.entity_id = links.entity_id
             WHERE links.fact_id = ?1
             ORDER BY entities.normalized_name ASC, entities.entity_id ASC",
            params![legacy_fact_id],
        )
        .await
        .map_err(|error| storage_error(COMPATIBILITY_WRITE_OPERATION, error))?;
    let mut entities = Vec::new();
    while let Some(row) = rows
        .next()
        .await
        .map_err(|error| storage_error(COMPATIBILITY_WRITE_OPERATION, error))?
    {
        entities.push(row_string(&row, 0, COMPATIBILITY_WRITE_OPERATION)?);
    }
    Ok(entities)
}

async fn compatibility_merge_entities_tx(
    db: &Database,
    transaction: &Transaction,
    owner: &FactOwnerV1,
    actor: Option<&ActorId>,
    operation: &CompatibilityFactMergeEntitiesV1,
    now: UtcMicros,
) -> FactStoreResult<Vec<FactId>> {
    let evidence =
        compatibility_curation_evidence_ids_tx(transaction, owner, operation.evidence_facts())
            .await?;
    let winner_id = operation.winner().legacy_entity_id();
    let (winner_name, winner_aliases) =
        compatibility_owner_entity_tx(transaction, owner, winner_id).await?;
    compatibility_entity_linked_to_evidence_tx(transaction, owner, winner_id, &evidence).await?;
    let mut entity_ids = vec![winner_id];
    let mut aliases = winner_aliases;
    for loser in operation.losers() {
        let loser_id = loser.legacy_entity_id();
        let (name, loser_aliases) =
            compatibility_owner_entity_tx(transaction, owner, loser_id).await?;
        compatibility_entity_linked_to_evidence_tx(transaction, owner, loser_id, &evidence).await?;
        entity_ids.push(loser_id);
        aliases.push(name);
        aliases.extend(loser_aliases);
    }
    let fact_ids = compatibility_owner_entity_fact_ids_tx(transaction, owner, &entity_ids).await?;
    let mut normalized_aliases = std::collections::BTreeMap::new();
    for alias in aliases {
        let alias = normalize_entity(&alias);
        if !alias.is_empty() && !alias.eq_ignore_ascii_case(&winner_name) {
            normalized_aliases
                .entry(alias.to_ascii_lowercase())
                .or_insert(alias);
        }
    }
    transaction
        .execute(
            "UPDATE memory_entities SET aliases = ?1, updated_at = ?2 WHERE entity_id = ?3",
            params![
                to_json(
                    &normalized_aliases.into_values().collect::<Vec<_>>(),
                    "serialize compatibility entity aliases",
                )?,
                compatibility_legacy_timestamp(now),
                winner_id,
            ],
        )
        .await
        .map_err(|error| storage_error(COMPATIBILITY_WRITE_OPERATION, error))?;
    for loser in operation.losers() {
        let loser_id = loser.legacy_entity_id();
        transaction
            .execute(
                "INSERT OR IGNORE INTO memory_fact_entities(fact_id, entity_id)
                 SELECT fact_id, ?1 FROM memory_fact_entities WHERE entity_id = ?2",
                params![winner_id, loser_id],
            )
            .await
            .map_err(|error| storage_error(COMPATIBILITY_WRITE_OPERATION, error))?;
        transaction
            .execute(
                "DELETE FROM memory_fact_entities WHERE entity_id = ?1",
                params![loser_id],
            )
            .await
            .map_err(|error| storage_error(COMPATIBILITY_WRITE_OPERATION, error))?;
        transaction
            .execute(
                "DELETE FROM memory_entities WHERE entity_id = ?1
                 AND NOT EXISTS(SELECT 1 FROM memory_fact_entities WHERE entity_id = ?1)",
                params![loser_id],
            )
            .await
            .map_err(|error| storage_error(COMPATIBILITY_WRITE_OPERATION, error))?;
    }
    let owner_key = OwnerKey::new(owner)?;
    for fact_id in &fact_ids {
        let Some(fact) = load_current_fact_tx(transaction, &owner_key, owner, fact_id).await?
        else {
            continue;
        };
        let Some(payload) = fact.payload() else {
            continue;
        };
        let mapping = compatibility_required_mapping_tx(transaction, owner, fact_id).await?;
        let entities =
            compatibility_fact_entities_tx(transaction, mapping.legacy_fact_id()).await?;
        let Some(sanitized) = compatibility_sanitize_payload(
            payload.content(),
            payload.category(),
            payload.tags(),
            &entities,
            payload.metadata(),
        )?
        else {
            return Err(storage_message(
                COMPATIBILITY_WRITE_OPERATION,
                "compatibility merged entities were rejected by the privacy sanitizer",
            ));
        };
        let source = compatibility_source_for_fact_tx(transaction, &mapping).await?;
        let batch = compatibility_curated_correction_batch(
            &fact,
            sanitized.payload.clone(),
            actor.cloned(),
            now,
        )?;
        compatibility_commit_batch_tx(transaction, &batch).await?;
        compatibility_mirror_update_tx(
            db,
            transaction,
            owner,
            mapping.legacy_fact_id(),
            &sanitized.payload,
            &source,
            fact.trust(),
            now,
        )
        .await?;
    }
    Ok(fact_ids)
}

async fn compatibility_add_entity_alias_tx(
    db: &Database,
    transaction: &Transaction,
    owner: &FactOwnerV1,
    operation: &CompatibilityFactAddAliasV1,
    now: UtcMicros,
) -> FactStoreResult<Vec<FactId>> {
    let evidence =
        compatibility_curation_evidence_ids_tx(transaction, owner, operation.evidence_facts())
            .await?;
    let entity_id = operation.entity().legacy_entity_id();
    let (name, mut aliases) = compatibility_owner_entity_tx(transaction, owner, entity_id).await?;
    compatibility_entity_linked_to_evidence_tx(transaction, owner, entity_id, &evidence).await?;
    let alias = normalize_entity(operation.alias());
    if alias.is_empty() || alias.eq_ignore_ascii_case(&name) {
        return Err(storage_message(
            COMPATIBILITY_WRITE_OPERATION,
            "compatibility alias is not distinct from its entity",
        ));
    }
    aliases.push(alias);
    let mut canonical_aliases = std::collections::BTreeMap::new();
    for value in aliases {
        let value = normalize_entity(&value);
        if !value.is_empty() && !value.eq_ignore_ascii_case(&name) {
            canonical_aliases
                .entry(value.to_ascii_lowercase())
                .or_insert(value);
        }
    }
    transaction
        .execute(
            "UPDATE memory_entities SET aliases = ?1, updated_at = ?2 WHERE entity_id = ?3",
            params![
                to_json(
                    &canonical_aliases.into_values().collect::<Vec<_>>(),
                    "serialize compatibility entity aliases",
                )?,
                compatibility_legacy_timestamp(now),
                entity_id,
            ],
        )
        .await
        .map_err(|error| storage_error(COMPATIBILITY_WRITE_OPERATION, error))?;
    let fact_ids = compatibility_owner_entity_fact_ids_tx(transaction, owner, &[entity_id]).await?;
    for fact_id in &fact_ids {
        let mapping = compatibility_required_mapping_tx(transaction, owner, fact_id).await?;
        let mut rows = transaction
            .query(
                "SELECT category FROM memory_facts WHERE fact_id = ?1",
                params![mapping.legacy_fact_id()],
            )
            .await
            .map_err(|error| storage_error(COMPATIBILITY_WRITE_OPERATION, error))?;
        let row = rows
            .next()
            .await
            .map_err(|error| storage_error(COMPATIBILITY_WRITE_OPERATION, error))?
            .ok_or_else(|| {
                storage_message(
                    COMPATIBILITY_WRITE_OPERATION,
                    "compatibility alias fact is missing from the legacy mirror",
                )
            })?;
        let category =
            compatibility_proposal_category(&row_string(&row, 0, COMPATIBILITY_WRITE_OPERATION)?)?;
        compatibility_mark_owner_banks_dirty_tx(db, transaction, owner, category, now).await?;
    }
    Ok(fact_ids)
}

fn compatibility_curation_operation_digest(
    operation: &CompatibilityFactCurationOperationV1,
) -> FactStoreResult<Value> {
    let evidence = |targets: &[CompatibilityFactTargetV1]| {
        targets
            .iter()
            .map(compatibility_target_digest)
            .collect::<FactStoreResult<Vec<_>>>()
    };
    match operation {
        CompatibilityFactCurationOperationV1::NormalizeTags(operation) => Ok(json!({
            "kind": "normalize_tags",
            "fact": compatibility_target_digest(operation.fact())?,
            "tags": operation.tags(),
            "evidence": evidence(operation.evidence_facts())?,
            "confidence": operation.confidence().as_f64(),
        })),
        CompatibilityFactCurationOperationV1::MergeEntities(operation) => Ok(json!({
            "kind": "merge_entities",
            "winner": operation.winner().legacy_entity_id(),
            "losers": operation.losers().iter().map(|target| target.legacy_entity_id()).collect::<Vec<_>>(),
            "evidence": evidence(operation.evidence_facts())?,
            "confidence": operation.confidence().as_f64(),
        })),
        CompatibilityFactCurationOperationV1::AddAlias(operation) => Ok(json!({
            "kind": "add_alias",
            "entity": operation.entity().legacy_entity_id(),
            "alias": operation.alias(),
            "evidence": evidence(operation.evidence_facts())?,
            "confidence": operation.confidence().as_f64(),
        })),
        CompatibilityFactCurationOperationV1::LinkFacts(operation) => Ok(json!({
            "kind": "link_facts",
            "source": compatibility_target_digest(operation.source())?,
            "target": compatibility_target_digest(operation.target())?,
            "relation": compatibility_relation_label(operation.relation()),
            "evidence": evidence(operation.evidence_facts())?,
            "confidence": operation.confidence().as_f64(),
            "source_label": operation.source_label(),
            "metadata": operation.metadata(),
        })),
        CompatibilityFactCurationOperationV1::RepairVector(operation) => Ok(json!({
            "kind": "repair_vector",
            "fact": compatibility_target_digest(operation.fact())?,
            "evidence": evidence(operation.evidence_facts())?,
            "confidence": operation.confidence().as_f64(),
        })),
    }
}

async fn compatibility_record_oplog_tx(
    transaction: &Transaction,
    operation: &str,
    mapping: Option<&CompatibilityFactMappingV1>,
    detail: &Value,
    now: UtcMicros,
) -> FactStoreResult<()> {
    transaction
        .execute(
            "INSERT INTO memory_oplog(ts, op, fact_id, detail_json) VALUES(?1, ?2, ?3, ?4)",
            params![
                compatibility_legacy_timestamp(now),
                operation,
                mapping.and_then(CompatibilityFactMappingV1::legacy_fact_id),
                to_json(detail, "serialize compatibility oplog detail")?,
            ],
        )
        .await
        .map_err(|error| storage_error(COMPATIBILITY_WRITE_OPERATION, error))?;
    Ok(())
}

async fn compatibility_replay_curation_tx(
    transaction: &Transaction,
    owner: &FactOwnerV1,
    receipt: &CompatibilityOperationReceiptV1,
) -> FactCompatibilityResult<CompatibilityFactCurationReceiptV1> {
    let ids = receipt
        .receipt
        .get("changed_fact_ids")
        .and_then(Value::as_array)
        .ok_or_else(|| {
            storage_message(
                COMPATIBILITY_WRITE_OPERATION,
                "compatibility curation receipt changed facts are malformed",
            )
        })?;
    let mut fact_ids = Vec::with_capacity(ids.len());
    for id in ids {
        fact_ids.push(
            FactId::new(id.as_str().ok_or_else(|| {
                storage_message(
                    COMPATIBILITY_WRITE_OPERATION,
                    "compatibility curation receipt fact id is malformed",
                )
            })?)
            .map_err(FactStoreError::from)?,
        );
    }
    let mappings =
        compatibility_curation_mappings_from_ids_tx(transaction, owner, &fact_ids).await?;
    let derived_repair = CompatibilityMemoryRepairStatsV1::new(
        compatibility_receipt_u64(&receipt.receipt, "missing_vectors_repaired")?,
        compatibility_receipt_u64(&receipt.receipt, "banks_rebuilt")?,
    );
    CompatibilityFactCurationReceiptV1::new(
        owner.clone(),
        mappings,
        compatibility_receipt_u64(&receipt.receipt, "normalized_tags")?,
        compatibility_receipt_u64(&receipt.receipt, "merged_entities")?,
        compatibility_receipt_u64(&receipt.receipt, "aliases_added")?,
        compatibility_receipt_u64(&receipt.receipt, "facts_linked")?,
        compatibility_receipt_u64(&receipt.receipt, "vectors_repaired")?,
        derived_repair,
    )
    .map_err(Into::into)
}

async fn apply_compatibility_fact_curation_tx(
    db: &Database,
    transaction: &Transaction,
    request: &CompatibilityFactCurationBatchV1,
) -> FactCompatibilityResult<CompatibilityFactCurationReceiptV1> {
    let request_digest = compatibility_digest(json!({
        "owner": request.owner(),
        "actor": request.actor().map(ActorId::as_str),
        "min_confidence": request.min_confidence().as_f64(),
        "operations": request
            .operations()
            .iter()
            .map(compatibility_curation_operation_digest)
            .collect::<FactStoreResult<Vec<_>>>()?,
    }))?;
    if let Some(receipt) = compatibility_lookup_operation_receipt_tx(
        transaction,
        request.owner(),
        request.operation_id(),
        "curation",
        &request_digest,
    )
    .await?
    {
        return compatibility_replay_curation_tx(transaction, request.owner(), &receipt).await;
    }
    let now = compatibility_now()?;
    let mut changed = Vec::new();
    let mut normalized_tags = 0_u64;
    let mut merged_entities = 0_u64;
    let mut aliases_added = 0_u64;
    let mut facts_linked = 0_u64;
    let mut vectors_repaired = 0_u64;
    for operation in request.operations() {
        match operation {
            CompatibilityFactCurationOperationV1::NormalizeTags(operation) => {
                changed.push(
                    compatibility_normalize_tags_tx(
                        db,
                        transaction,
                        request.owner(),
                        request.actor(),
                        operation,
                        now,
                    )
                    .await?,
                );
                normalized_tags = normalized_tags.saturating_add(1);
            }
            CompatibilityFactCurationOperationV1::MergeEntities(operation) => {
                changed.extend(
                    compatibility_merge_entities_tx(
                        db,
                        transaction,
                        request.owner(),
                        request.actor(),
                        operation,
                        now,
                    )
                    .await?,
                );
                merged_entities = merged_entities.saturating_add(1);
            }
            CompatibilityFactCurationOperationV1::AddAlias(operation) => {
                changed.extend(
                    compatibility_add_entity_alias_tx(
                        db,
                        transaction,
                        request.owner(),
                        operation,
                        now,
                    )
                    .await?,
                );
                aliases_added = aliases_added.saturating_add(1);
            }
            CompatibilityFactCurationOperationV1::LinkFacts(operation) => {
                let (fact_ids, _) = compatibility_link_facts_tx(
                    transaction,
                    request.owner(),
                    request.actor(),
                    operation,
                    now,
                )
                .await?;
                changed.extend(fact_ids);
                facts_linked = facts_linked.saturating_add(1);
            }
            CompatibilityFactCurationOperationV1::RepairVector(operation) => {
                changed.push(
                    compatibility_repair_vector_for_fact_tx(
                        db,
                        transaction,
                        request.owner(),
                        operation,
                        now,
                    )
                    .await?,
                );
                vectors_repaired = vectors_repaired.saturating_add(1);
            }
        }
    }
    let missing_vectors_repaired = compatibility_repair_missing_vectors_tx(
        db,
        transaction,
        request.owner(),
        COMPATIBILITY_REPAIR_VECTOR_BATCH,
    )
    .await?;
    let banks_rebuilt =
        compatibility_rebuild_dirty_banks_tx(db, transaction, request.owner()).await?;
    let mappings =
        compatibility_curation_mappings_from_ids_tx(transaction, request.owner(), &changed).await?;
    if mappings.len() > 256 {
        return Err(storage_message(
            COMPATIBILITY_WRITE_OPERATION,
            "compatibility curation changes exceed the fixed 256-fact receipt bound",
        )
        .into());
    }
    let receipt = json!({
        "changed_fact_ids": mappings.iter().map(|mapping| mapping.fact_id().as_str()).collect::<Vec<_>>(),
        "normalized_tags": normalized_tags,
        "merged_entities": merged_entities,
        "aliases_added": aliases_added,
        "facts_linked": facts_linked,
        "vectors_repaired": vectors_repaired,
        "missing_vectors_repaired": missing_vectors_repaired,
        "banks_rebuilt": banks_rebuilt,
    });
    compatibility_record_operation_receipt_tx(
        transaction,
        request.owner(),
        request.operation_id(),
        "curation",
        &request_digest,
        None,
        None,
        &receipt,
        now,
    )
    .await?;
    if let Some(mapping) = mappings.first() {
        compatibility_record_oplog_tx(
            transaction,
            "curate_apply",
            Some(mapping),
            &json!({
                "normalized_tags": normalized_tags,
                "merged_entities": merged_entities,
                "aliases_added": aliases_added,
                "facts_linked": facts_linked,
                "vectors_repaired": vectors_repaired,
            }),
            now,
        )
        .await?;
    }
    CompatibilityFactCurationReceiptV1::new(
        request.owner().clone(),
        mappings,
        normalized_tags,
        merged_entities,
        aliases_added,
        facts_linked,
        vectors_repaired,
        CompatibilityMemoryRepairStatsV1::new(missing_vectors_repaired, banks_rebuilt),
    )
    .map_err(Into::into)
}

fn compatibility_merge_removal_batch(
    owner: &FactOwnerV1,
    fact_id: &FactId,
    previous: PayloadAccessState,
    expected_last_event_id: Option<FactEventId>,
    winner: &FactId,
    actor: Option<ActorId>,
    now: UtcMicros,
) -> FactStoreResult<FactWriteBatch> {
    let curated = FactLineageEventV1::new(
        fact_id.clone(),
        owner.clone(),
        FactLineageEventKindV1::Curated {
            action: FactCurationActionV1::MergedInto {
                fact_id: winner.clone(),
            },
            evidence_ids: Vec::new(),
        },
        now,
        actor.clone(),
    )?;
    let deleted = FactLineageEventV1::new(
        fact_id.clone(),
        owner.clone(),
        FactLineageEventKindV1::PayloadAccessChanged {
            previous,
            current: PayloadAccessState::Deleted,
        },
        compatibility_event_time(now, 1)?,
        actor,
    )?;
    FactWriteBatch::new(
        fact_id.clone(),
        owner.clone(),
        None,
        vec![curated, deleted],
        Vec::new(),
        Vec::new(),
        None,
        expected_last_event_id,
    )
}

async fn compatibility_mirror_category_tx(
    transaction: &Transaction,
    legacy_fact_id: i64,
) -> FactStoreResult<FactCategoryV1> {
    let mut rows = transaction
        .query(
            "SELECT category FROM memory_facts WHERE fact_id = ?1",
            params![legacy_fact_id],
        )
        .await
        .map_err(|error| storage_error(COMPATIBILITY_WRITE_OPERATION, error))?;
    let row = rows
        .next()
        .await
        .map_err(|error| storage_error(COMPATIBILITY_WRITE_OPERATION, error))?
        .ok_or_else(|| {
            storage_message(
                COMPATIBILITY_WRITE_OPERATION,
                "compatibility legacy mirror fact is missing",
            )
        })?;
    compatibility_proposal_category(&row_string(&row, 0, COMPATIBILITY_WRITE_OPERATION)?)
}

async fn compatibility_replay_merge_tx(
    transaction: &Transaction,
    owner: &FactOwnerV1,
    receipt: &CompatibilityOperationReceiptV1,
) -> FactCompatibilityResult<CompatibilityFactMergeOutcomeV1> {
    let winner_id = receipt.fact_id.as_ref().ok_or_else(|| {
        storage_message(
            COMPATIBILITY_WRITE_OPERATION,
            "compatibility merge receipt winner is missing",
        )
    })?;
    let winner = compatibility_curation_mappings_from_ids_tx(
        transaction,
        owner,
        std::slice::from_ref(winner_id),
    )
    .await?
    .into_iter()
    .next()
    .ok_or_else(|| {
        storage_message(
            COMPATIBILITY_WRITE_OPERATION,
            "compatibility merge receipt winner mapping is missing",
        )
    })?;
    let deleted_ids = receipt
        .receipt
        .get("deleted_loser_fact_ids")
        .and_then(Value::as_array)
        .ok_or_else(|| {
            storage_message(
                COMPATIBILITY_WRITE_OPERATION,
                "compatibility merge receipt deleted losers are malformed",
            )
        })?;
    let mut ids = Vec::with_capacity(deleted_ids.len());
    for id in deleted_ids {
        ids.push(
            FactId::new(id.as_str().ok_or_else(|| {
                storage_message(
                    COMPATIBILITY_WRITE_OPERATION,
                    "compatibility merge receipt loser id is malformed",
                )
            })?)
            .map_err(FactStoreError::from)?,
        );
    }
    let deleted_losers =
        compatibility_curation_mappings_from_ids_tx(transaction, owner, &ids).await?;
    let content_updated = receipt
        .receipt
        .get("content_updated")
        .and_then(Value::as_bool)
        .ok_or_else(|| {
            storage_message(
                COMPATIBILITY_WRITE_OPERATION,
                "compatibility merge receipt content flag is malformed",
            )
        })?;
    CompatibilityFactMergeOutcomeV1::new(owner.clone(), winner, content_updated, deleted_losers)
        .map_err(Into::into)
}

async fn compatibility_rewire_merge_relations_tx(
    transaction: &Transaction,
    owner: &FactOwnerV1,
    winner_fact_id: &FactId,
    winner_legacy_fact_id: i64,
    loser_fact_ids: &[FactId],
    loser_legacy_fact_ids: &[i64],
    now: UtcMicros,
) -> FactStoreResult<()> {
    if loser_fact_ids.is_empty() {
        return Ok(());
    }
    let legacy_placeholders = std::iter::repeat_n("?", loser_legacy_fact_ids.len())
        .collect::<Vec<_>>()
        .join(",");
    let legacy_sql = format!(
        "SELECT source_fact_id, target_fact_id, relation, confidence, source, metadata
         FROM memory_fact_relations
         WHERE source_fact_id IN ({legacy_placeholders})
            OR target_fact_id IN ({legacy_placeholders})
         ORDER BY source_fact_id ASC, target_fact_id ASC, relation ASC
         LIMIT 257"
    );
    let mut legacy_values = Vec::with_capacity(loser_legacy_fact_ids.len() * 2);
    legacy_values.extend(
        loser_legacy_fact_ids
            .iter()
            .copied()
            .map(libsql::Value::Integer),
    );
    legacy_values.extend(
        loser_legacy_fact_ids
            .iter()
            .copied()
            .map(libsql::Value::Integer),
    );
    let mut legacy_rows = transaction
        .query(&legacy_sql, legacy_values)
        .await
        .map_err(|error| storage_error(COMPATIBILITY_WRITE_OPERATION, error))?;
    let mut legacy_relations = Vec::new();
    while let Some(row) = legacy_rows
        .next()
        .await
        .map_err(|error| storage_error(COMPATIBILITY_WRITE_OPERATION, error))?
    {
        legacy_relations.push((
            row_i64(&row, 0, COMPATIBILITY_WRITE_OPERATION)?,
            row_i64(&row, 1, COMPATIBILITY_WRITE_OPERATION)?,
            row_string(&row, 2, COMPATIBILITY_WRITE_OPERATION)?,
            Confidence::new(row_f64(&row, 3, COMPATIBILITY_WRITE_OPERATION)?)?,
            row_string(&row, 4, COMPATIBILITY_WRITE_OPERATION)?,
            from_json::<Value>(
                &row_string(&row, 5, COMPATIBILITY_WRITE_OPERATION)?,
                COMPATIBILITY_WRITE_OPERATION,
            )?,
        ));
    }
    drop(legacy_rows);
    if legacy_relations.len() > 256 {
        return Err(storage_message(
            COMPATIBILITY_WRITE_OPERATION,
            "compatibility merge relation rewiring exceeds the fixed 256-relation bound",
        ));
    }
    let loser_legacy = loser_legacy_fact_ids
        .iter()
        .copied()
        .collect::<BTreeSet<_>>();
    for (source, target, _, _, _, _) in &legacy_relations {
        for endpoint in [source, target] {
            if compatibility_fact_for_legacy_id_tx(transaction, owner, *endpoint)
                .await?
                .is_none()
            {
                return Err(storage_message(
                    COMPATIBILITY_WRITE_OPERATION,
                    "compatibility merge relation crosses an owner boundary",
                ));
            }
        }
    }
    transaction
        .execute(
            &format!(
                "DELETE FROM memory_fact_relations
                 WHERE source_fact_id IN ({legacy_placeholders})
                    OR target_fact_id IN ({legacy_placeholders})"
            ),
            {
                let mut values = Vec::with_capacity(loser_legacy_fact_ids.len() * 2);
                values.extend(
                    loser_legacy_fact_ids
                        .iter()
                        .copied()
                        .map(libsql::Value::Integer),
                );
                values.extend(
                    loser_legacy_fact_ids
                        .iter()
                        .copied()
                        .map(libsql::Value::Integer),
                );
                values
            },
        )
        .await
        .map_err(|error| storage_error(COMPATIBILITY_WRITE_OPERATION, error))?;
    for (source, target, relation, confidence, source_label, metadata) in legacy_relations {
        let source = if loser_legacy.contains(&source) {
            winner_legacy_fact_id
        } else {
            source
        };
        let target = if loser_legacy.contains(&target) {
            winner_legacy_fact_id
        } else {
            target
        };
        if source == target {
            continue;
        }
        let relation = match relation.as_str() {
            "supports" => CompatibilityFactRelationV1::Supports,
            "contradicts" => CompatibilityFactRelationV1::Contradicts,
            "supersedes" => CompatibilityFactRelationV1::Supersedes,
            "derived_from" => CompatibilityFactRelationV1::DerivedFrom,
            _ => {
                return Err(storage_message(
                    COMPATIBILITY_WRITE_OPERATION,
                    "compatibility merge found an unsupported legacy relation",
                ));
            }
        };
        compatibility_upsert_legacy_relation_tx(
            transaction,
            source,
            target,
            relation,
            confidence,
            &compatibility_source_label(Some(&source_label))?,
            &compatibility_sanitized_relation_metadata(&metadata).await?,
            compatibility_legacy_timestamp(now),
        )
        .await?;
    }

    let key = OwnerKey::new(owner)?;
    let canonical_placeholders = std::iter::repeat_n("?", loser_fact_ids.len())
        .collect::<Vec<_>>()
        .join(",");
    let canonical_sql = format!(
        "SELECT source_fact_id, target_fact_id, relation, confidence, source_label,
                provenance_json, evidence_fact_ids_json, occurred_at
         FROM memory_v2_fact_relations
         WHERE owner_kind = ? AND project_id = ?
           AND (source_fact_id IN ({canonical_placeholders})
                OR target_fact_id IN ({canonical_placeholders}))
         ORDER BY source_fact_id ASC, target_fact_id ASC, relation ASC
         LIMIT 257"
    );
    let mut canonical_values = Vec::with_capacity(loser_fact_ids.len() * 2 + 2);
    canonical_values.push(libsql::Value::Text(key.kind.to_string()));
    canonical_values.push(libsql::Value::Text(key.project_id.clone()));
    for _ in 0..2 {
        canonical_values.extend(
            loser_fact_ids
                .iter()
                .map(|fact_id| libsql::Value::Text(fact_id.as_str().to_owned())),
        );
    }
    let mut canonical_rows = transaction
        .query(&canonical_sql, canonical_values)
        .await
        .map_err(|error| storage_error(COMPATIBILITY_WRITE_OPERATION, error))?;
    let mut canonical_relations = Vec::new();
    while let Some(row) = canonical_rows
        .next()
        .await
        .map_err(|error| storage_error(COMPATIBILITY_WRITE_OPERATION, error))?
    {
        canonical_relations.push((
            FactId::new(row_string(&row, 0, COMPATIBILITY_WRITE_OPERATION)?)?,
            FactId::new(row_string(&row, 1, COMPATIBILITY_WRITE_OPERATION)?)?,
            row_string(&row, 2, COMPATIBILITY_WRITE_OPERATION)?,
            Confidence::new(row_f64(&row, 3, COMPATIBILITY_WRITE_OPERATION)?)?,
            row_string(&row, 4, COMPATIBILITY_WRITE_OPERATION)?,
            row_string(&row, 5, COMPATIBILITY_WRITE_OPERATION)?,
            row_string(&row, 6, COMPATIBILITY_WRITE_OPERATION)?,
            row_i64(&row, 7, COMPATIBILITY_WRITE_OPERATION)?,
        ));
    }
    drop(canonical_rows);
    if canonical_relations.len() > 256 {
        return Err(storage_message(
            COMPATIBILITY_WRITE_OPERATION,
            "canonical merge relation rewiring exceeds the fixed 256-relation bound",
        ));
    }
    let loser_canonical = loser_fact_ids.iter().cloned().collect::<BTreeSet<_>>();
    transaction
        .execute(
            &format!(
                "DELETE FROM memory_v2_fact_relations
                 WHERE owner_kind = ? AND project_id = ?
                   AND (source_fact_id IN ({canonical_placeholders})
                        OR target_fact_id IN ({canonical_placeholders}))"
            ),
            {
                let mut values = Vec::with_capacity(loser_fact_ids.len() * 2 + 2);
                values.push(libsql::Value::Text(key.kind.to_string()));
                values.push(libsql::Value::Text(key.project_id.clone()));
                for _ in 0..2 {
                    values.extend(
                        loser_fact_ids
                            .iter()
                            .map(|fact_id| libsql::Value::Text(fact_id.as_str().to_owned())),
                    );
                }
                values
            },
        )
        .await
        .map_err(|error| storage_error(COMPATIBILITY_WRITE_OPERATION, error))?;
    for (
        source,
        target,
        relation,
        confidence,
        source_label,
        provenance_json,
        evidence_json,
        occurred_at,
    ) in canonical_relations
    {
        let source = if loser_canonical.contains(&source) {
            winner_fact_id
        } else {
            &source
        };
        let target = if loser_canonical.contains(&target) {
            winner_fact_id
        } else {
            &target
        };
        if source == target {
            continue;
        }
        transaction
            .execute(
                "INSERT INTO memory_v2_fact_relations(
                    owner_kind, project_id, source_fact_id, target_fact_id, relation,
                    confidence, source_label, provenance_json, evidence_fact_ids_json,
                    occurred_at, updated_at
                 ) VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
                 ON CONFLICT(owner_kind, project_id, source_fact_id, target_fact_id, relation)
                 DO UPDATE SET confidence = excluded.confidence,
                               source_label = excluded.source_label,
                               provenance_json = excluded.provenance_json,
                               evidence_fact_ids_json = excluded.evidence_fact_ids_json,
                               updated_at = excluded.updated_at",
                params![
                    key.kind,
                    key.project_id.as_str(),
                    source.as_str(),
                    target.as_str(),
                    relation,
                    confidence.as_f64(),
                    compatibility_source_label(Some(&source_label))?,
                    provenance_json,
                    evidence_json,
                    occurred_at,
                    now.0,
                ],
            )
            .await
            .map_err(|error| storage_error(COMPATIBILITY_WRITE_OPERATION, error))?;
    }
    Ok(())
}

async fn merge_compatibility_facts_tx(
    db: &Database,
    transaction: &Transaction,
    request: &CompatibilityFactMergeCommandV1,
) -> FactCompatibilityResult<CompatibilityFactMergeOutcomeV1> {
    let request_digest = compatibility_digest(json!({
        "owner": request.owner(),
        "winner": compatibility_target_digest(request.winner())?,
        "losers": request
            .losers()
            .iter()
            .map(compatibility_target_digest)
            .collect::<FactStoreResult<Vec<_>>>()?,
        "merged_content": request.merged_content(),
        "actor": request.actor().map(ActorId::as_str),
    }))?;
    if let Some(receipt) = compatibility_lookup_operation_receipt_tx(
        transaction,
        request.owner(),
        request.operation_id(),
        "merge",
        &request_digest,
    )
    .await?
    {
        return compatibility_replay_merge_tx(transaction, request.owner(), &receipt).await;
    }
    let now = compatibility_now()?;
    let (winner_id, winner_fact, winner_mapping) =
        compatibility_available_curation_fact_tx(transaction, request.winner()).await?;
    let mut content_updated = false;
    if let Some(content) = request.merged_content() {
        let payload = winner_fact
            .payload()
            .ok_or(FactStoreError::PayloadAccessMismatch)?;
        let Some(sanitized) = compatibility_sanitize_payload(
            content,
            payload.category(),
            payload.tags(),
            payload.entities(),
            payload.metadata(),
        )?
        else {
            return Err(storage_message(
                COMPATIBILITY_WRITE_OPERATION,
                "compatibility merged content was rejected by the privacy sanitizer",
            )
            .into());
        };
        let source = compatibility_source_for_fact_tx(
            transaction,
            winner_mapping
                .legacy_mapping()
                .ok_or(FactStoreError::FactMismatch)?,
        )
        .await?;
        let batch = compatibility_curated_correction_batch(
            &winner_fact,
            sanitized.payload.clone(),
            request.actor().cloned(),
            now,
        )?;
        compatibility_commit_batch_tx(transaction, &batch).await?;
        compatibility_mirror_update_tx(
            db,
            transaction,
            request.owner(),
            winner_mapping
                .legacy_fact_id()
                .ok_or(FactStoreError::FactMismatch)?,
            &sanitized.payload,
            &source,
            winner_fact.trust(),
            now,
        )
        .await?;
        content_updated = true;
    }
    let owner_key = OwnerKey::new(request.owner())?;
    let mut loser_ids = Vec::with_capacity(request.losers().len());
    let mut loser_legacy_ids = Vec::with_capacity(request.losers().len());
    let mut pending_deletes = Vec::with_capacity(request.losers().len());
    for target in request.losers() {
        let loser_id = resolve_compatibility_target_tx(transaction, target)
            .await?
            .ok_or_else(|| {
                let loser_label = target
                    .legacy_query()
                    .map(|query| query.legacy_fact_id().to_string())
                    .or_else(|| {
                        target
                            .canonical_fact_id()
                            .map(|fact_id| fact_id.as_str().to_string())
                    })
                    .unwrap_or_else(|| "unknown".to_string());
                storage_message(
                    COMPATIBILITY_WRITE_OPERATION,
                    format!("compatibility merge loser fact {loser_label} not found"),
                )
            })?;
        if loser_id == winner_id {
            return Err(storage_message(
                COMPATIBILITY_WRITE_OPERATION,
                "compatibility merge winner cannot be a loser",
            )
            .into());
        }
        let projection = load_current_projection(transaction, &owner_key, &loser_id)
            .await?
            .ok_or_else(|| {
                storage_message(
                    COMPATIBILITY_WRITE_OPERATION,
                    "compatibility merge loser projection is missing",
                )
            })?;
        let mapping =
            compatibility_required_mapping_tx(transaction, request.owner(), &loser_id).await?;
        loser_ids.push(loser_id.clone());
        loser_legacy_ids.push(mapping.legacy_fact_id());
        if projection.access != PayloadAccessState::Deleted {
            let category =
                compatibility_mirror_category_tx(transaction, mapping.legacy_fact_id()).await?;
            pending_deletes.push((
                loser_id,
                projection.access,
                projection.last_event_id.clone(),
                mapping,
                category,
            ));
        }
    }
    compatibility_rewire_merge_relations_tx(
        transaction,
        request.owner(),
        &winner_id,
        winner_mapping
            .legacy_fact_id()
            .ok_or(FactStoreError::FactMismatch)?,
        &loser_ids,
        &loser_legacy_ids,
        now,
    )
    .await?;
    let mut deleted_ids = Vec::new();
    for (loser_id, previous_access, expected_last_event_id, mapping, category) in pending_deletes {
        let batch = compatibility_merge_removal_batch(
            request.owner(),
            &loser_id,
            previous_access,
            expected_last_event_id,
            &winner_id,
            request.actor().cloned(),
            now,
        )?;
        compatibility_commit_batch_tx(transaction, &batch).await?;
        compatibility_mirror_delete_tx(
            db,
            transaction,
            request.owner(),
            mapping.legacy_fact_id(),
            category,
            now,
        )
        .await?;
        deleted_ids.push(loser_id);
    }
    let winner = compatibility_curation_mappings_from_ids_tx(
        transaction,
        request.owner(),
        std::slice::from_ref(&winner_id),
    )
    .await?
    .into_iter()
    .next()
    .ok_or_else(|| {
        storage_message(
            COMPATIBILITY_WRITE_OPERATION,
            "compatibility merge winner mapping is missing",
        )
    })?;
    let deleted_losers =
        compatibility_curation_mappings_from_ids_tx(transaction, request.owner(), &deleted_ids)
            .await?;
    let receipt = json!({
        "content_updated": content_updated,
        "deleted_loser_fact_ids": deleted_ids.iter().map(FactId::as_str).collect::<Vec<_>>(),
    });
    compatibility_record_operation_receipt_tx(
        transaction,
        request.owner(),
        request.operation_id(),
        "merge",
        &request_digest,
        Some(&winner_id),
        None,
        &receipt,
        now,
    )
    .await?;
    compatibility_record_oplog_tx(
        transaction,
        "curate_apply",
        Some(&winner),
        &json!({
            "merged_fact_count": deleted_losers.len(),
            "content_updated": content_updated,
        }),
        now,
    )
    .await?;
    CompatibilityFactMergeOutcomeV1::new(
        request.owner().clone(),
        winner,
        content_updated,
        deleted_losers,
    )
    .map_err(Into::into)
}

fn compatibility_repair_request_digest(
    request: &CompatibilityMemoryRepairCommandV1,
) -> FactStoreResult<String> {
    compatibility_digest(json!({
        "owner": request.owner(),
        "actor": request.actor().map(ActorId::as_str),
    }))
}

async fn repair_compatibility_memory_tx(
    db: &Database,
    transaction: &Transaction,
    request: &CompatibilityMemoryRepairCommandV1,
) -> FactCompatibilityResult<CompatibilityMemoryRepairStatsV1> {
    let request_digest = compatibility_repair_request_digest(request)?;
    if let Some(receipt) = compatibility_lookup_operation_receipt_tx(
        transaction,
        request.owner(),
        request.operation_id(),
        "repair",
        &request_digest,
    )
    .await?
    {
        return Ok(CompatibilityMemoryRepairStatsV1::new(
            compatibility_receipt_u64(&receipt.receipt, "missing_vectors_repaired")?,
            compatibility_receipt_u64(&receipt.receipt, "banks_rebuilt")?,
        )
        .with_feedback_history_repair(compatibility_receipt_feedback_history_repair(
            &receipt.receipt,
        )?));
    }
    let feedback_repair =
        advance_compatibility_feedback_history_repair_tx(db, transaction, request.owner()).await?;
    let now = compatibility_now()?;
    let missing_vectors_repaired = compatibility_repair_missing_vectors_tx(
        db,
        transaction,
        request.owner(),
        COMPATIBILITY_REPAIR_VECTOR_BATCH,
    )
    .await?;
    compatibility_mark_absent_banks_dirty_tx(db, transaction, request.owner(), now).await?;
    let banks_rebuilt =
        compatibility_rebuild_dirty_banks_tx(db, transaction, request.owner()).await?;
    let receipt = json!({
        "missing_vectors_repaired": missing_vectors_repaired,
        "banks_rebuilt": banks_rebuilt,
        "feedback_history_repair": compatibility_feedback_history_repair_receipt(feedback_repair),
    });
    compatibility_record_operation_receipt_tx(
        transaction,
        request.owner(),
        request.operation_id(),
        "repair",
        &request_digest,
        None,
        None,
        &receipt,
        now,
    )
    .await?;
    Ok(
        CompatibilityMemoryRepairStatsV1::new(missing_vectors_repaired, banks_rebuilt)
            .with_feedback_history_repair(feedback_repair),
    )
}

async fn compatibility_repair_missing_vectors_tx(
    db: &Database,
    transaction: &Transaction,
    owner: &FactOwnerV1,
    limit: i64,
) -> FactStoreResult<u64> {
    let key = OwnerKey::new(owner)?;
    let source_store_id = compatibility_source_store_id()?;
    let mut rows = transaction
        .query(
            "SELECT mappings.fact_id
             FROM memory_v2_legacy_map AS mappings
             JOIN memory_facts AS legacy_facts
               ON legacy_facts.fact_id = mappings.legacy_fact_id
             JOIN memory_v2_current_facts AS current_facts
               ON current_facts.fact_id = mappings.fact_id
              AND current_facts.owner_kind = mappings.owner_kind
              AND current_facts.project_id = mappings.project_id
             JOIN memory_v2_assertion_payloads AS payloads
               ON payloads.assertion_id = current_facts.active_assertion_id
              AND payloads.fact_id = current_facts.fact_id
              AND payloads.owner_kind = current_facts.owner_kind
              AND payloads.project_id = current_facts.project_id
             WHERE mappings.owner_kind = ?1
               AND mappings.project_id = ?2
               AND mappings.owner_json = ?3
               AND mappings.source_store_id = ?4
               AND current_facts.payload_access = 'eligible'
               AND (
                    legacy_facts.hrr_vector IS NULL
                    OR legacy_facts.hrr_algebra <> 'amari_fhrr'
                    OR legacy_facts.hrr_dim <> ?5
                    OR legacy_facts.hrr_precision <> ?6
                    OR length(legacy_facts.hrr_vector) <> ?7
               )
             ORDER BY legacy_facts.updated_at DESC, mappings.fact_id ASC
             LIMIT ?8",
            params![
                key.kind,
                key.project_id.as_str(),
                key.json.as_str(),
                source_store_id.as_str(),
                HolographicEncoder::DIMENSIONS as i64,
                HolographicEncoder::HRR_PRECISION,
                HolographicEncoder::SERIALIZED_F32_BYTES as i64,
                limit,
            ],
        )
        .await
        .map_err(|error| storage_error(COMPATIBILITY_WRITE_OPERATION, error))?;
    let mut fact_ids = Vec::new();
    while let Some(row) = rows
        .next()
        .await
        .map_err(|error| storage_error(COMPATIBILITY_WRITE_OPERATION, error))?
    {
        fact_ids.push(
            FactId::new(row_string(&row, 0, COMPATIBILITY_WRITE_OPERATION)?)
                .map_err(FactStoreError::from)?,
        );
    }
    drop(rows);
    let now = compatibility_now()?;
    let mut repaired = 0_u64;
    for fact_id in fact_ids {
        let Some(fact) = load_current_fact_tx(transaction, &key, owner, &fact_id).await? else {
            continue;
        };
        let Some(payload) = fact.payload() else {
            continue;
        };
        let mapping = compatibility_required_mapping_tx(transaction, owner, &fact_id).await?;
        let vector = compatibility_mirror_vector(payload)?;
        let changed = transaction
            .execute(
                "UPDATE memory_facts
                 SET hrr_vector = ?1,
                     hrr_algebra = 'amari_fhrr',
                     hrr_dim = ?2,
                     hrr_precision = ?3
                 WHERE fact_id = ?4",
                params![
                    vector,
                    HolographicEncoder::DIMENSIONS as i64,
                    HolographicEncoder::HRR_PRECISION,
                    mapping.legacy_fact_id(),
                ],
            )
            .await
            .map_err(|error| storage_error(COMPATIBILITY_WRITE_OPERATION, error))?;
        if changed != 1 {
            return Err(storage_message(
                COMPATIBILITY_WRITE_OPERATION,
                "compatibility vector target is missing from the legacy mirror",
            ));
        }
        compatibility_mark_owner_banks_dirty_tx(db, transaction, owner, payload.category(), now)
            .await?;
        repaired = repaired.saturating_add(1);
    }
    Ok(repaired)
}

fn compatibility_average_vectors(vectors: &[Vec<f64>]) -> Vec<f64> {
    let mut average = vec![0.0; HolographicEncoder::DIMENSIONS];
    let mut count = 0_u64;
    for vector in vectors {
        if vector.len() != HolographicEncoder::DIMENSIONS {
            continue;
        }
        count = count.saturating_add(1);
        for (target, value) in average.iter_mut().zip(vector) {
            *target += value;
        }
    }
    if count != 0 {
        for value in &mut average {
            *value /= count as f64;
        }
    }
    average
}

/// Marks every populated bank dirty when the owner has eligible facts but no
/// materialized bank projections at all — the state a store lands in when its
/// legacy cutover predates dirty-marking (or a bank table was lost). Repair
/// then rebuilds them in the same pass; stores with any banks are untouched.
async fn compatibility_mark_absent_banks_dirty_tx(
    db: &Database,
    transaction: &Transaction,
    owner: &FactOwnerV1,
    now: UtcMicros,
) -> FactStoreResult<()> {
    let key = OwnerKey::new(owner)?;
    let source_store_id = compatibility_source_store_id()?;
    let mut rows = transaction
        .query(
            "SELECT COUNT(*) FROM memory_v2_compatibility_banks
             WHERE owner_kind = ?1 AND project_id = ?2
               AND owner_json = ?3 AND source_store_id = ?4",
            params![
                key.kind,
                key.project_id.as_str(),
                key.json.as_str(),
                source_store_id.as_str(),
            ],
        )
        .await
        .map_err(|error| storage_error(COMPATIBILITY_WRITE_OPERATION, error))?;
    let bank_count = rows
        .next()
        .await
        .map_err(|error| storage_error(COMPATIBILITY_WRITE_OPERATION, error))?
        .map(|row| row_i64(&row, 0, COMPATIBILITY_WRITE_OPERATION))
        .transpose()?
        .unwrap_or(0);
    drop(rows);
    if bank_count > 0 {
        return Ok(());
    }
    let mut rows = transaction
        .query(
            "SELECT DISTINCT json_extract(payloads.payload_json, '$.category')
             FROM memory_v2_current_facts AS current_facts
             JOIN memory_v2_assertion_payloads AS payloads
               ON payloads.assertion_id = current_facts.active_assertion_id
              AND payloads.fact_id = current_facts.fact_id
              AND payloads.owner_kind = current_facts.owner_kind
              AND payloads.project_id = current_facts.project_id
             WHERE current_facts.owner_kind = ?1
               AND current_facts.project_id = ?2
               AND current_facts.payload_access = 'eligible'
               AND json_extract(payloads.payload_json, '$.category') IS NOT NULL",
            params![key.kind, key.project_id.as_str()],
        )
        .await
        .map_err(|error| storage_error(COMPATIBILITY_WRITE_OPERATION, error))?;
    let mut bank_names = Vec::new();
    while let Some(row) = rows
        .next()
        .await
        .map_err(|error| storage_error(COMPATIBILITY_WRITE_OPERATION, error))?
    {
        bank_names.push(row_string(&row, 0, COMPATIBILITY_WRITE_OPERATION)?);
    }
    drop(rows);
    if bank_names.is_empty() {
        return Ok(());
    }
    bank_names.push("all".to_owned());
    for bank_name in bank_names {
        db.mark_memory_v2_compatibility_bank_dirty_in_transaction(
            transaction,
            owner,
            &source_store_id,
            &bank_name,
            now,
        )
        .await
        .map_err(|error| storage_error(COMPATIBILITY_WRITE_OPERATION, error))?;
    }
    Ok(())
}

async fn compatibility_rebuild_dirty_banks_tx(
    db: &Database,
    transaction: &Transaction,
    owner: &FactOwnerV1,
) -> FactStoreResult<u64> {
    let key = OwnerKey::new(owner)?;
    let source_store_id = compatibility_source_store_id()?;
    let mut rows = transaction
        .query(
            "SELECT bank_name, updated_at
             FROM memory_v2_compatibility_bank_dirty
             WHERE owner_kind = ?1 AND project_id = ?2
               AND owner_json = ?3 AND source_store_id = ?4
             ORDER BY bank_name ASC
             LIMIT 32",
            params![
                key.kind,
                key.project_id.as_str(),
                key.json.as_str(),
                source_store_id.as_str(),
            ],
        )
        .await
        .map_err(|error| storage_error(COMPATIBILITY_WRITE_OPERATION, error))?;
    let mut dirty = Vec::new();
    while let Some(row) = rows
        .next()
        .await
        .map_err(|error| storage_error(COMPATIBILITY_WRITE_OPERATION, error))?
    {
        dirty.push((
            row_string(&row, 0, COMPATIBILITY_WRITE_OPERATION)?,
            UtcMicros(row_i64(&row, 1, COMPATIBILITY_WRITE_OPERATION)?),
        ));
    }
    drop(rows);
    let now = compatibility_now()?;
    let mut rebuilt = 0_u64;
    for (bank_name, dirty_updated_at) in dirty {
        if bank_name != "all"
            && !matches!(
                bank_name.as_str(),
                "general" | "user_pref" | "project" | "tool" | "decision" | "code_area"
            )
        {
            return Err(storage_message(
                COMPATIBILITY_WRITE_OPERATION,
                "compatibility dirty bank has an unsupported category",
            ));
        }
        let mut vectors = transaction
            .query(
                "SELECT legacy_facts.fact_id, mappings.fact_id, legacy_facts.hrr_vector
                 FROM memory_v2_legacy_map AS mappings
                 JOIN memory_facts AS legacy_facts
                   ON legacy_facts.fact_id = mappings.legacy_fact_id
                 JOIN memory_v2_current_facts AS current_facts
                   ON current_facts.fact_id = mappings.fact_id
                  AND current_facts.owner_kind = mappings.owner_kind
                  AND current_facts.project_id = mappings.project_id
                 JOIN memory_v2_assertion_payloads AS payloads
                   ON payloads.assertion_id = current_facts.active_assertion_id
                  AND payloads.fact_id = current_facts.fact_id
                  AND payloads.owner_kind = current_facts.owner_kind
                  AND payloads.project_id = current_facts.project_id
                 WHERE mappings.owner_kind = ?1 AND mappings.project_id = ?2
                   AND mappings.owner_json = ?3 AND mappings.source_store_id = ?4
                   AND current_facts.payload_access = 'eligible'
                   AND legacy_facts.hrr_vector IS NOT NULL
                   AND legacy_facts.hrr_algebra = 'amari_fhrr'
                   AND legacy_facts.hrr_dim = ?6
                   AND legacy_facts.hrr_precision = ?7
                   AND length(legacy_facts.hrr_vector) = ?8
                   AND (?5 = 'all' OR legacy_facts.category = ?5)
                 ORDER BY legacy_facts.fact_id ASC",
                params![
                    key.kind,
                    key.project_id.as_str(),
                    key.json.as_str(),
                    source_store_id.as_str(),
                    bank_name.as_str(),
                    HolographicEncoder::DIMENSIONS as i64,
                    HolographicEncoder::HRR_PRECISION,
                    HolographicEncoder::SERIALIZED_F32_BYTES as i64,
                ],
            )
            .await
            .map_err(|error| storage_error(COMPATIBILITY_WRITE_OPERATION, error))?;
        let mut decoded = Vec::new();
        let mut malformed_legacy_fact_ids = Vec::new();
        while let Some(row) = vectors
            .next()
            .await
            .map_err(|error| storage_error(COMPATIBILITY_WRITE_OPERATION, error))?
        {
            let legacy_fact_id = row_i64(&row, 0, COMPATIBILITY_WRITE_OPERATION)?;
            let fact_id = FactId::new(row_string(&row, 1, COMPATIBILITY_WRITE_OPERATION)?)
                .map_err(FactStoreError::from)?;
            let vector = match row.get_value(2) {
                Ok(libsql::Value::Blob(bytes)) => HolographicEncoder::deserialize(&bytes)
                    .ok()
                    .filter(|vector| {
                        vector.len() == HolographicEncoder::DIMENSIONS
                            && vector.iter().all(|value| value.is_finite())
                    }),
                Ok(_) | Err(_) => None,
            };
            match vector {
                Some(vector) => decoded.push(vector),
                None => malformed_legacy_fact_ids.push((legacy_fact_id, fact_id)),
            }
        }
        drop(vectors);
        for (legacy_fact_id, fact_id) in malformed_legacy_fact_ids {
            let replacement = match load_current_fact_tx(transaction, &key, owner, &fact_id).await?
            {
                Some(fact) => fact.payload().and_then(|payload| {
                    compatibility_mirror_vector(payload).ok().and_then(|bytes| {
                        HolographicEncoder::deserialize(&bytes)
                            .ok()
                            .filter(|vector| {
                                vector.len() == HolographicEncoder::DIMENSIONS
                                    && vector.iter().all(|value| value.is_finite())
                            })
                            .map(|vector| (bytes, vector))
                    })
                }),
                None => None,
            };
            match replacement {
                Some((vector, decoded_vector)) => {
                    transaction
                        .execute(
                            "UPDATE memory_facts
                             SET hrr_vector = ?1,
                                 hrr_algebra = 'amari_fhrr',
                                 hrr_dim = ?2,
                                 hrr_precision = ?3
                             WHERE fact_id = ?4",
                            params![
                                vector,
                                HolographicEncoder::DIMENSIONS as i64,
                                HolographicEncoder::HRR_PRECISION,
                                legacy_fact_id,
                            ],
                        )
                        .await
                        .map_err(|error| storage_error(COMPATIBILITY_WRITE_OPERATION, error))?;
                    decoded.push(decoded_vector);
                }
                None => {
                    transaction
                        .execute(
                            "UPDATE memory_facts
                             SET hrr_vector = NULL,
                                 hrr_algebra = 'amari_fhrr',
                                 hrr_dim = ?1,
                                 hrr_precision = ?2
                             WHERE fact_id = ?3",
                            params![
                                HolographicEncoder::DIMENSIONS as i64,
                                HolographicEncoder::HRR_PRECISION,
                                legacy_fact_id,
                            ],
                        )
                        .await
                        .map_err(|error| storage_error(COMPATIBILITY_WRITE_OPERATION, error))?;
                }
            }
        }
        if decoded.is_empty() {
            db.delete_memory_v2_compatibility_bank_in_transaction(
                transaction,
                owner,
                &source_store_id,
                bank_name.as_str(),
            )
            .await
            .map_err(|error| storage_error(COMPATIBILITY_WRITE_OPERATION, error))?;
        } else {
            let vector = HolographicEncoder::serialize(&compatibility_average_vectors(&decoded))
                .map_err(|error| storage_error(COMPATIBILITY_WRITE_OPERATION, error))?;
            db.upsert_memory_v2_compatibility_bank_in_transaction(
                transaction,
                owner,
                &source_store_id,
                bank_name.as_str(),
                &vector,
                decoded.len() as u64,
                now,
            )
            .await
            .map_err(|error| storage_error(COMPATIBILITY_WRITE_OPERATION, error))?;
        }
        if db
            .clear_memory_v2_compatibility_bank_dirty_in_transaction(
                transaction,
                owner,
                &source_store_id,
                bank_name.as_str(),
                dirty_updated_at,
            )
            .await
            .map_err(|error| storage_error(COMPATIBILITY_WRITE_OPERATION, error))?
        {
            rebuilt = rebuilt.saturating_add(1);
        }
    }
    Ok(rebuilt)
}

async fn compatibility_owner_status_counts_tx(
    transaction: &Transaction,
    owner: &FactOwnerV1,
) -> FactStoreResult<(u64, u64, u64, [u64; 4], u64, u64, u64, u64, u64, u64)> {
    let key = OwnerKey::new(owner)?;
    let mut rows = transaction
        .query(
            "SELECT
                COUNT(*),
                COALESCE(SUM(CASE WHEN current_facts.trust_score < 0.25 THEN 1 ELSE 0 END), 0),
                COALESCE(SUM(CASE WHEN current_facts.trust_score >= 0.25 AND current_facts.trust_score < 0.50 THEN 1 ELSE 0 END), 0),
                COALESCE(SUM(CASE WHEN current_facts.trust_score >= 0.50 AND current_facts.trust_score < 0.75 THEN 1 ELSE 0 END), 0),
                COALESCE(SUM(CASE WHEN current_facts.trust_score >= 0.75 THEN 1 ELSE 0 END), 0),
                COALESCE(SUM(CASE WHEN current_facts.trust_score < ?4 THEN 1 ELSE 0 END), 0),
                COALESCE(SUM(current_facts.helpful_count), 0),
                COALESCE(SUM(current_facts.unhelpful_count), 0),
                COALESCE(SUM(current_facts.retrieval_count), 0),
                COALESCE(SUM(current_facts.access_count), 0),
                COALESCE(SUM(CASE WHEN current_facts.retrieval_count > 0 THEN 1 ELSE 0 END), 0),
                COALESCE(SUM(CASE WHEN current_facts.helpful_count + current_facts.unhelpful_count > 0 THEN 1 ELSE 0 END), 0)
             FROM memory_v2_current_facts AS current_facts
             JOIN memory_v2_facts AS facts
               ON facts.fact_id = current_facts.fact_id
              AND facts.owner_kind = current_facts.owner_kind
              AND facts.project_id = current_facts.project_id
             WHERE current_facts.owner_kind = ?1
               AND current_facts.project_id = ?2
               AND facts.owner_json = ?3
               AND current_facts.active_assertion_id IS NOT NULL",
            params![
                key.kind,
                key.project_id.as_str(),
                key.json.as_str(),
                crate::memory::trust::DEFAULT_MIN_TRUST
            ],
        )
        .await
        .map_err(|error| storage_error(COMPATIBILITY_WRITE_OPERATION, error))?;
    let row = rows
        .next()
        .await
        .map_err(|error| storage_error(COMPATIBILITY_WRITE_OPERATION, error))?
        .ok_or_else(|| {
            storage_message(
                COMPATIBILITY_WRITE_OPERATION,
                "compatibility status is missing",
            )
        })?;
    let fact_count = nonnegative_u64(
        row_i64(&row, 0, COMPATIBILITY_WRITE_OPERATION)?,
        "fact count",
    )?;
    let trust = [
        nonnegative_u64(
            row_i64(&row, 1, COMPATIBILITY_WRITE_OPERATION)?,
            "trust count",
        )?,
        nonnegative_u64(
            row_i64(&row, 2, COMPATIBILITY_WRITE_OPERATION)?,
            "trust count",
        )?,
        nonnegative_u64(
            row_i64(&row, 3, COMPATIBILITY_WRITE_OPERATION)?,
            "trust count",
        )?,
        nonnegative_u64(
            row_i64(&row, 4, COMPATIBILITY_WRITE_OPERATION)?,
            "trust count",
        )?,
    ];
    let below_default = nonnegative_u64(
        row_i64(&row, 5, COMPATIBILITY_WRITE_OPERATION)?,
        "trust count",
    )?;
    let helpful = nonnegative_u64(
        row_i64(&row, 6, COMPATIBILITY_WRITE_OPERATION)?,
        "helpful count",
    )?;
    let unhelpful = nonnegative_u64(
        row_i64(&row, 7, COMPATIBILITY_WRITE_OPERATION)?,
        "unhelpful count",
    )?;
    let retrieval_total = nonnegative_u64(
        row_i64(&row, 8, COMPATIBILITY_WRITE_OPERATION)?,
        "retrieval total",
    )?;
    let access_total = nonnegative_u64(
        row_i64(&row, 9, COMPATIBILITY_WRITE_OPERATION)?,
        "access total",
    )?;
    let retrieved_fact_count = nonnegative_u64(
        row_i64(&row, 10, COMPATIBILITY_WRITE_OPERATION)?,
        "retrieved fact count",
    )?;
    let rated_fact_count = nonnegative_u64(
        row_i64(&row, 11, COMPATIBILITY_WRITE_OPERATION)?,
        "rated fact count",
    )?;
    Ok((
        fact_count,
        helpful,
        unhelpful,
        trust,
        below_default,
        retrieval_total,
        access_total,
        retrieved_fact_count,
        rated_fact_count,
        helpful.saturating_add(unhelpful),
    ))
}

async fn compatibility_owner_has_dirty_banks_tx(
    transaction: &Transaction,
    owner: &FactOwnerV1,
) -> FactStoreResult<bool> {
    let key = OwnerKey::new(owner)?;
    let source_store_id = compatibility_source_store_id()?;
    let mut rows = transaction
        .query(
            "SELECT 1
             FROM memory_v2_compatibility_bank_dirty AS dirty
             WHERE dirty.owner_kind = ?1 AND dirty.project_id = ?2
               AND dirty.owner_json = ?3 AND dirty.source_store_id = ?4
             LIMIT 1",
            params![
                key.kind,
                key.project_id.as_str(),
                key.json.as_str(),
                source_store_id.as_str(),
            ],
        )
        .await
        .map_err(|error| storage_error(COMPATIBILITY_READ_OPERATION, error))?;
    Ok(rows
        .next()
        .await
        .map_err(|error| storage_error(COMPATIBILITY_READ_OPERATION, error))?
        .is_some())
}

async fn compatibility_feedback_history_repair_progress_tx(
    transaction: &Transaction,
    owner: &FactOwnerV1,
) -> FactCompatibilityResult<CompatibilityFeedbackRepairProgressV1> {
    let key = OwnerKey::new(owner)?;
    let source_store_id = compatibility_source_store_id()?;
    let mut rows = transaction
        .query(
            "SELECT owner_json, feedback_frontier, feedback_cursor, phase
             FROM memory_v2_feedback_history_repair_progress
             WHERE owner_kind = ?1 AND project_id = ?2 AND source_store_id = ?3",
            params![key.kind, key.project_id.as_str(), source_store_id.as_str()],
        )
        .await
        .map_err(|error| storage_error(COMPATIBILITY_READ_OPERATION, error))?;
    let Some(row) = rows
        .next()
        .await
        .map_err(|error| storage_error(COMPATIBILITY_READ_OPERATION, error))?
    else {
        return Ok(CompatibilityFeedbackRepairProgressV1::NotRequired);
    };
    if row_string(&row, 0, COMPATIBILITY_READ_OPERATION)? != key.json {
        return Err(FactStoreError::OwnerMismatch.into());
    }
    let frontier = nonnegative_u64(
        row_i64(&row, 1, COMPATIBILITY_READ_OPERATION)?,
        "feedback repair frontier",
    )?;
    let cursor = nonnegative_u64(
        row_i64(&row, 2, COMPATIBILITY_READ_OPERATION)?,
        "feedback repair cursor",
    )?;
    if cursor > frontier {
        return Err(storage_message(
            COMPATIBILITY_READ_OPERATION,
            "feedback repair cursor exceeds captured frontier",
        )
        .into());
    }
    match row_string(&row, 3, COMPATIBILITY_READ_OPERATION)?.as_str() {
        "pending" => Ok(CompatibilityFeedbackRepairProgressV1::Incomplete {
            processed: 0,
            remaining: Some(frontier.saturating_sub(cursor)),
        }),
        "complete" => Ok(CompatibilityFeedbackRepairProgressV1::Complete { processed: 0 }),
        _ => Err(storage_message(
            COMPATIBILITY_READ_OPERATION,
            "feedback repair progress has an unsupported phase",
        )
        .into()),
    }
}

async fn compatibility_memory_status_tx(
    transaction: &Transaction,
    owner: &FactOwnerV1,
    feedback_repair: CompatibilityFeedbackRepairProgressV1,
) -> FactCompatibilityResult<CompatibilityMemoryStatusV1> {
    let (
        fact_count,
        helpful_count,
        unhelpful_count,
        trust,
        below_default_recall_threshold_count,
        retrieval_count_total,
        access_count_total,
        retrieved_fact_count,
        rated_fact_count,
        feedback_total,
    ) = compatibility_owner_status_counts_tx(transaction, owner).await?;
    let key = OwnerKey::new(owner)?;
    let source_store_id = compatibility_source_store_id()?;
    let mut entity_rows = transaction
        .query(
            "SELECT COUNT(DISTINCT relations.entity_id)
             FROM memory_v2_legacy_map AS mappings
             JOIN memory_fact_entities AS relations ON relations.fact_id = mappings.legacy_fact_id
             WHERE mappings.owner_kind = ?1 AND mappings.project_id = ?2
               AND mappings.owner_json = ?3 AND mappings.source_store_id = ?4",
            params![
                key.kind,
                key.project_id.as_str(),
                key.json.as_str(),
                source_store_id.as_str()
            ],
        )
        .await
        .map_err(|error| storage_error(COMPATIBILITY_READ_OPERATION, error))?;
    let entity_row = entity_rows
        .next()
        .await
        .map_err(|error| storage_error(COMPATIBILITY_READ_OPERATION, error))?
        .ok_or_else(|| {
            storage_message(
                COMPATIBILITY_READ_OPERATION,
                "compatibility entity count is missing",
            )
        })?;
    let entity_count = nonnegative_u64(
        row_i64(&entity_row, 0, COMPATIBILITY_READ_OPERATION)?,
        "entity count",
    )?;
    let mut missing_rows = transaction
        .query(
            "SELECT COUNT(*) FROM memory_v2_legacy_map AS mappings
             JOIN memory_facts AS legacy_facts ON legacy_facts.fact_id = mappings.legacy_fact_id
             JOIN memory_v2_current_facts AS current_facts
               ON current_facts.fact_id = mappings.fact_id
              AND current_facts.owner_kind = mappings.owner_kind
              AND current_facts.project_id = mappings.project_id
             JOIN memory_v2_assertion_payloads AS payloads
               ON payloads.assertion_id = current_facts.active_assertion_id
              AND payloads.fact_id = current_facts.fact_id
              AND payloads.owner_kind = current_facts.owner_kind
              AND payloads.project_id = current_facts.project_id
             WHERE mappings.owner_kind = ?1 AND mappings.project_id = ?2
               AND mappings.owner_json = ?3 AND mappings.source_store_id = ?4
               AND current_facts.payload_access = 'eligible'
               AND (legacy_facts.hrr_vector IS NULL
                    OR legacy_facts.hrr_algebra <> 'amari_fhrr'
                    OR legacy_facts.hrr_dim <> ?5
                    OR legacy_facts.hrr_precision <> ?6
                    OR length(legacy_facts.hrr_vector) <> ?7)",
            params![
                key.kind,
                key.project_id.as_str(),
                key.json.as_str(),
                source_store_id.as_str(),
                HolographicEncoder::DIMENSIONS as i64,
                HolographicEncoder::HRR_PRECISION,
                HolographicEncoder::SERIALIZED_F32_BYTES as i64,
            ],
        )
        .await
        .map_err(|error| storage_error(COMPATIBILITY_READ_OPERATION, error))?;
    let missing_row = missing_rows
        .next()
        .await
        .map_err(|error| storage_error(COMPATIBILITY_READ_OPERATION, error))?
        .ok_or_else(|| {
            storage_message(
                COMPATIBILITY_READ_OPERATION,
                "compatibility missing vector count is missing",
            )
        })?;
    let missing_vector_count = nonnegative_u64(
        row_i64(&missing_row, 0, COMPATIBILITY_READ_OPERATION)?,
        "missing vector count",
    )?;
    let dirty_banks = compatibility_owner_has_dirty_banks_tx(transaction, owner).await?;
    let mut bank_rows = transaction
        .query(
            "SELECT COUNT(*) FROM memory_v2_compatibility_banks AS banks
             WHERE banks.owner_kind = ?1 AND banks.project_id = ?2
               AND banks.owner_json = ?3 AND banks.source_store_id = ?4",
            params![
                key.kind,
                key.project_id.as_str(),
                key.json.as_str(),
                source_store_id.as_str()
            ],
        )
        .await
        .map_err(|error| storage_error(COMPATIBILITY_READ_OPERATION, error))?;
    let bank_row = bank_rows
        .next()
        .await
        .map_err(|error| storage_error(COMPATIBILITY_READ_OPERATION, error))?
        .ok_or_else(|| {
            storage_message(
                COMPATIBILITY_READ_OPERATION,
                "compatibility bank count is missing",
            )
        })?;
    let bank_count = nonnegative_u64(
        row_i64(&bank_row, 0, COMPATIBILITY_READ_OPERATION)?,
        "bank count",
    )?;
    let mut backfill_rows = transaction
        .query(
            "SELECT phase, owner_json FROM memory_v2_backfill_progress
             WHERE owner_kind = ?1 AND project_id = ?2 AND source_store_id = ?3",
            params![key.kind, key.project_id.as_str(), source_store_id.as_str()],
        )
        .await
        .map_err(|error| storage_error(COMPATIBILITY_READ_OPERATION, error))?;
    let legacy_backfill_complete = match backfill_rows
        .next()
        .await
        .map_err(|error| storage_error(COMPATIBILITY_READ_OPERATION, error))?
    {
        None => true,
        Some(row) => {
            row_string(&row, 1, COMPATIBILITY_READ_OPERATION)? == key.json
                && row_string(&row, 0, COMPATIBILITY_READ_OPERATION)? == "cutover_complete"
        }
    };
    let projection_state = if missing_vector_count == 0 && !dirty_banks {
        CompatibilityProjectionStateV1::Ready
    } else {
        CompatibilityProjectionStateV1::Rebuilding
    };
    CompatibilityMemoryStatusV1::new(
        owner.clone(),
        fact_count,
        entity_count,
        bank_count,
        CompatibilityMemoryAlgebraV1::new(
            "amari_fhrr".to_owned(),
            HolographicEncoder::DIMENSIONS as u64,
            fact_count.saturating_mul(HolographicEncoder::DIMENSIONS as u64),
        )?,
        trust[0],
        trust[1],
        trust[2],
        trust[3],
        below_default_recall_threshold_count,
        helpful_count,
        unhelpful_count,
        missing_vector_count,
        legacy_backfill_complete,
        projection_state,
        CompatibilityMemoryRepairStatsV1::new(0, 0),
        CompatibilityMemoryFeedbackFunnelV1::new(
            retrieval_count_total,
            access_count_total,
            retrieved_fact_count,
            rated_fact_count,
            feedback_total,
        ),
    )
    .map(|status| status.with_feedback_history_repair(feedback_repair))
    .map_err(Into::into)
}

async fn inspect_compatibility_fact_tx(
    transaction: &Transaction,
    target: &CompatibilityFactTargetV1,
) -> FactCompatibilityResult<Option<CompatibilityFactInspectionV1>> {
    let Some(fact_id) = resolve_compatibility_target_tx(transaction, target).await? else {
        return Ok(None);
    };
    let Some(CompatibilityFactProjectionV1::Available(fact)) =
        load_compatibility_projection_tx(transaction, target.owner(), &fact_id).await?
    else {
        return Ok(None);
    };
    let lineage = FactLineageQuery::new(target.owner().clone(), fact_id.clone(), None, 1_000)?;
    let history = CompatibilityFactHistoryV1::new(
        target.owner().clone(),
        fact_id.clone(),
        query_fact_lineage_tx(transaction, &lineage).await?,
        None,
    )?;
    let key = OwnerKey::new(target.owner())?;
    let mut rows = transaction
        .query(
            "SELECT DISTINCT anchors.anchor_json
             FROM memory_v2_evidence AS evidence
             JOIN retrieval_anchors AS anchors
               ON anchors.anchor_id = evidence.anchor_id
              AND anchors.owner_json = evidence.owner_json
             WHERE evidence.fact_id = ?1
               AND evidence.owner_kind = ?2
               AND evidence.project_id = ?3
               AND evidence.owner_json = ?4
             ORDER BY anchors.anchor_id ASC
             LIMIT 1000",
            params![
                fact_id.as_str(),
                key.kind,
                key.project_id.as_str(),
                key.json.as_str(),
            ],
        )
        .await
        .map_err(|error| storage_error(COMPATIBILITY_READ_OPERATION, error))?;
    let mut anchors = Vec::new();
    while let Some(row) = rows
        .next()
        .await
        .map_err(|error| storage_error(COMPATIBILITY_READ_OPERATION, error))?
    {
        let anchor = from_json::<RetrievalAnchorRecordV2>(
            &row_string(&row, 0, COMPATIBILITY_READ_OPERATION)?,
            COMPATIBILITY_READ_OPERATION,
        )?;
        if FactOwnerV1::from(anchor.owner().clone()) != *target.owner() {
            return Err(FactStoreError::OwnerMismatch.into());
        }
        anchors.push(anchor);
    }
    let status = compatibility_fact_status_tx(transaction, target.owner(), &fact_id)
        .await?
        .ok_or_else(|| {
            storage_message(
                COMPATIBILITY_READ_OPERATION,
                "compatibility inspection status is missing",
            )
        })?;
    CompatibilityFactInspectionV1::new(*fact, history, anchors, status)
        .map(Some)
        .map_err(Into::into)
}

struct CommitAttempt {
    outcome: FactCommitOutcome,
    wrote: bool,
}

struct PromotionAttempt {
    outcome: PromoteFactProposalOutcome,
    wrote: bool,
}

// Dashboard reads deliberately start from the immutable owner-bound V1 mapping.
// The legacy tables remain a compatibility projection, never an alternate fact
// authority or a source for ownerless rows.
async fn dashboard_compatibility_fact_summaries_tx(
    transaction: &Transaction,
    owner: &FactOwnerV1,
    limit: usize,
) -> FactCompatibilityResult<Vec<tracedecay_store::CompatibilityDashboardFactSummaryV1>> {
    let key = OwnerKey::new(owner)?;
    let source_store_id = compatibility_source_store_id()?;
    let limit = i64::try_from(limit).map_err(|_| FactStoreError::InvalidQueryLimit {
        limit,
        max: usize::MAX,
    })?;
    let mut rows = transaction
        .query(
            "SELECT mappings.fact_id, legacy_facts.hrr_vector IS NOT NULL
             FROM memory_v2_legacy_map AS mappings
             JOIN memory_facts AS legacy_facts
               ON legacy_facts.fact_id = mappings.legacy_fact_id
             WHERE mappings.owner_kind = ?1
               AND mappings.project_id = ?2
               AND mappings.owner_json = ?3
               AND mappings.source_store_id = ?4
             ORDER BY legacy_facts.trust_score DESC,
                      legacy_facts.updated_at DESC,
                      mappings.fact_id ASC
             LIMIT ?5",
            params![
                key.kind,
                key.project_id.as_str(),
                key.json.as_str(),
                source_store_id.as_str(),
                limit,
            ],
        )
        .await
        .map_err(|error| storage_error(COMPATIBILITY_READ_OPERATION, error))?;
    let mut facts = Vec::with_capacity(usize::try_from(limit).unwrap_or_default());
    while let Some(row) = rows
        .next()
        .await
        .map_err(|error| storage_error(COMPATIBILITY_READ_OPERATION, error))?
    {
        let fact_id = FactId::new(row_string(&row, 0, COMPATIBILITY_READ_OPERATION)?)
            .map_err(FactStoreError::from)?;
        let Some(fact) = load_compatibility_projection_tx(transaction, owner, &fact_id).await?
        else {
            return Err(storage_message(
                COMPATIBILITY_READ_OPERATION,
                "owner-bound dashboard mapping has no canonical fact projection",
            )
            .into());
        };
        facts.push(tracedecay_store::CompatibilityDashboardFactSummaryV1 {
            has_hrr_vector: row_i64(&row, 1, COMPATIBILITY_READ_OPERATION)? != 0
                && matches!(&fact, CompatibilityFactProjectionV1::Available(_)),
            fact,
        });
    }
    Ok(facts)
}

async fn dashboard_compatibility_entities_tx(
    transaction: &Transaction,
    owner: &FactOwnerV1,
    limit: usize,
) -> FactCompatibilityResult<Vec<tracedecay_store::CompatibilityDashboardEntityV1>> {
    let key = OwnerKey::new(owner)?;
    let source_store_id = compatibility_source_store_id()?;
    let limit = i64::try_from(limit).map_err(|_| FactStoreError::InvalidQueryLimit {
        limit,
        max: usize::MAX,
    })?;
    let mut rows = transaction
        .query(
            "SELECT entities.entity_id, entities.name, entities.entity_type,
                    entities.aliases, entities.created_at,
                    COUNT(DISTINCT mappings.legacy_fact_id)
             FROM memory_v2_legacy_map AS mappings
             JOIN memory_facts AS legacy_facts
               ON legacy_facts.fact_id = mappings.legacy_fact_id
             JOIN memory_fact_entities AS relations
               ON relations.fact_id = legacy_facts.fact_id
             JOIN memory_entities AS entities
               ON entities.entity_id = relations.entity_id
             WHERE mappings.owner_kind = ?1
               AND mappings.project_id = ?2
               AND mappings.owner_json = ?3
               AND mappings.source_store_id = ?4
             GROUP BY entities.entity_id, entities.name, entities.entity_type,
                      entities.aliases, entities.created_at
             ORDER BY COUNT(DISTINCT mappings.legacy_fact_id) DESC,
                      entities.name ASC, entities.entity_id ASC
             LIMIT ?5",
            params![
                key.kind,
                key.project_id.as_str(),
                key.json.as_str(),
                source_store_id.as_str(),
                limit,
            ],
        )
        .await
        .map_err(|error| storage_error(COMPATIBILITY_READ_OPERATION, error))?;
    let mut entities = Vec::with_capacity(usize::try_from(limit).unwrap_or_default());
    while let Some(row) = rows
        .next()
        .await
        .map_err(|error| storage_error(COMPATIBILITY_READ_OPERATION, error))?
    {
        let aliases = from_json::<Vec<String>>(
            &row_string(&row, 3, COMPATIBILITY_READ_OPERATION)?,
            COMPATIBILITY_READ_OPERATION,
        )?;
        entities.push(tracedecay_store::CompatibilityDashboardEntityV1::new(
            tracedecay_store::CompatibilityLegacyEntityTargetV1::new(
                owner.clone(),
                row_i64(&row, 0, COMPATIBILITY_READ_OPERATION)?,
            )?,
            row_string(&row, 1, COMPATIBILITY_READ_OPERATION)?,
            row_string(&row, 2, COMPATIBILITY_READ_OPERATION)?,
            aliases,
            UtcMicros(row_i64(&row, 4, COMPATIBILITY_READ_OPERATION)?),
            nonnegative_u64(
                row_i64(&row, 5, COMPATIBILITY_READ_OPERATION)?,
                "dashboard entity fact count",
            )?,
        )?);
    }
    Ok(entities)
}

async fn dashboard_compatibility_fact_entity_links_tx(
    transaction: &Transaction,
    owner: &FactOwnerV1,
    fact_ids: &BTreeSet<String>,
    entity_ids: &BTreeSet<i64>,
    limit: usize,
) -> FactCompatibilityResult<Vec<tracedecay_store::CompatibilityDashboardFactEntityLinkV1>> {
    if fact_ids.is_empty() || entity_ids.is_empty() || limit == 0 {
        return Ok(Vec::new());
    }
    let key = OwnerKey::new(owner)?;
    let source_store_id = compatibility_source_store_id()?;
    let fetch_limit = i64::try_from(limit).map_err(|_| FactStoreError::InvalidQueryLimit {
        limit,
        max: usize::MAX,
    })?;
    let mut rows = transaction
        .query(
            "SELECT mappings.fact_id, relations.entity_id
             FROM memory_v2_legacy_map AS mappings
             JOIN memory_facts AS legacy_facts
               ON legacy_facts.fact_id = mappings.legacy_fact_id
             JOIN memory_fact_entities AS relations
               ON relations.fact_id = legacy_facts.fact_id
             WHERE mappings.owner_kind = ?1
               AND mappings.project_id = ?2
               AND mappings.owner_json = ?3
               AND mappings.source_store_id = ?4
             ORDER BY legacy_facts.trust_score DESC,
                      legacy_facts.updated_at DESC,
                      mappings.fact_id ASC, relations.entity_id ASC
             LIMIT ?5",
            params![
                key.kind,
                key.project_id.as_str(),
                key.json.as_str(),
                source_store_id.as_str(),
                fetch_limit,
            ],
        )
        .await
        .map_err(|error| storage_error(COMPATIBILITY_READ_OPERATION, error))?;
    let mut links = Vec::with_capacity(usize::try_from(fetch_limit).unwrap_or_default());
    while let Some(row) = rows
        .next()
        .await
        .map_err(|error| storage_error(COMPATIBILITY_READ_OPERATION, error))?
    {
        let fact_id = row_string(&row, 0, COMPATIBILITY_READ_OPERATION)?;
        let entity_id = row_i64(&row, 1, COMPATIBILITY_READ_OPERATION)?;
        if !fact_ids.contains(&fact_id) || !entity_ids.contains(&entity_id) {
            continue;
        }
        let fact_id = FactId::new(fact_id).map_err(FactStoreError::from)?;
        links.push(
            tracedecay_store::CompatibilityDashboardFactEntityLinkV1::new(
                CompatibilityFactTargetV1::Canonical(CompatibilityFactIdV1::new(
                    owner.clone(),
                    fact_id,
                )?),
                tracedecay_store::CompatibilityLegacyEntityTargetV1::new(owner.clone(), entity_id)?,
            )?,
        );
    }
    Ok(links)
}

async fn dashboard_compatibility_owner_count_tx(
    transaction: &Transaction,
    owner: &FactOwnerV1,
    entity_count: bool,
) -> FactCompatibilityResult<u64> {
    let key = OwnerKey::new(owner)?;
    let source_store_id = compatibility_source_store_id()?;
    let sql = if entity_count {
        "SELECT COUNT(DISTINCT relations.entity_id)
         FROM memory_v2_legacy_map AS mappings
         JOIN memory_facts AS legacy_facts
           ON legacy_facts.fact_id = mappings.legacy_fact_id
         JOIN memory_fact_entities AS relations
           ON relations.fact_id = legacy_facts.fact_id
         WHERE mappings.owner_kind = ?1
           AND mappings.project_id = ?2
           AND mappings.owner_json = ?3
           AND mappings.source_store_id = ?4"
    } else {
        "SELECT COUNT(*)
         FROM memory_v2_legacy_map AS mappings
         JOIN memory_facts AS legacy_facts
           ON legacy_facts.fact_id = mappings.legacy_fact_id
         WHERE mappings.owner_kind = ?1
           AND mappings.project_id = ?2
           AND mappings.owner_json = ?3
           AND mappings.source_store_id = ?4"
    };
    let mut rows = transaction
        .query(
            sql,
            params![
                key.kind,
                key.project_id.as_str(),
                key.json.as_str(),
                source_store_id.as_str(),
            ],
        )
        .await
        .map_err(|error| storage_error(COMPATIBILITY_READ_OPERATION, error))?;
    let row = rows
        .next()
        .await
        .map_err(|error| storage_error(COMPATIBILITY_READ_OPERATION, error))?
        .ok_or_else(|| {
            storage_message(
                COMPATIBILITY_READ_OPERATION,
                "compatibility dashboard owner count is missing",
            )
        })?;
    nonnegative_u64(
        row_i64(&row, 0, COMPATIBILITY_READ_OPERATION)?,
        "compatibility dashboard owner count",
    )
    .map_err(Into::into)
}

#[derive(Clone, Copy)]
enum CompatibilityDashboardNamedCountKind {
    Category,
    EntityType,
    TrustBucket,
}

async fn dashboard_compatibility_named_counts_tx(
    transaction: &Transaction,
    owner: &FactOwnerV1,
    kind: CompatibilityDashboardNamedCountKind,
) -> FactCompatibilityResult<Vec<tracedecay_store::CompatibilityDashboardNamedCountV1>> {
    let key = OwnerKey::new(owner)?;
    let source_store_id = compatibility_source_store_id()?;
    let (sql, limit) = match kind {
        CompatibilityDashboardNamedCountKind::Category => (
            "SELECT legacy_facts.category, COUNT(*)
             FROM memory_v2_legacy_map AS mappings
             JOIN memory_facts AS legacy_facts
               ON legacy_facts.fact_id = mappings.legacy_fact_id
             WHERE mappings.owner_kind = ?1
               AND mappings.project_id = ?2
               AND mappings.owner_json = ?3
               AND mappings.source_store_id = ?4
             GROUP BY legacy_facts.category
             ORDER BY COUNT(*) DESC, legacy_facts.category ASC
             LIMIT 128",
            128,
        ),
        CompatibilityDashboardNamedCountKind::EntityType => (
            "SELECT entities.entity_type, COUNT(DISTINCT entities.entity_id)
             FROM memory_v2_legacy_map AS mappings
             JOIN memory_facts AS legacy_facts
               ON legacy_facts.fact_id = mappings.legacy_fact_id
             JOIN memory_fact_entities AS relations
               ON relations.fact_id = legacy_facts.fact_id
             JOIN memory_entities AS entities
               ON entities.entity_id = relations.entity_id
             WHERE mappings.owner_kind = ?1
               AND mappings.project_id = ?2
               AND mappings.owner_json = ?3
               AND mappings.source_store_id = ?4
             GROUP BY entities.entity_type
             ORDER BY COUNT(DISTINCT entities.entity_id) DESC, entities.entity_type ASC
             LIMIT 128",
            128,
        ),
        CompatibilityDashboardNamedCountKind::TrustBucket => (
            "SELECT CASE
                        WHEN legacy_facts.trust_score < 0.0 THEN 0
                        WHEN legacy_facts.trust_score >= 1.0 THEN 9
                        ELSE CAST(legacy_facts.trust_score * 10.0 AS INTEGER)
                    END AS bucket,
                    COUNT(*)
             FROM memory_v2_legacy_map AS mappings
             JOIN memory_facts AS legacy_facts
               ON legacy_facts.fact_id = mappings.legacy_fact_id
             WHERE mappings.owner_kind = ?1
               AND mappings.project_id = ?2
               AND mappings.owner_json = ?3
               AND mappings.source_store_id = ?4
             GROUP BY bucket
             ORDER BY bucket ASC
             LIMIT 10",
            10,
        ),
    };
    let mut rows = transaction
        .query(
            sql,
            params![
                key.kind,
                key.project_id.as_str(),
                key.json.as_str(),
                source_store_id.as_str(),
            ],
        )
        .await
        .map_err(|error| storage_error(COMPATIBILITY_READ_OPERATION, error))?;
    let mut counts = Vec::with_capacity(limit);
    while let Some(row) = rows
        .next()
        .await
        .map_err(|error| storage_error(COMPATIBILITY_READ_OPERATION, error))?
    {
        let name = match kind {
            CompatibilityDashboardNamedCountKind::TrustBucket => {
                format!("trust-{}", row_i64(&row, 0, COMPATIBILITY_READ_OPERATION)?)
            }
            CompatibilityDashboardNamedCountKind::Category
            | CompatibilityDashboardNamedCountKind::EntityType => {
                row_string(&row, 0, COMPATIBILITY_READ_OPERATION)?
            }
        };
        counts.push(tracedecay_store::CompatibilityDashboardNamedCountV1::new(
            name,
            nonnegative_u64(
                row_i64(&row, 1, COMPATIBILITY_READ_OPERATION)?,
                "compatibility dashboard named count",
            )?,
        )?);
    }
    Ok(counts)
}

fn dashboard_compatibility_dimension(
    dimension: Option<i64>,
) -> FactCompatibilityResult<Option<u32>> {
    dimension
        .map(|value| {
            let value = u32::try_from(value).map_err(|_| {
                storage_message(
                    COMPATIBILITY_READ_OPERATION,
                    "dashboard HRR dimension is outside u32 range",
                )
            })?;
            if value == 0 {
                return Err(storage_message(
                    COMPATIBILITY_READ_OPERATION,
                    "dashboard HRR dimension must be positive",
                ));
            }
            Ok(value)
        })
        .transpose()
        .map_err(Into::into)
}

async fn dashboard_compatibility_hrr_coverage_tx(
    transaction: &Transaction,
    owner: &FactOwnerV1,
) -> FactCompatibilityResult<Vec<tracedecay_store::CompatibilityDashboardHrrCoverageV1>> {
    let key = OwnerKey::new(owner)?;
    let source_store_id = compatibility_source_store_id()?;
    let mut rows = transaction
        .query(
            "SELECT legacy_facts.category,
                    COUNT(*),
                    COALESCE(SUM(CASE WHEN legacy_facts.hrr_vector IS NOT NULL THEN 1 ELSE 0 END), 0),
                    MAX(CASE WHEN banks.bank_name IS NULL THEN 0 ELSE 1 END),
                    MAX(banks.hrr_dim),
                    MAX(banks.updated_at),
                    MAX(CASE WHEN dirty.bank_name IS NULL THEN 0 ELSE 1 END)
             FROM memory_v2_legacy_map AS mappings
             JOIN memory_facts AS legacy_facts
               ON legacy_facts.fact_id = mappings.legacy_fact_id
             LEFT JOIN memory_v2_compatibility_banks AS banks
               ON banks.owner_kind = mappings.owner_kind
              AND banks.project_id = mappings.project_id
              AND banks.source_store_id = mappings.source_store_id
              AND banks.owner_json = mappings.owner_json
              AND banks.bank_name = legacy_facts.category
             LEFT JOIN memory_v2_compatibility_bank_dirty AS dirty
               ON dirty.owner_kind = mappings.owner_kind
              AND dirty.project_id = mappings.project_id
              AND dirty.source_store_id = mappings.source_store_id
              AND dirty.owner_json = mappings.owner_json
              AND dirty.bank_name = legacy_facts.category
             WHERE mappings.owner_kind = ?1
               AND mappings.project_id = ?2
               AND mappings.owner_json = ?3
               AND mappings.source_store_id = ?4
             GROUP BY legacy_facts.category
             ORDER BY COUNT(*) DESC, legacy_facts.category ASC
             LIMIT 128",
            params![
                key.kind,
                key.project_id.as_str(),
                key.json.as_str(),
                source_store_id.as_str(),
            ],
        )
        .await
        .map_err(|error| storage_error(COMPATIBILITY_READ_OPERATION, error))?;
    let mut coverage = Vec::new();
    while let Some(row) = rows
        .next()
        .await
        .map_err(|error| storage_error(COMPATIBILITY_READ_OPERATION, error))?
    {
        let category = row_string(&row, 0, COMPATIBILITY_READ_OPERATION)?;
        let fact_count = nonnegative_u64(
            row_i64(&row, 1, COMPATIBILITY_READ_OPERATION)?,
            "dashboard category fact count",
        )?;
        let vector_count = nonnegative_u64(
            row_i64(&row, 2, COMPATIBILITY_READ_OPERATION)?,
            "dashboard category vector count",
        )?;
        let has_bank = row_i64(&row, 3, COMPATIBILITY_READ_OPERATION)? != 0;
        let dirty = row_i64(&row, 6, COMPATIBILITY_READ_OPERATION)? != 0;
        let state = if vector_count < fact_count {
            tracedecay_store::CompatibilityDashboardHrrStateV1::MissingVectors
        } else if !has_bank {
            tracedecay_store::CompatibilityDashboardHrrStateV1::MissingBank
        } else if dirty {
            tracedecay_store::CompatibilityDashboardHrrStateV1::StaleBank
        } else {
            tracedecay_store::CompatibilityDashboardHrrStateV1::Ready
        };
        let coverage_basis_points = vector_count
            .saturating_mul(10_000)
            .checked_div(fact_count)
            .map_or(0, |basis| u16::try_from(basis).unwrap_or(10_000));
        coverage.push(tracedecay_store::CompatibilityDashboardHrrCoverageV1::new(
            category.clone(),
            fact_count,
            vector_count,
            coverage_basis_points,
            category,
            if has_bank { vector_count } else { 0 },
            dashboard_compatibility_dimension(row_optional_i64(
                &row,
                4,
                COMPATIBILITY_READ_OPERATION,
            )?)?,
            row_optional_i64(&row, 5, COMPATIBILITY_READ_OPERATION)?.map(UtcMicros),
            state,
        )?);
    }
    Ok(coverage)
}

fn dashboard_compatibility_memory_bank_from_row(
    row: &libsql::Row,
) -> FactCompatibilityResult<tracedecay_store::CompatibilityDashboardMemoryBankV1> {
    tracedecay_store::CompatibilityDashboardMemoryBankV1::new(
        row_string(row, 0, COMPATIBILITY_READ_OPERATION)?,
        dashboard_compatibility_dimension(row_optional_i64(row, 1, COMPATIBILITY_READ_OPERATION)?)?,
        nonnegative_u64(
            row_i64(row, 3, COMPATIBILITY_READ_OPERATION)?,
            "dashboard bank fact count",
        )?,
        nonnegative_u64(
            row_i64(row, 4, COMPATIBILITY_READ_OPERATION)?,
            "dashboard bank bundled fact count",
        )?,
        row_optional_i64(row, 2, COMPATIBILITY_READ_OPERATION)?.map(UtcMicros),
    )
    .map_err(Into::into)
}

async fn dashboard_compatibility_memory_banks_tx(
    transaction: &Transaction,
    owner: &FactOwnerV1,
) -> FactCompatibilityResult<Vec<tracedecay_store::CompatibilityDashboardMemoryBankV1>> {
    let key = OwnerKey::new(owner)?;
    let source_store_id = compatibility_source_store_id()?;
    let mut rows = transaction
        .query(
            "SELECT banks.bank_name, banks.hrr_dim, banks.updated_at,
                    banks.fact_count, banks.fact_count
             FROM memory_v2_compatibility_banks AS banks
             WHERE banks.owner_kind = ?1
               AND banks.project_id = ?2
               AND banks.owner_json = ?3
               AND banks.source_store_id = ?4
             ORDER BY banks.fact_count DESC, banks.bank_name ASC
             LIMIT 128",
            params![
                key.kind,
                key.project_id.as_str(),
                key.json.as_str(),
                source_store_id.as_str(),
            ],
        )
        .await
        .map_err(|error| storage_error(COMPATIBILITY_READ_OPERATION, error))?;
    let mut banks = Vec::new();
    while let Some(row) = rows
        .next()
        .await
        .map_err(|error| storage_error(COMPATIBILITY_READ_OPERATION, error))?
    {
        banks.push(dashboard_compatibility_memory_bank_from_row(&row)?);
    }
    Ok(banks)
}

async fn dashboard_compatibility_growth_tx(
    transaction: &Transaction,
    owner: &FactOwnerV1,
) -> FactCompatibilityResult<Vec<tracedecay_store::CompatibilityDashboardGrowthPointV1>> {
    let key = OwnerKey::new(owner)?;
    let source_store_id = compatibility_source_store_id()?;
    let mut rows = transaction
        .query(
            "WITH latest_days AS (
                 SELECT date(legacy_facts.created_at, 'unixepoch') AS period,
                        COUNT(*) AS fact_count
                 FROM memory_v2_legacy_map AS mappings
                 JOIN memory_facts AS legacy_facts
                   ON legacy_facts.fact_id = mappings.legacy_fact_id
                 WHERE mappings.owner_kind = ?1
                   AND mappings.project_id = ?2
                   AND mappings.owner_json = ?3
                   AND mappings.source_store_id = ?4
                   AND legacy_facts.created_at > 0
                 GROUP BY period
                 ORDER BY period DESC
                 LIMIT 180
             ), prior AS (
                 SELECT COUNT(*) AS fact_count
                 FROM memory_v2_legacy_map AS mappings
                 JOIN memory_facts AS legacy_facts
                   ON legacy_facts.fact_id = mappings.legacy_fact_id
                 WHERE mappings.owner_kind = ?5
                   AND mappings.project_id = ?6
                   AND mappings.owner_json = ?7
                   AND mappings.source_store_id = ?8
                   AND legacy_facts.created_at > 0
                   AND date(legacy_facts.created_at, 'unixepoch') < (
                       SELECT MIN(period) FROM latest_days
                   )
             )
             SELECT latest_days.period, latest_days.fact_count,
                    prior.fact_count + SUM(latest_days.fact_count)
                        OVER (ORDER BY latest_days.period ASC)
             FROM latest_days CROSS JOIN prior
             ORDER BY latest_days.period ASC",
            params![
                key.kind,
                key.project_id.as_str(),
                key.json.as_str(),
                source_store_id.as_str(),
                key.kind,
                key.project_id.as_str(),
                key.json.as_str(),
                source_store_id.as_str(),
            ],
        )
        .await
        .map_err(|error| storage_error(COMPATIBILITY_READ_OPERATION, error))?;
    let mut growth = Vec::new();
    while let Some(row) = rows
        .next()
        .await
        .map_err(|error| storage_error(COMPATIBILITY_READ_OPERATION, error))?
    {
        growth.push(tracedecay_store::CompatibilityDashboardGrowthPointV1::new(
            row_string(&row, 0, COMPATIBILITY_READ_OPERATION)?,
            nonnegative_u64(
                row_i64(&row, 1, COMPATIBILITY_READ_OPERATION)?,
                "dashboard daily fact count",
            )?,
            nonnegative_u64(
                row_i64(&row, 2, COMPATIBILITY_READ_OPERATION)?,
                "dashboard cumulative fact count",
            )?,
        )?);
    }
    Ok(growth)
}

async fn dashboard_compatibility_memory_overview_tx(
    transaction: &Transaction,
    query: &CompatibilityDashboardMemoryOverviewQueryV1,
) -> FactCompatibilityResult<CompatibilityDashboardMemoryOverviewV1> {
    let owner = query.owner();
    let fact_count = dashboard_compatibility_owner_count_tx(transaction, owner, false).await?;
    let entity_count = dashboard_compatibility_owner_count_tx(transaction, owner, true).await?;
    let facts =
        dashboard_compatibility_fact_summaries_tx(transaction, owner, query.fact_limit()).await?;
    let entities =
        dashboard_compatibility_entities_tx(transaction, owner, query.graph_limit()).await?;
    let fact_ids = facts
        .iter()
        .map(|fact| fact.fact.fact_id().as_str().to_owned())
        .collect::<BTreeSet<_>>();
    let entity_ids = entities
        .iter()
        .map(|entity| entity.target.legacy_entity_id())
        .collect::<BTreeSet<_>>();
    let fact_entity_links = dashboard_compatibility_fact_entity_links_tx(
        transaction,
        owner,
        &fact_ids,
        &entity_ids,
        query.graph_limit(),
    )
    .await?;
    let categories = dashboard_compatibility_named_counts_tx(
        transaction,
        owner,
        CompatibilityDashboardNamedCountKind::Category,
    )
    .await?;
    let entity_types = dashboard_compatibility_named_counts_tx(
        transaction,
        owner,
        CompatibilityDashboardNamedCountKind::EntityType,
    )
    .await?;
    let hrr_coverage = dashboard_compatibility_hrr_coverage_tx(transaction, owner).await?;
    let memory_banks = dashboard_compatibility_memory_banks_tx(transaction, owner).await?;
    let trust_histogram = dashboard_compatibility_named_counts_tx(
        transaction,
        owner,
        CompatibilityDashboardNamedCountKind::TrustBucket,
    )
    .await?;
    let growth = dashboard_compatibility_growth_tx(transaction, owner).await?;
    CompatibilityDashboardMemoryOverviewV1::new(
        owner.clone(),
        fact_count,
        entity_count,
        memory_banks.len() as u64,
        facts,
        entities,
        fact_entity_links,
        categories,
        entity_types,
        hrr_coverage,
        memory_banks,
        trust_histogram,
        growth,
    )
    .map_err(Into::into)
}

async fn dashboard_compatibility_entities_for_fact_tx(
    transaction: &Transaction,
    owner: &FactOwnerV1,
    fact_id: &FactId,
) -> FactCompatibilityResult<Vec<tracedecay_store::CompatibilityDashboardEntityV1>> {
    let key = OwnerKey::new(owner)?;
    let source_store_id = compatibility_source_store_id()?;
    let mut rows = transaction
        .query(
            "SELECT entities.entity_id, entities.name, entities.entity_type,
                    entities.aliases, entities.created_at,
                    COUNT(DISTINCT related_mappings.legacy_fact_id)
             FROM memory_v2_legacy_map AS target_mappings
             JOIN memory_facts AS target_facts
               ON target_facts.fact_id = target_mappings.legacy_fact_id
             JOIN memory_fact_entities AS target_relations
               ON target_relations.fact_id = target_facts.fact_id
             JOIN memory_entities AS entities
               ON entities.entity_id = target_relations.entity_id
             LEFT JOIN memory_fact_entities AS related_relations
               ON related_relations.entity_id = entities.entity_id
             LEFT JOIN memory_v2_legacy_map AS related_mappings
               ON related_mappings.legacy_fact_id = related_relations.fact_id
              AND related_mappings.owner_kind = ?1
              AND related_mappings.project_id = ?2
              AND related_mappings.owner_json = ?3
              AND related_mappings.source_store_id = ?4
             WHERE target_mappings.owner_kind = ?1
               AND target_mappings.project_id = ?2
               AND target_mappings.owner_json = ?3
               AND target_mappings.source_store_id = ?4
               AND target_mappings.fact_id = ?5
             GROUP BY entities.entity_id, entities.name, entities.entity_type,
                      entities.aliases, entities.created_at
             ORDER BY entities.name ASC, entities.entity_id ASC
             LIMIT 128",
            params![
                key.kind,
                key.project_id.as_str(),
                key.json.as_str(),
                source_store_id.as_str(),
                fact_id.as_str(),
            ],
        )
        .await
        .map_err(|error| storage_error(COMPATIBILITY_READ_OPERATION, error))?;
    let mut entities = Vec::new();
    while let Some(row) = rows
        .next()
        .await
        .map_err(|error| storage_error(COMPATIBILITY_READ_OPERATION, error))?
    {
        entities.push(tracedecay_store::CompatibilityDashboardEntityV1::new(
            tracedecay_store::CompatibilityLegacyEntityTargetV1::new(
                owner.clone(),
                row_i64(&row, 0, COMPATIBILITY_READ_OPERATION)?,
            )?,
            row_string(&row, 1, COMPATIBILITY_READ_OPERATION)?,
            row_string(&row, 2, COMPATIBILITY_READ_OPERATION)?,
            from_json::<Vec<String>>(
                &row_string(&row, 3, COMPATIBILITY_READ_OPERATION)?,
                COMPATIBILITY_READ_OPERATION,
            )?,
            UtcMicros(row_i64(&row, 4, COMPATIBILITY_READ_OPERATION)?),
            nonnegative_u64(
                row_i64(&row, 5, COMPATIBILITY_READ_OPERATION)?,
                "dashboard entity fact count",
            )?,
        )?);
    }
    Ok(entities)
}

async fn dashboard_compatibility_fact_detail_tx(
    transaction: &Transaction,
    query: &CompatibilityDashboardFactDetailQueryV1,
) -> FactCompatibilityResult<Option<CompatibilityDashboardFactDetailV1>> {
    let owner = query.target().owner();
    let Some(fact_id) = resolve_compatibility_target_tx(transaction, query.target()).await? else {
        return Ok(None);
    };
    if compatibility_legacy_mapping_tx(transaction, owner, &fact_id)
        .await?
        .is_none()
    {
        return Ok(None);
    }
    let Some(fact) = load_compatibility_projection_tx(transaction, owner, &fact_id).await? else {
        return Ok(None);
    };
    let entities =
        dashboard_compatibility_entities_for_fact_tx(transaction, owner, &fact_id).await?;
    let target =
        CompatibilityFactTargetV1::Canonical(CompatibilityFactIdV1::new(owner.clone(), fact_id)?);
    let history = compatibility_fact_history_tx(
        transaction,
        &CompatibilityFactHistoryQueryV1::new(target, None, 128)?,
    )
    .await?;
    CompatibilityDashboardFactDetailV1::new(fact, entities, Some(history))
        .map(Some)
        .map_err(Into::into)
}

fn dashboard_compatibility_like_pattern(search: &str) -> String {
    let escaped = search
        .replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_");
    format!("%{escaped}%")
}

async fn dashboard_compatibility_vector_points_tx(
    transaction: &Transaction,
    query: &CompatibilityDashboardVectorPointsQueryV1,
) -> FactCompatibilityResult<Vec<CompatibilityDashboardVectorPointV1>> {
    let key = OwnerKey::new(query.owner())?;
    let source_store_id = compatibility_source_store_id()?;
    let limit = i64::try_from(query.limit()).map_err(|_| FactStoreError::InvalidQueryLimit {
        limit: query.limit(),
        max: usize::MAX,
    })?;
    let search = query
        .search()
        .filter(|search| !search.trim().is_empty())
        .map(dashboard_compatibility_like_pattern);
    let mut rows = transaction
        .query(
            // The V1 dashboard reported a fact's graph connections as its
            // entity-link count; parity keeps both columns on that basis.
            "SELECT mappings.fact_id, legacy_facts.hrr_vector, banks.bank_name,
                    COUNT(DISTINCT relations.entity_id),
                    COUNT(DISTINCT relations.entity_id)
             FROM memory_v2_legacy_map AS mappings
             JOIN memory_facts AS legacy_facts
               ON legacy_facts.fact_id = mappings.legacy_fact_id
             LEFT JOIN memory_v2_compatibility_banks AS banks
               ON banks.owner_kind = mappings.owner_kind
              AND banks.project_id = mappings.project_id
              AND banks.source_store_id = mappings.source_store_id
              AND banks.owner_json = mappings.owner_json
              AND banks.bank_name = legacy_facts.category
             LEFT JOIN memory_fact_entities AS relations
               ON relations.fact_id = legacy_facts.fact_id
             WHERE mappings.owner_kind = ?1
               AND mappings.project_id = ?2
               AND mappings.owner_json = ?3
               AND mappings.source_store_id = ?4
               AND (
                    ?5 IS NULL
                    OR legacy_facts.content LIKE ?5 ESCAPE '\\'
                    OR legacy_facts.tags LIKE ?5 ESCAPE '\\'
               )
             GROUP BY mappings.fact_id, legacy_facts.hrr_vector, banks.bank_name
             ORDER BY legacy_facts.trust_score DESC,
                      legacy_facts.updated_at DESC,
                      mappings.fact_id ASC
             LIMIT ?6",
            params![
                key.kind,
                key.project_id.as_str(),
                key.json.as_str(),
                source_store_id.as_str(),
                search,
                limit,
            ],
        )
        .await
        .map_err(|error| storage_error(COMPATIBILITY_READ_OPERATION, error))?;
    let mut points = Vec::with_capacity(query.limit());
    while let Some(row) = rows
        .next()
        .await
        .map_err(|error| storage_error(COMPATIBILITY_READ_OPERATION, error))?
    {
        let fact_id = FactId::new(row_string(&row, 0, COMPATIBILITY_READ_OPERATION)?)
            .map_err(FactStoreError::from)?;
        let Some(fact) =
            load_compatibility_projection_tx(transaction, query.owner(), &fact_id).await?
        else {
            return Err(storage_message(
                COMPATIBILITY_READ_OPERATION,
                "owner-bound dashboard vector mapping has no canonical fact projection",
            )
            .into());
        };
        let vector =
            match row.get_value(1) {
                Ok(libsql::Value::Blob(bytes)) => HolographicEncoder::deserialize(&bytes)
                    .ok()
                    .filter(|vector| {
                        !vector.is_empty()
                            && vector.len() <= 16_384
                            && vector.iter().all(|value| value.is_finite())
                    }),
                Ok(libsql::Value::Null | _) | Err(_) => None,
            };
        let vector = matches!(&fact, CompatibilityFactProjectionV1::Available(_))
            .then_some(vector)
            .flatten();
        let bank_name = row_optional_string(&row, 2, COMPATIBILITY_READ_OPERATION)?;
        points.push(CompatibilityDashboardVectorPointV1::new(
            tracedecay_store::CompatibilityDashboardFactSummaryV1 {
                has_hrr_vector: vector.is_some(),
                fact,
            },
            vector,
            bank_name,
            nonnegative_u64(
                row_i64(&row, 3, COMPATIBILITY_READ_OPERATION)?,
                "dashboard vector entity count",
            )?,
            nonnegative_u64(
                row_i64(&row, 4, COMPATIBILITY_READ_OPERATION)?,
                "dashboard vector connection count",
            )?,
        )?);
    }
    Ok(points)
}

fn dashboard_compatibility_oplog_operation(value: &str) -> String {
    match value {
        "add" | "update" | "remove" | "feedback" | "reject_secret_like" | "curate_apply" => {
            value.to_owned()
        }
        _ => "legacy_mutation".to_owned(),
    }
}

fn dashboard_compatibility_oplog_details(
    raw: Option<String>,
) -> tracedecay_store::CompatibilityDashboardOplogDetailsV1 {
    match raw {
        Some(raw) if serde_json::from_str::<Value>(&raw).is_ok() => {
            tracedecay_store::CompatibilityDashboardOplogDetailsV1::Redacted
        }
        Some(_) | None => tracedecay_store::CompatibilityDashboardOplogDetailsV1::Unknown,
    }
}

async fn dashboard_compatibility_memory_oplog_tx(
    transaction: &Transaction,
    query: &CompatibilityDashboardOplogQueryV1,
) -> FactCompatibilityResult<Vec<CompatibilityDashboardOplogEntryV1>> {
    let key = OwnerKey::new(query.owner())?;
    let source_store_id = compatibility_source_store_id()?;
    let limit = i64::try_from(query.limit()).map_err(|_| FactStoreError::InvalidQueryLimit {
        limit: query.limit(),
        max: usize::MAX,
    })?;
    let mut rows = transaction
        .query(
            "SELECT oplog.id, oplog.ts, oplog.op, oplog.fact_id, oplog.detail_json
             FROM memory_oplog AS oplog
             JOIN memory_v2_legacy_map AS mappings
               ON mappings.legacy_fact_id = oplog.fact_id
             WHERE mappings.owner_kind = ?1
               AND mappings.project_id = ?2
               AND mappings.owner_json = ?3
               AND mappings.source_store_id = ?4
             ORDER BY oplog.id DESC
             LIMIT ?5",
            params![
                key.kind,
                key.project_id.as_str(),
                key.json.as_str(),
                source_store_id.as_str(),
                limit,
            ],
        )
        .await
        .map_err(|error| storage_error(COMPATIBILITY_READ_OPERATION, error))?;
    let mut entries = Vec::with_capacity(query.limit());
    while let Some(row) = rows
        .next()
        .await
        .map_err(|error| storage_error(COMPATIBILITY_READ_OPERATION, error))?
    {
        let legacy_fact_id = row_i64(&row, 3, COMPATIBILITY_READ_OPERATION)?;
        entries.push(CompatibilityDashboardOplogEntryV1::new(
            row_i64(&row, 0, COMPATIBILITY_READ_OPERATION)?,
            UtcMicros(row_i64(&row, 1, COMPATIBILITY_READ_OPERATION)?),
            dashboard_compatibility_oplog_operation(&row_string(
                &row,
                2,
                COMPATIBILITY_READ_OPERATION,
            )?),
            Some(CompatibilityFactTargetV1::Legacy(LegacyFactQuery::new(
                query.owner().clone(),
                source_store_id.clone(),
                legacy_fact_id,
            )?)),
            dashboard_compatibility_oplog_details(row_optional_string(
                &row,
                4,
                COMPATIBILITY_READ_OPERATION,
            )?),
        )?);
    }
    Ok(entries)
}

#[derive(Clone)]
struct OwnerKey {
    kind: &'static str,
    project_id: String,
    json: String,
}

impl OwnerKey {
    fn new(owner: &FactOwnerV1) -> FactStoreResult<Self> {
        let (kind, project_id) = match owner {
            FactOwnerV1::Profile => ("profile", String::new()),
            FactOwnerV1::Project { project_id } => ("project", project_id.as_str().to_owned()),
        };
        Ok(Self {
            kind,
            project_id,
            json: to_json(owner, "serialize fact owner")?,
        })
    }
}

const COMPATIBILITY_PROPOSAL_PAGE_LIMIT: usize = 1_000;

fn compatibility_proposal_state_label(state: CompatibilityFactProposalStateV1) -> &'static str {
    match state {
        CompatibilityFactProposalStateV1::PendingApproval => "pending",
        CompatibilityFactProposalStateV1::Applying => "applying",
        CompatibilityFactProposalStateV1::Applied => "applied",
        CompatibilityFactProposalStateV1::Rejected => "rejected",
        CompatibilityFactProposalStateV1::Quarantined => "quarantined",
    }
}

fn compatibility_proposal_state(value: &str) -> FactStoreResult<CompatibilityFactProposalStateV1> {
    match value {
        "pending" => Ok(CompatibilityFactProposalStateV1::PendingApproval),
        "applying" => Ok(CompatibilityFactProposalStateV1::Applying),
        "applied" => Ok(CompatibilityFactProposalStateV1::Applied),
        "rejected" => Ok(CompatibilityFactProposalStateV1::Rejected),
        "quarantined" => Ok(CompatibilityFactProposalStateV1::Quarantined),
        _ => Err(storage_message(
            COMPATIBILITY_READ_OPERATION,
            format!("unknown compatibility proposal state {value:?}"),
        )),
    }
}

fn compatibility_proposal_category(value: &str) -> FactStoreResult<FactCategoryV1> {
    match value {
        "general" => Ok(FactCategoryV1::General),
        "user_pref" => Ok(FactCategoryV1::UserPref),
        "project" => Ok(FactCategoryV1::Project),
        "tool" => Ok(FactCategoryV1::Tool),
        "decision" => Ok(FactCategoryV1::Decision),
        "code_area" => Ok(FactCategoryV1::CodeArea),
        _ => Err(storage_message(
            COMPATIBILITY_READ_OPERATION,
            format!("unknown compatibility proposal category {value:?}"),
        )),
    }
}

fn compatibility_proposal_required_string(
    object: &serde_json::Map<String, Value>,
    field: &'static str,
) -> FactStoreResult<String> {
    object
        .get(field)
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
        .ok_or_else(|| {
            storage_message(
                COMPATIBILITY_READ_OPERATION,
                format!("compatibility proposal {field} is missing or malformed"),
            )
        })
}

fn compatibility_proposal_optional_string(
    object: &serde_json::Map<String, Value>,
    field: &'static str,
) -> FactStoreResult<Option<String>> {
    match object.get(field) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(value)) => Ok(Some(value.clone())),
        Some(_) => Err(storage_message(
            COMPATIBILITY_READ_OPERATION,
            format!("compatibility proposal {field} is malformed"),
        )),
    }
}

fn compatibility_proposal_request_value(request: &CompatibilityFactAddCommandV1) -> Value {
    json!({
        "owner": request.owner(),
        "operation_id": request.operation_id().as_str(),
        "content": request.content(),
        "category": compatibility_category_label(request.category()),
        "source": request.source(),
        "tags": request.tags(),
        "entities": request.entities(),
        "metadata": compatibility_payload_metadata(request.metadata()),
        "automation_run_id": request.automation_run_id(),
        "default_trust": request.default_trust().as_f64(),
        "actor": request.actor().map(ActorId::as_str),
    })
}

fn compatibility_proposal_request_from_value(
    owner: &FactOwnerV1,
    value: Value,
) -> FactStoreResult<CompatibilityFactAddCommandV1> {
    let object = value.as_object().ok_or_else(|| {
        storage_message(
            COMPATIBILITY_READ_OPERATION,
            "compatibility proposal request is not an object",
        )
    })?;
    let stored_owner = from_json::<FactOwnerV1>(
        &to_json(
            object.get("owner").ok_or_else(|| {
                storage_message(
                    COMPATIBILITY_READ_OPERATION,
                    "compatibility proposal request owner is missing",
                )
            })?,
            "serialize compatibility proposal request owner",
        )?,
        COMPATIBILITY_READ_OPERATION,
    )?;
    if &stored_owner != owner {
        return Err(FactStoreError::OwnerMismatch);
    }
    let operation_id = ProvenanceId::new(compatibility_proposal_required_string(
        object,
        "operation_id",
    )?)
    .map_err(FactStoreError::from)?;
    let content = compatibility_proposal_required_string(object, "content")?;
    let category = compatibility_proposal_category(&compatibility_proposal_required_string(
        object, "category",
    )?)?;
    let source = compatibility_proposal_optional_string(object, "source")?;
    let tags = compatibility_value_strings(
        object.get("tags").ok_or_else(|| {
            storage_message(
                COMPATIBILITY_READ_OPERATION,
                "compatibility proposal request tags are missing",
            )
        })?,
        "proposal tags",
    )?;
    let entities = compatibility_value_strings(
        object.get("entities").ok_or_else(|| {
            storage_message(
                COMPATIBILITY_READ_OPERATION,
                "compatibility proposal request entities are missing",
            )
        })?,
        "proposal entities",
    )?;
    let metadata =
        compatibility_payload_metadata(&object.get("metadata").cloned().ok_or_else(|| {
            storage_message(
                COMPATIBILITY_READ_OPERATION,
                "compatibility proposal request metadata is missing",
            )
        })?);
    let automation_run_id = compatibility_proposal_optional_string(object, "automation_run_id")?;
    let trust = Confidence::new(
        object
            .get("default_trust")
            .and_then(Value::as_f64)
            .ok_or_else(|| {
                storage_message(
                    COMPATIBILITY_READ_OPERATION,
                    "compatibility proposal request default trust is missing",
                )
            })?,
    )
    .map_err(FactStoreError::from)?;
    let actor = compatibility_proposal_optional_string(object, "actor")?
        .map(ActorId::new)
        .transpose()
        .map_err(FactStoreError::from)?;
    let request = CompatibilityFactAddCommandV1::new(
        owner.clone(),
        operation_id,
        content,
        category,
        source,
        tags,
        entities,
        metadata,
        trust,
        actor,
    )?;
    match automation_run_id {
        Some(run_id) => request.with_automation_run_id(run_id),
        None => Ok(request),
    }
}

fn compatibility_proposal_action_id(
    kind: &'static str,
    material: Value,
) -> FactStoreResult<ProvenanceId> {
    let digest = compatibility_digest(material)?;
    ProvenanceId::new(format!("compatibility-{kind}:{digest}")).map_err(FactStoreError::from)
}

#[allow(clippy::too_many_arguments)]
fn compatibility_proposal_transition_json(
    proposal_id: &ProvenanceId,
    previous_state: Option<&str>,
    current_state: &str,
    reviewer: Option<&ActorId>,
    reason: Option<&str>,
    request_digest: &str,
    promoted_fact_id: Option<&FactId>,
    promoted_event_id: Option<&FactEventId>,
) -> FactStoreResult<String> {
    to_json(
        &json!({
            "proposal_id": proposal_id.as_str(),
            "previous_state": previous_state,
            "current_state": current_state,
            "reviewer": reviewer.map(ActorId::as_str),
            "reason": reason,
            "request_digest": request_digest,
            "promoted_fact_id": promoted_fact_id.map(FactId::as_str),
            "promoted_event_id": promoted_event_id.map(FactEventId::as_str),
        }),
        "serialize compatibility proposal transition",
    )
}

async fn compatibility_proposal_record_tx(
    transaction: &Transaction,
    owner: &FactOwnerV1,
    proposal_id: &ProvenanceId,
) -> FactCompatibilityResult<Option<CompatibilityFactProposalRecordV1>> {
    let key = OwnerKey::new(owner)?;
    let mut rows = transaction
        .query(
            "SELECT proposals.proposal_id, proposals.owner_json, proposals.request_json,
                    current_state.state, current_state.revision,
                    transition.reviewer_json, transition.validation_json,
                    transition.promoted_fact_id
             FROM memory_v2_proposals AS proposals
             JOIN memory_v2_proposal_current AS current_state
               ON current_state.proposal_id = proposals.proposal_id
              AND current_state.owner_kind = proposals.owner_kind
              AND current_state.project_id = proposals.project_id
             JOIN memory_v2_proposal_transitions AS transition
               ON transition.transition_id = current_state.last_transition_id
              AND transition.proposal_id = current_state.proposal_id
              AND transition.owner_kind = current_state.owner_kind
              AND transition.project_id = current_state.project_id
             WHERE proposals.proposal_id = ?1
               AND proposals.owner_kind = ?2
               AND proposals.project_id = ?3",
            params![proposal_id.as_str(), key.kind, key.project_id.as_str()],
        )
        .await
        .map_err(|error| storage_error(COMPATIBILITY_READ_OPERATION, error))?;
    let Some(row) = rows
        .next()
        .await
        .map_err(|error| storage_error(COMPATIBILITY_READ_OPERATION, error))?
    else {
        return Ok(None);
    };
    let stored_id = ProvenanceId::new(row_string(&row, 0, COMPATIBILITY_READ_OPERATION)?)
        .map_err(FactStoreError::from)?;
    if &stored_id != proposal_id {
        return Err(storage_message(
            COMPATIBILITY_READ_OPERATION,
            "compatibility proposal identity mismatch",
        )
        .into());
    }
    if row_string(&row, 1, COMPATIBILITY_READ_OPERATION)? != key.json {
        return Err(FactStoreError::OwnerMismatch.into());
    }
    let request = compatibility_proposal_request_from_value(
        owner,
        from_json::<Value>(
            &row_string(&row, 2, COMPATIBILITY_READ_OPERATION)?,
            COMPATIBILITY_READ_OPERATION,
        )?,
    )?;
    let state = compatibility_proposal_state(&row_string(&row, 3, COMPATIBILITY_READ_OPERATION)?)?;
    let revision = CompatibilityFactProposalRevisionV1::new(
        u64::try_from(row_i64(&row, 4, COMPATIBILITY_READ_OPERATION)?).map_err(|_| {
            storage_message(
                COMPATIBILITY_READ_OPERATION,
                "compatibility proposal revision is negative",
            )
        })?,
    )?;
    let reviewer = row_optional_string(&row, 5, COMPATIBILITY_READ_OPERATION)?
        .map(|value| from_json::<ActorId>(&value, COMPATIBILITY_READ_OPERATION))
        .transpose()?;
    let reason = row_optional_string(&row, 6, COMPATIBILITY_READ_OPERATION)?
        .map(|value| from_json::<Value>(&value, COMPATIBILITY_READ_OPERATION))
        .transpose()?
        .and_then(|value| {
            value
                .get("reason")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned)
        });
    let applied_fact_id = row_optional_string(&row, 7, COMPATIBILITY_READ_OPERATION)?
        .map(FactId::new)
        .transpose()
        .map_err(FactStoreError::from)?;
    let applied_mapping = match (&state, &applied_fact_id) {
        (CompatibilityFactProposalStateV1::Applied, Some(fact_id)) => Some(
            compatibility_legacy_mapping_tx(transaction, owner, fact_id)
                .await?
                .ok_or_else(|| {
                    storage_message(
                        COMPATIBILITY_READ_OPERATION,
                        "applied compatibility proposal is missing its fixed legacy mapping",
                    )
                })?,
        ),
        (CompatibilityFactProposalStateV1::Applied, None) => {
            return Err(storage_message(
                COMPATIBILITY_READ_OPERATION,
                "applied compatibility proposal is missing its promoted fact",
            )
            .into());
        }
        (_, Some(_)) => {
            return Err(storage_message(
                COMPATIBILITY_READ_OPERATION,
                "non-applied compatibility proposal has a promoted fact",
            )
            .into());
        }
        (_, None) => None,
    };
    let mapping = match (applied_mapping, applied_fact_id.as_ref()) {
        (Some(mapping), Some(fact_id)) => Some(CompatibilityFactMappingV1::new(
            CompatibilityFactIdV1::new(owner.clone(), fact_id.clone())?,
            Some(mapping),
        )?),
        (None, None) => None,
        _ => {
            return Err(storage_message(
                COMPATIBILITY_READ_OPERATION,
                "compatibility proposal mapping and fact identity disagree",
            )
            .into());
        }
    };
    CompatibilityFactProposalRecordV1::new(
        stored_id,
        owner.clone(),
        revision,
        state,
        request,
        applied_fact_id,
        mapping,
        reviewer,
        reason,
    )
    .map(Some)
    .map_err(Into::into)
}

async fn get_compatibility_fact_proposal_tx(
    transaction: &Transaction,
    owner: &FactOwnerV1,
    proposal_id: &ProvenanceId,
) -> FactCompatibilityResult<Option<CompatibilityFactProposalRecordV1>> {
    compatibility_proposal_record_tx(transaction, owner, proposal_id).await
}

async fn list_compatibility_fact_proposals_tx(
    transaction: &Transaction,
    owner: &FactOwnerV1,
    state: Option<CompatibilityFactProposalStateV1>,
    after_proposal_id: Option<&ProvenanceId>,
    limit: usize,
) -> FactCompatibilityResult<CompatibilityFactProposalPageV1> {
    if limit == 0 || limit > COMPATIBILITY_PROPOSAL_PAGE_LIMIT {
        return Err(FactStoreError::InvalidQueryLimit {
            limit,
            max: COMPATIBILITY_PROPOSAL_PAGE_LIMIT,
        }
        .into());
    }
    let key = OwnerKey::new(owner)?;
    let fetch_limit =
        i64::try_from(limit.saturating_add(1)).map_err(|_| FactStoreError::InvalidQueryLimit {
            limit,
            max: COMPATIBILITY_PROPOSAL_PAGE_LIMIT,
        })?;
    let state_label = state.map(compatibility_proposal_state_label);
    let mut rows = match (state_label, after_proposal_id) {
        (Some(state), Some(after)) => {
            transaction
                .query(
                    "SELECT current_state.proposal_id
                 FROM memory_v2_proposal_current AS current_state
                 JOIN memory_v2_proposals AS proposals
                   ON proposals.proposal_id = current_state.proposal_id
                  AND proposals.owner_kind = current_state.owner_kind
                  AND proposals.project_id = current_state.project_id
                 WHERE current_state.owner_kind = ?1 AND current_state.project_id = ?2
                   AND proposals.owner_json = ?3 AND current_state.state = ?4
                   AND current_state.proposal_id > ?5
                 ORDER BY current_state.proposal_id ASC LIMIT ?6",
                    params![
                        key.kind,
                        key.project_id.as_str(),
                        key.json.as_str(),
                        state,
                        after.as_str(),
                        fetch_limit
                    ],
                )
                .await
        }
        (Some(state), None) => {
            transaction
                .query(
                    "SELECT current_state.proposal_id
                 FROM memory_v2_proposal_current AS current_state
                 JOIN memory_v2_proposals AS proposals
                   ON proposals.proposal_id = current_state.proposal_id
                  AND proposals.owner_kind = current_state.owner_kind
                  AND proposals.project_id = current_state.project_id
                 WHERE current_state.owner_kind = ?1 AND current_state.project_id = ?2
                   AND proposals.owner_json = ?3 AND current_state.state = ?4
                 ORDER BY current_state.proposal_id ASC LIMIT ?5",
                    params![
                        key.kind,
                        key.project_id.as_str(),
                        key.json.as_str(),
                        state,
                        fetch_limit
                    ],
                )
                .await
        }
        (None, Some(after)) => {
            transaction
                .query(
                    "SELECT current_state.proposal_id
                 FROM memory_v2_proposal_current AS current_state
                 JOIN memory_v2_proposals AS proposals
                   ON proposals.proposal_id = current_state.proposal_id
                  AND proposals.owner_kind = current_state.owner_kind
                  AND proposals.project_id = current_state.project_id
                 WHERE current_state.owner_kind = ?1 AND current_state.project_id = ?2
                   AND proposals.owner_json = ?3 AND current_state.proposal_id > ?4
                 ORDER BY current_state.proposal_id ASC LIMIT ?5",
                    params![
                        key.kind,
                        key.project_id.as_str(),
                        key.json.as_str(),
                        after.as_str(),
                        fetch_limit
                    ],
                )
                .await
        }
        (None, None) => {
            transaction
                .query(
                    "SELECT current_state.proposal_id
                 FROM memory_v2_proposal_current AS current_state
                 JOIN memory_v2_proposals AS proposals
                   ON proposals.proposal_id = current_state.proposal_id
                  AND proposals.owner_kind = current_state.owner_kind
                  AND proposals.project_id = current_state.project_id
                 WHERE current_state.owner_kind = ?1 AND current_state.project_id = ?2
                   AND proposals.owner_json = ?3
                 ORDER BY current_state.proposal_id ASC LIMIT ?4",
                    params![
                        key.kind,
                        key.project_id.as_str(),
                        key.json.as_str(),
                        fetch_limit
                    ],
                )
                .await
        }
    }
    .map_err(|error| storage_error(COMPATIBILITY_READ_OPERATION, error))?;
    let mut ids = Vec::with_capacity(limit.saturating_add(1));
    while let Some(row) = rows
        .next()
        .await
        .map_err(|error| storage_error(COMPATIBILITY_READ_OPERATION, error))?
    {
        ids.push(
            ProvenanceId::new(row_string(&row, 0, COMPATIBILITY_READ_OPERATION)?)
                .map_err(FactStoreError::from)?,
        );
    }
    drop(rows);
    let has_more = ids.len() > limit;
    ids.truncate(limit);
    let mut proposals = Vec::with_capacity(ids.len());
    for proposal_id in &ids {
        proposals.push(
            compatibility_proposal_record_tx(transaction, owner, proposal_id)
                .await?
                .ok_or_else(|| {
                    storage_message(
                        COMPATIBILITY_READ_OPERATION,
                        "compatibility proposal disappeared from its read snapshot",
                    )
                })?,
        );
    }
    CompatibilityFactProposalPageV1::new(
        owner.clone(),
        proposals,
        has_more.then(|| ids.last().cloned()).flatten(),
    )
    .map_err(Into::into)
}

async fn count_pending_compatibility_fact_proposals_tx(
    transaction: &Transaction,
    owner: &FactOwnerV1,
) -> FactCompatibilityResult<u64> {
    let key = OwnerKey::new(owner)?;
    let mut rows = transaction
        .query(
            "SELECT COUNT(*)
             FROM memory_v2_proposal_current AS current_state
             JOIN memory_v2_proposals AS proposals
               ON proposals.proposal_id = current_state.proposal_id
              AND proposals.owner_kind = current_state.owner_kind
              AND proposals.project_id = current_state.project_id
             WHERE current_state.owner_kind = ?1 AND current_state.project_id = ?2
               AND proposals.owner_json = ?3 AND current_state.state = 'pending'",
            params![key.kind, key.project_id.as_str(), key.json.as_str()],
        )
        .await
        .map_err(|error| storage_error(COMPATIBILITY_READ_OPERATION, error))?;
    let row = rows
        .next()
        .await
        .map_err(|error| storage_error(COMPATIBILITY_READ_OPERATION, error))?
        .ok_or_else(|| {
            storage_message(
                COMPATIBILITY_READ_OPERATION,
                "compatibility proposal count returned no row",
            )
        })?;
    nonnegative_u64(
        row_i64(&row, 0, COMPATIBILITY_READ_OPERATION)?,
        "pending proposal count",
    )
    .map_err(Into::into)
}

fn compatibility_proposal_request_digest(
    request: &CompatibilityFactAddCommandV1,
) -> FactStoreResult<String> {
    compatibility_digest(json!({
        "owner": request.owner(),
        "content": request.content(),
        "category": compatibility_category_label(request.category()),
        "source": request.source(),
        "tags": request.tags(),
        "entities": request.entities(),
        "metadata": compatibility_payload_metadata(request.metadata()),
        "automation_run_id": request.automation_run_id(),
        "default_trust": request.default_trust().as_f64(),
        "actor": request.actor().map(ActorId::as_str),
    }))
}

async fn compatibility_proposal_digest_tx(
    transaction: &Transaction,
    owner: &FactOwnerV1,
    proposal_id: &ProvenanceId,
) -> FactStoreResult<Option<String>> {
    let key = OwnerKey::new(owner)?;
    let mut rows = transaction
        .query(
            "SELECT owner_json, request_digest FROM memory_v2_proposals
             WHERE proposal_id = ?1 AND owner_kind = ?2 AND project_id = ?3",
            params![proposal_id.as_str(), key.kind, key.project_id.as_str()],
        )
        .await
        .map_err(|error| storage_error(COMPATIBILITY_WRITE_OPERATION, error))?;
    let Some(row) = rows
        .next()
        .await
        .map_err(|error| storage_error(COMPATIBILITY_WRITE_OPERATION, error))?
    else {
        return Ok(None);
    };
    if row_string(&row, 0, COMPATIBILITY_WRITE_OPERATION)? != key.json {
        return Err(FactStoreError::OwnerMismatch);
    }
    Ok(Some(row_string(&row, 1, COMPATIBILITY_WRITE_OPERATION)?))
}

async fn compatibility_proposal_for_digest_tx(
    transaction: &Transaction,
    owner: &FactOwnerV1,
    request_digest: &str,
) -> FactStoreResult<Option<ProvenanceId>> {
    let key = OwnerKey::new(owner)?;
    let mut rows = transaction
        .query(
            "SELECT proposal_id, owner_json FROM memory_v2_proposals
             WHERE owner_kind = ?1 AND project_id = ?2 AND request_digest = ?3",
            params![key.kind, key.project_id.as_str(), request_digest],
        )
        .await
        .map_err(|error| storage_error(COMPATIBILITY_WRITE_OPERATION, error))?;
    let Some(row) = rows
        .next()
        .await
        .map_err(|error| storage_error(COMPATIBILITY_WRITE_OPERATION, error))?
    else {
        return Ok(None);
    };
    if row_string(&row, 1, COMPATIBILITY_WRITE_OPERATION)? != key.json {
        return Err(FactStoreError::OwnerMismatch);
    }
    ProvenanceId::new(row_string(&row, 0, COMPATIBILITY_WRITE_OPERATION)?)
        .map(Some)
        .map_err(FactStoreError::from)
}

fn compatibility_proposal_receipt_proposal_id(
    receipt: &CompatibilityOperationReceiptV1,
) -> FactStoreResult<ProvenanceId> {
    let proposal_id = receipt
        .receipt
        .get("proposal_id")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            storage_message(
                COMPATIBILITY_WRITE_OPERATION,
                "compatibility proposal receipt is missing its proposal identity",
            )
        })?;
    ProvenanceId::new(proposal_id.to_owned()).map_err(FactStoreError::from)
}

async fn compatibility_replay_proposal_tx(
    transaction: &Transaction,
    owner: &FactOwnerV1,
    receipt: &CompatibilityOperationReceiptV1,
) -> FactCompatibilityResult<CompatibilityFactProposalRecordV1> {
    let proposal_id = compatibility_proposal_receipt_proposal_id(receipt)?;
    compatibility_proposal_record_tx(transaction, owner, &proposal_id)
        .await?
        .ok_or_else(|| {
            storage_message(
                COMPATIBILITY_WRITE_OPERATION,
                "compatibility proposal replay target is missing",
            )
            .into()
        })
}

#[allow(clippy::too_many_arguments)]
async fn compatibility_insert_proposal_tx(
    transaction: &Transaction,
    proposal_id: &ProvenanceId,
    request: &CompatibilityFactAddCommandV1,
    idempotency_key: &ProvenanceId,
    request_digest: &str,
    evidence: &Value,
    state: CompatibilityFactProposalStateV1,
    reviewer: Option<&ActorId>,
    reason: Option<&str>,
    origin: &'static str,
    occurred_at: UtcMicros,
) -> FactStoreResult<()> {
    let key = OwnerKey::new(request.owner())?;
    let state_label = compatibility_proposal_state_label(state);
    if matches!(
        state,
        CompatibilityFactProposalStateV1::Applying | CompatibilityFactProposalStateV1::Applied
    ) {
        return Err(storage_message(
            COMPATIBILITY_WRITE_OPERATION,
            "compatibility proposal initial state is not durable in V22",
        ));
    }
    let transition_json = compatibility_proposal_transition_json(
        proposal_id,
        None,
        state_label,
        reviewer,
        reason,
        request_digest,
        None,
        None,
    )?;
    let transition_id = proposal_transition_id(&transition_json);
    let reviewer_json = reviewer
        .map(|value| to_json(value, "serialize compatibility proposal reviewer"))
        .transpose()?;
    let validation_json = reason
        .map(|value| {
            to_json(
                &json!({ "reason": value }),
                "serialize compatibility proposal validation",
            )
        })
        .transpose()?;
    transaction
        .execute(
            "INSERT INTO memory_v2_proposals(
                proposal_id, owner_kind, project_id, owner_json, idempotency_key,
                request_digest, request_json, evidence_json, submitted_at
             ) VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                proposal_id.as_str(),
                key.kind,
                key.project_id.as_str(),
                key.json.as_str(),
                idempotency_key.as_str(),
                request_digest,
                to_json(
                    &compatibility_proposal_request_value(request),
                    "serialize compatibility proposal request",
                )?,
                to_json(evidence, "serialize compatibility proposal evidence")?,
                occurred_at.0,
            ],
        )
        .await
        .map_err(|error| storage_error(COMPATIBILITY_WRITE_OPERATION, error))?;
    transaction
        .execute(
            "INSERT INTO memory_v2_proposal_transitions(
                transition_id, proposal_id, owner_kind, project_id, previous_state,
                current_state, reviewer_json, validation_json, origin,
                promoted_fact_id, promoted_assertion_id, promoted_event_id,
                transition_json, occurred_at
             ) VALUES(?1, ?2, ?3, ?4, NULL, ?5, ?6, ?7, ?8,
                      NULL, NULL, NULL, ?9, ?10)",
            params![
                transition_id.as_str(),
                proposal_id.as_str(),
                key.kind,
                key.project_id.as_str(),
                state_label,
                reviewer_json,
                validation_json,
                origin,
                transition_json,
                occurred_at.0,
            ],
        )
        .await
        .map_err(|error| storage_error(COMPATIBILITY_WRITE_OPERATION, error))?;
    transaction
        .execute(
            "INSERT INTO memory_v2_proposal_current(
                proposal_id, owner_kind, project_id, state, revision,
                last_transition_id, updated_at
             ) VALUES(?1, ?2, ?3, ?4, 1, ?5, ?6)",
            params![
                proposal_id.as_str(),
                key.kind,
                key.project_id.as_str(),
                state_label,
                transition_id.as_str(),
                occurred_at.0,
            ],
        )
        .await
        .map_err(|error| storage_error(COMPATIBILITY_WRITE_OPERATION, error))?;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn compatibility_advance_proposal_tx(
    transaction: &Transaction,
    owner: &FactOwnerV1,
    proposal_id: &ProvenanceId,
    expected_state: CompatibilityFactProposalStateV1,
    expected_revision: CompatibilityFactProposalRevisionV1,
    state: CompatibilityFactProposalStateV1,
    reviewer: Option<&ActorId>,
    reason: Option<&str>,
    request_digest: &str,
    promoted_fact_id: Option<&FactId>,
    promoted_assertion_id: Option<&FactAssertionId>,
    promoted_event_id: Option<&FactEventId>,
    occurred_at: UtcMicros,
) -> FactStoreResult<()> {
    let key = OwnerKey::new(owner)?;
    let expected_label = compatibility_proposal_state_label(expected_state);
    let state_label = compatibility_proposal_state_label(state);
    let applied = state == CompatibilityFactProposalStateV1::Applied;
    if applied != (promoted_fact_id.is_some() && promoted_event_id.is_some())
        || (!applied
            && (promoted_fact_id.is_some()
                || promoted_assertion_id.is_some()
                || promoted_event_id.is_some()))
    {
        return Err(storage_message(
            COMPATIBILITY_WRITE_OPERATION,
            "compatibility proposal transition has inconsistent promoted identities",
        ));
    }
    let transition_json = compatibility_proposal_transition_json(
        proposal_id,
        Some(expected_label),
        state_label,
        reviewer,
        reason,
        request_digest,
        promoted_fact_id,
        promoted_event_id,
    )?;
    let transition_id = proposal_transition_id(&transition_json);
    let reviewer_json = reviewer
        .map(|value| to_json(value, "serialize compatibility proposal reviewer"))
        .transpose()?;
    let validation_json = reason
        .map(|value| {
            to_json(
                &json!({ "reason": value }),
                "serialize compatibility proposal validation",
            )
        })
        .transpose()?;
    transaction
        .execute(
            "INSERT INTO memory_v2_proposal_transitions(
                transition_id, proposal_id, owner_kind, project_id, previous_state,
                current_state, reviewer_json, validation_json, origin,
                promoted_fact_id, promoted_assertion_id, promoted_event_id,
                transition_json, occurred_at
             ) VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, 'runtime',
                      ?9, ?10, ?11, ?12, ?13)",
            params![
                transition_id.as_str(),
                proposal_id.as_str(),
                key.kind,
                key.project_id.as_str(),
                expected_label,
                state_label,
                reviewer_json,
                validation_json,
                promoted_fact_id.map(FactId::as_str),
                promoted_assertion_id.map(FactAssertionId::as_str),
                promoted_event_id.map(FactEventId::as_str),
                transition_json,
                occurred_at.0,
            ],
        )
        .await
        .map_err(|error| storage_error(COMPATIBILITY_WRITE_OPERATION, error))?;
    let changed = transaction
        .execute(
            "UPDATE memory_v2_proposal_current
             SET state = ?1, revision = revision + 1,
                 last_transition_id = ?2, updated_at = ?3
             WHERE proposal_id = ?4 AND owner_kind = ?5 AND project_id = ?6
               AND state = ?7 AND revision = ?8",
            params![
                state_label,
                transition_id.as_str(),
                occurred_at.0,
                proposal_id.as_str(),
                key.kind,
                key.project_id.as_str(),
                expected_label,
                i64::try_from(expected_revision.get()).map_err(|_| {
                    storage_message(
                        COMPATIBILITY_WRITE_OPERATION,
                        "compatibility proposal revision exceeds storage range",
                    )
                })?,
            ],
        )
        .await
        .map_err(|error| storage_error(COMPATIBILITY_WRITE_OPERATION, error))?;
    if changed != 1 {
        return Err(storage_message(
            COMPATIBILITY_WRITE_OPERATION,
            "compatibility proposal revision or state changed before transition",
        ));
    }
    Ok(())
}

async fn submit_compatibility_fact_proposal_tx(
    transaction: &Transaction,
    proposal_id: ProvenanceId,
    request: &CompatibilityFactAddCommandV1,
    submitter: Option<&ActorId>,
) -> FactCompatibilityResult<CompatibilityFactProposalRecordV1> {
    let request_digest = compatibility_proposal_request_digest(request)?;
    if let Some(receipt) = compatibility_lookup_operation_receipt_tx(
        transaction,
        request.owner(),
        request.operation_id(),
        "proposal_submit",
        &request_digest,
    )
    .await?
    {
        return compatibility_replay_proposal_tx(transaction, request.owner(), &receipt).await;
    }
    if let Some(existing_digest) =
        compatibility_proposal_digest_tx(transaction, request.owner(), &proposal_id).await?
    {
        if existing_digest != request_digest {
            return Err(storage_message(
                COMPATIBILITY_WRITE_OPERATION,
                "compatibility proposal id was reused with a different request",
            )
            .into());
        }
        let proposal = compatibility_proposal_record_tx(transaction, request.owner(), &proposal_id)
            .await?
            .ok_or_else(|| {
                storage_message(
                    COMPATIBILITY_WRITE_OPERATION,
                    "compatibility proposal record is missing after identity lookup",
                )
            })?;
        let receipt = json!({
            "proposal_id": proposal.proposal_id().as_str(),
            "state": compatibility_proposal_state_label(proposal.state()),
        });
        compatibility_record_operation_receipt_tx(
            transaction,
            request.owner(),
            request.operation_id(),
            "proposal_submit",
            &request_digest,
            proposal.applied_fact_id(),
            None,
            &receipt,
            compatibility_now()?,
        )
        .await?;
        return Ok(proposal);
    }
    if let Some(existing_id) =
        compatibility_proposal_for_digest_tx(transaction, request.owner(), &request_digest).await?
    {
        let proposal = compatibility_proposal_record_tx(transaction, request.owner(), &existing_id)
            .await?
            .ok_or_else(|| {
                storage_message(
                    COMPATIBILITY_WRITE_OPERATION,
                    "compatibility proposal record is missing after digest lookup",
                )
            })?;
        let receipt = json!({
            "proposal_id": proposal.proposal_id().as_str(),
            "state": compatibility_proposal_state_label(proposal.state()),
        });
        compatibility_record_operation_receipt_tx(
            transaction,
            request.owner(),
            request.operation_id(),
            "proposal_submit",
            &request_digest,
            proposal.applied_fact_id(),
            None,
            &receipt,
            compatibility_now()?,
        )
        .await?;
        return Ok(proposal);
    }
    let now = compatibility_now()?;
    compatibility_insert_proposal_tx(
        transaction,
        &proposal_id,
        request,
        request.operation_id(),
        &request_digest,
        &json!({ "kind": "compatibility-proposal-v1" }),
        CompatibilityFactProposalStateV1::PendingApproval,
        submitter,
        None,
        "runtime",
        now,
    )
    .await?;
    let receipt = json!({ "proposal_id": proposal_id.as_str(), "state": "pending" });
    compatibility_record_operation_receipt_tx(
        transaction,
        request.owner(),
        request.operation_id(),
        "proposal_submit",
        &request_digest,
        None,
        None,
        &receipt,
        now,
    )
    .await?;
    compatibility_replay_proposal_tx(
        transaction,
        request.owner(),
        &CompatibilityOperationReceiptV1 {
            fact_id: None,
            event_id: None,
            receipt,
        },
    )
    .await
}

async fn reject_compatibility_fact_proposal_tx(
    transaction: &Transaction,
    owner: &FactOwnerV1,
    proposal_id: &ProvenanceId,
    expected_revision: CompatibilityFactProposalRevisionV1,
    reviewer: &ActorId,
    reason: &str,
) -> FactCompatibilityResult<CompatibilityFactProposalRecordV1> {
    if reason.trim().is_empty() || reason.len() > 4_096 {
        return Err(
            FactStoreError::Contract(tracedecay_domain::DomainError::NonCanonical {
                field: "compatibility fact proposal reason",
            })
            .into(),
        );
    }
    let material = json!({
        "proposal_id": proposal_id.as_str(),
        "expected_revision": expected_revision.get(),
        "reviewer": reviewer.as_str(),
        "reason": reason,
    });
    let request_digest = compatibility_digest(material.clone())?;
    let operation_id = compatibility_proposal_action_id("proposal-reject", material)?;
    if let Some(receipt) = compatibility_lookup_operation_receipt_tx(
        transaction,
        owner,
        &operation_id,
        "proposal_reject",
        &request_digest,
    )
    .await?
    {
        return compatibility_replay_proposal_tx(transaction, owner, &receipt).await;
    }
    let proposal = compatibility_proposal_record_tx(transaction, owner, proposal_id)
        .await?
        .ok_or_else(|| {
            storage_message(
                COMPATIBILITY_WRITE_OPERATION,
                "compatibility proposal is missing",
            )
        })?;
    if proposal.state() != CompatibilityFactProposalStateV1::PendingApproval
        || proposal.revision() != expected_revision
    {
        return Err(storage_message(
            COMPATIBILITY_WRITE_OPERATION,
            "compatibility proposal revision or state changed before rejection",
        )
        .into());
    }
    let now = compatibility_now()?;
    compatibility_advance_proposal_tx(
        transaction,
        owner,
        proposal_id,
        CompatibilityFactProposalStateV1::PendingApproval,
        expected_revision,
        CompatibilityFactProposalStateV1::Rejected,
        Some(reviewer),
        Some(reason),
        &request_digest,
        None,
        None,
        None,
        now,
    )
    .await?;
    let receipt = json!({
        "proposal_id": proposal_id.as_str(),
        "state": "rejected",
        "revision": expected_revision.get().saturating_add(1),
    });
    compatibility_record_operation_receipt_tx(
        transaction,
        owner,
        &operation_id,
        "proposal_reject",
        &request_digest,
        None,
        None,
        &receipt,
        now,
    )
    .await?;
    compatibility_replay_proposal_tx(
        transaction,
        owner,
        &CompatibilityOperationReceiptV1 {
            fact_id: None,
            event_id: None,
            receipt,
        },
    )
    .await
}

async fn compatibility_legacy_proposal_mapping_tx(
    transaction: &Transaction,
    owner: &FactOwnerV1,
    source_store_id: &SourceStoreId,
    legacy_proposal_id: i64,
) -> FactStoreResult<Option<(ProvenanceId, Value)>> {
    let key = OwnerKey::new(owner)?;
    let mut rows = transaction
        .query(
            "SELECT mappings.proposal_id, proposals.owner_json, mappings.import_receipt_json
             FROM memory_v2_legacy_proposal_map AS mappings
             JOIN memory_v2_proposals AS proposals
               ON proposals.proposal_id = mappings.proposal_id
              AND proposals.owner_kind = mappings.owner_kind
              AND proposals.project_id = mappings.project_id
             WHERE mappings.owner_kind = ?1 AND mappings.project_id = ?2
               AND mappings.source_store_id = ?3 AND mappings.legacy_proposal_id = ?4",
            params![
                key.kind,
                key.project_id.as_str(),
                source_store_id.as_str(),
                legacy_proposal_id.to_string(),
            ],
        )
        .await
        .map_err(|error| storage_error(COMPATIBILITY_WRITE_OPERATION, error))?;
    let Some(row) = rows
        .next()
        .await
        .map_err(|error| storage_error(COMPATIBILITY_WRITE_OPERATION, error))?
    else {
        return Ok(None);
    };
    if row_string(&row, 1, COMPATIBILITY_WRITE_OPERATION)? != key.json {
        return Err(FactStoreError::OwnerMismatch);
    }
    let proposal_id = ProvenanceId::new(row_string(&row, 0, COMPATIBILITY_WRITE_OPERATION)?)
        .map_err(FactStoreError::from)?;
    let import_receipt = from_json::<Value>(
        &row_string(&row, 2, COMPATIBILITY_WRITE_OPERATION)?,
        COMPATIBILITY_WRITE_OPERATION,
    )?;
    Ok(Some((proposal_id, import_receipt)))
}

fn compatibility_import_receipt_from_value(
    request: &CompatibilityFactProposalImportV1,
    receipt: &Value,
) -> FactStoreResult<CompatibilityFactProposalImportReceiptV1> {
    let imported_count = receipt
        .get("imported_count")
        .and_then(Value::as_u64)
        .and_then(|value| usize::try_from(value).ok())
        .ok_or_else(|| {
            storage_message(
                COMPATIBILITY_WRITE_OPERATION,
                "compatibility proposal import receipt is malformed",
            )
        })?;
    let quarantined_count = receipt
        .get("quarantined_count")
        .and_then(Value::as_u64)
        .and_then(|value| usize::try_from(value).ok())
        .ok_or_else(|| {
            storage_message(
                COMPATIBILITY_WRITE_OPERATION,
                "compatibility proposal import receipt is malformed",
            )
        })?;
    CompatibilityFactProposalImportReceiptV1::new(
        request.owner().clone(),
        request.source_store_id().clone(),
        request.sidecar_digest().clone(),
        imported_count,
        quarantined_count,
    )
}

fn compatibility_import_initial_state(
    state: CompatibilityFactProposalStateV1,
) -> (CompatibilityFactProposalStateV1, Option<&'static str>) {
    match state {
        CompatibilityFactProposalStateV1::PendingApproval => {
            (CompatibilityFactProposalStateV1::PendingApproval, None)
        }
        CompatibilityFactProposalStateV1::Applying => (
            CompatibilityFactProposalStateV1::PendingApproval,
            Some("legacy applying state normalized to pending"),
        ),
        CompatibilityFactProposalStateV1::Rejected => {
            (CompatibilityFactProposalStateV1::Rejected, None)
        }
        CompatibilityFactProposalStateV1::Quarantined => (
            CompatibilityFactProposalStateV1::Quarantined,
            Some("legacy proposal was quarantined"),
        ),
        CompatibilityFactProposalStateV1::Applied => (
            CompatibilityFactProposalStateV1::Quarantined,
            Some("legacy applied proposal lacks a verifiable canonical promotion"),
        ),
    }
}

async fn import_legacy_compatibility_fact_proposals_tx(
    transaction: &Transaction,
    request: &CompatibilityFactProposalImportV1,
) -> FactCompatibilityResult<CompatibilityFactProposalImportReceiptV1> {
    let fixed_source_store_id = compatibility_source_store_id()?;
    if request.source_store_id() != &fixed_source_store_id {
        return Err(storage_message(
            COMPATIBILITY_WRITE_OPERATION,
            "compatibility proposal imports require the fixed legacy-memory-v1 source store",
        )
        .into());
    }
    let records = request
        .records()
        .iter()
        .map(|record| {
            Ok::<_, FactStoreError>(json!({
                "legacy_proposal_id": record.legacy_proposal_id(),
                "state": compatibility_proposal_state_label(record.state()),
                "request_digest": compatibility_proposal_request_digest(record.request())?,
            }))
        })
        .collect::<FactStoreResult<Vec<_>>>()?;
    let material = json!({
        "owner": request.owner(),
        "source_store_id": request.source_store_id().as_str(),
        "sidecar_digest": request.sidecar_digest().as_str(),
        "records": records,
    });
    let request_digest = compatibility_digest(material.clone())?;
    let operation_id = compatibility_proposal_action_id("proposal-import", material)?;
    if let Some(receipt) = compatibility_lookup_operation_receipt_tx(
        transaction,
        request.owner(),
        &operation_id,
        "proposal_import",
        &request_digest,
    )
    .await?
    {
        return compatibility_import_receipt_from_value(request, &receipt.receipt)
            .map_err(Into::into);
    }
    let now = compatibility_now()?;
    let mut imported_count = 0_usize;
    let mut quarantined_count = 0_usize;
    for record in request.records() {
        let legacy_proposal_id = record.legacy_proposal_id();
        let record_digest = compatibility_proposal_request_digest(record.request())?;
        let (state, reason) = compatibility_import_initial_state(record.state());
        let proposal_id = compatibility_proposal_action_id(
            "legacy-proposal",
            json!({
                "source_store_id": request.source_store_id().as_str(),
                "legacy_proposal_id": legacy_proposal_id,
            }),
        )?;
        let resolved_id = match compatibility_legacy_proposal_mapping_tx(
            transaction,
            request.owner(),
            request.source_store_id(),
            legacy_proposal_id,
        )
        .await?
        {
            Some((existing_id, import_receipt)) => {
                if import_receipt.get("sidecar_digest").and_then(Value::as_str)
                    != Some(request.sidecar_digest().as_str())
                {
                    return Err(storage_message(
                        COMPATIBILITY_WRITE_OPERATION,
                        "legacy proposal id was reused with a different sidecar digest",
                    )
                    .into());
                }
                let stored_digest =
                    compatibility_proposal_digest_tx(transaction, request.owner(), &existing_id)
                        .await?
                        .ok_or_else(|| {
                            storage_message(
                                COMPATIBILITY_WRITE_OPERATION,
                                "legacy proposal map references a missing proposal",
                            )
                        })?;
                if stored_digest != record_digest {
                    return Err(storage_message(
                        COMPATIBILITY_WRITE_OPERATION,
                        "legacy proposal id was reused with a different request",
                    )
                    .into());
                }
                existing_id
            }
            None => {
                if let Some(existing_id) = compatibility_proposal_for_digest_tx(
                    transaction,
                    request.owner(),
                    &record_digest,
                )
                .await?
                {
                    return Err(storage_message(
                        COMPATIBILITY_WRITE_OPERATION,
                        format!(
                            "legacy proposal request is already bound to proposal {}",
                            existing_id.as_str()
                        ),
                    )
                    .into());
                }
                compatibility_insert_proposal_tx(
                    transaction,
                    &proposal_id,
                    record.request(),
                    &proposal_id,
                    &record_digest,
                    &json!({
                        "source_store_id": request.source_store_id().as_str(),
                        "sidecar_digest": request.sidecar_digest().as_str(),
                        "legacy_proposal_id": legacy_proposal_id,
                    }),
                    state,
                    None,
                    reason,
                    "legacy_import",
                    now,
                )
                .await?;
                transaction
                    .execute(
                        "INSERT INTO memory_v2_legacy_proposal_map(
                            owner_kind, project_id, source_store_id, legacy_proposal_id,
                            proposal_id, history_coverage, import_receipt_json, imported_at
                         ) VALUES(?1, ?2, ?3, ?4, ?5, 'unknown', ?6, ?7)",
                        params![
                            OwnerKey::new(request.owner())?.kind,
                            OwnerKey::new(request.owner())?.project_id.as_str(),
                            request.source_store_id().as_str(),
                            legacy_proposal_id.to_string(),
                            proposal_id.as_str(),
                            to_json(
                                &json!({
                                    "source_store_id": request.source_store_id().as_str(),
                                    "sidecar_digest": request.sidecar_digest().as_str(),
                                    "request_digest": record_digest,
                                }),
                                "serialize compatibility legacy proposal import receipt",
                            )?,
                            now.0,
                        ],
                    )
                    .await
                    .map_err(|error| storage_error(COMPATIBILITY_WRITE_OPERATION, error))?;
                proposal_id
            }
        };
        let proposal = compatibility_proposal_record_tx(transaction, request.owner(), &resolved_id)
            .await?
            .ok_or_else(|| {
                storage_message(
                    COMPATIBILITY_WRITE_OPERATION,
                    "compatibility proposal is missing after legacy import",
                )
            })?;
        if proposal.state() == CompatibilityFactProposalStateV1::Quarantined {
            quarantined_count = quarantined_count.saturating_add(1);
        } else {
            imported_count = imported_count.saturating_add(1);
        }
    }
    let receipt = json!({
        "imported_count": imported_count,
        "quarantined_count": quarantined_count,
    });
    compatibility_record_operation_receipt_tx(
        transaction,
        request.owner(),
        &operation_id,
        "proposal_import",
        &request_digest,
        None,
        None,
        &receipt,
        now,
    )
    .await?;
    CompatibilityFactProposalImportReceiptV1::new(
        request.owner().clone(),
        request.source_store_id().clone(),
        request.sidecar_digest().clone(),
        imported_count,
        quarantined_count,
    )
    .map_err(Into::into)
}

async fn promote_compatibility_fact_proposal_tx(
    db: &Database,
    transaction: &Transaction,
    request: &CompatibilityFactProposalPromotionV1,
) -> FactCompatibilityResult<CompatibilityFactProposalRecordV1> {
    let result =
        promote_compatibility_fact_proposal_with_disposition_tx(db, transaction, request).await?;
    Ok(result.proposal().clone())
}

async fn promote_compatibility_fact_proposal_with_disposition_tx(
    db: &Database,
    transaction: &Transaction,
    request: &CompatibilityFactProposalPromotionV1,
) -> FactCompatibilityResult<CompatibilityFactProposalPromotionResultV1> {
    let material = json!({
        "proposal_id": request.proposal_id().as_str(),
        "expected_revision": request.expected_revision().get(),
        "reviewer": request.reviewer().map(ActorId::as_str),
    });
    let request_digest = compatibility_digest(material.clone())?;
    let operation_id = compatibility_proposal_action_id("proposal-promote", material)?;
    if let Some(receipt) = compatibility_lookup_operation_receipt_tx(
        transaction,
        request.owner(),
        &operation_id,
        "proposal_promote",
        &request_digest,
    )
    .await?
    {
        let proposal =
            compatibility_replay_proposal_tx(transaction, request.owner(), &receipt).await?;
        let disposition = match proposal.state() {
            CompatibilityFactProposalStateV1::Applied => {
                CompatibilityFactProposalPromotionDispositionV1::AlreadyPromoted
            }
            CompatibilityFactProposalStateV1::Quarantined => {
                CompatibilityFactProposalPromotionDispositionV1::Quarantined
            }
            _ => {
                return Err(storage_message(
                    COMPATIBILITY_WRITE_OPERATION,
                    "compatibility promotion receipt does not resolve to a terminal proposal",
                )
                .into());
            }
        };
        return CompatibilityFactProposalPromotionResultV1::new(proposal, disposition)
            .map_err(Into::into);
    }
    let proposal =
        compatibility_proposal_record_tx(transaction, request.owner(), request.proposal_id())
            .await?
            .ok_or_else(|| {
                storage_message(
                    COMPATIBILITY_WRITE_OPERATION,
                    "compatibility proposal is missing",
                )
            })?;
    if proposal.state() != CompatibilityFactProposalStateV1::PendingApproval
        || proposal.revision() != request.expected_revision()
    {
        return Err(storage_message(
            COMPATIBILITY_WRITE_OPERATION,
            "compatibility proposal revision or state changed before promotion",
        )
        .into());
    }
    let now = compatibility_now()?;
    let payload_metadata = compatibility_payload_metadata(proposal.request().metadata());
    let sanitized = compatibility_sanitize_payload(
        proposal.request().content(),
        proposal.request().category(),
        proposal.request().tags(),
        proposal.request().entities(),
        &payload_metadata,
    )?;
    let Some(sanitized) = sanitized else {
        let reason = "content rejected by privacy sanitizer";
        compatibility_advance_proposal_tx(
            transaction,
            request.owner(),
            request.proposal_id(),
            CompatibilityFactProposalStateV1::PendingApproval,
            request.expected_revision(),
            CompatibilityFactProposalStateV1::Quarantined,
            request.reviewer(),
            Some(reason),
            &request_digest,
            None,
            None,
            None,
            now,
        )
        .await?;
        let receipt = json!({
            "proposal_id": request.proposal_id().as_str(),
            "state": "quarantined",
            "revision": request.expected_revision().get().saturating_add(1),
        });
        compatibility_record_operation_receipt_tx(
            transaction,
            request.owner(),
            &operation_id,
            "proposal_promote",
            &request_digest,
            None,
            None,
            &receipt,
            now,
        )
        .await?;
        let quarantined = compatibility_replay_proposal_tx(
            transaction,
            request.owner(),
            &CompatibilityOperationReceiptV1 {
                fact_id: None,
                event_id: None,
                receipt,
            },
        )
        .await?;
        return CompatibilityFactProposalPromotionResultV1::new(
            quarantined,
            CompatibilityFactProposalPromotionDispositionV1::Quarantined,
        )
        .map_err(Into::into);
    };
    let source = compatibility_source_label(proposal.request().source())?;
    let (fact_id, assertion_id, event_id) = match compatibility_mirror_insert_tx(
        db,
        transaction,
        request.owner(),
        &sanitized.payload,
        &source,
        proposal.request().default_trust(),
        now,
    )
    .await?
    {
        CompatibilityMirrorInsertV1::Existing { fact_id, .. } => {
            let key = OwnerKey::new(request.owner())?;
            let fact = load_current_fact_tx(transaction, &key, request.owner(), &fact_id)
                .await?
                .ok_or_else(|| {
                    storage_message(
                        COMPATIBILITY_WRITE_OPERATION,
                        "existing compatibility mirror has no canonical current fact",
                    )
                })?;
            (
                fact_id,
                fact.active_assertion_id().clone(),
                fact.last_event_id().clone(),
            )
        }
        CompatibilityMirrorInsertV1::Inserted(legacy_fact_id) => {
            let (identity, mapping) =
                compatibility_legacy_mapping_for_new_fact(request.owner(), legacy_fact_id, now)?;
            let batch = compatibility_initial_batch(
                request.owner(),
                identity,
                mapping.clone(),
                sanitized.payload,
                sanitized.access,
                proposal.request().default_trust(),
                proposal.request().actor().cloned(),
                now,
            )?;
            let (receipt, _) = compatibility_commit_batch_tx(transaction, &batch).await?;
            let assertion_id = receipt.active_assertion_id().cloned().ok_or_else(|| {
                storage_message(
                    COMPATIBILITY_WRITE_OPERATION,
                    "promoted compatibility fact has no active assertion",
                )
            })?;
            (
                mapping.fact_id().clone(),
                assertion_id,
                receipt.last_event_id().clone(),
            )
        }
    };
    compatibility_advance_proposal_tx(
        transaction,
        request.owner(),
        request.proposal_id(),
        CompatibilityFactProposalStateV1::PendingApproval,
        request.expected_revision(),
        CompatibilityFactProposalStateV1::Applied,
        request.reviewer(),
        None,
        &request_digest,
        Some(&fact_id),
        Some(&assertion_id),
        Some(&event_id),
        now,
    )
    .await?;
    let receipt = json!({
        "proposal_id": request.proposal_id().as_str(),
        "state": "applied",
        "revision": request.expected_revision().get().saturating_add(1),
    });
    compatibility_record_operation_receipt_tx(
        transaction,
        request.owner(),
        &operation_id,
        "proposal_promote",
        &request_digest,
        Some(&fact_id),
        Some(&event_id),
        &receipt,
        now,
    )
    .await?;
    let promoted = compatibility_replay_proposal_tx(
        transaction,
        request.owner(),
        &CompatibilityOperationReceiptV1 {
            fact_id: Some(fact_id),
            event_id: Some(event_id),
            receipt,
        },
    )
    .await?;
    CompatibilityFactProposalPromotionResultV1::new(
        promoted,
        CompatibilityFactProposalPromotionDispositionV1::NewlyPromoted,
    )
    .map_err(Into::into)
}

/// The immutable assertion record deliberately excludes `FactPayloadV1`.
/// Payload bytes belong only in `memory_v2_assertion_payloads`, which is the
/// storage locus erased when an access transition reaches `Deleted`.
#[derive(Serialize)]
struct StoredAssertionHeaderV1<'a> {
    assertion_id: &'a FactAssertionId,
    fact_id: &'a FactId,
    owner: &'a FactOwnerV1,
    kind: &'a FactAssertionKindV1,
    payload_reference: &'a tracedecay_domain::PayloadReferenceV1,
    evidence: &'a [tracedecay_domain::FactEvidenceRefV1],
    asserted_at: UtcMicros,
    actor_id: Option<&'a tracedecay_domain::ActorId>,
}

fn assertion_header_json(assertion: &FactAssertionV1) -> FactStoreResult<String> {
    let payload_reference = assertion.payload().payload_reference()?;
    to_json(
        &StoredAssertionHeaderV1 {
            assertion_id: assertion.assertion_id(),
            fact_id: assertion.fact_id(),
            owner: assertion.owner(),
            kind: assertion.kind(),
            payload_reference: &payload_reference,
            evidence: assertion.evidence(),
            asserted_at: assertion.asserted_at(),
            actor_id: assertion.actor_id(),
        },
        "serialize payload-free fact assertion header",
    )
}

async fn commit_fact_tx(
    transaction: &Transaction,
    batch: &FactWriteBatch,
) -> FactStoreResult<CommitAttempt> {
    let owner = OwnerKey::new(batch.owner())?;
    let actual_last = current_last_event(transaction, &owner, batch.fact_id()).await?;
    if batch_is_exact_replay(transaction, &owner, batch, actual_last.as_ref()).await? {
        return Ok(CommitAttempt {
            outcome: receipt_outcome(transaction, &owner, batch, true).await?,
            wrote: false,
        });
    }
    if let Some(conflict) = batch_identity_collision(transaction, &owner, batch).await? {
        return Ok(CommitAttempt {
            outcome: FactCommitOutcome::Conflict(conflict),
            wrote: false,
        });
    }
    if actual_last.as_ref() != batch.expected_last_event_id() {
        return Ok(CommitAttempt {
            outcome: FactCommitOutcome::Conflict(FactCommitConflict::LastEventMismatch {
                expected: batch.expected_last_event_id().cloned(),
                actual: actual_last,
            }),
            wrote: false,
        });
    }
    ensure_append_order(transaction, &owner, batch, actual_last.as_ref()).await?;

    ensure_fact_identity(transaction, &owner, batch).await?;
    ensure_referenced_anchors(transaction, &owner, batch).await?;
    for anchor in batch.new_anchors() {
        insert_or_verify_anchor(transaction, &owner, anchor).await?;
    }
    if let Some(assertion) = batch.assertion() {
        insert_assertion(transaction, &owner, assertion).await?;
    }
    if let Some(mapping) = batch.legacy_mapping() {
        insert_legacy_mapping(transaction, &owner, mapping).await?;
    }
    for event in batch.events() {
        ensure_event_references(transaction, &owner, event).await?;
    }
    for event in batch.events() {
        insert_event(transaction, &owner, event).await?;
    }
    publish_current_projection(transaction, &owner, batch).await?;

    Ok(CommitAttempt {
        outcome: receipt_outcome(transaction, &owner, batch, false).await?,
        wrote: true,
    })
}

async fn current_last_event(
    transaction: &Transaction,
    owner: &OwnerKey,
    fact_id: &FactId,
) -> FactStoreResult<Option<FactEventId>> {
    let mut rows = transaction
        .query(
            "SELECT last_event_id FROM memory_v2_current_facts
             WHERE fact_id = ?1 AND owner_kind = ?2 AND project_id = ?3",
            params![fact_id.as_str(), owner.kind, owner.project_id.as_str()],
        )
        .await
        .map_err(|error| storage_error(QUERY_OPERATION, error))?;
    let Some(row) = rows
        .next()
        .await
        .map_err(|error| storage_error(QUERY_OPERATION, error))?
    else {
        return Ok(None);
    };
    Ok(Some(FactEventId::new(row_string(
        &row,
        0,
        QUERY_OPERATION,
    )?)?))
}

async fn ensure_append_order(
    transaction: &Transaction,
    owner: &OwnerKey,
    batch: &FactWriteBatch,
    actual_last: Option<&FactEventId>,
) -> FactStoreResult<()> {
    let Some(last_event_id) = actual_last else {
        return Ok(());
    };
    let first = batch.events().first().ok_or(FactStoreError::EmptyBatch)?;
    let mut rows = transaction
        .query(
            "SELECT occurred_at, event_id FROM memory_v2_lineage_events
             WHERE event_id = ?1 AND fact_id = ?2
               AND owner_kind = ?3 AND project_id = ?4",
            params![
                last_event_id.as_str(),
                batch.fact_id().as_str(),
                owner.kind,
                owner.project_id.as_str(),
            ],
        )
        .await
        .map_err(|error| storage_error(COMMIT_OPERATION, error))?;
    let row = rows
        .next()
        .await
        .map_err(|error| storage_error(COMMIT_OPERATION, error))?
        .ok_or_else(|| storage_message(COMMIT_OPERATION, "current fact points at missing event"))?;
    let last = (
        UtcMicros(row_i64(&row, 0, COMMIT_OPERATION)?),
        FactEventId::new(row_string(&row, 1, COMMIT_OPERATION)?)?,
    );
    if (first.occurred_at(), first.event_id()) <= (last.0, &last.1) {
        return Err(FactStoreError::EventsOutOfOrder);
    }
    Ok(())
}

async fn batch_is_exact_replay(
    transaction: &Transaction,
    owner: &OwnerKey,
    batch: &FactWriteBatch,
    actual_last: Option<&FactEventId>,
) -> FactStoreResult<bool> {
    if actual_last != batch.events().last().map(FactLineageEventV1::event_id) {
        return Ok(false);
    }
    if !fact_identity_matches(transaction, owner, batch).await? {
        return Ok(false);
    }
    for anchor in batch.new_anchors() {
        if !anchor_matches(transaction, owner, anchor).await? {
            return Ok(false);
        }
    }
    if let Some(assertion) = batch.assertion()
        && !assertion_matches(transaction, owner, assertion).await?
    {
        return Ok(false);
    }
    if let Some(mapping) = batch.legacy_mapping()
        && !legacy_mapping_matches(transaction, owner, mapping).await?
    {
        return Ok(false);
    }
    for event in batch.events() {
        if !event_matches(transaction, owner, event).await? {
            return Ok(false);
        }
    }
    Ok(true)
}

async fn batch_identity_collision(
    transaction: &Transaction,
    owner: &OwnerKey,
    batch: &FactWriteBatch,
) -> FactStoreResult<Option<FactCommitConflict>> {
    if fact_exists(transaction, batch.fact_id()).await?
        && !fact_identity_matches(transaction, owner, batch).await?
    {
        return Ok(Some(collision("fact", batch.fact_id().as_str())));
    }
    for anchor in batch.new_anchors() {
        if anchor_exists(transaction, anchor.anchor_id()).await?
            && !anchor_matches(transaction, owner, anchor).await?
        {
            return Ok(Some(collision(
                "retrieval anchor",
                anchor.anchor_id().as_str(),
            )));
        }
    }
    if let Some(assertion) = batch.assertion()
        && assertion_exists(transaction, assertion.assertion_id()).await?
        && !assertion_matches(transaction, owner, assertion).await?
    {
        return Ok(Some(collision(
            "assertion",
            assertion.assertion_id().as_str(),
        )));
    }
    if let Some(mapping) = batch.legacy_mapping()
        && legacy_mapping_exists(transaction, owner, mapping).await?
        && !legacy_mapping_matches(transaction, owner, mapping).await?
    {
        return Ok(Some(collision(
            "legacy mapping",
            mapping.fact_id().as_str(),
        )));
    }
    for event in batch.events() {
        if event_exists(transaction, event.event_id()).await?
            && !event_matches(transaction, owner, event).await?
        {
            return Ok(Some(collision("event", event.event_id().as_str())));
        }
    }
    Ok(None)
}

fn collision(kind: &'static str, id: &str) -> FactCommitConflict {
    FactCommitConflict::IdentityCollision {
        kind,
        id: id.to_owned(),
    }
}

async fn fact_exists(transaction: &Transaction, fact_id: &FactId) -> FactStoreResult<bool> {
    row_exists(
        transaction,
        "SELECT 1 FROM memory_v2_facts WHERE fact_id = ?1",
        [fact_id.as_str()],
    )
    .await
}

async fn fact_identity_matches(
    transaction: &Transaction,
    owner: &OwnerKey,
    batch: &FactWriteBatch,
) -> FactStoreResult<bool> {
    let mut rows = transaction
        .query(
            "SELECT owner_kind, project_id, owner_json, identity_json
             FROM memory_v2_facts WHERE fact_id = ?1",
            [batch.fact_id().as_str()],
        )
        .await
        .map_err(|error| storage_error(QUERY_OPERATION, error))?;
    let Some(row) = rows
        .next()
        .await
        .map_err(|error| storage_error(QUERY_OPERATION, error))?
    else {
        return Ok(false);
    };
    let identity_matches = match batch.identity_material() {
        Some(identity) => {
            row_string(&row, 3, QUERY_OPERATION)? == to_json(identity, "serialize fact identity")?
        }
        None => true,
    };
    Ok(row_string(&row, 0, QUERY_OPERATION)? == owner.kind
        && row_string(&row, 1, QUERY_OPERATION)? == owner.project_id
        && row_string(&row, 2, QUERY_OPERATION)? == owner.json
        && identity_matches)
}

async fn ensure_referenced_anchors(
    transaction: &Transaction,
    owner: &OwnerKey,
    batch: &FactWriteBatch,
) -> FactStoreResult<()> {
    for anchor_id in batch.referenced_anchor_ids() {
        let mut rows = transaction
            .query(
                "SELECT 1 FROM retrieval_anchors
                 WHERE anchor_id = ?1 AND owner_json = ?2",
                params![anchor_id.as_str(), owner.json.as_str()],
            )
            .await
            .map_err(|error| storage_error(COMMIT_OPERATION, error))?;
        let Some(_row) = rows
            .next()
            .await
            .map_err(|error| storage_error(COMMIT_OPERATION, error))?
        else {
            return Err(FactStoreError::MissingEvidenceAnchor {
                anchor_id: anchor_id.clone(),
            });
        };
    }
    Ok(())
}

async fn insert_or_verify_anchor(
    transaction: &Transaction,
    owner: &OwnerKey,
    anchor: &RetrievalAnchorRecordV2,
) -> FactStoreResult<()> {
    if anchor_exists(transaction, anchor.anchor_id()).await? {
        if anchor_matches(transaction, owner, anchor).await? {
            return Ok(());
        }
        return Err(storage_message(
            COMMIT_OPERATION,
            "retrieval anchor identity collision",
        ));
    }
    transaction
        .execute(
            "INSERT INTO retrieval_anchors(
                anchor_id, anchor_json, owner_json, projection_generation
             ) VALUES(?1, ?2, ?3, ?4)",
            params![
                anchor.anchor_id().as_str(),
                to_json(anchor, "serialize retrieval anchor")?,
                owner.json.as_str(),
                anchor.projection_generation().as_str(),
            ],
        )
        .await
        .map_err(|error| storage_error(COMMIT_OPERATION, error))?;
    for alias in anchor.aliases() {
        transaction
            .execute(
                "INSERT INTO retrieval_anchor_aliases(
                    owner_json, alias_kind, locator_digest, anchor_id
                 ) VALUES(?1, ?2, ?3, ?4)",
                params![
                    owner.json.as_str(),
                    to_json(&alias.kind(), "serialize anchor alias kind")?,
                    to_json(alias.locator_digest(), "serialize anchor locator digest")?,
                    anchor.anchor_id().as_str(),
                ],
            )
            .await
            .map_err(|error| storage_error(COMMIT_OPERATION, error))?;
    }
    Ok(())
}

async fn anchor_exists(
    transaction: &Transaction,
    anchor_id: &RetrievalAnchorId,
) -> FactStoreResult<bool> {
    row_exists(
        transaction,
        "SELECT 1 FROM retrieval_anchors WHERE anchor_id = ?1",
        [anchor_id.as_str()],
    )
    .await
}

async fn anchor_matches(
    transaction: &Transaction,
    owner: &OwnerKey,
    anchor: &RetrievalAnchorRecordV2,
) -> FactStoreResult<bool> {
    let mut rows = transaction
        .query(
            "SELECT anchor_json, owner_json, projection_generation
             FROM retrieval_anchors WHERE anchor_id = ?1",
            [anchor.anchor_id().as_str()],
        )
        .await
        .map_err(|error| storage_error(QUERY_OPERATION, error))?;
    let Some(row) = rows
        .next()
        .await
        .map_err(|error| storage_error(QUERY_OPERATION, error))?
    else {
        return Ok(false);
    };
    if row_string(&row, 0, QUERY_OPERATION)? != to_json(anchor, "serialize retrieval anchor")?
        || row_string(&row, 1, QUERY_OPERATION)? != owner.json
        || row_string(&row, 2, QUERY_OPERATION)? != anchor.projection_generation().as_str()
    {
        return Ok(false);
    }
    let mut aliases = transaction
        .query(
            "SELECT alias_kind, locator_digest FROM retrieval_anchor_aliases
             WHERE anchor_id = ?1 ORDER BY alias_kind, locator_digest",
            [anchor.anchor_id().as_str()],
        )
        .await
        .map_err(|error| storage_error(QUERY_OPERATION, error))?;
    let mut stored = Vec::new();
    while let Some(row) = aliases
        .next()
        .await
        .map_err(|error| storage_error(QUERY_OPERATION, error))?
    {
        stored.push((
            row_string(&row, 0, QUERY_OPERATION)?,
            row_string(&row, 1, QUERY_OPERATION)?,
        ));
    }
    let mut expected = anchor
        .aliases()
        .iter()
        .map(|alias| {
            Ok((
                to_json(&alias.kind(), "serialize anchor alias kind")?,
                to_json(alias.locator_digest(), "serialize anchor locator digest")?,
            ))
        })
        .collect::<FactStoreResult<Vec<_>>>()?;
    expected.sort();
    Ok(stored == expected)
}

async fn insert_assertion(
    transaction: &Transaction,
    owner: &OwnerKey,
    assertion: &FactAssertionV1,
) -> FactStoreResult<()> {
    if assertion_exists(transaction, assertion.assertion_id()).await? {
        if assertion_matches(transaction, owner, assertion).await? {
            return Ok(());
        }
        return Err(storage_message(
            COMMIT_OPERATION,
            "assertion identity collision",
        ));
    }
    let header_json = assertion_header_json(assertion)?;
    let actor_id = assertion.actor_id().map(ToString::to_string);
    transaction
        .execute(
            "INSERT INTO memory_v2_assertions(
                assertion_id, fact_id, owner_kind, project_id, owner_json,
                assertion_header_json, kind_json, payload_reference_json,
                receipt_json, asserted_at, actor_id
             ) VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            params![
                assertion.assertion_id().as_str(),
                assertion.fact_id().as_str(),
                owner.kind,
                owner.project_id.as_str(),
                owner.json.as_str(),
                header_json,
                to_json(assertion.kind(), "serialize assertion kind")?,
                to_json(
                    &assertion.payload().payload_reference()?,
                    "serialize assertion payload reference",
                )?,
                to_json(assertion.payload().receipt(), "serialize assertion receipt")?,
                assertion.asserted_at().0,
                actor_id,
            ],
        )
        .await
        .map_err(|error| storage_error(COMMIT_OPERATION, error))?;

    for (ordinal, superseded) in superseded_assertions(assertion.kind()).iter().enumerate() {
        transaction
            .execute(
                "INSERT INTO memory_v2_assertion_supersession(
                    assertion_id, fact_id, owner_kind, project_id,
                    superseded_assertion_id, ordinal
                 ) VALUES(?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    assertion.assertion_id().as_str(),
                    assertion.fact_id().as_str(),
                    owner.kind,
                    owner.project_id.as_str(),
                    superseded.as_str(),
                    ordinal as i64,
                ],
            )
            .await
            .map_err(|error| storage_error(COMMIT_OPERATION, error))?;
    }

    transaction
        .execute(
            "INSERT INTO memory_v2_assertion_payloads(
                assertion_id, fact_id, owner_kind, project_id, payload_json, content
             ) VALUES(?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                assertion.assertion_id().as_str(),
                assertion.fact_id().as_str(),
                owner.kind,
                owner.project_id.as_str(),
                to_json(assertion.payload(), "serialize assertion payload")?,
                assertion.payload().content(),
            ],
        )
        .await
        .map_err(|error| storage_error(COMMIT_OPERATION, error))?;

    for (ordinal, evidence) in assertion.evidence().iter().enumerate() {
        let evidence_json = to_json(evidence, "serialize fact evidence")?;
        let changed = transaction
            .execute(
                "INSERT OR IGNORE INTO memory_v2_evidence(
                    evidence_id, fact_id, owner_kind, project_id,
                    owner_json, anchor_id, evidence_json
                 ) VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![
                    evidence.evidence_id().as_str(),
                    assertion.fact_id().as_str(),
                    owner.kind,
                    owner.project_id.as_str(),
                    owner.json.as_str(),
                    evidence.anchor_id().as_str(),
                    evidence_json.as_str(),
                ],
            )
            .await
            .map_err(|error| storage_error(COMMIT_OPERATION, error))?;
        if changed == 0 {
            let mut rows = transaction
                .query(
                    "SELECT evidence_json, owner_json, anchor_id
                     FROM memory_v2_evidence
                     WHERE evidence_id = ?1 AND fact_id = ?2
                       AND owner_kind = ?3 AND project_id = ?4",
                    params![
                        evidence.evidence_id().as_str(),
                        assertion.fact_id().as_str(),
                        owner.kind,
                        owner.project_id.as_str(),
                    ],
                )
                .await
                .map_err(|error| storage_error(COMMIT_OPERATION, error))?;
            let Some(row) = rows
                .next()
                .await
                .map_err(|error| storage_error(COMMIT_OPERATION, error))?
            else {
                return Err(storage_message(
                    COMMIT_OPERATION,
                    "evidence insert disappeared",
                ));
            };
            if row_string(&row, 0, COMMIT_OPERATION)? != evidence_json
                || row_string(&row, 1, COMMIT_OPERATION)? != owner.json
                || row_string(&row, 2, COMMIT_OPERATION)? != evidence.anchor_id().as_str()
            {
                return Err(storage_message(
                    COMMIT_OPERATION,
                    "evidence identity collision",
                ));
            }
        }
        transaction
            .execute(
                "INSERT INTO memory_v2_assertion_evidence(
                    assertion_id, evidence_id, fact_id, owner_kind, project_id, ordinal
                 ) VALUES(?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    assertion.assertion_id().as_str(),
                    evidence.evidence_id().as_str(),
                    assertion.fact_id().as_str(),
                    owner.kind,
                    owner.project_id.as_str(),
                    ordinal as i64,
                ],
            )
            .await
            .map_err(|error| storage_error(COMMIT_OPERATION, error))?;
    }
    Ok(())
}

fn superseded_assertions(kind: &FactAssertionKindV1) -> Vec<&FactAssertionId> {
    match kind {
        FactAssertionKindV1::Correction { supersedes } => vec![supersedes],
        FactAssertionKindV1::Merge { supersedes } => supersedes.iter().collect(),
        FactAssertionKindV1::Initial | FactAssertionKindV1::LegacyImport => Vec::new(),
    }
}

async fn assertion_exists(
    transaction: &Transaction,
    assertion_id: &FactAssertionId,
) -> FactStoreResult<bool> {
    row_exists(
        transaction,
        "SELECT 1 FROM memory_v2_assertions WHERE assertion_id = ?1",
        [assertion_id.as_str()],
    )
    .await
}

async fn assertion_matches(
    transaction: &Transaction,
    owner: &OwnerKey,
    assertion: &FactAssertionV1,
) -> FactStoreResult<bool> {
    let mut rows = transaction
        .query(
            "SELECT fact_id, owner_kind, project_id, owner_json,
                    assertion_header_json, kind_json, payload_reference_json,
                    receipt_json, asserted_at, actor_id
             FROM memory_v2_assertions WHERE assertion_id = ?1",
            [assertion.assertion_id().as_str()],
        )
        .await
        .map_err(|error| storage_error(QUERY_OPERATION, error))?;
    let Some(row) = rows
        .next()
        .await
        .map_err(|error| storage_error(QUERY_OPERATION, error))?
    else {
        return Ok(false);
    };
    let stored_actor = row_optional_string(&row, 9, QUERY_OPERATION)?;
    let expected_actor = assertion.actor_id().map(ToString::to_string);
    if row_string(&row, 0, QUERY_OPERATION)? != assertion.fact_id().as_str()
        || row_string(&row, 1, QUERY_OPERATION)? != owner.kind
        || row_string(&row, 2, QUERY_OPERATION)? != owner.project_id
        || row_string(&row, 3, QUERY_OPERATION)? != owner.json
        || row_string(&row, 4, QUERY_OPERATION)? != assertion_header_json(assertion)?
        || row_string(&row, 5, QUERY_OPERATION)?
            != to_json(assertion.kind(), "serialize assertion kind")?
        || row_string(&row, 6, QUERY_OPERATION)?
            != to_json(
                &assertion.payload().payload_reference()?,
                "serialize assertion payload reference",
            )?
        || row_string(&row, 7, QUERY_OPERATION)?
            != to_json(assertion.payload().receipt(), "serialize assertion receipt")?
        || row_i64(&row, 8, QUERY_OPERATION)? != assertion.asserted_at().0
        || stored_actor != expected_actor
    {
        return Ok(false);
    }

    let mut supersession = transaction
        .query(
            "SELECT superseded_assertion_id FROM memory_v2_assertion_supersession
             WHERE assertion_id = ?1 AND fact_id = ?2
               AND owner_kind = ?3 AND project_id = ?4 ORDER BY ordinal",
            params![
                assertion.assertion_id().as_str(),
                assertion.fact_id().as_str(),
                owner.kind,
                owner.project_id.as_str(),
            ],
        )
        .await
        .map_err(|error| storage_error(QUERY_OPERATION, error))?;
    let mut stored_supersession = Vec::new();
    while let Some(row) = supersession
        .next()
        .await
        .map_err(|error| storage_error(QUERY_OPERATION, error))?
    {
        stored_supersession.push(row_string(&row, 0, QUERY_OPERATION)?);
    }
    let expected_supersession = superseded_assertions(assertion.kind())
        .into_iter()
        .map(|id| id.as_str().to_owned())
        .collect::<Vec<_>>();
    if stored_supersession != expected_supersession {
        return Ok(false);
    }

    let mut payload = transaction
        .query(
            "SELECT payload_json, content FROM memory_v2_assertion_payloads
             WHERE assertion_id = ?1 AND fact_id = ?2
               AND owner_kind = ?3 AND project_id = ?4",
            params![
                assertion.assertion_id().as_str(),
                assertion.fact_id().as_str(),
                owner.kind,
                owner.project_id.as_str(),
            ],
        )
        .await
        .map_err(|error| storage_error(QUERY_OPERATION, error))?;
    let payload_row = payload
        .next()
        .await
        .map_err(|error| storage_error(QUERY_OPERATION, error))?;
    drop(payload);
    let payload_matches = match payload_row {
        Some(row) => {
            row_string(&row, 0, QUERY_OPERATION)?
                == to_json(assertion.payload(), "serialize assertion payload")?
                && row_string(&row, 1, QUERY_OPERATION)? == assertion.payload().content()
        }
        None => payload_is_purged_projection(transaction, owner, assertion.fact_id()).await?,
    };
    if !payload_matches {
        return Ok(false);
    }

    let mut evidence = transaction
        .query(
            "SELECT ae.evidence_id, e.evidence_json, e.owner_json, e.anchor_id
             FROM memory_v2_assertion_evidence ae
             JOIN memory_v2_evidence e ON
                e.evidence_id = ae.evidence_id AND e.fact_id = ae.fact_id AND
                e.owner_kind = ae.owner_kind AND e.project_id = ae.project_id
             WHERE ae.assertion_id = ?1 AND ae.fact_id = ?2
               AND ae.owner_kind = ?3 AND ae.project_id = ?4 ORDER BY ae.ordinal",
            params![
                assertion.assertion_id().as_str(),
                assertion.fact_id().as_str(),
                owner.kind,
                owner.project_id.as_str(),
            ],
        )
        .await
        .map_err(|error| storage_error(QUERY_OPERATION, error))?;
    let mut stored_evidence = Vec::new();
    while let Some(row) = evidence
        .next()
        .await
        .map_err(|error| storage_error(QUERY_OPERATION, error))?
    {
        stored_evidence.push((
            row_string(&row, 0, QUERY_OPERATION)?,
            row_string(&row, 1, QUERY_OPERATION)?,
            row_string(&row, 2, QUERY_OPERATION)?,
            row_string(&row, 3, QUERY_OPERATION)?,
        ));
    }
    let expected_evidence = assertion
        .evidence()
        .iter()
        .map(|evidence| {
            Ok((
                evidence.evidence_id().as_str().to_owned(),
                to_json(evidence, "serialize fact evidence")?,
                owner.json.clone(),
                evidence.anchor_id().as_str().to_owned(),
            ))
        })
        .collect::<FactStoreResult<Vec<_>>>()?;
    Ok(stored_evidence == expected_evidence)
}

async fn payload_is_purged_projection(
    transaction: &Transaction,
    owner: &OwnerKey,
    fact_id: &FactId,
) -> FactStoreResult<bool> {
    let mut rows = transaction
        .query(
            "SELECT current_facts.payload_access
             FROM memory_v2_current_facts AS current_facts
             JOIN memory_v2_facts AS facts
               ON facts.fact_id = current_facts.fact_id
              AND facts.owner_kind = current_facts.owner_kind
              AND facts.project_id = current_facts.project_id
             WHERE current_facts.fact_id = ?1
               AND current_facts.owner_kind = ?2
               AND current_facts.project_id = ?3
               AND facts.owner_json = ?4",
            params![
                fact_id.as_str(),
                owner.kind,
                owner.project_id.as_str(),
                owner.json.as_str(),
            ],
        )
        .await
        .map_err(|error| storage_error(QUERY_OPERATION, error))?;
    let Some(row) = rows
        .next()
        .await
        .map_err(|error| storage_error(QUERY_OPERATION, error))?
    else {
        return Ok(false);
    };
    Ok(matches!(
        parse_payload_access(&row_string(&row, 0, QUERY_OPERATION)?)?,
        PayloadAccessState::Quarantined | PayloadAccessState::Deleted
    ))
}

async fn insert_legacy_mapping(
    transaction: &Transaction,
    owner: &OwnerKey,
    mapping: &LegacyFactMappingV1,
) -> FactStoreResult<()> {
    if legacy_mapping_exists(transaction, owner, mapping).await? {
        if legacy_mapping_matches(transaction, owner, mapping).await? {
            return Ok(());
        }
        return Err(storage_message(
            COMMIT_OPERATION,
            "legacy mapping identity collision",
        ));
    }
    transaction
        .execute(
            "INSERT INTO memory_v2_legacy_map(
                owner_kind, project_id, owner_json, source_store_id,
                legacy_fact_id, fact_id, mapping_json
             ) VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                owner.kind,
                owner.project_id.as_str(),
                owner.json.as_str(),
                mapping.source_store_id().as_str(),
                mapping.legacy_fact_id(),
                mapping.fact_id().as_str(),
                to_json(mapping, "serialize legacy fact mapping")?,
            ],
        )
        .await
        .map_err(|error| storage_error(COMMIT_OPERATION, error))?;
    Ok(())
}

async fn legacy_mapping_exists(
    transaction: &Transaction,
    owner: &OwnerKey,
    mapping: &LegacyFactMappingV1,
) -> FactStoreResult<bool> {
    row_exists_params(
        transaction,
        "SELECT 1 FROM memory_v2_legacy_map
         WHERE owner_kind = ?1 AND project_id = ?2
           AND source_store_id = ?3 AND legacy_fact_id = ?4",
        params![
            owner.kind,
            owner.project_id.as_str(),
            mapping.source_store_id().as_str(),
            mapping.legacy_fact_id(),
        ],
    )
    .await
}

async fn legacy_mapping_matches(
    transaction: &Transaction,
    owner: &OwnerKey,
    mapping: &LegacyFactMappingV1,
) -> FactStoreResult<bool> {
    let mut rows = transaction
        .query(
            "SELECT owner_json, fact_id, mapping_json FROM memory_v2_legacy_map
             WHERE owner_kind = ?1 AND project_id = ?2
               AND source_store_id = ?3 AND legacy_fact_id = ?4",
            params![
                owner.kind,
                owner.project_id.as_str(),
                mapping.source_store_id().as_str(),
                mapping.legacy_fact_id(),
            ],
        )
        .await
        .map_err(|error| storage_error(QUERY_OPERATION, error))?;
    let Some(row) = rows
        .next()
        .await
        .map_err(|error| storage_error(QUERY_OPERATION, error))?
    else {
        return Ok(false);
    };
    Ok(row_string(&row, 0, QUERY_OPERATION)? == owner.json
        && row_string(&row, 1, QUERY_OPERATION)? == mapping.fact_id().as_str()
        && row_string(&row, 2, QUERY_OPERATION)?
            == to_json(mapping, "serialize legacy fact mapping")?)
}

async fn ensure_event_references(
    transaction: &Transaction,
    owner: &OwnerKey,
    event: &FactLineageEventV1,
) -> FactStoreResult<()> {
    match event.kind() {
        FactLineageEventKindV1::AssertionRecorded { assertion_id } => {
            if !owned_assertion_exists(transaction, owner, event.fact_id(), assertion_id).await? {
                return Err(storage_message(
                    COMMIT_OPERATION,
                    "lineage assertion reference is missing",
                ));
            }
        }
        FactLineageEventKindV1::TrustChanged { evidence_ids, .. } => {
            ensure_event_evidence(transaction, owner, event.fact_id(), evidence_ids).await?;
        }
        FactLineageEventKindV1::Curated {
            action,
            evidence_ids,
        } => {
            ensure_event_evidence(transaction, owner, event.fact_id(), evidence_ids).await?;
            if let FactCurationActionV1::ContradictedBy { fact_id }
            | FactCurationActionV1::SupersededBy { fact_id }
            | FactCurationActionV1::MergedInto { fact_id } = action
                && !owned_fact_exists(transaction, owner, fact_id).await?
            {
                return Err(storage_message(
                    COMMIT_OPERATION,
                    "lineage curation target is missing",
                ));
            }
        }
        FactLineageEventKindV1::PayloadAccessChanged { .. } => {}
        FactLineageEventKindV1::LegacyImported { mapping } => {
            if !legacy_mapping_matches(transaction, owner, mapping).await? {
                return Err(storage_message(
                    COMMIT_OPERATION,
                    "lineage legacy mapping reference is missing",
                ));
            }
        }
    }
    Ok(())
}

async fn ensure_event_evidence(
    transaction: &Transaction,
    owner: &OwnerKey,
    fact_id: &FactId,
    evidence_ids: &[FactEvidenceId],
) -> FactStoreResult<()> {
    for evidence_id in evidence_ids {
        if !owned_evidence_exists(transaction, owner, fact_id, evidence_id).await? {
            return Err(storage_message(
                COMMIT_OPERATION,
                "lineage evidence reference is missing",
            ));
        }
    }
    Ok(())
}

async fn owned_assertion_exists(
    transaction: &Transaction,
    owner: &OwnerKey,
    fact_id: &FactId,
    assertion_id: &FactAssertionId,
) -> FactStoreResult<bool> {
    row_exists_params(
        transaction,
        "SELECT 1 FROM memory_v2_assertions
         WHERE assertion_id = ?1 AND fact_id = ?2 AND owner_kind = ?3
           AND project_id = ?4 AND owner_json = ?5",
        params![
            assertion_id.as_str(),
            fact_id.as_str(),
            owner.kind,
            owner.project_id.as_str(),
            owner.json.as_str(),
        ],
    )
    .await
}

async fn owned_evidence_exists(
    transaction: &Transaction,
    owner: &OwnerKey,
    fact_id: &FactId,
    evidence_id: &FactEvidenceId,
) -> FactStoreResult<bool> {
    row_exists_params(
        transaction,
        "SELECT 1 FROM memory_v2_evidence
         WHERE evidence_id = ?1 AND fact_id = ?2 AND owner_kind = ?3
           AND project_id = ?4 AND owner_json = ?5",
        params![
            evidence_id.as_str(),
            fact_id.as_str(),
            owner.kind,
            owner.project_id.as_str(),
            owner.json.as_str(),
        ],
    )
    .await
}

async fn owned_fact_exists(
    transaction: &Transaction,
    owner: &OwnerKey,
    fact_id: &FactId,
) -> FactStoreResult<bool> {
    row_exists_params(
        transaction,
        "SELECT 1 FROM memory_v2_facts
         WHERE fact_id = ?1 AND owner_kind = ?2 AND project_id = ?3
           AND owner_json = ?4",
        params![
            fact_id.as_str(),
            owner.kind,
            owner.project_id.as_str(),
            owner.json.as_str(),
        ],
    )
    .await
}

async fn insert_event(
    transaction: &Transaction,
    owner: &OwnerKey,
    event: &FactLineageEventV1,
) -> FactStoreResult<()> {
    if event_exists(transaction, event.event_id()).await? {
        if event_matches(transaction, owner, event).await? {
            return Ok(());
        }
        return Err(storage_message(
            COMMIT_OPERATION,
            "lineage event identity collision",
        ));
    }
    transaction
        .execute(
            "INSERT INTO memory_v2_lineage_events(
                event_id, fact_id, owner_kind, project_id,
                event_json, occurred_at, recorded_at
             ) VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                event.event_id().as_str(),
                event.fact_id().as_str(),
                owner.kind,
                owner.project_id.as_str(),
                to_json(event, "serialize fact lineage event")?,
                event.occurred_at().0,
                event.occurred_at().0,
            ],
        )
        .await
        .map_err(|error| storage_error(COMMIT_OPERATION, error))?;
    Ok(())
}

async fn event_exists(transaction: &Transaction, event_id: &FactEventId) -> FactStoreResult<bool> {
    row_exists(
        transaction,
        "SELECT 1 FROM memory_v2_lineage_events WHERE event_id = ?1",
        [event_id.as_str()],
    )
    .await
}

async fn event_matches(
    transaction: &Transaction,
    owner: &OwnerKey,
    event: &FactLineageEventV1,
) -> FactStoreResult<bool> {
    let mut rows = transaction
        .query(
            "SELECT fact_id, owner_kind, project_id, event_json, occurred_at
             FROM memory_v2_lineage_events WHERE event_id = ?1",
            [event.event_id().as_str()],
        )
        .await
        .map_err(|error| storage_error(QUERY_OPERATION, error))?;
    let Some(row) = rows
        .next()
        .await
        .map_err(|error| storage_error(QUERY_OPERATION, error))?
    else {
        return Ok(false);
    };
    Ok(
        row_string(&row, 0, QUERY_OPERATION)? == event.fact_id().as_str()
            && row_string(&row, 1, QUERY_OPERATION)? == owner.kind
            && row_string(&row, 2, QUERY_OPERATION)? == owner.project_id
            && row_string(&row, 3, QUERY_OPERATION)?
                == to_json(event, "serialize fact lineage event")?
            && row_i64(&row, 4, QUERY_OPERATION)? == event.occurred_at().0,
    )
}

#[derive(Clone)]
struct Projection {
    access: PayloadAccessState,
    trust: Confidence,
    active_assertion_id: Option<FactAssertionId>,
    last_event_id: Option<FactEventId>,
    updated_at: UtcMicros,
}

impl Projection {
    fn empty() -> FactStoreResult<Self> {
        Ok(Self {
            access: PayloadAccessState::Eligible,
            trust: Confidence::new(DEFAULT_TRUST)?,
            active_assertion_id: None,
            last_event_id: None,
            updated_at: UtcMicros(0),
        })
    }

    fn apply(&mut self, event: &FactLineageEventV1) -> FactStoreResult<()> {
        match event.kind() {
            FactLineageEventKindV1::AssertionRecorded { assertion_id } => {
                self.active_assertion_id = Some(assertion_id.clone());
            }
            FactLineageEventKindV1::TrustChanged {
                previous, current, ..
            } => {
                if previous != &self.trust {
                    return Err(storage_message(
                        COMMIT_OPERATION,
                        "trust transition is stale",
                    ));
                }
                self.trust = *current;
            }
            FactLineageEventKindV1::PayloadAccessChanged { previous, current } => {
                if previous != &self.access {
                    return Err(storage_message(
                        COMMIT_OPERATION,
                        "payload access transition is stale",
                    ));
                }
                self.access = *current;
                if requires_payload_purge(*current) {
                    self.active_assertion_id = None;
                }
            }
            FactLineageEventKindV1::Curated { .. }
            | FactLineageEventKindV1::LegacyImported { .. } => {}
        }
        self.last_event_id = Some(event.event_id().clone());
        self.updated_at = event.occurred_at();
        Ok(())
    }
}

async fn publish_current_projection(
    transaction: &Transaction,
    owner: &OwnerKey,
    batch: &FactWriteBatch,
) -> FactStoreResult<()> {
    let mut projection = load_current_projection(transaction, owner, batch.fact_id())
        .await?
        .unwrap_or(Projection::empty()?);
    for event in batch.events() {
        projection.apply(event)?;
    }
    if projection.active_assertion_id.is_none() && !requires_payload_purge(projection.access) {
        return Err(storage_message(
            COMMIT_OPERATION,
            "fact projection has no active assertion",
        ));
    }
    let last = projection
        .last_event_id
        .as_ref()
        .ok_or(FactStoreError::EmptyBatch)?;
    transaction
        .execute(
            "INSERT INTO memory_v2_current_facts(
                fact_id, owner_kind, project_id, payload_access, trust_score,
                active_assertion_id, last_event_id, updated_at
             ) VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
             ON CONFLICT(fact_id, owner_kind, project_id) DO UPDATE SET
                payload_access = excluded.payload_access,
                trust_score = excluded.trust_score,
                active_assertion_id = excluded.active_assertion_id,
                last_event_id = excluded.last_event_id,
                updated_at = excluded.updated_at",
            params![
                batch.fact_id().as_str(),
                owner.kind,
                owner.project_id.as_str(),
                payload_access_label(projection.access),
                projection.trust.as_f64(),
                projection
                    .active_assertion_id
                    .as_ref()
                    .map(FactAssertionId::as_str),
                last.as_str(),
                projection.updated_at.0,
            ],
        )
        .await
        .map_err(|error| storage_error(COMMIT_OPERATION, error))?;
    if requires_payload_purge(projection.access) {
        transaction
            .execute_batch("PRAGMA secure_delete = ON;")
            .await
            .map_err(|error| storage_error(COMMIT_OPERATION, error))?;
        transaction
            .execute(
                "DELETE FROM memory_v2_assertion_vectors
                 WHERE fact_id = ?1 AND owner_kind = ?2 AND project_id = ?3",
                params![
                    batch.fact_id().as_str(),
                    owner.kind,
                    owner.project_id.as_str()
                ],
            )
            .await
            .map_err(|error| storage_error(COMMIT_OPERATION, error))?;
        transaction
            .execute(
                "DELETE FROM memory_v2_assertion_payloads
                 WHERE fact_id = ?1 AND owner_kind = ?2 AND project_id = ?3",
                params![
                    batch.fact_id().as_str(),
                    owner.kind,
                    owner.project_id.as_str()
                ],
            )
            .await
            .map_err(|error| storage_error(COMMIT_OPERATION, error))?;
        // A live transition to a terminal payload access must erase the same
        // free-text feedback surface as the canonical purge path
        // (`purge_payload_rows`), so a deleted fact never retains
        // API-reachable feedback source/note text.
        transaction
            .execute(
                "UPDATE memory_v2_feedback_history
                 SET source = NULL, note = NULL,
                     details_availability = CASE
                         WHEN details_availability = 'available' THEN 'legacy_redacted'
                         ELSE details_availability
                     END
                 WHERE fact_id = ?1 AND owner_kind = ?2 AND project_id = ?3",
                params![
                    batch.fact_id().as_str(),
                    owner.kind,
                    owner.project_id.as_str()
                ],
            )
            .await
            .map_err(|error| storage_error(COMMIT_OPERATION, error))?;
    }
    Ok(())
}

async fn load_current_projection(
    transaction: &Transaction,
    owner: &OwnerKey,
    fact_id: &FactId,
) -> FactStoreResult<Option<Projection>> {
    let mut rows = transaction
        .query(
            "SELECT payload_access, trust_score, active_assertion_id,
                    last_event_id, updated_at
             FROM memory_v2_current_facts
             WHERE fact_id = ?1 AND owner_kind = ?2 AND project_id = ?3",
            params![fact_id.as_str(), owner.kind, owner.project_id.as_str()],
        )
        .await
        .map_err(|error| storage_error(QUERY_OPERATION, error))?;
    let Some(row) = rows
        .next()
        .await
        .map_err(|error| storage_error(QUERY_OPERATION, error))?
    else {
        return Ok(None);
    };
    Ok(Some(Projection {
        access: parse_payload_access(&row_string(&row, 0, QUERY_OPERATION)?)?,
        trust: Confidence::new(row_f64(&row, 1, QUERY_OPERATION)?)?,
        active_assertion_id: row_optional_string(&row, 2, QUERY_OPERATION)?
            .map(FactAssertionId::new)
            .transpose()?,
        last_event_id: row_optional_string(&row, 3, QUERY_OPERATION)?
            .map(FactEventId::new)
            .transpose()?,
        updated_at: UtcMicros(row_i64(&row, 4, QUERY_OPERATION)?),
    }))
}

async fn receipt_outcome(
    transaction: &Transaction,
    owner: &OwnerKey,
    batch: &FactWriteBatch,
    replay: bool,
) -> FactStoreResult<FactCommitOutcome> {
    let projection = load_current_projection(transaction, owner, batch.fact_id())
        .await?
        .ok_or_else(|| storage_message(COMMIT_OPERATION, "committed projection is missing"))?;
    let last = batch
        .events()
        .last()
        .map(FactLineageEventV1::event_id)
        .ok_or(FactStoreError::EmptyBatch)?;
    let receipt = FactCommitReceipt::new(
        batch.fact_id().clone(),
        batch.owner().clone(),
        batch
            .events()
            .iter()
            .map(|event| event.event_id().clone())
            .collect(),
        last.clone(),
        projection.active_assertion_id,
    )?;
    Ok(if replay {
        FactCommitOutcome::IdempotentReplay(receipt)
    } else {
        FactCommitOutcome::Committed(receipt)
    })
}

async fn ensure_fact_identity(
    transaction: &Transaction,
    owner: &OwnerKey,
    batch: &FactWriteBatch,
) -> FactStoreResult<()> {
    let mut rows = transaction
        .query(
            "SELECT owner_kind, project_id, owner_json, identity_json
             FROM memory_v2_facts WHERE fact_id = ?1",
            [batch.fact_id().as_str()],
        )
        .await
        .map_err(|error| storage_error(COMMIT_OPERATION, error))?;
    if let Some(row) = rows
        .next()
        .await
        .map_err(|error| storage_error(COMMIT_OPERATION, error))?
    {
        let stored_owner_kind = row_string(&row, 0, COMMIT_OPERATION)?;
        let stored_project_id = row_string(&row, 1, COMMIT_OPERATION)?;
        let stored_owner_json = row_string(&row, 2, COMMIT_OPERATION)?;
        let stored_identity = row_string(&row, 3, COMMIT_OPERATION)?;
        let supplied_identity = batch
            .identity_material()
            .map(|identity| to_json(identity, "serialize fact identity"))
            .transpose()?;
        if stored_owner_kind != owner.kind
            || stored_project_id != owner.project_id
            || stored_owner_json != owner.json
            || supplied_identity
                .as_ref()
                .is_some_and(|identity| identity != &stored_identity)
        {
            return identity_collision("fact", batch.fact_id().as_str());
        }
        return Ok(());
    }
    let identity = batch
        .identity_material()
        .ok_or_else(|| FactStoreError::Storage {
            operation: COMMIT_OPERATION,
            source: Box::new(std::io::Error::other(
                "new fact requires deterministic identity material",
            )),
        })?;
    let identity_json = to_json(identity, "serialize fact identity")?;
    let created_at = batch
        .events()
        .first()
        .map(FactLineageEventV1::occurred_at)
        .ok_or(FactStoreError::EmptyBatch)?;
    transaction
        .execute(
            "INSERT INTO memory_v2_facts(
                fact_id, owner_kind, project_id, owner_json, identity_json, created_at
             ) VALUES(?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                batch.fact_id().as_str(),
                owner.kind,
                owner.project_id.as_str(),
                owner.json.as_str(),
                identity_json,
                created_at.0,
            ],
        )
        .await
        .map_err(|error| storage_error(COMMIT_OPERATION, error))?;
    Ok(())
}

fn storage_error(
    operation: &'static str,
    source: impl Error + Send + Sync + 'static,
) -> FactStoreError {
    FactStoreError::Storage {
        operation,
        source: Box::new(source),
    }
}

fn storage_message(operation: &'static str, message: impl Into<String>) -> FactStoreError {
    storage_error(operation, std::io::Error::other(message.into()))
}

fn authority_storage_error(
    operation: &'static str,
    source: impl Error + Send + Sync + 'static,
) -> FactProposalStoreError {
    FactProposalStoreError::Storage {
        operation,
        source: Box::new(source),
    }
}

fn identity_collision<T>(kind: &'static str, id: &str) -> FactStoreResult<T> {
    Err(storage_message(
        COMMIT_OPERATION,
        format!("{kind} identity collision for {id}"),
    ))
}

fn to_json<T: Serialize + ?Sized>(value: &T, operation: &'static str) -> FactStoreResult<String> {
    serde_json::to_string(value).map_err(|error| storage_error(operation, error))
}

fn from_json<T: DeserializeOwned>(value: &str, operation: &'static str) -> FactStoreResult<T> {
    serde_json::from_str(value).map_err(|error| storage_error(operation, error))
}

fn row_string(row: &libsql::Row, index: i32, operation: &'static str) -> FactStoreResult<String> {
    row.get(index)
        .map_err(|error| storage_error(operation, error))
}

fn row_optional_string(
    row: &libsql::Row,
    index: i32,
    operation: &'static str,
) -> FactStoreResult<Option<String>> {
    row.get(index)
        .map_err(|error| storage_error(operation, error))
}

fn row_i64(row: &libsql::Row, index: i32, operation: &'static str) -> FactStoreResult<i64> {
    row.get(index)
        .map_err(|error| storage_error(operation, error))
}

fn row_optional_i64(
    row: &libsql::Row,
    index: i32,
    operation: &'static str,
) -> FactStoreResult<Option<i64>> {
    row.get(index)
        .map_err(|error| storage_error(operation, error))
}

fn row_optional_f64(
    row: &libsql::Row,
    index: i32,
    operation: &'static str,
) -> FactStoreResult<Option<f64>> {
    row.get(index)
        .map_err(|error| storage_error(operation, error))
}

fn row_f64(row: &libsql::Row, index: i32, operation: &'static str) -> FactStoreResult<f64> {
    row.get(index)
        .map_err(|error| storage_error(operation, error))
}

async fn row_exists(
    transaction: &Transaction,
    sql: &str,
    values: impl libsql::params::IntoParams,
) -> FactStoreResult<bool> {
    let mut rows = transaction
        .query(sql, values)
        .await
        .map_err(|error| storage_error(QUERY_OPERATION, error))?;
    Ok(rows
        .next()
        .await
        .map_err(|error| storage_error(QUERY_OPERATION, error))?
        .is_some())
}

async fn row_exists_params(
    transaction: &Transaction,
    sql: &str,
    values: impl libsql::params::IntoParams,
) -> FactStoreResult<bool> {
    row_exists(transaction, sql, values).await
}

fn payload_access_label(state: PayloadAccessState) -> &'static str {
    match state {
        PayloadAccessState::Eligible => "eligible",
        PayloadAccessState::Redacted => "redacted",
        PayloadAccessState::Quarantined => "quarantined",
        PayloadAccessState::RetentionExpired => "retention_expired",
        PayloadAccessState::Deleted => "deleted",
        PayloadAccessState::Unavailable => "unavailable",
        PayloadAccessState::Ambiguous => "ambiguous",
    }
}

fn parse_payload_access(value: &str) -> FactStoreResult<PayloadAccessState> {
    match value {
        "eligible" => Ok(PayloadAccessState::Eligible),
        "redacted" => Ok(PayloadAccessState::Redacted),
        "quarantined" => Ok(PayloadAccessState::Quarantined),
        "retention_expired" => Ok(PayloadAccessState::RetentionExpired),
        "deleted" => Ok(PayloadAccessState::Deleted),
        "unavailable" => Ok(PayloadAccessState::Unavailable),
        "ambiguous" => Ok(PayloadAccessState::Ambiguous),
        _ => Err(storage_message(
            QUERY_OPERATION,
            format!("unknown payload access state {value:?}"),
        )),
    }
}

fn requires_payload_purge(access: PayloadAccessState) -> bool {
    matches!(
        access,
        PayloadAccessState::Quarantined | PayloadAccessState::Deleted
    )
}

async fn query_current_facts_tx(
    snapshot: &Transaction,
    query: &CurrentFactsQuery,
) -> FactStoreResult<Vec<StoredFactV1>> {
    let owner = OwnerKey::new(query.owner())?;
    let mut rows = match query.after_fact_id() {
        Some(after) => {
            snapshot
                .query(
                    "SELECT fact_id FROM memory_v2_current_facts
                 WHERE owner_kind = ?1 AND project_id = ?2
                   AND active_assertion_id IS NOT NULL AND fact_id > ?3
                 ORDER BY fact_id ASC LIMIT ?4",
                    params![
                        owner.kind,
                        owner.project_id.as_str(),
                        after.as_str(),
                        query.limit() as i64,
                    ],
                )
                .await
        }
        None => {
            snapshot
                .query(
                    "SELECT fact_id FROM memory_v2_current_facts
                 WHERE owner_kind = ?1 AND project_id = ?2
                   AND active_assertion_id IS NOT NULL
                 ORDER BY fact_id ASC LIMIT ?3",
                    params![owner.kind, owner.project_id.as_str(), query.limit() as i64],
                )
                .await
        }
    }
    .map_err(|error| storage_error(QUERY_OPERATION, error))?;
    let mut fact_ids = Vec::new();
    while let Some(row) = rows
        .next()
        .await
        .map_err(|error| storage_error(QUERY_OPERATION, error))?
    {
        fact_ids.push(FactId::new(row_string(&row, 0, QUERY_OPERATION)?)?);
    }
    drop(rows);

    let mut facts = Vec::with_capacity(fact_ids.len());
    for fact_id in fact_ids {
        let fact = load_current_fact_tx(snapshot, &owner, query.owner(), &fact_id)
            .await?
            .ok_or_else(|| {
                storage_message(QUERY_OPERATION, "current fact disappeared in snapshot")
            })?;
        facts.push(fact);
    }
    Ok(facts)
}

async fn query_fact_current_tx(
    snapshot: &Transaction,
    owner: &FactOwnerV1,
    fact_id: &FactId,
) -> FactStoreResult<Option<StoredFactV1>> {
    let key = OwnerKey::new(owner)?;
    load_current_fact_tx(snapshot, &key, owner, fact_id).await
}

async fn load_current_fact_tx(
    snapshot: &Transaction,
    owner: &OwnerKey,
    typed_owner: &FactOwnerV1,
    fact_id: &FactId,
) -> FactStoreResult<Option<StoredFactV1>> {
    let mut rows = snapshot
        .query(
            "SELECT facts.fact_id, current_facts.payload_access, current_facts.trust_score,
                    current_facts.active_assertion_id, current_facts.last_event_id,
                    current_facts.updated_at, payloads.payload_json
             FROM memory_v2_current_facts AS current_facts
             JOIN memory_v2_facts AS facts
               ON facts.fact_id = current_facts.fact_id
              AND facts.owner_kind = current_facts.owner_kind
              AND facts.project_id = current_facts.project_id
             LEFT JOIN memory_v2_assertion_payloads AS payloads
               ON payloads.assertion_id = current_facts.active_assertion_id
              AND payloads.fact_id = current_facts.fact_id
              AND payloads.owner_kind = current_facts.owner_kind
              AND payloads.project_id = current_facts.project_id
             WHERE current_facts.fact_id = ?1
               AND current_facts.owner_kind = ?2
               AND current_facts.project_id = ?3
               AND facts.owner_json = ?4",
            params![
                fact_id.as_str(),
                owner.kind,
                owner.project_id.as_str(),
                owner.json.as_str(),
            ],
        )
        .await
        .map_err(|error| storage_error(QUERY_OPERATION, error))?;
    let Some(row) = rows
        .next()
        .await
        .map_err(|error| storage_error(QUERY_OPERATION, error))?
    else {
        return Ok(None);
    };
    let stored_id = FactId::new(row_string(&row, 0, QUERY_OPERATION)?)?;
    if &stored_id != fact_id {
        return Err(storage_message(
            QUERY_OPERATION,
            "current fact identity mismatch",
        ));
    }
    let access = parse_payload_access(&row_string(&row, 1, QUERY_OPERATION)?)?;
    let trust = Confidence::new(row_optional_f64(&row, 2, QUERY_OPERATION)?.ok_or_else(|| {
        storage_message(
            QUERY_OPERATION,
            "current fact trust score is unexpectedly null",
        )
    })?)?;
    let Some(active_assertion_id) = row_optional_string(&row, 3, QUERY_OPERATION)? else {
        return Ok(None);
    };
    let active_assertion_id = FactAssertionId::new(active_assertion_id)?;
    let last_event_id = FactEventId::new(row_string(&row, 4, QUERY_OPERATION)?)?;
    let projected_as_of = UtcMicros(row_i64(&row, 5, QUERY_OPERATION)?);
    let payload = match access {
        PayloadAccessState::Eligible => {
            let payload_json = row_optional_string(&row, 6, QUERY_OPERATION)?
                .ok_or(FactStoreError::PayloadAccessMismatch)?;
            Some(from_json::<FactPayloadV1>(&payload_json, QUERY_OPERATION)?)
        }
        _ => None,
    };
    let mapping = load_current_legacy_mapping_tx(snapshot, owner, typed_owner, fact_id).await?;
    StoredFactV1::new(
        stored_id,
        typed_owner.clone(),
        payload,
        access,
        trust,
        active_assertion_id,
        last_event_id,
        mapping,
        projected_as_of,
    )
    .map(Some)
}

async fn query_fact_as_of_tx(
    snapshot: &Transaction,
    query: &FactAsOfQuery,
) -> FactStoreResult<Option<StoredFactV1>> {
    let owner = OwnerKey::new(query.owner())?;
    let mut rows = snapshot
        .query(
            "SELECT event_json FROM memory_v2_lineage_events
             WHERE fact_id = ?1 AND owner_kind = ?2 AND project_id = ?3
               AND occurred_at <= ?4
             ORDER BY occurred_at ASC, event_id ASC",
            params![
                query.fact_id().as_str(),
                owner.kind,
                owner.project_id.as_str(),
                query.as_of().0,
            ],
        )
        .await
        .map_err(|error| storage_error(QUERY_OPERATION, error))?;
    let mut projection = Projection::empty()?;
    let mut observed_event = false;
    while let Some(row) = rows
        .next()
        .await
        .map_err(|error| storage_error(QUERY_OPERATION, error))?
    {
        let event = from_json::<FactLineageEventV1>(
            &row_string(&row, 0, QUERY_OPERATION)?,
            QUERY_OPERATION,
        )?;
        if event.fact_id() != query.fact_id() || event.owner() != query.owner() {
            return Err(storage_message(
                QUERY_OPERATION,
                "stored lineage event identity mismatch",
            ));
        }
        projection.apply(&event)?;
        observed_event = true;
    }
    drop(rows);
    if !observed_event {
        return Ok(None);
    }
    let Some(active_assertion_id) = projection.active_assertion_id.clone() else {
        return Ok(None);
    };
    let last_event_id = projection
        .last_event_id
        .clone()
        .ok_or(FactStoreError::EmptyBatch)?;
    let (payload, payload_access) = match projection.access {
        PayloadAccessState::Eligible => {
            match load_assertion_payload_tx(snapshot, &owner, query.fact_id(), &active_assertion_id)
                .await?
            {
                Some(payload) => (Some(payload), PayloadAccessState::Eligible),
                // A later deletion physically erases the payload and FTS/vector
                // copies. Do not resurrect that data merely because an as-of
                // projection predates the deletion event; retain the lineage but
                // make the unavailable payload explicit.
                None => (None, PayloadAccessState::Unavailable),
            }
        }
        access => (None, access),
    };
    let mapping = load_current_legacy_mapping_tx(snapshot, &owner, query.owner(), query.fact_id())
        .await?
        .filter(|mapping| mapping.migrated_at() <= query.as_of());
    StoredFactV1::new(
        query.fact_id().clone(),
        query.owner().clone(),
        payload,
        payload_access,
        projection.trust,
        active_assertion_id,
        last_event_id,
        mapping,
        projection.updated_at,
    )
    .map(Some)
}

async fn load_assertion_payload_tx(
    snapshot: &Transaction,
    owner: &OwnerKey,
    fact_id: &FactId,
    assertion_id: &FactAssertionId,
) -> FactStoreResult<Option<FactPayloadV1>> {
    let mut rows = snapshot
        .query(
            "SELECT payload_json FROM memory_v2_assertion_payloads
             WHERE assertion_id = ?1 AND fact_id = ?2
               AND owner_kind = ?3 AND project_id = ?4",
            params![
                assertion_id.as_str(),
                fact_id.as_str(),
                owner.kind,
                owner.project_id.as_str(),
            ],
        )
        .await
        .map_err(|error| storage_error(QUERY_OPERATION, error))?;
    let Some(row) = rows
        .next()
        .await
        .map_err(|error| storage_error(QUERY_OPERATION, error))?
    else {
        return Ok(None);
    };
    from_json(&row_string(&row, 0, QUERY_OPERATION)?, QUERY_OPERATION).map(Some)
}

async fn query_fact_lineage_tx(
    snapshot: &Transaction,
    query: &FactLineageQuery,
) -> FactStoreResult<Vec<FactLineageEventV1>> {
    let owner = OwnerKey::new(query.owner())?;
    let mut rows = match query.after() {
        Some(after) => {
            snapshot
                .query(
                    "SELECT event_json FROM memory_v2_lineage_events
                 WHERE fact_id = ?1 AND owner_kind = ?2 AND project_id = ?3
                   AND (occurred_at > ?4 OR (occurred_at = ?4 AND event_id > ?5))
                 ORDER BY occurred_at ASC, event_id ASC LIMIT ?6",
                    params![
                        query.fact_id().as_str(),
                        owner.kind,
                        owner.project_id.as_str(),
                        after.occurred_at().0,
                        after.event_id().as_str(),
                        query.limit() as i64,
                    ],
                )
                .await
        }
        None => {
            snapshot
                .query(
                    "SELECT event_json FROM memory_v2_lineage_events
                 WHERE fact_id = ?1 AND owner_kind = ?2 AND project_id = ?3
                 ORDER BY occurred_at ASC, event_id ASC LIMIT ?4",
                    params![
                        query.fact_id().as_str(),
                        owner.kind,
                        owner.project_id.as_str(),
                        query.limit() as i64,
                    ],
                )
                .await
        }
    }
    .map_err(|error| storage_error(QUERY_OPERATION, error))?;
    let mut events = Vec::new();
    while let Some(row) = rows
        .next()
        .await
        .map_err(|error| storage_error(QUERY_OPERATION, error))?
    {
        let event = from_json::<FactLineageEventV1>(
            &row_string(&row, 0, QUERY_OPERATION)?,
            QUERY_OPERATION,
        )?;
        if event.fact_id() != query.fact_id() || event.owner() != query.owner() {
            return Err(storage_message(
                QUERY_OPERATION,
                "stored lineage event identity mismatch",
            ));
        }
        events.push(event);
    }
    Ok(events)
}

async fn resolve_legacy_fact_tx(
    snapshot: &Transaction,
    query: &LegacyFactQuery,
) -> FactStoreResult<Option<FactId>> {
    let owner = OwnerKey::new(query.owner())?;
    let mut rows = snapshot
        .query(
            "SELECT fact_id, owner_json FROM memory_v2_legacy_map
             WHERE owner_kind = ?1 AND project_id = ?2
               AND source_store_id = ?3 AND legacy_fact_id = ?4",
            params![
                owner.kind,
                owner.project_id.as_str(),
                query.source_store_id().as_str(),
                query.legacy_fact_id(),
            ],
        )
        .await
        .map_err(|error| storage_error(QUERY_OPERATION, error))?;
    let Some(row) = rows
        .next()
        .await
        .map_err(|error| storage_error(QUERY_OPERATION, error))?
    else {
        return Ok(None);
    };
    if row_string(&row, 1, QUERY_OPERATION)? != owner.json {
        return Err(FactStoreError::OwnerMismatch);
    }
    let fact_id = FactId::new(row_string(&row, 0, QUERY_OPERATION)?)?;
    query.validate_resolved_fact_id(&fact_id)?;
    Ok(Some(fact_id))
}

async fn get_retrieval_anchor_tx(
    snapshot: &Transaction,
    query: &RetrievalAnchorQuery,
) -> FactStoreResult<Option<RetrievalAnchorRecordV2>> {
    let owner = OwnerKey::new(query.owner())?;
    let mut rows = snapshot
        .query(
            "SELECT anchor_json FROM retrieval_anchors
             WHERE anchor_id = ?1 AND owner_json = ?2",
            params![query.anchor_id().as_str(), owner.json.as_str()],
        )
        .await
        .map_err(|error| storage_error(QUERY_OPERATION, error))?;
    let Some(row) = rows
        .next()
        .await
        .map_err(|error| storage_error(QUERY_OPERATION, error))?
    else {
        return Ok(None);
    };
    let anchor = from_json::<RetrievalAnchorRecordV2>(
        &row_string(&row, 0, QUERY_OPERATION)?,
        QUERY_OPERATION,
    )?;
    if anchor.anchor_id() != query.anchor_id()
        || FactOwnerV1::from(anchor.owner().clone()) != *query.owner()
        || !anchor_matches(snapshot, &owner, &anchor).await?
    {
        return Err(storage_message(
            QUERY_OPERATION,
            "retrieval anchor identity mismatch",
        ));
    }
    Ok(Some(anchor))
}

async fn load_current_legacy_mapping_tx(
    snapshot: &Transaction,
    owner: &OwnerKey,
    typed_owner: &FactOwnerV1,
    fact_id: &FactId,
) -> FactStoreResult<Option<LegacyFactMappingV1>> {
    let mut rows = snapshot
        .query(
            "SELECT mapping_json FROM memory_v2_legacy_map
             WHERE owner_kind = ?1 AND project_id = ?2 AND fact_id = ?3
             ORDER BY source_store_id ASC LIMIT 1",
            params![owner.kind, owner.project_id.as_str(), fact_id.as_str()],
        )
        .await
        .map_err(|error| storage_error(QUERY_OPERATION, error))?;
    let Some(row) = rows
        .next()
        .await
        .map_err(|error| storage_error(QUERY_OPERATION, error))?
    else {
        return Ok(None);
    };
    let mapping =
        from_json::<LegacyFactMappingV1>(&row_string(&row, 0, QUERY_OPERATION)?, QUERY_OPERATION)?;
    if mapping.owner() != typed_owner || mapping.fact_id() != fact_id {
        return Err(storage_message(
            QUERY_OPERATION,
            "legacy mapping identity mismatch",
        ));
    }
    Ok(Some(mapping))
}

async fn promote_fact_proposal_tx(
    transaction: &Transaction,
    promotion: &PromoteFactProposal,
) -> Result<PromotionAttempt, FactProposalStoreError> {
    let owner = OwnerKey::new(promotion.owner())?;
    let actual = proposal_current_state(transaction, &owner, promotion.proposal_id()).await?;
    if actual != Some(promotion.expected_state()) {
        if let Some(stored_transition_json) =
            matching_applied_promotion_transition(transaction, &owner, promotion).await?
        {
            let actual_last =
                current_last_event(transaction, &owner, promotion.batch().fact_id()).await?;
            if actual_last.as_ref()
                == promotion
                    .batch()
                    .events()
                    .last()
                    .map(FactLineageEventV1::event_id)
            {
                let commit = commit_fact_tx(transaction, promotion.batch())
                    .await?
                    .outcome;
                if let FactCommitOutcome::IdempotentReplay(receipt) = &commit
                    && promotion_transition_json(promotion, receipt)? == stored_transition_json
                {
                    return Ok(PromotionAttempt {
                        outcome: PromoteFactProposalOutcome::new(
                            promotion.proposal_id().clone(),
                            promotion.expected_state(),
                            commit,
                        )
                        .map_err(FactStoreError::from)?,
                        wrote: false,
                    });
                }
            }
        }
        return Err(FactProposalStoreError::ProposalStateConflict {
            proposal_id: promotion.proposal_id().clone(),
            expected: promotion.expected_state(),
            actual,
        });
    }

    let commit = commit_fact_tx(transaction, promotion.batch())
        .await?
        .outcome;
    if matches!(&commit, FactCommitOutcome::Conflict(_)) {
        return Ok(PromotionAttempt {
            outcome: PromoteFactProposalOutcome::new(
                promotion.proposal_id().clone(),
                promotion.expected_state(),
                commit,
            )
            .map_err(FactStoreError::from)?,
            wrote: false,
        });
    }
    let receipt = match &commit {
        FactCommitOutcome::Committed(receipt) | FactCommitOutcome::IdempotentReplay(receipt) => {
            receipt
        }
        FactCommitOutcome::Conflict(_) => unreachable!("handled above"),
        _ => {
            return Err(authority_storage_error(
                PROMOTE_OPERATION,
                std::io::Error::other("unrecognized fact commit outcome"),
            ));
        }
    };
    let transition_json = promotion_transition_json(promotion, receipt)?;
    let transition_id = proposal_transition_id(&transition_json);
    let reviewer_json = promotion
        .reviewer()
        .map(|reviewer| to_json(reviewer, PROMOTE_OPERATION))
        .transpose()?;
    let occurred_at = promotion
        .batch()
        .events()
        .last()
        .ok_or(FactStoreError::EmptyBatch)?
        .occurred_at()
        .0;
    transaction
        .execute(
            "INSERT INTO memory_v2_proposal_transitions(
                transition_id, proposal_id, owner_kind, project_id,
                previous_state, current_state, reviewer_json, validation_json,
                origin, promoted_fact_id, promoted_assertion_id, promoted_event_id,
                transition_json, occurred_at
             ) VALUES(?1, ?2, ?3, ?4, ?5, 'applied', ?6, NULL,
                      'runtime', ?7, ?8, ?9, ?10, ?11)",
            params![
                transition_id.as_str(),
                promotion.proposal_id().as_str(),
                owner.kind,
                owner.project_id.as_str(),
                proposal_state_label(promotion.expected_state()),
                reviewer_json,
                receipt.fact_id().as_str(),
                receipt.active_assertion_id().map(FactAssertionId::as_str),
                receipt.last_event_id().as_str(),
                transition_json,
                occurred_at,
            ],
        )
        .await
        .map_err(|error| authority_storage_error(PROMOTE_OPERATION, error))?;
    let changed = transaction
        .execute(
            "UPDATE memory_v2_proposal_current
             SET state = 'applied', revision = revision + 1,
                 last_transition_id = ?1, updated_at = ?2
             WHERE proposal_id = ?3 AND owner_kind = ?4 AND project_id = ?5
               AND state = ?6",
            params![
                transition_id.as_str(),
                occurred_at,
                promotion.proposal_id().as_str(),
                owner.kind,
                owner.project_id.as_str(),
                proposal_state_label(promotion.expected_state()),
            ],
        )
        .await
        .map_err(|error| authority_storage_error(PROMOTE_OPERATION, error))?;
    if changed != 1 {
        return Err(FactProposalStoreError::ProposalStateConflict {
            proposal_id: promotion.proposal_id().clone(),
            expected: promotion.expected_state(),
            actual: proposal_current_state(transaction, &owner, promotion.proposal_id()).await?,
        });
    }
    Ok(PromotionAttempt {
        outcome: PromoteFactProposalOutcome::new(
            promotion.proposal_id().clone(),
            promotion.expected_state(),
            commit,
        )
        .map_err(FactStoreError::from)?,
        wrote: true,
    })
}

async fn proposal_current_state(
    transaction: &Transaction,
    owner: &OwnerKey,
    proposal_id: &ProvenanceId,
) -> Result<Option<FactProposalPromotionStateV1>, FactProposalStoreError> {
    let mut rows = transaction
        .query(
            "SELECT current_state.state, proposals.owner_json
             FROM memory_v2_proposal_current AS current_state
             JOIN memory_v2_proposals AS proposals
               ON proposals.proposal_id = current_state.proposal_id
              AND proposals.owner_kind = current_state.owner_kind
              AND proposals.project_id = current_state.project_id
             WHERE current_state.proposal_id = ?1
               AND current_state.owner_kind = ?2
               AND current_state.project_id = ?3",
            params![proposal_id.as_str(), owner.kind, owner.project_id.as_str(),],
        )
        .await
        .map_err(|error| authority_storage_error(PROMOTE_OPERATION, error))?;
    let Some(row) = rows
        .next()
        .await
        .map_err(|error| authority_storage_error(PROMOTE_OPERATION, error))?
    else {
        return Ok(None);
    };
    let owner_json = row
        .get::<String>(1)
        .map_err(|error| authority_storage_error(PROMOTE_OPERATION, error))?;
    if owner_json != owner.json {
        return Err(authority_storage_error(
            PROMOTE_OPERATION,
            std::io::Error::other("proposal owner identity mismatch"),
        ));
    }
    let state = row
        .get::<String>(0)
        .map_err(|error| authority_storage_error(PROMOTE_OPERATION, error))?;
    parse_proposal_current_state(&state)
}

async fn matching_applied_promotion_transition(
    transaction: &Transaction,
    owner: &OwnerKey,
    promotion: &PromoteFactProposal,
) -> Result<Option<String>, FactProposalStoreError> {
    let mut rows = transaction
        .query(
            "SELECT current_state.state, proposals.owner_json,
                    transition.previous_state, transition.current_state,
                    transition.promoted_fact_id, transition.promoted_event_id,
                    transition.transition_json
             FROM memory_v2_proposal_current AS current_state
             JOIN memory_v2_proposals AS proposals
               ON proposals.proposal_id = current_state.proposal_id
              AND proposals.owner_kind = current_state.owner_kind
              AND proposals.project_id = current_state.project_id
             JOIN memory_v2_proposal_transitions AS transition
               ON transition.transition_id = current_state.last_transition_id
              AND transition.proposal_id = current_state.proposal_id
              AND transition.owner_kind = current_state.owner_kind
              AND transition.project_id = current_state.project_id
             WHERE current_state.proposal_id = ?1
               AND current_state.owner_kind = ?2
               AND current_state.project_id = ?3",
            params![
                promotion.proposal_id().as_str(),
                owner.kind,
                owner.project_id.as_str(),
            ],
        )
        .await
        .map_err(|error| authority_storage_error(PROMOTE_OPERATION, error))?;
    let Some(row) = rows
        .next()
        .await
        .map_err(|error| authority_storage_error(PROMOTE_OPERATION, error))?
    else {
        return Ok(None);
    };
    if row_string(&row, 1, PROMOTE_OPERATION)? != owner.json {
        return Err(authority_storage_error(
            PROMOTE_OPERATION,
            std::io::Error::other("proposal owner identity mismatch"),
        ));
    }
    let last_event_id = promotion
        .batch()
        .events()
        .last()
        .map(FactLineageEventV1::event_id)
        .ok_or(FactStoreError::EmptyBatch)?;
    if row_string(&row, 0, PROMOTE_OPERATION)? != "applied"
        || row_string(&row, 2, PROMOTE_OPERATION)?
            != proposal_state_label(promotion.expected_state())
        || row_string(&row, 3, PROMOTE_OPERATION)? != "applied"
        || row_optional_string(&row, 4, PROMOTE_OPERATION)?.as_deref()
            != Some(promotion.batch().fact_id().as_str())
        || row_optional_string(&row, 5, PROMOTE_OPERATION)?.as_deref()
            != Some(last_event_id.as_str())
    {
        return Ok(None);
    }
    Ok(Some(row_string(&row, 6, PROMOTE_OPERATION)?))
}

fn proposal_state_label(state: FactProposalPromotionStateV1) -> &'static str {
    match state {
        FactProposalPromotionStateV1::PendingApproval => "pending",
        FactProposalPromotionStateV1::Applying => "applying",
    }
}

fn parse_proposal_current_state(
    state: &str,
) -> Result<Option<FactProposalPromotionStateV1>, FactProposalStoreError> {
    match state {
        "pending" => Ok(Some(FactProposalPromotionStateV1::PendingApproval)),
        "applying" => Ok(Some(FactProposalPromotionStateV1::Applying)),
        "applied" | "rejected" => Ok(None),
        _ => Err(authority_storage_error(
            PROMOTE_OPERATION,
            std::io::Error::other(format!("unknown proposal state {state:?}")),
        )),
    }
}

fn promotion_transition_json(
    promotion: &PromoteFactProposal,
    receipt: &FactCommitReceipt,
) -> Result<String, FactProposalStoreError> {
    to_json(
        &json!({
            "proposal_id": promotion.proposal_id().as_str(),
            "previous_state": proposal_state_label(promotion.expected_state()),
            "current_state": "applied",
            "reviewer": promotion.reviewer().map(|reviewer| reviewer.as_str()),
            "fact_id": receipt.fact_id().as_str(),
            "active_assertion_id": receipt.active_assertion_id().map(FactAssertionId::as_str),
            "last_event_id": receipt.last_event_id().as_str(),
        }),
        PROMOTE_OPERATION,
    )
    .map_err(FactProposalStoreError::from)
}

fn proposal_transition_id(transition_json: &str) -> String {
    let digest = Sha256::digest(transition_json.as_bytes());
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut id = String::from("proposal-transition:");
    for byte in digest {
        id.push(char::from(HEX[usize::from(byte >> 4)]));
        id.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    id
}

#[cfg(test)]
#[path = "memory_repair_test.rs"]
mod memory_repair_test;

#[cfg(test)]
#[path = "memory_cutover_test.rs"]
mod memory_cutover_test;
