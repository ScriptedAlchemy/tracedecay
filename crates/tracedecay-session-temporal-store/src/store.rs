use std::{
    future::Future,
    sync::{Arc, LazyLock},
};

use tracedecay_graph_db::GraphCancellation;
use tracedecay_temporal_query::ports::ExecutionControl;

use tracedecay_store::{
    SessionGenerationActivatePermit, SessionGenerationActivationReceiptV1,
    SessionGenerationActivationRequestV1, SessionGenerationRebuildBeginPermit,
    SessionGenerationRebuildReceiptV1, SessionGenerationRebuildRequestV1,
    SessionProjectionBatchPersistPermit, SessionRefreshBeginOrJoinPermit,
    SessionRefreshBeginOrJoinReceiptV1, SessionRefreshBeginOrJoinRequestV1,
    SessionRefreshCancelPermit, SessionRefreshCancellationRequestV1, SessionRefreshCompletePermit,
    SessionRefreshCompletionRequestV1, SessionRefreshFailPermit, SessionRefreshFailureRequestV1,
    SessionRefreshProgressPersistPermit, SessionRefreshProgressReadPermit,
    SessionRefreshProgressRequestV1, SessionRefreshProgressV1, SessionRefreshReceiptReadPermit,
    SessionRefreshReceiptRequestV1, SessionRefreshReceiptV1, SessionRefreshStore,
    SessionRetrievalPageV1, SessionRetrievalStore, SessionSnapshotFreezePermit, SessionStoreResult,
    SessionTemporalCapabilitiesV1, SessionTemporalCapabilityProvider, SessionTemporalCapabilityV1,
    SessionTemporalPageRetrievePermit, SessionTemporalProjectionBatchReceiptV1,
    SessionTemporalProjectionBatchV1, SessionTemporalProjectionStore,
    SessionTemporalRetrievalRequestV1, SessionTemporalSnapshotRequestV1, SessionTemporalSnapshotV1,
};

use super::refresh::SessionRefreshRecoveryV1;
#[cfg(any(test, feature = "test-helpers"))]
use super::refresh::SessionRefreshRestartStateV1;
use crate::handle::{SessionTemporalAccess, SessionTemporalRegisteredDb};
#[cfg(any(test, feature = "test-helpers"))]
use tracedecay_store::SessionStoreError;

/// Session-temporal projection adapter over an already-open authoritative database.
pub struct GlobalDbSessionTemporalStore<'a, D: SessionTemporalRegisteredDb> {
    db: &'a D,
}

#[derive(Clone, Debug)]
struct ExecutionControlGraphCancellation(ExecutionControl);

impl GraphCancellation for ExecutionControlGraphCancellation {
    fn is_cancelled(&self) -> bool {
        self.0.checkpoint().is_err()
    }
}

/// Adapts the request's live cancellation signal for bounded Grafeo calls.
///
/// `GraphCancellation` has no error channel, so it treats cancellation,
/// deadline expiry, and work-budget exhaustion as an interruption. Every
/// caller must checkpoint the same [`ExecutionControl`] immediately before and
/// after its graph operation to restore the original typed reason. This
/// adapter preserves all three interruption paths during traversal without
/// manufacturing a default control.
pub fn execution_control_graph_cancellation(
    control: &ExecutionControl,
) -> Arc<dyn GraphCancellation> {
    Arc::new(ExecutionControlGraphCancellation(control.clone()))
}

impl<'a, D: SessionTemporalRegisteredDb + Sync> GlobalDbSessionTemporalStore<'a, D> {
    #[hotpath::skip]
    pub const fn new(db: &'a D) -> Self {
        Self { db }
    }

    fn access(&self) -> SessionTemporalAccess<'_, D> {
        SessionTemporalAccess::new(self.db)
    }

    #[hotpath::skip]
    pub async fn persist_session_refresh_projection_batch(
        &self,
        progress: SessionRefreshProgressV1,
        batch: SessionTemporalProjectionBatchV1,
    ) -> SessionStoreResult<(
        SessionRefreshProgressV1,
        SessionTemporalProjectionBatchReceiptV1,
    )> {
        self.access()
            .persist_session_refresh_projection_batch_result(progress, batch)
            .await
    }

    #[hotpath::skip]
    pub async fn persist_session_refresh_projection_batch_controlled(
        &self,
        progress: SessionRefreshProgressV1,
        batch: SessionTemporalProjectionBatchV1,
        execution_control: ExecutionControl,
    ) -> SessionStoreResult<(
        SessionRefreshProgressV1,
        SessionTemporalProjectionBatchReceiptV1,
    )> {
        self.access()
            .persist_session_refresh_projection_batch_controlled_result(
                progress,
                batch,
                execution_control,
            )
            .await
    }

    #[hotpath::skip]
    pub async fn session_refresh_recovery(
        &self,
        session_id: &tracedecay_domain::SessionId,
    ) -> SessionStoreResult<Option<SessionRefreshRecoveryV1>> {
        self.access()
            .session_refresh_recovery_result(session_id)
            .await
    }

    #[hotpath::skip]
    pub async fn running_session_refreshes(
        &self,
    ) -> SessionStoreResult<Vec<SessionRefreshRecoveryV1>> {
        self.access().running_session_refreshes_result().await
    }

    #[cfg(any(test, feature = "test-helpers"))]
    #[hotpath::skip]
    pub async fn materialize_session_temporal_refresh_batch_for_test(
        &self,
        recovery: &SessionRefreshRecoveryV1,
    ) -> SessionStoreResult<Option<(SessionRefreshProgressV1, SessionTemporalProjectionBatchV1)>>
    {
        self.access()
            .materialize_session_temporal_refresh_batch_result(recovery)
            .await
    }

    #[cfg(any(test, feature = "test-helpers"))]
    #[hotpath::skip]
    pub async fn materialize_pending_session_refresh_for_test(
        &self,
        session_id: &tracedecay_domain::SessionId,
    ) -> SessionStoreResult<()> {
        let request = SessionTemporalAccess::new(self.db)
            .pending_session_temporal_refresh_page_result(128, 0, None)
            .await?
            .into_parts()
            .0
            .into_iter()
            .find(|request| request.session_id() == session_id)
            .ok_or(SessionStoreError::InvalidStateTransition {
                context: "test temporal fixture pending refresh",
            })?;
        self.begin_or_join_session_refresh(request).await?;

        loop {
            let recovery = self.session_refresh_recovery(session_id).await?.ok_or(
                SessionStoreError::InvalidStateTransition {
                    context: "test temporal fixture refresh recovery",
                },
            )?;
            match recovery.restart_state() {
                SessionRefreshRestartStateV1::ReadyToComplete => {
                    let progress =
                        recovery
                            .progress()
                            .ok_or(SessionStoreError::InvalidStateTransition {
                                context: "test temporal fixture refresh progress",
                            })?;
                    let mut request = SessionRefreshCompletionRequestV1::new(
                        recovery.operation_id().clone(),
                        recovery.session_id().clone(),
                        progress.frontier(),
                        *progress.coverage(),
                    )?;
                    if let Some(source_coverage) =
                        progress.source_coverage().cloned().or_else(|| {
                            recovery
                                .source_coverage(progress.frontier().committed_through())
                                .ok()
                        })
                    {
                        request = request.with_source_coverage(source_coverage);
                    }
                    self.complete_session_refresh(request, ExecutionControl::default())
                        .await?;
                    return Ok(());
                }
                SessionRefreshRestartStateV1::BeginProjection
                | SessionRefreshRestartStateV1::ResumeProjection { .. } => {
                    let (progress, batch) = self
                        .access()
                        .materialize_session_temporal_refresh_batch_result(&recovery)
                        .await?
                        .ok_or(SessionStoreError::InvalidStateTransition {
                            context: "test temporal fixture projection batch",
                        })?;
                    self.persist_session_refresh_projection_batch(progress, batch)
                        .await?;
                }
            }
        }
    }
}

impl<D: SessionTemporalRegisteredDb + Sync> SessionTemporalCapabilityProvider
    for GlobalDbSessionTemporalStore<'_, D>
{
    fn session_temporal_capabilities(&self) -> &SessionTemporalCapabilitiesV1 {
        static CAPABILITIES: LazyLock<SessionTemporalCapabilitiesV1> = LazyLock::new(|| {
            SessionTemporalCapabilitiesV1::new([
                SessionTemporalCapabilityV1::FrozenWatermarks,
                SessionTemporalCapabilityV1::GenerationRebuild,
                SessionTemporalCapabilityV1::RefreshJoin,
                SessionTemporalCapabilityV1::RefreshProgressPersistence,
                SessionTemporalCapabilityV1::RefreshCancellation,
            ])
        });
        &CAPABILITIES
    }
}

impl<D: SessionTemporalRegisteredDb + Sync> SessionRetrievalStore
    for GlobalDbSessionTemporalStore<'_, D>
{
    fn freeze_session_temporal_snapshot_supported(
        &self,
        _permit: SessionSnapshotFreezePermit,
        request: SessionTemporalSnapshotRequestV1,
    ) -> impl Future<Output = SessionStoreResult<SessionTemporalSnapshotV1>> + Send {
        let db = self.db;
        async move {
            SessionTemporalAccess::new(db)
                .freeze_session_temporal_snapshot_result(request)
                .await
        }
    }

    fn retrieve_session_temporal_page_supported(
        &self,
        _permit: SessionTemporalPageRetrievePermit,
        request: SessionTemporalRetrievalRequestV1,
    ) -> impl Future<Output = SessionStoreResult<SessionRetrievalPageV1>> + Send {
        let db = self.db;
        async move {
            SessionTemporalAccess::new(db)
                .retrieve_session_temporal_page_result(request)
                .await
        }
    }
}

impl<D: SessionTemporalRegisteredDb + Sync> SessionTemporalProjectionStore
    for GlobalDbSessionTemporalStore<'_, D>
{
    fn begin_session_generation_rebuild_supported(
        &self,
        _permit: SessionGenerationRebuildBeginPermit,
        request: SessionGenerationRebuildRequestV1,
    ) -> impl Future<Output = SessionStoreResult<SessionGenerationRebuildReceiptV1>> + Send {
        let db = self.db;
        async move {
            SessionTemporalAccess::new(db)
                .begin_session_generation_rebuild_result(request)
                .await
        }
    }

    fn persist_session_temporal_projection_batch_supported(
        &self,
        _permit: SessionProjectionBatchPersistPermit,
        batch: SessionTemporalProjectionBatchV1,
    ) -> impl Future<Output = SessionStoreResult<SessionTemporalProjectionBatchReceiptV1>> + Send
    {
        let db = self.db;
        async move {
            SessionTemporalAccess::new(db)
                .persist_session_temporal_projection_batch_result(batch)
                .await
        }
    }

    fn activate_session_temporal_generation_supported(
        &self,
        _permit: SessionGenerationActivatePermit,
        request: SessionGenerationActivationRequestV1,
    ) -> impl Future<Output = SessionStoreResult<SessionGenerationActivationReceiptV1>> + Send {
        let db = self.db;
        async move {
            SessionTemporalAccess::new(db)
                .activate_session_temporal_generation_result(request)
                .await
        }
    }
}

impl<D: SessionTemporalRegisteredDb + Sync> SessionRefreshStore
    for GlobalDbSessionTemporalStore<'_, D>
{
    fn begin_or_join_session_refresh_supported(
        &self,
        _permit: SessionRefreshBeginOrJoinPermit,
        request: SessionRefreshBeginOrJoinRequestV1,
    ) -> impl Future<Output = SessionStoreResult<SessionRefreshBeginOrJoinReceiptV1>> + Send {
        let db = self.db;
        async move {
            SessionTemporalAccess::new(db)
                .begin_or_join_session_refresh_result(request)
                .await
        }
    }

    fn persist_session_refresh_progress_supported(
        &self,
        _permit: SessionRefreshProgressPersistPermit,
        progress: SessionRefreshProgressV1,
    ) -> impl Future<Output = SessionStoreResult<SessionRefreshProgressV1>> + Send {
        let db = self.db;
        async move {
            SessionTemporalAccess::new(db)
                .persist_session_refresh_progress_result(progress)
                .await
        }
    }

    fn session_refresh_progress_supported(
        &self,
        _permit: SessionRefreshProgressReadPermit,
        request: SessionRefreshProgressRequestV1,
    ) -> impl Future<Output = SessionStoreResult<Option<SessionRefreshProgressV1>>> + Send {
        let db = self.db;
        async move {
            SessionTemporalAccess::new(db)
                .session_refresh_progress_result(request)
                .await
        }
    }

    fn complete_session_refresh_supported(
        &self,
        _permit: SessionRefreshCompletePermit,
        request: SessionRefreshCompletionRequestV1,
        execution_control: ExecutionControl,
    ) -> impl Future<Output = SessionStoreResult<SessionRefreshReceiptV1>> + Send {
        let db = self.db;
        async move {
            SessionTemporalAccess::new(db)
                .complete_session_refresh_result(request, execution_control)
                .await
        }
    }

    fn fail_session_refresh_supported(
        &self,
        _permit: SessionRefreshFailPermit,
        request: SessionRefreshFailureRequestV1,
    ) -> impl Future<Output = SessionStoreResult<SessionRefreshReceiptV1>> + Send {
        let db = self.db;
        async move {
            SessionTemporalAccess::new(db)
                .fail_session_refresh_result(request)
                .await
        }
    }

    fn cancel_session_refresh_supported(
        &self,
        _permit: SessionRefreshCancelPermit,
        request: SessionRefreshCancellationRequestV1,
    ) -> impl Future<Output = SessionStoreResult<SessionRefreshReceiptV1>> + Send {
        let db = self.db;
        async move {
            SessionTemporalAccess::new(db)
                .cancel_session_refresh_result(request)
                .await
        }
    }

    fn session_refresh_receipt_supported(
        &self,
        _permit: SessionRefreshReceiptReadPermit,
        request: SessionRefreshReceiptRequestV1,
    ) -> impl Future<Output = SessionStoreResult<Option<SessionRefreshReceiptV1>>> + Send {
        let db = self.db;
        async move {
            SessionTemporalAccess::new(db)
                .session_refresh_receipt_result(request)
                .await
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn adapter_contains_only_the_borrowed_registered_db_handle() {
        #[allow(dead_code)]
        fn assert_exact_fields<D: SessionTemporalRegisteredDb>(
            store: &GlobalDbSessionTemporalStore<'_, D>,
        ) {
            let GlobalDbSessionTemporalStore { db: _ } = store;
        }
    }

    #[test]
    fn graph_cancellation_observes_the_callers_execution_control() {
        let control = tracedecay_temporal_query::ports::ExecutionControl::default();
        let cancellation = execution_control_graph_cancellation(&control);

        assert!(!cancellation.is_cancelled());
        control.cancel();
        assert!(cancellation.is_cancelled());
    }

    #[test]
    fn graph_cancellation_observes_deadlines_and_work_budgets() {
        let expired = tracedecay_temporal_query::ports::ExecutionControl::new(Some(
            std::time::Instant::now(),
        ));
        assert!(execution_control_graph_cancellation(&expired).is_cancelled());

        let budgeted =
            tracedecay_temporal_query::ports::ExecutionControl::default().with_work_limit(1);
        let cancellation = execution_control_graph_cancellation(&budgeted);
        assert!(!cancellation.is_cancelled());
        assert!(cancellation.is_cancelled());
    }
}
