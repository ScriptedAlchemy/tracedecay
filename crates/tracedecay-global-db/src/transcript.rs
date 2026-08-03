use std::error::Error;

use super::{ParseOffset, RegisteredGlobalDb, RegisteredGlobalDbWriteTransaction, TranscriptBatch};
use tracedecay_runtime_core::db::engine::{Executor, QueryExecutor, Row, params};
use tracedecay_sessions::runtime::{SessionMessageRecord, SessionRecord};

#[derive(Debug, Clone, Copy)]
enum TranscriptWritePolicy {
    CheckedOffset { expected_offset: ParseOffset },
    MonotonicOffset,
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
    ) -> Result<(), TranscriptPersistenceError> {
        let mut canonical_message = message.clone();
        canonical_message.timestamp = Self::normalize_session_message_timestamp(message.timestamp);
        if !Self::upsert_session_message_projection(
            conn,
            &canonical_message,
            None,
            &canonical_message.text,
            canonical_message.metadata_json.as_deref(),
        )
        .await
        {
            return Err(TranscriptPersistenceError::message(
                "upsert session message projection",
                "database write failed",
            ));
        }
        Ok(())
    }

    async fn upsert_session_message_projection(
        conn: &impl Executor,
        message: &SessionMessageRecord,
        occurrence_id: Option<&str>,
        text: &str,
        metadata_json: Option<&str>,
    ) -> bool {
        let snippet_text = tracedecay_sessions::compatibility::derived_text_for_snippet(text);
        let index_text = tracedecay_sessions::compatibility::derived_text_for_index(text);
        conn.execute(
            "INSERT INTO session_messages
                 (provider, message_id, session_id, role, timestamp, ordinal, occurrence_id,
                  snippet_text, index_text, kind, model, tool_names, source_path, source_offset,
                  metadata_json)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)
             ON CONFLICT(provider, message_id) DO UPDATE SET
                session_id = excluded.session_id,
                role = excluded.role,
                timestamp = excluded.timestamp,
                ordinal = excluded.ordinal,
                occurrence_id = excluded.occurrence_id,
                snippet_text = excluded.snippet_text,
                index_text = excluded.index_text,
                kind = excluded.kind,
                model = excluded.model,
                tool_names = excluded.tool_names,
                source_path = excluded.source_path,
                source_offset = excluded.source_offset,
                metadata_json = excluded.metadata_json
             WHERE session_messages.occurrence_id IS NULL
                OR excluded.occurrence_id IS NOT NULL",
            params![
                message.provider.as_str(),
                message.message_id.as_str(),
                message.session_id.as_str(),
                message.role.as_str(),
                message.timestamp,
                message.ordinal,
                occurrence_id,
                snippet_text.as_str(),
                index_text.as_str(),
                message.kind.as_deref(),
                message.model.as_deref(),
                message.tool_names.as_deref(),
                message.source_path.as_deref(),
                message.source_offset,
                metadata_json,
            ],
        )
        .await
        .is_ok()
    }

    /// Atomically upserts bounded provisional projections and advances the
    /// parse cursor. Canonical bodies arrive only through observation admission.
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

    /// Atomically persists bounded projections, direct commit evidence, and the
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
            TranscriptWritePolicy::CheckedOffset { expected_offset },
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
            TranscriptWritePolicy::MonotonicOffset,
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

        let write_result: Result<(), TranscriptPersistenceError> = async {
            if let TranscriptWritePolicy::CheckedOffset { expected_offset } = policy {
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
                    self.upsert_session_message_in_existing_tx(&transaction, message)
                        .await?;
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
            if matches!(policy, TranscriptWritePolicy::CheckedOffset { .. }) {
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
