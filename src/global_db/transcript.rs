use std::error::Error;

use libsql::{Connection, params};

use super::{GlobalDb, GlobalDbWriteTransaction, ParseOffset, TranscriptBatch};
use crate::sessions::{SessionMessageRecord, SessionRecord};

#[derive(Debug, Clone, Copy)]
enum TranscriptWritePolicy {
    Full { expected_offset: ParseOffset },
    ProjectionOnly,
}

#[derive(Debug)]
pub(crate) enum TranscriptPersistenceError {
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
    pub(crate) fn storage(
        operation: &'static str,
        source: impl Error + Send + Sync + 'static,
    ) -> Self {
        Self::Storage {
            operation,
            source: Box::new(source),
        }
    }

    pub(crate) fn message(operation: &'static str, message: impl Into<String>) -> Self {
        Self::storage(operation, std::io::Error::other(message.into()))
    }
}

pub(super) async fn get_parse_offset(
    conn: &Connection,
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
    row: &libsql::Row,
    index: i32,
    operation: &'static str,
) -> Result<u64, TranscriptPersistenceError> {
    row.get::<i64>(index)
        .map(|value| value as u64)
        .map_err(|error| TranscriptPersistenceError::storage(operation, error))
}

pub(super) async fn require_expected_offset(
    conn: &Connection,
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
    conn: &Connection,
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

impl GlobalDb {
    pub(super) async fn begin_transcript_transaction(
        &self,
    ) -> Result<GlobalDbWriteTransaction<'_>, TranscriptPersistenceError> {
        self.begin_write_transaction()
            .await
            .map_err(|error| TranscriptPersistenceError::storage("begin transcript batch", error))
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

    pub(crate) async fn persist_transcript_batch_result(
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

    pub(crate) async fn persist_transcript_offset_result(
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
    pub(crate) async fn persist_transcript_batch_with_git_evidence_result(
        &self,
        batch: &TranscriptBatch,
        commit_records: &[crate::sessions::git_correlation::CommitSessionRecord],
        span_observations: &[crate::sessions::git_correlation::SpanObservation],
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
    ) -> bool {
        self.upsert_transcript_batches_inner(
            batches,
            &[],
            &[],
            parse_offset_path,
            parse_offset,
            TranscriptWritePolicy::ProjectionOnly,
        )
        .await
        .is_ok()
    }

    async fn upsert_transcript_batches_inner(
        &self,
        batches: &[TranscriptBatch],
        commit_records: &[crate::sessions::git_correlation::CommitSessionRecord],
        span_observations: &[crate::sessions::git_correlation::SpanObservation],
        parse_offset_path: &str,
        parse_offset: ParseOffset,
        policy: TranscriptWritePolicy,
    ) -> Result<(), TranscriptPersistenceError> {
        let transaction = self.begin_transcript_transaction().await?;
        let mut payload_rollback =
            crate::sessions::lcm::payload::PayloadFileRollback::begin_cancellation_safe(
                &self.storage_root,
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
                                crate::sessions::lcm::raw::derived_text_for_index(&message.text);
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
                crate::sessions::git_correlation::upsert_commit_session(&transaction, record)
                    .await
                    .map_err(|error| {
                        TranscriptPersistenceError::storage("upsert commit evidence", error)
                    })?;
            }
            for observation in span_observations {
                crate::sessions::git_correlation::record_span_observation_in_transaction(
                    &transaction,
                    observation,
                    crate::sessions::git_correlation::DEFAULT_SPAN_MERGE_GAP_SECS,
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

    /// Returns the saved parse cursor for a JSONL file.
    pub async fn get_parse_offset(&self, path: &str) -> Option<ParseOffset> {
        self.get_parse_offset_result(path).await.ok().flatten()
    }

    pub(crate) async fn get_parse_offset_result(
        &self,
        path: &str,
    ) -> Result<Option<ParseOffset>, TranscriptPersistenceError> {
        get_parse_offset(&self.conn, path).await
    }

    /// Saves the parse cursor for a transcript path. Best-effort.
    pub async fn set_parse_offset(&self, path: &str, offset: ParseOffset) {
        let Ok(transaction) = self.begin_transcript_transaction().await else {
            return;
        };
        if Self::set_parse_offset_in_existing_tx(&transaction, path, offset).await {
            let _ = transaction.commit().await;
        }
    }

    /// Advances a transcript cursor without allowing an older sweep to move it backwards.
    pub async fn advance_parse_offset(&self, path: &str, offset: ParseOffset) {
        let _ = self.advance_parse_offset_result(path, offset).await;
    }

    pub(crate) async fn advance_parse_offset_result(
        &self,
        path: &str,
        offset: ParseOffset,
    ) -> Result<(), String> {
        let transaction = self
            .begin_write_transaction()
            .await
            .map_err(|error| format!("failed to begin transcript parse offset: {error}"))?;
        Self::set_parse_offset_monotonic_in_existing_tx(&transaction, path, offset).await?;
        transaction
            .commit()
            .await
            .map_err(|error| format!("failed to commit transcript parse offset: {error}"))
    }

    async fn set_parse_offset_monotonic_in_existing_tx(
        conn: &Connection,
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

    async fn set_parse_offset_in_existing_tx(
        conn: &Connection,
        path: &str,
        offset: ParseOffset,
    ) -> bool {
        if conn
            .execute(
                "INSERT INTO parse_offsets (file_path, byte_offset, mtime, file_id)
                 VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT(file_path) DO UPDATE SET
                    byte_offset = ?2,
                    mtime = ?3,
                    file_id = ?4",
                params![
                    path,
                    offset.byte_offset as i64,
                    offset.mtime as i64,
                    offset.file_id as i64
                ],
            )
            .await
            .is_ok()
        {
            return true;
        }
        conn.execute(
            "INSERT INTO parse_offsets (file_path, byte_offset, mtime)
                 VALUES (?1, ?2, ?3)
                 ON CONFLICT(file_path) DO UPDATE SET
                    byte_offset = ?2,
                    mtime = ?3",
            params![path, offset.byte_offset as i64, offset.mtime as i64],
        )
        .await
        .is_ok()
    }
}
