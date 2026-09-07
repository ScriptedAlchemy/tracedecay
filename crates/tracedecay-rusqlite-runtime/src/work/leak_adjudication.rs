//! Revision-CAS persistence for bounded Work leak adjudications.

use tracedecay_application::{
    WorkLeakAdjudicationOutcomeV1, WorkLeakAdjudicationStorageErrorV1,
    WorkLeakAdjudicationStoragePortV1, WorkLeakAdjudicationWriteV1,
};
use tracedecay_domain::{WorkAuthority, canonical_sha256};

use crate::exact_sql::{ExactSqlTransaction, ExactSqlValue};
use crate::work::{
    WorkSqliteStorage, authority_params_owned, exact_sql_integer, exact_sql_statement,
    exact_sql_text, registered_work_query,
};

type StorageError = WorkLeakAdjudicationStorageErrorV1;

impl WorkLeakAdjudicationStoragePortV1 for WorkSqliteStorage {
    fn leak_by_command(
        &self,
        authority: &WorkAuthority,
        command_id: &tracedecay_domain::WorkCommandId,
    ) -> Result<Option<tracedecay_application::WorkLeakAdjudicationReceiptV1>, StorageError> {
        let transaction = self
            .handle()
            .begin_deferred()
            .map_err(|_| StorageError::Unavailable)?;
        let receipt = replay_by_command(&transaction, authority, command_id.as_str())?
            .map(|(_, receipt)| receipt);
        transaction
            .commit()
            .map_err(|_| StorageError::Unavailable)?;
        Ok(receipt)
    }

    fn compare_and_record_leak(
        &self,
        authority: &WorkAuthority,
        write: &WorkLeakAdjudicationWriteV1,
    ) -> Result<WorkLeakAdjudicationOutcomeV1, StorageError> {
        validate_write(write)?;
        let transaction = self
            .handle()
            .begin_immediate()
            .map_err(|_| StorageError::Unavailable)?;
        if let Some((digest, receipt)) = replay_by_command(
            &transaction,
            authority,
            write.receipt.command.command_id.as_str(),
        )? {
            let _ = transaction.rollback();
            return if digest == write.receipt.canonical_input_digest.as_str() {
                Ok(WorkLeakAdjudicationOutcomeV1::Replayed(receipt))
            } else {
                Err(StorageError::IdempotencyConflict)
            };
        }
        require_attempt(&transaction, authority, write)?;
        let current = current_revision(
            &transaction,
            authority,
            &write.receipt.command.adjudication_id,
        )?;
        if current != write.receipt.command.expected_revision {
            let _ = transaction.rollback();
            return Err(StorageError::RevisionConflict);
        }
        insert_receipt(&transaction, authority, write)?;
        transaction
            .commit()
            .map_err(|_| StorageError::Unavailable)?;
        Ok(WorkLeakAdjudicationOutcomeV1::Appended(
            write.receipt.clone(),
        ))
    }
}

fn validate_write(write: &WorkLeakAdjudicationWriteV1) -> Result<(), StorageError> {
    let receipt = &write.receipt;
    let expected_revision = receipt
        .command
        .expected_revision
        .unwrap_or(0)
        .checked_add(1)
        .ok_or(StorageError::Unavailable)?;
    let expected_digest = canonical_sha256(&(
        "tracedecay.application.work-leak-adjudication.v1",
        &receipt.command,
        &receipt.evidence,
        receipt.scan_deadline,
    ))
    .map_err(|_| StorageError::Unavailable)?;
    if receipt.revision != expected_revision
        || !receipt.validate_for_observation()
        || receipt.evidence.attempt != receipt.command.attempt
        || receipt.canonical_input_digest != expected_digest
    {
        return Err(StorageError::IdempotencyConflict);
    }
    Ok(())
}

fn replay_by_command(
    transaction: &ExactSqlTransaction,
    authority: &WorkAuthority,
    command_id: &str,
) -> Result<
    Option<(
        String,
        tracedecay_application::WorkLeakAdjudicationReceiptV1,
    )>,
    StorageError,
> {
    let rows = registered_work_query(
        transaction,
        "SELECT canonical_input_digest, receipt_payload, adjudication_id, revision,
                task_id, run_id, attempt_id, observed_at, receipt_digest
         FROM work_leak_adjudications_v1
         WHERE project_id = ?1 AND repository_id = ?2 AND worktree_id = ?3
           AND actor_id = ?4 AND policy_digest = ?5 AND command_id = ?6",
        authority_params_owned(authority)
            .into_iter()
            .chain([ExactSqlValue::Text(command_id.to_owned())])
            .collect(),
    )
    .map_err(|_| StorageError::Unavailable)?;
    let Some(row) = rows.rows.first() else {
        return Ok(None);
    };
    let digest = exact_sql_text(&row.values, 0)
        .ok_or(StorageError::Unavailable)?
        .to_owned();
    let receipt: tracedecay_application::WorkLeakAdjudicationReceiptV1 =
        serde_json::from_str(exact_sql_text(&row.values, 1).ok_or(StorageError::Unavailable)?)
            .map_err(|_| StorageError::Unavailable)?;
    let expected_receipt_digest = canonical_sha256(
        &tracedecay_application::WorkOwnerObservationReceiptV1::Leak(receipt.clone()),
    )
    .map_err(|_| StorageError::Unavailable)?;
    let revision = exact_sql_integer(&row.values, 3)
        .and_then(|value| u64::try_from(value).ok())
        .ok_or(StorageError::Unavailable)?;
    if !receipt.validate_for_observation()
        || receipt.command.command_id.as_str() != command_id
        || digest != receipt.canonical_input_digest.as_str()
        || exact_sql_text(&row.values, 2) != Some(receipt.command.adjudication_id.as_str())
        || revision != receipt.revision
        || exact_sql_text(&row.values, 4) != Some(receipt.command.attempt.task_id().as_str())
        || exact_sql_text(&row.values, 5) != Some(receipt.command.attempt.run_id().as_str())
        || exact_sql_text(&row.values, 6) != Some(receipt.command.attempt.attempt_id().as_str())
        || exact_sql_integer(&row.values, 7) != Some(receipt.evidence.scan_completed_at.0)
        || exact_sql_text(&row.values, 8) != Some(expected_receipt_digest.as_str())
    {
        return Err(StorageError::Unavailable);
    }
    Ok(Some((digest, receipt)))
}

fn require_attempt(
    transaction: &ExactSqlTransaction,
    authority: &WorkAuthority,
    write: &WorkLeakAdjudicationWriteV1,
) -> Result<(), StorageError> {
    let identity = &write.receipt.command.attempt;
    let rows = registered_work_query(
        transaction,
        "SELECT 1 FROM work_attempts_v1
         WHERE project_id = ?1 AND repository_id = ?2 AND worktree_id = ?3
           AND actor_id = ?4 AND policy_digest = ?5
           AND task_id = ?6 AND run_id = ?7 AND attempt_id = ?8",
        authority_params_owned(authority)
            .into_iter()
            .chain([
                ExactSqlValue::Text(identity.task_id().as_str().to_owned()),
                ExactSqlValue::Text(identity.run_id().as_str().to_owned()),
                ExactSqlValue::Text(identity.attempt_id().as_str().to_owned()),
            ])
            .collect(),
    )
    .map_err(|_| StorageError::Unavailable)?;
    if rows.rows.len() == 1 {
        Ok(())
    } else {
        Err(StorageError::NotFoundOrNotAuthorized)
    }
}

fn current_revision(
    transaction: &ExactSqlTransaction,
    authority: &WorkAuthority,
    adjudication_id: &str,
) -> Result<Option<u64>, StorageError> {
    let rows = registered_work_query(
        transaction,
        "SELECT revision FROM work_leak_adjudications_v1
         WHERE project_id = ?1 AND repository_id = ?2 AND worktree_id = ?3
           AND actor_id = ?4 AND policy_digest = ?5 AND adjudication_id = ?6
         ORDER BY revision DESC LIMIT 1",
        authority_params_owned(authority)
            .into_iter()
            .chain([ExactSqlValue::Text(adjudication_id.to_owned())])
            .collect(),
    )
    .map_err(|_| StorageError::Unavailable)?;
    rows.rows
        .first()
        .map(|row| {
            exact_sql_integer(&row.values, 0)
                .and_then(|value| u64::try_from(value).ok())
                .filter(|revision| *revision > 0)
                .ok_or(StorageError::Unavailable)
        })
        .transpose()
}

fn insert_receipt(
    transaction: &ExactSqlTransaction,
    authority: &WorkAuthority,
    write: &WorkLeakAdjudicationWriteV1,
) -> Result<(), StorageError> {
    let receipt = &write.receipt;
    let payload = serde_json::to_string(receipt).map_err(|_| StorageError::Unavailable)?;
    let receipt_digest = canonical_sha256(
        &tracedecay_application::WorkOwnerObservationReceiptV1::Leak(receipt.clone()),
    )
    .map_err(|_| StorageError::Unavailable)?;
    transaction
        .execute(
            exact_sql_statement(
                "INSERT INTO work_leak_adjudications_v1 (
                    project_id, repository_id, worktree_id, actor_id, policy_digest,
                    adjudication_id, revision, command_id, canonical_input_digest,
                    task_id, run_id, attempt_id, observed_at, receipt_digest, receipt_payload
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)",
                authority_params_owned(authority)
                    .into_iter()
                    .chain([
                        ExactSqlValue::Text(receipt.command.adjudication_id.clone()),
                        ExactSqlValue::Integer(
                            i64::try_from(receipt.revision)
                                .map_err(|_| StorageError::Unavailable)?,
                        ),
                        ExactSqlValue::Text(receipt.command.command_id.as_str().to_owned()),
                        ExactSqlValue::Text(receipt.canonical_input_digest.as_str().to_owned()),
                        ExactSqlValue::Text(receipt.command.attempt.task_id().as_str().to_owned()),
                        ExactSqlValue::Text(receipt.command.attempt.run_id().as_str().to_owned()),
                        ExactSqlValue::Text(
                            receipt.command.attempt.attempt_id().as_str().to_owned(),
                        ),
                        ExactSqlValue::Integer(receipt.evidence.scan_completed_at.0),
                        ExactSqlValue::Text(receipt_digest.as_str().to_owned()),
                        ExactSqlValue::Text(payload),
                    ])
                    .collect(),
            )
            .map_err(|_| StorageError::Unavailable)?,
        )
        .map_err(|_| StorageError::Unavailable)?;
    Ok(())
}
