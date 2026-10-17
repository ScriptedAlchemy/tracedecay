//! Durable workflow run journal and artifact payload store on the registered writer.
//!
//! The run journal is the append-only source of truth for run state: every
//! projection is rebuilt from the exact journaled events, command identity is
//! enforced once per run, and artifact payloads are digest-addressed rows that
//! are verified against their declared reference on every hydration.

use tracedecay_contracts::{
    WORKFLOW_ACTIVE_RECOVERY_PAGE_SIZE_V1, WorkflowActiveRunRecoveryCursorV1,
    WorkflowActiveRunRecoveryPageV1, WorkflowArtifactPayload, WorkflowArtifactPersistOutcome,
    WorkflowArtifactStoreError, WorkflowArtifactStorePort, WorkflowFanOutAttemptBindingV1,
    WorkflowRunAppendOutcome, WorkflowRunAppendRequest, WorkflowRunReadPort,
    WorkflowRunStorageError, WorkflowRunStoragePort,
};
use tracedecay_domain::{
    RunId, WorkArtifactRefV1, WorkAttemptIdentityV1, WorkAuthority, WorkflowRunEvent,
    WorkflowRunProjection, canonical_sha256,
};

use super::{ExactSqlValue, WorkflowSqliteAuthority, encode_json, execute_tx, query_tx, sql_text};
use crate::exact_sql::ExactSqlTransaction;
use crate::work::{RegisteredWorkQuery, registered_work_query};
use crate::work_run_control::WorkSqliteJobStorage;

fn run_journal_unavailable<E>(_: E) -> WorkflowRunStorageError {
    WorkflowRunStorageError::Unavailable
}

fn decode_event(
    payload: &str,
    stored_digest: &str,
) -> Result<WorkflowRunEvent, WorkflowRunStorageError> {
    tracedecay_domain::decode_with_canonical_digest(payload, stored_digest)
        .ok_or(WorkflowRunStorageError::InvalidHistory)
}

fn history_tx(
    source: &impl RegisteredWorkQuery,
    run_id: &RunId,
) -> Result<Vec<WorkflowRunEvent>, WorkflowRunStorageError> {
    let rows = registered_work_query(
        source,
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

/// Replays one run's journal from a single statement, so the projection is a
/// consistent snapshot whatever `source` is.
pub(crate) fn projection_from(
    source: &impl RegisteredWorkQuery,
    run_id: &RunId,
) -> Result<WorkflowRunProjection, WorkflowRunStorageError> {
    let history = history_tx(source, run_id)?;
    if history.is_empty() {
        return Err(WorkflowRunStorageError::NotFound);
    }
    rebuild(&history)
}

pub(crate) fn append_in_transaction(
    transaction: &impl crate::work::RegisteredWorkWrite,
    request: &WorkflowRunAppendRequest,
) -> Result<WorkflowRunAppendOutcome, WorkflowRunStorageError> {
    let payload = encode_json(&request.event).map_err(|_| WorkflowRunStorageError::Unavailable)?;
    let digest =
        canonical_sha256(&request.event).map_err(|_| WorkflowRunStorageError::Unavailable)?;
    let sequence = i64::try_from(request.event.sequence())
        .map_err(|_| WorkflowRunStorageError::Unavailable)?;
    let history = history_tx(transaction, request.event.run_id())?;
    if let Some(existing) = history
        .iter()
        .find(|event| event.command_id() == request.event.command_id())
    {
        return if existing == &request.event {
            rebuild(&history).map(WorkflowRunAppendOutcome::Replayed)
        } else {
            Err(WorkflowRunStorageError::IdempotencyConflict)
        };
    }
    if history.last().map(WorkflowRunEvent::sequence) != request.expected_sequence {
        return Err(WorkflowRunStorageError::VersionConflict);
    }
    execute_tx(
        transaction,
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
    )
    .map_err(run_journal_unavailable)?;
    let mut appended = history;
    appended.push(request.event.clone());
    rebuild(&appended).map(WorkflowRunAppendOutcome::Appended)
}

fn deferred<R>(
    authority: &WorkflowSqliteAuthority,
    read: impl FnOnce(&ExactSqlTransaction) -> Result<R, WorkflowRunStorageError>,
) -> Result<R, WorkflowRunStorageError> {
    let transaction = authority
        .handle()
        .begin_deferred()
        .map_err(run_journal_unavailable)?;
    let result = read(&transaction);
    let _ = transaction.rollback();
    result
}

impl WorkflowRunReadPort for WorkflowSqliteAuthority {
    fn projection(&self, run_id: &RunId) -> Result<WorkflowRunProjection, WorkflowRunStorageError> {
        deferred(self, |transaction| projection_from(transaction, run_id))
    }

    #[tracing::instrument(name = "rusqlite.workflow.projections_scan", level = "trace", skip_all)]
    fn projections(&self) -> Result<Vec<WorkflowRunProjection>, WorkflowRunStorageError> {
        deferred(self, projections_from)
    }

    #[tracing::instrument(name = "rusqlite.workflow.recovery_page", level = "trace", skip_all)]
    fn active_projection_page(
        &self,
        authority: &WorkAuthority,
        after: Option<&WorkflowActiveRunRecoveryCursorV1>,
    ) -> Result<WorkflowActiveRunRecoveryPageV1, WorkflowRunStorageError> {
        deferred(self, |transaction| {
            active_projection_page_from(transaction, authority, after)
        })
    }

    #[tracing::instrument(name = "rusqlite.workflow.fan_out_binding", level = "trace", skip_all)]
    fn fan_out_binding(
        &self,
        identity: &WorkAttemptIdentityV1,
    ) -> Result<Option<WorkflowFanOutAttemptBindingV1>, WorkflowRunStorageError> {
        deferred(self, |transaction| {
            fan_out_binding_from(transaction, identity)
        })
    }
}

impl WorkflowRunReadPort for WorkSqliteJobStorage<'_> {
    fn projection(&self, run_id: &RunId) -> Result<WorkflowRunProjection, WorkflowRunStorageError> {
        projection_from(self.transaction, run_id)
    }

    fn projections(&self) -> Result<Vec<WorkflowRunProjection>, WorkflowRunStorageError> {
        projections_from(self.transaction)
    }

    fn active_projection_page(
        &self,
        authority: &WorkAuthority,
        after: Option<&WorkflowActiveRunRecoveryCursorV1>,
    ) -> Result<WorkflowActiveRunRecoveryPageV1, WorkflowRunStorageError> {
        active_projection_page_from(self.transaction, authority, after)
    }

    fn fan_out_binding(
        &self,
        identity: &WorkAttemptIdentityV1,
    ) -> Result<Option<WorkflowFanOutAttemptBindingV1>, WorkflowRunStorageError> {
        fan_out_binding_from(self.transaction, identity)
    }
}

impl WorkflowRunStoragePort for WorkSqliteJobStorage<'_> {
    fn append(
        &self,
        request: &WorkflowRunAppendRequest,
    ) -> Result<WorkflowRunAppendOutcome, WorkflowRunStorageError> {
        append_in_transaction(self.transaction, request)
    }
}

fn run_ids(rows: &crate::exact_sql::ExactSqlRows) -> Result<Vec<RunId>, WorkflowRunStorageError> {
    rows.rows
        .iter()
        .map(|row| {
            let value = sql_text(&row.values, 0).ok_or(WorkflowRunStorageError::InvalidHistory)?;
            RunId::new(value.to_owned()).map_err(|_| WorkflowRunStorageError::InvalidHistory)
        })
        .collect()
}

fn projections_from(
    source: &impl RegisteredWorkQuery,
) -> Result<Vec<WorkflowRunProjection>, WorkflowRunStorageError> {
    let rows = query_tx(
        source,
        "SELECT DISTINCT run_id FROM workflow_run_journal ORDER BY run_id",
        Vec::new(),
    )
    .map_err(run_journal_unavailable)?;
    run_ids(&rows)?
        .iter()
        .map(|run_id| history_tx(source, run_id).and_then(|history| rebuild(&history)))
        .collect()
}

fn active_projection_page_from(
    source: &impl RegisteredWorkQuery,
    authority: &WorkAuthority,
    after: Option<&WorkflowActiveRunRecoveryCursorV1>,
) -> Result<WorkflowActiveRunRecoveryPageV1, WorkflowRunStorageError> {
    let page_limit = i64::try_from(WORKFLOW_ACTIVE_RECOVERY_PAGE_SIZE_V1 + 1)
        .map_err(|_| WorkflowRunStorageError::Unavailable)?;
    let rows = match after {
        Some(cursor) => query_tx(
            source,
            "SELECT DISTINCT run_id FROM workflow_run_journal
             WHERE run_id > ?1 ORDER BY run_id LIMIT ?2",
            vec![
                ExactSqlValue::Text(cursor.after_run_id.as_str().to_owned()),
                ExactSqlValue::Integer(page_limit),
            ],
        ),
        None => query_tx(
            source,
            "SELECT DISTINCT run_id FROM workflow_run_journal
             ORDER BY run_id LIMIT ?1",
            vec![ExactSqlValue::Integer(page_limit)],
        ),
    }
    .map_err(run_journal_unavailable)?;
    let run_ids = run_ids(&rows)?;
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
        .map(|run_id| history_tx(source, run_id).and_then(|history| rebuild(&history)))
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
    Ok(WorkflowActiveRunRecoveryPageV1 {
        projections,
        continuation,
    })
}

fn fan_out_binding_from(
    source: &impl RegisteredWorkQuery,
    identity: &WorkAttemptIdentityV1,
) -> Result<Option<WorkflowFanOutAttemptBindingV1>, WorkflowRunStorageError> {
    let projection = match projection_from(source, identity.run_id()) {
        Ok(projection) => projection,
        Err(WorkflowRunStorageError::NotFound) => return Ok(None),
        Err(error) => return Err(error),
    };
    let mut binding = None;
    for plan in projection.fan_out_plans().values() {
        if !plan.children.iter().any(|child| {
            projection.planned_fan_out_attempt(identity) == Some(&child.attempt_identity)
        }) {
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

fn artifact_store_unavailable<E>(_: E) -> WorkflowArtifactStoreError {
    WorkflowArtifactStoreError::Unavailable
}

fn stored_payload_tx(
    transaction: &impl RegisteredWorkQuery,
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

impl WorkflowArtifactStorePort for WorkSqliteJobStorage<'_> {
    fn persist(
        &self,
        payload: &WorkflowArtifactPayload,
    ) -> Result<WorkflowArtifactPersistOutcome, WorkflowArtifactStoreError> {
        let digest = payload.artifact().digest().as_str();
        let byte_length = i64::try_from(payload.artifact().byte_length())
            .map_err(|_| WorkflowArtifactStoreError::Oversized)?;
        if let Some(stored) = stored_payload_tx(self.transaction, digest)? {
            return if stored.as_slice() == payload.bytes() {
                Ok(WorkflowArtifactPersistOutcome::Replayed)
            } else {
                Err(WorkflowArtifactStoreError::PayloadConflict)
            };
        }
        execute_tx(
            self.transaction,
            "INSERT INTO workflow_artifact_payloads (
                 payload_digest, byte_length, payload
             ) VALUES (?1, ?2, ?3)",
            vec![
                ExactSqlValue::Text(digest.to_owned()),
                ExactSqlValue::Integer(byte_length),
                ExactSqlValue::Blob(payload.bytes().to_vec()),
            ],
        )
        .map_err(artifact_store_unavailable)?;
        Ok(WorkflowArtifactPersistOutcome::Persisted)
    }

    fn load(
        &self,
        artifact: &WorkArtifactRefV1,
    ) -> Result<WorkflowArtifactPayload, WorkflowArtifactStoreError> {
        let Some(bytes) = stored_payload_tx(self.transaction, artifact.digest().as_str())? else {
            return Err(WorkflowArtifactStoreError::Missing);
        };
        // Construction re-verifies byte length and content digest, so a
        // corrupted or foreign row can never re-enter execution.
        WorkflowArtifactPayload::new(artifact.clone(), bytes)
    }
}
