use std::path::Path;

use tracedecay_domain::SessionId;
use tracedecay_lcm::{
    LcmCompressionRequest, LcmCompressionResponse, LcmDescribeRequest, LcmDescribeResponse,
    LcmError, LcmExpandQueryRequest, LcmExpandQueryResponse, LcmExpandRequest, LcmExpandResponse,
    LcmGcConfig, LcmGcReport, LcmGrepOutcome, LcmGrepRequest, LcmLoadSessionPage,
    LcmLoadSessionRequest, LcmPreflightRequest, LcmPreflightResponse, LcmRecentSession,
    LcmRelationProjectionStatus, LcmSessionBoundaryRequest, LcmSessionBoundaryResponse,
    LcmSessionReplayRequest, LcmSessionReplaySlice, LcmStatus, LcmSummaryExpansion, compression,
    dag::LcmSummaryPublicationPort,
    payload,
    types::{LcmImmutableSummaryPublication, LcmSummaryPublicationReceipt},
};
use tracedecay_sessions::runtime::{SessionMessageRecord, SessionStoreAccess};
use tracedecay_temporal_query::ports::{ExecutionControl, TemporalPortError};

use super::RegisteredGlobalDb;
use tracedecay_session_temporal_store::operations as session_temporal_operations;
use tracedecay_session_temporal_store::seed_session_relation_projection;
use tracedecay_session_temporal_store::store::execution_control_graph_cancellation;

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

impl RegisteredGlobalDb {
    #[hotpath::skip]
    pub(super) async fn lcm_read_snapshot(
        &self,
    ) -> Result<tracedecay_runtime_core::db::DatabaseEngineReadSnapshot, LcmError> {
        SessionStoreAccess::new(self).lcm_read_snapshot().await
    }

    pub(super) fn lcm_storage_root(&self) -> Result<&Path, LcmError> {
        SessionStoreAccess::new(self).lcm_storage_root()
    }

    #[hotpath::skip]
    pub async fn lcm_status(
        &self,
        provider: &str,
        session_id: Option<&str>,
    ) -> Result<LcmStatus, LcmError> {
        SessionStoreAccess::new(self)
            .lcm_status(provider, session_id)
            .await
    }

    #[hotpath::skip]
    pub async fn lcm_describe(
        &self,
        request: LcmDescribeRequest,
    ) -> Result<LcmDescribeResponse, LcmError> {
        SessionStoreAccess::new(self).lcm_describe(request).await
    }

    #[hotpath::skip]
    pub async fn lcm_expand(
        &self,
        request: LcmExpandRequest,
    ) -> Result<LcmExpandResponse, LcmError> {
        SessionStoreAccess::new(self).lcm_expand(request).await
    }

    #[hotpath::skip]
    pub async fn lcm_expand_summary_node(
        &self,
        provider: &str,
        session_id: &str,
        node_id: &str,
    ) -> Result<LcmSummaryExpansion, LcmError> {
        SessionStoreAccess::new(self)
            .lcm_expand_summary_node(provider, session_id, node_id)
            .await
    }

    #[hotpath::skip]
    pub async fn lcm_expand_query(
        &self,
        request: LcmExpandQueryRequest,
    ) -> Result<LcmExpandQueryResponse, LcmError> {
        SessionStoreAccess::new(self)
            .lcm_expand_query(request)
            .await
    }

    #[hotpath::measure(future = true, label = "global_db.registered.lcm.grep")]
    pub async fn lcm_grep(&self, request: LcmGrepRequest) -> Result<LcmGrepOutcome, LcmError> {
        let git_scope_session_ids =
            tracedecay_session_temporal_store::SessionTemporalAccess::new(self)
                .git_scope_session_ids(&request.git_filter)
                .map_err(|error| LcmError::Db(error.to_string()))?;
        SessionStoreAccess::new(self)
            .lcm_grep(request, git_scope_session_ids.as_deref())
            .await
    }

    #[hotpath::skip]
    pub async fn lcm_load_session(
        &self,
        request: LcmLoadSessionRequest,
    ) -> Result<LcmLoadSessionPage, LcmError> {
        SessionStoreAccess::new(self)
            .lcm_load_session(request)
            .await
    }

    #[hotpath::skip]
    pub async fn lcm_recent_sessions(
        &self,
        provider: Option<&str>,
        limit: usize,
    ) -> Result<Vec<LcmRecentSession>, LcmError> {
        SessionStoreAccess::new(self)
            .lcm_recent_sessions(provider, limit)
            .await
    }

    #[hotpath::skip]
    pub async fn lcm_session_providers(&self, session_id: &str) -> Result<Vec<String>, LcmError> {
        SessionStoreAccess::new(self)
            .lcm_session_providers(session_id)
            .await
    }

    #[hotpath::skip]
    pub async fn lcm_session_replay_slice(
        &self,
        request: &LcmSessionReplayRequest,
    ) -> Result<LcmSessionReplaySlice, LcmError> {
        SessionStoreAccess::new(self)
            .lcm_session_replay_slice(request)
            .await
    }

    #[hotpath::skip]
    pub async fn lcm_raw_message_store_id(
        &self,
        provider: &str,
        message_id: &str,
    ) -> Result<Option<i64>, LcmError> {
        SessionStoreAccess::new(self)
            .lcm_raw_message_store_id(provider, message_id)
            .await
    }

    #[hotpath::skip]
    pub async fn lcm_status_with_options(
        &self,
        provider: &str,
        session_id: Option<&str>,
        deep: bool,
        gc_config: &LcmGcConfig,
    ) -> Result<LcmStatus, LcmError> {
        SessionStoreAccess::new(self)
            .lcm_status_with_options(provider, session_id, deep, gc_config)
            .await
    }

    /// Publishes one immutable summary and advances its native relation
    /// projection in the same controlled mutation journey.
    #[hotpath::measure(future = true, label = "global_db.registered.lcm.publish")]
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
        check_execution(control)?;
        self.apply_active_session_relation_projection(
            &session_id,
            execution_control_graph_cancellation(control),
        )
        .await
        .map_err(|error| {
            LcmError::Db(format!(
                "apply native LCM summary relation projection: {error}"
            ))
        })?;
        check_execution(control)?;
        Ok(receipt)
    }

    #[hotpath::skip]
    pub async fn lcm_session_boundary_guarded<F>(
        &self,
        request: LcmSessionBoundaryRequest,
        before_commit: F,
    ) -> Result<LcmSessionBoundaryResponse, LcmError>
    where
        F: FnOnce() -> Result<(), LcmError>,
    {
        SessionStoreAccess::new(self)
            .lcm_session_boundary_guarded(request, before_commit)
            .await
    }

    #[hotpath::skip]
    pub async fn lcm_preflight(
        &self,
        request: LcmPreflightRequest,
    ) -> Result<LcmPreflightResponse, LcmError> {
        SessionStoreAccess::new(self).lcm_preflight(request).await
    }

    #[hotpath::skip]
    pub async fn lcm_compress_guarded<F>(
        &self,
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
        let mut response = compression::compress(
            &transaction,
            &publisher,
            storage_root,
            request,
            &mut payload_rollback,
        )
        .await?;
        check_execution(control)?;
        before_commit()?;
        transaction.commit().await?;
        payload_rollback.disarm();
        if !response.summary_nodes.is_empty() {
            check_execution(control)?;
            self.apply_active_session_relation_projection(
                &session_id,
                execution_control_graph_cancellation(control),
            )
            .await
            .map_err(|error| {
                LcmError::Db(format!("apply native LCM relation projection: {error}"))
            })?;
            response.relation_projection_status = LcmRelationProjectionStatus::Applied;
            check_execution(control)?;
        }
        Ok(response)
    }

    #[hotpath::skip]
    pub async fn lcm_compress_retained_page_guarded<F>(
        &self,
        request: LcmCompressionRequest,
        control: &ExecutionControl,
        before_commit: F,
        row_limit: usize,
        byte_limit: u64,
        convergence_candidate: Option<
            &tracedecay_lcm::summary_convergence::LcmSummaryConvergenceCandidate,
        >,
    ) -> Result<tracedecay_lcm::summary_convergence::LcmBoundedCompressionResponse, LcmError>
    where
        F: FnOnce() -> Result<(), LcmError>,
    {
        check_execution(control)?;
        let storage_root = self.lcm_storage_root()?;
        let session_id = SessionId::new(request.session_id.clone()).map_err(|error| {
            LcmError::Db(format!(
                "invalid retained LCM compression session identity '{}': {error}",
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
        .map_err(|error| {
            LcmError::Db(format!(
                "seed native retained LCM relation projection: {error}"
            ))
        })?;
        check_execution(control)?;
        let publisher = session_temporal_operations::GlobalDbLcmSummaryPublication::for_scope(
            &transaction,
            relation_projection,
        );
        let mut bounded = compression::compress_retained_page(
            &transaction,
            &publisher,
            storage_root,
            request,
            &mut payload_rollback,
            row_limit,
            byte_limit,
        )
        .await?;
        if bounded.response.status != "needs_summary"
            && let Some(candidate) = convergence_candidate
        {
            let state = if bounded.has_more {
                tracedecay_lcm::summary_convergence::LcmSummaryConvergenceQueueState::Pending
            } else {
                tracedecay_lcm::summary_convergence::LcmSummaryConvergenceQueueState::Current
            };
            tracedecay_lcm::summary_convergence::record_outcome(
                &transaction,
                candidate,
                state,
                None,
                0,
                0,
            )
            .await?;
        }
        check_execution(control)?;
        before_commit()?;
        transaction.commit().await?;
        payload_rollback.disarm();
        if !bounded.response.summary_nodes.is_empty() {
            check_execution(control)?;
            self.apply_active_session_relation_projection(
                &session_id,
                execution_control_graph_cancellation(control),
            )
            .await
            .map_err(|error| {
                LcmError::Db(format!(
                    "apply native retained LCM relation projection: {error}"
                ))
            })?;
            bounded.response.relation_projection_status = LcmRelationProjectionStatus::Applied;
            check_execution(control)?;
        }
        Ok(bounded)
    }

    #[hotpath::skip]
    pub async fn lcm_payload_health_detail(
        &self,
        storage_root: &Path,
        provider: &str,
        session_id: Option<&str>,
        deep: bool,
        sample_limit: usize,
        cfg: &LcmGcConfig,
    ) -> Result<tracedecay_lcm::query::PayloadHealthDetail, LcmError> {
        SessionStoreAccess::new(self)
            .lcm_payload_health_detail(storage_root, provider, session_id, deep, sample_limit, cfg)
            .await
    }

    #[hotpath::skip]
    pub async fn lcm_preview_payload_gc(
        &self,
        storage_root: &Path,
        provider: &str,
        session_id: Option<&str>,
        cfg: &LcmGcConfig,
        now: i64,
    ) -> Result<LcmGcReport, LcmError> {
        SessionStoreAccess::new(self)
            .lcm_preview_payload_gc(storage_root, provider, session_id, cfg, now)
            .await
    }

    #[hotpath::skip]
    pub async fn lcm_run_payload_gc_apply(
        &self,
        storage_root: &Path,
        provider: &str,
        session_id: Option<&str>,
        cfg: &LcmGcConfig,
        now: i64,
    ) -> Result<LcmGcReport, LcmError> {
        SessionStoreAccess::new(self)
            .lcm_run_payload_gc_apply(storage_root, provider, session_id, cfg, now)
            .await
    }

    #[hotpath::skip]
    pub async fn lcm_protect_session_raw_messages(
        &self,
        provider: &str,
        session_id: &str,
    ) -> Result<u64, LcmError> {
        SessionStoreAccess::new(self)
            .lcm_protect_session_raw_messages(provider, session_id)
            .await
    }

    #[hotpath::skip]
    pub async fn lcm_protect_session_raw_messages_page(
        &self,
        provider: &str,
        session_id: &str,
        after_store_id: i64,
        page_limit: usize,
        page_max_bytes: u64,
    ) -> Result<tracedecay_lcm::summary_convergence::LcmRawProtectionPage, LcmError> {
        SessionStoreAccess::new(self)
            .lcm_protect_session_raw_messages_page(
                provider,
                session_id,
                after_store_id,
                page_limit,
                page_max_bytes,
            )
            .await
    }

    #[hotpath::skip]
    pub async fn lcm_ingest_raw_message(
        &self,
        storage_root: &Path,
        message: &SessionMessageRecord,
    ) -> Result<(), LcmError> {
        crate::hotpath_observe::record_transaction_rows(1);
        SessionStoreAccess::new(self)
            .lcm_ingest_raw_message(storage_root, message)
            .await
    }
}
