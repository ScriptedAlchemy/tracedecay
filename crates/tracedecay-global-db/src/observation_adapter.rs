use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use tracedecay_application::clock::now_micros;
use tracing::Instrument;

use tracedecay_domain::{
    CanonicalObservationIdV1, ClaudeSourceCursorV1, ClaudeSourceIdentityV1, DurableObservationV1,
    EvidenceAvailabilityV1, GenerationBoundRepositoryProvenanceV1, ManifestDigest,
    ObservationCollisionOutcomeV1, ObservationIdentityMaterialV1, ObservationScopeV1,
    PayloadDigestV1, PayloadReferenceV1, ProjectionGenerationId, RetrievalAnchorId,
    RetrievalAnchorRecordV2, SanitizationReceiptV1, canonical_json_bytes_and_sha256,
    canonical_sha256, classify_observation_collision, is_canonical_payload_revision_replay,
};
use tracedecay_store::observation::{
    CursorAdvanceOutcome, ObservationCoverageReason, ObservationCursorAdvance,
    ObservationIdentityCollisionDispositionV1,
};
use tracedecay_store::{
    AnchoredObservationWrite, CommandDigestV1, ConsistencyModeV1,
    CursorAdvanceLedgerDisagreementV1, CursorAdvanceLedgerIdentityV1, DurabilityClassV1,
    IdempotencyIdentityV1, ObservationBatchFallbackCause, ObservationBatchPersistOutcome,
    ObservationCommitReceipt, ObservationPersistOutcome, ObservationProjectionStatus,
    ObservationProjectionStore, ObservationReadOperationV1, ObservationReadResultV1,
    ObservationReplayRequest, ObservationStore, ObservationStoreError, ObservationStoreResult,
    OperationPriorityV1, ProjectReadOperationV1, ProjectReadResultV1, ProjectionCheckpoint,
    ProjectionPersistOutcome, ProjectionPredecessorConvergence, ProjectionRebuildOutcome,
    ProjectionStoreResult, RepositoryOperationEnvelopeV1, RepositoryProvenanceAttachmentV1,
    RepositoryReadOperationV1, RepositoryReadResultV1, RepositoryWritePayloadV1,
    RuntimeBatchCompatibilityV1, RuntimeCancellationIdV1, RuntimeCancellationIdentityV1,
    RuntimeDeadlineIdV1, RuntimeDeadlineV1, RuntimeInterruptionV1, RuntimeReadCoverageV1,
    RuntimeReadOperationV1, RuntimeReadRequestV1, RuntimeReadResultV1, RuntimeRequestControlV1,
    RuntimeRequestProbeV1, RuntimeSubmitOutcomeV1, RuntimeSubmitRequestV1, RuntimeTransactionIdV1,
    RuntimeTransactionScopeV1, StorageRuntimeErrorV1, StoreClientIdV1, StoreIdempotencyKeyV1,
    StoreOperationIdV1, StoreOperationMetadataV1, StoredObservation, StoredObservationRowV1,
};

use tracedecay_runtime_core::db::{Database, DatabaseEngineReadSnapshot, DatabaseRuntimeClientV1};
use tracedecay_runtime_core::store_runtime::registry::StoreRuntimeRegistryFailure;
use tracedecay_rusqlite_runtime::repository::observation_cursor_authority::{
    COMMIT_SOURCE_CURSOR_SQL, READ_CURSOR_ADVANCE_SQL, READ_SOURCE_CURSOR_SQL,
    RECORD_CURSOR_ADVANCE_SQL, cursor_advance_ledger_row_matches,
};

/// Observation-store adapter over the already-registered authoritative
/// runtime. The struct is concrete: the collision tests prove the
/// terminal-refusal fast path never accesses the retained observation row —
/// they corrupt and hide that row after the marker exists, so any later read
/// fails loudly — not through an adapter seam or test-only port.
#[derive(Clone)]
pub struct GlobalDbObservationStore {
    database: Database,
    runtime: DatabaseRuntimeClientV1,
}

impl GlobalDbObservationStore {
    pub fn new(database: Database) -> Self {
        let runtime = database.runtime_client();
        Self { database, runtime }
    }

    /// Records a terminal refusal — the marker in
    /// `observation_admission_refusals` AND the typed
    /// `observation_identity_collision` coverage advance — in ONE atomic
    /// authority transaction, but only when
    /// the candidate stands at the sequential scan frontier; any other shape
    /// (covered replay, stale expected cursor, gap, generation jump) leaves
    /// every ledger untouched.
    ///
    /// Atomicity is the contract: either the marker and its coverage land
    /// together or neither is visible, so a failure while recording coverage
    /// can never orphan a marker whose record the cursor still re-reads. The
    /// frontier is re-verified INSIDE the transaction (exact compare-and-set
    /// against the durable cursor), the advance-ledger row must carry the
    /// `observation_identity_collision` reason with no receipt, and the cursor moves to
    /// the advance's next position — executed through the one canonical
    /// cursor-advance statement set
    /// (`tracedecay_rusqlite_runtime::repository::observation_cursor_authority`)
    /// that the runtime write path also executes. No record content is
    /// decoded, derived, or hashed.
    #[hotpath::skip]
    async fn record_refusal_with_coverage(
        &self,
        write: &AnchoredObservationWrite,
        retained_digest: &PayloadDigestV1,
        actual_cursor: Option<&ClaudeSourceCursorV1>,
    ) -> ObservationStoreResult<RefusalCoverageOutcome> {
        const OPERATION: &str = "record refused admission terminal and coverage";
        let candidate = write.observation();
        let identity = candidate.identity();
        let mut advance = match refused_scan_frontier(write, actual_cursor)? {
            RefusedScanFrontier::AtFrontier(advance) => *advance,
            RefusedScanFrontier::NotAtFrontier => {
                return Ok(RefusalCoverageOutcome::NotAtFrontier {
                    actual: actual_cursor.cloned(),
                });
            }
        };
        match (
            write.next_cursor().file_identity(),
            write.next_cursor().resume_fingerprint(),
        ) {
            (Some(file_identity), Some(resume_fingerprint)) => {
                advance = advance.with_resume_checkpoint(file_identity, resume_fingerprint);
            }
            (None, None) => {}
            _ => {
                return Err(runtime_storage_error(
                    OPERATION,
                    "cursor resume checkpoint is incomplete",
                ));
            }
        }
        let source_json = serde_json::to_string(identity.source())
            .map_err(|error| runtime_storage_error(OPERATION, error))?;
        let scope_json = serde_json::to_string(identity.scope())
            .map_err(|error| runtime_storage_error(OPERATION, error))?;
        let coverage_json = serde_json::to_string(&advance.coverage())
            .map_err(|error| runtime_storage_error(OPERATION, error))?;
        let next_cursor_json = serde_json::to_string(advance.next_cursor())
            .map_err(|error| runtime_storage_error(OPERATION, error))?;
        let transaction = self
            .database
            .begin_write_transaction(OPERATION)
            .await
            .map_err(|error| runtime_storage_error(OPERATION, error))?;
        // Exact compare-and-set under the write lock: the durable cursor must
        // still be the caller's expected frontier.
        let mut cursor_rows = transaction
            .query(
                READ_SOURCE_CURSOR_SQL,
                tracedecay_runtime_core::db::engine::params![
                    source_json.as_str(),
                    scope_json.as_str()
                ],
            )
            .await
            .map_err(|error| runtime_storage_error(OPERATION, error))?;
        let durable_cursor = cursor_rows
            .next()
            .await
            .map_err(|error| runtime_storage_error(OPERATION, error))?
            .map(|row| {
                let encoded = row
                    .get::<String>(0)
                    .map_err(|error| runtime_storage_error(OPERATION, error))?;
                serde_json::from_str::<ClaudeSourceCursorV1>(&encoded)
                    .map_err(|error| runtime_storage_error(OPERATION, error))
            })
            .transpose()?;
        drop(cursor_rows);
        // Compare the typed cursor authority, not its incidental JSON wire
        // spelling. Legacy/default-equivalent encodings are the same CAS
        // value; malformed cursor state stays a hard storage error above.
        if durable_cursor.as_ref() != write.expected_cursor() {
            transaction
                .rollback()
                .await
                .map_err(|error| runtime_storage_error(OPERATION, error))?;
            return Ok(RefusalCoverageOutcome::NotAtFrontier {
                actual: durable_cursor,
            });
        }
        transaction
            .execute(
                "INSERT INTO observation_admission_refusals (
                    observation_id, refused_payload_digest, retained_payload_digest, refused_at
                 ) VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT DO NOTHING",
                tracedecay_runtime_core::db::engine::params![
                    candidate.observation_id().as_str(),
                    candidate.payload_reference().digest().as_str(),
                    retained_digest.as_str(),
                    now_micros().0
                ],
            )
            .await
            .map_err(|error| runtime_storage_error(OPERATION, error))?;
        transaction
            .execute(
                RECORD_CURSOR_ADVANCE_SQL,
                tracedecay_runtime_core::db::engine::params![
                    source_json.as_str(),
                    scope_json.as_str(),
                    coverage_json.as_str(),
                    ObservationCoverageReason::ObservationIdentityCollision.as_str(),
                    None::<&str>
                ],
            )
            .await
            .map_err(|error| runtime_storage_error(OPERATION, error))?;
        let mut ledger_rows = transaction
            .query(
                READ_CURSOR_ADVANCE_SQL,
                tracedecay_runtime_core::db::engine::params![
                    source_json.as_str(),
                    scope_json.as_str(),
                    coverage_json.as_str()
                ],
            )
            .await
            .map_err(|error| runtime_storage_error(OPERATION, error))?;
        let ledger = ledger_rows
            .next()
            .await
            .map_err(|error| runtime_storage_error(OPERATION, error))?
            .map(|row| {
                Ok::<_, ObservationStoreError>((
                    row.get::<String>(0)
                        .map_err(|error| runtime_storage_error(OPERATION, error))?,
                    row.get::<Option<String>>(1)
                        .map_err(|error| runtime_storage_error(OPERATION, error))?,
                ))
            })
            .transpose()?;
        drop(ledger_rows);
        // A coverage row that names any other reason or a receipt is a real
        // cursor-advance failure: roll the WHOLE transaction back so the
        // marker is not visible either — no orphan, by construction.
        if !cursor_advance_ledger_row_matches(
            ledger.as_ref(),
            ObservationCoverageReason::ObservationIdentityCollision.as_str(),
            None,
        ) {
            let disagreement = if let Some((stored_reason, stored_receipt_id)) = ledger.as_ref() {
                let authority_receipt = if let Some(stored_receipt_id) =
                    stored_receipt_id.as_deref()
                {
                    let mut receipt_rows = transaction
                        .query(
                            "SELECT receipt_json FROM sanitization_receipts WHERE receipt_id = ?1",
                            tracedecay_runtime_core::db::engine::params![stored_receipt_id],
                        )
                        .await
                        .map_err(|error| runtime_storage_error(OPERATION, error))?;
                    let receipt_json = receipt_rows
                        .next()
                        .await
                        .map_err(|error| runtime_storage_error(OPERATION, error))?
                        .map(|row| {
                            row.get::<String>(0)
                                .map_err(|error| runtime_storage_error(OPERATION, error))
                        })
                        .transpose()?;
                    drop(receipt_rows);
                    receipt_json.and_then(|receipt_json| {
                        let receipt =
                            serde_json::from_str::<SanitizationReceiptV1>(&receipt_json).ok()?;
                        (receipt.receipt().receipt_id().as_str() == stored_receipt_id
                            && serde_json::to_string(&receipt).ok().as_deref()
                                == Some(receipt_json.as_str()))
                        .then_some(receipt)
                    })
                } else {
                    None
                };
                Some(CursorAdvanceLedgerDisagreementV1::new(
                    advance.next_cursor().source().clone(),
                    advance.next_cursor().scope().clone(),
                    advance.coverage(),
                    CursorAdvanceLedgerIdentityV1::from_stored_row_with_authority_receipt(
                        stored_reason,
                        stored_receipt_id.as_deref(),
                        authority_receipt.as_ref(),
                    ),
                    advance.ledger_identity(),
                ))
            } else {
                None
            };
            transaction
                .rollback()
                .await
                .map_err(|error| runtime_storage_error(OPERATION, error))?;
            return Err(disagreement.map_or(
                ObservationStoreError::CursorAdvanceCollision,
                |disagreement| ObservationStoreError::CursorAdvanceLedgerDisagreement {
                    disagreement: Box::new(disagreement),
                },
            ));
        }
        transaction
            .execute(
                COMMIT_SOURCE_CURSOR_SQL,
                tracedecay_runtime_core::db::engine::params![
                    source_json.as_str(),
                    scope_json.as_str(),
                    next_cursor_json.as_str()
                ],
            )
            .await
            .map_err(|error| runtime_storage_error(OPERATION, error))?;
        transaction
            .commit()
            .await
            .map_err(|error| runtime_storage_error(OPERATION, error))?;
        Ok(RefusalCoverageOutcome::Recorded)
    }

    #[hotpath::skip]
    pub async fn converge_projection_predecessor(
        &self,
    ) -> ProjectionStoreResult<ProjectionPredecessorConvergence> {
        crate::converge_projection_predecessor(&self.database).await
    }

    #[hotpath::skip]
    async fn prepare_observation_persist(
        &self,
        write: AnchoredObservationWrite,
        preflight: &ObservationPreflightSnapshot,
        batch_state: &mut ObservationBatchState,
        known_cursor: Option<Option<ClaudeSourceCursorV1>>,
    ) -> ObservationStoreResult<PreparedObservationPersist> {
        let observation = write.observation();
        let observation_id = observation.observation_id().clone();
        let actual_cursor = || {
            known_cursor.clone().unwrap_or_else(|| {
                preflight.source_cursor(observation.source(), observation.scope())
            })
        };
        if let Some(retained_digest) =
            preflight.admission_refusal(&observation_id, observation.payload_reference().digest())
        {
            let cursor = actual_cursor();
            if write.identity_collision_disposition()
                == ObservationIdentityCollisionDispositionV1::RetryWithAlternateIdentity
            {
                if matches!(
                    refused_scan_frontier(&write, cursor.as_ref())?,
                    RefusedScanFrontier::NotAtFrontier
                ) {
                    return Err(ObservationStoreError::CursorConflict {
                        expected: Box::new(write.expected_cursor().cloned()),
                        actual: Box::new(cursor),
                    });
                }
                return Err(identity_collision(
                    observation_id,
                    retained_digest.clone(),
                    observation.payload_reference().digest().clone(),
                ));
            }
            // The exact refusal marker is already durable. A later stale
            // reader may no longer stand at its original scan frontier, but
            // that does not invalidate the terminal content verdict.
            let _ = self
                .record_refusal_with_coverage(&write, retained_digest, cursor.as_ref())
                .await?;
            return Err(identity_collision(
                observation_id,
                retained_digest.clone(),
                observation.payload_reference().digest().clone(),
            ));
        }
        let pending = batch_state.pending_observation(&observation_id).cloned();
        let existing = preflight.stored_observation(&observation_id);
        let collision = pending
            .as_ref()
            .map(|retained| {
                if retained
                    .payload_reference
                    .eq(observation.payload_reference())
                {
                    ObservationCollisionOutcomeV1::ExactDuplicate
                } else {
                    ObservationCollisionOutcomeV1::IdentityCollision
                }
            })
            .or_else(|| {
                existing.as_ref().map(|retained| {
                    classify_observation_collision(retained.observation(), observation)
                })
            });
        let canonical_payload_revision = pending.is_none()
            && existing.as_ref().is_some_and(|retained| {
                is_canonical_payload_revision_replay(retained.observation(), observation)
            });
        if collision == Some(ObservationCollisionOutcomeV1::IdentityCollision)
            && !canonical_payload_revision
        {
            if pending.is_some() {
                return Err(ObservationStoreError::BatchRequiresScalarFallback {
                    cause: ObservationBatchFallbackCause::IntraBatchIdentityCollision,
                });
            }
            let retained_digest = pending
                .as_ref()
                .map(|retained| retained.payload_reference.digest())
                .or_else(|| {
                    existing
                        .as_ref()
                        .map(|retained| retained.observation().payload_reference().digest())
                });
            let Some(retained_digest) = retained_digest else {
                return Err(ObservationStoreError::Storage {
                    operation: "persist_observation",
                    source: Box::new(std::io::Error::other(
                        "classified collisions always have an existing observation",
                    )),
                });
            };
            if pending.is_none() {
                let cursor = actual_cursor();
                if write.identity_collision_disposition()
                    == ObservationIdentityCollisionDispositionV1::RetryWithAlternateIdentity
                {
                    if matches!(
                        refused_scan_frontier(&write, cursor.as_ref())?,
                        RefusedScanFrontier::NotAtFrontier
                    ) {
                        return Err(ObservationStoreError::CursorConflict {
                            expected: Box::new(write.expected_cursor().cloned()),
                            actual: Box::new(cursor),
                        });
                    }
                    return Err(identity_collision(
                        observation_id,
                        retained_digest.clone(),
                        observation.payload_reference().digest().clone(),
                    ));
                }
                if let Some(fallback) = durable_frontier_owned_by_batch(&known_cursor) {
                    return Err(fallback);
                }
                if let RefusalCoverageOutcome::NotAtFrontier { actual } = self
                    .record_refusal_with_coverage(&write, retained_digest, cursor.as_ref())
                    .await?
                {
                    return Err(ObservationStoreError::CursorConflict {
                        expected: Box::new(write.expected_cursor().cloned()),
                        actual: Box::new(actual),
                    });
                }
            }
            return Err(identity_collision(
                observation_id,
                retained_digest.clone(),
                observation.payload_reference().digest().clone(),
            ));
        }
        if canonical_payload_revision {
            let Some(existing) = existing.as_ref() else {
                return Err(runtime_storage_error(
                    "persist canonical payload revision",
                    "classified revision replay has no retained observation",
                ));
            };
            let identity = observation.identity();
            let actual_cursor = actual_cursor();
            let revision_covered = actual_cursor.as_ref().is_some_and(|cursor| {
                cursor.generation() == identity.generation()
                    && cursor.ordering_domain() == identity.ordering_domain()
                    && cursor.position() >= identity.position().end()
            });
            if revision_covered {
                return Ok(PreparedObservationPersist::ready(
                    ObservationPersistOutcome::CoveredDuplicate(
                        ObservationCommitReceipt::new(
                            existing.sequence(),
                            existing.observation().clone(),
                            write.next_cursor().clone(),
                            existing.retrieval_anchor().clone(),
                            existing.projection_generation().clone(),
                        )?
                        .with_repository_provenance_attachment(
                            existing.repository_provenance_attachment().clone(),
                        )?,
                    ),
                    existing,
                ));
            }
            if let Some(fallback) = durable_frontier_owned_by_batch(&known_cursor) {
                return Err(fallback);
            }
            let mut advance = ObservationCursorAdvance::for_ordering_with_sanitization_receipt(
                identity.source().clone(),
                identity.scope().clone(),
                identity.generation(),
                identity.ordering_domain(),
                write.expected_cursor().cloned(),
                identity.position(),
                ObservationCoverageReason::CanonicalPayloadRevision,
                observation.receipt().clone(),
            )?;
            match (
                write.next_cursor().file_identity(),
                write.next_cursor().resume_fingerprint(),
            ) {
                (Some(file_identity), Some(resume_fingerprint)) => {
                    advance = advance.with_resume_checkpoint(file_identity, resume_fingerprint);
                }
                (None, None) => {}
                _ => {
                    return Err(runtime_storage_error(
                        "persist canonical payload revision",
                        "cursor resume checkpoint is incomplete",
                    ));
                }
            }
            self.advance_source_cursor(advance).await?;
            return Ok(PreparedObservationPersist::ready(
                ObservationPersistOutcome::CoveredDuplicate(
                    ObservationCommitReceipt::new(
                        existing.sequence(),
                        existing.observation().clone(),
                        write.next_cursor().clone(),
                        existing.retrieval_anchor().clone(),
                        existing.projection_generation().clone(),
                    )?
                    .with_repository_provenance_attachment(
                        existing.repository_provenance_attachment().clone(),
                    )?,
                ),
                existing,
            ));
        }
        let same_identity = pending
            .as_ref()
            .is_some_and(|retained| retained.identity.eq(observation.identity()))
            || existing.as_ref().is_some_and(|retained| {
                retained.observation().identity() == observation.identity()
            });
        if same_identity
            && (pending
                .as_ref()
                .is_some_and(|retained| !retained.receipt.eq(observation.receipt()))
                || existing.as_ref().is_some_and(|retained| {
                    retained.observation().receipt() != observation.receipt()
                }))
        {
            if pending.is_some() {
                return Err(ObservationStoreError::BatchRequiresScalarFallback {
                    cause: ObservationBatchFallbackCause::IntraBatchSanitizationReceiptCollision,
                });
            }
            return Err(ObservationStoreError::SanitizationReceiptCollision);
        }
        if batch_state
            .pending_receipt(observation.receipt())
            .is_some_and(|retained| retained != observation.receipt())
        {
            return Err(ObservationStoreError::BatchRequiresScalarFallback {
                cause: ObservationBatchFallbackCause::IntraBatchSanitizationReceiptCollision,
            });
        }
        for alias in write.retrieval_anchor().aliases() {
            if let Some(existing) =
                batch_state.retrieval_anchor_by_alias(observation.scope(), alias)?
                && existing.anchor_id != *write.retrieval_anchor_id()
            {
                if existing.pending {
                    return Err(ObservationStoreError::BatchRequiresScalarFallback {
                        cause:
                            ObservationBatchFallbackCause::IntraBatchRetrievalAnchorAliasCollision,
                    });
                }
                return Err(ObservationStoreError::RetrievalAnchorAliasCollision {
                    alias: Box::new(alias.clone()),
                    existing_anchor_id: Box::new(existing.anchor_id),
                    candidate_anchor_id: Box::new(write.retrieval_anchor_id().clone()),
                });
            }
        }
        let covered_duplicate =
            collision == Some(ObservationCollisionOutcomeV1::ExactDuplicate) && !same_identity;
        if (pending.is_none() && existing.is_none()) || covered_duplicate {
            let actual_cursor = actual_cursor();
            let covered_duplicate_replay =
                covered_duplicate && actual_cursor.as_ref() == Some(write.next_cursor());
            if !covered_duplicate_replay && actual_cursor.as_ref() != write.expected_cursor() {
                return Err(ObservationStoreError::CursorConflict {
                    expected: Box::new(write.expected_cursor().cloned()),
                    actual: Box::new(actual_cursor),
                });
            }
        }
        let existed_exact = same_identity
            && (pending
                .as_ref()
                .is_some_and(|retained| retained.receipt.eq(observation.receipt()))
                || existing.as_ref().is_some_and(|retained| {
                    retained.observation().receipt() == observation.receipt()
                }));
        if existed_exact {
            if pending.is_some() {
                return Ok(PreparedObservationPersist::DeferredExactDuplicate(
                    Box::new(write),
                ));
            }
            let Some(existing) = existing.as_ref() else {
                return Err(ObservationStoreError::Storage {
                    operation: "persist_observation",
                    source: Box::new(std::io::Error::other(
                        "exact duplicate classification requires a stored observation",
                    )),
                });
            };
            return Ok(PreparedObservationPersist::ready(
                ObservationPersistOutcome::ExactDuplicate(existing.commit_receipt().clone()),
                existing,
            ));
        }
        if pending.is_none() && existing.is_none() {
            batch_state.register_pending(&write)?;
        }
        Ok(PreparedObservationPersist::Submit(Box::new(write)))
    }
}

/// Immutable read authority for one bounded admission window.
///
/// The fixed query set removes per-item reader admission while the caller's
/// ordered fold still publishes source cursors in input order. The writer
/// transaction remains the final collision and compare-and-set authority.
struct ObservationPreflightSnapshot {
    admission_refusals: HashMap<(String, String), PayloadDigestV1>,
    stored_observations: HashMap<String, StoredObservation>,
    retrieval_aliases: HashMap<(String, String, String), RetrievalAnchorId>,
    source_cursors: HashMap<(ClaudeSourceIdentityV1, ObservationScopeV1), ClaudeSourceCursorV1>,
}

#[inline(always)]
fn record_observation_snapshot_probe() {
    tracing::trace!(
        target: "tracedecay::observation_snapshot_query",
        "query observation batch snapshot"
    );
    #[cfg(feature = "hotpath")]
    hotpath::gauge!("global_db.observation_batch.snapshot_query_probes").inc(1_u64);
}

#[inline(always)]
fn record_observation_snapshot_row() {
    #[cfg(feature = "hotpath")]
    hotpath::gauge!("global_db.observation_batch.snapshot_rows").inc(1_u64);
}

impl ObservationPreflightSnapshot {
    fn admission_refusal(
        &self,
        observation_id: &CanonicalObservationIdV1,
        refused_digest: &PayloadDigestV1,
    ) -> Option<&PayloadDigestV1> {
        self.admission_refusals.get(&(
            observation_id.as_str().to_owned(),
            refused_digest.as_str().to_owned(),
        ))
    }

    fn stored_observation(
        &self,
        observation_id: &CanonicalObservationIdV1,
    ) -> Option<&StoredObservation> {
        self.stored_observations.get(observation_id.as_str())
    }

    fn source_cursor(
        &self,
        source: &ClaudeSourceIdentityV1,
        scope: &ObservationScopeV1,
    ) -> Option<ClaudeSourceCursorV1> {
        self.source_cursors
            .get(&(source.clone(), scope.clone()))
            .cloned()
    }
}

/// Ordered identities accepted by this batch but not yet visible durably.
/// This mirrors only authorities that later writes in the same transaction
/// can observe; the writer remains the final compare-and-set authority.
struct ObservationBatchState {
    pending_observations: HashMap<String, PendingObservationAuthority>,
    pending_receipts: HashMap<String, SanitizationReceiptV1>,
    retrieval_aliases: HashMap<(String, String, String), BatchAliasAuthority>,
}

#[derive(Clone)]
struct PendingObservationAuthority {
    payload_reference: PayloadReferenceV1,
    identity: ObservationIdentityMaterialV1,
    receipt: SanitizationReceiptV1,
}

#[derive(Clone)]
struct BatchAliasAuthority {
    anchor_id: RetrievalAnchorId,
    pending: bool,
}

impl ObservationBatchState {
    fn from_preflight(preflight: &ObservationPreflightSnapshot) -> Self {
        Self {
            pending_observations: HashMap::new(),
            pending_receipts: HashMap::new(),
            retrieval_aliases: preflight
                .retrieval_aliases
                .iter()
                .map(|(key, anchor_id)| {
                    (
                        key.clone(),
                        BatchAliasAuthority {
                            anchor_id: anchor_id.clone(),
                            pending: false,
                        },
                    )
                })
                .collect(),
        }
    }

    fn pending_observation(
        &self,
        observation_id: &CanonicalObservationIdV1,
    ) -> Option<&PendingObservationAuthority> {
        self.pending_observations.get(observation_id.as_str())
    }

    fn retrieval_anchor_by_alias(
        &self,
        scope: &ObservationScopeV1,
        alias: &tracedecay_domain::NativeAliasV2,
    ) -> ObservationStoreResult<Option<BatchAliasAuthority>> {
        let key = retrieval_alias_key(scope, alias)?;
        Ok(self.retrieval_aliases.get(&key).cloned())
    }

    fn pending_receipt(&self, receipt: &SanitizationReceiptV1) -> Option<&SanitizationReceiptV1> {
        self.pending_receipts
            .get(receipt.receipt().receipt_id().as_str())
    }

    fn register_pending(&mut self, write: &AnchoredObservationWrite) -> ObservationStoreResult<()> {
        for alias in write.retrieval_anchor().aliases() {
            self.retrieval_aliases.insert(
                retrieval_alias_key(write.observation().scope(), alias)?,
                BatchAliasAuthority {
                    anchor_id: write.retrieval_anchor_id().clone(),
                    pending: true,
                },
            );
        }
        self.pending_observations.insert(
            write.observation().observation_id().as_str().to_owned(),
            PendingObservationAuthority {
                payload_reference: write.observation().payload_reference().clone(),
                identity: write.observation().identity().clone(),
                receipt: write.observation().receipt().clone(),
            },
        );
        self.pending_receipts.insert(
            write
                .observation()
                .receipt()
                .receipt()
                .receipt_id()
                .as_str()
                .to_owned(),
            write.observation().receipt().clone(),
        );
        Ok(())
    }
}

#[hotpath::measure(future = true, label = "global_db.observation.query.preflight")]
async fn load_observation_preflight(
    database: &Database,
    writes: &[AnchoredObservationWrite],
) -> ObservationStoreResult<ObservationPreflightSnapshot> {
    const OPERATION: &str = "load observation batch preflight";
    let snapshot = database
        .begin_engine_read_snapshot(OPERATION)
        .await
        .map_err(|error| runtime_storage_error(OPERATION, error))?;
    let admission_refusals =
        read_admission_refusals_from_snapshot(&snapshot, writes, OPERATION).await?;
    let mut seen_observation_ids = HashSet::new();
    let mut observation_ids = Vec::new();
    for write in writes {
        let observation = write.observation();
        let observation_id = observation.observation_id().as_str();
        let refusal_key = (
            observation_id.to_owned(),
            observation.payload_reference().digest().as_str().to_owned(),
        );
        if !admission_refusals.contains_key(&refusal_key)
            && seen_observation_ids.insert(observation_id.to_owned())
        {
            observation_ids.push(observation.observation_id().clone());
        }
    }
    let stored_observations =
        read_stored_observations_from_snapshot(&snapshot, &observation_ids, OPERATION).await?;
    let retrieval_aliases =
        read_retrieval_aliases_from_snapshot(&snapshot, writes, OPERATION).await?;
    let source_cursors = read_source_cursors_from_snapshot(&snapshot, writes, OPERATION).await?;
    snapshot
        .commit()
        .await
        .map_err(|error| runtime_storage_error(OPERATION, error))?;
    Ok(ObservationPreflightSnapshot {
        admission_refusals,
        stored_observations,
        retrieval_aliases,
        source_cursors,
    })
}

async fn read_admission_refusals_from_snapshot(
    snapshot: &DatabaseEngineReadSnapshot,
    writes: &[AnchoredObservationWrite],
    operation: &'static str,
) -> ObservationStoreResult<HashMap<(String, String), PayloadDigestV1>> {
    let requested = writes
        .iter()
        .map(|write| {
            (
                write.observation().observation_id().as_str().to_owned(),
                write
                    .observation()
                    .payload_reference()
                    .digest()
                    .as_str()
                    .to_owned(),
            )
        })
        .collect::<HashSet<_>>()
        .into_iter()
        .map(|(observation_id, refused_digest)| {
            serde_json::json!({
                "observation_id": observation_id,
                "refused_digest": refused_digest,
            })
        })
        .collect::<Vec<_>>();
    let requested = serde_json::to_string(&requested)
        .map_err(|error| runtime_storage_error(operation, error))?;
    record_observation_snapshot_probe();
    let mut rows = snapshot
        .query(
            "SELECT refusal.observation_id, refusal.refused_payload_digest,
                    refusal.retained_payload_digest
             FROM observation_admission_refusals AS refusal
             JOIN json_each(?1) AS requested
               ON refusal.observation_id =
                    json_extract(requested.value, '$.observation_id')
              AND refusal.refused_payload_digest =
                    json_extract(requested.value, '$.refused_digest')",
            [requested],
        )
        .await
        .map_err(|error| runtime_storage_error(operation, error))?;
    let mut refusals = HashMap::new();
    while let Some(row) = rows
        .next()
        .await
        .map_err(|error| runtime_storage_error(operation, error))?
    {
        record_observation_snapshot_row();
        let observation_id = row
            .get::<String>(0)
            .map_err(|error| runtime_storage_error(operation, error))?;
        let refused_digest = row
            .get::<String>(1)
            .map_err(|error| runtime_storage_error(operation, error))?;
        let retained_digest = PayloadDigestV1::new(
            row.get::<String>(2)
                .map_err(|error| runtime_storage_error(operation, error))?,
        )
        .map_err(|error| runtime_storage_error(operation, error))?;
        refusals.insert((observation_id, refused_digest), retained_digest);
    }
    Ok(refusals)
}

async fn read_source_cursors_from_snapshot(
    snapshot: &DatabaseEngineReadSnapshot,
    writes: &[AnchoredObservationWrite],
    operation: &'static str,
) -> ObservationStoreResult<
    HashMap<(ClaudeSourceIdentityV1, ObservationScopeV1), ClaudeSourceCursorV1>,
> {
    let mut requested = HashSet::with_capacity(writes.len());
    for write in writes {
        requested.insert((
            serde_json::to_string(write.observation().source())
                .map_err(|error| runtime_storage_error(operation, error))?,
            serde_json::to_string(write.observation().scope())
                .map_err(|error| runtime_storage_error(operation, error))?,
        ));
    }
    let requested = requested
        .into_iter()
        .map(|(source_json, scope_json)| {
            serde_json::json!({
                "source_json": source_json,
                "scope_json": scope_json,
            })
        })
        .collect::<Vec<_>>();
    let requested = serde_json::to_string(&requested)
        .map_err(|error| runtime_storage_error(operation, error))?;
    record_observation_snapshot_probe();
    let mut rows = snapshot
        .query(
            "SELECT cursor.source_json, cursor.scope_json, cursor.cursor_json
             FROM source_cursors AS cursor
             JOIN json_each(?1) AS requested
               ON cursor.source_json =
                    json_extract(requested.value, '$.source_json')
              AND cursor.scope_json =
                    json_extract(requested.value, '$.scope_json')",
            [requested],
        )
        .await
        .map_err(|error| runtime_storage_error(operation, error))?;
    let mut cursors = HashMap::new();
    while let Some(row) = rows
        .next()
        .await
        .map_err(|error| runtime_storage_error(operation, error))?
    {
        record_observation_snapshot_row();
        let source = decode_json(
            row.get::<String>(0)
                .map_err(|error| runtime_storage_error(operation, error))?,
            operation,
        )?;
        let scope = decode_json(
            row.get::<String>(1)
                .map_err(|error| runtime_storage_error(operation, error))?,
            operation,
        )?;
        let cursor = decode_json(
            row.get::<String>(2)
                .map_err(|error| runtime_storage_error(operation, error))?,
            operation,
        )?;
        cursors.insert((source, scope), cursor);
    }
    Ok(cursors)
}

fn retrieval_alias_key(
    scope: &ObservationScopeV1,
    alias: &tracedecay_domain::NativeAliasV2,
) -> ObservationStoreResult<(String, String, String)> {
    Ok((
        serde_json::to_string(scope)
            .map_err(|error| runtime_storage_error("encode retrieval alias preflight", error))?,
        serde_json::to_string(&alias.kind())
            .map_err(|error| runtime_storage_error("encode retrieval alias preflight", error))?,
        serde_json::to_string(alias.locator_digest())
            .map_err(|error| runtime_storage_error("encode retrieval alias preflight", error))?,
    ))
}

async fn read_retrieval_aliases_from_snapshot(
    snapshot: &DatabaseEngineReadSnapshot,
    writes: &[AnchoredObservationWrite],
    operation: &'static str,
) -> ObservationStoreResult<HashMap<(String, String, String), RetrievalAnchorId>> {
    let mut requested = HashSet::new();
    for write in writes {
        for alias in write.retrieval_anchor().aliases() {
            requested.insert(retrieval_alias_key(write.observation().scope(), alias)?);
        }
    }
    if requested.is_empty() {
        return Ok(HashMap::new());
    }
    let requested = requested
        .into_iter()
        .map(|(scope_json, kind_json, locator_json)| {
            serde_json::json!({
                "scope_json": scope_json,
                "kind_json": kind_json,
                "locator_json": locator_json,
            })
        })
        .collect::<Vec<_>>();
    let requested = serde_json::to_string(&requested)
        .map_err(|error| runtime_storage_error(operation, error))?;
    record_observation_snapshot_probe();
    let mut rows = snapshot
        .query(
            "SELECT alias.owner_json, alias.alias_kind, alias.locator_digest,
                    alias.anchor_id
             FROM retrieval_anchor_aliases AS alias
             JOIN json_each(?1) AS requested
               ON alias.owner_json =
                    json_extract(requested.value, '$.scope_json')
              AND alias.alias_kind =
                    json_extract(requested.value, '$.kind_json')
              AND alias.locator_digest =
                    json_extract(requested.value, '$.locator_json')",
            [requested],
        )
        .await
        .map_err(|error| runtime_storage_error(operation, error))?;
    let mut aliases = HashMap::new();
    while let Some(row) = rows
        .next()
        .await
        .map_err(|error| runtime_storage_error(operation, error))?
    {
        record_observation_snapshot_row();
        let key = (
            row.get::<String>(0)
                .map_err(|error| runtime_storage_error(operation, error))?,
            row.get::<String>(1)
                .map_err(|error| runtime_storage_error(operation, error))?,
            row.get::<String>(2)
                .map_err(|error| runtime_storage_error(operation, error))?,
        );
        let anchor_id = RetrievalAnchorId::new(
            row.get::<String>(3)
                .map_err(|error| runtime_storage_error(operation, error))?,
        )
        .map_err(|error| runtime_storage_error(operation, error))?;
        aliases.insert(key, anchor_id);
    }
    Ok(aliases)
}

const OBSERVATION_BATCH_ROW_PROJECTION: &str =
    "SELECT observation.observation_id, observation.sequence,
            observation.observation_json, observation.committed_cursor_json,
            anchor.anchor_json, anchor.projection_generation,
            repository.availability_json, repository.capture_json,
            repository_anchor.anchor_json, repository.owner_json,
            EXISTS(
                SELECT 1 FROM projection_queue
                WHERE projection_queue.observation_id = observation.observation_id
            )
     FROM observations AS observation
     LEFT JOIN observation_retrieval_anchors AS binding
       ON binding.observation_id = observation.observation_id
     LEFT JOIN retrieval_anchors AS anchor
       ON anchor.anchor_id = binding.anchor_id
     LEFT JOIN observation_repository_provenance AS repository
       ON repository.observation_id = observation.observation_id
     LEFT JOIN retrieval_anchors AS repository_anchor
       ON repository_anchor.anchor_id = repository.retrieval_anchor_id
     JOIN json_each(?1) AS requested
       ON requested.value = observation.observation_id";

async fn read_stored_observations_from_snapshot(
    snapshot: &DatabaseEngineReadSnapshot,
    observation_ids: &[CanonicalObservationIdV1],
    operation: &'static str,
) -> ObservationStoreResult<HashMap<String, StoredObservation>> {
    if observation_ids.is_empty() {
        return Ok(HashMap::new());
    }
    let requested = serde_json::to_string(
        &observation_ids
            .iter()
            .map(CanonicalObservationIdV1::as_str)
            .collect::<HashSet<_>>(),
    )
    .map_err(|error| runtime_storage_error(operation, error))?;
    record_observation_snapshot_probe();
    let mut rows = snapshot
        .query(OBSERVATION_BATCH_ROW_PROJECTION, [requested])
        .await
        .map_err(|error| runtime_storage_error(operation, error))?;
    let mut observations = HashMap::new();
    while let Some(row) = rows
        .next()
        .await
        .map_err(|error| runtime_storage_error(operation, error))?
    {
        record_observation_snapshot_row();
        let observation_id = row
            .get::<String>(0)
            .map_err(|error| runtime_storage_error(operation, error))?;
        let sequence = u64::try_from(
            row.get::<i64>(1)
                .map_err(|error| runtime_storage_error(operation, error))?,
        )
        .map_err(|_| runtime_storage_error(operation, "negative observation sequence"))?;
        let observation: DurableObservationV1 = decode_json(
            row.get::<String>(2)
                .map_err(|error| runtime_storage_error(operation, error))?,
            operation,
        )?;
        if observation.observation_id().as_str() != observation_id {
            return Err(runtime_storage_error(
                operation,
                "observation row identity mismatch",
            ));
        }
        let committed_cursor: ClaudeSourceCursorV1 = decode_json(
            row.get::<String>(3)
                .map_err(|error| runtime_storage_error(operation, error))?,
            operation,
        )?;
        if observation.source() != committed_cursor.source()
            || observation.scope() != committed_cursor.scope()
            || observation.identity().generation() != committed_cursor.generation()
            || observation.identity().ordering_domain() != committed_cursor.ordering_domain()
            || observation.identity().position().end() != committed_cursor.position()
        {
            return Err(runtime_storage_error(
                operation,
                "observation committed cursor binding mismatch",
            ));
        }
        let retrieval_anchor: RetrievalAnchorRecordV2 = decode_json(
            row.get::<Option<String>>(4)
                .map_err(|error| runtime_storage_error(operation, error))?
                .ok_or_else(|| {
                    runtime_storage_error(operation, "observation retrieval anchor is missing")
                })?,
            operation,
        )?;
        let projection_generation = ProjectionGenerationId::new(
            row.get::<Option<String>>(5)
                .map_err(|error| runtime_storage_error(operation, error))?
                .ok_or_else(|| {
                    runtime_storage_error(operation, "observation projection generation is missing")
                })?,
        )
        .map_err(|error| runtime_storage_error(operation, error))?;
        let repository_availability: EvidenceAvailabilityV1<GenerationBoundRepositoryProvenanceV1> =
            decode_json(
                row.get::<Option<String>>(6)
                    .map_err(|error| runtime_storage_error(operation, error))?
                    .ok_or_else(|| {
                        runtime_storage_error(
                            operation,
                            "observation repository provenance is missing",
                        )
                    })?,
                operation,
            )?;
        let repository_capture = row
            .get::<Option<String>>(7)
            .map_err(|error| runtime_storage_error(operation, error))?
            .map(|encoded| decode_json(encoded, operation))
            .transpose()?;
        if repository_availability.value() != repository_capture.as_ref() {
            return Err(runtime_storage_error(
                operation,
                "repository provenance binding mismatch",
            ));
        }
        let repository_anchor = row
            .get::<Option<String>>(8)
            .map_err(|error| runtime_storage_error(operation, error))?
            .map(|encoded| decode_json(encoded, operation))
            .transpose()?;
        let repository_owner = row
            .get::<Option<String>>(9)
            .map_err(|error| runtime_storage_error(operation, error))?;
        let expected_repository_owner = repository_anchor
            .as_ref()
            .map(|anchor: &RetrievalAnchorRecordV2| serde_json::to_string(anchor.owner()))
            .transpose()
            .map_err(|error| runtime_storage_error(operation, error))?;
        if repository_owner != expected_repository_owner {
            return Err(runtime_storage_error(
                operation,
                "observation repository owner binding mismatch",
            ));
        }
        let repository_provenance =
            RepositoryProvenanceAttachmentV1::new(repository_availability, repository_anchor)
                .map_err(|error| runtime_storage_error(operation, error))?;
        let projection_queued = row
            .get::<i64>(10)
            .map_err(|error| runtime_storage_error(operation, error))?
            != 0;
        let stored = stored_observation_from_runtime_row(StoredObservationRowV1 {
            sequence,
            observation,
            committed_cursor,
            retrieval_anchor,
            projection_generation,
            repository_provenance,
            projection_queued,
        })?;
        observations.insert(observation_id, stored);
    }
    Ok(observations)
}

fn decode_json<T: serde::de::DeserializeOwned>(
    encoded: String,
    operation: &'static str,
) -> ObservationStoreResult<T> {
    serde_json::from_str(&encoded).map_err(|error| runtime_storage_error(operation, error))
}

enum PreparedObservationPersist {
    // Both payloads are large and only one is ever live, so both are boxed:
    // leaving either inline makes every value of this enum carry that variant's
    // footprint.
    Ready(Box<ObservationBatchPersistOutcome>),
    Submit(Box<AnchoredObservationWrite>),
    DeferredExactDuplicate(Box<AnchoredObservationWrite>),
}

impl PreparedObservationPersist {
    fn ready(outcome: ObservationPersistOutcome, stored: &StoredObservation) -> Self {
        Self::Ready(Box::new(ObservationBatchPersistOutcome::new(
            outcome,
            Some(stored.clone()),
        )))
    }
}

impl ObservationStore for GlobalDbObservationStore {
    #[hotpath::skip]
    async fn persist_observation(
        &self,
        write: AnchoredObservationWrite,
    ) -> ObservationStoreResult<ObservationPersistOutcome> {
        let mut outcomes = self.persist_observations(vec![write]).await?;
        if outcomes.len() != 1 {
            return Err(runtime_storage_error(
                "persist_observation",
                "batch returned an unexpected outcome count",
            ));
        }
        outcomes
            .pop()
            .map(ObservationBatchPersistOutcome::into_parts)
            .map(|(outcome, _)| outcome)
            .ok_or_else(|| {
                runtime_storage_error("persist_observation", "batch returned no outcome")
            })
    }

    #[hotpath::measure(future = true, label = "global_db.observation.persist.batch")]
    async fn persist_observations(
        &self,
        writes: Vec<AnchoredObservationWrite>,
    ) -> ObservationStoreResult<Vec<ObservationBatchPersistOutcome>> {
        if writes.is_empty() {
            return Ok(Vec::new());
        }
        let span = tracing::info_span!(
            target: "tracedecay::observation_admission_work",
            "observation.admission.batch",
            writes = writes.len()
        );
        async move {
            crate::hotpath_observe::record_transaction_rows(1);
            let preflight = load_observation_preflight(&self.database, &writes).await?;
            let mut batch_state = ObservationBatchState::from_preflight(&preflight);
            let mut published_cursors =
                HashMap::<(ClaudeSourceIdentityV1, ObservationScopeV1), ClaudeSourceCursorV1>::new(
                );
            let mut prepared = Vec::with_capacity(writes.len());
            for write in writes {
                let key = (
                    write.observation().source().clone(),
                    write.observation().scope().clone(),
                );
                let known_cursor = published_cursors.get(&key).cloned().map(Some);
                let next_cursor = write.next_cursor().clone();
                let item = self
                    .prepare_observation_persist(write, &preflight, &mut batch_state, known_cursor)
                    .await?;
                published_cursors.insert(key, next_cursor);
                prepared.push(item);
            }
            let mut outcomes: Vec<Option<ObservationBatchPersistOutcome>> =
                Vec::with_capacity(prepared.len());
            let mut submits = Vec::new();
            let mut deferred_exact_duplicates = Vec::new();
            for item in prepared {
                match item {
                    PreparedObservationPersist::Ready(outcome) => outcomes.push(Some(*outcome)),
                    PreparedObservationPersist::Submit(write) => {
                        submits.push((outcomes.len(), *write));
                        outcomes.push(None);
                    }
                    PreparedObservationPersist::DeferredExactDuplicate(write) => {
                        deferred_exact_duplicates.push((outcomes.len(), *write));
                        outcomes.push(None);
                    }
                }
            }
            if !submits.is_empty() {
                let submitted = submit_observation_writes(
                    &self.database,
                    &self.runtime,
                    submits,
                    deferred_exact_duplicates,
                )
                .await?;
                for (slot, outcome) in submitted {
                    outcomes[slot] = Some(outcome);
                }
            } else if !deferred_exact_duplicates.is_empty() {
                return Err(runtime_storage_error(
                    "persist_observations",
                    "deferred duplicate has no preceding batch submission",
                ));
            }
            outcomes
                .into_iter()
                .map(|outcome| {
                    outcome.ok_or_else(|| {
                        runtime_storage_error(
                            "persist_observations",
                            "batch slot was not settled by writer authority",
                        )
                    })
                })
                .collect()
        }
        .instrument(span)
        .await
    }

    #[hotpath::skip]
    async fn get_source_cursor(
        &self,
        source: &ClaudeSourceIdentityV1,
        scope: &ObservationScopeV1,
    ) -> ObservationStoreResult<Option<ClaudeSourceCursorV1>> {
        read_runtime_source_cursor(&self.runtime, source, scope)
    }

    #[hotpath::skip]
    async fn advance_source_cursor(
        &self,
        advance: ObservationCursorAdvance,
    ) -> ObservationStoreResult<CursorAdvanceOutcome> {
        let runtime = &self.runtime;
        let actual_cursor = read_runtime_source_cursor(
            runtime,
            advance.next_cursor().source(),
            advance.next_cursor().scope(),
        )?;
        let existed_at_next = actual_cursor.as_ref() == Some(advance.next_cursor());
        if !existed_at_next && actual_cursor.as_ref() != advance.expected_cursor() {
            return Err(ObservationStoreError::CursorConflict {
                expected: Box::new(advance.expected_cursor().cloned()),
                actual: Box::new(actual_cursor),
            });
        }
        let identity = serde_json::json!({
            "source": advance.next_cursor().source(),
            "scope": advance.next_cursor().scope(),
            "coverage": advance.coverage(),
        });
        let key = format!("cursor.{}", canonical_runtime_digest(&identity)?);
        let payload = RepositoryWritePayloadV1::ObservationCursorAdvance(Box::new(advance));
        let (command_bytes, command_digest) = canonical_json_bytes_and_sha256(
            &runtime_command_value(&payload)?,
        )
        .map_err(|error| {
            runtime_storage_error("advance observation source cursor", error.to_string())
        })?;
        let outcome = submit_runtime_write(
            runtime,
            payload,
            command_digest,
            command_bytes.len(),
            key,
            "advance observation source cursor",
        )
        .await;
        let outcome = match outcome {
            Err(error @ ObservationStoreError::CursorAdvanceLedgerDisagreement { .. }) => {
                return Err(error);
            }
            Err(_) if existed_at_next => {
                return Err(ObservationStoreError::CursorAdvanceCollision);
            }
            outcome => outcome,
        };
        match outcome? {
            RuntimeSubmitOutcomeV1::Committed { .. }
            | RuntimeSubmitOutcomeV1::CommittedAfterCancellation { .. }
                if existed_at_next =>
            {
                Ok(CursorAdvanceOutcome::ExactDuplicate)
            }
            RuntimeSubmitOutcomeV1::Committed { .. }
            | RuntimeSubmitOutcomeV1::CommittedAfterCancellation { .. } => {
                Ok(CursorAdvanceOutcome::Committed)
            }
            RuntimeSubmitOutcomeV1::ExactReplay { .. } => Ok(CursorAdvanceOutcome::ExactDuplicate),
            RuntimeSubmitOutcomeV1::IdempotencyConflict { .. } => {
                Err(ObservationStoreError::CursorAdvanceCollision)
            }
            other => Err(runtime_storage_error(
                "advance observation source cursor",
                format!("runtime rejected cursor advance: {other:?}"),
            )),
        }
    }

    #[hotpath::skip]
    async fn get_observation(
        &self,
        observation_id: &CanonicalObservationIdV1,
    ) -> ObservationStoreResult<Option<StoredObservation>> {
        read_runtime_stored_observation(&self.runtime, observation_id)
    }

    #[hotpath::skip]
    async fn replay_observations(
        &self,
        request: ObservationReplayRequest,
    ) -> ObservationStoreResult<Vec<StoredObservation>> {
        let limit = u16::try_from(request.limit()).map_err(|_| {
            runtime_storage_error(
                "replay observations",
                "observation replay limit exceeds runtime contract",
            )
        })?;
        match dispatch_runtime_observation_read(
            &self.runtime,
            ObservationReadOperationV1::Replay {
                after_sequence: request.after_sequence(),
                limit,
            },
        )? {
            ObservationReadResultV1::Replay(rows) => rows
                .into_iter()
                .map(stored_observation_from_runtime_row)
                .collect(),
            _ => Err(runtime_storage_error(
                "replay observations",
                "runtime returned a mismatched observation read result",
            )),
        }
    }
}

struct RuntimeObservationProbe {
    cancellation: RuntimeCancellationIdentityV1,
    deadline: RuntimeDeadlineV1,
    commit_started: AtomicBool,
}

impl RuntimeObservationProbe {
    fn from_control(control: &RuntimeRequestControlV1) -> Self {
        Self {
            cancellation: control.cancellation.clone(),
            deadline: control.deadline.clone(),
            commit_started: AtomicBool::new(false),
        }
    }
}

impl RuntimeRequestProbeV1 for RuntimeObservationProbe {
    fn cancellation_identity(&self) -> &RuntimeCancellationIdentityV1 {
        &self.cancellation
    }

    fn deadline_identity(&self) -> &RuntimeDeadlineV1 {
        &self.deadline
    }

    fn interruption(&self) -> Option<RuntimeInterruptionV1> {
        None
    }

    fn try_begin_commit(&self) -> bool {
        // Observation submits are never externally cancelled (interruption is
        // always None), so commit arbitration is only the at-most-once gate.
        self.commit_started
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
    }
}

fn identity_collision(
    observation_id: CanonicalObservationIdV1,
    existing_digest: PayloadDigestV1,
    candidate_digest: PayloadDigestV1,
) -> ObservationStoreError {
    ObservationStoreError::ObservationCollision {
        observation_id: Box::new(observation_id),
        existing_digest: Box::new(existing_digest),
        candidate_digest: Box::new(candidate_digest),
        outcome: ObservationCollisionOutcomeV1::IdentityCollision,
    }
}

fn dispatch_runtime_observation_read(
    runtime: &DatabaseRuntimeClientV1,
    operation: ObservationReadOperationV1,
) -> ObservationStoreResult<ObservationReadResultV1> {
    let (command_bytes, command_digest) = canonical_json_bytes_and_sha256(&operation)
        .map_err(|error| runtime_storage_error("build observation runtime read", error))?;
    let suffix = command_digest
        .as_str()
        .strip_prefix("sha256:")
        .ok_or_else(|| {
            runtime_storage_error(
                "build observation runtime read",
                "canonical digest prefix is invalid",
            )
        })?;
    let admission_bytes = command_bytes.len();
    let requested_at = now_micros();
    let control = RuntimeRequestControlV1 {
        requested_at,
        deadline: RuntimeDeadlineV1 {
            deadline_id: RuntimeDeadlineIdV1::new(format!(
                "deadline.host-observation-read.{suffix}"
            ))
            .map_err(|error| runtime_storage_error("build observation runtime read", error))?,
        },
        cancellation: RuntimeCancellationIdentityV1 {
            cancellation_id: RuntimeCancellationIdV1::new(format!(
                "cancellation.host-observation-read.{suffix}"
            ))
            .map_err(|error| runtime_storage_error("build observation runtime read", error))?,
            generation: 1,
        },
    };
    let request = RuntimeReadRequestV1::new(
        runtime.binding().clone(),
        ConsistencyModeV1::LatestAvailable,
        RuntimeReadOperationV1::Repository {
            op: RepositoryReadOperationV1::Project(ProjectReadOperationV1::Observation(operation)),
        },
        OperationPriorityV1::Foreground,
        u64::try_from(admission_bytes).unwrap_or(u64::MAX).max(1),
        control,
    )
    .map_err(|error| runtime_storage_error("build observation runtime read", error))?;
    let probe = RuntimeObservationProbe::from_control(request.control());
    tracing::trace!(
        target: "tracedecay::observation_admission_work",
        work = "runtime_command",
        "dispatch observation runtime read"
    );
    let outcome = runtime.dispatch_read(request, &probe).map_err(|error| {
        runtime_storage_error(
            "dispatch observation runtime read",
            format!("runtime read failed: {error:?}"),
        )
    })?;
    if !matches!(
        outcome.coverage(),
        RuntimeReadCoverageV1::Latest { .. } | RuntimeReadCoverageV1::Complete { .. }
    ) {
        return Err(runtime_storage_error(
            "dispatch observation runtime read",
            "runtime did not provide current observation coverage",
        ));
    }
    match outcome.value() {
        Some(RuntimeReadResultV1::Repository {
            result: RepositoryReadResultV1::Project(project),
        }) => match project.as_ref() {
            ProjectReadResultV1::Observation(result) => Ok(result.clone()),
            _ => Err(runtime_storage_error(
                "dispatch observation runtime read",
                "runtime returned a mismatched project read result",
            )),
        },
        _ => Err(runtime_storage_error(
            "dispatch observation runtime read",
            "runtime returned a mismatched read result",
        )),
    }
}

fn stored_observation_from_runtime_row(
    row: StoredObservationRowV1,
) -> ObservationStoreResult<StoredObservation> {
    let projection_status = if row.projection_queued {
        ObservationProjectionStatus::Queued
    } else {
        ObservationProjectionStatus::NotQueued
    };
    let receipt = ObservationCommitReceipt::new(
        row.sequence,
        row.observation,
        row.committed_cursor,
        row.retrieval_anchor,
        row.projection_generation,
    )?
    .with_repository_provenance_attachment(row.repository_provenance)?;
    Ok(StoredObservation::from_commit_receipt(
        receipt,
        projection_status,
    ))
}

fn read_runtime_source_cursor(
    runtime: &DatabaseRuntimeClientV1,
    source: &ClaudeSourceIdentityV1,
    scope: &ObservationScopeV1,
) -> ObservationStoreResult<Option<ClaudeSourceCursorV1>> {
    match dispatch_runtime_observation_read(
        runtime,
        ObservationReadOperationV1::SourceCursor {
            source: source.clone(),
            scope: scope.clone(),
        },
    )? {
        ObservationReadResultV1::SourceCursor(cursor) => Ok(cursor),
        _ => Err(runtime_storage_error(
            "read observation source cursor",
            "runtime returned a mismatched observation read result",
        )),
    }
}

/// The typed cursor advance for a refused candidate standing at the
/// sequential scan frontier: the durable cursor has NOT covered its range and
/// the caller's expected cursor matches the durable one. Generation values
/// are opaque source identities, not ordered counters.
///
/// `NotAtFrontier` is the not-at-frontier verdict — a covered replay or a stale
/// expected view — and leaves every ledger untouched. Gaps and generation
/// jumps never reach the advance constructor here: `ObservationWrite::new`
/// already validated that this write's expected→next cursor transition
/// covers the candidate range, which is exactly the transition the advance
/// re-derives from the same identity, expected cursor, and range. A
/// construction failure is therefore a contract violation, and it surfaces
/// as the typed store error — silently answering "not at the scan frontier"
/// would record no coverage and leave the refused record re-read forever.
enum RefusedScanFrontier {
    AtFrontier(Box<ObservationCursorAdvance>),
    NotAtFrontier,
}

enum RefusalCoverageOutcome {
    Recorded,
    NotAtFrontier {
        actual: Option<ClaudeSourceCursorV1>,
    },
}

/// Whether an earlier member of this batch already published the source cursor
/// this write would compare-and-set against.
///
/// The collision paths below read the *durable* frontier, but a published
/// batch cursor only becomes durable when the batch submits. Replaying the
/// batch as scalar writes lets each earlier write land first, instead of
/// refusing the whole window as a cursor conflict and wedging the frontier.
fn durable_frontier_owned_by_batch(
    known_cursor: &Option<Option<ClaudeSourceCursorV1>>,
) -> Option<ObservationStoreError> {
    known_cursor
        .is_some()
        .then_some(ObservationStoreError::BatchRequiresScalarFallback {
            cause: ObservationBatchFallbackCause::IntraBatchDurableFrontier,
        })
}

fn refused_scan_frontier(
    write: &AnchoredObservationWrite,
    actual_cursor: Option<&ClaudeSourceCursorV1>,
) -> ObservationStoreResult<RefusedScanFrontier> {
    let identity = write.observation().identity();
    let candidate_covered = actual_cursor.is_some_and(|cursor| {
        cursor.generation() == identity.generation()
            && cursor.ordering_domain() == identity.ordering_domain()
            && cursor.position() >= identity.position().end()
    });
    if candidate_covered || actual_cursor != write.expected_cursor() {
        return Ok(RefusedScanFrontier::NotAtFrontier);
    }
    ObservationCursorAdvance::for_ordering(
        identity.source().clone(),
        identity.scope().clone(),
        identity.generation(),
        identity.ordering_domain(),
        write.expected_cursor().cloned(),
        identity.position(),
        ObservationCoverageReason::ObservationIdentityCollision,
    )
    .map(Box::new)
    .map(RefusedScanFrontier::AtFrontier)
}

fn read_runtime_stored_observation(
    runtime: &DatabaseRuntimeClientV1,
    observation_id: &CanonicalObservationIdV1,
) -> ObservationStoreResult<Option<StoredObservation>> {
    match dispatch_runtime_observation_read(
        runtime,
        ObservationReadOperationV1::Observation {
            observation_id: observation_id.clone(),
        },
    )? {
        ObservationReadResultV1::Observation(row) => match *row {
            Some(row) => stored_observation_from_runtime_row(row).map(Some),
            None => Ok(None),
        },
        _ => Err(runtime_storage_error(
            "read observation",
            "runtime returned a mismatched observation read result",
        )),
    }
}

async fn submit_observation_writes(
    database: &Database,
    runtime: &DatabaseRuntimeClientV1,
    writes: Vec<(usize, AnchoredObservationWrite)>,
    deferred_exact_duplicates: Vec<(usize, AnchoredObservationWrite)>,
) -> ObservationStoreResult<Vec<(usize, ObservationBatchPersistOutcome)>> {
    let admitted_at = now_micros();
    let priority = if writes.len() == 1 {
        OperationPriorityV1::Foreground
    } else {
        OperationPriorityV1::Background
    };
    let command = serde_json::json!({
        "kind": "observation_batch",
        "writes": writes
            .iter()
            .map(|(_, write)| runtime_observation_command(write))
            .collect::<Vec<_>>(),
    });
    let (command_bytes, command_digest) =
        canonical_json_bytes_and_sha256(&command).map_err(|error| {
            runtime_storage_error("derive observation runtime identity", error.to_string())
        })?;
    let digest_suffix = runtime_digest_suffix(&command_digest)?;
    let metadata = observation_submit_metadata(
        runtime,
        SubmitCommandIdentity {
            digest_suffix,
            command_digest: command_digest.as_str(),
            command_bytes: command_bytes.len(),
        },
        format!("observation.batch.{digest_suffix}"),
        admitted_at,
        priority,
        "submit observation batch",
    )?;
    let compatibility = RuntimeBatchCompatibilityV1::from_operation(&metadata)
        .map_err(|error| runtime_storage_error("submit observation batch", error.to_string()))?;
    let transaction_id = RuntimeTransactionIdV1::new(format!(
        "transaction.observation.admission.batch.{digest_suffix}"
    ))
    .map_err(|error| runtime_storage_error("submit observation batch", error.to_string()))?;
    let transaction_scope = RuntimeTransactionScopeV1 {
        transaction_id,
        compatibility,
        opened_at: admitted_at,
    };
    let submitted_writes = writes
        .iter()
        .map(|(_, write)| write.clone())
        .collect::<Vec<_>>()
        .into_boxed_slice();
    let outcome = dispatch_runtime_submit(
        runtime,
        RepositoryWritePayloadV1::ObservationBatch(submitted_writes),
        metadata,
        transaction_scope,
        "submit observation batch",
    )
    .await?;
    const READBACK_OPERATION: &str = "read committed observation batch";
    let observation_ids = writes
        .iter()
        .chain(deferred_exact_duplicates.iter())
        .map(|(_, write)| write.observation().observation_id().clone())
        .collect::<Vec<_>>();
    let snapshot = database
        .begin_engine_read_snapshot(READBACK_OPERATION)
        .await
        .map_err(|error| runtime_storage_error(READBACK_OPERATION, error))?;
    let stored =
        read_stored_observations_from_snapshot(&snapshot, &observation_ids, READBACK_OPERATION)
            .await?;
    snapshot
        .commit()
        .await
        .map_err(|error| runtime_storage_error(READBACK_OPERATION, error))?;
    let mut outcomes = writes
        .into_iter()
        .map(|(slot, write)| {
            let retained = stored
                .get(write.observation().observation_id().as_str())
                .ok_or_else(|| {
                    runtime_storage_error("read committed observation", "row unavailable")
                })?;
            persist_outcome_from_submit(
                retained,
                write.observation(),
                write.next_cursor().clone(),
                outcome.clone(),
            )
            .map(|outcome| {
                (
                    slot,
                    ObservationBatchPersistOutcome::new(outcome, Some(retained.clone())),
                )
            })
        })
        .collect::<ObservationStoreResult<Vec<_>>>()?;
    for (slot, write) in deferred_exact_duplicates {
        let retained = stored
            .get(write.observation().observation_id().as_str())
            .ok_or_else(|| {
                runtime_storage_error("read committed observation", "row unavailable")
            })?;
        outcomes.push((
            slot,
            ObservationBatchPersistOutcome::new(
                ObservationPersistOutcome::ExactDuplicate(retained.commit_receipt().clone()),
                Some(retained.clone()),
            ),
        ));
    }
    Ok(outcomes)
}

fn persist_outcome_from_submit(
    stored: &StoredObservation,
    candidate: &DurableObservationV1,
    candidate_cursor: ClaudeSourceCursorV1,
    outcome: RuntimeSubmitOutcomeV1,
) -> ObservationStoreResult<ObservationPersistOutcome> {
    #[cfg(tracedecay_observation_fault_harness)]
    tracedecay_store::fault_harness::wait_at_observation_persist_barrier(
        tracedecay_store::fault_harness::ObservationPersistBarrierStageV1::PostCommitPreAck,
        candidate.source().session_id().as_str(),
    )
    .map_err(|(operation, detail)| runtime_storage_error(operation, detail))?;
    let receipt = stored.commit_receipt().clone();
    match outcome {
        RuntimeSubmitOutcomeV1::Committed { .. }
        | RuntimeSubmitOutcomeV1::CommittedAfterCancellation { .. }
        | RuntimeSubmitOutcomeV1::ExactReplay { .. }
            if stored.observation().identity() != candidate.identity()
                && classify_observation_collision(stored.observation(), candidate)
                    == ObservationCollisionOutcomeV1::ExactDuplicate =>
        {
            Ok(ObservationPersistOutcome::CoveredDuplicate(
                ObservationCommitReceipt::new(
                    stored.sequence(),
                    stored.observation().clone(),
                    candidate_cursor,
                    stored.retrieval_anchor().clone(),
                    stored.projection_generation().clone(),
                )?
                .with_repository_provenance_attachment(
                    stored.repository_provenance_attachment().clone(),
                )?,
            ))
        }
        RuntimeSubmitOutcomeV1::Committed { .. }
        | RuntimeSubmitOutcomeV1::CommittedAfterCancellation { .. } => {
            Ok(ObservationPersistOutcome::Committed(receipt))
        }
        RuntimeSubmitOutcomeV1::ExactReplay { .. } => {
            Ok(ObservationPersistOutcome::ExactDuplicate(receipt))
        }
        other => Err(runtime_storage_error(
            "submit anchored observation",
            format!("runtime rejected observation write: {other:?}"),
        )),
    }
}

/// One canonical command's identity: the digest, the short suffix derived from
/// it, and its encoded size. All three come from a single
/// `canonical_json_bytes_and_sha256` and are always consumed together.
#[derive(Clone, Copy)]
struct SubmitCommandIdentity<'command> {
    digest_suffix: &'command str,
    command_digest: &'command str,
    command_bytes: usize,
}

fn observation_submit_metadata(
    runtime: &DatabaseRuntimeClientV1,
    command: SubmitCommandIdentity<'_>,
    idempotency_key: String,
    admitted_at: tracedecay_domain::UtcMicros,
    priority: OperationPriorityV1,
    operation: &'static str,
) -> ObservationStoreResult<StoreOperationMetadataV1> {
    let binding = runtime.binding();
    Ok(StoreOperationMetadataV1 {
        operation_id: StoreOperationIdV1::new(format!(
            "operation.host-observation.{}",
            command.digest_suffix
        ))
        .map_err(|error| runtime_storage_error(operation, error.to_string()))?,
        client_id: StoreClientIdV1::new("client.host-admission")
            .map_err(|error| runtime_storage_error(operation, error.to_string()))?,
        shard_id: binding.shard_id.clone(),
        incarnation: binding.incarnation,
        authority_epoch: binding.authority_epoch,
        idempotency: IdempotencyIdentityV1 {
            key: StoreIdempotencyKeyV1::new(idempotency_key)
                .map_err(|error| runtime_storage_error(operation, error.to_string()))?,
            command_digest: CommandDigestV1::new(command.command_digest)
                .map_err(|error| runtime_storage_error(operation, error.to_string()))?,
        },
        durability: DurabilityClassV1::Full,
        priority,
        admission_bytes: u64::try_from(command.command_bytes)
            .unwrap_or(u64::MAX)
            .max(1),
        admitted_at,
    })
}

async fn submit_runtime_write(
    runtime: &DatabaseRuntimeClientV1,
    payload: RepositoryWritePayloadV1,
    command_digest: ManifestDigest,
    command_bytes: usize,
    idempotency_key: String,
    operation: &'static str,
) -> ObservationStoreResult<RuntimeSubmitOutcomeV1> {
    let digest_suffix = runtime_digest_suffix(&command_digest)?;
    let admitted_at = now_micros();
    let metadata = observation_submit_metadata(
        runtime,
        SubmitCommandIdentity {
            digest_suffix,
            command_digest: command_digest.as_str(),
            command_bytes,
        },
        idempotency_key,
        admitted_at,
        OperationPriorityV1::Foreground,
        operation,
    )?;
    let compatibility = RuntimeBatchCompatibilityV1::from_operation(&metadata)
        .map_err(|error| runtime_storage_error(operation, error.to_string()))?;
    let transaction_scope = RuntimeTransactionScopeV1 {
        transaction_id: RuntimeTransactionIdV1::new(format!(
            "transaction.{}",
            metadata.operation_id.as_str()
        ))
        .map_err(|error| runtime_storage_error(operation, error.to_string()))?,
        compatibility,
        opened_at: admitted_at,
    };
    dispatch_runtime_submit(runtime, payload, metadata, transaction_scope, operation).await
}

async fn dispatch_runtime_submit(
    runtime: &DatabaseRuntimeClientV1,
    payload: RepositoryWritePayloadV1,
    metadata: StoreOperationMetadataV1,
    transaction_scope: RuntimeTransactionScopeV1,
    operation: &'static str,
) -> ObservationStoreResult<RuntimeSubmitOutcomeV1> {
    let digest_suffix = metadata
        .idempotency
        .command_digest
        .as_str()
        .strip_prefix("sha256:")
        .ok_or_else(|| runtime_storage_error(operation, "canonical digest prefix is invalid"))?;
    let deadline = RuntimeDeadlineV1 {
        deadline_id: RuntimeDeadlineIdV1::new(format!("deadline.{digest_suffix}"))
            .map_err(|error| runtime_storage_error(operation, error.to_string()))?,
    };
    let cancellation = RuntimeCancellationIdentityV1 {
        cancellation_id: RuntimeCancellationIdV1::new(format!("cancellation.{digest_suffix}"))
            .map_err(|error| runtime_storage_error(operation, error.to_string()))?,
        generation: 1,
    };
    let control = RuntimeRequestControlV1 {
        requested_at: metadata.admitted_at,
        deadline: deadline.clone(),
        cancellation: cancellation.clone(),
    };
    let request = RuntimeSubmitRequestV1::new(
        RepositoryOperationEnvelopeV1 { metadata, payload },
        transaction_scope,
        control,
    )
    .map_err(|error| runtime_storage_error(operation, error.to_string()))?;
    tracing::trace!(
        target: "tracedecay::observation_admission_work",
        work = "runtime_command",
        "dispatch observation runtime submit"
    );
    runtime
        .dispatch_submit(
            request,
            Arc::new(RuntimeObservationProbe {
                cancellation,
                deadline,
                commit_started: AtomicBool::new(false),
            }),
        )
        .await
        .map_err(|error| map_observation_submit_error(operation, error))
}

fn map_observation_submit_error(
    operation: &'static str,
    error: StoreRuntimeRegistryFailure,
) -> ObservationStoreError {
    match error {
        StoreRuntimeRegistryFailure::StorageRuntime(error) => match *error {
            StorageRuntimeErrorV1::ObservationSourceCursorConflict { expected, actual } => {
                ObservationStoreError::CursorConflict { expected, actual }
            }
            StorageRuntimeErrorV1::ObservationCursorAdvanceLedgerDisagreement { disagreement } => {
                ObservationStoreError::CursorAdvanceLedgerDisagreement { disagreement }
            }
            error => runtime_storage_error(operation, error.to_string()),
        },
        error => runtime_storage_error(operation, format!("{error:?}")),
    }
}

fn runtime_command_value(
    payload: &RepositoryWritePayloadV1,
) -> ObservationStoreResult<serde_json::Value> {
    match payload {
        RepositoryWritePayloadV1::Observation(write) => Ok(runtime_observation_command(write)),
        RepositoryWritePayloadV1::ObservationBatch(writes) => Ok(serde_json::json!({
            "kind": "observation_batch",
            "writes": writes
                .iter()
                .map(runtime_observation_command)
                .collect::<Vec<_>>(),
        })),
        RepositoryWritePayloadV1::ObservationCursorAdvance(advance) => Ok(serde_json::json!({
            "kind": "observation_cursor_advance",
            "expected_cursor": advance.expected_cursor(),
            "next_cursor": advance.next_cursor(),
            "coverage": advance.coverage(),
            "reason": advance.reason().as_str(),
            "sanitization_receipt": advance.sanitization_receipt(),
        })),
        _ => Err(runtime_storage_error(
            "build observation runtime request",
            "payload is not owned by the observation authority",
        )),
    }
}

fn runtime_observation_command(write: &AnchoredObservationWrite) -> serde_json::Value {
    serde_json::json!({
        "kind": "observation",
        "observation": write.observation(),
        "expected_cursor": write.expected_cursor(),
        "next_cursor": write.next_cursor(),
        "retrieval_anchor": write.retrieval_anchor(),
        "projection_generation": write.projection_generation(),
        "repository_provenance": write.repository_provenance_attachment(),
    })
}

fn canonical_runtime_digest(value: &serde_json::Value) -> ObservationStoreResult<String> {
    let digest = canonical_sha256(value).map_err(|error| {
        runtime_storage_error("derive observation runtime identity", error.to_string())
    })?;
    runtime_digest_suffix(&digest).map(str::to_owned)
}

fn runtime_digest_suffix(digest: &ManifestDigest) -> ObservationStoreResult<&str> {
    digest.as_str().strip_prefix("sha256:").ok_or_else(|| {
        runtime_storage_error(
            "derive observation runtime identity",
            "canonical digest prefix is invalid",
        )
    })
}

fn runtime_storage_error(
    operation: &'static str,
    message: impl std::fmt::Display,
) -> ObservationStoreError {
    ObservationStoreError::Storage {
        operation,
        source: Box::new(std::io::Error::other(message.to_string())),
    }
}

impl ObservationProjectionStore for GlobalDbObservationStore {
    #[hotpath::skip]
    async fn next_queued_observation(
        &self,
    ) -> ProjectionStoreResult<Option<CanonicalObservationIdV1>> {
        match dispatch_runtime_observation_read(
            &self.runtime,
            ObservationReadOperationV1::NextQueuedProjection {
                now_micros: now_micros().0,
            },
        )
        .map_err(projection_runtime_error)?
        {
            ObservationReadResultV1::NextQueuedProjection(observation_id) => Ok(observation_id),
            _ => Err(projection_runtime_error(runtime_storage_error(
                "read next queued observation",
                "runtime returned a mismatched observation read result",
            ))),
        }
    }

    #[hotpath::skip]
    async fn project_observation(
        &self,
        observation_id: &CanonicalObservationIdV1,
    ) -> ProjectionStoreResult<ProjectionPersistOutcome> {
        crate::project_observation(&self.database, observation_id).await
    }

    #[hotpath::skip]
    async fn project_queued_observations(
        &self,
        max: usize,
    ) -> ProjectionStoreResult<Option<tracedecay_store::ProjectionDrainBatch>> {
        crate::project_queued_observations(&self.database, max)
            .await
            .map(Some)
    }

    #[hotpath::skip]
    async fn projection_checkpoint(&self) -> ProjectionStoreResult<ProjectionCheckpoint> {
        match dispatch_runtime_observation_read(
            &self.runtime,
            ObservationReadOperationV1::ProjectionCheckpoint,
        )
        .map_err(projection_runtime_error)?
        {
            ObservationReadResultV1::ProjectionCheckpoint(sequence) => {
                Ok(ProjectionCheckpoint::new(sequence))
            }
            _ => Err(projection_runtime_error(runtime_storage_error(
                "read observation projection checkpoint",
                "runtime returned a mismatched observation read result",
            ))),
        }
    }

    #[hotpath::skip]
    async fn rebuild_projection(
        &self,
        frontier_sequence: u64,
    ) -> ProjectionStoreResult<ProjectionRebuildOutcome> {
        crate::rebuild_projection(&self.database, frontier_sequence).await
    }
}

fn projection_runtime_error(
    error: ObservationStoreError,
) -> tracedecay_store::ProjectionStoreError {
    tracedecay_store::ProjectionStoreError::Storage {
        operation: "dispatch observation projection runtime operation",
        source: Box::new(error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn adapter_contains_only_guarded_database_client() {
        fn assert_exact_fields(store: &GlobalDbObservationStore) {
            let GlobalDbObservationStore {
                database: _,
                runtime: _,
            } = store;
        }

        let _ = assert_exact_fields;
    }
}
