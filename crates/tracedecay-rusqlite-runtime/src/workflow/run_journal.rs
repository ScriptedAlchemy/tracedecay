//! Durable workflow run journal and artifact payload store on the registered writer.
//!
//! The run journal is the append-only source of truth for run state: every
//! projection is rebuilt from the exact journaled events, command identity is
//! enforced once per run, and artifact payloads are digest-addressed rows that
//! are verified against their declared reference on every hydration.

use tracedecay_application::{
    WORKFLOW_ACTIVE_RECOVERY_PAGE_SIZE_V1, WorkflowActiveRunRecoveryCursorV1,
    WorkflowActiveRunRecoveryPageV1, WorkflowArtifactPayload, WorkflowArtifactPersistOutcome,
    WorkflowArtifactStoreError, WorkflowArtifactStorePort, WorkflowFanOutAttemptBindingV1,
    WorkflowRunAppendOutcome, WorkflowRunAppendRequest, WorkflowRunStorageError,
    WorkflowRunStoragePort,
};
use tracedecay_domain::{
    RunId, WorkArtifactRefV1, WorkAttemptIdentityV1, WorkAuthority, WorkflowRunEvent,
    WorkflowRunProjection, canonical_sha256,
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
            .handle()
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
            .handle()
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

    fn projections(&self) -> Result<Vec<WorkflowRunProjection>, WorkflowRunStorageError> {
        let transaction = self
            .handle()
            .begin_immediate()
            .map_err(run_journal_unavailable)?;
        let rows = query_tx(
            &transaction,
            "SELECT DISTINCT run_id FROM workflow_run_journal ORDER BY run_id",
            Vec::new(),
        )
        .map_err(run_journal_unavailable)?;
        let projections = rows
            .rows
            .iter()
            .map(|row| {
                let run_id = sql_text(&row.values, 0)
                    .ok_or(WorkflowRunStorageError::InvalidHistory)
                    .and_then(|value| {
                        RunId::new(value.to_owned())
                            .map_err(|_| WorkflowRunStorageError::InvalidHistory)
                    })?;
                history_tx(&transaction, &run_id).and_then(|history| rebuild(&history))
            })
            .collect::<Result<Vec<_>, _>>()?;
        let _ = transaction.rollback();
        Ok(projections)
    }

    fn active_projection_page(
        &self,
        authority: &WorkAuthority,
        after: Option<&WorkflowActiveRunRecoveryCursorV1>,
    ) -> Result<WorkflowActiveRunRecoveryPageV1, WorkflowRunStorageError> {
        let transaction = self
            .handle()
            .begin_immediate()
            .map_err(run_journal_unavailable)?;
        let page_limit = i64::try_from(WORKFLOW_ACTIVE_RECOVERY_PAGE_SIZE_V1 + 1)
            .map_err(|_| WorkflowRunStorageError::Unavailable)?;
        let rows = match after {
            Some(cursor) => query_tx(
                &transaction,
                "SELECT DISTINCT run_id FROM workflow_run_journal
                 WHERE run_id > ?1 ORDER BY run_id LIMIT ?2",
                vec![
                    ExactSqlValue::Text(cursor.after_run_id.as_str().to_owned()),
                    ExactSqlValue::Integer(page_limit),
                ],
            ),
            None => query_tx(
                &transaction,
                "SELECT DISTINCT run_id FROM workflow_run_journal
                 ORDER BY run_id LIMIT ?1",
                vec![ExactSqlValue::Integer(page_limit)],
            ),
        }
        .map_err(run_journal_unavailable)?;
        let run_ids = rows
            .rows
            .iter()
            .map(|row| {
                let value =
                    sql_text(&row.values, 0).ok_or(WorkflowRunStorageError::InvalidHistory)?;
                RunId::new(value.to_owned()).map_err(|_| WorkflowRunStorageError::InvalidHistory)
            })
            .collect::<Result<Vec<_>, _>>()?;
        let page_run_ids = run_ids
            .iter()
            .take(WORKFLOW_ACTIVE_RECOVERY_PAGE_SIZE_V1)
            .cloned()
            .collect::<Vec<_>>();
        let continuation = (run_ids.len() > WORKFLOW_ACTIVE_RECOVERY_PAGE_SIZE_V1).then(|| {
            WorkflowActiveRunRecoveryCursorV1 {
                after_run_id: page_run_ids[WORKFLOW_ACTIVE_RECOVERY_PAGE_SIZE_V1 - 1].clone(),
            }
        });
        let projections = page_run_ids
            .iter()
            .map(|run_id| history_tx(&transaction, run_id).and_then(|history| rebuild(&history)))
            .collect::<Result<Vec<_>, _>>()?
            .into_iter()
            .filter(|projection| {
                !projection.status().is_terminal()
                    && projection
                        .fan_out_plans()
                        .values()
                        .all(|plan| &plan.authority == authority)
            })
            .collect();
        let _ = transaction.rollback();
        Ok(WorkflowActiveRunRecoveryPageV1 {
            projections,
            continuation,
        })
    }

    fn fan_out_binding(
        &self,
        identity: &WorkAttemptIdentityV1,
    ) -> Result<Option<WorkflowFanOutAttemptBindingV1>, WorkflowRunStorageError> {
        // The attempt identity already carries its owning workflow run ID, so
        // the journal primary key provides a bounded lookup. Do not use the
        // trait's cross-run fallback scan on response-delivery paths.
        let projection = match self.projection(identity.run_id()) {
            Ok(projection) => projection,
            Err(WorkflowRunStorageError::NotFound) => return Ok(None),
            Err(error) => return Err(error),
        };
        let mut binding = None;
        for plan in projection.fan_out_plans().values() {
            if !plan
                .children
                .iter()
                .any(|child| &child.attempt_identity == identity)
            {
                continue;
            }
            let candidate = WorkflowFanOutAttemptBindingV1 {
                run_id: projection.run_id().clone(),
                step_id: plan.step_id.clone(),
                plan_digest: plan.plan_digest.clone(),
            };
            if binding
                .as_ref()
                .is_some_and(|existing| existing != &candidate)
            {
                return Err(WorkflowRunStorageError::InvalidHistory);
            }
            binding = Some(candidate);
        }
        Ok(binding)
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
            .handle()
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
            .handle()
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
