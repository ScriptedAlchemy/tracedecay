use std::{future::Future, sync::LazyLock};

use tracedecay_domain::{DerivedEvidenceIdV1, DerivedEvidenceKindV1};
use tracedecay_store::{
    DerivedEvidenceMemberPageV1, SessionGenerationActivatePermit,
    SessionGenerationActivationReceiptV1, SessionGenerationActivationRequestV1,
    SessionGenerationRebuildBeginPermit, SessionGenerationRebuildReceiptV1,
    SessionGenerationRebuildRequestV1, SessionProjectionBatchPersistPermit,
    SessionRefreshBeginOrJoinPermit, SessionRefreshBeginOrJoinReceiptV1,
    SessionRefreshBeginOrJoinRequestV1, SessionRefreshCancelPermit,
    SessionRefreshCancellationRequestV1, SessionRefreshCompletePermit,
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

use crate::global_db::GlobalDb;
pub use crate::global_db::session_temporal::{
    SessionRefreshRecoveryV1, SessionRefreshRestartStateV1,
};

/// Session-temporal projection adapter over an already-open authoritative database.
pub struct GlobalDbSessionTemporalStore<'a> {
    db: &'a GlobalDb,
}

impl<'a> GlobalDbSessionTemporalStore<'a> {
    pub const fn new(db: &'a GlobalDb) -> Self {
        Self { db }
    }

    pub async fn persist_session_refresh_projection_batch(
        &self,
        progress: SessionRefreshProgressV1,
        batch: SessionTemporalProjectionBatchV1,
    ) -> SessionStoreResult<(
        SessionRefreshProgressV1,
        SessionTemporalProjectionBatchReceiptV1,
    )> {
        self.db
            .persist_session_refresh_projection_batch_result(progress, batch)
            .await
    }

    pub async fn session_refresh_recovery(
        &self,
        session_id: &tracedecay_domain::SessionId,
    ) -> SessionStoreResult<Option<SessionRefreshRecoveryV1>> {
        self.db.session_refresh_recovery_result(session_id).await
    }

    pub async fn running_session_refreshes(
        &self,
    ) -> SessionStoreResult<Vec<SessionRefreshRecoveryV1>> {
        self.db.running_session_refreshes_result().await
    }
}

impl SessionTemporalCapabilityProvider for GlobalDbSessionTemporalStore<'_> {
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

impl SessionRetrievalStore for GlobalDbSessionTemporalStore<'_> {
    fn freeze_session_temporal_snapshot_supported(
        &self,
        _permit: SessionSnapshotFreezePermit,
        request: SessionTemporalSnapshotRequestV1,
    ) -> impl Future<Output = SessionStoreResult<SessionTemporalSnapshotV1>> + Send {
        self.db.freeze_session_temporal_snapshot_result(request)
    }

    fn retrieve_session_temporal_page_supported(
        &self,
        _permit: SessionTemporalPageRetrievePermit,
        request: SessionTemporalRetrievalRequestV1,
    ) -> impl Future<Output = SessionStoreResult<SessionRetrievalPageV1>> + Send {
        self.db.retrieve_session_temporal_page_result(request)
    }

    fn expand_derived_members_supported(
        &self,
        _permit: SessionTemporalPageRetrievePermit,
        snapshot: SessionTemporalSnapshotV1,
        evidence_kind: DerivedEvidenceKindV1,
        evidence_id: DerivedEvidenceIdV1,
        after_ordinal: Option<u32>,
        limit: usize,
    ) -> impl Future<Output = SessionStoreResult<DerivedEvidenceMemberPageV1>> + Send {
        self.db.expand_derived_members_result(
            snapshot,
            evidence_kind,
            evidence_id,
            after_ordinal,
            limit,
        )
    }
}

impl SessionTemporalProjectionStore for GlobalDbSessionTemporalStore<'_> {
    fn begin_session_generation_rebuild_supported(
        &self,
        _permit: SessionGenerationRebuildBeginPermit,
        request: SessionGenerationRebuildRequestV1,
    ) -> impl Future<Output = SessionStoreResult<SessionGenerationRebuildReceiptV1>> + Send {
        self.db.begin_session_generation_rebuild_result(request)
    }

    fn persist_session_temporal_projection_batch_supported(
        &self,
        _permit: SessionProjectionBatchPersistPermit,
        batch: SessionTemporalProjectionBatchV1,
    ) -> impl Future<Output = SessionStoreResult<SessionTemporalProjectionBatchReceiptV1>> + Send
    {
        self.db
            .persist_session_temporal_projection_batch_result(batch)
    }

    fn activate_session_temporal_generation_supported(
        &self,
        _permit: SessionGenerationActivatePermit,
        request: SessionGenerationActivationRequestV1,
    ) -> impl Future<Output = SessionStoreResult<SessionGenerationActivationReceiptV1>> + Send {
        self.db.activate_session_temporal_generation_result(request)
    }
}

impl SessionRefreshStore for GlobalDbSessionTemporalStore<'_> {
    fn begin_or_join_session_refresh_supported(
        &self,
        _permit: SessionRefreshBeginOrJoinPermit,
        request: SessionRefreshBeginOrJoinRequestV1,
    ) -> impl Future<Output = SessionStoreResult<SessionRefreshBeginOrJoinReceiptV1>> + Send {
        self.db.begin_or_join_session_refresh_result(request)
    }

    fn persist_session_refresh_progress_supported(
        &self,
        _permit: SessionRefreshProgressPersistPermit,
        progress: SessionRefreshProgressV1,
    ) -> impl Future<Output = SessionStoreResult<SessionRefreshProgressV1>> + Send {
        self.db.persist_session_refresh_progress_result(progress)
    }

    fn session_refresh_progress_supported(
        &self,
        _permit: SessionRefreshProgressReadPermit,
        request: SessionRefreshProgressRequestV1,
    ) -> impl Future<Output = SessionStoreResult<Option<SessionRefreshProgressV1>>> + Send {
        self.db.session_refresh_progress_result(request)
    }

    fn complete_session_refresh_supported(
        &self,
        _permit: SessionRefreshCompletePermit,
        request: SessionRefreshCompletionRequestV1,
    ) -> impl Future<Output = SessionStoreResult<SessionRefreshReceiptV1>> + Send {
        self.db.complete_session_refresh_result(request)
    }

    fn fail_session_refresh_supported(
        &self,
        _permit: SessionRefreshFailPermit,
        request: SessionRefreshFailureRequestV1,
    ) -> impl Future<Output = SessionStoreResult<SessionRefreshReceiptV1>> + Send {
        self.db.fail_session_refresh_result(request)
    }

    fn cancel_session_refresh_supported(
        &self,
        _permit: SessionRefreshCancelPermit,
        request: SessionRefreshCancellationRequestV1,
    ) -> impl Future<Output = SessionStoreResult<SessionRefreshReceiptV1>> + Send {
        self.db.cancel_session_refresh_result(request)
    }

    fn session_refresh_receipt_supported(
        &self,
        _permit: SessionRefreshReceiptReadPermit,
        request: SessionRefreshReceiptRequestV1,
    ) -> impl Future<Output = SessionStoreResult<Option<SessionRefreshReceiptV1>>> + Send {
        self.db.session_refresh_receipt_result(request)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn adapter_contains_only_the_borrowed_global_db_handle() {
        fn assert_exact_fields(store: &GlobalDbSessionTemporalStore<'_>) {
            let GlobalDbSessionTemporalStore { db: _ } = store;
        }

        let _ = assert_exact_fields;
        assert_eq!(
            std::mem::size_of::<GlobalDbSessionTemporalStore<'static>>(),
            std::mem::size_of::<&'static GlobalDb>()
        );
    }
}
