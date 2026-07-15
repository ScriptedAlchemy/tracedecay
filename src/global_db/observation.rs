use libsql::{Connection, params};
use tracedecay_domain::{
    CanonicalObservationIdV1, ClaudeSourceCursorV1, ClaudeSourceIdentityV1,
    ObservationCollisionOutcomeV1, ObservationScopeV1, classify_observation_collision,
};
use tracedecay_store::{
    ObservationCommitReceipt, ObservationPersistOutcome, ObservationProjectionStatus,
    ObservationReplayRequest, ObservationStoreError, ObservationStoreResult, ObservationWrite,
    StoredObservation,
};

use super::GlobalDb;

pub(super) async fn ensure_observation_schema(conn: &Connection) -> Result<(), libsql::Error> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS sanitization_receipts (
            receipt_id TEXT PRIMARY KEY,
            sanitizer_version TEXT NOT NULL,
            payload_digest TEXT NOT NULL,
            receipt_json TEXT NOT NULL
        );
        CREATE TABLE IF NOT EXISTS observations (
            sequence INTEGER PRIMARY KEY AUTOINCREMENT,
            observation_id TEXT NOT NULL UNIQUE,
            idempotency_key TEXT NOT NULL UNIQUE,
            payload_digest TEXT NOT NULL,
            receipt_id TEXT NOT NULL,
            observation_json TEXT NOT NULL,
            committed_cursor_json TEXT NOT NULL,
            FOREIGN KEY(receipt_id) REFERENCES sanitization_receipts(receipt_id)
        );
        CREATE TABLE IF NOT EXISTS source_cursors (
            source_json TEXT NOT NULL,
            scope_json TEXT NOT NULL,
            cursor_json TEXT NOT NULL,
            PRIMARY KEY(source_json, scope_json)
        );
        CREATE TABLE IF NOT EXISTS projection_queue (
            observation_id TEXT PRIMARY KEY,
            observation_sequence INTEGER NOT NULL UNIQUE,
            FOREIGN KEY(observation_id) REFERENCES observations(observation_id)
        );
        CREATE TRIGGER IF NOT EXISTS observations_immutable_update
            BEFORE UPDATE ON observations BEGIN
                SELECT RAISE(ABORT, 'observations are immutable');
            END;
        CREATE TRIGGER IF NOT EXISTS observations_immutable_delete
            BEFORE DELETE ON observations BEGIN
                SELECT RAISE(ABORT, 'observations are immutable');
            END;",
    )
    .await
    .map(|_| ())
}

fn storage(
    operation: &'static str,
    source: impl std::error::Error + Send + Sync + 'static,
) -> ObservationStoreError {
    ObservationStoreError::Storage {
        operation,
        source: Box::new(source),
    }
}

fn storage_message(operation: &'static str, message: impl Into<String>) -> ObservationStoreError {
    storage(operation, std::io::Error::other(message.into()))
}

fn encode<T: serde::Serialize>(
    value: &T,
    operation: &'static str,
) -> ObservationStoreResult<String> {
    serde_json::to_string(value).map_err(|error| storage(operation, error))
}

fn decode<T: serde::de::DeserializeOwned>(
    value: &str,
    operation: &'static str,
) -> ObservationStoreResult<T> {
    serde_json::from_str(value).map_err(|error| storage(operation, error))
}

fn decode_sequence(value: i64, operation: &'static str) -> ObservationStoreResult<u64> {
    u64::try_from(value).map_err(|_| storage_message(operation, "negative observation sequence"))
}

async fn read_observation_row(
    conn: &Connection,
    sql: &'static str,
    value: &str,
    operation: &'static str,
) -> ObservationStoreResult<Option<(ObservationCommitReceipt, String)>> {
    let mut rows = conn
        .query(sql, params![value])
        .await
        .map_err(|error| storage(operation, error))?;
    let Some(row) = rows
        .next()
        .await
        .map_err(|error| storage(operation, error))?
    else {
        return Ok(None);
    };
    let sequence = decode_sequence(
        row.get::<i64>(0)
            .map_err(|error| storage(operation, error))?,
        operation,
    )?;
    let observation_json = row
        .get::<String>(1)
        .map_err(|error| storage(operation, error))?;
    let cursor_json = row
        .get::<String>(2)
        .map_err(|error| storage(operation, error))?;
    let idempotency_key = row
        .get::<String>(3)
        .map_err(|error| storage(operation, error))?;
    Ok(Some((
        ObservationCommitReceipt::new(
            sequence,
            decode(&observation_json, operation)?,
            decode(&cursor_json, operation)?,
        ),
        idempotency_key,
    )))
}

async fn read_by_observation_id(
    conn: &Connection,
    observation_id: &CanonicalObservationIdV1,
) -> ObservationStoreResult<Option<(ObservationCommitReceipt, String)>> {
    read_observation_row(
        conn,
        "SELECT sequence, observation_json, committed_cursor_json, idempotency_key
         FROM observations WHERE observation_id = ?1",
        observation_id.as_str(),
        "read observation",
    )
    .await
}

async fn read_by_idempotency_key(
    conn: &Connection,
    idempotency_key: &str,
) -> ObservationStoreResult<Option<(ObservationCommitReceipt, String)>> {
    read_observation_row(
        conn,
        "SELECT sequence, observation_json, committed_cursor_json, idempotency_key
         FROM observations WHERE idempotency_key = ?1",
        idempotency_key,
        "read observation by idempotency key",
    )
    .await
}

async fn read_cursor(
    conn: &Connection,
    source_json: &str,
    scope_json: &str,
) -> ObservationStoreResult<Option<ClaudeSourceCursorV1>> {
    let mut rows = conn
        .query(
            "SELECT cursor_json FROM source_cursors
             WHERE source_json = ?1 AND scope_json = ?2",
            params![source_json, scope_json],
        )
        .await
        .map_err(|error| storage("read observation source cursor", error))?;
    let Some(row) = rows
        .next()
        .await
        .map_err(|error| storage("read observation source cursor", error))?
    else {
        return Ok(None);
    };
    let cursor_json = row
        .get::<String>(0)
        .map_err(|error| storage("read observation source cursor", error))?;
    decode(&cursor_json, "decode observation source cursor").map(Some)
}

async fn read_projection_status(
    conn: &Connection,
    observation_id: &CanonicalObservationIdV1,
) -> ObservationStoreResult<ObservationProjectionStatus> {
    let mut rows = conn
        .query(
            "SELECT EXISTS(
                SELECT 1 FROM projection_queue WHERE observation_id = ?1
             )",
            params![observation_id.as_str()],
        )
        .await
        .map_err(|error| storage("read observation projection status", error))?;
    let row = rows
        .next()
        .await
        .map_err(|error| storage("read observation projection status", error))?
        .ok_or_else(|| {
            storage_message(
                "read observation projection status",
                "projection status query returned no row",
            )
        })?;
    match row
        .get::<i64>(0)
        .map_err(|error| storage("read observation projection status", error))?
    {
        0 => Ok(ObservationProjectionStatus::NotQueued),
        _ => Ok(ObservationProjectionStatus::Queued),
    }
}

impl GlobalDb {
    pub(crate) async fn persist_observation_result(
        &self,
        write: ObservationWrite,
    ) -> ObservationStoreResult<ObservationPersistOutcome> {
        let _writer = self.transaction.lock().await;
        let transaction = self
            .begin_authoritative_transaction()
            .await
            .map_err(|error| storage("begin observation transaction", error))?;

        let candidate = write.observation();
        if let Some((existing, _)) =
            read_by_observation_id(&transaction, candidate.observation_id()).await?
        {
            let existing_observation = existing.observation();
            let outcome = classify_observation_collision(existing_observation, candidate);
            return match outcome {
                ObservationCollisionOutcomeV1::ExactDuplicate
                    if existing_observation.receipt() == candidate.receipt() =>
                {
                    Ok(ObservationPersistOutcome::ExactDuplicate(existing))
                }
                ObservationCollisionOutcomeV1::ExactDuplicate => {
                    Err(ObservationStoreError::SanitizationReceiptCollision)
                }
                ObservationCollisionOutcomeV1::IdentityCollision => {
                    Err(ObservationStoreError::ObservationCollision {
                        observation_id: candidate.observation_id().clone(),
                        existing_digest: existing_observation.payload_reference().digest().clone(),
                        candidate_digest: candidate.payload_reference().digest().clone(),
                        outcome,
                    })
                }
                ObservationCollisionOutcomeV1::Distinct => Err(storage_message(
                    "classify observation collision",
                    "matching observation identifier classified as distinct",
                )),
            };
        }
        if read_by_idempotency_key(&transaction, candidate.idempotency_key().as_str())
            .await?
            .is_some()
        {
            return Err(ObservationStoreError::IdempotencyCollision);
        }

        let source_json = encode(candidate.source(), "encode observation source")?;
        let scope_json = encode(candidate.scope(), "encode observation scope")?;
        let actual_cursor = read_cursor(&transaction, &source_json, &scope_json).await?;
        if actual_cursor.as_ref() != write.expected_cursor() {
            return Err(ObservationStoreError::CursorConflict {
                expected: write.expected_cursor().cloned(),
                actual: actual_cursor,
            });
        }

        let observation_json = encode(candidate, "encode observation")?;
        let cursor_json = encode(write.next_cursor(), "encode committed observation cursor")?;
        let receipt = candidate.receipt();
        let receipt_json = encode(receipt, "encode sanitization receipt")?;
        let receipt_id = receipt.receipt().receipt_id().as_str();
        let sanitizer_version = receipt.receipt().sanitizer_version().as_str();
        let payload_digest = candidate.payload_reference().digest().as_str();

        transaction
            .execute(
                "INSERT INTO sanitization_receipts
                    (receipt_id, sanitizer_version, payload_digest, receipt_json)
                 VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT(receipt_id) DO NOTHING",
                params![
                    receipt_id,
                    sanitizer_version,
                    payload_digest,
                    receipt_json.as_str()
                ],
            )
            .await
            .map_err(|error| storage("insert sanitization receipt", error))?;
        let mut receipt_rows = transaction
            .query(
                "SELECT receipt_json FROM sanitization_receipts WHERE receipt_id = ?1",
                params![receipt_id],
            )
            .await
            .map_err(|error| storage("verify sanitization receipt", error))?;
        let stored_receipt_json = receipt_rows
            .next()
            .await
            .map_err(|error| storage("verify sanitization receipt", error))?
            .ok_or_else(|| {
                storage_message("verify sanitization receipt", "receipt insert disappeared")
            })?
            .get::<String>(0)
            .map_err(|error| storage("verify sanitization receipt", error))?;
        drop(receipt_rows);
        if stored_receipt_json != receipt_json {
            return Err(ObservationStoreError::SanitizationReceiptCollision);
        }

        transaction
            .execute(
                "INSERT INTO observations
                    (observation_id, idempotency_key, payload_digest, receipt_id,
                     observation_json, committed_cursor_json)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    candidate.observation_id().as_str(),
                    candidate.idempotency_key().as_str(),
                    payload_digest,
                    receipt_id,
                    observation_json.as_str(),
                    cursor_json.as_str()
                ],
            )
            .await
            .map_err(|error| storage("insert immutable observation", error))?;
        let (committed, _) = read_by_observation_id(&transaction, candidate.observation_id())
            .await?
            .ok_or_else(|| {
                storage_message(
                    "read committed observation",
                    "observation insert disappeared",
                )
            })?;

        transaction
            .execute(
                "INSERT INTO source_cursors (source_json, scope_json, cursor_json)
                 VALUES (?1, ?2, ?3)
                 ON CONFLICT(source_json, scope_json) DO UPDATE SET
                    cursor_json = excluded.cursor_json",
                params![
                    source_json.as_str(),
                    scope_json.as_str(),
                    cursor_json.as_str()
                ],
            )
            .await
            .map_err(|error| storage("advance observation source cursor", error))?;
        transaction
            .execute(
                "INSERT INTO projection_queue (observation_id, observation_sequence)
                 VALUES (?1, ?2)",
                params![
                    candidate.observation_id().as_str(),
                    i64::try_from(committed.sequence()).map_err(|_| storage_message(
                        "enqueue observation projection",
                        "observation sequence exceeds SQLite integer range"
                    ))?
                ],
            )
            .await
            .map_err(|error| storage("enqueue observation projection", error))?;

        transaction
            .commit()
            .await
            .map_err(|error| storage("commit observation transaction", error))?;
        Ok(ObservationPersistOutcome::Committed(committed))
    }

    pub(crate) async fn get_observation_source_cursor_result(
        &self,
        source: &ClaudeSourceIdentityV1,
        scope: &ObservationScopeV1,
    ) -> ObservationStoreResult<Option<ClaudeSourceCursorV1>> {
        let _reader = self.transaction.lock().await;
        let source_json = encode(source, "encode observation source")?;
        let scope_json = encode(scope, "encode observation scope")?;
        read_cursor(&self.conn, &source_json, &scope_json).await
    }

    pub(crate) async fn get_observation_result(
        &self,
        observation_id: &CanonicalObservationIdV1,
    ) -> ObservationStoreResult<Option<StoredObservation>> {
        let _reader = self.transaction.lock().await;
        let Some((receipt, _)) = read_by_observation_id(&self.conn, observation_id).await? else {
            return Ok(None);
        };
        let projection_status = read_projection_status(&self.conn, observation_id).await?;
        Ok(Some(StoredObservation::new(
            receipt.sequence(),
            receipt.observation().clone(),
            receipt.committed_cursor().clone(),
            projection_status,
        )))
    }

    pub(crate) async fn replay_observations_result(
        &self,
        request: ObservationReplayRequest,
    ) -> ObservationStoreResult<Vec<StoredObservation>> {
        let _reader = self.transaction.lock().await;
        let after_sequence = i64::try_from(request.after_sequence()).map_err(|_| {
            storage_message(
                "replay observations",
                "observation replay sequence exceeds SQLite integer range",
            )
        })?;
        let limit = i64::try_from(request.limit()).map_err(|_| {
            storage_message(
                "replay observations",
                "observation replay limit exceeds SQLite integer range",
            )
        })?;
        let mut rows = self
            .conn
            .query(
                "SELECT observations.sequence, observations.observation_json,
                        observations.committed_cursor_json,
                        EXISTS(
                            SELECT 1 FROM projection_queue
                            WHERE projection_queue.observation_id = observations.observation_id
                        )
                 FROM observations
                 WHERE sequence > ?1 ORDER BY sequence ASC LIMIT ?2",
                params![after_sequence, limit],
            )
            .await
            .map_err(|error| storage("replay observations", error))?;
        let mut observations = Vec::with_capacity(request.limit());
        while let Some(row) = rows
            .next()
            .await
            .map_err(|error| storage("replay observations", error))?
        {
            let sequence = decode_sequence(
                row.get::<i64>(0)
                    .map_err(|error| storage("replay observations", error))?,
                "replay observations",
            )?;
            let observation_json = row
                .get::<String>(1)
                .map_err(|error| storage("replay observations", error))?;
            let committed_cursor_json = row
                .get::<String>(2)
                .map_err(|error| storage("replay observations", error))?;
            let projection_status = match row
                .get::<i64>(3)
                .map_err(|error| storage("replay observations", error))?
            {
                0 => ObservationProjectionStatus::NotQueued,
                _ => ObservationProjectionStatus::Queued,
            };
            observations.push(StoredObservation::new(
                sequence,
                decode(&observation_json, "decode replayed observation")?,
                decode(&committed_cursor_json, "decode replayed observation cursor")?,
                projection_status,
            ));
        }
        Ok(observations)
    }
}
