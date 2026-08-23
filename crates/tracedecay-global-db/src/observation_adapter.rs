use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use tracedecay_application::clock::now_micros;
use tracing::Instrument;

use tracedecay_domain::{
    CanonicalObservationIdV1, ClaudeSourceCursorV1, ClaudeSourceIdentityV1, DurableObservationV1,
    ManifestDigest, ObservationCollisionOutcomeV1, ObservationScopeV1, PayloadDigestV1,
    canonical_json_bytes_and_sha256, canonical_sha256, classify_observation_collision,
    is_canonical_payload_revision_replay,
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
    /// `observation_admission_refusals` AND the typed `admission_refused`
    /// coverage advance — in ONE atomic authority transaction, but only when
    /// the candidate stands at the sequential scan frontier; any other shape
    /// (covered replay, stale expected cursor, gap, generation jump) leaves
    /// every ledger untouched.
    ///
    /// Atomicity is the contract: either the marker and its coverage land
    /// together or neither is visible, so a failure while recording coverage
    /// can never orphan a marker whose record the cursor still re-reads. The
    /// frontier is re-verified INSIDE the transaction (exact compare-and-set
    /// against the durable cursor), the advance-ledger row must carry the
    /// `admission_refused` reason with no receipt, and the cursor moves to
    /// the advance's next position — executed through the one canonical
    /// cursor-advance statement set
    /// (`tracedecay_rusqlite_runtime::repository::observation_cursor_authority`)
    /// that the runtime write path also executes. No record content is
    /// decoded, derived, or hashed.
    async fn record_refusal_with_coverage(
        &self,
        write: &AnchoredObservationWrite,
        retained_digest: &PayloadDigestV1,
    ) -> ObservationStoreResult<()> {
        const OPERATION: &str = "record refused admission terminal and coverage";
        let candidate = write.observation();
        let identity = candidate.identity();
        let actual_cursor =
            read_runtime_source_cursor(&self.runtime, identity.source(), identity.scope())?;
        let Some(mut advance) = refused_scan_frontier(write, actual_cursor.as_ref())? else {
            return Ok(());
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
            return Ok(());
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
                    ObservationCoverageReason::AdmissionRefused.as_str(),
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
            ObservationCoverageReason::AdmissionRefused.as_str(),
            None,
        ) {
            transaction
                .rollback()
                .await
                .map_err(|error| runtime_storage_error(OPERATION, error))?;
            return Err(ObservationStoreError::CursorAdvanceCollision);
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
        Ok(())
    }

    pub async fn converge_projection_predecessor(
        &self,
    ) -> ProjectionStoreResult<ProjectionPredecessorConvergence> {
        crate::converge_projection_predecessor(&self.database).await
    }

    async fn prepare_observation_persist(
        &self,
        write: AnchoredObservationWrite,
        known_cursor: Option<Option<ClaudeSourceCursorV1>>,
    ) -> ObservationStoreResult<PreparedObservationPersist> {
        let runtime = &self.runtime;
        let observation = write.observation();
        let observation_id = observation.observation_id().clone();
        if let Some(retained_digest) = read_admission_refusal(
            &self.database,
            &observation_id,
            observation.payload_reference().digest(),
        )
        .await?
        {
            self.record_refusal_with_coverage(&write, &retained_digest)
                .await?;
            return Err(identity_collision(
                observation_id,
                retained_digest,
                observation.payload_reference().digest().clone(),
            ));
        }
        let existing = read_runtime_stored_observation(runtime, &observation_id)?;
        let collision = existing
            .as_ref()
            .map(|existing| classify_observation_collision(existing.observation(), observation));
        let canonical_payload_revision = existing.as_ref().is_some_and(|existing| {
            is_canonical_payload_revision_replay(existing.observation(), observation)
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
            self.record_refusal_with_coverage(
                &write,
                existing.observation().payload_reference().digest(),
            )
            .await?;
            return Err(identity_collision(
                observation_id,
                existing.observation().payload_reference().digest().clone(),
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
            let actual_cursor = match known_cursor {
                Some(cursor) => cursor,
                None => read_runtime_source_cursor(runtime, identity.source(), identity.scope())?,
            };
            let revision_covered = actual_cursor.as_ref().is_some_and(|cursor| {
                cursor.generation() == identity.generation()
                    && cursor.ordering_domain() == identity.ordering_domain()
                    && cursor.position() >= identity.position().end()
            });
            if revision_covered {
                return Ok(PreparedObservationPersist::Ready(Box::new(
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
                )));
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
            return Ok(PreparedObservationPersist::Ready(Box::new(
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
            )));
        }
        let same_identity = existing
            .as_ref()
            .is_some_and(|existing| existing.observation().identity() == observation.identity());
        if same_identity
            && existing
                .as_ref()
                .is_some_and(|existing| existing.observation().receipt() != observation.receipt())
        {
            return Err(ObservationStoreError::SanitizationReceiptCollision);
        }
        for alias in write.retrieval_anchor().aliases() {
            if let Some(existing_anchor_id) =
                read_runtime_retrieval_anchor_by_alias(runtime, observation.scope(), alias)?
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
            let actual_cursor = match known_cursor {
                Some(cursor) => cursor,
                None => {
                    read_runtime_source_cursor(runtime, observation.source(), observation.scope())?
                }
            };
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
            && existing
                .as_ref()
                .is_some_and(|existing| existing.observation().receipt() == observation.receipt());
        if existed_exact {
            let Some(existing) = existing.as_ref() else {
                return Err(ObservationStoreError::Storage {
                    operation: "persist_observation",
                    source: Box::new(std::io::Error::other(
                        "exact duplicate classification requires a stored observation",
                    )),
                });
            };
            return Ok(PreparedObservationPersist::Ready(Box::new(
                ObservationPersistOutcome::ExactDuplicate(existing.commit_receipt().clone()),
            )));
        }
        Ok(PreparedObservationPersist::Submit(Box::new(write)))
    }
}

enum PreparedObservationPersist {
    // Both payloads are large and only one is ever live, so both are boxed:
    // leaving either inline makes every value of this enum carry that variant's
    // footprint.
    Ready(Box<ObservationPersistOutcome>),
    Submit(Box<AnchoredObservationWrite>),
}

impl ObservationStore for GlobalDbObservationStore {
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
        outcomes.pop().ok_or_else(|| {
            runtime_storage_error("persist_observation", "batch returned no outcome")
        })
    }

    #[hotpath::measure]
    async fn persist_observations(
        &self,
        writes: Vec<AnchoredObservationWrite>,
    ) -> ObservationStoreResult<Vec<ObservationPersistOutcome>> {
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
                    .prepare_observation_persist(write, known_cursor)
                    .await?;
                published_cursors.insert(key, next_cursor);
                prepared.push(item);
            }
            let mut outcomes: Vec<Option<ObservationPersistOutcome>> =
                Vec::with_capacity(prepared.len());
            let mut submits = Vec::new();
            for item in prepared {
                match item {
                    PreparedObservationPersist::Ready(outcome) => outcomes.push(Some(*outcome)),
                    PreparedObservationPersist::Submit(write) => {
                        submits.push((outcomes.len(), *write));
                        outcomes.push(None);
                    }
                }
            }
            if !submits.is_empty() {
                let submitted = submit_observation_writes(&self.runtime, submits).await?;
                for (slot, outcome) in submitted {
                    outcomes[slot] = Some(outcome);
                }
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

/// The typed cursor advance for a refused candidate standing at the
/// sequential scan frontier: the durable cursor has NOT covered its range and
/// the caller's expected cursor matches the durable one. Generation values
/// are opaque source identities, not ordered counters.
///
/// `Ok(None)` is the not-at-frontier verdict — a covered replay or a stale
/// expected view — and leaves every ledger untouched. Gaps and generation
/// jumps never reach the advance constructor here: `ObservationWrite::new`
/// already validated that this write's expected→next cursor transition
/// covers the candidate range, which is exactly the transition the advance
/// re-derives from the same identity, expected cursor, and range. A
/// construction failure is therefore a contract violation, and it surfaces
/// as the typed store error — silently answering "not at the scan frontier"
/// would record no coverage and leave the refused record re-read forever.
fn refused_scan_frontier(
    write: &AnchoredObservationWrite,
    actual_cursor: Option<&ClaudeSourceCursorV1>,
) -> ObservationStoreResult<Option<ObservationCursorAdvance>> {
    let identity = write.observation().identity();
    let candidate_covered = actual_cursor.is_some_and(|cursor| {
        cursor.generation() == identity.generation()
            && cursor.ordering_domain() == identity.ordering_domain()
            && cursor.position() >= identity.position().end()
    });
    if candidate_covered || actual_cursor != write.expected_cursor() {
        return Ok(None);
    }
    ObservationCursorAdvance::for_ordering(
        identity.source().clone(),
        identity.scope().clone(),
        identity.generation(),
        identity.ordering_domain(),
        write.expected_cursor().cloned(),
        identity.position(),
        ObservationCoverageReason::AdmissionRefused,
    )
    .map(Some)
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
    runtime: &DatabaseRuntimeClientV1,
    writes: Vec<(usize, AnchoredObservationWrite)>,
) -> ObservationStoreResult<Vec<(usize, ObservationPersistOutcome)>> {
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
    writes
        .into_iter()
        .map(|(slot, write)| {
            persist_outcome_from_submit(
                runtime,
                write.observation().observation_id(),
                write.observation(),
                write.next_cursor().clone(),
                outcome.clone(),
            )
            .map(|outcome| (slot, outcome))
        })
        .collect()
}

fn persist_outcome_from_submit(
    runtime: &DatabaseRuntimeClientV1,
    observation_id: &CanonicalObservationIdV1,
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
    let stored = read_runtime_stored_observation(runtime, observation_id)?
        .ok_or_else(|| runtime_storage_error("read committed observation", "row unavailable"))?;
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
        .map_err(|error| runtime_storage_error(operation, format!("{error:?}")))
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
            } = store;
        }

        let _ = assert_exact_fields;
    }
}
