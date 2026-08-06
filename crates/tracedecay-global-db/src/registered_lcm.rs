use std::path::Path;

use serde_json::Value as JsonValue;

use tracedecay_runtime_core::db::engine::{
    Executor, IntoParams, QueryExecutor, Rows, Value, params,
};
use tracedecay_sessions::compatibility::projected_content_hash;
use tracedecay_sessions::runtime::{
    SessionMessageRecord,
    lcm::{
        LcmCompressionRequest, LcmCompressionResponse, LcmDescribeRequest, LcmDescribeResponse,
        LcmError, LcmExpandQueryRequest, LcmExpandQueryResponse, LcmExpandRequest,
        LcmExpandResponse, LcmGcConfig, LcmGcReport, LcmGrepFilters, LcmGrepOutcome,
        LcmGrepRequest, LcmLoadSessionPage, LcmLoadSessionRequest, LcmPreflightRequest,
        LcmPreflightResponse, LcmRecentSession, LcmSessionBoundaryRequest,
        LcmSessionBoundaryResponse, LcmSessionReplayRequest, LcmSessionReplaySlice, LcmSourceRef,
        LcmStatus, LcmSummaryExpansion, LcmSummaryNode, LcmSummaryNodeDraft, LcmSummaryRequest,
        LcmSummarySourceMessage, LcmSummarySourceRange, compression, dag, gc, payload, query, raw,
    },
};

use super::{
    PendingCodexCompactionSummary, RegisteredGlobalDb,
    registered::RegisteredGlobalDbWriterConnection, session_temporal_operations,
};

const CODEX_COMPACTION_SUMMARY_PROMPT: &str = concat!(
    "Summarize the visible transcript messages that Codex compacted. ",
    "Preserve durable user intent, implementation decisions, file/module names, ",
    "unresolved tasks, and verification status. Return only the summary text."
);

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

async fn codex_compaction_summary_request_for_node(
    conn: &(impl QueryExecutor + ?Sized),
    node_id: &str,
    session_id: &str,
) -> Result<Option<LcmSummaryRequest>, LcmError> {
    let mut rows = conn
        .query(
            "SELECT r.store_id, r.role, COALESCE(r.content, r.snippet_text, '')
             FROM lcm_summary_sources s
             JOIN lcm_raw_messages r
               ON s.source_kind = 'raw_message'
              AND CAST(s.source_id AS INTEGER) = r.store_id
             WHERE s.node_id = ?1
               AND r.provider = 'codex'
               AND r.session_id = ?2
             ORDER BY s.ordinal",
            params![node_id, session_id],
        )
        .await?;
    let mut source_messages = Vec::new();
    while let Some(row) = rows.next().await? {
        let store_id: i64 = row.get(0)?;
        let role: String = row.get(1)?;
        let content: String = row.get(2)?;
        source_messages.push(LcmSummarySourceMessage {
            store_id,
            role,
            content,
        });
    }
    let (Some(first), Some(last)) = (source_messages.first(), source_messages.last()) else {
        return Ok(None);
    };
    Ok(Some(LcmSummaryRequest {
        provider: "codex".to_string(),
        session_id: session_id.to_string(),
        focus_topic: Some("Codex context compaction".to_string()),
        prompt: CODEX_COMPACTION_SUMMARY_PROMPT.to_string(),
        source_range: LcmSummarySourceRange {
            from_store_id: first.store_id,
            to_store_id: last.store_id,
        },
        source_messages,
        extraction_request: None,
    }))
}

async fn codex_compaction_summary_draft(
    conn: &(impl QueryExecutor + ?Sized),
    node_id: &str,
) -> Result<LcmSummaryNodeDraft, LcmError> {
    let mut rows = conn
        .query(
            "SELECT provider, conversation_id, session_id, depth, summary_text,
                    summary_token_count, source_token_count, source_time_start,
                    source_time_end, expand_hint, metadata_json
             FROM lcm_summary_nodes
             WHERE node_id = ?1",
            params![node_id],
        )
        .await?;
    let row = rows.next().await?.ok_or(LcmError::SummaryNodeNotFound)?;
    let source_refs = summary_source_refs(conn, node_id).await?;
    Ok(LcmSummaryNodeDraft {
        provider: row.get(0)?,
        conversation_id: row.get(1)?,
        session_id: row.get(2)?,
        depth: row.get(3)?,
        summary_text: row.get(4)?,
        summary_token_count: row.get(5)?,
        source_token_count: row.get(6)?,
        source_time_start: row.get(7)?,
        source_time_end: row.get(8)?,
        expand_hint: row.get(9)?,
        metadata_json: row.get(10)?,
        source_refs,
    })
}

async fn summary_source_refs(
    conn: &(impl QueryExecutor + ?Sized),
    node_id: &str,
) -> Result<Vec<LcmSourceRef>, LcmError> {
    let mut rows = conn
        .query(
            "SELECT source_kind, source_id
             FROM lcm_summary_sources
             WHERE node_id = ?1
             ORDER BY ordinal",
            params![node_id],
        )
        .await?;
    let mut refs = Vec::new();
    while let Some(row) = rows.next().await? {
        let source_kind: String = row.get(0)?;
        let source_id: String = row.get(1)?;
        match source_kind.as_str() {
            "raw_message" => refs.push(LcmSourceRef::RawMessage {
                store_id: source_id.parse().map_err(|error| {
                    LcmError::Db(format!(
                        "invalid raw message source id '{source_id}': {error}"
                    ))
                })?,
            }),
            "summary_node" => refs.push(LcmSourceRef::SummaryNode { node_id: source_id }),
            _ => {
                return Err(LcmError::Db(format!(
                    "invalid summary source kind '{source_kind}'"
                )));
            }
        }
    }
    Ok(refs)
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

    /// Resolves only the persisted locator for admission and readiness checks.
    ///
    /// Production callers that do not need content must use this metadata-only
    /// route. Content hydration remains owned by authorized temporal execution.
    pub async fn lcm_raw_message_store_id(
        &self,
        provider: &str,
        message_id: &str,
    ) -> Result<Option<i64>, LcmError> {
        let snapshot = self.read_snapshot().await?;
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

    /// Returns Codex compaction summary nodes that still need an auxiliary
    /// Codex app-server summary.
    pub async fn pending_codex_compaction_summary_requests(
        &self,
        session_id: Option<&str>,
        limit: usize,
    ) -> Result<Vec<PendingCodexCompactionSummary>, LcmError> {
        let snapshot = self.read_snapshot().await?;
        let limit = limit.clamp(1, 100) as i64;
        let mut sql = String::from(
            "SELECT candidate.node_id, candidate.session_id
             FROM lcm_summary_nodes AS candidate
             JOIN session_summary_nodes AS authority
               ON authority.summary_id = candidate.node_id
              AND authority.session_id = candidate.session_id
             WHERE candidate.provider = 'codex'
               AND CASE
                     WHEN json_valid(candidate.metadata_json) THEN
                       json_extract(candidate.metadata_json, '$.source') =
                         'codex_context_compacted'
                       AND COALESCE(
                             json_extract(
                               candidate.metadata_json,
                               '$.tracedecay_summary_source'
                             ),
                             ''
                           ) <> 'codex_app_server'
                     ELSE 0
                   END = 1
               AND NOT EXISTS (
                     SELECT 1
                     FROM session_summary_successors AS lineage
                     WHERE lineage.predecessor_summary_id = candidate.node_id
                   )
               AND EXISTS (
                     SELECT 1
                     FROM lcm_summary_sources AS source
                     JOIN lcm_raw_messages AS raw
                       ON source.source_kind = 'raw_message'
                      AND CAST(source.source_id AS INTEGER) = raw.store_id
                      AND raw.provider = candidate.provider
                      AND raw.session_id = candidate.session_id
                     WHERE source.node_id = candidate.node_id
                   )",
        );
        let mut query_params = vec![Value::Integer(limit)];
        if let Some(session_id) = session_id {
            sql.push_str(
                " AND candidate.session_id = ?2
                  ORDER BY candidate.depth DESC, candidate.created_at DESC, candidate.node_id
                  LIMIT ?1",
            );
            query_params.push(Value::Text(session_id.to_string()));
        } else {
            sql.push_str(
                " ORDER BY candidate.created_at DESC, candidate.depth DESC, candidate.node_id
                  LIMIT ?1",
            );
        }

        let mut rows = snapshot.query(&sql, query_params).await?;
        let mut pending = Vec::new();
        while let Some(row) = rows.next().await? {
            let node_id: String = row.get(0)?;
            let row_session_id: String = row.get(1)?;
            if let Some(request) =
                codex_compaction_summary_request_for_node(&snapshot, &node_id, &row_session_id)
                    .await?
            {
                pending.push(PendingCodexCompactionSummary { node_id, request });
            }
        }
        Ok(pending)
    }

    /// Publishes a deterministic Codex auxiliary summary as an immutable
    /// successor of the placeholder while preserving exact source lineage.
    pub async fn publish_codex_compaction_summary_successor(
        &self,
        node_id: &str,
        summary_text: &str,
        route: &str,
        model: Option<&str>,
    ) -> Result<LcmSummaryNode, LcmError> {
        let snapshot = self.read_snapshot().await?;
        let mut draft = codex_compaction_summary_draft(&snapshot, node_id).await?;
        if draft.provider != "codex" {
            return Err(LcmError::SummaryNodeNotFound);
        }
        let mut metadata: serde_json::Map<String, JsonValue> = draft
            .metadata_json
            .as_deref()
            .and_then(|raw| serde_json::from_str::<JsonValue>(raw).ok())
            .and_then(|value| value.as_object().cloned())
            .unwrap_or_default();
        if metadata.get("source").and_then(JsonValue::as_str) != Some("codex_context_compacted") {
            return Err(LcmError::SummaryNodeNotFound);
        }
        draft.summary_text = summary_text.trim().to_string();
        draft.summary_token_count = i64::from(crate::estimate_tokens(&draft.summary_text));
        metadata.insert(
            "tracedecay_summary_source".to_string(),
            JsonValue::String(route.to_string()),
        );
        if let Some(model) = model.filter(|model| !model.trim().is_empty()) {
            metadata.insert(
                "codex_auxiliary_model".to_string(),
                JsonValue::String(model.trim().to_string()),
            );
        }
        draft.metadata_json = Some(JsonValue::Object(metadata).to_string());
        let draft = tracedecay_sessions::runtime::lcm::dag::sanitize_summary_draft(draft)?;
        drop(snapshot);

        let transaction = self
            .begin_write_transaction()
            .await
            .map_err(|error| LcmError::Db(error.to_string()))?;
        let summary_hash = projected_content_hash(&draft.summary_text);
        let mut successor_id = tracedecay_sessions::runtime::lcm::dag::summary_node_id(
            &draft.provider,
            &draft.session_id,
            draft.depth,
            &draft.source_refs,
            &summary_hash,
        );
        if successor_id == node_id {
            successor_id = format!(
                "sum_{}",
                projected_content_hash(&format!(
                    "{node_id}\0{}",
                    draft.metadata_json.as_deref().unwrap_or_default()
                ))
            );
        }
        let receipt = session_temporal_operations::publish_immutable_summary(
            &transaction,
            tracedecay_sessions::runtime::lcm::types::LcmImmutableSummaryPublication {
                summary_id: successor_id,
                predecessor_summary_id: Some(node_id.to_string()),
                draft,
            },
        )
        .await?;
        transaction.commit().await?;
        Ok(receipt.summary)
    }

    pub async fn lcm_session_boundary(
        &self,
        request: LcmSessionBoundaryRequest,
    ) -> Result<LcmSessionBoundaryResponse, LcmError> {
        let transaction = self
            .begin_write_transaction()
            .await
            .map_err(|error| LcmError::Db(error.to_string()))?;
        let response = compression::record_session_boundary(&transaction, request).await?;
        transaction.commit().await?;
        Ok(response)
    }

    pub async fn lcm_preflight(
        &self,
        request: LcmPreflightRequest,
    ) -> Result<LcmPreflightResponse, LcmError> {
        let storage_root = self.lcm_storage_root()?;
        let transaction = self
            .begin_write_transaction()
            .await
            .map_err(|error| LcmError::Db(error.to_string()))?;
        let mut payload_rollback =
            payload::PayloadFileRollback::begin_cancellation_safe(storage_root);
        let response =
            compression::preflight(&transaction, storage_root, request, &mut payload_rollback)
                .await?;
        transaction.commit().await?;
        payload_rollback.disarm();
        Ok(response)
    }

    pub async fn lcm_compress(
        &self,
        request: LcmCompressionRequest,
    ) -> Result<LcmCompressionResponse, LcmError> {
        let storage_root = self.lcm_storage_root()?;
        let transaction = self
            .begin_write_transaction()
            .await
            .map_err(|error| LcmError::Db(error.to_string()))?;
        let mut payload_rollback =
            payload::PayloadFileRollback::begin_cancellation_safe(storage_root);
        let publisher =
            session_temporal_operations::GlobalDbLcmSummaryPublication::new(&transaction);
        let response = compression::compress(
            &transaction,
            &publisher,
            storage_root,
            request,
            &mut payload_rollback,
        )
        .await?;
        transaction.commit().await?;
        payload_rollback.disarm();
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
