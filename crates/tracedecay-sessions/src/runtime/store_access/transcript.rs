use tracedecay_runtime_core::db::engine::{Executor, QueryExecutor, Row, params};
use tracedecay_store::{ParseOffset, SessionMessageRecord, SessionRecord};

use tracedecay_lcm::payload::PayloadFileRollback;
use tracedecay_lcm::raw;
use tracedecay_lcm::retrieval_content::derived_text_for_index;

use super::super::registered_db::{SessionRegisteredDb, SessionStoreAccess, SessionWriteTxn};
use super::types::{TranscriptBatch, TranscriptPersistenceError};

#[derive(Debug, Clone, Copy)]
enum TranscriptWritePolicy {
    Full { expected_offset: ParseOffset },
    ProjectionOnly,
}

#[hotpath::measure(label = "sessions.transcript.offset_read", future = true)]
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

#[hotpath::measure(label = "sessions.transcript.offset_write", future = true)]
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
    pub(super) async fn begin_transcript_transaction(
        &self,
    ) -> Result<D::WriteTxn<'_>, TranscriptPersistenceError> {
        self.begin_write_transaction()
            .await
            .map_err(|error| TranscriptPersistenceError::storage("begin transcript batch", error))
    }

    #[hotpath::measure(label = "sessions.transcript.session_upsert", future = true)]
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

    #[hotpath::measure(label = "sessions.transcript.session_read", future = true)]
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

    #[hotpath::measure(label = "sessions.transcript.message_upsert", future = true)]
    async fn upsert_session_message_in_existing_tx(
        &self,
        conn: &impl Executor,
        message: &SessionMessageRecord,
        payload_rollback: &mut PayloadFileRollback,
    ) -> Result<(), TranscriptPersistenceError> {
        let mut canonical_message = message.clone();
        canonical_message.timestamp = Self::normalize_session_message_timestamp(message.timestamp);
        let storage_root = self
            .db_path()
            .parent()
            .unwrap_or_else(|| std::path::Path::new("."));
        let raw = raw::upsert_raw_message_with_payload_tracked(
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
        Ok(())
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
        self.upsert_transcript_batches_inner(
            std::slice::from_ref(&batch),
            parse_offset_path,
            parse_offset,
            TranscriptWritePolicy::Full { expected_offset },
        )
        .await
    }

    #[hotpath::measure(label = "sessions.transcript.offset_commit", future = true)]
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
        )
        .await
        .map_err(|error| error.to_string())
    }

    #[hotpath::measure(label = "sessions.transcript.batch_persist", future = true)]
    async fn upsert_transcript_batches_inner(
        &self,
        batches: &[TranscriptBatch],
        parse_offset_path: &str,
        parse_offset: ParseOffset,
        policy: TranscriptWritePolicy,
    ) -> Result<(), TranscriptPersistenceError> {
        let storage_root = self
            .db_path()
            .parent()
            .unwrap_or_else(|| std::path::Path::new("."));
        let transaction = self.begin_transcript_transaction().await?;
        let mut payload_rollback = PayloadFileRollback::begin_cancellation_safe(storage_root);

        let write_result: Result<(), TranscriptPersistenceError> = async {
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
                            let text = derived_text_for_index(&message.text);
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
        let reader = self.read_connection();
        get_parse_offset(&reader, path).await
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

    #[hotpath::measure(label = "sessions.transcript.offset_advance", future = true)]
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
    #[hotpath::measure(label = "sessions.transcript.offset_replace", future = true)]
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
    #[hotpath::measure(label = "sessions.transcript.offset_replace_pair", future = true)]
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
    use super::{decode_file_id_value, encode_file_id};

    #[test]
    fn transcript_file_id_encoding_round_trips_the_full_u64_domain() {
        for file_id in [0, i64::MAX as u64, (i64::MAX as u64) + 1, u64::MAX] {
            assert_eq!(decode_file_id_value(encode_file_id(file_id)), file_id);
        }
    }
}
