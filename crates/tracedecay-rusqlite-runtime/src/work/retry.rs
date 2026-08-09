//! Atomic persistence of a new retry attempt and its durable lineage receipt.

use serde::{Deserialize, Serialize};
use tracedecay_application::{
    WorkAttemptStorageError, WorkRetryAttemptOutcomeV1, WorkRetryReceiptV1, WorkRetryStoragePortV1,
    WorkRetryWriteV1,
};
use tracedecay_domain::{
    TopologyConcurrencyPolicyV1, WorkAttemptIdentityV1, WorkAttemptV1, WorkAuthority,
    canonical_sha256,
};

use crate::exact_sql::{ExactSqlTransaction, ExactSqlValue};
use crate::work::{
    WorkSqliteStorage, authority_params_owned, exact_sql_integer, exact_sql_statement,
    exact_sql_text, registered_work_query,
};

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct StoredWorkAttemptV1 {
    attempt: WorkAttemptV1,
    synthesis: Option<serde_json::Value>,
}

impl WorkRetryStoragePortV1 for WorkSqliteStorage {
    fn retry_by_command(
        &self,
        authority: &WorkAuthority,
        command_id: &tracedecay_domain::WorkCommandId,
    ) -> Result<Option<WorkRetryAttemptOutcomeV1>, WorkAttemptStorageError> {
        let transaction = self
            .handle
            .begin_deferred()
            .map_err(|_| WorkAttemptStorageError::Unavailable)?;
        let outcome = replay(&transaction, authority, command_id.as_str())?;
        transaction
            .commit()
            .map_err(|_| WorkAttemptStorageError::Unavailable)?;
        Ok(outcome)
    }

    fn insert_retry_bounded(
        &self,
        authority: &WorkAuthority,
        write: &WorkRetryWriteV1,
        concurrency: &TopologyConcurrencyPolicyV1,
    ) -> Result<WorkRetryAttemptOutcomeV1, WorkAttemptStorageError> {
        validate_write(write)?;
        let transaction = self
            .handle
            .begin_immediate()
            .map_err(|_| WorkAttemptStorageError::Unavailable)?;
        if let Some(replayed) = replay(
            &transaction,
            authority,
            write.receipt.command.command_id.as_str(),
        )? {
            let _ = transaction.rollback();
            return if replayed.receipt().canonical_input_digest
                == write.receipt.canonical_input_digest
            {
                Ok(replayed)
            } else {
                Err(WorkAttemptStorageError::AttemptConflict)
            };
        }
        require_attempt(
            &transaction,
            authority,
            &write.receipt.command.original_attempt,
            true,
        )?;
        if load_attempt(&transaction, authority, write.attempt.identity())?.is_some() {
            let _ = transaction.rollback();
            return Err(WorkAttemptStorageError::AttemptConflict);
        }
        require_run_reservation(&transaction, authority, write.attempt.identity())?;
        require_first_run_admission(&transaction, authority, &write.attempt)?;
        crate::work::capacity::require_capacity(
            &transaction,
            authority,
            write.attempt.identity().task_id(),
            concurrency,
        )?;
        insert_attempt(&transaction, authority, &write.attempt)?;
        insert_receipt(&transaction, authority, write)?;
        transaction
            .commit()
            .map_err(|_| WorkAttemptStorageError::Unavailable)?;
        Ok(WorkRetryAttemptOutcomeV1::Created {
            receipt: write.receipt.clone(),
            attempt: write.attempt.clone(),
        })
    }
}

fn validate_write(write: &WorkRetryWriteV1) -> Result<(), WorkAttemptStorageError> {
    let command = &write.receipt.command;
    let expected = canonical_sha256(&("tracedecay.application.work-retry-input.v1", command))
        .map_err(|_| WorkAttemptStorageError::Unavailable)?;
    if write.attempt.identity() != &write.receipt.new_attempt
        || !write.receipt.validate_for_observation()
        || write.receipt.new_attempt.task_id() != command.original_attempt.task_id()
        || write.receipt.new_attempt.run_id() != command.original_attempt.run_id()
        || write.receipt.new_attempt.attempt_id() != &command.new_attempt_id
        || write.receipt.failure.selector != command.failure
        || write.receipt.canonical_input_digest != expected
        || write.attempt.is_terminal()
    {
        return Err(WorkAttemptStorageError::AttemptConflict);
    }
    Ok(())
}

fn replay(
    transaction: &ExactSqlTransaction,
    authority: &WorkAuthority,
    command_id: &str,
) -> Result<Option<WorkRetryAttemptOutcomeV1>, WorkAttemptStorageError> {
    let rows = registered_work_query(
        transaction,
        "SELECT receipt_payload, task_id, run_id, new_attempt_id,
                canonical_input_digest, original_attempt_id, restarted_at, receipt_digest
         FROM work_retry_receipts_v1
         WHERE project_id = ?1 AND repository_id = ?2 AND worktree_id = ?3
           AND actor_id = ?4 AND policy_digest = ?5 AND command_id = ?6",
        authority_params_owned(authority)
            .into_iter()
            .chain([ExactSqlValue::Text(command_id.to_owned())])
            .collect(),
    )
    .map_err(|_| WorkAttemptStorageError::Unavailable)?;
    let Some(row) = rows.rows.first() else {
        return Ok(None);
    };
    let receipt: WorkRetryReceiptV1 = serde_json::from_str(
        exact_sql_text(&row.values, 0).ok_or(WorkAttemptStorageError::Unavailable)?,
    )
    .map_err(|_| WorkAttemptStorageError::Unavailable)?;
    let identity = WorkAttemptIdentityV1::new(
        tracedecay_domain::TaskId::new(
            exact_sql_text(&row.values, 1)
                .ok_or(WorkAttemptStorageError::Unavailable)?
                .to_owned(),
        )
        .map_err(|_| WorkAttemptStorageError::Unavailable)?,
        tracedecay_domain::RunId::new(
            exact_sql_text(&row.values, 2)
                .ok_or(WorkAttemptStorageError::Unavailable)?
                .to_owned(),
        )
        .map_err(|_| WorkAttemptStorageError::Unavailable)?,
        tracedecay_domain::AttemptId::new(
            exact_sql_text(&row.values, 3)
                .ok_or(WorkAttemptStorageError::Unavailable)?
                .to_owned(),
        )
        .map_err(|_| WorkAttemptStorageError::Unavailable)?,
    )
    .map_err(|_| WorkAttemptStorageError::Unavailable)?;
    let expected_receipt_digest = canonical_sha256(
        &tracedecay_application::WorkOwnerObservationReceiptV1::Retry(receipt.clone()),
    )
    .map_err(|_| WorkAttemptStorageError::Unavailable)?;
    if !receipt.validate_for_observation()
        || receipt.command.command_id.as_str() != command_id
        || receipt.new_attempt != identity
        || exact_sql_text(&row.values, 4) != Some(receipt.canonical_input_digest.as_str())
        || exact_sql_text(&row.values, 5)
            != Some(receipt.command.original_attempt.attempt_id().as_str())
        || exact_sql_integer(&row.values, 6) != Some(receipt.restarted_at.0)
        || exact_sql_text(&row.values, 7) != Some(expected_receipt_digest.as_str())
    {
        return Err(WorkAttemptStorageError::Unavailable);
    }
    let attempt = load_attempt(transaction, authority, &identity)?
        .ok_or(WorkAttemptStorageError::Unavailable)?;
    Ok(Some(WorkRetryAttemptOutcomeV1::Replayed {
        receipt,
        attempt,
    }))
}

fn load_attempt(
    transaction: &ExactSqlTransaction,
    authority: &WorkAuthority,
    identity: &WorkAttemptIdentityV1,
) -> Result<Option<WorkAttemptV1>, WorkAttemptStorageError> {
    let rows = registered_work_query(
        transaction,
        "SELECT task_id, run_id, attempt_id, state, lease_id, fence_epoch, terminal,
                attempt_payload FROM work_attempts_v1
         WHERE project_id = ?1 AND repository_id = ?2 AND worktree_id = ?3
           AND actor_id = ?4 AND policy_digest = ?5
           AND task_id = ?6 AND run_id = ?7 AND attempt_id = ?8",
        authority_params_owned(authority)
            .into_iter()
            .chain(identity_params(identity))
            .collect(),
    )
    .map_err(|_| WorkAttemptStorageError::Unavailable)?;
    rows.rows
        .first()
        .map(|row| {
            let stored = serde_json::from_str::<StoredWorkAttemptV1>(
                exact_sql_text(&row.values, 7).ok_or(WorkAttemptStorageError::Unavailable)?,
            )
            .map_err(|_| WorkAttemptStorageError::Unavailable)?;
            let attempt = stored.attempt;
            let epoch = exact_sql_integer(&row.values, 5)
                .and_then(|value| u64::try_from(value).ok())
                .ok_or(WorkAttemptStorageError::Unavailable)?;
            if exact_sql_text(&row.values, 0) != Some(attempt.identity().task_id().as_str())
                || exact_sql_text(&row.values, 1) != Some(attempt.identity().run_id().as_str())
                || exact_sql_text(&row.values, 2) != Some(attempt.identity().attempt_id().as_str())
                || exact_sql_text(&row.values, 3) != Some(attempt_state(attempt.state()))
                || exact_sql_text(&row.values, 4) != Some(attempt.lease().lease_id().as_str())
                || epoch != attempt.lease().epoch().get()
                || exact_sql_integer(&row.values, 6) != Some(i64::from(attempt.is_terminal()))
                || attempt.identity() != identity
            {
                return Err(WorkAttemptStorageError::Unavailable);
            }
            Ok(attempt)
        })
        .transpose()
}

const fn attempt_state(state: tracedecay_domain::WorkAttemptStateV1) -> &'static str {
    use tracedecay_domain::WorkAttemptStateV1;

    match state {
        WorkAttemptStateV1::Leased => "leased",
        WorkAttemptStateV1::Running => "running",
        WorkAttemptStateV1::CancellationRequested => "cancellation_requested",
        WorkAttemptStateV1::CancellationAcknowledged => "cancellation_acknowledged",
        WorkAttemptStateV1::CancellationEscalated => "cancellation_escalated",
        WorkAttemptStateV1::RecoveryRequired => "recovery_required",
        WorkAttemptStateV1::Succeeded => "succeeded",
        WorkAttemptStateV1::Failed => "failed",
        WorkAttemptStateV1::TimedOut => "timed_out",
        WorkAttemptStateV1::Cancelled => "cancelled",
    }
}

fn require_attempt(
    transaction: &ExactSqlTransaction,
    authority: &WorkAuthority,
    identity: &WorkAttemptIdentityV1,
    terminal: bool,
) -> Result<(), WorkAttemptStorageError> {
    let rows = registered_work_query(
        transaction,
        "SELECT terminal FROM work_attempts_v1
         WHERE project_id = ?1 AND repository_id = ?2 AND worktree_id = ?3
           AND actor_id = ?4 AND policy_digest = ?5
           AND task_id = ?6 AND run_id = ?7 AND attempt_id = ?8",
        authority_params_owned(authority)
            .into_iter()
            .chain(identity_params(identity))
            .collect(),
    )
    .map_err(|_| WorkAttemptStorageError::Unavailable)?;
    let observed = rows
        .rows
        .first()
        .and_then(|row| exact_sql_integer(&row.values, 0))
        .ok_or(WorkAttemptStorageError::NotFoundOrNotAuthorized)?;
    if observed == i64::from(terminal) {
        Ok(())
    } else {
        Err(WorkAttemptStorageError::AttemptConflict)
    }
}

fn require_run_reservation(
    transaction: &ExactSqlTransaction,
    authority: &WorkAuthority,
    identity: &WorkAttemptIdentityV1,
) -> Result<(), WorkAttemptStorageError> {
    let rows = registered_work_query(
        transaction,
        "SELECT state FROM work_run_controls_v1
         WHERE project_id = ?1 AND repository_id = ?2 AND worktree_id = ?3
           AND actor_id = ?4 AND policy_digest = ?5 AND task_id = ?6 AND run_id = ?7",
        authority_params_owned(authority)
            .into_iter()
            .chain([
                ExactSqlValue::Text(identity.task_id().as_str().to_owned()),
                ExactSqlValue::Text(identity.run_id().as_str().to_owned()),
            ])
            .collect(),
    )
    .map_err(|_| WorkAttemptStorageError::Unavailable)?;
    match rows
        .rows
        .first()
        .and_then(|row| exact_sql_text(&row.values, 0))
    {
        None | Some("running") => Ok(()),
        Some("paused") => Err(WorkAttemptStorageError::ReservationFenced),
        Some(_) => Err(WorkAttemptStorageError::Unavailable),
    }
}

fn require_first_run_admission(
    transaction: &ExactSqlTransaction,
    authority: &WorkAuthority,
    attempt: &WorkAttemptV1,
) -> Result<(), WorkAttemptStorageError> {
    let rows = registered_work_query(
        transaction,
        "SELECT attempt_payload FROM work_attempts_v1
         WHERE project_id = ?1 AND repository_id = ?2 AND worktree_id = ?3
           AND actor_id = ?4 AND policy_digest = ?5 AND task_id = ?6 AND run_id = ?7
         ORDER BY rowid LIMIT 1",
        authority_params_owned(authority)
            .into_iter()
            .chain([
                ExactSqlValue::Text(attempt.identity().task_id().as_str().to_owned()),
                ExactSqlValue::Text(attempt.identity().run_id().as_str().to_owned()),
            ])
            .collect(),
    )
    .map_err(|_| WorkAttemptStorageError::Unavailable)?;
    let payload = rows
        .rows
        .first()
        .and_then(|row| exact_sql_text(&row.values, 0))
        .ok_or(WorkAttemptStorageError::NotFoundOrNotAuthorized)?;
    let first: StoredWorkAttemptV1 =
        serde_json::from_str(payload).map_err(|_| WorkAttemptStorageError::Unavailable)?;
    if first.attempt.execution().deadline() == attempt.execution().deadline()
        && first.attempt.execution().execution_snapshot().topology()
            == attempt.execution().execution_snapshot().topology()
    {
        Ok(())
    } else {
        Err(WorkAttemptStorageError::RunAdmissionConflict)
    }
}

fn insert_attempt(
    transaction: &ExactSqlTransaction,
    authority: &WorkAuthority,
    attempt: &WorkAttemptV1,
) -> Result<(), WorkAttemptStorageError> {
    let payload = serde_json::to_string(&StoredWorkAttemptV1 {
        attempt: attempt.clone(),
        synthesis: None,
    })
    .map_err(|_| WorkAttemptStorageError::Unavailable)?;
    transaction
        .execute(
            exact_sql_statement(
                "INSERT INTO work_attempts_v1 (
                    project_id, repository_id, worktree_id, actor_id, policy_digest,
                    task_id, run_id, attempt_id, state, lease_id, fence_epoch,
                    terminal, attempt_payload, evidence_payload
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, 'recovery_required', ?9, ?10, 0, ?11, NULL)",
                authority_params_owned(authority)
                    .into_iter()
                    .chain(identity_params(attempt.identity()))
                    .chain([
                        ExactSqlValue::Text(attempt.lease().lease_id().as_str().to_owned()),
                        ExactSqlValue::Integer(
                            i64::try_from(attempt.lease().epoch().get())
                                .map_err(|_| WorkAttemptStorageError::Unavailable)?,
                        ),
                        ExactSqlValue::Text(payload),
                    ])
                    .collect(),
            )
            .map_err(|_| WorkAttemptStorageError::Unavailable)?,
        )
        .map_err(|_| WorkAttemptStorageError::Unavailable)?;
    Ok(())
}

fn insert_receipt(
    transaction: &ExactSqlTransaction,
    authority: &WorkAuthority,
    write: &WorkRetryWriteV1,
) -> Result<(), WorkAttemptStorageError> {
    let payload =
        serde_json::to_string(&write.receipt).map_err(|_| WorkAttemptStorageError::Unavailable)?;
    let receipt_digest = canonical_sha256(
        &tracedecay_application::WorkOwnerObservationReceiptV1::Retry(write.receipt.clone()),
    )
    .map_err(|_| WorkAttemptStorageError::Unavailable)?;
    transaction
        .execute(
            exact_sql_statement(
                "INSERT INTO work_retry_receipts_v1 (
                    project_id, repository_id, worktree_id, actor_id, policy_digest,
                    command_id, canonical_input_digest, task_id, run_id,
                    original_attempt_id, new_attempt_id, restarted_at, receipt_digest,
                    receipt_payload
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)",
                authority_params_owned(authority)
                    .into_iter()
                    .chain([
                        ExactSqlValue::Text(write.receipt.command.command_id.as_str().to_owned()),
                        ExactSqlValue::Text(
                            write.receipt.canonical_input_digest.as_str().to_owned(),
                        ),
                        ExactSqlValue::Text(
                            write.receipt.new_attempt.task_id().as_str().to_owned(),
                        ),
                        ExactSqlValue::Text(write.receipt.new_attempt.run_id().as_str().to_owned()),
                        ExactSqlValue::Text(
                            write
                                .receipt
                                .command
                                .original_attempt
                                .attempt_id()
                                .as_str()
                                .to_owned(),
                        ),
                        ExactSqlValue::Text(
                            write.receipt.new_attempt.attempt_id().as_str().to_owned(),
                        ),
                        ExactSqlValue::Integer(write.receipt.restarted_at.0),
                        ExactSqlValue::Text(receipt_digest.as_str().to_owned()),
                        ExactSqlValue::Text(payload),
                    ])
                    .collect(),
            )
            .map_err(|_| WorkAttemptStorageError::Unavailable)?,
        )
        .map_err(|_| WorkAttemptStorageError::Unavailable)?;
    Ok(())
}

fn identity_params(identity: &WorkAttemptIdentityV1) -> [ExactSqlValue; 3] {
    [
        ExactSqlValue::Text(identity.task_id().as_str().to_owned()),
        ExactSqlValue::Text(identity.run_id().as_str().to_owned()),
        ExactSqlValue::Text(identity.attempt_id().as_str().to_owned()),
    ]
}
