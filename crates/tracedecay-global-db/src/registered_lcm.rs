use std::path::Path;
use std::sync::Arc;

use serde_json::Value as JsonValue;
use tracedecay_domain::SessionId;

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
        LcmPreflightResponse, LcmRawMessage, LcmRecentSession, LcmSessionBoundaryRequest,
        LcmSessionBoundaryResponse, LcmSessionReplayRequest, LcmSessionReplaySlice, LcmSourceRef,
        LcmStatus, LcmSummaryExpansion, LcmSummaryNode, LcmSummaryNodeDraft, LcmSummaryRequest,
        LcmSummarySourceMessage, LcmSummarySourceRange, compression, dag, doctor, gc, payload,
        query, raw, schema,
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

    async fn lcm_relation_projection_seed(
        &self,
        session_id: &str,
    ) -> Result<crate::session_temporal::relations::SessionRelationProjection, LcmError> {
        let binding_project_id = self.binding().shard_id.scope.project_id().ok_or_else(|| {
            LcmError::Db("LCM relation authority requires a registered project shard".to_owned())
        })?;
        let (project_id, graph) = self
            .session_relation_graph()
            .map_err(|error| LcmError::Db(error.to_string()))?;
        if project_id != binding_project_id {
            return Err(LcmError::Db(
                "session relation graph binding does not match the project shard".to_owned(),
            ));
        }
        let session_id =
            SessionId::new(session_id).map_err(|error| LcmError::Db(error.to_string()))?;
        let snapshot = self.read_snapshot().await?;
        let mut rows = snapshot
            .query(
                "SELECT generation FROM session_temporal_generations
                 WHERE session_id = ?1 AND state = 'active'
                 ORDER BY generation",
                params![session_id.as_str()],
            )
            .await?;
        let active = rows
            .next()
            .await?
            .map(|row| row.get::<i64>(0))
            .transpose()?;
        if rows.next().await?.is_some() {
            return Err(LcmError::Db(
                "session has multiple active generations".to_owned(),
            ));
        }
        drop(rows);
        drop(snapshot);
        let Some(active) = active else {
            return Ok(
                crate::session_temporal::relation_publication::empty_projection(
                    project_id.clone(),
                    session_id,
                    0,
                ),
            );
        };
        let active = u64::try_from(active)
            .map_err(|error| LcmError::Db(format!("invalid active generation: {error}")))?;
        crate::session_temporal::relations::SessionRelationGraphStore::new(Arc::clone(graph))
            .load_projection(project_id, &session_id, active)
            .map_err(|error| LcmError::Db(error.to_string()))
    }

    /// Replays bounded, durably staged LCM graph publications after a daemon
    /// restart. Graph replacement is idempotent and SQL activation remains an
    /// exact compare-and-swap against the staged active generation.
    pub async fn recover_lcm_relation_publications(&self, limit: usize) -> Result<usize, LcmError> {
        crate::session_temporal::relation_publication::recover_ready_lcm_intents(self, limit)
            .await
            .map_err(|error| LcmError::Db(format!("{error:?}")))
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

    /// Returns Codex compaction summary nodes that still need an auxiliary
    /// Codex app-server summary.
    pub async fn pending_codex_compaction_summary_requests(
        &self,
        session_id: Option<&str>,
        limit: usize,
    ) -> Result<Vec<PendingCodexCompactionSummary>, LcmError> {
        let snapshot = self.read_snapshot().await?;
        let limit = limit.clamp(1, 100) as i64;
        let mut active_sql = String::from(
            "SELECT session_id, generation
             FROM session_temporal_generations
             WHERE state = 'active'",
        );
        let active_params = if let Some(session_id) = session_id {
            active_sql.push_str(" AND session_id = ?1 ORDER BY session_id");
            vec![Value::Text(session_id.to_owned())]
        } else {
            active_sql.push_str(" ORDER BY session_id");
            Vec::new()
        };
        let mut active_rows = snapshot.query(&active_sql, active_params).await?;
        let mut successor_predecessors = std::collections::BTreeSet::new();
        while let Some(row) = active_rows.next().await? {
            let relation_session_id = SessionId::new(row.get::<String>(0)?)
                .map_err(|error| LcmError::Db(error.to_string()))?;
            let generation = u64::try_from(row.get::<i64>(1)?)
                .map_err(|error| LcmError::Db(error.to_string()))?;
            let projection = self
                .session_relation_projection(&relation_session_id, generation)
                .map_err(|error| LcmError::Db(error.to_string()))?;
            successor_predecessors.extend(
                projection
                    .summaries
                    .into_iter()
                    .filter_map(|summary| summary.predecessor_summary_id),
            );
        }
        drop(active_rows);
        let successor_predecessors = serde_json::to_string(&successor_predecessors)
            .map_err(|error| LcmError::Db(error.to_string()))?;
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
               AND candidate.node_id NOT IN (
                     SELECT value FROM json_each(?2)
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
        let mut query_params = vec![
            Value::Integer(limit),
            Value::Text(successor_predecessors),
        ];
        if let Some(session_id) = session_id {
            sql.push_str(
                " AND candidate.session_id = ?3
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
        let relation_projection = self.lcm_relation_projection_seed(&draft.session_id).await?;
        let relation_session_id = draft.session_id.clone();
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
        let publication =
            tracedecay_sessions::runtime::lcm::types::LcmImmutableSummaryPublication {
                summary_id: successor_id,
                predecessor_summary_id: Some(node_id.to_string()),
                draft,
            };
        let publisher =
            session_temporal_operations::GlobalDbLcmSummaryPublication::for_project(
                &transaction,
                relation_projection,
            );
        let receipt =
            tracedecay_sessions::runtime::lcm::dag::LcmSummaryPublicationPort::publish_immutable_summary(
                &publisher,
                publication,
            )
            .await?;
        transaction.commit().await?;
        let session_id = SessionId::new(relation_session_id)
            .map_err(|error| LcmError::Db(error.to_string()))?;
        crate::session_temporal::relation_publication::apply_and_activate_latest_lcm_intent(
            self,
            &session_id,
        )
        .await
        .map_err(|error| LcmError::Db(format!("{error:?}")))?;
        Ok(receipt.summary)
    }

    pub async fn lcm_doctor(
        &self,
        provider: &str,
        session_id: Option<&str>,
        mode: &str,
    ) -> Result<serde_json::Value, LcmError> {
        if !matches!(mode, "diagnose" | "retention") {
            return Err(LcmError::Db(
                "LCM Doctor only supports read-only diagnose and retention modes".to_string(),
            ));
        }
        let storage_root = self.lcm_storage_root()?;
        let snapshot = self.read_snapshot().await?;
        let request = doctor::DoctorRequest {
            storage_root,
            provider,
            session_id,
            mode,
            gc_config: LcmGcConfig::default(),
        };
        doctor::doctor(&snapshot, request).await
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
        let relation_projection = self
            .lcm_relation_projection_seed(&request.session_id)
            .await?;
        let relation_session_id = request.session_id.clone();
        let storage_root = self.lcm_storage_root()?;
        let transaction = self
            .begin_write_transaction()
            .await
            .map_err(|error| LcmError::Db(error.to_string()))?;
        let mut payload_rollback =
            payload::PayloadFileRollback::begin_cancellation_safe(storage_root);
        let publisher =
            session_temporal_operations::GlobalDbLcmSummaryPublication::for_project(
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
        transaction.commit().await?;
        payload_rollback.disarm();
        if response.summary_nodes_created > 0 {
            let session_id = SessionId::new(relation_session_id)
                .map_err(|error| LcmError::Db(error.to_string()))?;
            crate::session_temporal::relation_publication::apply_and_activate_latest_lcm_intent(
                self,
                &session_id,
            )
            .await
            .map_err(|error| LcmError::Db(format!("{error:?}")))?;
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::harness::RegisteredGlobalDbHarness;

    #[tokio::test]
    async fn read_only_doctor_does_not_acquire_the_registered_writer_lane() {
        let harness = RegisteredGlobalDbHarness::open("lcm-doctor-read-only").await;
        let writer = harness
            .registered
            .begin_write_transaction()
            .await
            .expect("hold registered writer lane");
        let storage_root = harness
            .registered
            .db_path()
            .parent()
            .expect("registered database storage root");
        let entries_before = directory_entries(storage_root);

        for mode in ["diagnose", "retention"] {
            let started = std::time::Instant::now();
            let report = tokio::time::timeout(
                std::time::Duration::from_secs(1),
                harness.registered.lcm_doctor("cursor", None, mode),
            )
            .await
            .unwrap_or_else(|_| panic!("{mode} Doctor blocked on the registered writer lane"))
            .unwrap_or_else(|error| {
                panic!("{mode} Doctor must not acquire the writer lane: {error}")
            });
            assert_eq!(report["mode"], mode);
            assert!(
                report.get("apply").is_none()
                    && report.get("dry_run").is_none()
                    && report.get("repairs").is_none(),
                "read-only Doctor must not expose legacy mutation state: {report}"
            );
            eprintln!(
                "registered read-only Doctor ({mode}) warm latency: {:?}",
                started.elapsed()
            );
        }
        let started = std::time::Instant::now();
        let status = tokio::time::timeout(
            std::time::Duration::from_secs(1),
            harness.registered.lcm_status("cursor", None),
        )
        .await
        .expect("LCM status blocked on the registered writer lane")
        .expect("LCM status must remain available while the writer lane is occupied");
        assert_eq!(
            status.schema_version,
            tracedecay_sessions::runtime::lcm::LCM_SCHEMA_VERSION
        );
        eprintln!(
            "registered read-only status warm latency: {:?}",
            started.elapsed()
        );

        let started = std::time::Instant::now();
        let grep = tokio::time::timeout(
            std::time::Duration::from_secs(1),
            harness.registered.lcm_grep(LcmGrepRequest {
                provider: "cursor".to_string(),
                query: "read-only-warm-probe".to_string(),
                scope: tracedecay_sessions::runtime::lcm::LcmScope::All,
                session_id: None,
                include_summaries: false,
                limit: 10,
                sort: tracedecay_sessions::runtime::lcm::LcmGrepSort::Relevance,
                source: None,
                role: None,
                start_time: None,
                end_time: None,
                git_filter: Default::default(),
            }),
        )
        .await
        .expect("LCM grep blocked on the registered writer lane")
        .expect("LCM grep must remain available while the writer lane is occupied");
        assert!(grep.hits.is_empty());
        eprintln!(
            "registered read-only grep warm latency: {:?}",
            started.elapsed()
        );
        assert_eq!(
            directory_entries(storage_root),
            entries_before,
            "read-only Doctor must not create storage artifacts"
        );

        writer
            .rollback()
            .await
            .expect("release registered writer lane");
    }

    fn directory_entries(path: &std::path::Path) -> Vec<std::ffi::OsString> {
        let mut entries = std::fs::read_dir(path)
            .expect("read registered database storage root")
            .map(|entry| entry.expect("read storage-root entry").file_name())
            .collect::<Vec<_>>();
        entries.sort();
        entries
    }
}
