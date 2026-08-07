//! Durable workflow run journal and artifact payload store on the registered writer.
//!
//! The run journal is the append-only source of truth for run state: every
//! projection is rebuilt from the exact journaled events, command identity is
//! enforced once per run, and artifact payloads are digest-addressed rows that
//! are verified against their declared reference on every hydration.

use tracedecay_application::{
    WorkflowArtifactPayload, WorkflowArtifactPersistOutcome, WorkflowArtifactStoreError,
    WorkflowArtifactStorePort, WorkflowRunAppendOutcome, WorkflowRunAppendRequest,
    WorkflowRunStorageError, WorkflowRunStoragePort,
};
use tracedecay_domain::{
    RunId, WorkArtifactRefV1, WorkflowRunEvent, WorkflowRunProjection, canonical_sha256,
};

use super::{
    ExactSqlTransaction, ExactSqlValue, WorkflowSqliteAuthority, decode_json, encode_json,
    execute_tx, query_tx, sql_text,
};

fn run_journal_unavailable<E>(_: E) -> WorkflowRunStorageError {
    WorkflowRunStorageError::Unavailable
}

fn decode_event(
    payload: &str,
    stored_digest: &str,
) -> Result<WorkflowRunEvent, WorkflowRunStorageError> {
    let event: WorkflowRunEvent =
        decode_json(payload).map_err(|_| WorkflowRunStorageError::InvalidHistory)?;
    let digest = canonical_sha256(&event).map_err(|_| WorkflowRunStorageError::InvalidHistory)?;
    if digest.as_str() != stored_digest {
        return Err(WorkflowRunStorageError::InvalidHistory);
    }
    Ok(event)
}

fn history_tx(
    transaction: &ExactSqlTransaction,
    run_id: &RunId,
) -> Result<Vec<WorkflowRunEvent>, WorkflowRunStorageError> {
    let rows = query_tx(
        transaction,
        "SELECT event_payload, event_digest FROM workflow_run_journal
         WHERE run_id = ?1 ORDER BY sequence",
        vec![ExactSqlValue::Text(run_id.as_str().to_owned())],
    )
    .map_err(run_journal_unavailable)?;
    rows.rows
        .iter()
        .map(|row| {
            let payload =
                sql_text(&row.values, 0).ok_or(WorkflowRunStorageError::InvalidHistory)?;
            let digest = sql_text(&row.values, 1).ok_or(WorkflowRunStorageError::InvalidHistory)?;
            decode_event(payload, digest)
        })
        .collect()
}

fn rebuild(history: &[WorkflowRunEvent]) -> Result<WorkflowRunProjection, WorkflowRunStorageError> {
    WorkflowRunProjection::rebuild(history).map_err(|_| WorkflowRunStorageError::InvalidHistory)
}

impl WorkflowRunStoragePort for WorkflowSqliteAuthority {
    fn projection(&self, run_id: &RunId) -> Result<WorkflowRunProjection, WorkflowRunStorageError> {
        let transaction = self
            .storage
            .begin_immediate()
            .map_err(run_journal_unavailable)?;
        let history = history_tx(&transaction, run_id)?;
        let _ = transaction.rollback();
        if history.is_empty() {
            return Err(WorkflowRunStorageError::NotFound);
        }
        rebuild(&history)
    }

    fn append(
        &self,
        request: &WorkflowRunAppendRequest,
    ) -> Result<WorkflowRunAppendOutcome, WorkflowRunStorageError> {
        let payload =
            encode_json(&request.event).map_err(|_| WorkflowRunStorageError::Unavailable)?;
        let digest =
            canonical_sha256(&request.event).map_err(|_| WorkflowRunStorageError::Unavailable)?;
        let sequence = i64::try_from(request.event.sequence())
            .map_err(|_| WorkflowRunStorageError::Unavailable)?;
        let transaction = self
            .storage
            .begin_immediate()
            .map_err(run_journal_unavailable)?;
        let history = match history_tx(&transaction, request.event.run_id()) {
            Ok(history) => history,
            Err(error) => {
                let _ = transaction.rollback();
                return Err(error);
            }
        };
        if let Some(existing) = history
            .iter()
            .find(|event| event.command_id() == request.event.command_id())
        {
            let outcome = if existing == &request.event {
                rebuild(&history).map(WorkflowRunAppendOutcome::Replayed)
            } else {
                Err(WorkflowRunStorageError::IdempotencyConflict)
            };
            let _ = transaction.rollback();
            return outcome;
        }
        if history.last().map(WorkflowRunEvent::sequence) != request.expected_sequence {
            let _ = transaction.rollback();
            return Err(WorkflowRunStorageError::VersionConflict);
        }
        if let Err(error) = execute_tx(
            &transaction,
            "INSERT INTO workflow_run_journal (
                 run_id, sequence, command_id, event_payload, event_digest
             ) VALUES (?1, ?2, ?3, ?4, ?5)",
            vec![
                ExactSqlValue::Text(request.event.run_id().as_str().to_owned()),
                ExactSqlValue::Integer(sequence),
                ExactSqlValue::Text(request.event.command_id().as_str().to_owned()),
                ExactSqlValue::Text(payload),
                ExactSqlValue::Text(digest.as_str().to_owned()),
            ],
        ) {
            let _ = transaction.rollback();
            return Err(run_journal_unavailable(error));
        }
        let mut appended = history;
        appended.push(request.event.clone());
        // Rebuild before commit: an event that does not extend a valid
        // history must never become durable.
        let projection = match rebuild(&appended) {
            Ok(projection) => projection,
            Err(error) => {
                let _ = transaction.rollback();
                return Err(error);
            }
        };
        transaction
            .commit()
            .map(|_| WorkflowRunAppendOutcome::Appended(projection))
            .map_err(run_journal_unavailable)
    }
}

fn artifact_store_unavailable<E>(_: E) -> WorkflowArtifactStoreError {
    WorkflowArtifactStoreError::Unavailable
}

fn stored_payload_tx(
    transaction: &ExactSqlTransaction,
    digest: &str,
) -> Result<Option<Vec<u8>>, WorkflowArtifactStoreError> {
    let rows = query_tx(
        transaction,
        "SELECT payload FROM workflow_artifact_payloads WHERE payload_digest = ?1",
        vec![ExactSqlValue::Text(digest.to_owned())],
    )
    .map_err(artifact_store_unavailable)?;
    match rows.rows.first() {
        None => Ok(None),
        Some(row) => match row.values.first() {
            Some(ExactSqlValue::Blob(bytes)) => Ok(Some(bytes.clone())),
            _ => Err(WorkflowArtifactStoreError::Unavailable),
        },
    }
}

impl WorkflowArtifactStorePort for WorkflowSqliteAuthority {
    fn persist(
        &self,
        payload: &WorkflowArtifactPayload,
    ) -> Result<WorkflowArtifactPersistOutcome, WorkflowArtifactStoreError> {
        let digest = payload.artifact().digest().as_str();
        let byte_length = i64::try_from(payload.artifact().byte_length())
            .map_err(|_| WorkflowArtifactStoreError::Oversized)?;
        let transaction = self
            .storage
            .begin_immediate()
            .map_err(artifact_store_unavailable)?;
        let existing = match stored_payload_tx(&transaction, digest) {
            Ok(existing) => existing,
            Err(error) => {
                let _ = transaction.rollback();
                return Err(error);
            }
        };
        if let Some(stored) = existing {
            let _ = transaction.rollback();
            return if stored.as_slice() == payload.bytes() {
                Ok(WorkflowArtifactPersistOutcome::Replayed)
            } else {
                Err(WorkflowArtifactStoreError::PayloadConflict)
            };
        }
        if let Err(error) = execute_tx(
            &transaction,
            "INSERT INTO workflow_artifact_payloads (
                 payload_digest, byte_length, payload
             ) VALUES (?1, ?2, ?3)",
            vec![
                ExactSqlValue::Text(digest.to_owned()),
                ExactSqlValue::Integer(byte_length),
                ExactSqlValue::Blob(payload.bytes().to_vec()),
            ],
        ) {
            let _ = transaction.rollback();
            return Err(artifact_store_unavailable(error));
        }
        transaction
            .commit()
            .map(|_| WorkflowArtifactPersistOutcome::Persisted)
            .map_err(artifact_store_unavailable)
    }

    fn load(
        &self,
        artifact: &WorkArtifactRefV1,
    ) -> Result<WorkflowArtifactPayload, WorkflowArtifactStoreError> {
        let transaction = self
            .storage
            .begin_immediate()
            .map_err(artifact_store_unavailable)?;
        let stored = stored_payload_tx(&transaction, artifact.digest().as_str());
        let _ = transaction.rollback();
        let Some(bytes) = stored? else {
            return Err(WorkflowArtifactStoreError::Missing);
        };
        // Construction re-verifies byte length and content digest, so a
        // corrupted or foreign row can never re-enter execution.
        WorkflowArtifactPayload::new(artifact.clone(), bytes)
    }
}
