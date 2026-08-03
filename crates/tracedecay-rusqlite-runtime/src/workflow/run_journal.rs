use std::time::Duration;

use tracedecay_application::{
    WorkflowRunAppendOutcome, WorkflowRunAppendRequest, WorkflowRunStorageError,
    WorkflowRunStoragePort,
};
use tracedecay_domain::{ManifestDigest, RunId, WorkflowRunProjection, canonical_sha256};

use crate::migration_sql::{
    MigrationSqlError, MigrationSqlRows, MigrationSqlStatement, MigrationSqlTransaction,
    MigrationSqlValue,
};

use super::{WorkflowSqliteAuthority, sql_integer, sql_text};

const READ_TIMEOUT: Duration = Duration::from_secs(5);

fn unavailable(_: MigrationSqlError) -> WorkflowRunStorageError {
    WorkflowRunStorageError::Unavailable
}

fn statement(
    sql: &str,
    params: Vec<MigrationSqlValue>,
) -> Result<MigrationSqlStatement, WorkflowRunStorageError> {
    MigrationSqlStatement::new(sql.to_owned(), params).map_err(unavailable)
}

fn query(
    transaction: &MigrationSqlTransaction,
    sql: &str,
    params: Vec<MigrationSqlValue>,
) -> Result<MigrationSqlRows, WorkflowRunStorageError> {
    transaction
        .query(statement(sql, params)?)
        .map_err(unavailable)
}

fn encode<T: serde::Serialize>(value: &T) -> Result<String, WorkflowRunStorageError> {
    serde_json::to_string(value).map_err(|_| WorkflowRunStorageError::InvalidHistory)
}

fn decode<T: serde::de::DeserializeOwned>(value: &str) -> Result<T, WorkflowRunStorageError> {
    serde_json::from_str(value).map_err(|_| WorkflowRunStorageError::InvalidHistory)
}

fn digest<T: serde::Serialize>(value: &T) -> Result<ManifestDigest, WorkflowRunStorageError> {
    canonical_sha256(value).map_err(|_| WorkflowRunStorageError::InvalidHistory)
}

fn row_projection(
    rows: &MigrationSqlRows,
    payload_index: usize,
) -> Result<WorkflowRunProjection, WorkflowRunStorageError> {
    let row = rows.rows.first().ok_or(WorkflowRunStorageError::NotFound)?;
    decode(sql_text(&row.values, payload_index).ok_or(WorkflowRunStorageError::InvalidHistory)?)
}

impl WorkflowRunStoragePort for WorkflowSqliteAuthority {
    fn projection(
        &self,
        run_id: &RunId,
    ) -> Result<WorkflowRunProjection, WorkflowRunStorageError> {
        let rows = self
            .storage
            .handle
            .query(
                statement(
                    "SELECT projection_payload
                     FROM workflow_run_heads
                     WHERE run_id = ?1",
                    vec![MigrationSqlValue::Text(run_id.as_str().to_owned())],
                )?,
                READ_TIMEOUT,
            )
            .map_err(unavailable)?;
        row_projection(&rows, 0)
    }

    fn append(
        &self,
        request: &WorkflowRunAppendRequest,
    ) -> Result<WorkflowRunAppendOutcome, WorkflowRunStorageError> {
        let transaction = self.storage.handle.begin_immediate().map_err(unavailable)?;
        let event_payload = encode(&request.event)?;
        let event_digest = digest(&request.event)?;
        let replay = query(
            &transaction,
            "SELECT input_digest, event_digest
             FROM workflow_run_events
             WHERE run_id = ?1 AND command_id = ?2",
            vec![
                MigrationSqlValue::Text(request.event.run_id().as_str().to_owned()),
                MigrationSqlValue::Text(request.event.command_id().as_str().to_owned()),
            ],
        )?;
        if let Some(row) = replay.rows.first() {
            let stored_input =
                sql_text(&row.values, 0).ok_or(WorkflowRunStorageError::InvalidHistory)?;
            let stored_event =
                sql_text(&row.values, 1).ok_or(WorkflowRunStorageError::InvalidHistory)?;
            if stored_input != request.event.input_digest().as_str()
                || stored_event != event_digest.as_str()
            {
                let _ = transaction.rollback();
                return Err(WorkflowRunStorageError::IdempotencyConflict);
            }
            let head = query(
                &transaction,
                "SELECT projection_payload
                 FROM workflow_run_heads
                 WHERE run_id = ?1",
                vec![MigrationSqlValue::Text(
                    request.event.run_id().as_str().to_owned(),
                )],
            )?;
            let projection = row_projection(&head, 0)?;
            let _ = transaction.rollback();
            return Ok(WorkflowRunAppendOutcome::Replayed(projection));
        }

        let head = query(
            &transaction,
            "SELECT sequence, projection_payload
             FROM workflow_run_heads
             WHERE run_id = ?1",
            vec![MigrationSqlValue::Text(
                request.event.run_id().as_str().to_owned(),
            )],
        )?;
        let projection = match head.rows.first() {
            Some(row) => {
                let sequence = sql_integer(&row.values, 0)
                    .and_then(|value| u64::try_from(value).ok())
                    .ok_or(WorkflowRunStorageError::InvalidHistory)?;
                if request.expected_sequence != Some(sequence)
                    || request.event.sequence() != sequence.saturating_add(1)
                {
                    let _ = transaction.rollback();
                    return Err(WorkflowRunStorageError::VersionConflict);
                }
                let current: WorkflowRunProjection = decode(
                    sql_text(&row.values, 1).ok_or(WorkflowRunStorageError::InvalidHistory)?,
                )?;
                current
                    .apply(&request.event)
                    .map_err(|_| WorkflowRunStorageError::InvalidHistory)?
            }
            None => {
                if request.expected_sequence.is_some() || request.event.sequence() != 1 {
                    let _ = transaction.rollback();
                    return Err(WorkflowRunStorageError::VersionConflict);
                }
                WorkflowRunProjection::rebuild(std::slice::from_ref(&request.event))
                    .map_err(|_| WorkflowRunStorageError::InvalidHistory)?
            }
        };
        let projection_payload = encode(&projection)?;
        let projection_digest = digest(&projection)?;
        transaction
            .execute(statement(
                "INSERT INTO workflow_run_events (
                     run_id, sequence, command_id, input_digest, occurred_at,
                     event_payload, event_digest
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                vec![
                    MigrationSqlValue::Text(request.event.run_id().as_str().to_owned()),
                    MigrationSqlValue::Integer(
                        i64::try_from(request.event.sequence())
                            .map_err(|_| WorkflowRunStorageError::InvalidHistory)?,
                    ),
                    MigrationSqlValue::Text(request.event.command_id().as_str().to_owned()),
                    MigrationSqlValue::Text(request.event.input_digest().as_str().to_owned()),
                    MigrationSqlValue::Integer(request.event.occurred_at().0),
                    MigrationSqlValue::Text(event_payload),
                    MigrationSqlValue::Text(event_digest.as_str().to_owned()),
                ],
            )?)
            .map_err(unavailable)?;
        transaction
            .execute(statement(
                "INSERT INTO workflow_run_heads (
                     run_id, sequence, projection_payload, projection_digest, last_event_digest
                 ) VALUES (?1, ?2, ?3, ?4, ?5)
                 ON CONFLICT(run_id) DO UPDATE SET
                     sequence = excluded.sequence,
                     projection_payload = excluded.projection_payload,
                     projection_digest = excluded.projection_digest,
                     last_event_digest = excluded.last_event_digest",
                vec![
                    MigrationSqlValue::Text(request.event.run_id().as_str().to_owned()),
                    MigrationSqlValue::Integer(
                        i64::try_from(projection.sequence())
                            .map_err(|_| WorkflowRunStorageError::InvalidHistory)?,
                    ),
                    MigrationSqlValue::Text(projection_payload),
                    MigrationSqlValue::Text(projection_digest.as_str().to_owned()),
                    MigrationSqlValue::Text(event_digest.as_str().to_owned()),
                ],
            )?)
            .map_err(unavailable)?;
        transaction.commit().map_err(unavailable)?;
        Ok(WorkflowRunAppendOutcome::Appended(projection))
    }
}
