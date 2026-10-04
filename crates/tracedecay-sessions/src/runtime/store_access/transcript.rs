use tracedecay_runtime_core::db::engine::{Executor, QueryExecutor, Row, params};
use tracedecay_store::{ParseOffset, SessionRecord};

use super::super::registered_db::{SessionRegisteredDb, SessionStoreAccess, SessionWriteTxn};
use super::super::shared::{durable_project_path_key, path_identity_key};
use super::types::TranscriptPersistenceError;

/// Reads one durable cursor by its canonical key.
///
/// `path_identity_key` is applied on every write to this table, so the stored
/// form is unique and this stays a single primary-key lookup, no candidate
/// expansion, no table scan, on the per-file-per-pass ingest hot path.
pub async fn get_parse_offset(
    conn: &impl QueryExecutor,
    path: &str,
) -> Result<Option<ParseOffset>, TranscriptPersistenceError> {
    let path = path_identity_key(path);
    let path = path.as_str();
    let mut rows = conn
        .query(
            "SELECT byte_offset, mtime, file_id FROM parse_offsets WHERE file_path = ?1",
            params![path],
        )
        .await
        .map_err(|error| {
            TranscriptPersistenceError::storage("read transcript parse offset", error)
        })?;
    let Some(row) = rows.next().await.map_err(|error| {
        TranscriptPersistenceError::storage("read transcript parse offset", error)
    })?
    else {
        return Ok(None);
    };
    Ok(Some(ParseOffset {
        byte_offset: decode_u64_bits(&row, 0, "decode transcript byte offset")?,
        mtime: decode_u64_bits(&row, 1, "decode transcript mtime")?,
        file_id: decode_u64_bits(&row, 2, "decode transcript file id")?,
    }))
}

/// Every `parse_offsets` numeric column carries the full `u64` domain of its
/// `ParseOffset` field through SQLite's signed 64-bit INTEGER as a two's
/// complement bit-cast. Transcript byte positions never leave the
/// non-negative half, but the same three columns are the durable authority
/// for versioned host frontiers whose fields are digests and sentinels (the
/// Codex corpus epoch packs a 128-bit digest into `byte_offset`/`mtime`, the
/// OpenCode rewrite frontier uses `u64::MAX`), so a range-checked encode
/// refused to persist them and left every history pass retrying forever.
fn decode_u64_bits(
    row: &Row,
    index: i32,
    operation: &'static str,
) -> Result<u64, TranscriptPersistenceError> {
    let value = row
        .get::<i64>(index)
        .map_err(|error| TranscriptPersistenceError::storage(operation, error))?;
    Ok(decode_u64_bits_value(value))
}

fn encode_u64_bits(value: u64) -> i64 {
    i64::from_le_bytes(value.to_le_bytes())
}

fn decode_u64_bits_value(value: i64) -> u64 {
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

/// Writes one durable cursor under its canonical key.
///
/// Normalising here, the single write funnel for this table, is what keeps
/// [`get_parse_offset`] a primary-key lookup.
pub async fn set_parse_offset(
    conn: &impl Executor,
    path: &str,
    offset: ParseOffset,
) -> Result<(), TranscriptPersistenceError> {
    let path = path_identity_key(path);
    conn.execute(
        "INSERT INTO parse_offsets (file_path, byte_offset, mtime, file_id)
         VALUES (?1, ?2, ?3, ?4)
         ON CONFLICT(file_path) DO UPDATE SET
            byte_offset = excluded.byte_offset,
            mtime = excluded.mtime,
            file_id = excluded.file_id",
        params![
            path,
            encode_u64_bits(offset.byte_offset),
            encode_u64_bits(offset.mtime),
            encode_u64_bits(offset.file_id)
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

    pub async fn upsert_session(&self, session: &SessionRecord) -> bool {
        let Ok(transaction) = self.begin_transcript_transaction().await else {
            return false;
        };
        if !Self::upsert_session_in_existing_tx(&transaction, session).await {
            return false;
        }
        transaction.commit().await.is_ok()
    }

    /// Writes one session row with its path column in the canonical form that
    /// project-scoped reads query. `project_key` is an opaque authority and
    /// remains byte-exact; `transcript_path` remains the real display path.
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
                durable_project_path_key(&session.project_path),
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

    pub async fn get_session(
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

    pub async fn get_parse_offset(
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

    /// The SQL ordering compares the stored signed encoding, so it is exact
    /// for transcript positions and mtimes (never above `i64::MAX`); host
    /// frontiers that carry sentinels or digests in these columns advance
    /// through a changed `file_id` or a strictly greater revision `mtime`
    /// (see `opencode_frontier`), never through the byte-offset comparison.
    async fn set_parse_offset_monotonic_in_existing_tx(
        conn: &impl Executor,
        path: &str,
        offset: ParseOffset,
    ) -> Result<(), String> {
        let path = path_identity_key(path);
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
                encode_u64_bits(offset.byte_offset),
                encode_u64_bits(offset.mtime),
                encode_u64_bits(offset.file_id)
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
    use super::{decode_u64_bits_value, encode_u64_bits};

    /// Every `parse_offsets` column round-trips the whole `u64` domain: the
    /// Codex corpus epoch stores a 128-bit digest across `byte_offset` and
    /// `mtime`, so any half with its top bit set must persist losslessly and
    /// non-negative transcript positions must keep their identity encoding.
    #[test]
    fn parse_offset_field_encoding_round_trips_the_full_u64_domain() {
        for value in [0, 1, i64::MAX as u64, (i64::MAX as u64) + 1, u64::MAX] {
            assert_eq!(decode_u64_bits_value(encode_u64_bits(value)), value);
        }
        assert_eq!(
            encode_u64_bits(7),
            7,
            "non-negative values keep their stored form"
        );
        assert!(
            encode_u64_bits((i64::MAX as u64) + 1) < 0,
            "the upper half maps onto the negative INTEGER range instead of failing"
        );
    }
}
