use std::path::Path;

use tracedecay_runtime_core::db::DatabaseEngineReadSnapshot;
use tracedecay_runtime_core::db::engine::{QueryExecutor, params};

use tracedecay_lcm::{
    LcmDescribeRequest, LcmDescribeResponse, LcmError, LcmExpandQueryRequest,
    LcmExpandQueryResponse, LcmExpandRequest, LcmExpandResponse, LcmGcConfig, LcmGcReport,
    LcmGrepFilters, LcmGrepOutcome, LcmGrepRequest, LcmLoadSessionPage, LcmLoadSessionRequest,
    LcmPreflightRequest, LcmPreflightResponse, LcmRecentSession, LcmSessionBoundaryRequest,
    LcmSessionBoundaryResponse, LcmSessionReplayRequest, LcmSessionReplaySlice, LcmStatus,
    LcmSummaryExpansion, compression, dag, gc, payload, query, raw,
};

use crate::runtime::SessionMessageRecord;

use super::super::registered_db::{SessionRegisteredDb, SessionStoreAccess, SessionWriteTxn};

impl<'a, D: SessionRegisteredDb + Sync> SessionStoreAccess<'a, D> {
    #[hotpath::skip]
    pub async fn lcm_read_snapshot(&self) -> Result<DatabaseEngineReadSnapshot, LcmError> {
        self.read_snapshot()
            .await
            .map_err(|error| LcmError::Db(error.to_string()))
    }

    pub fn lcm_storage_root(&self) -> Result<&'a Path, LcmError> {
        self.inner()
            .db_path()
            .parent()
            .ok_or_else(|| LcmError::Db("registered session database has no parent".to_string()))
    }

    #[hotpath::skip]
    pub async fn lcm_status(
        &self,
        provider: &str,
        session_id: Option<&str>,
    ) -> Result<LcmStatus, LcmError> {
        self.lcm_status_with_options(provider, session_id, false, &LcmGcConfig::default())
            .await
    }

    #[hotpath::skip]
    pub async fn lcm_describe(
        &self,
        request: LcmDescribeRequest,
    ) -> Result<LcmDescribeResponse, LcmError> {
        let snapshot = self.lcm_read_snapshot().await?;
        query::describe(&snapshot, request).await
    }

    #[hotpath::measure(future = true, label = "global_db.registered.lcm.expand")]
    pub async fn lcm_expand(
        &self,
        request: LcmExpandRequest,
    ) -> Result<LcmExpandResponse, LcmError> {
        let snapshot = self.lcm_read_snapshot().await?;
        query::expand(&snapshot, self.lcm_storage_root()?, request).await
    }

    #[hotpath::skip]
    pub async fn lcm_expand_summary_node(
        &self,
        provider: &str,
        session_id: &str,
        node_id: &str,
    ) -> Result<LcmSummaryExpansion, LcmError> {
        let snapshot = self.lcm_read_snapshot().await?;
        dag::expand_summary_node(&snapshot, provider, session_id, node_id).await
    }

    #[hotpath::skip]
    pub async fn lcm_expand_query(
        &self,
        request: LcmExpandQueryRequest,
    ) -> Result<LcmExpandQueryResponse, LcmError> {
        let snapshot = self.lcm_read_snapshot().await?;
        query::expand_query(&snapshot, request).await
    }

    /// Grep after the caller has already resolved any git-scope pre-pass.
    ///
    /// The temporal git-scope resolution lives above this crate; global-db
    /// runs that pre-pass and then calls this method.
    #[hotpath::skip]
    pub async fn lcm_grep(
        &self,
        request: LcmGrepRequest,
        git_scope_session_ids: Option<&[(String, String)]>,
    ) -> Result<LcmGrepOutcome, LcmError> {
        let snapshot = self.lcm_read_snapshot().await?;
        query::grep(
            &snapshot,
            request,
            LcmGrepFilters::default(),
            git_scope_session_ids,
        )
        .await
    }

    #[hotpath::measure(future = true, label = "global_db.registered.lcm.load")]
    pub async fn lcm_load_session(
        &self,
        request: LcmLoadSessionRequest,
    ) -> Result<LcmLoadSessionPage, LcmError> {
        let snapshot = self.lcm_read_snapshot().await?;
        query::load_session(&snapshot, request).await
    }

    #[hotpath::skip]
    pub async fn lcm_recent_sessions(
        &self,
        provider: Option<&str>,
        limit: usize,
    ) -> Result<Vec<LcmRecentSession>, LcmError> {
        let snapshot = self.lcm_read_snapshot().await?;
        query::recent_sessions(&snapshot, provider, limit).await
    }

    #[hotpath::skip]
    pub async fn lcm_session_providers(&self, session_id: &str) -> Result<Vec<String>, LcmError> {
        let snapshot = self.lcm_read_snapshot().await?;
        query::session_providers(&snapshot, session_id).await
    }

    #[hotpath::skip]
    pub async fn lcm_session_replay_slice(
        &self,
        request: &LcmSessionReplayRequest,
    ) -> Result<LcmSessionReplaySlice, LcmError> {
        let snapshot = self.lcm_read_snapshot().await?;
        query::session_replay_slice(&snapshot, request).await
    }

    /// Resolves only the persisted locator for admission and readiness checks.
    ///
    /// Production callers that do not need content must use this metadata-only
    /// route. Content hydration remains owned by authorized temporal execution.
    #[hotpath::skip]
    pub async fn lcm_raw_message_store_id(
        &self,
        provider: &str,
        message_id: &str,
    ) -> Result<Option<i64>, LcmError> {
        let snapshot = self.lcm_read_snapshot().await?;
        let mut rows = snapshot
            .query(
                "SELECT store_id
                 FROM lcm_raw_messages
                 WHERE provider = ?1 AND message_id = ?2",
                params![provider, message_id],
            )
            .await?;
        rows.next()
            .await?
            .map(|row| row.get(0))
            .transpose()
            .map_err(Into::into)
    }

    #[hotpath::measure(future = true, label = "global_db.registered.lcm.status")]
    pub async fn lcm_status_with_options(
        &self,
        provider: &str,
        session_id: Option<&str>,
        deep: bool,
        gc_config: &LcmGcConfig,
    ) -> Result<LcmStatus, LcmError> {
        let snapshot = self.lcm_read_snapshot().await?;
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

    #[hotpath::skip]
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
        SessionWriteTxn::commit(transaction).await?;
        Ok(response)
    }

    #[hotpath::skip]
    pub async fn lcm_preflight(
        &self,
        request: LcmPreflightRequest,
    ) -> Result<LcmPreflightResponse, LcmError> {
        let snapshot = self.lcm_read_snapshot().await?;
        compression::preflight(&snapshot, request).await
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
    ) -> Result<query::PayloadHealthDetail, LcmError> {
        let snapshot = self.lcm_read_snapshot().await?;
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

    #[hotpath::skip]
    pub async fn lcm_preview_payload_gc(
        &self,
        storage_root: &Path,
        provider: &str,
        session_id: Option<&str>,
        cfg: &LcmGcConfig,
        now: i64,
    ) -> Result<LcmGcReport, LcmError> {
        let snapshot = self.lcm_read_snapshot().await?;
        gc::run_payload_gc(&snapshot, storage_root, provider, session_id, cfg, now).await
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
        let transaction = self
            .begin_write_transaction()
            .await
            .map_err(|error| LcmError::Db(error.to_string()))?;
        let mut drain =
            gc::drain_pending_payload_deletes_in_transaction(&transaction, storage_root).await?;
        SessionWriteTxn::commit(transaction).await?;

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
        SessionWriteTxn::commit(transaction).await?;

        let transaction = self
            .begin_write_transaction()
            .await
            .map_err(|error| LcmError::Db(error.to_string()))?;
        let post_commit_drain =
            gc::drain_pending_payload_deletes_in_transaction(&transaction, storage_root).await?;
        drain.merge(post_commit_drain);
        gc::finalize_gc_report(&transaction, &mut report, drain).await?;
        SessionWriteTxn::commit(transaction).await?;
        Ok(report)
    }

    /// Upgrades a session's projection-landed raw messages to the canonical
    /// ingest-protection shape before an LCM read or compression consumes
    /// them.
    ///
    /// The observation projection lands `lcm_raw_messages` rows without a
    /// sanitization receipt and deliberately preserves protected payloads on
    /// replay, so this pass is the second phase of that design: each
    /// unreceipted row is re-ingested from its canonical `session_messages`
    /// projection through the privacy firewall, binding the receipt the
    /// verified raw loads require. Already-protected rows are left untouched,
    /// making the pass idempotent and bounded to one session.
    #[hotpath::skip]
    pub async fn lcm_protect_session_raw_messages(
        &self,
        provider: &str,
        session_id: &str,
    ) -> Result<u64, LcmError> {
        let storage_root = self.lcm_storage_root()?.to_path_buf();
        let transaction = self
            .begin_write_transaction()
            .await
            .map_err(|error| LcmError::Db(error.to_string()))?;
        let mut rows = QueryExecutor::query(
            &transaction,
            "SELECT message.provider, message.message_id, message.session_id, message.role,
                        message.timestamp, message.ordinal, message.text, message.kind,
                        message.model, message.tool_names, message.source_path,
                        message.source_offset, message.metadata_json
                 FROM session_messages AS message
                 JOIN lcm_raw_messages AS raw
                   ON raw.provider = message.provider
                  AND raw.message_id = message.message_id
                 WHERE message.provider = ?1 AND message.session_id = ?2
                   AND json_extract(
                           raw.metadata_json,
                           '$.ingest_protection.sanitization_receipt'
                       ) IS NULL
                 ORDER BY message.ordinal, message.message_id",
            params![provider, session_id],
        )
        .await?;
        let mut unprotected = Vec::new();
        while let Some(row) = rows.next().await? {
            unprotected.push(SessionMessageRecord {
                provider: row.get(0)?,
                message_id: row.get(1)?,
                session_id: row.get(2)?,
                role: row.get(3)?,
                timestamp: row.get(4)?,
                ordinal: row.get(5)?,
                text: row.get(6)?,
                kind: row.get(7)?,
                model: row.get(8)?,
                tool_names: row.get(9)?,
                source_path: row.get(10)?,
                source_offset: row.get(11)?,
                metadata_json: row.get(12)?,
            });
        }
        drop(rows);
        SessionWriteTxn::commit(transaction).await?;
        if unprotected.is_empty() {
            return Ok(0);
        }
        let mut payload_rollback =
            payload::PayloadFileRollback::begin_cancellation_safe(&storage_root);
        let mut staged = Vec::with_capacity(unprotected.len());
        for message in &unprotected {
            staged.push(raw::stage_raw_message_with_payload_tracked(
                &storage_root,
                message,
                &mut payload_rollback,
            )?);
        }
        let transaction = self
            .begin_write_transaction()
            .await
            .map_err(|error| LcmError::Db(error.to_string()))?;
        let protected = u64::try_from(unprotected.len())
            .map_err(|error| LcmError::Db(format!("invalid protection batch size: {error}")))?;
        for (message, staged) in unprotected.iter().zip(staged) {
            raw::commit_staged_raw_message(&transaction, message, staged).await?;
        }
        SessionWriteTxn::commit(transaction).await?;
        payload_rollback.disarm();
        Ok(protected)
    }

    #[hotpath::measure(future = true, label = "global_db.registered.lcm.ingest")]
    pub async fn lcm_ingest_raw_message(
        &self,
        storage_root: &Path,
        message: &SessionMessageRecord,
    ) -> Result<(), LcmError> {
        let mut payload_rollback =
            payload::PayloadFileRollback::begin_cancellation_safe(storage_root);
        let staged = raw::stage_raw_message_with_payload_tracked(
            storage_root,
            message,
            &mut payload_rollback,
        )?;
        let transaction = self
            .begin_write_transaction()
            .await
            .map_err(|error| LcmError::Db(error.to_string()))?;
        raw::commit_staged_raw_message(&transaction, message, staged).await?;
        SessionWriteTxn::commit(transaction).await?;
        payload_rollback.disarm();
        Ok(())
    }
}
