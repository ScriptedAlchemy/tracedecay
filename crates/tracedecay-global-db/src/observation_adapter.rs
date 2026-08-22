use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use tracedecay_application::clock::now_micros;

use tracedecay_domain::{
    CanonicalObservationIdV1, ClaudeSourceCursorV1, ClaudeSourceIdentityV1,
    ObservationCollisionOutcomeV1, ObservationScopeV1, PayloadDigestV1, canonical_sha256,
    classify_observation_collision, is_canonical_payload_revision_replay,
};
use tracedecay_store::observation::{
    CursorAdvanceOutcome, ObservationCoverageReason, ObservationCursorAdvance,
};
use tracedecay_store::{
    AnchoredObservationWrite, CommandDigestV1, ConsistencyModeV1, DurabilityClassV1,
    IdempotencyIdentityV1, ObservationCommitReceipt, ObservationPersistOutcome,
    ObservationProjectionStatus, ObservationProjectionStore, ObservationReadOperationV1,
    ObservationReadResultV1, ObservationReplayRequest, ObservationStore, ObservationStoreError,
    ObservationStoreResult, OperationPriorityV1, ProjectReadOperationV1, ProjectReadResultV1,
    ProjectionCheckpoint, ProjectionPersistOutcome, ProjectionPredecessorConvergence,
    ProjectionRebuildOutcome, ProjectionStoreResult, RepositoryOperationEnvelopeV1,
    RepositoryReadOperationV1, RepositoryReadResultV1, RepositoryWritePayloadV1,
    RuntimeBatchCompatibilityV1, RuntimeCancellationIdV1, RuntimeCancellationIdentityV1,
    RuntimeDeadlineIdV1, RuntimeDeadlineV1, RuntimeInterruptionV1, RuntimeReadCoverageV1,
    RuntimeReadOperationV1, RuntimeReadRequestV1, RuntimeReadResultV1, RuntimeRequestControlV1,
    RuntimeRequestProbeV1, RuntimeSubmitOutcomeV1, RuntimeSubmitRequestV1, RuntimeTransactionIdV1,
    RuntimeTransactionScopeV1, StoreClientIdV1, StoreIdempotencyKeyV1, StoreOperationIdV1,
    StoreOperationMetadataV1, StoredObservation, StoredObservationRowV1,
};

use tracedecay_runtime_core::db::{Database, DatabaseRuntimeClientV1};

/// Test-only counters over the expensive persist-path work (stored-row
/// decode, collision classification, payload-revision probing, canonical
/// command digesting). A re-admitted terminal identity collision must repeat
/// none of it, and only counters observed at these exact call sites can prove
/// that without editing the domain identity derivation.
#[cfg(test)]
#[derive(Debug, Default)]
pub(crate) struct ObservationPersistProbeV1 {
    pub(crate) stored_observation_reads: std::sync::atomic::AtomicU64,
    pub(crate) collision_classifications: std::sync::atomic::AtomicU64,
    pub(crate) payload_revision_probes: std::sync::atomic::AtomicU64,
    pub(crate) canonical_command_digests: std::sync::atomic::AtomicU64,
}

#[cfg(test)]
impl ObservationPersistProbeV1 {
    pub(crate) fn snapshot(&self) -> (u64, u64, u64, u64) {
        use std::sync::atomic::Ordering::Relaxed;
        (
            self.stored_observation_reads.load(Relaxed),
            self.collision_classifications.load(Relaxed),
            self.payload_revision_probes.load(Relaxed),
            self.canonical_command_digests.load(Relaxed),
        )
    }
}

#[cfg(test)]
macro_rules! probe_count {
    ($store:expr, $counter:ident) => {
        $store
            .persist_probe
            .$counter
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    };
}

#[cfg(not(test))]
macro_rules! probe_count {
    ($store:expr, $counter:ident) => {};
}

/// Observation-store adapter over the already-registered authoritative runtime.
#[derive(Clone)]
pub struct GlobalDbObservationStore {
    database: Database,
    runtime: DatabaseRuntimeClientV1,
    #[cfg(test)]
    persist_probe: Arc<ObservationPersistProbeV1>,
}

impl GlobalDbObservationStore {
    pub fn new(database: Database) -> Self {
        let runtime = database.runtime_client();
        Self {
            database,
            runtime,
            #[cfg(test)]
            persist_probe: Arc::default(),
        }
    }

    #[cfg(test)]
    pub(crate) fn persist_probe(&self) -> Arc<ObservationPersistProbeV1> {
        Arc::clone(&self.persist_probe)
    }

    /// Converges typed coverage past a terminally refused record when the
    /// candidate stands at the sequential scan frontier; any other shape
    /// (covered replay, stale expected cursor, gap, generation jump) leaves
    /// every ledger untouched. One typed cursor read plus, at the frontier,
    /// one typed `admission_refused` advance — no record decode, no identity
    /// derivation, no payload hashing.
    async fn converge_refused_coverage(
        &self,
        write: &AnchoredObservationWrite,
    ) -> ObservationStoreResult<()> {
        let identity = write.observation().identity();
        let actual_cursor =
            read_runtime_source_cursor(&self.runtime, identity.source(), identity.scope())?;
        if !refused_scan_frontier(write, actual_cursor.as_ref()) {
            return Ok(());
        }
        self.advance_refused_coverage(write).await
    }

    /// Records the typed `admission_refused` advance that moves the source
    /// cursor past a refused record's coverage.
    async fn advance_refused_coverage(
        &self,
        write: &AnchoredObservationWrite,
    ) -> ObservationStoreResult<()> {
        let identity = write.observation().identity();
        let mut advance = ObservationCursorAdvance::for_ordering(
            identity.source().clone(),
            identity.scope().clone(),
            identity.generation(),
            identity.ordering_domain(),
            write.expected_cursor().cloned(),
            identity.position(),
            ObservationCoverageReason::AdmissionRefused,
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
                    "record refused admission coverage",
                    "cursor resume checkpoint is incomplete",
                ));
            }
        }
        self.advance_source_cursor(advance).await?;
        Ok(())
    }

    pub async fn converge_projection_predecessor(
        &self,
    ) -> ProjectionStoreResult<ProjectionPredecessorConvergence> {
        crate::converge_projection_predecessor(&self.database).await
    }
}

impl ObservationStore for GlobalDbObservationStore {
    async fn persist_observation(
        &self,
        write: AnchoredObservationWrite,
    ) -> ObservationStoreResult<ObservationPersistOutcome> {
        let runtime = &self.runtime;
        let observation_id = write.observation().observation_id().clone();
        let candidate = write.observation().clone();
        let candidate_cursor = write.next_cursor().clone();
        // A previously refused identity collision is deterministic and
        // terminal. The refusal authority is its own retained table keyed by
        // the exact refused candidate signature `(observation_id,
        // refused_payload_digest)`, so cursor-advance retention can never
        // reclaim it and a candidate with any OTHER payload digest — e.g. a
        // recognized canonical payload revision replay — falls through to the
        // full path untouched. Answering from the marker is one bare-column
        // read: no stored-row decode, no identity re-derivation, no payload
        // canonicalization, no hashing.
        if let Some(retained_digest) = read_admission_refusal(
            &self.database,
            &observation_id,
            candidate.payload_reference().digest(),
        )
        .await?
        {
            // The terminal answers the refusal, but the candidate may stand
            // at a NEW scan frontier the first refusal never covered — a
            // rescan generation, or coverage lost to a failure between the
            // marker commit and its cursor advance. Production ingest aborts
            // a pass on this collision, so if coverage does not converge HERE
            // a refused record at end-of-file would be re-read, re-decoded,
            // and re-hashed by every later rescan forever. Converging costs
            // one typed cursor-advance write and touches no record content.
            self.converge_refused_coverage(&write).await?;
            return Err(ObservationStoreError::ObservationCollision {
                observation_id: Box::new(observation_id),
                existing_digest: Box::new(retained_digest),
                candidate_digest: Box::new(candidate.payload_reference().digest().clone()),
                outcome: ObservationCollisionOutcomeV1::IdentityCollision,
            });
        }
        probe_count!(self, stored_observation_reads);
        let existing = read_runtime_stored_observation(runtime, &observation_id)?;
        let collision = existing.as_ref().map(|existing| {
            probe_count!(self, collision_classifications);
            classify_observation_collision(existing.observation(), &candidate)
        });
        let canonical_payload_revision = existing.as_ref().is_some_and(|existing| {
            probe_count!(self, payload_revision_probes);
            is_canonical_payload_revision_replay(existing.observation(), &candidate)
        });
        if collision == Some(ObservationCollisionOutcomeV1::IdentityCollision)
            && !canonical_payload_revision
        {
            let Some(existing) = existing.as_ref() else {
                return Err(ObservationStoreError::Storage {
                    operation: "persist_observation",
                    source: Box::new(std::io::Error::other(
                        "classified collisions always have an existing observation",
                    )),
                });
            };
            // Durable terminal coverage: the refusal is deterministic (the
            // identity is content-derived and already owned by a different
            // payload). Two records land together, in this order:
            //
            // 1. the refused candidate signature in the retained
            //    `observation_admission_refusals` authority, which answers any
            //    re-admitted identical candidate without decode or hash work;
            // 2. an `admission_refused` advance in the typed
            //    `source_cursor_advances` ledger that moves the source cursor
            //    past the refused record so catch-up never re-reads it.
            //
            // The retained observation row is never touched. Both records are
            // written only for the shape that actually loops in production —
            // a sequential scan standing exactly at the refused record: the
            // durable cursor has NOT covered it, the caller's expected cursor
            // matches the durable one, and the record either continues the
            // current generation contiguously or restarts a new generation
            // from position zero. A gap or a stale expected cursor proves the
            // caller's view is NOT the scan frontier, so nothing is recorded
            // and the refusal stays typed and fail-closed with all
            // authoritative state — rows, cursor, ledger — left untouched;
            // an already-covered candidate is a replayed verification probe
            // and is likewise left untouched.
            if refused_scan_frontier(
                &write,
                read_runtime_source_cursor(
                    runtime,
                    candidate.identity().source(),
                    candidate.identity().scope(),
                )?
                .as_ref(),
            ) {
                record_admission_refusal(
                    &self.database,
                    &observation_id,
                    candidate.payload_reference().digest(),
                    existing.observation().payload_reference().digest(),
                )
                .await?;
                self.advance_refused_coverage(&write).await?;
            }
            return Err(ObservationStoreError::ObservationCollision {
                observation_id: Box::new(observation_id),
                existing_digest: Box::new(
                    existing.observation().payload_reference().digest().clone(),
                ),
                candidate_digest: Box::new(candidate.payload_reference().digest().clone()),
                outcome: ObservationCollisionOutcomeV1::IdentityCollision,
            });
        }
        if canonical_payload_revision {
            let Some(existing) = existing.as_ref() else {
                return Err(runtime_storage_error(
                    "persist canonical payload revision",
                    "classified revision replay has no retained observation",
                ));
            };
            let identity = candidate.identity();
            // A revision replay whose range the durable cursor already covers
            // has no missing coverage to restore. Advancing anyway would
            // collide with whatever advance already covers that range — e.g.
            // the admission-refused advance recorded for an earlier invalid
            // rewrite of the same record — and turn a recognized revision
            // into a permanent cursor-advance collision.
            let actual_cursor =
                read_runtime_source_cursor(runtime, identity.source(), identity.scope())?;
            let revision_covered = actual_cursor.as_ref().is_some_and(|cursor| {
                cursor.generation() == identity.generation()
                    && cursor.ordering_domain() == identity.ordering_domain()
                    && cursor.position() >= identity.position().end()
            });
            if revision_covered {
                return Ok(ObservationPersistOutcome::CoveredDuplicate(
                    ObservationCommitReceipt::new(
                        existing.sequence(),
                        existing.observation().clone(),
                        candidate_cursor,
                        existing.retrieval_anchor().clone(),
                        existing.projection_generation().clone(),
                    )?
                    .with_repository_provenance_attachment(
                        existing.repository_provenance_attachment().clone(),
                    )?,
                ));
            }
            let mut advance = ObservationCursorAdvance::for_ordering_with_sanitization_receipt(
                identity.source().clone(),
                identity.scope().clone(),
                identity.generation(),
                identity.ordering_domain(),
                write.expected_cursor().cloned(),
                identity.position(),
                ObservationCoverageReason::CanonicalPayloadRevision,
                candidate.receipt().clone(),
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
            return Ok(ObservationPersistOutcome::CoveredDuplicate(
                ObservationCommitReceipt::new(
                    existing.sequence(),
                    existing.observation().clone(),
                    candidate_cursor,
                    existing.retrieval_anchor().clone(),
                    existing.projection_generation().clone(),
                )?
                .with_repository_provenance_attachment(
                    existing.repository_provenance_attachment().clone(),
                )?,
            ));
        }
        let same_identity = existing
            .as_ref()
            .is_some_and(|existing| existing.observation().identity() == candidate.identity());
        if same_identity
            && existing
                .as_ref()
                .is_some_and(|existing| existing.observation().receipt() != candidate.receipt())
        {
            return Err(ObservationStoreError::SanitizationReceiptCollision);
        }
        for alias in write.retrieval_anchor().aliases() {
            if let Some(existing_anchor_id) =
                read_runtime_retrieval_anchor_by_alias(runtime, candidate.scope(), alias)?
                && existing_anchor_id != *write.retrieval_anchor_id()
            {
                return Err(ObservationStoreError::RetrievalAnchorAliasCollision {
                    alias: Box::new(alias.clone()),
                    existing_anchor_id: Box::new(existing_anchor_id),
                    candidate_anchor_id: Box::new(write.retrieval_anchor_id().clone()),
                });
            }
        }
        let covered_duplicate =
            collision == Some(ObservationCollisionOutcomeV1::ExactDuplicate) && !same_identity;
        if existing.is_none() || covered_duplicate {
            let actual_cursor =
                read_runtime_source_cursor(runtime, candidate.source(), candidate.scope())?;
            let covered_duplicate_replay =
                covered_duplicate && actual_cursor.as_ref() == Some(&candidate_cursor);
            if !covered_duplicate_replay && actual_cursor.as_ref() != write.expected_cursor() {
                return Err(ObservationStoreError::CursorConflict {
                    expected: Box::new(write.expected_cursor().cloned()),
                    actual: Box::new(actual_cursor),
                });
            }
        }
        let existed_exact = same_identity
            && existing
                .as_ref()
                .is_some_and(|existing| existing.observation().receipt() == candidate.receipt());
        if existed_exact {
            let Some(existing) = existing.as_ref() else {
                return Err(ObservationStoreError::Storage {
                    operation: "persist_observation",
                    source: Box::new(std::io::Error::other(
                        "exact duplicate classification requires a stored observation",
                    )),
                });
            };
            return Ok(ObservationPersistOutcome::ExactDuplicate(
                existing.commit_receipt().clone(),
            ));
        }
        probe_count!(self, canonical_command_digests);
        let idempotency_key = format!(
            "observation.{}",
            canonical_runtime_digest(&runtime_observation_command(&write))?
        );
        let outcome = submit_runtime_write(
            runtime,
            RepositoryWritePayloadV1::Observation(Box::new(write)),
            idempotency_key,
            "submit anchored observation",
        )
        .await?;
        // The authority is durable but the caller has not been told yet: the
        // daemon-crash harness stops here to prove a kill in this window loses
        // the acknowledgement without losing the commit.
        #[cfg(tracedecay_observation_fault_harness)]
        tracedecay_store::fault_harness::wait_at_observation_persist_barrier(
            tracedecay_store::fault_harness::ObservationPersistBarrierStageV1::PostCommitPreAck,
            candidate.source().session_id().as_str(),
        )
        .map_err(|(operation, detail)| runtime_storage_error(operation, detail))?;
        probe_count!(self, stored_observation_reads);
        let stored =
            read_runtime_stored_observation(runtime, &observation_id)?.ok_or_else(|| {
                runtime_storage_error("read committed observation", "row unavailable")
            })?;
        let receipt = stored.commit_receipt().clone();
        match outcome {
            RuntimeSubmitOutcomeV1::Committed { .. }
            | RuntimeSubmitOutcomeV1::CommittedAfterCancellation { .. }
            | RuntimeSubmitOutcomeV1::ExactReplay { .. }
                if stored.observation().identity() != candidate.identity()
                    && classify_observation_collision(stored.observation(), &candidate)
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

    async fn get_source_cursor(
        &self,
        source: &ClaudeSourceIdentityV1,
        scope: &ObservationScopeV1,
    ) -> ObservationStoreResult<Option<ClaudeSourceCursorV1>> {
        read_runtime_source_cursor(&self.runtime, source, scope)
    }

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
        probe_count!(self, canonical_command_digests);
        let key = format!("cursor.{}", canonical_runtime_digest(&identity)?);
        let outcome = submit_runtime_write(
            runtime,
            RepositoryWritePayloadV1::ObservationCursorAdvance(Box::new(advance)),
            key,
            "advance observation source cursor",
        )
        .await;
        if existed_at_next && outcome.is_err() {
            return Err(ObservationStoreError::CursorAdvanceCollision);
        }
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

    async fn get_observation(
        &self,
        observation_id: &CanonicalObservationIdV1,
    ) -> ObservationStoreResult<Option<StoredObservation>> {
        read_runtime_stored_observation(&self.runtime, observation_id)
    }

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

fn dispatch_runtime_observation_read(
    runtime: &DatabaseRuntimeClientV1,
    operation: ObservationReadOperationV1,
) -> ObservationStoreResult<ObservationReadResultV1> {
    let command_digest = canonical_sha256(&operation)
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
    let admission_bytes = serde_json::to_vec(&operation)
        .map_err(|error| runtime_storage_error("build observation runtime read", error))?
        .len();
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

fn read_runtime_retrieval_anchor_by_alias(
    runtime: &DatabaseRuntimeClientV1,
    scope: &ObservationScopeV1,
    alias: &tracedecay_domain::NativeAliasV2,
) -> ObservationStoreResult<Option<tracedecay_domain::RetrievalAnchorId>> {
    match dispatch_runtime_observation_read(
        runtime,
        ObservationReadOperationV1::RetrievalAnchorByAlias {
            scope: scope.clone(),
            alias: alias.clone(),
        },
    )? {
        ObservationReadResultV1::RetrievalAnchorByAlias(anchor_id) => Ok(anchor_id),
        _ => Err(runtime_storage_error(
            "read observation retrieval anchor by alias",
            "runtime returned a mismatched observation read result",
        )),
    }
}

/// Whether a refused candidate stands at the sequential scan frontier: the
/// durable cursor has NOT covered its range, the caller's expected cursor
/// matches the durable one (so an advance is a pure forward move, never a
/// regression), and the record either continues the current generation
/// contiguously or restarts a new generation from position zero. Coverage is
/// recorded only for this shape — the one production ingest actually loops
/// on; gaps and stale views prove the caller is not the scan frontier.
fn refused_scan_frontier(
    write: &AnchoredObservationWrite,
    actual_cursor: Option<&ClaudeSourceCursorV1>,
) -> bool {
    let identity = write.observation().identity();
    let candidate_covered = actual_cursor.is_some_and(|cursor| {
        cursor.generation() == identity.generation()
            && cursor.ordering_domain() == identity.ordering_domain()
            && cursor.position() >= identity.position().end()
    });
    if candidate_covered || actual_cursor != write.expected_cursor() {
        return false;
    }
    match write.expected_cursor() {
        Some(cursor)
            if cursor.generation() == identity.generation()
                && cursor.ordering_domain() == identity.ordering_domain() =>
        {
            cursor.position() == identity.position().start()
        }
        Some(_) | None => identity.position().start() == 0,
    }
}

/// Durable terminal marker for a previously refused identity collision.
///
/// Keyed by the exact refused candidate signature `(observation_id,
/// refused_payload_digest)` in the retained `observation_admission_refusals`
/// authority. The read is one bare-column lookup: it never decodes an
/// observation, re-derives an identity, or hashes anything — the candidate's
/// digest arrives precomputed in the write. A candidate carrying any other
/// payload digest (an exact replay of the retained row, or a canonical
/// payload revision replay) misses the key and falls through to the full
/// path untouched.
async fn read_admission_refusal(
    database: &Database,
    observation_id: &CanonicalObservationIdV1,
    refused_digest: &PayloadDigestV1,
) -> ObservationStoreResult<Option<PayloadDigestV1>> {
    const OPERATION: &str = "read admission refusal terminal";
    let snapshot = database
        .begin_engine_read_snapshot(OPERATION)
        .await
        .map_err(|error| runtime_storage_error(OPERATION, error))?;
    let mut rows = snapshot
        .query(
            "SELECT retained_payload_digest FROM observation_admission_refusals
             WHERE observation_id = ?1 AND refused_payload_digest = ?2",
            tracedecay_runtime_core::db::engine::params![
                observation_id.as_str(),
                refused_digest.as_str()
            ],
        )
        .await
        .map_err(|error| runtime_storage_error(OPERATION, error))?;
    let Some(row) = rows
        .next()
        .await
        .map_err(|error| runtime_storage_error(OPERATION, error))?
    else {
        return Ok(None);
    };
    PayloadDigestV1::new(
        row.get::<String>(0)
            .map_err(|error| runtime_storage_error(OPERATION, error))?,
    )
    .map(Some)
    .map_err(ObservationStoreError::Contract)
}

/// Records one refused candidate signature in the retained refusal authority.
///
/// Idempotent: a replayed refusal conflicts on the primary key and changes
/// nothing, which also keeps the row immutable under its schema triggers.
async fn record_admission_refusal(
    database: &Database,
    observation_id: &CanonicalObservationIdV1,
    refused_digest: &PayloadDigestV1,
    retained_digest: &PayloadDigestV1,
) -> ObservationStoreResult<()> {
    const OPERATION: &str = "record admission refusal terminal";
    let transaction = database
        .begin_write_transaction(OPERATION)
        .await
        .map_err(|error| runtime_storage_error(OPERATION, error))?;
    transaction
        .execute(
            "INSERT INTO observation_admission_refusals (
                observation_id, refused_payload_digest, retained_payload_digest, refused_at
             ) VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT DO NOTHING",
            tracedecay_runtime_core::db::engine::params![
                observation_id.as_str(),
                refused_digest.as_str(),
                retained_digest.as_str(),
                now_micros().0
            ],
        )
        .await
        .map_err(|error| runtime_storage_error(OPERATION, error))?;
    transaction
        .commit()
        .await
        .map_err(|error| runtime_storage_error(OPERATION, error))?;
    Ok(())
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
        ObservationReadResultV1::Observation(row) => {
            (*row).map(stored_observation_from_runtime_row).transpose()
        }
        _ => Err(runtime_storage_error(
            "read observation",
            "runtime returned a mismatched observation read result",
        )),
    }
}

async fn submit_runtime_write(
    runtime: &DatabaseRuntimeClientV1,
    payload: RepositoryWritePayloadV1,
    idempotency_key: String,
    operation: &'static str,
) -> ObservationStoreResult<RuntimeSubmitOutcomeV1> {
    let command = runtime_command_value(&payload)?;
    let command_digest = canonical_sha256(&command)
        .map_err(|error| runtime_storage_error(operation, error.to_string()))?;
    let digest_suffix = command_digest
        .as_str()
        .strip_prefix("sha256:")
        .ok_or_else(|| runtime_storage_error(operation, "canonical digest prefix is invalid"))?;
    let admitted_at = now_micros();
    let binding = runtime.binding();
    let metadata = StoreOperationMetadataV1 {
        operation_id: StoreOperationIdV1::new(format!(
            "operation.host-observation.{digest_suffix}"
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
            command_digest: CommandDigestV1::new(command_digest.as_str())
                .map_err(|error| runtime_storage_error(operation, error.to_string()))?,
        },
        durability: DurabilityClassV1::Full,
        priority: OperationPriorityV1::Foreground,
        admission_bytes: u64::try_from(
            serde_json::to_vec(&command)
                .map_err(|error| runtime_storage_error(operation, error.to_string()))?
                .len(),
        )
        .unwrap_or(u64::MAX)
        .max(1),
        admitted_at,
    };
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
        requested_at: admitted_at,
        deadline: deadline.clone(),
        cancellation: cancellation.clone(),
    };
    let request = RuntimeSubmitRequestV1::new(
        RepositoryOperationEnvelopeV1 { metadata, payload },
        transaction_scope,
        control,
    )
    .map_err(|error| runtime_storage_error(operation, error.to_string()))?;
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
        .map_err(|error| runtime_storage_error(operation, format!("{error:?}")))
}

fn runtime_command_value(
    payload: &RepositoryWritePayloadV1,
) -> ObservationStoreResult<serde_json::Value> {
    match payload {
        RepositoryWritePayloadV1::Observation(write) => Ok(runtime_observation_command(write)),
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
    digest
        .as_str()
        .strip_prefix("sha256:")
        .map(str::to_owned)
        .ok_or_else(|| {
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

    async fn project_observation(
        &self,
        observation_id: &CanonicalObservationIdV1,
    ) -> ProjectionStoreResult<ProjectionPersistOutcome> {
        crate::project_observation(&self.database, observation_id).await
    }

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
                persist_probe: _,
            } = store;
        }

        let _ = assert_exact_fields;
    }
}
