use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use tracedecay_domain::{
    CanonicalObservationIdV1, ClaudeSourceCursorV1, ClaudeSourceIdentityV1,
    ObservationCollisionOutcomeV1, ObservationScopeV1, UtcMicros, canonical_sha256,
    classify_observation_collision,
};
use tracedecay_store::observation::{CursorAdvanceOutcome, ObservationCursorAdvance};
use tracedecay_store::{
    AnchoredObservationWrite, CommandDigestV1, ConsistencyModeV1, DurabilityClassV1,
    IdempotencyIdentityV1, ObservationCommitReceipt, ObservationPersistOutcome,
    ObservationProjectionStatus, ObservationProjectionStore, ObservationReadOperationV1,
    ObservationReadResultV1, ObservationReplayRequest, ObservationStore, ObservationStoreError,
    ObservationStoreResult, OperationPriorityV1, ProjectReadOperationV1, ProjectReadResultV1,
    ProjectionCheckpoint, ProjectionPersistOutcome, ProjectionRebuildOutcome,
    ProjectionStoreResult, RepositoryOperationEnvelopeV1, RepositoryReadOperationV1,
    RepositoryReadResultV1, RepositoryWritePayloadV1, RuntimeBatchCompatibilityV1,
    RuntimeCancellationIdV1, RuntimeCancellationIdentityV1, RuntimeDeadlineIdV1, RuntimeDeadlineV1,
    RuntimeInterruptionV1, RuntimeReadCoverageV1, RuntimeReadOperationV1, RuntimeReadRequestV1,
    RuntimeReadResultV1, RuntimeRequestControlV1, RuntimeRequestProbeV1, RuntimeSubmitOutcomeV1,
    RuntimeSubmitRequestV1, RuntimeTransactionIdV1, RuntimeTransactionScopeV1, StoreClientIdV1,
    StoreIdempotencyKeyV1, StoreOperationIdV1, StoreOperationMetadataV1, StoredObservation,
    StoredObservationRowV1,
};

use tracedecay_runtime_core::db::DatabaseAuthority;
use tracedecay_runtime_core::store_runtime::registry::StoreRuntimeHandle;
/// Observation-store adapter over the already-registered authoritative runtime.
pub struct GlobalDbObservationStore<'a> {
    runtime: &'a StoreRuntimeHandle,
    write_authority: &'a DatabaseAuthority,
}

impl<'a> GlobalDbObservationStore<'a> {
    pub const fn with_runtime(
        runtime: &'a StoreRuntimeHandle,
        write_authority: &'a DatabaseAuthority,
    ) -> Self {
        Self {
            runtime,
            write_authority,
        }
    }
}

impl ObservationStore for GlobalDbObservationStore<'_> {
    async fn persist_observation(
        &self,
        write: AnchoredObservationWrite,
    ) -> ObservationStoreResult<ObservationPersistOutcome> {
        let runtime = self.runtime;
        let authority = self.write_authority;
        let observation_id = write.observation().observation_id().clone();
        let candidate = write.observation().clone();
        let candidate_cursor = write.next_cursor().clone();
        let existing = read_runtime_stored_observation(runtime, &observation_id)?;
        let collision = existing
            .as_ref()
            .map(|existing| classify_observation_collision(existing.observation(), &candidate));
        if collision == Some(ObservationCollisionOutcomeV1::IdentityCollision) {
            let Some(existing) = existing.as_ref() else {
                return Err(ObservationStoreError::Storage {
                    operation: "persist_observation",
                    source: Box::new(std::io::Error::other(
                        "classified collisions always have an existing observation",
                    )),
                });
            };
            return Err(ObservationStoreError::ObservationCollision {
                observation_id: Box::new(observation_id),
                existing_digest: Box::new(
                    existing.observation().payload_reference().digest().clone(),
                ),
                candidate_digest: Box::new(candidate.payload_reference().digest().clone()),
                outcome: ObservationCollisionOutcomeV1::IdentityCollision,
            });
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
        let idempotency_key = format!(
            "observation.{}",
            canonical_runtime_digest(&runtime_observation_command(&write))?
        );
        let outcome = submit_runtime_write(
            runtime,
            authority,
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
        read_runtime_source_cursor(self.runtime, source, scope)
    }

    async fn advance_source_cursor(
        &self,
        advance: ObservationCursorAdvance,
    ) -> ObservationStoreResult<CursorAdvanceOutcome> {
        let runtime = self.runtime;
        let authority = self.write_authority;
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
        let outcome = submit_runtime_write(
            runtime,
            authority,
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
        read_runtime_stored_observation(self.runtime, observation_id)
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
            self.runtime,
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
}

impl RuntimeObservationProbe {
    fn from_control(control: &RuntimeRequestControlV1) -> Self {
        Self {
            cancellation: control.cancellation.clone(),
            deadline: control.deadline.clone(),
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
}

fn dispatch_runtime_observation_read(
    runtime: &StoreRuntimeHandle,
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
    let requested_at = runtime_now();
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
    runtime: &StoreRuntimeHandle,
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
    runtime: &StoreRuntimeHandle,
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

fn read_runtime_stored_observation(
    runtime: &StoreRuntimeHandle,
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
    runtime: &StoreRuntimeHandle,
    authority: &DatabaseAuthority,
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
    let admitted_at = runtime_now();
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
        .dispatch_submit_authorized(
            request,
            Arc::new(RuntimeObservationProbe {
                cancellation,
                deadline,
            }),
            authority.clone(),
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

fn runtime_now() -> UtcMicros {
    let micros = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_micros();
    UtcMicros(i64::try_from(micros).unwrap_or(i64::MAX))
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

impl ObservationProjectionStore for GlobalDbObservationStore<'_> {
    async fn next_queued_observation(
        &self,
    ) -> ProjectionStoreResult<Option<CanonicalObservationIdV1>> {
        match dispatch_runtime_observation_read(
            self.runtime,
            ObservationReadOperationV1::NextQueuedProjection,
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
        let handle = self
            .runtime
            .authorized_exact_sql_handle(self.write_authority.clone())
            .map_err(|error| {
                projection_runtime_error(runtime_storage_error(
                    "project observation",
                    format!("registered runtime authority failed: {error:?}"),
                ))
            })?;
        let connection = tracedecay_runtime_core::db::engine::Connection::attach(handle);
        crate::project_observation_with_engine(&connection, observation_id).await
    }

    async fn projection_checkpoint(&self) -> ProjectionStoreResult<ProjectionCheckpoint> {
        match dispatch_runtime_observation_read(
            self.runtime,
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
        let handle = self
            .runtime
            .authorized_exact_sql_handle(self.write_authority.clone())
            .map_err(|error| {
                projection_runtime_error(runtime_storage_error(
                    "rebuild observation projection",
                    format!("registered runtime authority failed: {error:?}"),
                ))
            })?;
        let connection = tracedecay_runtime_core::db::engine::Connection::attach(handle);
        crate::rebuild_projection_with_engine(&connection, frontier_sequence).await
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
    fn adapter_contains_only_registered_runtime_handles() {
        fn assert_exact_fields(store: &GlobalDbObservationStore<'_>) {
            let GlobalDbObservationStore {
                runtime: _,
                write_authority: _,
            } = store;
        }

        let _ = assert_exact_fields;
        assert_eq!(
            std::mem::size_of::<GlobalDbObservationStore<'static>>(),
            std::mem::size_of::<(&'static StoreRuntimeHandle, &'static DatabaseAuthority,)>()
        );
    }
}
