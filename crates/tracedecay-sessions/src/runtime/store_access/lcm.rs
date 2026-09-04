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
        let mut frontier_store_id = 0;
        let mut protected = 0_u64;
        loop {
            let page = self
                .lcm_protect_session_raw_messages_page(
                    provider,
                    session_id,
                    frontier_store_id,
                    tracedecay_lcm::LCM_SCAN_PAGE_ROWS as usize,
                    tracedecay_lcm::LCM_SCAN_PAGE_MAX_BYTES as u64,
                )
                .await?;
            protected =
                protected.saturating_add(u64::try_from(page.rows_protected).map_err(|error| {
                    LcmError::Db(format!("invalid protection page size: {error}"))
                })?);
            frontier_store_id = page.frontier_store_id;
            if !page.has_more {
                return Ok(protected);
            }
        }
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
        let storage_root = self.lcm_storage_root()?.to_path_buf();
        let transaction = self
            .begin_write_transaction()
            .await
            .map_err(|error| LcmError::Db(error.to_string()))?;
        let mut rows = QueryExecutor::query(
            &transaction,
            "SELECT raw.store_id,
                        CASE WHEN json_extract(
                            raw.metadata_json,
                            '$.ingest_protection.sanitization_receipt'
                        ) IS NULL THEN 1 ELSE 0 END,
                        COALESCE(length(CAST(message.text AS BLOB)), 0),
                        message.provider, message.message_id, message.session_id, message.role,
                        message.timestamp, message.ordinal, message.text, message.kind,
                        message.model, message.tool_names, message.source_path,
                        message.source_offset, message.metadata_json
                 FROM lcm_raw_messages AS raw
                 LEFT JOIN session_messages AS message
                   ON raw.provider = message.provider
                  AND raw.message_id = message.message_id
                 WHERE raw.provider = ?1 AND raw.session_id = ?2
                   AND raw.store_id > ?3
                 ORDER BY raw.store_id
                 LIMIT ?4",
            params![
                provider,
                session_id,
                after_store_id,
                i64::try_from(page_limit.max(1)).map_err(|_| {
                    LcmError::Db("LCM protection page limit overflow".to_string())
                })?,
            ],
        )
        .await?;
        let mut unprotected = Vec::new();
        let mut rows_scanned = 0_usize;
        let mut bytes_scanned = 0_u64;
        let mut frontier_store_id = after_store_id;
        let mut byte_limited = false;
        while let Some(row) = rows.next().await? {
            let store_id: i64 = row.get(0)?;
            let needs_protection = row.get::<i64>(1)? != 0;
            let row_bytes = u64::try_from(row.get::<i64>(2)?).map_err(|error| {
                LcmError::Db(format!("invalid LCM protection row byte count: {error}"))
            })?;
            if bytes_scanned.saturating_add(row_bytes) > page_max_bytes {
                if rows_scanned == 0 {
                    return Err(LcmError::BudgetExhausted);
                }
                byte_limited = true;
                break;
            }
            rows_scanned = rows_scanned.saturating_add(1);
            bytes_scanned = bytes_scanned.saturating_add(row_bytes);
            frontier_store_id = store_id;
            if !needs_protection {
                continue;
            }
            unprotected.push(SessionMessageRecord {
                provider: row.get::<Option<String>>(3)?.ok_or_else(|| {
                    LcmError::SummarySourceUnavailable {
                        source_id: store_id.to_string(),
                        reason: "canonical_session_message_missing".to_string(),
                    }
                })?,
                message_id: row.get::<Option<String>>(4)?.ok_or_else(|| {
                    LcmError::SummarySourceUnavailable {
                        source_id: store_id.to_string(),
                        reason: "canonical_session_message_missing".to_string(),
                    }
                })?,
                session_id: row.get::<Option<String>>(5)?.ok_or_else(|| {
                    LcmError::SummarySourceUnavailable {
                        source_id: store_id.to_string(),
                        reason: "canonical_session_message_missing".to_string(),
                    }
                })?,
                role: row.get::<Option<String>>(6)?.ok_or_else(|| {
                    LcmError::SummarySourceUnavailable {
                        source_id: store_id.to_string(),
                        reason: "canonical_session_message_missing".to_string(),
                    }
                })?,
                timestamp: row.get(7)?,
                ordinal: row.get::<Option<i64>>(8)?.ok_or_else(|| {
                    LcmError::SummarySourceUnavailable {
                        source_id: store_id.to_string(),
                        reason: "canonical_session_message_missing".to_string(),
                    }
                })?,
                text: row.get::<Option<String>>(9)?.ok_or_else(|| {
                    LcmError::SummarySourceUnavailable {
                        source_id: store_id.to_string(),
                        reason: "canonical_session_message_missing".to_string(),
                    }
                })?,
                kind: row.get(10)?,
                model: row.get(11)?,
                tool_names: row.get(12)?,
                source_path: row.get(13)?,
                source_offset: row.get(14)?,
                metadata_json: row.get(15)?,
            });
        }
        drop(rows);
        let has_more = byte_limited || rows_scanned == page_limit.max(1);
        if unprotected.is_empty() {
            tracedecay_lcm::summary_convergence::record_current_protection_progress(
                &transaction,
                provider,
                session_id,
                frontier_store_id,
            )
            .await?;
            SessionWriteTxn::commit(transaction).await?;
            return Ok(tracedecay_lcm::summary_convergence::LcmRawProtectionPage {
                rows_scanned,
                rows_protected: 0,
                bytes_scanned,
                frontier_store_id,
                has_more,
            });
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
        for (message, staged) in unprotected.iter().zip(staged) {
            raw::commit_staged_raw_message(&transaction, message, staged).await?;
        }
        tracedecay_lcm::summary_convergence::record_current_protection_progress(
            &transaction,
            provider,
            session_id,
            frontier_store_id,
        )
        .await?;
        SessionWriteTxn::commit(transaction).await?;
        payload_rollback.disarm();
        Ok(tracedecay_lcm::summary_convergence::LcmRawProtectionPage {
            rows_scanned,
            rows_protected: unprotected.len(),
            bytes_scanned,
            frontier_store_id,
            has_more,
        })
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
