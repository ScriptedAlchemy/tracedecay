use tracedecay_runtime_core::db::engine::{Executor, QueryExecutor, Row, WriteStatement, params};
use tracedecay_store::{ParseOffset, SessionMessageRecord, SessionRecord, StoreShardScopeV1};

use tracedecay_lcm::payload::PayloadFileRollback;
use tracedecay_lcm::raw;
use tracedecay_lcm::retrieval_content::derived_text_for_index;

use super::super::git_correlation::{
    CommitSessionRecord, SpanObservation, enqueue_git_evidence_publication,
};
use super::super::registered_db::{SessionRegisteredDb, SessionStoreAccess, SessionWriteTxn};
use super::codex_goal_reconciliation::find_preceding_codex_goal_response;
use super::types::{TranscriptBatch, TranscriptPersistenceError};

#[derive(Debug, Clone, Copy)]
enum TranscriptWritePolicy {
    Full { expected_offset: ParseOffset },
    ProjectionOnly,
}

/// Exact Git evidence staged atomically with one transcript write.
#[derive(Debug, Clone, Copy)]
pub struct TranscriptGitEvidence<'a> {
    publication_prefix: &'a str,
    commit_records: &'a [CommitSessionRecord],
    span_observations: &'a [SpanObservation],
}

impl<'a> TranscriptGitEvidence<'a> {
    pub const fn new(
        publication_prefix: &'a str,
        commit_records: &'a [CommitSessionRecord],
        span_observations: &'a [SpanObservation],
    ) -> Self {
        Self {
            publication_prefix,
            commit_records,
            span_observations,
        }
    }
}

const TRANSCRIPT_STATEMENT_WINDOW: usize = 64;

/// Prepares every privacy-protected raw-message write before the transaction
/// acquires SQLite's single-writer lease.
///
/// The returned vector preserves batch/message order so the transactional
/// phase can pair each staged payload with its canonical projection row.
#[hotpath::measure(label = "sessions.store.transcript.stage_messages")]
fn stage_full_transcript_messages(
    storage_root: &std::path::Path,
    batches: &[TranscriptBatch],
    payload_rollback: &mut PayloadFileRollback,
) -> Result<Vec<raw::StagedRawMessageIngest>, TranscriptPersistenceError> {
    let message_count = batches.iter().fold(0_usize, |count, batch| {
        count.saturating_add(batch.messages.len())
    });
    let mut staged = Vec::with_capacity(message_count);
    for batch in batches {
        for message in &batch.messages {
            staged.push(
                raw::stage_raw_message_with_payload_tracked(
                    storage_root,
                    message,
                    payload_rollback,
                )
                .map_err(|error| {
                    TranscriptPersistenceError::storage("upsert LCM raw message", error)
                })?,
            );
        }
    }
    Ok(staged)
}

#[hotpath::measure(
    label = "sessions.store.transcript.flush_statement_window",
    future = true
)]
async fn flush_transcript_statement_window(
    conn: &impl Executor,
    statements: &mut Vec<WriteStatement>,
) -> Result<(), TranscriptPersistenceError> {
    if statements.is_empty() {
        return Ok(());
    }
    conn.execute_statements(std::mem::take(statements))
        .await
        .map(|_| ())
        .map_err(|error| {
            TranscriptPersistenceError::storage("upsert session message projections", error)
        })
}

async fn reconcile_codex_goal_response(
    conn: &impl Executor,
    current: &SessionMessageRecord,
) -> Result<(), TranscriptPersistenceError> {
    let Some(response_message_id) = find_preceding_codex_goal_response(conn, current)
        .await
        .map_err(|error| {
            TranscriptPersistenceError::storage("find preceding Codex goal response", error)
        })?
    else {
        return Ok(());
    };
    conn.execute(
        "DELETE FROM lcm_raw_messages WHERE provider = ?1 AND message_id = ?2",
        params![current.provider.as_str(), response_message_id.as_str()],
    )
    .await
    .map_err(|error| {
        TranscriptPersistenceError::storage("remove paired Codex goal raw message", error)
    })?;
    conn.execute(
        "DELETE FROM session_messages WHERE provider = ?1 AND message_id = ?2",
        params![current.provider.as_str(), response_message_id.as_str()],
    )
    .await
    .map(|_| ())
    .map_err(|error| {
        TranscriptPersistenceError::storage("remove paired Codex goal projection", error)
    })
}

pub async fn get_parse_offset(
    conn: &impl QueryExecutor,
    path: &str,
) -> Result<Option<ParseOffset>, TranscriptPersistenceError> {
    match conn
        .query(
            "SELECT byte_offset, mtime, file_id FROM parse_offsets WHERE file_path = ?1",
            params![path],
        )
        .await
    {
        Ok(mut rows) => {
            let Some(row) = rows.next().await.map_err(|error| {
                TranscriptPersistenceError::storage("read transcript parse offset", error)
            })?
            else {
                return Ok(None);
            };
            Ok(Some(ParseOffset {
                byte_offset: decode_u64(&row, 0, "decode transcript byte offset")?,
                mtime: decode_u64(&row, 1, "decode transcript mtime")?,
                file_id: decode_file_id(&row, 2, "decode transcript file id")?,
            }))
        }
        Err(error) if sqlite_missing_column(&error, "file_id") => {
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
            Ok(Some(ParseOffset {
                byte_offset: decode_u64(&row, 0, "decode transcript byte offset")?,
                mtime: decode_u64(&row, 1, "decode transcript mtime")?,
                file_id: 0,
            }))
        }
        Err(error) => Err(TranscriptPersistenceError::storage(
            "read transcript parse offset",
            error,
        )),
    }
}

fn sqlite_missing_column(error: &tracedecay_runtime_core::db::engine::Error, column: &str) -> bool {
    match error {
        tracedecay_runtime_core::db::engine::Error::Sqlite { message, .. } => {
            message.contains(&format!("no such column: {column}"))
        }
        _ => false,
    }
}

fn decode_u64(
    row: &Row,
    index: i32,
    operation: &'static str,
) -> Result<u64, TranscriptPersistenceError> {
    let value = row
        .get::<i64>(index)
        .map_err(|error| TranscriptPersistenceError::storage(operation, error))?;
    u64::try_from(value).map_err(|error| TranscriptPersistenceError::storage(operation, error))
}

fn encode_i64(value: u64, operation: &'static str) -> Result<i64, TranscriptPersistenceError> {
    i64::try_from(value).map_err(|error| TranscriptPersistenceError::storage(operation, error))
}

fn decode_file_id(
    row: &Row,
    index: i32,
    operation: &'static str,
) -> Result<u64, TranscriptPersistenceError> {
    let value = row
        .get::<i64>(index)
        .map_err(|error| TranscriptPersistenceError::storage(operation, error))?;
    Ok(decode_file_id_value(value))
}

fn encode_file_id(value: u64) -> i64 {
    i64::from_le_bytes(value.to_le_bytes())
}

fn decode_file_id_value(value: i64) -> u64 {
    u64::from_le_bytes(value.to_le_bytes())
}

pub async fn require_expected_offset(
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

pub async fn set_parse_offset(
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
            encode_i64(offset.byte_offset, "encode transcript byte offset")?,
            encode_i64(offset.mtime, "encode transcript mtime")?,
            encode_file_id(offset.file_id)
        ],
    )
    .await
    .map(|_| ())
    .map_err(|error| TranscriptPersistenceError::storage("write transcript parse offset", error))
}

impl<D: SessionRegisteredDb + Sync> SessionStoreAccess<'_, D> {
    #[hotpath::skip]
    pub(super) async fn begin_transcript_transaction(
        &self,
    ) -> Result<D::WriteTxn<'_>, TranscriptPersistenceError> {
        self.begin_write_transaction()
            .await
            .map_err(|error| TranscriptPersistenceError::storage("begin transcript batch", error))
    }

    #[hotpath::skip]
    pub async fn upsert_session(&self, session: &SessionRecord) -> bool {
        let Ok(transaction) = self.begin_transcript_transaction().await else {
            return false;
        };
        if !Self::upsert_session_in_existing_tx(&transaction, session).await {
            return false;
        }
        transaction.commit().await.is_ok()
    }

    #[hotpath::skip]
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

    #[hotpath::skip]
    pub async fn get_session(&self, provider: &str, session_id: &str) -> Option<SessionRecord> {
        self.get_session_result(provider, session_id)
            .await
            .ok()
            .flatten()
    }

    #[hotpath::skip]
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

    #[hotpath::skip]
    async fn upsert_session_message_in_existing_tx(
        &self,
        conn: &impl Executor,
        message: &SessionMessageRecord,
        staged: raw::StagedRawMessageIngest,
    ) -> Result<WriteStatement, TranscriptPersistenceError> {
        // Clone-on-normalize: message text can be hundreds of kilobytes, and
        // most providers already emit in-range timestamps, so the full-record
        // copy is paid only when the timestamp actually changes.
        let normalized_timestamp = Self::normalize_session_message_timestamp(message.timestamp);
        let canonical_message = if normalized_timestamp == message.timestamp {
            std::borrow::Cow::Borrowed(message)
        } else {
            let mut owned = message.clone();
            owned.timestamp = normalized_timestamp;
            std::borrow::Cow::Owned(owned)
        };
        let raw = raw::commit_staged_raw_message(conn, canonical_message.as_ref(), staged)
            .await
            .map_err(|error| {
                TranscriptPersistenceError::storage("upsert LCM raw message", error)
            })?;
        Self::session_message_projection_statement(
            canonical_message.as_ref(),
            &raw.projection_text,
            raw.projection_metadata_json.as_deref(),
        )
    }

    fn session_message_projection_statement(
        message: &SessionMessageRecord,
        text: &str,
        metadata_json: Option<&str>,
    ) -> Result<WriteStatement, TranscriptPersistenceError> {
        WriteStatement::new(
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
        .map_err(|error| {
            TranscriptPersistenceError::storage("prepare session message projection", error)
        })
    }

    /// Atomically upserts one transcript session + all parsed messages and then
    /// advances the parse cursor. Any failure rolls back the entire batch so a
    /// follow-up ingest can safely replay from the previous offset.
    #[hotpath::skip]
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

    #[hotpath::skip]
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
        self.upsert_transcript_batches_inner(
            std::slice::from_ref(&batch),
            parse_offset_path,
            parse_offset,
            TranscriptWritePolicy::Full { expected_offset },
            None,
        )
        .await
    }

    /// Atomically commits parsed transcript rows, the parse cursor, and the
    /// exact receipt needed to publish derived Git evidence after commit.
    #[hotpath::skip]
    pub async fn persist_transcript_batch_with_git_evidence_result(
        &self,
        session: &SessionRecord,
        messages: &[SessionMessageRecord],
        parse_offset_path: &str,
        expected_offset: ParseOffset,
        parse_offset: ParseOffset,
        git_evidence: TranscriptGitEvidence<'_>,
    ) -> Result<(), TranscriptPersistenceError> {
        if (!git_evidence.commit_records.is_empty() || !git_evidence.span_observations.is_empty())
            && !matches!(
                &self.registered_binding().shard_id.scope,
                StoreShardScopeV1::ProjectSessions { .. }
            )
        {
            return Err(TranscriptPersistenceError::storage(
                "stage transcript git evidence",
                std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "transcript Git evidence requires ProjectSessions authority",
                ),
            ));
        }
        let batch = TranscriptBatch {
            session: session.clone(),
            messages: messages.to_vec(),
        };
        self.upsert_transcript_batches_inner(
            std::slice::from_ref(&batch),
            parse_offset_path,
            parse_offset,
            TranscriptWritePolicy::Full { expected_offset },
            Some(git_evidence),
        )
        .await
    }

    #[hotpath::skip]
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

    /// Atomically upserts several transcript sessions (and their messages),
    /// writing only the searchable `session_messages` projection — never
    /// `lcm_raw_messages` — and then advances one shared parse cursor.
    #[hotpath::skip]
    pub async fn upsert_transcript_projection_batches(
        &self,
        batches: &[TranscriptBatch],
        parse_offset_path: &str,
        parse_offset: ParseOffset,
    ) -> Result<(), String> {
        self.upsert_transcript_batches_inner(
            batches,
            parse_offset_path,
            parse_offset,
            TranscriptWritePolicy::ProjectionOnly,
            None,
        )
        .await
        .map_err(|error| error.to_string())
    }

    #[hotpath::measure(label = "sessions.store.transcript.write_batches", future = true)]
    async fn upsert_transcript_batches_inner(
        &self,
        batches: &[TranscriptBatch],
        parse_offset_path: &str,
        parse_offset: ParseOffset,
        policy: TranscriptWritePolicy,
        git_evidence: Option<TranscriptGitEvidence<'_>>,
    ) -> Result<(), TranscriptPersistenceError> {
        let storage_root = self
            .db_path()
            .parent()
            .unwrap_or_else(|| std::path::Path::new("."));
        let mut payload_rollback = PayloadFileRollback::begin_cancellation_safe(storage_root);
        let staged_messages = match policy {
            TranscriptWritePolicy::Full { .. } => {
                stage_full_transcript_messages(storage_root, batches, &mut payload_rollback)?
            }
            TranscriptWritePolicy::ProjectionOnly => Vec::new(),
        };
        let mut staged_messages = staged_messages.into_iter();
        let transaction = self.begin_transcript_transaction().await?;

        let write_result: Result<(), TranscriptPersistenceError> = async {
            let mut projection_statements = Vec::with_capacity(TRANSCRIPT_STATEMENT_WINDOW);
            if let TranscriptWritePolicy::Full { expected_offset } = policy {
                // Full batches are one-winner compare-and-swap on the durable
                // parse cursor. `actual == next_offset` is not a retry grant:
                // a competing writer can share that destination while carrying
                // different parse products. Post-commit publication retries
                // must not re-enter this CAS with a stale expected cursor.
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
                    if tracedecay_store::codex_goal_context_correlation(
                        message.kind.as_deref(),
                        message.metadata_json.as_deref(),
                    )
                    .is_some_and(|correlation| {
                        correlation.source()
                            == tracedecay_store::CodexGoalContextSource::ItemCompleted
                    }) {
                        // Make any response written earlier in this batch
                        // visible to the bounded correlation query.
                        flush_transcript_statement_window(&transaction, &mut projection_statements)
                            .await?;
                        reconcile_codex_goal_response(&transaction, message).await?;
                    }
                    match policy {
                        TranscriptWritePolicy::Full { .. } => {
                            let staged = staged_messages.next().ok_or_else(|| {
                                TranscriptPersistenceError::message(
                                    "upsert LCM raw message",
                                    "staged transcript message count did not match the write batch",
                                )
                            })?;
                            projection_statements.push(
                                self.upsert_session_message_in_existing_tx(
                                    &transaction,
                                    message,
                                    staged,
                                )
                                .await?,
                            );
                        }
                        TranscriptWritePolicy::ProjectionOnly => {
                            let text = derived_text_for_index(&message.text);
                            projection_statements.push(Self::session_message_projection_statement(
                                message,
                                &text,
                                message.metadata_json.as_deref(),
                            )?);
                        }
                    }
                    if projection_statements.len() >= TRANSCRIPT_STATEMENT_WINDOW {
                        flush_transcript_statement_window(&transaction, &mut projection_statements)
                            .await?;
                    }
                }
            }
            flush_transcript_statement_window(&transaction, &mut projection_statements).await?;
            if matches!(policy, TranscriptWritePolicy::Full { .. })
                && staged_messages.next().is_some()
            {
                return Err(TranscriptPersistenceError::message(
                    "upsert LCM raw message",
                    "staged transcript message count exceeded the write batch",
                ));
            }
            if let Some(evidence) = git_evidence {
                enqueue_git_evidence_publication(
                    &transaction,
                    evidence.publication_prefix,
                    evidence.commit_records,
                    evidence.span_observations,
                )
                .await
                .map_err(|error| {
                    TranscriptPersistenceError::storage("stage transcript git evidence", error)
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

    #[hotpath::skip]
    pub async fn get_parse_offset(&self, path: &str) -> Option<ParseOffset> {
        self.get_parse_offset_result(path).await.ok().flatten()
    }

    #[hotpath::skip]
    pub async fn get_parse_offset_result(
        &self,
        path: &str,
    ) -> Result<Option<ParseOffset>, TranscriptPersistenceError> {
        // Per-transcript point lookup on the shared registered reader pool: take
        // one short-held query lease rather than pinning a snapshot worker for
        // the whole read.
        let reader = self.read_connection();
        get_parse_offset(&reader, path).await
    }

    #[hotpath::skip]
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

    #[hotpath::skip]
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

    /// Exact compare-and-set for versioned parse-offset authorities whose
    /// numeric fields are not monotonic transcript positions.
    #[hotpath::skip]
    pub async fn replace_parse_offset_result(
        &self,
        path: &str,
        expected: ParseOffset,
        next: ParseOffset,
    ) -> Result<(), TranscriptPersistenceError> {
        let transaction = self.begin_transcript_transaction().await?;
        require_expected_offset(&transaction, path, expected).await?;
        set_parse_offset(&transaction, path, next).await?;
        transaction.commit().await.map_err(|error| {
            TranscriptPersistenceError::storage("commit transcript parse-offset replacement", error)
        })
    }

    /// Atomically compare-and-replace two parse-offset keys. Both expected
    /// values are checked before either write and one transaction owns the
    /// pair through commit.
    #[hotpath::skip]
    pub async fn replace_parse_offset_pair_result(
        &self,
        first: (&str, ParseOffset, ParseOffset),
        second: (&str, ParseOffset, ParseOffset),
    ) -> Result<(), TranscriptPersistenceError> {
        if first.0 == second.0 {
            return Err(TranscriptPersistenceError::storage(
                "replace transcript parse-offset pair",
                std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "parse-offset pair keys must be distinct",
                ),
            ));
        }
        let transaction = self.begin_transcript_transaction().await?;
        require_expected_pair_offset(&transaction, first.0, first.1).await?;
        require_expected_pair_offset(&transaction, second.0, second.1).await?;
        set_parse_offset(&transaction, first.0, first.2).await?;
        set_parse_offset(&transaction, second.0, second.2).await?;
        transaction.commit().await.map_err(|error| {
            TranscriptPersistenceError::storage(
                "commit transcript parse-offset pair replacement",
                error,
            )
        })
    }

    #[hotpath::skip]
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
                i64::try_from(offset.byte_offset)
                    .map_err(|error| format!("encode transcript byte offset: {error}"))?,
                i64::try_from(offset.mtime)
                    .map_err(|error| format!("encode transcript mtime: {error}"))?,
                encode_file_id(offset.file_id)
            ],
        )
        .await
        .map(|_| ())
        .map_err(|error| format!("failed to advance transcript parse offset: {error}"))
    }
}

async fn require_expected_pair_offset(
    conn: &impl QueryExecutor,
    path: &str,
    expected: ParseOffset,
) -> Result<(), TranscriptPersistenceError> {
    match require_expected_offset(conn, path, expected).await {
        Err(TranscriptPersistenceError::Conflict { expected, actual }) => {
            Err(TranscriptPersistenceError::PairConflict {
                path: path.to_owned(),
                expected,
                actual,
            })
        }
        result => result,
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use tracedecay_runtime_core::db::engine::{
        Executor, IntoParams, QueryExecutor, Rows, WriteStatement, params,
    };
    use tracedecay_store::{SessionMessageRecord, SessionRecord};

    use super::{
        PayloadFileRollback, TranscriptBatch, TranscriptPersistenceError, decode_file_id_value,
        encode_file_id, flush_transcript_statement_window, stage_full_transcript_messages,
    };

    #[derive(Default)]
    struct BatchCountingExecutor {
        batch_submissions: AtomicUsize,
    }

    impl QueryExecutor for BatchCountingExecutor {
        async fn query<P>(
            &self,
            _sql: &str,
            _params: P,
        ) -> tracedecay_runtime_core::db::engine::Result<Rows>
        where
            P: IntoParams,
        {
            panic!("statement-window test must not query")
        }
    }

    impl Executor for BatchCountingExecutor {
        async fn execute<P>(
            &self,
            _sql: &str,
            _params: P,
        ) -> tracedecay_runtime_core::db::engine::Result<u64>
        where
            P: IntoParams,
        {
            panic!("statement-window test must not submit scalar writes")
        }

        async fn execute_statements(
            &self,
            statements: Vec<WriteStatement>,
        ) -> tracedecay_runtime_core::db::engine::Result<Vec<u64>> {
            self.batch_submissions.fetch_add(1, Ordering::Relaxed);
            Ok(vec![1; statements.len()])
        }

        async fn execute_batch(
            &self,
            _sql: &str,
        ) -> tracedecay_runtime_core::db::engine::Result<()> {
            panic!("statement-window test must not submit raw SQL batches")
        }
    }

    #[test]
    fn transcript_file_id_encoding_round_trips_the_full_u64_domain() {
        for file_id in [0, i64::MAX as u64, (i64::MAX as u64) + 1, u64::MAX] {
            assert_eq!(decode_file_id_value(encode_file_id(file_id)), file_id);
        }
    }

    #[test]
    fn full_transcript_staging_preserves_sanitization_failure_attribution() {
        let temp = tempfile::tempdir().unwrap();
        let storage_root = temp.path().join("store");
        std::fs::create_dir(&storage_root).unwrap();
        let batch = TranscriptBatch {
            session: SessionRecord {
                provider: "claude".to_owned(),
                session_id: "session-1".to_owned(),
                project_key: "/tmp/project".to_owned(),
                project_path: "/tmp/project".to_owned(),
                title: None,
                started_at: None,
                ended_at: None,
                transcript_path: None,
                metadata_json: None,
                parent_session_id: None,
                is_subagent: false,
                agent_id: None,
                parent_tool_use_id: None,
            },
            messages: vec![SessionMessageRecord {
                provider: "claude".to_owned(),
                message_id: "message-1".to_owned(),
                session_id: "session-1".to_owned(),
                role: "assistant".to_owned(),
                timestamp: Some(1),
                ordinal: 1,
                text: "ordinary content".to_owned(),
                kind: None,
                model: None,
                tool_names: None,
                source_path: None,
                source_offset: None,
                metadata_json: Some("[]".to_owned()),
            }],
        };
        let mut rollback = PayloadFileRollback::begin_cancellation_safe(&storage_root);

        let error = match stage_full_transcript_messages(
            &storage_root,
            std::slice::from_ref(&batch),
            &mut rollback,
        ) {
            Ok(_) => panic!("non-object metadata must be refused before transaction acquisition"),
            Err(error) => error,
        };

        match error {
            TranscriptPersistenceError::Storage { operation, source } => {
                assert_eq!(operation, "upsert LCM raw message");
                assert!(
                    source
                        .to_string()
                        .contains("LCM metadata sanitization failed"),
                    "sanitization cause was lost: {source}"
                );
            }
            other => panic!("expected attributed sanitization failure, got {other}"),
        }
    }

    #[tokio::test]
    async fn transcript_statement_window_uses_one_batch_submission() {
        let executor = BatchCountingExecutor::default();
        let mut statements = vec![
            WriteStatement::new("INSERT INTO example(value) VALUES (?1)", params![1_i64]).unwrap(),
            WriteStatement::new("INSERT INTO example(value) VALUES (?1)", params![2_i64]).unwrap(),
        ];

        flush_transcript_statement_window(&executor, &mut statements)
            .await
            .unwrap();

        assert!(statements.is_empty());
        assert_eq!(executor.batch_submissions.load(Ordering::Relaxed), 1);
    }
}
