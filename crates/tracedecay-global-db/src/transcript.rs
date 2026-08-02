use std::{error::Error, fmt::Write as _};

use serde_json::Value as JsonValue;

use super::{ParseOffset, RegisteredGlobalDb, RegisteredGlobalDbWriteTransaction, TranscriptBatch};
use tracedecay_runtime_core::db::engine::{Executor, QueryExecutor, Row, params};
use tracedecay_sessions::runtime::{
    SessionMessageRecord, SessionRecord,
    lcm::{LcmSourceRef, LcmSummaryNodeDraft},
};

struct TranscriptSummarySources {
    refs: Vec<LcmSourceRef>,
    source_token_count: i64,
    source_time_start: Option<i64>,
    source_time_end: Option<i64>,
    excerpts: Vec<TranscriptSummaryExcerpt>,
}

struct TranscriptSummaryExcerpt {
    role: String,
    text: String,
}

fn estimated_tokens_from_chars(char_count: i64) -> i64 {
    ((char_count.max(0) + 3) / 4).max(1)
}

fn estimate_summary_tokens(text: &str) -> i64 {
    i64::from(crate::estimate_tokens(text))
}

fn transcript_summary_text(
    message: &SessionMessageRecord,
    metadata: &JsonValue,
    sources: &TranscriptSummarySources,
) -> String {
    if metadata.get("summary_body").and_then(JsonValue::as_str) == Some("plaintext") {
        return message.text.clone();
    }
    let Some(source_summary) = extractive_transcript_summary(&sources.excerpts) else {
        return message.text.clone();
    };
    let codex_body = metadata
        .get("summary_body")
        .and_then(JsonValue::as_str)
        .unwrap_or("unavailable");
    format!(
        "TraceDecay-generated Codex compaction summary from visible transcript messages. Codex's own compaction body is {codex_body} in the rollout.\n\n{source_summary}"
    )
}

fn extractive_transcript_summary(excerpts: &[TranscriptSummaryExcerpt]) -> Option<String> {
    let meaningful = excerpts
        .iter()
        .filter_map(|excerpt| {
            let text = normalize_summary_excerpt(&excerpt.text);
            if text.is_empty() {
                None
            } else {
                Some((&excerpt.role, text))
            }
        })
        .collect::<Vec<_>>();
    if meaningful.is_empty() {
        return None;
    }

    let mut selected = Vec::new();
    if meaningful.len() <= 12 {
        selected.extend(meaningful.iter());
    } else {
        selected.extend(meaningful.iter().take(4));
        selected.extend(meaningful.iter().skip(meaningful.len().saturating_sub(8)));
    }

    let mut summary = String::from("Visible source highlights:");
    for (role, text) in selected {
        let role = role.trim();
        let role = if role.is_empty() { "unknown" } else { role };
        let line = truncate_summary_excerpt(text, 320);
        let _ = write!(summary, "\n- {role}: {line}");
    }
    Some(summary)
}

fn normalize_summary_excerpt(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn truncate_summary_excerpt(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_string();
    }
    let keep = max_chars.saturating_sub(3);
    format!("{}...", text.chars().take(keep).collect::<String>())
}

#[derive(Debug, Clone, Copy)]
enum TranscriptWritePolicy {
    Full { expected_offset: ParseOffset },
    ProjectionOnly,
}

#[derive(Debug)]
pub enum TranscriptPersistenceError {
    Conflict {
        expected: ParseOffset,
        actual: ParseOffset,
    },
    Storage {
        operation: &'static str,
        source: Box<dyn Error + Send + Sync>,
    },
}

impl TranscriptPersistenceError {
    pub fn storage(operation: &'static str, source: impl Error + Send + Sync + 'static) -> Self {
        Self::Storage {
            operation,
            source: Box::new(source),
        }
    }

    pub fn message(operation: &'static str, message: impl Into<String>) -> Self {
        Self::storage(operation, std::io::Error::other(message.into()))
    }
}

impl std::fmt::Display for TranscriptPersistenceError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Conflict { expected, actual } => write!(
                formatter,
                "transcript parse offset conflict: expected {expected:?}, actual {actual:?}"
            ),
            Self::Storage { operation, source } => write!(formatter, "{operation}: {source}"),
        }
    }
}

impl Error for TranscriptPersistenceError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Conflict { .. } => None,
            Self::Storage { source, .. } => Some(source.as_ref()),
        }
    }
}

pub(super) async fn get_parse_offset(
    conn: &impl QueryExecutor,
    path: &str,
) -> Result<Option<ParseOffset>, TranscriptPersistenceError> {
    let rows = conn
        .query(
            "SELECT byte_offset, mtime, file_id FROM parse_offsets WHERE file_path = ?1",
            params![path],
        )
        .await;
    let Ok(mut rows) = rows else {
        let mut legacy_rows = conn
            .query(
                "SELECT byte_offset, mtime FROM parse_offsets WHERE file_path = ?1",
                params![path],
            )
            .await
            .map_err(|error| {
                TranscriptPersistenceError::storage("read transcript parse offset", error)
            })?;
        let Some(row) = legacy_rows.next().await.map_err(|error| {
            TranscriptPersistenceError::storage("read transcript parse offset", error)
        })?
        else {
            return Ok(None);
        };
        return Ok(Some(ParseOffset {
            byte_offset: decode_u64(&row, 0, "decode transcript byte offset")?,
            mtime: decode_u64(&row, 1, "decode transcript mtime")?,
            file_id: 0,
        }));
    };
    let Some(row) = rows.next().await.map_err(|error| {
        TranscriptPersistenceError::storage("read transcript parse offset", error)
    })?
    else {
        return Ok(None);
    };
    Ok(Some(ParseOffset {
        byte_offset: decode_u64(&row, 0, "decode transcript byte offset")?,
        mtime: decode_u64(&row, 1, "decode transcript mtime")?,
        file_id: decode_u64(&row, 2, "decode transcript file id")?,
    }))
}

fn decode_u64(
    row: &Row,
    index: i32,
    operation: &'static str,
) -> Result<u64, TranscriptPersistenceError> {
    row.get::<i64>(index)
        .map(|value| value as u64)
        .map_err(|error| TranscriptPersistenceError::storage(operation, error))
}

pub(super) async fn require_expected_offset(
    conn: &impl QueryExecutor,
    path: &str,
    expected: ParseOffset,
) -> Result<(), TranscriptPersistenceError> {
    let actual = get_parse_offset(conn, path).await?.unwrap_or_default();
    if actual == expected {
        Ok(())
    } else {
        Err(TranscriptPersistenceError::Conflict { expected, actual })
    }
}

pub(super) async fn set_parse_offset(
    conn: &impl Executor,
    path: &str,
    offset: ParseOffset,
) -> Result<(), TranscriptPersistenceError> {
    conn.execute(
        "INSERT INTO parse_offsets (file_path, byte_offset, mtime, file_id)
         VALUES (?1, ?2, ?3, ?4)
         ON CONFLICT(file_path) DO UPDATE SET
            byte_offset = excluded.byte_offset,
            mtime = excluded.mtime,
            file_id = excluded.file_id",
        params![
            path,
            offset.byte_offset as i64,
            offset.mtime as i64,
            offset.file_id as i64
        ],
    )
    .await
    .map(|_| ())
    .map_err(|error| TranscriptPersistenceError::storage("write transcript parse offset", error))
}

impl RegisteredGlobalDb {
    pub(super) async fn begin_transcript_transaction(
        &self,
    ) -> Result<RegisteredGlobalDbWriteTransaction<'_>, TranscriptPersistenceError> {
        self.begin_write_transaction()
            .await
            .map_err(|error| TranscriptPersistenceError::storage("begin transcript batch", error))
    }

    pub async fn upsert_session(&self, session: &SessionRecord) -> bool {
        let Ok(transaction) = self.begin_transcript_transaction().await else {
            return false;
        };
        if !Self::upsert_session_in_existing_tx(&transaction, session).await {
            return false;
        }
        transaction.commit().await.is_ok()
    }

    async fn upsert_session_in_existing_tx(conn: &impl Executor, session: &SessionRecord) -> bool {
        conn.execute(
            "INSERT INTO sessions
                 (provider, session_id, project_key, project_path, title, started_at, ended_at,
                  transcript_path, metadata_json, parent_session_id, is_subagent, agent_id,
                  parent_tool_use_id)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)
             ON CONFLICT(provider, session_id) DO UPDATE SET
                project_key = excluded.project_key,
                project_path = excluded.project_path,
                title = excluded.title,
                started_at = excluded.started_at,
                ended_at = excluded.ended_at,
                transcript_path = excluded.transcript_path,
                metadata_json = excluded.metadata_json,
                parent_session_id = excluded.parent_session_id,
                is_subagent = excluded.is_subagent,
                agent_id = excluded.agent_id,
                parent_tool_use_id = excluded.parent_tool_use_id",
            params![
                session.provider.clone(),
                session.session_id.clone(),
                session.project_key.clone(),
                session.project_path.clone(),
                session.title.clone(),
                session.started_at,
                session.ended_at,
                session.transcript_path.clone(),
                session.metadata_json.clone(),
                session.parent_session_id.clone(),
                i64::from(session.is_subagent),
                session.agent_id.clone(),
                session.parent_tool_use_id.clone(),
            ],
        )
        .await
        .is_ok()
    }

    pub async fn get_session(&self, provider: &str, session_id: &str) -> Option<SessionRecord> {
        self.get_session_result(provider, session_id)
            .await
            .ok()
            .flatten()
    }

    pub async fn get_session_result(
        &self,
        provider: &str,
        session_id: &str,
    ) -> Result<Option<SessionRecord>, TranscriptPersistenceError> {
        let mut rows = self
            .read_connection()
            .query(
                "SELECT provider, session_id, project_key, project_path, title, started_at,
                        ended_at, transcript_path, metadata_json, parent_session_id,
                        is_subagent, agent_id, parent_tool_use_id
                 FROM sessions WHERE provider = ?1 AND session_id = ?2",
                params![provider, session_id],
            )
            .await
            .map_err(|error| {
                TranscriptPersistenceError::storage("load transcript session", error)
            })?;
        let Some(row) = rows.next().await.map_err(|error| {
            TranscriptPersistenceError::storage("load transcript session", error)
        })?
        else {
            return Ok(None);
        };
        Ok(Some(SessionRecord {
            provider: row.get(0).map_err(|error| {
                TranscriptPersistenceError::storage("decode transcript provider", error)
            })?,
            session_id: row.get(1).map_err(|error| {
                TranscriptPersistenceError::storage("decode transcript session id", error)
            })?,
            project_key: row.get(2).map_err(|error| {
                TranscriptPersistenceError::storage("decode transcript project key", error)
            })?,
            project_path: row.get(3).map_err(|error| {
                TranscriptPersistenceError::storage("decode transcript project path", error)
            })?,
            title: row.get(4).map_err(|error| {
                TranscriptPersistenceError::storage("decode transcript title", error)
            })?,
            started_at: row.get(5).map_err(|error| {
                TranscriptPersistenceError::storage("decode transcript start", error)
            })?,
            ended_at: row.get(6).map_err(|error| {
                TranscriptPersistenceError::storage("decode transcript end", error)
            })?,
            transcript_path: row.get(7).map_err(|error| {
                TranscriptPersistenceError::storage("decode transcript path", error)
            })?,
            metadata_json: row.get(8).map_err(|error| {
                TranscriptPersistenceError::storage("decode transcript metadata", error)
            })?,
            parent_session_id: row.get(9).map_err(|error| {
                TranscriptPersistenceError::storage("decode transcript parent", error)
            })?,
            is_subagent: row.get::<i64>(10).map_err(|error| {
                TranscriptPersistenceError::storage("decode transcript subagent flag", error)
            })? != 0,
            agent_id: row.get(11).map_err(|error| {
                TranscriptPersistenceError::storage("decode transcript agent", error)
            })?,
            parent_tool_use_id: row.get(12).map_err(|error| {
                TranscriptPersistenceError::storage("decode transcript parent tool", error)
            })?,
        }))
    }

    fn normalize_session_message_timestamp(timestamp: Option<i64>) -> Option<i64> {
        timestamp.map(|timestamp| {
            let magnitude = timestamp.unsigned_abs();
            if magnitude >= 100_000_000_000_000_000 {
                timestamp / 1_000_000_000
            } else if magnitude >= 100_000_000_000_000 {
                timestamp / 1_000_000
            } else if magnitude >= 100_000_000_000 {
                timestamp / 1_000
            } else {
                timestamp
            }
        })
    }

    async fn upsert_session_message_in_existing_tx(
        &self,
        conn: &impl Executor,
        message: &SessionMessageRecord,
        payload_rollback: &mut tracedecay_sessions::runtime::lcm::payload::PayloadFileRollback,
    ) -> Result<(), TranscriptPersistenceError> {
        let mut canonical_message = message.clone();
        canonical_message.timestamp = Self::normalize_session_message_timestamp(message.timestamp);
        let storage_root = self
            .db_path()
            .parent()
            .unwrap_or_else(|| std::path::Path::new("."));
        let raw = tracedecay_sessions::runtime::lcm::raw::upsert_raw_message_with_payload_tracked(
            conn,
            storage_root,
            &canonical_message,
            payload_rollback,
        )
        .await
        .map_err(|error| TranscriptPersistenceError::storage("upsert LCM raw message", error))?;
        if !Self::upsert_session_message_projection(
            conn,
            &canonical_message,
            &raw.projection_text,
            raw.projection_metadata_json.as_deref(),
        )
        .await
        {
            return Err(TranscriptPersistenceError::message(
                "upsert session message projection",
                "database write failed",
            ));
        }
        Self::upsert_lcm_summary_for_transcript_summary(conn, &canonical_message).await
    }

    async fn upsert_lcm_summary_for_transcript_summary(
        conn: &impl Executor,
        message: &SessionMessageRecord,
    ) -> Result<(), TranscriptPersistenceError> {
        if message.kind.as_deref() != Some("summary") {
            return Ok(());
        }
        let Some(metadata_json) = message.metadata_json.as_deref() else {
            return Ok(());
        };
        let Ok(metadata) = serde_json::from_str::<JsonValue>(metadata_json) else {
            return Ok(());
        };
        if metadata.get("source").and_then(JsonValue::as_str) != Some("codex_context_compacted") {
            return Ok(());
        }
        let sources = Self::transcript_summary_sources(conn, message).await?;
        if sources.refs.is_empty() {
            return Ok(());
        }
        let depth = metadata
            .get("codex_compaction_depth")
            .and_then(JsonValue::as_i64)
            .unwrap_or(1)
            .max(1);
        let summary_text = transcript_summary_text(message, &metadata, &sources);
        let mut summary_metadata = metadata.as_object().cloned().unwrap_or_default();
        if summary_metadata
            .get("summary_body")
            .and_then(JsonValue::as_str)
            == Some("encrypted")
            && !sources.excerpts.is_empty()
        {
            summary_metadata.insert(
                "tracedecay_summary_source".to_string(),
                JsonValue::String("visible_transcript_source_messages".to_string()),
            );
            summary_metadata.insert(
                "codex_summary_body".to_string(),
                JsonValue::String("encrypted".to_string()),
            );
        }
        let summary_metadata_json =
            serde_json::to_string(&JsonValue::Object(summary_metadata)).ok();
        let draft = LcmSummaryNodeDraft {
            provider: message.provider.clone(),
            conversation_id: message.session_id.clone(),
            session_id: message.session_id.clone(),
            depth,
            summary_text: summary_text.clone(),
            source_refs: sources.refs,
            summary_token_count: estimate_summary_tokens(&summary_text),
            source_token_count: sources.source_token_count,
            source_time_start: sources.source_time_start,
            source_time_end: sources.source_time_end.or(message.timestamp),
            expand_hint: Some("Codex context compaction boundary".to_string()),
            metadata_json: summary_metadata_json.or_else(|| Some(metadata_json.to_string())),
        };
        let publisher =
            crate::session_temporal_operations::GlobalDbLcmSummaryPublication::new(conn);
        tracedecay_sessions::runtime::lcm::dag::insert_summary_node(&publisher, draft)
            .await
            .map(|_| ())
            .map_err(|error| {
                TranscriptPersistenceError::storage("upsert transcript summary projection", error)
            })
    }

    async fn transcript_summary_sources(
        conn: &impl Executor,
        message: &SessionMessageRecord,
    ) -> Result<TranscriptSummarySources, TranscriptPersistenceError> {
        let mut rows = conn
            .query(
                "SELECT r.store_id, r.timestamp,
                        length(COALESCE(r.content, r.snippet_text, '')),
                        r.role,
                        substr(COALESCE(r.content, r.snippet_text, ''), 1, 4000)
                 FROM lcm_raw_messages r
                 JOIN session_messages m
                   ON m.provider = r.provider
                  AND m.message_id = r.message_id
                 WHERE r.provider = ?1
                   AND r.session_id = ?2
                   AND r.ordinal < ?3
                   AND r.ordinal > COALESCE((
                       SELECT MAX(prev.ordinal)
                       FROM session_messages prev
                       WHERE prev.provider = ?1
                         AND prev.session_id = ?2
                         AND prev.ordinal < ?3
                         AND COALESCE(prev.kind, 'message') = 'summary'
                   ), -9223372036854775808)
                   AND COALESCE(m.kind, 'message') <> 'summary'
                 ORDER BY r.store_id",
                params![
                    message.provider.as_str(),
                    message.session_id.as_str(),
                    message.ordinal,
                ],
            )
            .await
            .map_err(|error| {
                TranscriptPersistenceError::storage(
                    "load transcript compaction summary sources",
                    error,
                )
            })?;
        let mut refs = Vec::new();
        let mut source_token_count = 0_i64;
        let mut source_time_start = None;
        let mut source_time_end = None;
        let mut excerpts = Vec::new();
        while let Some(row) = rows.next().await.map_err(|error| {
            TranscriptPersistenceError::storage("load transcript compaction summary sources", error)
        })? {
            let store_id = row.get::<i64>(0).map_err(|error| {
                TranscriptPersistenceError::storage(
                    "decode transcript compaction summary source",
                    error,
                )
            })?;
            let timestamp = row.get::<Option<i64>>(1).map_err(|error| {
                TranscriptPersistenceError::storage(
                    "decode transcript compaction summary timestamp",
                    error,
                )
            })?;
            let char_count = row.get::<i64>(2).map_err(|error| {
                TranscriptPersistenceError::storage(
                    "decode transcript compaction summary character count",
                    error,
                )
            })?;
            let role = row.get::<String>(3).map_err(|error| {
                TranscriptPersistenceError::storage(
                    "decode transcript compaction summary role",
                    error,
                )
            })?;
            let excerpt_text = row.get::<String>(4).map_err(|error| {
                TranscriptPersistenceError::storage(
                    "decode transcript compaction summary excerpt",
                    error,
                )
            })?;
            refs.push(LcmSourceRef::RawMessage { store_id });
            source_token_count =
                source_token_count.saturating_add(estimated_tokens_from_chars(char_count));
            if !excerpt_text.trim().is_empty() {
                excerpts.push(TranscriptSummaryExcerpt {
                    role,
                    text: excerpt_text,
                });
            }
            if let Some(timestamp) = timestamp {
                source_time_start = Some(
                    source_time_start
                        .map_or(timestamp, |start: i64| std::cmp::min(start, timestamp)),
                );
                source_time_end = Some(
                    source_time_end.map_or(timestamp, |end: i64| std::cmp::max(end, timestamp)),
                );
            }
        }

        Ok(TranscriptSummarySources {
            refs,
            source_token_count,
            source_time_start,
            source_time_end,
            excerpts,
        })
    }

    async fn upsert_session_message_projection(
        conn: &impl Executor,
        message: &SessionMessageRecord,
        text: &str,
        metadata_json: Option<&str>,
    ) -> bool {
        conn.execute(
            "INSERT INTO session_messages
                 (provider, message_id, session_id, role, timestamp, ordinal, text, kind, model,
                  tool_names, source_path, source_offset, metadata_json)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)
             ON CONFLICT(provider, message_id) DO UPDATE SET
                session_id = excluded.session_id,
                role = excluded.role,
                timestamp = excluded.timestamp,
                ordinal = excluded.ordinal,
                text = excluded.text,
                kind = excluded.kind,
                model = excluded.model,
                tool_names = excluded.tool_names,
                source_path = excluded.source_path,
                source_offset = excluded.source_offset,
                metadata_json = excluded.metadata_json",
            params![
                message.provider.clone(),
                message.message_id.clone(),
                message.session_id.clone(),
                message.role.clone(),
                message.timestamp,
                message.ordinal,
                text,
                message.kind.clone(),
                message.model.clone(),
                message.tool_names.clone(),
                message.source_path.clone(),
                message.source_offset,
                metadata_json,
            ],
        )
        .await
        .is_ok()
    }

    /// Atomically upserts one transcript session + all parsed messages and then
    /// advances the parse cursor. Any failure rolls back the entire batch so a
    /// follow-up ingest can safely replay from the previous offset.
    pub async fn upsert_transcript_batch(
        &self,
        session: &SessionRecord,
        messages: &[SessionMessageRecord],
        parse_offset_path: &str,
        parse_offset: ParseOffset,
    ) -> bool {
        let Ok(expected_offset) = self.get_parse_offset_result(parse_offset_path).await else {
            return false;
        };
        self.persist_transcript_batch_result(
            session,
            messages,
            parse_offset_path,
            expected_offset.unwrap_or_default(),
            parse_offset,
        )
        .await
        .is_ok()
    }

    pub async fn persist_transcript_batch_result(
        &self,
        session: &SessionRecord,
        messages: &[SessionMessageRecord],
        parse_offset_path: &str,
        expected_offset: ParseOffset,
        parse_offset: ParseOffset,
    ) -> Result<(), TranscriptPersistenceError> {
        let batch = TranscriptBatch {
            session: session.clone(),
            messages: messages.to_vec(),
        };
        self.persist_transcript_batch_with_git_evidence_result(
            &batch,
            &[],
            &[],
            parse_offset_path,
            expected_offset,
            parse_offset,
        )
        .await
    }

    pub async fn persist_transcript_offset_result(
        &self,
        parse_offset_path: &str,
        expected_offset: ParseOffset,
        parse_offset: ParseOffset,
    ) -> Result<(), TranscriptPersistenceError> {
        let transaction = self.begin_transcript_transaction().await?;
        require_expected_offset(&transaction, parse_offset_path, expected_offset).await?;
        set_parse_offset(&transaction, parse_offset_path, parse_offset).await?;
        transaction
            .commit()
            .await
            .map_err(|error| TranscriptPersistenceError::storage("commit transcript batch", error))
    }

    /// Atomically persists transcript rows, direct commit evidence, and the
    /// parse cursor so a failed attribution write is replayed on the next sync.
    pub async fn persist_transcript_batch_with_git_evidence_result(
        &self,
        batch: &TranscriptBatch,
        commit_records: &[tracedecay_sessions::runtime::git_correlation::CommitSessionRecord],
        span_observations: &[tracedecay_sessions::runtime::git_correlation::SpanObservation],
        parse_offset_path: &str,
        expected_offset: ParseOffset,
        parse_offset: ParseOffset,
    ) -> Result<(), TranscriptPersistenceError> {
        self.upsert_transcript_batches_inner(
            std::slice::from_ref(batch),
            commit_records,
            span_observations,
            parse_offset_path,
            parse_offset,
            TranscriptWritePolicy::Full { expected_offset },
        )
        .await
    }

    /// Atomically upserts several transcript sessions (and their messages),
    /// writing only the searchable `session_messages` projection — never
    /// `lcm_raw_messages` — and then advances one shared parse cursor.
    pub async fn upsert_transcript_projection_batches(
        &self,
        batches: &[TranscriptBatch],
        parse_offset_path: &str,
        parse_offset: ParseOffset,
    ) -> Result<(), String> {
        self.upsert_transcript_batches_inner(
            batches,
            &[],
            &[],
            parse_offset_path,
            parse_offset,
            TranscriptWritePolicy::ProjectionOnly,
        )
        .await
        .map_err(|error| error.to_string())
    }

    async fn upsert_transcript_batches_inner(
        &self,
        batches: &[TranscriptBatch],
        commit_records: &[tracedecay_sessions::runtime::git_correlation::CommitSessionRecord],
        span_observations: &[tracedecay_sessions::runtime::git_correlation::SpanObservation],
        parse_offset_path: &str,
        parse_offset: ParseOffset,
        policy: TranscriptWritePolicy,
    ) -> Result<(), TranscriptPersistenceError> {
        let transaction = self.begin_transcript_transaction().await?;
        let storage_root = self
            .db_path()
            .parent()
            .unwrap_or_else(|| std::path::Path::new("."));
        let mut payload_rollback =
            tracedecay_sessions::runtime::lcm::payload::PayloadFileRollback::begin_cancellation_safe(
                storage_root,
            );

        let write_result: Result<(), TranscriptPersistenceError> = async {
            if let TranscriptWritePolicy::Full { expected_offset } = policy {
                require_expected_offset(&transaction, parse_offset_path, expected_offset).await?;
            }
            for batch in batches {
                if !Self::upsert_session_in_existing_tx(&transaction, &batch.session).await {
                    return Err(TranscriptPersistenceError::message(
                        "upsert transcript session",
                        "database write failed",
                    ));
                }
                for message in &batch.messages {
                    match policy {
                        TranscriptWritePolicy::Full { .. } => {
                            self.upsert_session_message_in_existing_tx(
                                &transaction,
                                message,
                                &mut payload_rollback,
                            )
                            .await?;
                        }
                        TranscriptWritePolicy::ProjectionOnly => {
                            let text =
                                tracedecay_sessions::compatibility::derived_text_for_index(
                                    &message.text,
                                );
                            if !Self::upsert_session_message_projection(
                                &transaction,
                                message,
                                &text,
                                message.metadata_json.as_deref(),
                            )
                            .await
                            {
                                return Err(TranscriptPersistenceError::message(
                                    "upsert session message projection",
                                    "database write failed",
                                ));
                            }
                        }
                    }
                }
            }
            for record in commit_records {
                tracedecay_sessions::runtime::git_correlation::upsert_commit_session(&transaction, record)
                    .await
                    .map_err(|error| {
                        TranscriptPersistenceError::storage("upsert commit evidence", error)
                    })?;
            }
            for observation in span_observations {
                tracedecay_sessions::runtime::git_correlation::record_span_observation_in_transaction(
                    &transaction,
                    observation,
                    tracedecay_sessions::runtime::git_correlation::DEFAULT_SPAN_MERGE_GAP_SECS,
                )
                .await
                .map_err(|error| {
                    TranscriptPersistenceError::storage("upsert span evidence", error)
                })?;
            }
            if matches!(policy, TranscriptWritePolicy::Full { .. }) {
                set_parse_offset(&transaction, parse_offset_path, parse_offset).await?;
            } else {
                Self::set_parse_offset_monotonic_in_existing_tx(
                    &transaction,
                    parse_offset_path,
                    parse_offset,
                )
                .await
                .map_err(|message| {
                    TranscriptPersistenceError::message("advance projection parse offset", message)
                })?;
            }
            Ok(())
        }
        .await;

        write_result?;
        transaction.commit().await.map_err(|error| {
            TranscriptPersistenceError::storage("commit transcript batch", error)
        })?;
        payload_rollback.disarm();
        Ok(())
    }

    pub async fn get_parse_offset(&self, path: &str) -> Option<ParseOffset> {
        self.get_parse_offset_result(path).await.ok().flatten()
    }

    pub async fn get_parse_offset_result(
        &self,
        path: &str,
    ) -> Result<Option<ParseOffset>, TranscriptPersistenceError> {
        // Per-transcript point lookup on the shared registered reader pool: take
        // one short-held query lease rather than pinning a snapshot worker for
        // the whole read.
        get_parse_offset(self.read_connection(), path).await
    }

    pub async fn set_parse_offset(&self, path: &str, offset: ParseOffset) -> Result<(), String> {
        let transaction = self
            .begin_transcript_transaction()
            .await
            .map_err(|error| error.to_string())?;
        set_parse_offset(&transaction, path, offset)
            .await
            .map_err(|error| error.to_string())?;
        transaction
            .commit()
            .await
            .map_err(|error| format!("commit transcript parse offset: {error}"))
    }

    pub async fn advance_parse_offset_result(
        &self,
        path: &str,
        offset: ParseOffset,
    ) -> Result<(), TranscriptPersistenceError> {
        let transaction = self.begin_transcript_transaction().await?;
        Self::set_parse_offset_monotonic_in_existing_tx(&transaction, path, offset)
            .await
            .map_err(|message| {
                TranscriptPersistenceError::message("advance transcript parse offset", message)
            })?;
        transaction.commit().await.map_err(|error| {
            TranscriptPersistenceError::storage("commit transcript parse offset", error)
        })
    }

    async fn set_parse_offset_monotonic_in_existing_tx(
        conn: &impl Executor,
        path: &str,
        offset: ParseOffset,
    ) -> Result<(), String> {
        conn.execute(
            "INSERT INTO parse_offsets (file_path, byte_offset, mtime, file_id)
                 VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT(file_path) DO UPDATE SET
                    byte_offset = excluded.byte_offset,
                    mtime = excluded.mtime,
                    file_id = excluded.file_id
                 WHERE excluded.file_id != parse_offsets.file_id
                    OR excluded.mtime > parse_offsets.mtime
                    OR (excluded.mtime = parse_offsets.mtime
                        AND excluded.byte_offset >= parse_offsets.byte_offset)",
            params![
                path,
                offset.byte_offset as i64,
                offset.mtime as i64,
                offset.file_id as i64
            ],
        )
        .await
        .map(|_| ())
        .map_err(|error| format!("failed to advance transcript parse offset: {error}"))
    }
}
