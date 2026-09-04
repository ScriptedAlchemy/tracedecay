//! `RegisteredGlobalDb` implements the session-temporal registered-store handle.
//!
//! The temporal store crate owns projection and retrieval behavior. This
//! module is the only composition edge: connection, path, and relation-graph
//! identity stay here.

use std::future::Future;
use std::path::Path;

use tracedecay_domain::errors::TraceDecayError;
use tracedecay_runtime_core::db::DatabaseEngineReadSnapshot;
use tracedecay_runtime_core::db::engine::{Error as EngineError, IntoParams, Rows};
use tracedecay_runtime_core::store_runtime::VerifiedGraphRuntimeWeakProxyV1;
use tracedecay_session_temporal_store::relations::{
    SessionRelationGraphStore, SessionRelationScope,
};
use tracedecay_session_temporal_store::{
    SessionTemporalAccess, SessionTemporalExec, SessionTemporalQuery, SessionTemporalRegisteredDb,
    SessionTemporalWriteTxn,
};

use crate::{RegisteredGlobalDb, RegisteredGlobalDbWriteTransaction};

impl SessionTemporalQuery for RegisteredGlobalDbWriteTransaction<'_> {
    fn query<P>(
        &self,
        sql: &str,
        params: P,
    ) -> impl Future<Output = Result<Rows, EngineError>> + Send
    where
        P: IntoParams + Send,
    {
        RegisteredGlobalDbWriteTransaction::query(self, sql, params)
    }
}

impl SessionTemporalExec for RegisteredGlobalDbWriteTransaction<'_> {
    fn execute<P>(
        &self,
        sql: &str,
        params: P,
    ) -> impl Future<Output = Result<u64, EngineError>> + Send
    where
        P: IntoParams + Send,
    {
        RegisteredGlobalDbWriteTransaction::execute(self, sql, params)
    }

    fn execute_batch(&self, sql: &str) -> impl Future<Output = Result<(), EngineError>> + Send {
        RegisteredGlobalDbWriteTransaction::execute_batch(self, sql)
    }
}

impl SessionTemporalWriteTxn for RegisteredGlobalDbWriteTransaction<'_> {
    fn commit(self) -> impl Future<Output = Result<(), EngineError>> + Send {
        RegisteredGlobalDbWriteTransaction::commit(self)
    }
}

impl SessionTemporalRegisteredDb for RegisteredGlobalDb {
    type WriteTxn<'a> = RegisteredGlobalDbWriteTransaction<'a>;

    fn read_snapshot(
        &self,
    ) -> impl Future<Output = Result<DatabaseEngineReadSnapshot, TraceDecayError>> + Send {
        RegisteredGlobalDb::read_snapshot(self)
    }

    fn begin_write_transaction(
        &self,
    ) -> impl Future<Output = Result<Self::WriteTxn<'_>, TraceDecayError>> + Send {
        RegisteredGlobalDb::begin_write_transaction(self)
    }

    fn db_path(&self) -> &Path {
        RegisteredGlobalDb::db_path(self)
    }

    fn session_relation_store(
        &self,
    ) -> Result<(SessionRelationScope, SessionRelationGraphStore), TraceDecayError> {
        let (scope, graph, _, _) = self.session_relation_graph()?;
        Ok((scope.clone(), SessionRelationGraphStore::new(graph.clone())))
    }

    fn project_graph_runtime(&self) -> Option<&VerifiedGraphRuntimeWeakProxyV1> {
        RegisteredGlobalDb::project_graph_runtime(self)
    }
}

/// Composition wrappers so existing `RegisteredGlobalDb` call sites keep
/// working. This is not a module re-export of the temporal crate.
impl RegisteredGlobalDb {
    pub fn git_scope_session_ids(
        &self,
        filter: &tracedecay_sessions::runtime::git_correlation::GitScopeFilter,
    ) -> Result<
        Option<Vec<(String, String)>>,
        tracedecay_sessions::runtime::git_correlation::GitCorrelationError,
    > {
        SessionTemporalAccess::new(self).git_scope_session_ids(filter)
    }

    #[hotpath::measure(future = true, label = "global_db.session_temporal.doctor_health")]
    pub async fn session_temporal_doctor_health(
        &self,
    ) -> tracedecay_session_temporal_store::SessionTemporalHealthReport {
        SessionTemporalAccess::new(self)
            .session_temporal_doctor_health()
            .await
    }

    #[hotpath::measure(future = true, label = "global_db.session_temporal.ensure_cursor_key")]
    pub async fn ensure_active_session_cursor_key_result(
        &self,
    ) -> tracedecay_store::SessionStoreResult<tracedecay_domain::SignedCursorKeyRefV1> {
        SessionTemporalAccess::new(self)
            .ensure_active_session_cursor_key_result()
            .await
    }

    #[hotpath::measure(
        future = true,
        label = "global_db.session_temporal.load_cursor_key_provider"
    )]
    pub async fn load_session_cursor_key_provider_result(
        &self,
    ) -> Result<
        tracedecay_session_temporal_store::GlobalDbCursorKeyProvider,
        tracedecay_session_temporal_store::GlobalDbCursorKeyProviderError,
    > {
        SessionTemporalAccess::new(self)
            .load_session_cursor_key_provider_result()
            .await
    }

    #[hotpath::measure(
        future = true,
        label = "global_db.session_temporal.load_preprovisioned_cursor_key"
    )]
    pub async fn load_preprovisioned_session_cursor_key_provider_result(
        &self,
    ) -> Result<
        tracedecay_session_temporal_store::GlobalDbCursorKeyProvider,
        tracedecay_session_temporal_store::GlobalDbCursorKeyProviderError,
    > {
        SessionTemporalAccess::new(self)
            .load_preprovisioned_session_cursor_key_provider_result()
            .await
    }

    #[hotpath::measure(
        future = true,
        label = "global_db.session_temporal.pending_refresh_page"
    )]
    pub async fn pending_session_temporal_refresh_page_result(
        &self,
        limit: usize,
        active_scan_slots: usize,
        active_after: Option<&tracedecay_domain::SessionId>,
    ) -> tracedecay_store::SessionStoreResult<
        tracedecay_session_temporal_store::SessionTemporalRefreshDiscoveryPage,
    > {
        SessionTemporalAccess::new(self)
            .pending_session_temporal_refresh_page_result(limit, active_scan_slots, active_after)
            .await
    }

    #[hotpath::measure(
        future = true,
        label = "global_db.session_temporal.materialize_refresh_batch"
    )]
    pub async fn materialize_session_temporal_refresh_batch_result(
        &self,
        recovery: &tracedecay_session_temporal_store::SessionRefreshRecoveryV1,
    ) -> tracedecay_store::SessionStoreResult<
        Option<(
            tracedecay_store::SessionRefreshProgressV1,
            tracedecay_store::SessionTemporalProjectionBatchV1,
        )>,
    > {
        SessionTemporalAccess::new(self)
            .materialize_session_temporal_refresh_batch_result(recovery)
            .await
    }

    #[hotpath::measure(future = true, label = "global_db.session_temporal.freeze_snapshot")]
    pub async fn freeze_session_temporal_snapshot_result(
        &self,
        request: tracedecay_store::SessionTemporalSnapshotRequestV1,
    ) -> tracedecay_store::SessionStoreResult<tracedecay_store::SessionTemporalSnapshotV1> {
        SessionTemporalAccess::new(self)
            .freeze_session_temporal_snapshot_result(request)
            .await
    }

    #[hotpath::skip]
    pub async fn active_session_summary_relations(
        &self,
        session_id: &tracedecay_domain::SessionId,
        summary_ids: &[String],
        max_relations: usize,
        cancellation: std::sync::Arc<dyn tracedecay_graph_db::GraphCancellation>,
    ) -> tracedecay_store::SessionStoreResult<(
        tracedecay_domain::SessionProjectionGenerationV1,
        Vec<tracedecay_session_temporal_store::relations::SummaryRelationRead>,
    )> {
        SessionTemporalAccess::new(self)
            .active_session_summary_relations(session_id, summary_ids, max_relations, cancellation)
            .await
    }

    #[hotpath::measure(
        future = true,
        label = "global_db.session_temporal.apply_relation_projection"
    )]
    pub async fn apply_active_session_relation_projection(
        &self,
        session_id: &tracedecay_domain::SessionId,
        cancellation: std::sync::Arc<dyn tracedecay_graph_db::GraphCancellation>,
    ) -> tracedecay_store::SessionStoreResult<tracedecay_graph_db::GraphWatermark> {
        SessionTemporalAccess::new(self)
            .apply_active_session_relation_projection(session_id, cancellation)
            .await
    }

    #[hotpath::measure(
        future = true,
        label = "global_db.session_temporal.recover_relation_projections"
    )]
    pub async fn recover_pending_session_relation_projections(
        &self,
        limit: usize,
        cancellation: std::sync::Arc<dyn tracedecay_graph_db::GraphCancellation>,
    ) -> tracedecay_store::SessionStoreResult<usize> {
        SessionTemporalAccess::new(self)
            .recover_pending_session_relation_projections(limit, cancellation)
            .await
    }

    #[hotpath::measure(
        future = true,
        label = "global_db.session_temporal.recover_relation_projection_page"
    )]
    pub async fn recover_pending_session_relation_projection_page(
        &self,
        limit: usize,
        cancellation: std::sync::Arc<dyn tracedecay_graph_db::GraphCancellation>,
    ) -> tracedecay_store::SessionStoreResult<
        tracedecay_session_temporal_store::SessionRelationRecoveryPage,
    > {
        SessionTemporalAccess::new(self)
            .recover_pending_session_relation_projection_page(limit, cancellation)
            .await
    }

    #[hotpath::measure(future = true, label = "global_db.session_temporal.refresh_recovery")]
    pub async fn session_refresh_recovery_result(
        &self,
        session_id: &tracedecay_domain::SessionId,
    ) -> tracedecay_store::SessionStoreResult<
        Option<tracedecay_session_temporal_store::SessionRefreshRecoveryV1>,
    > {
        SessionTemporalAccess::new(self)
            .session_refresh_recovery_result(session_id)
            .await
    }

    #[hotpath::measure(future = true, label = "global_db.session_temporal.complete_refresh")]
    pub async fn complete_session_refresh_result(
        &self,
        request: tracedecay_store::SessionRefreshCompletionRequestV1,
        execution_control: tracedecay_temporal_query::ports::ExecutionControl,
    ) -> tracedecay_store::SessionStoreResult<tracedecay_store::SessionRefreshReceiptV1> {
        SessionTemporalAccess::new(self)
            .complete_session_refresh_result(request, execution_control)
            .await
    }
}
