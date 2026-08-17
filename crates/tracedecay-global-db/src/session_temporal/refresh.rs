use serde_json::{Value, json};
use tracedecay_domain::{
    SessionCursorKeyIdV1, SessionCursorVersionV1, SessionId, SessionProjectionGenerationV1,
    SessionRefreshKeyV1, SessionRefreshOperationIdV1, SessionRefreshSourceTargetV1,
    SessionSourceCoverageReceiptV1, SessionSourceCoverageV1, SessionSourceFrontierV1,
    SessionSourceIdV1, SessionTemporalCoverageRequestV1, SignedCursorKeyRefV1,
    TemporalCoverageCountsV1, TemporalModeV1, UtcMicros,
};
use tracedecay_runtime_core::db::engine::{Executor, QueryExecutor, Row, params};
use tracedecay_store::{
    SessionFrozenWatermarksV1, SessionRefreshBeginOrJoinReceiptV1,
    SessionRefreshBeginOrJoinRequestV1, SessionRefreshCancellationRequestV1,
    SessionRefreshCompletionRequestV1, SessionRefreshDispositionV1, SessionRefreshFailureCodeV1,
    SessionRefreshFailureRequestV1, SessionRefreshFrontierV1, SessionRefreshProgressRequestV1,
    SessionRefreshProgressV1, SessionRefreshReceiptRequestV1, SessionRefreshReceiptV1,
    SessionRefreshStateV1, SessionRefreshTerminalStateV1, SessionStoreError, SessionStoreResult,
    SessionTemporalProjectionBatchReceiptV1, SessionTemporalProjectionBatchV1,
};
use tracedecay_temporal_query::ports::ExecutionControl;

use super::super::RegisteredGlobalDb;
use super::cursor_keys::ensure_active_session_cursor_key_in_transaction;
use super::projection::{
    digest_bytes, persist_session_temporal_projection_batch_in_transaction,
    seed_active_projection_in_transaction, session_temporal_projection_record_count,
    validate_final_projection_receipt,
};
use super::query::{
    encode_watermarks, frontier_i64, generation_i64, now_micros, read_generation, storage,
    storage_message,
};
use super::rebuild::{
    checkpoint_relation_rebuild_control, rebuild_candidate_session_relations,
    validate_candidate_frontier,
};

const BEGIN_REFRESH: &str = "begin or join session refresh";
const PERSIST_REFRESH: &str = "persist session refresh progress";
const COMPLETE_REFRESH: &str = "complete session refresh";
const FAIL_REFRESH: &str = "fail session refresh";
const CANCEL_REFRESH: &str = "cancel session refresh";
const READ_REFRESH: &str = "read session refresh";
const PROJECTOR_VERSION: &str = "session-temporal-projector.v1";
const CONFIG_VERSION: &str = "session-refresh-config.v1";
/// Hard cap on durable running-refresh recovery materialization per read.
const MAX_RUNNING_REFRESH_RECOVERIES: i64 = 256;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SessionRefreshRestartStateV1 {
    BeginProjection,
    ResumeProjection { next_batch_ordinal: u64 },
    ReadyToComplete,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionRefreshRecoveryV1 {
    operation_id: SessionRefreshOperationIdV1,
    session_id: SessionId,
    source_targets: Vec<SessionRefreshSourceTargetV1>,
    coverage_request: SessionTemporalCoverageRequestV1,
    source_frontier: u64,
    target_frontier: SessionRefreshFrontierV1,
    candidate_generation: SessionProjectionGenerationV1,
    frozen_watermarks: SessionFrozenWatermarksV1,
    projector_version: String,
    config_digest: String,
    binding_digest: String,
    progress: Option<SessionRefreshProgressV1>,
    restart_state: SessionRefreshRestartStateV1,
}

impl SessionRefreshRecoveryV1 {
    pub fn operation_id(&self) -> &SessionRefreshOperationIdV1 {
        &self.operation_id
    }

    pub fn session_id(&self) -> &SessionId {
        &self.session_id
    }

    pub fn source_id(&self) -> &SessionSourceIdV1 {
        self.source_targets[0].source_id()
    }

    pub fn source_targets(&self) -> &[SessionRefreshSourceTargetV1] {
        &self.source_targets
    }

    pub fn coverage_request(&self) -> &SessionTemporalCoverageRequestV1 {
        &self.coverage_request
    }

    pub const fn target_frontier(&self) -> SessionRefreshFrontierV1 {
        self.target_frontier
    }

    pub const fn source_frontier(&self) -> u64 {
        self.source_frontier
    }

    pub const fn candidate_generation(&self) -> SessionProjectionGenerationV1 {
        self.candidate_generation
    }

    pub fn frozen_watermarks(&self) -> &SessionFrozenWatermarksV1 {
        &self.frozen_watermarks
    }

    pub fn projector_version(&self) -> &str {
        &self.projector_version
    }

    pub fn config_digest(&self) -> &str {
        &self.config_digest
    }

    pub fn binding_digest(&self) -> &str {
        &self.binding_digest
    }

    pub fn progress(&self) -> Option<&SessionRefreshProgressV1> {
        self.progress.as_ref()
    }

    pub const fn restart_state(&self) -> SessionRefreshRestartStateV1 {
        self.restart_state
    }

    pub fn source_coverage(
        &self,
        committed_through: u64,
    ) -> SessionStoreResult<SessionSourceCoverageReceiptV1> {
        let sources = self
            .source_targets
            .iter()
            .map(|target| {
                SessionSourceCoverageV1::from_frontiers(
                    target.source_id().clone(),
                    target.observed_frontier(),
                    SessionSourceFrontierV1::new(
                        committed_through.min(target.observed_frontier().value()),
                    ),
                    target.target_watermark(),
                    self.coverage_request.clone(),
                )
            })
            .collect::<Result<Vec<_>, _>>()
            .map_err(SessionStoreError::from)?;
        SessionSourceCoverageReceiptV1::new(self.coverage_request.clone(), sources)
            .map_err(SessionStoreError::from)
    }
}

impl RegisteredGlobalDb {
    pub async fn begin_or_join_session_refresh_result(
        &self,
        request: SessionRefreshBeginOrJoinRequestV1,
    ) -> SessionStoreResult<SessionRefreshBeginOrJoinReceiptV1> {
        let transaction = self
            .begin_write_transaction()
            .await
            .map_err(|error| storage(BEGIN_REFRESH, error))?;
        let request_digest = refresh_binding_digest(&request)?;

        if let Some(existing) =
            read_joinable_operation_by_digest(&transaction, request.session_id(), &request_digest)
                .await?
        {
            transaction
                .commit()
                .await
                .map_err(|error| storage(BEGIN_REFRESH, error))?;
            return Ok(SessionRefreshBeginOrJoinReceiptV1::new(
                existing.operation_id,
                request.session_id().clone(),
                request.target_frontier(),
                SessionRefreshDispositionV1::Joined,
                existing.created_at,
            ));
        }
        if read_running_operation(&transaction, request.session_id())
            .await?
            .is_some()
        {
            return Err(SessionStoreError::IdempotencyConflict {
                context: "session refresh busy",
            });
        }

        let provisioned_cursor_key =
            ensure_active_session_cursor_key_in_transaction(&transaction).await?;
        let (active_generation, active_watermarks) =
            ensure_active_generation(&transaction, &request).await?;
        if request.target_frontier().committed_through() != active_watermarks.projection_frontier()
        {
            return Err(SessionStoreError::InvalidStateTransition {
                context: "refresh source frontier must match active projection frontier",
            });
        }
        let candidate_generation = next_generation(&transaction, request.session_id()).await?;
        let mut frozen_watermarks = SessionFrozenWatermarksV1::new(
            active_generation,
            request.target_frontier().observed_through(),
            request.target_frontier().observed_through(),
            active_watermarks.summary_frontier(),
        );
        if let Some(cursor_key) = active_watermarks.cursor_key() {
            frozen_watermarks = frozen_watermarks.with_cursor_key(cursor_key.clone());
        } else {
            frozen_watermarks = frozen_watermarks.with_cursor_key(provisioned_cursor_key);
        }
        let frozen_watermarks_json = encode_watermarks(&frozen_watermarks, BEGIN_REFRESH)?;
        let accepted_at = now_micros(BEGIN_REFRESH)?;
        let attempt =
            next_operation_attempt(&transaction, request.session_id(), &request_digest).await?;
        let operation_id = operation_id_for_digest(&request_digest, attempt)?;

        // Claim one-running ownership before inserting the candidate generation so a failed
        // begin cannot leave an unbound building generation if transaction boundaries change.
        transaction
            .execute(
                "INSERT INTO session_refresh_operations (
                    session_id, operation_id, request_digest, target_frontier_json,
                    state, created_at, updated_at
                 ) VALUES (?1, ?2, ?3, ?4, 'running', ?5, ?5)",
                params![
                    request.session_id().as_str(),
                    operation_id.as_str(),
                    request_digest.as_str(),
                    encode_refresh_target(&request)?,
                    accepted_at.0,
                ],
            )
            .await
            .map_err(map_begin_conflict)?;
        transaction
            .execute(
                "INSERT INTO session_temporal_generations (
                    session_id, generation, state, frozen_watermarks_json, created_at
                 ) VALUES (?1, ?2, 'building', ?3, ?4)",
                params![
                    request.session_id().as_str(),
                    generation_i64(candidate_generation, BEGIN_REFRESH)?,
                    frozen_watermarks_json.as_str(),
                    accepted_at.0,
                ],
            )
            .await
            .map_err(|error| storage(BEGIN_REFRESH, error))?;
        // Summary availability is generation-bound: the candidate inherits the
        // active generation's rows exactly like the summary-publication
        // route's generation builder, otherwise activating this refresh would
        // silently drop every published summary from generation-bound reads.
        transaction
            .execute(
                "INSERT INTO session_summary_availability (
                    session_id, generation, summary_id, availability,
                    source_horizon_json, reason, checked_at
                 )
                 SELECT session_id, ?2, summary_id, availability,
                        source_horizon_json, reason, ?3
                 FROM session_summary_availability
                 WHERE session_id = ?1 AND generation = ?4",
                params![
                    request.session_id().as_str(),
                    generation_i64(candidate_generation, BEGIN_REFRESH)?,
                    accepted_at.0,
                    generation_i64(active_generation, BEGIN_REFRESH)?,
                ],
            )
            .await
            .map_err(|error| storage(BEGIN_REFRESH, error))?;
        transaction
            .execute(
                "INSERT INTO session_refresh_bindings (
                    session_id, operation_id, scope_kind, source_frontier, target_frontier,
                    projector_version, config_digest, generation, frozen_watermarks_json,
                    binding_digest, created_at
                 ) VALUES (?1, ?2, 'session_store', ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
                params![
                    request.session_id().as_str(),
                    operation_id.as_str(),
                    frontier_i64(request.target_frontier().committed_through(), BEGIN_REFRESH,)?,
                    frontier_i64(request.target_frontier().observed_through(), BEGIN_REFRESH)?,
                    PROJECTOR_VERSION,
                    config_digest(),
                    generation_i64(candidate_generation, BEGIN_REFRESH)?,
                    frozen_watermarks_json,
                    request_digest,
                    accepted_at.0,
                ],
            )
            .await
            .map_err(|error| storage(BEGIN_REFRESH, error))?;
        transaction
            .commit()
            .await
            .map_err(|error| storage(BEGIN_REFRESH, error))?;
        Ok(SessionRefreshBeginOrJoinReceiptV1::new(
            operation_id,
            request.session_id().clone(),
            request.target_frontier(),
            SessionRefreshDispositionV1::Started,
            accepted_at,
        ))
    }

    pub async fn persist_session_refresh_projection_batch_result(
        &self,
        progress: SessionRefreshProgressV1,
        batch: SessionTemporalProjectionBatchV1,
    ) -> SessionStoreResult<(
        SessionRefreshProgressV1,
        SessionTemporalProjectionBatchReceiptV1,
    )> {
        validate_progress_batch_identity(&progress, &batch)?;
        let transaction = self
            .begin_write_transaction()
            .await
            .map_err(|error| storage(PERSIST_REFRESH, error))?;
        let binding = require_running_binding(
            &transaction,
            progress.session_id(),
            progress.operation_id(),
            PERSIST_REFRESH,
        )
        .await?;
        validate_batch_binding(&binding, &batch)?;
        validate_progress_binding(&binding, &progress)?;
        require_progress_timestamp(&progress)?;

        if let Some(existing) =
            read_progress(&transaction, progress.session_id(), progress.operation_id()).await?
        {
            if progress_logically_equal(&existing, &progress) {
                require_progress_batch_ordinal(&progress, batch.batch_ordinal())?;
                let receipt =
                    persist_session_temporal_projection_batch_in_transaction(&transaction, &batch)
                        .await?;
                require_batch_binding(
                    &transaction,
                    progress.session_id(),
                    progress.operation_id(),
                    batch.batch_ordinal(),
                    batch.generation(),
                )
                .await?;
                transaction
                    .commit()
                    .await
                    .map_err(|error| storage(PERSIST_REFRESH, error))?;
                return Ok((existing, receipt));
            }
            if existing.committed_batches() == progress.committed_batches() {
                return Err(SessionStoreError::IdempotencyConflict {
                    context: "refresh progress conflict",
                });
            }
        }

        seed_active_projection_in_transaction(&transaction, &batch).await?;
        let receipt =
            persist_session_temporal_projection_batch_in_transaction(&transaction, &batch).await?;
        validate_next_progress(
            &transaction,
            &progress,
            batch.generation(),
            batch.batch_ordinal(),
            batch.item_count(),
        )
        .await?;
        validate_progress_binding(&binding, &progress)?;
        insert_progress_and_binding(&transaction, &progress, &batch).await?;
        touch_running_operation(
            &transaction,
            progress.session_id(),
            progress.operation_id(),
            progress.updated_at(),
        )
        .await?;
        transaction
            .commit()
            .await
            .map_err(|error| storage(PERSIST_REFRESH, error))?;
        Ok((progress, receipt))
    }

    pub async fn persist_session_refresh_progress_result(
        &self,
        progress: SessionRefreshProgressV1,
    ) -> SessionStoreResult<SessionRefreshProgressV1> {
        let transaction = self
            .begin_write_transaction()
            .await
            .map_err(|error| storage(PERSIST_REFRESH, error))?;
        let binding = require_running_binding(
            &transaction,
            progress.session_id(),
            progress.operation_id(),
            PERSIST_REFRESH,
        )
        .await?;
        validate_progress_binding(&binding, &progress)?;
        require_progress_timestamp(&progress)?;
        if let Some(existing) =
            read_progress(&transaction, progress.session_id(), progress.operation_id()).await?
        {
            if progress_logically_equal(&existing, &progress) {
                transaction
                    .commit()
                    .await
                    .map_err(|error| storage(PERSIST_REFRESH, error))?;
                return Ok(existing);
            }
            if existing.committed_batches() == progress.committed_batches() {
                return Err(SessionStoreError::IdempotencyConflict {
                    context: "refresh progress conflict",
                });
            }
        }
        let batch_ordinal = progress.committed_batches().checked_sub(1).ok_or(
            SessionStoreError::InvalidStateTransition {
                context: "refresh progress requires a projection batch",
            },
        )?;
        let batch_items = projection_receipt_item_count(
            &transaction,
            progress.session_id(),
            binding.generation,
            batch_ordinal,
        )
        .await?;
        validate_next_progress(
            &transaction,
            &progress,
            binding.generation,
            batch_ordinal,
            batch_items,
        )
        .await?;
        insert_progress(&transaction, &progress, batch_ordinal).await?;
        insert_batch_binding(
            &transaction,
            progress.session_id(),
            progress.operation_id(),
            batch_ordinal,
            binding.generation,
        )
        .await?;
        touch_running_operation(
            &transaction,
            progress.session_id(),
            progress.operation_id(),
            progress.updated_at(),
        )
        .await?;
        transaction
            .commit()
            .await
            .map_err(|error| storage(PERSIST_REFRESH, error))?;
        Ok(progress)
    }

    pub async fn session_refresh_progress_result(
        &self,
        request: SessionRefreshProgressRequestV1,
    ) -> SessionStoreResult<Option<SessionRefreshProgressV1>> {
        let snapshot = self
            .read_snapshot()
            .await
            .map_err(|error| storage(READ_REFRESH, error))?;
        read_progress(&snapshot, request.session_id(), request.operation_id()).await
    }

    pub async fn complete_session_refresh_result(
        &self,
        request: SessionRefreshCompletionRequestV1,
        execution_control: ExecutionControl,
    ) -> SessionStoreResult<SessionRefreshReceiptV1> {
        checkpoint_relation_rebuild_control(&execution_control)?;
        let snapshot = self
            .read_snapshot()
            .await
            .map_err(|error| storage(COMPLETE_REFRESH, error))?;
        if let Some(receipt) =
            read_receipt(&snapshot, request.session_id(), request.operation_id()).await?
        {
            require_exact_completion(&receipt, &request)?;
            checkpoint_relation_rebuild_control(&execution_control)?;
            return Ok(receipt);
        }
        drop(snapshot);

        let preflight = self
            .begin_write_transaction()
            .await
            .map_err(|error| storage(COMPLETE_REFRESH, error))?;
        let binding = require_running_binding(
            &preflight,
            request.session_id(),
            request.operation_id(),
            COMPLETE_REFRESH,
        )
        .await?;
        if request.frontier().committed_through() != binding.target_frontier {
            return Err(SessionStoreError::InvalidStateTransition {
                context: "refresh completion target coverage",
            });
        }
        preflight
            .commit()
            .await
            .map_err(|error| storage(COMPLETE_REFRESH, error))?;

        let relation_projection = rebuild_candidate_session_relations(
            self,
            request.session_id(),
            binding.generation,
            &execution_control,
            COMPLETE_REFRESH,
        )
        .await?;

        let transaction = self
            .begin_write_transaction()
            .await
            .map_err(|error| storage(COMPLETE_REFRESH, error))?;
        if let Some(receipt) =
            read_receipt(&transaction, request.session_id(), request.operation_id()).await?
        {
            require_exact_completion(&receipt, &request)?;
            transaction
                .commit()
                .await
                .map_err(|error| storage(COMPLETE_REFRESH, error))?;
            return Ok(receipt);
        }
        let binding = require_running_binding(
            &transaction,
            request.session_id(),
            request.operation_id(),
            COMPLETE_REFRESH,
        )
        .await?;
        let progress = require_exact_terminal_progress(
            &transaction,
            request.session_id(),
            request.operation_id(),
            request.frontier(),
            request.coverage(),
        )
        .await?;
        let mut request = request;
        if request.source_coverage().is_none()
            && let Some(source_coverage) = progress.source_coverage().cloned()
        {
            request = request.with_source_coverage(source_coverage);
        }
        if request.frontier().committed_through() != binding.target_frontier {
            return Err(SessionStoreError::InvalidStateTransition {
                context: "refresh completion target coverage",
            });
        }
        validate_final_projection_receipt(
            &transaction,
            request.session_id(),
            binding.generation,
            &binding.watermarks,
            &relation_projection,
        )
        .await?;
        validate_candidate_frontier(
            &transaction,
            request.session_id().as_str(),
            generation_i64(binding.generation, COMPLETE_REFRESH)?,
            binding.target_frontier,
            &relation_projection,
        )
        .await?;
        let terminal_at = terminal_timestamp(&progress, COMPLETE_REFRESH)?;
        activate_bound_generation(&transaction, request.session_id(), &binding, terminal_at)
            .await?;
        finish_operation(
            &transaction,
            request.session_id(),
            request.operation_id(),
            "complete",
            None,
            terminal_at,
        )
        .await?;
        insert_terminal_receipt(
            &transaction,
            request.session_id(),
            request.operation_id(),
            "complete",
            progress.frontier(),
            progress.coverage(),
            request
                .source_coverage()
                .or_else(|| progress.source_coverage()),
            None,
            terminal_at,
        )
        .await?;
        transaction
            .commit()
            .await
            .map_err(|error| storage(COMPLETE_REFRESH, error))?;
        Ok(SessionRefreshReceiptV1::completed(request, terminal_at))
    }

    pub async fn fail_session_refresh_result(
        &self,
        request: SessionRefreshFailureRequestV1,
    ) -> SessionStoreResult<SessionRefreshReceiptV1> {
        let transaction = self
            .begin_write_transaction()
            .await
            .map_err(|error| storage(FAIL_REFRESH, error))?;
        if let Some(receipt) =
            read_receipt(&transaction, request.session_id(), request.operation_id()).await?
        {
            require_exact_failure(&receipt, &request)?;
            transaction
                .commit()
                .await
                .map_err(|error| storage(FAIL_REFRESH, error))?;
            return Ok(receipt);
        }
        let binding = require_running_binding(
            &transaction,
            request.session_id(),
            request.operation_id(),
            FAIL_REFRESH,
        )
        .await?;
        let seeded_at = now_micros(FAIL_REFRESH)?;
        let progress = require_or_seed_terminal_progress(
            &transaction,
            &binding,
            request.session_id(),
            request.operation_id(),
            request.frontier(),
            request.coverage(),
            request.source_coverage(),
            seeded_at,
            FAIL_REFRESH,
        )
        .await?;
        let mut normalized_request = SessionRefreshFailureRequestV1::new(
            request.operation_id().clone(),
            request.session_id().clone(),
            request.frontier(),
            *request.coverage(),
            request.failure_code().as_str(),
        )?;
        if let Some(source_coverage) = progress.source_coverage().cloned() {
            normalized_request = normalized_request.with_source_coverage(source_coverage);
        }
        let request = normalized_request;
        let terminal_at = terminal_timestamp(&progress, FAIL_REFRESH)?;
        terminate_candidate(
            &transaction,
            request.session_id(),
            binding.generation,
            "failed",
            terminal_at,
            FAIL_REFRESH,
        )
        .await?;
        finish_operation(
            &transaction,
            request.session_id(),
            request.operation_id(),
            "failed",
            Some(request.failure_code().as_str()),
            terminal_at,
        )
        .await?;
        insert_terminal_receipt(
            &transaction,
            request.session_id(),
            request.operation_id(),
            "failed",
            progress.frontier(),
            progress.coverage(),
            request
                .source_coverage()
                .or_else(|| progress.source_coverage()),
            Some(request.failure_code().as_str()),
            terminal_at,
        )
        .await?;
        transaction
            .commit()
            .await
            .map_err(|error| storage(FAIL_REFRESH, error))?;
        Ok(SessionRefreshReceiptV1::failed(request, terminal_at))
    }

    pub async fn cancel_session_refresh_result(
        &self,
        request: SessionRefreshCancellationRequestV1,
    ) -> SessionStoreResult<SessionRefreshReceiptV1> {
        let transaction = self
            .begin_write_transaction()
            .await
            .map_err(|error| storage(CANCEL_REFRESH, error))?;
        if let Some(receipt) =
            read_receipt(&transaction, request.session_id(), request.operation_id()).await?
        {
            require_exact_cancellation(&receipt, &request)?;
            transaction
                .commit()
                .await
                .map_err(|error| storage(CANCEL_REFRESH, error))?;
            return Ok(receipt);
        }
        let binding = require_running_binding(
            &transaction,
            request.session_id(),
            request.operation_id(),
            CANCEL_REFRESH,
        )
        .await?;
        let seeded_at = now_micros(CANCEL_REFRESH)?;
        let progress = require_or_seed_terminal_progress(
            &transaction,
            &binding,
            request.session_id(),
            request.operation_id(),
            request.frontier(),
            request.coverage(),
            request.source_coverage(),
            seeded_at,
            CANCEL_REFRESH,
        )
        .await?;
        let mut normalized_request = SessionRefreshCancellationRequestV1::new(
            request.operation_id().clone(),
            request.session_id().clone(),
            request.frontier(),
            *request.coverage(),
        );
        if let Some(source_coverage) = progress.source_coverage().cloned() {
            normalized_request = normalized_request.with_source_coverage(source_coverage);
        }
        let request = normalized_request;
        let terminal_at = terminal_timestamp(&progress, CANCEL_REFRESH)?;
        terminate_candidate(
            &transaction,
            request.session_id(),
            binding.generation,
            "cancelled",
            terminal_at,
            CANCEL_REFRESH,
        )
        .await?;
        finish_operation(
            &transaction,
            request.session_id(),
            request.operation_id(),
            "cancelled",
            None,
            terminal_at,
        )
        .await?;
        insert_terminal_receipt(
            &transaction,
            request.session_id(),
            request.operation_id(),
            "cancelled",
            progress.frontier(),
            progress.coverage(),
            request
                .source_coverage()
                .or_else(|| progress.source_coverage()),
            None,
            terminal_at,
        )
        .await?;
        transaction
            .commit()
            .await
            .map_err(|error| storage(CANCEL_REFRESH, error))?;
        Ok(SessionRefreshReceiptV1::cancelled(request, terminal_at))
    }

    pub async fn session_refresh_receipt_result(
        &self,
        request: SessionRefreshReceiptRequestV1,
    ) -> SessionStoreResult<Option<SessionRefreshReceiptV1>> {
        let snapshot = self
            .read_snapshot()
            .await
            .map_err(|error| storage(READ_REFRESH, error))?;
        read_receipt(&snapshot, request.session_id(), request.operation_id()).await
    }

    pub async fn session_refresh_recovery_result(
        &self,
        session_id: &SessionId,
    ) -> SessionStoreResult<Option<SessionRefreshRecoveryV1>> {
        let snapshot = self
            .read_snapshot()
            .await
            .map_err(|error| storage(READ_REFRESH, error))?;
        let mut recoveries = read_running_recoveries(&snapshot, Some(session_id)).await?;
        Ok(recoveries.pop())
    }

    pub async fn running_session_refreshes_result(
        &self,
    ) -> SessionStoreResult<Vec<SessionRefreshRecoveryV1>> {
        let snapshot = self
            .read_snapshot()
            .await
            .map_err(|error| storage(READ_REFRESH, error))?;
        read_running_recoveries(&snapshot, None).await
    }
}

#[derive(Clone)]
struct OperationRow {
    operation_id: SessionRefreshOperationIdV1,
    created_at: UtcMicros,
}

#[derive(Clone)]
struct RefreshBinding {
    generation: SessionProjectionGenerationV1,
    source_frontier: u64,
    target_frontier: u64,
    watermarks: SessionFrozenWatermarksV1,
    projector_version: String,
    config_digest: String,
    binding_digest: String,
}

fn refresh_binding_digest(
    request: &SessionRefreshBeginOrJoinRequestV1,
) -> SessionStoreResult<String> {
    let encoded = serde_json::to_vec(&json!({
        "config": CONFIG_VERSION,
        "projector": PROJECTOR_VERSION,
        "scope": {"kind": "session_store"},
        "session_id": request.session_id().as_str(),
        "source_frontier": request.target_frontier().committed_through(),
        "target_frontier": request.target_frontier().observed_through(),
        "refresh_key": request.refresh_key(),
    }))
    .map_err(|error| storage(BEGIN_REFRESH, error))?;
    Ok(digest_bytes(&encoded))
}

fn config_digest() -> String {
    digest_bytes(CONFIG_VERSION.as_bytes())
}

fn operation_id_for_digest(
    digest: &str,
    attempt: u64,
) -> SessionStoreResult<SessionRefreshOperationIdV1> {
    let hex = digest.trim_start_matches("sha256:");
    let value = if attempt <= 1 {
        format!("refresh.{hex}")
    } else {
        format!("refresh.{hex}.{attempt}")
    };
    SessionRefreshOperationIdV1::new(value).map_err(SessionStoreError::from)
}

fn encode_frontier(frontier: SessionRefreshFrontierV1) -> String {
    json!({
        "committed_through": frontier.committed_through(),
        "observed_through": frontier.observed_through(),
    })
    .to_string()
}

fn encode_refresh_target(
    request: &SessionRefreshBeginOrJoinRequestV1,
) -> SessionStoreResult<String> {
    serde_json::to_string(&json!({
        "committed_through": request.target_frontier().committed_through(),
        "coverage_request": request.coverage_request(),
        "observed_through": request.target_frontier().observed_through(),
        "refresh_key": request.refresh_key(),
    }))
    .map_err(|error| storage(BEGIN_REFRESH, error))
}

fn decode_frontier(encoded: &str) -> SessionStoreResult<SessionRefreshFrontierV1> {
    let value: Value =
        serde_json::from_str(encoded).map_err(|error| storage(READ_REFRESH, error))?;
    let observed = value["observed_through"]
        .as_u64()
        .ok_or_else(|| storage_message(READ_REFRESH, "refresh observed frontier is invalid"))?;
    let committed = value["committed_through"]
        .as_u64()
        .ok_or_else(|| storage_message(READ_REFRESH, "refresh committed frontier is invalid"))?;
    SessionRefreshFrontierV1::new(observed, committed)
}

fn decode_refresh_target(
    encoded: &str,
) -> SessionStoreResult<(
    SessionRefreshFrontierV1,
    SessionTemporalCoverageRequestV1,
    Option<SessionRefreshKeyV1>,
)> {
    let value: Value =
        serde_json::from_str(encoded).map_err(|error| storage(READ_REFRESH, error))?;
    let frontier = decode_frontier(encoded)?;
    let coverage_request = value
        .get("coverage_request")
        .map(|value| {
            serde_json::from_value(value.clone()).map_err(|error| storage(READ_REFRESH, error))
        })
        .transpose()?
        .unwrap_or_else(|| SessionTemporalCoverageRequestV1::new(TemporalModeV1::Current));
    let refresh_key = value
        .get("refresh_key")
        .filter(|value| !value.is_null())
        .map(|value| {
            serde_json::from_value(value.clone()).map_err(|error| storage(READ_REFRESH, error))
        })
        .transpose()?;
    Ok((frontier, coverage_request, refresh_key))
}

fn decode_watermarks(encoded: &str) -> SessionStoreResult<SessionFrozenWatermarksV1> {
    let value: Value =
        serde_json::from_str(encoded).map_err(|error| storage(READ_REFRESH, error))?;
    let generation = decode_generation_value(&value, "active_generation")?;
    let source = decode_u64(&value, "source_frontier")?;
    let projection = decode_u64(&value, "projection_frontier")?;
    let summary = decode_u64(&value, "summary_frontier")?;
    let mut watermarks = SessionFrozenWatermarksV1::new(generation, source, projection, summary);
    if let Some(cursor) = value.get("cursor_key").filter(|value| !value.is_null()) {
        let key_id = cursor["key_id"]
            .as_str()
            .ok_or_else(|| storage_message(READ_REFRESH, "cursor key id is invalid"))?;
        let version = cursor["version"]
            .as_u64()
            .and_then(|value| u16::try_from(value).ok())
            .ok_or_else(|| storage_message(READ_REFRESH, "cursor key version is invalid"))?;
        watermarks = watermarks.with_cursor_key(SignedCursorKeyRefV1 {
            key_id: SessionCursorKeyIdV1::new(key_id).map_err(SessionStoreError::from)?,
            version: SessionCursorVersionV1::new(version).map_err(SessionStoreError::from)?,
        });
    }
    Ok(watermarks)
}

fn decode_generation_value(
    value: &Value,
    field: &'static str,
) -> SessionStoreResult<SessionProjectionGenerationV1> {
    SessionProjectionGenerationV1::new(decode_u64(value, field)?).map_err(SessionStoreError::from)
}

fn decode_u64(value: &Value, field: &'static str) -> SessionStoreResult<u64> {
    value[field]
        .as_u64()
        .ok_or_else(|| storage_message(READ_REFRESH, format!("{field} is invalid")))
}

fn encode_coverage(
    coverage: &TemporalCoverageCountsV1,
    source_coverage: Option<&SessionSourceCoverageReceiptV1>,
) -> String {
    json!({
        "hidden": coverage.hidden,
        "redacted": coverage.redacted,
        "source_coverage": source_coverage,
        "unknown": coverage.unknown,
        "visible": coverage.visible,
    })
    .to_string()
}

fn decode_coverage(
    encoded: &str,
) -> SessionStoreResult<(
    TemporalCoverageCountsV1,
    Option<SessionSourceCoverageReceiptV1>,
)> {
    let value: Value =
        serde_json::from_str(encoded).map_err(|error| storage(READ_REFRESH, error))?;
    let source_coverage = value
        .get("source_coverage")
        .filter(|value| !value.is_null())
        .cloned()
        .map(serde_json::from_value)
        .transpose()
        .map_err(|error| storage(READ_REFRESH, error))?;
    Ok((
        TemporalCoverageCountsV1 {
            visible: decode_u64(&value, "visible")?,
            hidden: decode_u64(&value, "hidden")?,
            unknown: decode_u64(&value, "unknown")?,
            redacted: decode_u64(&value, "redacted")?,
        },
        source_coverage,
    ))
}

async fn read_joinable_operation_by_digest(
    conn: &impl QueryExecutor,
    session_id: &SessionId,
    digest: &str,
) -> SessionStoreResult<Option<OperationRow>> {
    let mut rows = conn
        .query(
            "SELECT operation_id, created_at
             FROM session_refresh_operations
             WHERE session_id = ?1
               AND request_digest = ?2
               AND state IN ('running', 'complete')
             ORDER BY CASE state WHEN 'running' THEN 0 ELSE 1 END,
                      created_at, operation_id
             LIMIT 1",
            params![session_id.as_str(), digest],
        )
        .await
        .map_err(|error| storage(BEGIN_REFRESH, error))?;
    rows.next()
        .await
        .map_err(|error| storage(BEGIN_REFRESH, error))?
        .map(|row| {
            Ok(OperationRow {
                operation_id: SessionRefreshOperationIdV1::new(
                    row.get::<String>(0)
                        .map_err(|error| storage(BEGIN_REFRESH, error))?,
                )?,
                created_at: UtcMicros(row.get(1).map_err(|error| storage(BEGIN_REFRESH, error))?),
            })
        })
        .transpose()
}

async fn next_operation_attempt(
    conn: &impl QueryExecutor,
    session_id: &SessionId,
    digest: &str,
) -> SessionStoreResult<u64> {
    let mut rows = conn
        .query(
            "SELECT COUNT(*) FROM session_refresh_operations
             WHERE session_id = ?1 AND request_digest = ?2",
            params![session_id.as_str(), digest],
        )
        .await
        .map_err(|error| storage(BEGIN_REFRESH, error))?;
    let count: i64 = rows
        .next()
        .await
        .map_err(|error| storage(BEGIN_REFRESH, error))?
        .ok_or_else(|| storage_message(BEGIN_REFRESH, "refresh attempt count returned no row"))?
        .get(0)
        .map_err(|error| storage(BEGIN_REFRESH, error))?;
    let count = u64::try_from(count).map_err(|error| storage(BEGIN_REFRESH, error))?;
    Ok(count.saturating_add(1))
}

async fn read_running_operation(
    conn: &impl QueryExecutor,
    session_id: &SessionId,
) -> SessionStoreResult<Option<String>> {
    let mut rows = conn
        .query(
            "SELECT operation_id FROM session_refresh_operations
             WHERE session_id = ?1 AND state = 'running' LIMIT 1",
            params![session_id.as_str()],
        )
        .await
        .map_err(|error| storage(BEGIN_REFRESH, error))?;
    rows.next()
        .await
        .map_err(|error| storage(BEGIN_REFRESH, error))?
        .map(|row| row.get(0).map_err(|error| storage(BEGIN_REFRESH, error)))
        .transpose()
}

async fn ensure_active_generation(
    conn: &impl Executor,
    request: &SessionRefreshBeginOrJoinRequestV1,
) -> SessionStoreResult<(SessionProjectionGenerationV1, SessionFrozenWatermarksV1)> {
    let mut rows = conn
        .query(
            "SELECT generation, frozen_watermarks_json
             FROM session_temporal_generations
             WHERE session_id = ?1 AND state = 'active'",
            params![request.session_id().as_str()],
        )
        .await
        .map_err(|error| storage(BEGIN_REFRESH, error))?;
    if let Some(row) = rows
        .next()
        .await
        .map_err(|error| storage(BEGIN_REFRESH, error))?
    {
        let generation = decode_generation_i64(
            row.get(0).map_err(|error| storage(BEGIN_REFRESH, error))?,
            BEGIN_REFRESH,
        )?;
        let encoded: String = row.get(1).map_err(|error| storage(BEGIN_REFRESH, error))?;
        return Ok((generation, decode_watermarks(&encoded)?));
    }
    drop(rows);

    let generation = SessionProjectionGenerationV1::new(1)?;
    let watermarks = SessionFrozenWatermarksV1::new(
        generation,
        request.target_frontier().committed_through(),
        request.target_frontier().committed_through(),
        0,
    );
    let encoded = encode_watermarks(&watermarks, BEGIN_REFRESH)?;
    let recorded_at = now_micros(BEGIN_REFRESH)?;
    conn.execute(
        "INSERT INTO session_temporal_generations (
            session_id, generation, state, frozen_watermarks_json, created_at
         ) VALUES (?1, 1, 'building', ?2, ?3)",
        params![request.session_id().as_str(), encoded, recorded_at.0],
    )
    .await
    .map_err(|error| storage(BEGIN_REFRESH, error))?;
    conn.execute(
        "UPDATE session_temporal_generations
         SET state = 'ready', ready_at = ?2
         WHERE session_id = ?1 AND generation = 1 AND state = 'building'",
        params![request.session_id().as_str(), recorded_at.0],
    )
    .await
    .map_err(|error| storage(BEGIN_REFRESH, error))?;
    conn.execute(
        "UPDATE session_temporal_generations
         SET state = 'active', activated_at = ?2
         WHERE session_id = ?1 AND generation = 1 AND state = 'ready'",
        params![request.session_id().as_str(), recorded_at.0],
    )
    .await
    .map_err(|error| storage(BEGIN_REFRESH, error))?;
    Ok((generation, watermarks))
}

async fn next_generation(
    conn: &impl QueryExecutor,
    session_id: &SessionId,
) -> SessionStoreResult<SessionProjectionGenerationV1> {
    let mut rows = conn
        .query(
            "SELECT COALESCE(MAX(generation), 0) + 1
             FROM session_temporal_generations WHERE session_id = ?1",
            params![session_id.as_str()],
        )
        .await
        .map_err(|error| storage(BEGIN_REFRESH, error))?;
    let value: i64 = rows
        .next()
        .await
        .map_err(|error| storage(BEGIN_REFRESH, error))?
        .ok_or_else(|| storage_message(BEGIN_REFRESH, "next generation query returned no row"))?
        .get(0)
        .map_err(|error| storage(BEGIN_REFRESH, error))?;
    decode_generation_i64(value, BEGIN_REFRESH)
}

fn decode_generation_i64(
    value: i64,
    operation: &'static str,
) -> SessionStoreResult<SessionProjectionGenerationV1> {
    let value = u64::try_from(value).map_err(|error| storage(operation, error))?;
    SessionProjectionGenerationV1::new(value).map_err(SessionStoreError::from)
}

fn map_begin_conflict(error: tracedecay_runtime_core::db::engine::Error) -> SessionStoreError {
    let message = error.to_string();
    if message.contains("idx_session_refresh_operations_one_running")
        || message.contains("UNIQUE constraint failed: session_refresh_operations.session_id")
        || message.contains("UNIQUE constraint failed: session_refresh_operations.operation_id")
        || message.contains("PRIMARY KEY constraint failed: session_refresh_operations")
    {
        SessionStoreError::IdempotencyConflict {
            context: "session refresh busy",
        }
    } else {
        storage(BEGIN_REFRESH, error)
    }
}

fn progress_logically_equal(
    left: &SessionRefreshProgressV1,
    right: &SessionRefreshProgressV1,
) -> bool {
    left.operation_id() == right.operation_id()
        && left.session_id() == right.session_id()
        && left.frontier() == right.frontier()
        && left.coverage() == right.coverage()
        && left.source_coverage() == right.source_coverage()
        && left.committed_batches() == right.committed_batches()
        && left.committed_records() == right.committed_records()
}

fn require_progress_batch_ordinal(
    progress: &SessionRefreshProgressV1,
    batch_ordinal: u64,
) -> SessionStoreResult<()> {
    if progress.committed_batches() != batch_ordinal.saturating_add(1) {
        return Err(SessionStoreError::IdempotencyConflict {
            context: "refresh projection batch binding replay",
        });
    }
    Ok(())
}

fn require_progress_timestamp(progress: &SessionRefreshProgressV1) -> SessionStoreResult<()> {
    let now = now_micros(PERSIST_REFRESH)?;
    if progress.updated_at() > now {
        return Err(SessionStoreError::InvalidStateTransition {
            context: "refresh progress timestamp is in the future",
        });
    }
    Ok(())
}

fn terminal_timestamp(
    progress: &SessionRefreshProgressV1,
    operation: &'static str,
) -> SessionStoreResult<UtcMicros> {
    let now = now_micros(operation)?;
    if now < progress.updated_at() {
        Ok(progress.updated_at())
    } else {
        Ok(now)
    }
}

fn validate_progress_batch_identity(
    progress: &SessionRefreshProgressV1,
    batch: &SessionTemporalProjectionBatchV1,
) -> SessionStoreResult<()> {
    if progress.session_id() != batch.session_id() {
        return Err(SessionStoreError::SessionMismatch {
            context: "refresh projection batch",
        });
    }
    Ok(())
}

async fn require_running_binding(
    conn: &impl QueryExecutor,
    session_id: &SessionId,
    operation_id: &SessionRefreshOperationIdV1,
    operation: &'static str,
) -> SessionStoreResult<RefreshBinding> {
    let mut rows = conn
        .query(
            "SELECT binding.generation, binding.source_frontier, binding.target_frontier,
                    binding.frozen_watermarks_json, operation.state,
                    binding.projector_version, binding.config_digest, binding.binding_digest
             FROM session_refresh_bindings AS binding
             JOIN session_refresh_operations AS operation
               ON operation.session_id = binding.session_id
              AND operation.operation_id = binding.operation_id
             WHERE binding.session_id = ?1 AND binding.operation_id = ?2",
            params![session_id.as_str(), operation_id.as_str()],
        )
        .await
        .map_err(|error| storage(operation, error))?;
    let row = rows
        .next()
        .await
        .map_err(|error| storage(operation, error))?
        .ok_or_else(|| storage_message(operation, "session refresh binding is missing"))?;
    let state: String = row.get(4).map_err(|error| storage(operation, error))?;
    if state != "running" {
        return Err(SessionStoreError::InvalidRefreshState {
            operation_id: operation_id.clone(),
            state: decode_refresh_state(&state, operation)?,
        });
    }
    decode_binding(&row, operation)
}

fn decode_binding(row: &Row, operation: &'static str) -> SessionStoreResult<RefreshBinding> {
    let generation = decode_generation_i64(
        row.get(0).map_err(|error| storage(operation, error))?,
        operation,
    )?;
    let source_frontier = decode_nonnegative_i64(
        row.get(1).map_err(|error| storage(operation, error))?,
        operation,
    )?;
    let target_frontier = decode_nonnegative_i64(
        row.get(2).map_err(|error| storage(operation, error))?,
        operation,
    )?;
    let watermarks_json: String = row.get(3).map_err(|error| storage(operation, error))?;
    Ok(RefreshBinding {
        generation,
        source_frontier,
        target_frontier,
        watermarks: decode_watermarks(&watermarks_json)?,
        projector_version: row.get(5).map_err(|error| storage(operation, error))?,
        config_digest: row.get(6).map_err(|error| storage(operation, error))?,
        binding_digest: row.get(7).map_err(|error| storage(operation, error))?,
    })
}

fn decode_nonnegative_i64(value: i64, operation: &'static str) -> SessionStoreResult<u64> {
    u64::try_from(value).map_err(|error| storage(operation, error))
}

fn validate_batch_binding(
    binding: &RefreshBinding,
    batch: &SessionTemporalProjectionBatchV1,
) -> SessionStoreResult<()> {
    if batch.generation() != binding.generation {
        return Err(SessionStoreError::ProjectionBatchGenerationMismatch);
    }
    if batch.watermarks() != &binding.watermarks {
        return Err(SessionStoreError::FrozenWatermarkMismatch);
    }
    if batch.source_through() < binding.source_frontier
        || batch.source_through() > binding.target_frontier
        || batch.projection_through() > binding.target_frontier
    {
        return Err(SessionStoreError::FrozenWatermarkMismatch);
    }
    Ok(())
}

fn validate_progress_binding(
    binding: &RefreshBinding,
    progress: &SessionRefreshProgressV1,
) -> SessionStoreResult<()> {
    let frontier = progress.frontier();
    if frontier.observed_through() != binding.target_frontier
        || frontier.committed_through() < binding.source_frontier
        || frontier.committed_through() > binding.target_frontier
        || progress.coverage().total() != Some(progress.committed_records())
    {
        return Err(SessionStoreError::InvalidStateTransition {
            context: "refresh progress binding",
        });
    }
    Ok(())
}

async fn validate_next_progress(
    conn: &impl QueryExecutor,
    progress: &SessionRefreshProgressV1,
    generation: SessionProjectionGenerationV1,
    batch_ordinal: u64,
    batch_items: usize,
) -> SessionStoreResult<()> {
    if progress.committed_batches() != batch_ordinal.saturating_add(1) {
        return Err(SessionStoreError::InvalidStateTransition {
            context: "refresh progress batch ordinal",
        });
    }
    let batch_items =
        u64::try_from(batch_items).map_err(|error| storage(PERSIST_REFRESH, error))?;
    if let Some(previous) =
        read_progress(conn, progress.session_id(), progress.operation_id()).await?
    {
        previous.validate_successor(progress)?;
        if progress.committed_batches() != previous.committed_batches().saturating_add(1)
            || progress.committed_records()
                != previous.committed_records().saturating_add(batch_items)
            || progress.updated_at() <= previous.updated_at()
        {
            return Err(SessionStoreError::InvalidStateTransition {
                context: "refresh progress projection accounting",
            });
        }
    } else {
        let copy_count =
            projection_receipt_copy_count(conn, progress.session_id(), generation, batch_ordinal)
                .await?;
        let materialized_records = session_temporal_projection_record_count(
            conn,
            progress.session_id(),
            generation,
            copy_count,
        )
        .await?;
        if batch_ordinal != 0 || progress.committed_records() != materialized_records {
            return Err(SessionStoreError::InvalidStateTransition {
                context: "initial refresh progress projection accounting",
            });
        }
    }
    Ok(())
}

async fn projection_receipt_copy_count(
    conn: &impl QueryExecutor,
    session_id: &SessionId,
    generation: SessionProjectionGenerationV1,
    batch_ordinal: u64,
) -> SessionStoreResult<u64> {
    let mut rows = conn
        .query(
            "SELECT copy_count
             FROM session_temporal_projection_receipts
             WHERE session_id = ?1 AND generation = ?2 AND batch_ordinal = ?3",
            params![
                session_id.as_str(),
                generation_i64(generation, PERSIST_REFRESH)?,
                frontier_i64(batch_ordinal, PERSIST_REFRESH)?,
            ],
        )
        .await
        .map_err(|error| storage(PERSIST_REFRESH, error))?;
    let value = rows
        .next()
        .await
        .map_err(|error| storage(PERSIST_REFRESH, error))?
        .ok_or_else(|| {
            storage_message(
                PERSIST_REFRESH,
                "refresh projection copy receipt is unavailable",
            )
        })?
        .get::<i64>(0)
        .map_err(|error| storage(PERSIST_REFRESH, error))?;
    u64::try_from(value).map_err(|error| storage(PERSIST_REFRESH, error))
}

async fn insert_progress_and_binding(
    conn: &impl Executor,
    progress: &SessionRefreshProgressV1,
    batch: &SessionTemporalProjectionBatchV1,
) -> SessionStoreResult<()> {
    insert_progress(conn, progress, batch.batch_ordinal()).await?;
    insert_batch_binding(
        conn,
        progress.session_id(),
        progress.operation_id(),
        batch.batch_ordinal(),
        batch.generation(),
    )
    .await
}

async fn insert_progress(
    conn: &impl Executor,
    progress: &SessionRefreshProgressV1,
    progress_ordinal: u64,
) -> SessionStoreResult<()> {
    conn.execute(
        "INSERT INTO session_refresh_progress (
            session_id, operation_id, progress_ordinal, frontier_json, coverage_json,
            committed_batches, committed_records, recorded_at
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        params![
            progress.session_id().as_str(),
            progress.operation_id().as_str(),
            frontier_i64(progress_ordinal, PERSIST_REFRESH)?,
            encode_frontier(progress.frontier()),
            encode_coverage(progress.coverage(), progress.source_coverage()),
            frontier_i64(progress.committed_batches(), PERSIST_REFRESH)?,
            frontier_i64(progress.committed_records(), PERSIST_REFRESH)?,
            progress.updated_at().0,
        ],
    )
    .await
    .map_err(|error| storage(PERSIST_REFRESH, error))?;
    Ok(())
}

async fn insert_batch_binding(
    conn: &impl Executor,
    session_id: &SessionId,
    operation_id: &SessionRefreshOperationIdV1,
    progress_ordinal: u64,
    generation: SessionProjectionGenerationV1,
) -> SessionStoreResult<()> {
    conn.execute(
        "INSERT INTO session_refresh_batch_bindings (
            session_id, operation_id, progress_ordinal, generation, batch_ordinal
         ) VALUES (?1, ?2, ?3, ?4, ?3)",
        params![
            session_id.as_str(),
            operation_id.as_str(),
            frontier_i64(progress_ordinal, PERSIST_REFRESH)?,
            generation_i64(generation, PERSIST_REFRESH)?,
        ],
    )
    .await
    .map_err(|error| storage(PERSIST_REFRESH, error))?;
    Ok(())
}

async fn require_batch_binding(
    conn: &impl QueryExecutor,
    session_id: &SessionId,
    operation_id: &SessionRefreshOperationIdV1,
    batch_ordinal: u64,
    generation: SessionProjectionGenerationV1,
) -> SessionStoreResult<()> {
    let mut rows = conn
        .query(
            "SELECT 1 FROM session_refresh_batch_bindings
             WHERE session_id = ?1 AND operation_id = ?2
               AND progress_ordinal = ?3 AND generation = ?4 AND batch_ordinal = ?3",
            params![
                session_id.as_str(),
                operation_id.as_str(),
                frontier_i64(batch_ordinal, PERSIST_REFRESH)?,
                generation_i64(generation, PERSIST_REFRESH)?,
            ],
        )
        .await
        .map_err(|error| storage(PERSIST_REFRESH, error))?;
    if rows
        .next()
        .await
        .map_err(|error| storage(PERSIST_REFRESH, error))?
        .is_none()
    {
        return Err(SessionStoreError::IdempotencyConflict {
            context: "refresh projection batch binding replay",
        });
    }
    Ok(())
}

async fn projection_receipt_item_count(
    conn: &impl QueryExecutor,
    session_id: &SessionId,
    generation: SessionProjectionGenerationV1,
    batch_ordinal: u64,
) -> SessionStoreResult<usize> {
    let mut rows = conn
        .query(
            "SELECT current.occurrence_count + current.copy_count + current.assertion_count,
                    previous.occurrence_count + previous.copy_count + previous.assertion_count
             FROM session_temporal_projection_receipts AS current
             LEFT JOIN session_temporal_projection_receipts AS previous
               ON previous.session_id = current.session_id
              AND previous.generation = current.generation
              AND previous.batch_ordinal = current.batch_ordinal - 1
             WHERE current.session_id = ?1
               AND current.generation = ?2
               AND current.batch_ordinal = ?3",
            params![
                session_id.as_str(),
                generation_i64(generation, PERSIST_REFRESH)?,
                frontier_i64(batch_ordinal, PERSIST_REFRESH)?,
            ],
        )
        .await
        .map_err(|error| storage(PERSIST_REFRESH, error))?;
    let Some(row) = rows
        .next()
        .await
        .map_err(|error| storage(PERSIST_REFRESH, error))?
    else {
        return Err(SessionStoreError::InvalidStateTransition {
            context: "refresh progress projection receipt",
        });
    };
    let current: i64 = row
        .get(0)
        .map_err(|error| storage(PERSIST_REFRESH, error))?;
    let previous: Option<i64> = row
        .get(1)
        .map_err(|error| storage(PERSIST_REFRESH, error))?;
    let delta = if batch_ordinal == 0 {
        current
    } else {
        let previous = previous.ok_or(SessionStoreError::InvalidStateTransition {
            context: "refresh progress predecessor projection receipt",
        })?;
        current
            .checked_sub(previous)
            .filter(|value| *value >= 0)
            .ok_or(SessionStoreError::InvalidStateTransition {
                context: "refresh progress projection receipt is non-monotonic",
            })?
    };
    usize::try_from(delta).map_err(|error| storage(PERSIST_REFRESH, error))
}

async fn read_progress(
    conn: &impl QueryExecutor,
    session_id: &SessionId,
    operation_id: &SessionRefreshOperationIdV1,
) -> SessionStoreResult<Option<SessionRefreshProgressV1>> {
    let mut rows = conn
        .query(
            "SELECT frontier_json, coverage_json, committed_batches,
                    committed_records, recorded_at
             FROM session_refresh_progress
             WHERE session_id = ?1 AND operation_id = ?2
             ORDER BY progress_ordinal DESC LIMIT 1",
            params![session_id.as_str(), operation_id.as_str()],
        )
        .await
        .map_err(|error| storage(READ_REFRESH, error))?;
    rows.next()
        .await
        .map_err(|error| storage(READ_REFRESH, error))?
        .map(|row| decode_progress(&row, operation_id.clone(), session_id.clone()))
        .transpose()
}

fn decode_progress(
    row: &Row,
    operation_id: SessionRefreshOperationIdV1,
    session_id: SessionId,
) -> SessionStoreResult<SessionRefreshProgressV1> {
    let frontier_json: String = row.get(0).map_err(|error| storage(READ_REFRESH, error))?;
    let coverage_json: String = row.get(1).map_err(|error| storage(READ_REFRESH, error))?;
    let (coverage, source_coverage) = decode_coverage(&coverage_json)?;
    let progress = SessionRefreshProgressV1::new(
        operation_id,
        session_id,
        decode_frontier(&frontier_json)?,
        coverage,
        decode_nonnegative_i64(
            row.get(2).map_err(|error| storage(READ_REFRESH, error))?,
            READ_REFRESH,
        )?,
        decode_nonnegative_i64(
            row.get(3).map_err(|error| storage(READ_REFRESH, error))?,
            READ_REFRESH,
        )?,
        UtcMicros(row.get(4).map_err(|error| storage(READ_REFRESH, error))?),
    );
    Ok(match source_coverage {
        Some(source_coverage) => progress.with_source_coverage(source_coverage),
        None => progress,
    })
}

async fn require_exact_terminal_progress(
    conn: &impl QueryExecutor,
    session_id: &SessionId,
    operation_id: &SessionRefreshOperationIdV1,
    frontier: SessionRefreshFrontierV1,
    coverage: &TemporalCoverageCountsV1,
) -> SessionStoreResult<SessionRefreshProgressV1> {
    let progress = read_progress(conn, session_id, operation_id).await?.ok_or(
        SessionStoreError::InvalidStateTransition {
            context: "refresh terminal transition requires durable progress",
        },
    )?;
    if progress.frontier() != frontier || progress.coverage() != coverage {
        return Err(SessionStoreError::InvalidStateTransition {
            context: "refresh terminal transition must use last durable progress",
        });
    }
    Ok(progress)
}

#[allow(clippy::too_many_arguments)]
async fn require_or_seed_terminal_progress(
    conn: &impl Executor,
    binding: &RefreshBinding,
    session_id: &SessionId,
    operation_id: &SessionRefreshOperationIdV1,
    frontier: SessionRefreshFrontierV1,
    coverage: &TemporalCoverageCountsV1,
    source_coverage: Option<&SessionSourceCoverageReceiptV1>,
    terminal_at: UtcMicros,
    operation: &'static str,
) -> SessionStoreResult<SessionRefreshProgressV1> {
    if let Some(progress) = read_progress(conn, session_id, operation_id).await? {
        if progress.frontier() != frontier || progress.coverage() != coverage {
            return Err(SessionStoreError::InvalidStateTransition {
                context: "refresh terminal transition must use last durable progress",
            });
        }
        return Ok(progress);
    }
    let empty_frontier =
        SessionRefreshFrontierV1::new(binding.target_frontier, binding.source_frontier)?;
    let empty_coverage = TemporalCoverageCountsV1 {
        visible: 0,
        hidden: 0,
        unknown: 0,
        redacted: 0,
    };
    if frontier != empty_frontier || coverage != &empty_coverage {
        return Err(SessionStoreError::InvalidStateTransition {
            context: "refresh terminal transition requires durable progress",
        });
    }
    let progress = SessionRefreshProgressV1::new(
        operation_id.clone(),
        session_id.clone(),
        empty_frontier,
        empty_coverage,
        0,
        0,
        terminal_at,
    );
    let progress = match source_coverage {
        Some(source_coverage) => progress.with_source_coverage(source_coverage.clone()),
        None => progress,
    };
    insert_progress(conn, &progress, 0)
        .await
        .map_err(|error| match error {
            SessionStoreError::Storage { .. } => storage_message(
                operation,
                "failed to seed empty refresh progress for terminal transition",
            ),
            other => other,
        })?;
    Ok(progress)
}

async fn touch_running_operation(
    conn: &impl Executor,
    session_id: &SessionId,
    operation_id: &SessionRefreshOperationIdV1,
    updated_at: UtcMicros,
) -> SessionStoreResult<()> {
    conn.execute(
        "UPDATE session_refresh_operations
         SET updated_at = ?3
         WHERE session_id = ?1
           AND operation_id = ?2
           AND state = 'running'
           AND updated_at <= ?3",
        params![session_id.as_str(), operation_id.as_str(), updated_at.0],
    )
    .await
    .map_err(|error| storage(PERSIST_REFRESH, error))?;
    Ok(())
}

async fn activate_bound_generation(
    conn: &impl Executor,
    session_id: &SessionId,
    binding: &RefreshBinding,
    terminal_at: UtcMicros,
) -> SessionStoreResult<()> {
    let generation = generation_i64(binding.generation, COMPLETE_REFRESH)?;
    let candidate = read_generation(conn, session_id, binding.generation, COMPLETE_REFRESH)
        .await?
        .ok_or(SessionStoreError::MissingGeneration {
            generation: binding.generation,
        })?;
    if candidate.frozen_watermarks_json != encode_watermarks(&binding.watermarks, COMPLETE_REFRESH)?
    {
        return Err(SessionStoreError::FrozenWatermarkMismatch);
    }
    if candidate.state == "building" {
        conn.execute(
            "UPDATE session_temporal_generations
             SET state = 'ready', ready_at = ?3
             WHERE session_id = ?1 AND generation = ?2 AND state = 'building'",
            params![session_id.as_str(), generation, terminal_at.0],
        )
        .await
        .map_err(|error| storage(COMPLETE_REFRESH, error))?;
    }
    conn.execute(
        "UPDATE session_temporal_generations
         SET state = 'superseded', completed_at = ?3
         WHERE session_id = ?1 AND generation <> ?2 AND state = 'active'",
        params![session_id.as_str(), generation, terminal_at.0],
    )
    .await
    .map_err(|error| storage(COMPLETE_REFRESH, error))?;
    let changed = conn
        .execute(
            "UPDATE session_temporal_generations
             SET state = 'active', activated_at = ?3
             WHERE session_id = ?1 AND generation = ?2 AND state = 'ready'",
            params![session_id.as_str(), generation, terminal_at.0],
        )
        .await
        .map_err(|error| storage(COMPLETE_REFRESH, error))?;
    if changed != 1 {
        return Err(SessionStoreError::InvalidStateTransition {
            context: "refresh candidate activation",
        });
    }
    Ok(())
}

async fn terminate_candidate(
    conn: &impl Executor,
    session_id: &SessionId,
    generation: SessionProjectionGenerationV1,
    state: &str,
    terminal_at: UtcMicros,
    operation: &'static str,
) -> SessionStoreResult<()> {
    let changed = conn
        .execute(
            "UPDATE session_temporal_generations
             SET state = ?3, completed_at = ?4
             WHERE session_id = ?1 AND generation = ?2 AND state IN ('building', 'ready')",
            params![
                session_id.as_str(),
                generation_i64(generation, operation)?,
                state,
                terminal_at.0,
            ],
        )
        .await
        .map_err(|error| storage(operation, error))?;
    if changed != 1 {
        return Err(SessionStoreError::InvalidStateTransition {
            context: "refresh candidate termination",
        });
    }
    Ok(())
}

async fn finish_operation(
    conn: &impl Executor,
    session_id: &SessionId,
    operation_id: &SessionRefreshOperationIdV1,
    state: &str,
    failure_code: Option<&str>,
    terminal_at: UtcMicros,
) -> SessionStoreResult<()> {
    let changed = conn
        .execute(
            "UPDATE session_refresh_operations
             SET state = ?3, updated_at = ?4, terminal_at = ?4, failure_code = ?5
             WHERE session_id = ?1 AND operation_id = ?2 AND state = 'running'",
            params![
                session_id.as_str(),
                operation_id.as_str(),
                state,
                terminal_at.0,
                failure_code,
            ],
        )
        .await
        .map_err(|error| storage(READ_REFRESH, error))?;
    if changed != 1 {
        return Err(SessionStoreError::InvalidStateTransition {
            context: "refresh operation termination",
        });
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn insert_terminal_receipt(
    conn: &impl Executor,
    session_id: &SessionId,
    operation_id: &SessionRefreshOperationIdV1,
    state: &str,
    frontier: SessionRefreshFrontierV1,
    coverage: &TemporalCoverageCountsV1,
    source_coverage: Option<&SessionSourceCoverageReceiptV1>,
    failure_code: Option<&str>,
    terminal_at: UtcMicros,
) -> SessionStoreResult<()> {
    conn.execute(
        "INSERT INTO session_refresh_receipts (
            session_id, operation_id, terminal_state, frontier_json,
            coverage_json, failure_code, terminal_at
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![
            session_id.as_str(),
            operation_id.as_str(),
            state,
            encode_frontier(frontier),
            encode_coverage(coverage, source_coverage),
            failure_code,
            terminal_at.0,
        ],
    )
    .await
    .map_err(|error| storage(READ_REFRESH, error))?;
    Ok(())
}

async fn read_receipt(
    conn: &impl QueryExecutor,
    session_id: &SessionId,
    operation_id: &SessionRefreshOperationIdV1,
) -> SessionStoreResult<Option<SessionRefreshReceiptV1>> {
    let mut rows = conn
        .query(
            "SELECT terminal_state, frontier_json, coverage_json, failure_code, terminal_at
             FROM session_refresh_receipts
             WHERE session_id = ?1 AND operation_id = ?2",
            params![session_id.as_str(), operation_id.as_str()],
        )
        .await
        .map_err(|error| storage(READ_REFRESH, error))?;
    rows.next()
        .await
        .map_err(|error| storage(READ_REFRESH, error))?
        .map(|row| decode_receipt(&row, operation_id.clone(), session_id.clone()))
        .transpose()
}

fn decode_receipt(
    row: &Row,
    operation_id: SessionRefreshOperationIdV1,
    session_id: SessionId,
) -> SessionStoreResult<SessionRefreshReceiptV1> {
    let state: String = row.get(0).map_err(|error| storage(READ_REFRESH, error))?;
    let frontier_json: String = row.get(1).map_err(|error| storage(READ_REFRESH, error))?;
    let coverage_json: String = row.get(2).map_err(|error| storage(READ_REFRESH, error))?;
    let failure_code: Option<String> = row.get(3).map_err(|error| storage(READ_REFRESH, error))?;
    let terminal_at = UtcMicros(row.get(4).map_err(|error| storage(READ_REFRESH, error))?);
    let frontier = decode_frontier(&frontier_json)?;
    let (coverage, source_coverage) = decode_coverage(&coverage_json)?;
    match state.as_str() {
        "complete" => {
            let request = SessionRefreshCompletionRequestV1::new(
                operation_id,
                session_id,
                frontier,
                coverage,
            )?;
            let request = match source_coverage {
                Some(source_coverage) => request.with_source_coverage(source_coverage),
                None => request,
            };
            Ok(SessionRefreshReceiptV1::completed(request, terminal_at))
        }
        "failed" => {
            let request = SessionRefreshFailureRequestV1::new(
                operation_id,
                session_id,
                frontier,
                coverage,
                failure_code.ok_or_else(|| {
                    storage_message(READ_REFRESH, "failed refresh receipt has no failure code")
                })?,
            )?;
            let request = match source_coverage {
                Some(source_coverage) => request.with_source_coverage(source_coverage),
                None => request,
            };
            Ok(SessionRefreshReceiptV1::failed(request, terminal_at))
        }
        "cancelled" => {
            let request = SessionRefreshCancellationRequestV1::new(
                operation_id,
                session_id,
                frontier,
                coverage,
            );
            let request = match source_coverage {
                Some(source_coverage) => request.with_source_coverage(source_coverage),
                None => request,
            };
            Ok(SessionRefreshReceiptV1::cancelled(request, terminal_at))
        }
        _ => Err(storage_message(
            READ_REFRESH,
            "refresh receipt terminal state is invalid",
        )),
    }
}

fn require_exact_completion(
    receipt: &SessionRefreshReceiptV1,
    request: &SessionRefreshCompletionRequestV1,
) -> SessionStoreResult<()> {
    require_exact_terminal(
        receipt,
        request.operation_id(),
        request.session_id(),
        request.frontier(),
        request.coverage(),
        request.source_coverage(),
        SessionRefreshTerminalStateV1::Complete,
        None,
    )
}

fn require_exact_failure(
    receipt: &SessionRefreshReceiptV1,
    request: &SessionRefreshFailureRequestV1,
) -> SessionStoreResult<()> {
    require_exact_terminal(
        receipt,
        request.operation_id(),
        request.session_id(),
        request.frontier(),
        request.coverage(),
        request.source_coverage(),
        SessionRefreshTerminalStateV1::Failed,
        Some(request.failure_code()),
    )
}

fn require_exact_cancellation(
    receipt: &SessionRefreshReceiptV1,
    request: &SessionRefreshCancellationRequestV1,
) -> SessionStoreResult<()> {
    require_exact_terminal(
        receipt,
        request.operation_id(),
        request.session_id(),
        request.frontier(),
        request.coverage(),
        request.source_coverage(),
        SessionRefreshTerminalStateV1::Cancelled,
        None,
    )
}

#[allow(clippy::too_many_arguments)]
fn require_exact_terminal(
    receipt: &SessionRefreshReceiptV1,
    operation_id: &SessionRefreshOperationIdV1,
    session_id: &SessionId,
    frontier: SessionRefreshFrontierV1,
    coverage: &TemporalCoverageCountsV1,
    source_coverage: Option<&SessionSourceCoverageReceiptV1>,
    state: SessionRefreshTerminalStateV1,
    failure_code: Option<&SessionRefreshFailureCodeV1>,
) -> SessionStoreResult<()> {
    if receipt.operation_id() != operation_id
        || receipt.session_id() != session_id
        || receipt.frontier() != frontier
        || receipt.coverage() != coverage
        || source_coverage.is_some_and(|expected| receipt.source_coverage() != Some(expected))
        || receipt.state() != state
        || receipt.failure_code() != failure_code
    {
        return Err(SessionStoreError::IdempotencyConflict {
            context: "refresh terminal retry",
        });
    }
    Ok(())
}

async fn read_running_recoveries(
    conn: &impl QueryExecutor,
    session_filter: Option<&SessionId>,
) -> SessionStoreResult<Vec<SessionRefreshRecoveryV1>> {
    let mut rows = conn
        .query(
            "SELECT operation.operation_id, operation.session_id,
                    COALESCE((
                        SELECT MIN(source.provider)
                        FROM sessions AS source
                        WHERE source.session_id = operation.session_id
                    ), 'all'),
                    operation.target_frontier_json, binding.generation,
                    binding.source_frontier, binding.target_frontier,
                    binding.frozen_watermarks_json, binding.projector_version,
                    binding.config_digest, binding.binding_digest
             FROM session_refresh_operations AS operation
             JOIN session_refresh_bindings AS binding
               ON binding.session_id = operation.session_id
              AND binding.operation_id = operation.operation_id
             WHERE operation.state = 'running'
               AND (?1 IS NULL OR operation.session_id = ?1)
             ORDER BY operation.created_at, operation.session_id, operation.operation_id
             LIMIT ?2",
            params![
                session_filter.map(SessionId::as_str),
                if session_filter.is_some() {
                    1_i64
                } else {
                    MAX_RUNNING_REFRESH_RECOVERIES
                },
            ],
        )
        .await
        .map_err(|error| storage(READ_REFRESH, error))?;
    let mut pending = Vec::new();
    while let Some(row) = rows
        .next()
        .await
        .map_err(|error| storage(READ_REFRESH, error))?
    {
        let operation_id = SessionRefreshOperationIdV1::new(
            row.get::<String>(0)
                .map_err(|error| storage(READ_REFRESH, error))?,
        )?;
        let session_id = SessionId::new(
            row.get::<String>(1)
                .map_err(|error| storage(READ_REFRESH, error))?,
        )
        .map_err(|error| storage(READ_REFRESH, error))?;
        let provider: String = row.get(2).map_err(|error| storage(READ_REFRESH, error))?;
        let target_frontier_json: String =
            row.get(3).map_err(|error| storage(READ_REFRESH, error))?;
        let (target_frontier, coverage_request, refresh_key) =
            decode_refresh_target(&target_frontier_json)?;
        let source_targets = match refresh_key {
            Some(refresh_key) => refresh_key.sources().to_vec(),
            None => vec![
                SessionRefreshSourceTargetV1::new(
                    SessionSourceIdV1::new(format!("{}:{provider}", session_id.as_str()))
                        .map_err(SessionStoreError::from)?,
                    SessionSourceFrontierV1::new(target_frontier.observed_through()),
                    SessionSourceFrontierV1::new(target_frontier.observed_through()),
                )
                .map_err(SessionStoreError::from)?,
            ],
        };
        let generation = decode_generation_i64(
            row.get(4).map_err(|error| storage(READ_REFRESH, error))?,
            READ_REFRESH,
        )?;
        let source_frontier = decode_nonnegative_i64(
            row.get(5).map_err(|error| storage(READ_REFRESH, error))?,
            READ_REFRESH,
        )?;
        let binding_target_frontier = decode_nonnegative_i64(
            row.get(6).map_err(|error| storage(READ_REFRESH, error))?,
            READ_REFRESH,
        )?;
        let watermarks_json: String = row.get(7).map_err(|error| storage(READ_REFRESH, error))?;
        let binding = RefreshBinding {
            generation,
            source_frontier,
            target_frontier: binding_target_frontier,
            watermarks: decode_watermarks(&watermarks_json)?,
            projector_version: row.get(8).map_err(|error| storage(READ_REFRESH, error))?,
            config_digest: row.get(9).map_err(|error| storage(READ_REFRESH, error))?,
            binding_digest: row.get(10).map_err(|error| storage(READ_REFRESH, error))?,
        };
        pending.push((
            operation_id,
            session_id,
            source_targets,
            coverage_request,
            target_frontier,
            binding,
        ));
    }
    drop(rows);

    let mut recoveries = Vec::with_capacity(pending.len());
    for (operation_id, session_id, source_targets, coverage_request, target_frontier, binding) in
        pending
    {
        let progress = read_progress(conn, &session_id, &operation_id).await?;
        let restart_state = match progress.as_ref() {
            None => SessionRefreshRestartStateV1::BeginProjection,
            Some(progress)
                if progress.frontier().is_complete()
                    && progress.committed_batches() > 0
                    && projection_receipt_exists(
                        conn,
                        &session_id,
                        binding.generation,
                        progress.committed_batches().saturating_sub(1),
                    )
                    .await? =>
            {
                SessionRefreshRestartStateV1::ReadyToComplete
            }
            Some(progress) => SessionRefreshRestartStateV1::ResumeProjection {
                next_batch_ordinal: progress.committed_batches(),
            },
        };
        recoveries.push(SessionRefreshRecoveryV1 {
            operation_id,
            session_id,
            source_targets,
            coverage_request,
            source_frontier: binding.source_frontier,
            target_frontier,
            candidate_generation: binding.generation,
            frozen_watermarks: binding.watermarks,
            projector_version: binding.projector_version,
            config_digest: binding.config_digest,
            binding_digest: binding.binding_digest,
            progress,
            restart_state,
        });
    }
    Ok(recoveries)
}

async fn projection_receipt_exists(
    conn: &impl QueryExecutor,
    session_id: &SessionId,
    generation: SessionProjectionGenerationV1,
    batch_ordinal: u64,
) -> SessionStoreResult<bool> {
    let mut rows = conn
        .query(
            "SELECT 1 FROM session_temporal_projection_receipts
             WHERE session_id = ?1 AND generation = ?2 AND batch_ordinal = ?3",
            params![
                session_id.as_str(),
                generation_i64(generation, READ_REFRESH)?,
                frontier_i64(batch_ordinal, READ_REFRESH)?,
            ],
        )
        .await
        .map_err(|error| storage(READ_REFRESH, error))?;
    Ok(rows
        .next()
        .await
        .map_err(|error| storage(READ_REFRESH, error))?
        .is_some())
}

fn decode_refresh_state(
    state: &str,
    operation: &'static str,
) -> SessionStoreResult<SessionRefreshStateV1> {
    match state {
        "running" => Ok(SessionRefreshStateV1::Running),
        "complete" => Ok(SessionRefreshStateV1::Complete),
        "failed" => Ok(SessionRefreshStateV1::Failed),
        "cancelled" => Ok(SessionRefreshStateV1::Cancelled),
        _ => Err(storage_message(
            operation,
            "refresh operation state is invalid",
        )),
    }
}
