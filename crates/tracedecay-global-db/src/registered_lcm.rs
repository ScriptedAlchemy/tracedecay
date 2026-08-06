use std::path::Path;

use tracedecay_domain::SessionId;
use tracedecay_runtime_core::db::engine::{Executor, IntoParams, QueryExecutor, Rows, params};
use tracedecay_sessions::runtime::{
    SessionMessageRecord,
    lcm::{
        LcmCompressionRequest, LcmCompressionResponse, LcmDescribeRequest, LcmDescribeResponse,
        LcmError, LcmExpandQueryRequest, LcmExpandQueryResponse, LcmExpandRequest,
        LcmExpandResponse, LcmGcConfig, LcmGcReport, LcmGrepFilters, LcmGrepOutcome,
        LcmGrepRequest, LcmLoadSessionPage, LcmLoadSessionRequest, LcmPreflightRequest,
        LcmPreflightResponse, LcmRawMessage, LcmRecentSession, LcmSessionBoundaryRequest,
        LcmSessionBoundaryResponse, LcmSessionReplayRequest, LcmSessionReplaySlice, LcmStatus,
        LcmSummaryExpansion, compression,
        dag::{self, LcmSummaryPublicationPort},
        doctor, gc, payload, query, raw, schema,
        types::{LcmImmutableSummaryPublication, LcmSummaryPublicationReceipt},
    },
};
use tracedecay_temporal_query::ports::{ExecutionControl, TemporalPortError};

use super::{
    RegisteredGlobalDb,
    registered::RegisteredGlobalDbWriterConnection,
    session_temporal::{
        seed_session_relation_projection, store::execution_control_graph_cancellation,
    },
    session_temporal_operations,
};

fn check_execution(control: &ExecutionControl) -> Result<(), LcmError> {
    control.checkpoint().map_err(|error| match error {
        TemporalPortError::Cancelled => LcmError::Cancelled,
        TemporalPortError::DeadlineExceeded => LcmError::DeadlineExceeded,
        TemporalPortError::BudgetExceeded { resource } => LcmError::Db(format!(
            "LCM relation execution exhausted {resource} budget"
        )),
        other => LcmError::Db(format!("LCM relation execution control failed: {other}")),
    })
}

async fn complete_lcm_host_effect_receipt(
    conn: &impl Executor,
    effect_id: &str,
    response: &LcmCompressionResponse,
) -> Result<(), LcmError> {
    let summary_node_ids = response
        .summary_nodes
        .iter()
        .map(|node| node.node_id.as_str())
        .collect::<Vec<_>>();
    let encoded_ids = serde_json::to_string(&summary_node_ids)
        .map_err(|error| LcmError::Db(format!("encode host compaction receipt: {error}")))?;
    let changed = conn
        .execute(
            "UPDATE session_lcm_effect_receipts
             SET state = 'completed', reason = ?2, summary_node_ids_json = ?3,
                 completed_at = unixepoch()
             WHERE effect_id = ?1 AND state = 'pending'",
            params![effect_id, response.reason.as_str(), encoded_ids],
        )
        .await?;
    if changed != 1 {
        return Err(LcmError::Db(
            "host compaction receipt changed before the atomic LCM commit".to_owned(),
        ));
    }
    Ok(())
}

impl QueryExecutor for RegisteredGlobalDbWriterConnection<'_> {
    async fn query<P>(
        &self,
        sql: &str,
        params: P,
    ) -> tracedecay_runtime_core::db::engine::Result<Rows>
    where
        P: IntoParams,
    {
        RegisteredGlobalDbWriterConnection::query(self, sql, params).await
    }
}

impl Executor for RegisteredGlobalDbWriterConnection<'_> {
    async fn execute<P>(
        &self,
        sql: &str,
        params: P,
    ) -> tracedecay_runtime_core::db::engine::Result<u64>
    where
        P: IntoParams,
    {
        RegisteredGlobalDbWriterConnection::execute(self, sql, params).await
    }

    async fn execute_batch(&self, sql: &str) -> tracedecay_runtime_core::db::engine::Result<()> {
        RegisteredGlobalDbWriterConnection::execute_batch(self, sql).await
    }
}

impl RegisteredGlobalDb {
    fn lcm_storage_root(&self) -> Result<&Path, LcmError> {
        self.db_path()
            .parent()
            .ok_or_else(|| LcmError::Db("registered session database has no parent".to_string()))
    }

    pub async fn lcm_status(
        &self,
        provider: &str,
        session_id: Option<&str>,
    ) -> Result<LcmStatus, LcmError> {
        self.lcm_status_with_options(provider, session_id, false, &LcmGcConfig::default())
            .await
    }

    pub async fn lcm_describe(
        &self,
        request: LcmDescribeRequest,
    ) -> Result<LcmDescribeResponse, LcmError> {
        let snapshot = self.read_snapshot().await?;
        query::describe(&snapshot, request).await
    }

    pub async fn lcm_expand(
        &self,
        request: LcmExpandRequest,
    ) -> Result<LcmExpandResponse, LcmError> {
        let snapshot = self.read_snapshot().await?;
        query::expand(&snapshot, self.lcm_storage_root()?, request).await
    }

    pub async fn lcm_expand_summary_node(
        &self,
        provider: &str,
        session_id: &str,
        node_id: &str,
    ) -> Result<LcmSummaryExpansion, LcmError> {
        let snapshot = self.read_snapshot().await?;
        dag::expand_summary_node(&snapshot, provider, session_id, node_id).await
    }

    pub async fn lcm_expand_query(
        &self,
        request: LcmExpandQueryRequest,
    ) -> Result<LcmExpandQueryResponse, LcmError> {
        let snapshot = self.read_snapshot().await?;
        query::expand_query(&snapshot, request).await
    }

    pub async fn lcm_grep(&self, request: LcmGrepRequest) -> Result<LcmGrepOutcome, LcmError> {
        let snapshot = self.read_snapshot().await?;
        query::grep(&snapshot, request, LcmGrepFilters::default()).await
    }

    pub async fn lcm_load_session(
        &self,
        request: LcmLoadSessionRequest,
    ) -> Result<LcmLoadSessionPage, LcmError> {
        let snapshot = self.read_snapshot().await?;
        query::load_session(&snapshot, request).await
    }

    pub async fn lcm_recent_sessions(
        &self,
        provider: Option<&str>,
        limit: usize,
    ) -> Result<Vec<LcmRecentSession>, LcmError> {
        let snapshot = self.read_snapshot().await?;
        query::recent_sessions(&snapshot, provider, limit).await
    }

    pub async fn lcm_session_providers(&self, session_id: &str) -> Result<Vec<String>, LcmError> {
        let snapshot = self.read_snapshot().await?;
        query::session_providers(&snapshot, session_id).await
    }

    pub async fn lcm_session_replay_slice(
        &self,
        request: &LcmSessionReplayRequest,
    ) -> Result<LcmSessionReplaySlice, LcmError> {
        let snapshot = self.read_snapshot().await?;
        query::session_replay_slice(&snapshot, request).await
    }

    pub async fn lcm_load_raw_message(
        &self,
        provider: &str,
        message_id: &str,
    ) -> Option<LcmRawMessage> {
        let snapshot = self.read_snapshot().await.ok()?;
        schema::load_raw_message(&snapshot, provider, message_id).await
    }

    pub async fn lcm_status_with_options(
        &self,
        provider: &str,
        session_id: Option<&str>,
        deep: bool,
        gc_config: &LcmGcConfig,
    ) -> Result<LcmStatus, LcmError> {
        let snapshot = self.read_snapshot().await?;
        query::status(
            &snapshot,
            self.lcm_storage_root()?,
            provider,
            session_id,
            deep,
            gc_config,
        )
        .await
    }

    /// Publishes one immutable summary and advances its native relation
    /// projection in the same controlled mutation journey.
    pub async fn lcm_publish_immutable_summary_guarded<F>(
        &self,
        publication: LcmImmutableSummaryPublication,
        control: &ExecutionControl,
        before_commit: F,
    ) -> Result<LcmSummaryPublicationReceipt, LcmError>
    where
        F: FnOnce() -> Result<(), LcmError>,
    {
        check_execution(control)?;
        let session_id = SessionId::new(publication.draft.session_id.clone()).map_err(|error| {
            LcmError::Db(format!(
                "invalid LCM summary session identity '{}': {error}",
                publication.draft.session_id
            ))
        })?;
        let transaction = self
            .begin_write_transaction()
            .await
            .map_err(|error| LcmError::Db(error.to_string()))?;
        let relation_projection = seed_session_relation_projection(
            self,
            &transaction,
            &session_id,
            execution_control_graph_cancellation(control),
        )
        .await
        .map_err(|error| {
            LcmError::Db(format!(
                "seed native LCM summary relation projection: {error}"
            ))
        })?;
        check_execution(control)?;
        let publisher = session_temporal_operations::GlobalDbLcmSummaryPublication::for_scope(
            &transaction,
            relation_projection,
        );
        let receipt = publisher.publish_immutable_summary(publication).await?;
        check_execution(control)?;
        before_commit()?;
        transaction.commit().await?;
        self.notify_session_relation_effect_appended();
        check_execution(control)?;
        Ok(receipt)
    }

    pub async fn lcm_doctor(
        &self,
        provider: &str,
        session_id: Option<&str>,
        mode: &str,
    ) -> Result<serde_json::Value, LcmError> {
        let storage_root = self.lcm_storage_root()?;
        let request = doctor::DoctorRequest {
            storage_root,
            provider,
            session_id,
            mode,
        };
        let snapshot = self
            .read_snapshot()
            .await
            .map_err(|error| LcmError::Db(error.to_string()))?;
        doctor::doctor(&snapshot, request).await
    }

    pub async fn lcm_session_boundary_guarded<F>(
        &self,
        request: LcmSessionBoundaryRequest,
        before_commit: F,
    ) -> Result<LcmSessionBoundaryResponse, LcmError>
    where
        F: FnOnce() -> Result<(), LcmError>,
    {
        let transaction = self
            .begin_write_transaction()
            .await
            .map_err(|error| LcmError::Db(error.to_string()))?;
        let response = compression::record_session_boundary(&transaction, request).await?;
        before_commit()?;
        transaction.commit().await?;
        Ok(response)
    }

    pub async fn lcm_preflight(
        &self,
        request: LcmPreflightRequest,
    ) -> Result<LcmPreflightResponse, LcmError> {
        let snapshot = self.read_snapshot().await?;
        compression::preflight(&snapshot, request).await
    }

    pub async fn lcm_compress_guarded<F>(
        &self,
        request: LcmCompressionRequest,
        control: &ExecutionControl,
        before_commit: F,
    ) -> Result<LcmCompressionResponse, LcmError>
    where
        F: FnOnce() -> Result<(), LcmError>,
    {
        self.lcm_compress_guarded_inner(None, request, control, before_commit)
            .await
    }

    pub async fn lcm_compress_host_effect_guarded<F>(
        &self,
        effect_id: &str,
        request: LcmCompressionRequest,
        control: &ExecutionControl,
        before_commit: F,
    ) -> Result<LcmCompressionResponse, LcmError>
    where
        F: FnOnce() -> Result<(), LcmError>,
    {
        self.lcm_compress_guarded_inner(Some(effect_id), request, control, before_commit)
            .await
    }

    async fn lcm_compress_guarded_inner<F>(
        &self,
        effect_id: Option<&str>,
        request: LcmCompressionRequest,
        control: &ExecutionControl,
        before_commit: F,
    ) -> Result<LcmCompressionResponse, LcmError>
    where
        F: FnOnce() -> Result<(), LcmError>,
    {
        check_execution(control)?;
        let storage_root = self.lcm_storage_root()?;
        let session_id = SessionId::new(request.session_id.clone()).map_err(|error| {
            LcmError::Db(format!(
                "invalid LCM compression session identity '{}': {error}",
                request.session_id
            ))
        })?;
        let transaction = self
            .begin_write_transaction()
            .await
            .map_err(|error| LcmError::Db(error.to_string()))?;
        let mut payload_rollback =
            payload::PayloadFileRollback::begin_cancellation_safe(storage_root);
        let relation_projection = seed_session_relation_projection(
            self,
            &transaction,
            &session_id,
            execution_control_graph_cancellation(control),
        )
        .await
        .map_err(|error| LcmError::Db(format!("seed native LCM relation projection: {error}")))?;
        check_execution(control)?;
        let publisher = session_temporal_operations::GlobalDbLcmSummaryPublication::for_scope(
            &transaction,
            relation_projection,
        );
        let response = compression::compress(
            &transaction,
            &publisher,
            storage_root,
            request,
            &mut payload_rollback,
        )
        .await?;
        check_execution(control)?;
        if let Some(effect_id) = effect_id
            && response.status != "needs_summary"
        {
            complete_lcm_host_effect_receipt(&transaction, effect_id, &response).await?;
        }
        before_commit()?;
        transaction.commit().await?;
        payload_rollback.disarm();
        if !response.summary_nodes.is_empty() {
            self.notify_session_relation_effect_appended();
        }
        Ok(response)
    }

    pub async fn lcm_payload_health_detail(
        &self,
        storage_root: &Path,
        provider: &str,
        session_id: Option<&str>,
        deep: bool,
        sample_limit: usize,
        cfg: &LcmGcConfig,
    ) -> Result<query::PayloadHealthDetail, LcmError> {
        let snapshot = self.read_snapshot().await?;
        query::payload_health_detail(
            &snapshot,
            storage_root,
            provider,
            session_id,
            deep,
            sample_limit,
            cfg,
        )
        .await
    }

    pub async fn lcm_preview_payload_gc(
        &self,
        storage_root: &Path,
        provider: &str,
        session_id: Option<&str>,
        cfg: &LcmGcConfig,
        now: i64,
    ) -> Result<LcmGcReport, LcmError> {
        let snapshot = self.read_snapshot().await?;
        gc::run_payload_gc(&snapshot, storage_root, provider, session_id, cfg, now).await
    }

    pub async fn lcm_run_payload_gc_apply(
        &self,
        storage_root: &Path,
        provider: &str,
        session_id: Option<&str>,
        cfg: &LcmGcConfig,
        now: i64,
    ) -> Result<LcmGcReport, LcmError> {
        let transaction = self
            .begin_write_transaction()
            .await
            .map_err(|error| LcmError::Db(error.to_string()))?;
        let mut drain =
            gc::drain_pending_payload_deletes_in_transaction(&transaction, storage_root).await?;
        transaction.commit().await?;

        let transaction = self
            .begin_write_transaction()
            .await
            .map_err(|error| LcmError::Db(error.to_string()))?;
        let mut report = gc::run_payload_gc_in_transaction(
            &transaction,
            storage_root,
            provider,
            session_id,
            cfg,
            true,
            now,
        )
        .await?;
        transaction.commit().await?;

        let transaction = self
            .begin_write_transaction()
            .await
            .map_err(|error| LcmError::Db(error.to_string()))?;
        let post_commit_drain =
            gc::drain_pending_payload_deletes_in_transaction(&transaction, storage_root).await?;
        drain.merge(post_commit_drain);
        gc::finalize_gc_report(&transaction, &mut report, drain).await?;
        transaction.commit().await?;
        Ok(report)
    }

    pub async fn lcm_ingest_raw_message(
        &self,
        storage_root: &Path,
        message: &SessionMessageRecord,
    ) -> Result<(), LcmError> {
        let transaction = self
            .begin_write_transaction()
            .await
            .map_err(|error| LcmError::Db(error.to_string()))?;
        let mut payload_rollback =
            payload::PayloadFileRollback::begin_cancellation_safe(storage_root);
        raw::upsert_raw_message_with_payload_tracked(
            &transaction,
            storage_root,
            message,
            &mut payload_rollback,
        )
        .await?;
        transaction.commit().await?;
        payload_rollback.disarm();
        Ok(())
    }
}
