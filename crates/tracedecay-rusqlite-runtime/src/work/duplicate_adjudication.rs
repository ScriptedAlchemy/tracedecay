//! Revision-CAS persistence for explicit duplicate-Work adjudications.

use tracedecay_application::{
    MAX_WORK_DUPLICATE_CLASSIFICATION_ATTEMPTS_V1, WorkDuplicateAdjudicationAppendOutcomeV1,
    WorkDuplicateAdjudicationPortV1, WorkDuplicateAdjudicationStorageErrorV1,
    WorkDuplicateAdjudicationWriteV1, WorkOwnerObservationReceiptV1,
    work_duplicate_adjudication_input_digest,
};
use tracedecay_domain::{
    ProjectionGenerationId, WorkAttemptIdentityV1, WorkAuthority,
    WorkDuplicateAdjudicationCommandV1, WorkDuplicateAdjudicationReceiptV1,
    WorkDuplicateAdjudicationRevisionV1, WorkTopologyGenerationRefV1,
};

use crate::exact_sql::{ExactSqlTransaction, ExactSqlValue};
use crate::work::{
    RegisteredWorkQuery, WorkSqliteStorage, authority_params_owned, exact_sql_statement,
    exact_sql_text, registered_work_query,
};

type StorageError = WorkDuplicateAdjudicationStorageErrorV1;

impl WorkDuplicateAdjudicationPortV1 for WorkSqliteStorage {
    fn compare_and_record_duplicate_adjudication(
        &self,
        authority: &WorkAuthority,
        write: &WorkDuplicateAdjudicationWriteV1,
    ) -> Result<WorkDuplicateAdjudicationAppendOutcomeV1, StorageError> {
        if &write.actor_id != authority.actor_id()
            || write.command.validate().is_err()
            || write.command.clone().canonicalized() != write.command
        {
            return Err(StorageError::NotFoundOrNotAuthorized);
        }
        let canonical_input_digest = work_duplicate_adjudication_input_digest(&write.command)
            .map_err(|_| StorageError::Unavailable)?;
        if canonical_input_digest != write.canonical_input_digest {
            return Err(StorageError::IdempotencyConflict);
        }
        let relation_digest = write
            .command
            .relation_ref(authority)
            .map_err(|_| StorageError::Unavailable)?;
        let transaction = self
            .handle()
            .begin_immediate()
            .map_err(|_| StorageError::Unavailable)?;

        if let Some((stored_digest, receipt)) =
            replay_by_command(&transaction, authority, write.command.command_id.as_str())?
        {
            let _ = transaction.rollback();
            return if stored_digest == write.canonical_input_digest.as_str() {
                Ok(WorkDuplicateAdjudicationAppendOutcomeV1::Replayed(receipt))
            } else {
                Err(StorageError::IdempotencyConflict)
            };
        }

        require_attempt(&transaction, authority, &write.command.first_attempt)?;
        require_attempt(&transaction, authority, &write.command.second_attempt)?;

        let current_receipt =
            current_adjudication(&transaction, authority, relation_digest.as_str())?;
        let current = current_receipt.as_ref().map(|receipt| receipt.revision());
        if current != write.command.expected_revision {
            let _ = transaction.rollback();
            return Err(StorageError::RevisionConflict);
        }
        let revision = match current {
            None => WorkDuplicateAdjudicationRevisionV1::initial(),
            Some(current) => current.next().map_err(|_| StorageError::Unavailable)?,
        };
        let receipt = WorkDuplicateAdjudicationReceiptV1::new(
            authority,
            write.command.clone(),
            revision,
            write.canonical_input_digest.clone(),
        )
        .map_err(|_| StorageError::Unavailable)?;
        insert_receipt(&transaction, authority, &receipt)?;
        transaction
            .commit()
            .map_err(|_| StorageError::Unavailable)?;
        Ok(WorkDuplicateAdjudicationAppendOutcomeV1::Appended(receipt))
    }

    fn latest_duplicate_adjudications_for_attempts(
        &self,
        authority: &WorkAuthority,
        work_generation: &ProjectionGenerationId,
        topology_generation: &WorkTopologyGenerationRefV1,
        attempts: &[WorkAttemptIdentityV1],
    ) -> Result<Vec<WorkDuplicateAdjudicationReceiptV1>, StorageError> {
        if attempts.len() > MAX_WORK_DUPLICATE_CLASSIFICATION_ATTEMPTS_V1
            || attempts.windows(2).any(|pair| pair[0] >= pair[1])
        {
            return Err(StorageError::NotFoundOrNotAuthorized);
        }
        const MAX_LATEST_RELATIONS: usize = MAX_WORK_DUPLICATE_CLASSIFICATION_ATTEMPTS_V1
            * (MAX_WORK_DUPLICATE_CLASSIFICATION_ATTEMPTS_V1 - 1)
            / 2;
        let rows = registered_work_query(
            self.handle(),
            "SELECT receipt_payload, relation_digest FROM (
                SELECT receipt_payload, relation_digest, work_generation, topology_generation,
                       ROW_NUMBER() OVER (
                           PARTITION BY relation_digest ORDER BY revision DESC
                       ) AS latest_ordinal
                FROM work_duplicate_adjudications_v1
                WHERE project_id = ?1 AND repository_id = ?2 AND worktree_id = ?3
                  AND actor_id = ?4 AND policy_digest = ?5
             )
             WHERE latest_ordinal = 1 AND work_generation = ?6
               AND topology_generation = ?7
             LIMIT ?8",
            authority_params_owned(authority)
                .into_iter()
                .chain([
                    ExactSqlValue::Text(work_generation.as_str().to_owned()),
                    ExactSqlValue::Text(topology_generation.as_str().to_owned()),
                    ExactSqlValue::Integer(
                        i64::try_from(MAX_LATEST_RELATIONS + 1)
                            .map_err(|_| StorageError::Unavailable)?,
                    ),
                ])
                .collect(),
        )
        .map_err(|_| StorageError::Unavailable)?;
        if rows.rows.len() > MAX_LATEST_RELATIONS {
            return Err(StorageError::Unavailable);
        }
        let mut receipts = Vec::new();
        for row in rows.rows {
            let receipt = decode_receipt(
                authority,
                exact_sql_text(&row.values, 0).ok_or(StorageError::Unavailable)?,
            )?;
            if exact_sql_text(&row.values, 1) != Some(receipt.adjudication_ref().as_str()) {
                return Err(StorageError::Unavailable);
            }
            if attempts
                .binary_search(&receipt.command().first_attempt)
                .is_ok()
                && attempts
                    .binary_search(&receipt.command().second_attempt)
                    .is_ok()
            {
                receipts.push(receipt);
            }
        }
        Ok(receipts)
    }

    fn latest_duplicate_adjudication_for_pair(
        &self,
        authority: &WorkAuthority,
        first_attempt: &WorkAttemptIdentityV1,
        second_attempt: &WorkAttemptIdentityV1,
    ) -> Result<Option<WorkDuplicateAdjudicationReceiptV1>, StorageError> {
        if first_attempt >= second_attempt {
            return Err(StorageError::NotFoundOrNotAuthorized);
        }
        let relation_ref = WorkDuplicateAdjudicationCommandV1::relation_ref_for_pair(
            authority,
            first_attempt,
            second_attempt,
        )
        .map_err(|_| StorageError::Unavailable)?;
        current_adjudication(self.handle(), authority, relation_ref.as_str())
    }
}

fn replay_by_command(
    transaction: &ExactSqlTransaction,
    authority: &WorkAuthority,
    command_id: &str,
) -> Result<Option<(String, WorkDuplicateAdjudicationReceiptV1)>, StorageError> {
    let rows = registered_work_query(
        transaction,
        "SELECT canonical_input_digest, receipt_payload
         FROM work_duplicate_adjudications_v1
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
    let receipt = decode_receipt(
        authority,
        exact_sql_text(&row.values, 1).ok_or(StorageError::Unavailable)?,
    )?;
    Ok(Some((digest, receipt)))
}

fn current_adjudication(
    source: &impl RegisteredWorkQuery,
    authority: &WorkAuthority,
    relation_digest: &str,
) -> Result<Option<WorkDuplicateAdjudicationReceiptV1>, StorageError> {
    let rows = registered_work_query(
        source,
        "SELECT receipt_payload FROM work_duplicate_adjudications_v1
         WHERE project_id = ?1 AND repository_id = ?2 AND worktree_id = ?3
           AND actor_id = ?4 AND policy_digest = ?5 AND relation_digest = ?6
         ORDER BY revision DESC LIMIT 1",
        authority_params_owned(authority)
            .into_iter()
            .chain([ExactSqlValue::Text(relation_digest.to_owned())])
            .collect(),
    )
    .map_err(|_| StorageError::Unavailable)?;
    rows.rows
        .first()
        .map(|row| {
            decode_receipt(
                authority,
                exact_sql_text(&row.values, 0).ok_or(StorageError::Unavailable)?,
            )
        })
        .transpose()
}

fn require_attempt(
    transaction: &ExactSqlTransaction,
    authority: &WorkAuthority,
    identity: &WorkAttemptIdentityV1,
) -> Result<(), StorageError> {
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

fn insert_receipt(
    transaction: &ExactSqlTransaction,
    authority: &WorkAuthority,
    receipt: &WorkDuplicateAdjudicationReceiptV1,
) -> Result<(), StorageError> {
    let payload = serde_json::to_string(receipt).map_err(|_| StorageError::Unavailable)?;
    let receipt_digest = tracedecay_domain::canonical_sha256(
        &WorkOwnerObservationReceiptV1::Duplicate(receipt.clone()),
    )
    .map_err(|_| StorageError::Unavailable)?;
    transaction
        .execute(
            exact_sql_statement(
                "INSERT INTO work_duplicate_adjudications_v1 (
                    project_id, repository_id, worktree_id, actor_id, policy_digest,
                    relation_digest, revision, command_id, canonical_input_digest,
                    work_generation, topology_generation, occurred_at, receipt_digest,
                    observation_state, receipt_payload
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13,
                    'pending', ?14)",
                authority_params_owned(authority)
                    .into_iter()
                    .chain([
                        ExactSqlValue::Text(receipt.adjudication_ref().as_str().to_owned()),
                        ExactSqlValue::Integer(
                            i64::try_from(receipt.revision().get())
                                .map_err(|_| StorageError::Unavailable)?,
                        ),
                        ExactSqlValue::Text(receipt.command().command_id.as_str().to_owned()),
                        ExactSqlValue::Text(receipt.canonical_input_digest().as_str().to_owned()),
                        ExactSqlValue::Text(
                            receipt
                                .command()
                                .evidence
                                .work_generation
                                .as_str()
                                .to_owned(),
                        ),
                        ExactSqlValue::Text(
                            receipt
                                .command()
                                .evidence
                                .topology_generation
                                .as_str()
                                .to_owned(),
                        ),
                        ExactSqlValue::Integer(receipt.command().occurred_at.0),
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

fn decode_receipt(
    authority: &WorkAuthority,
    payload: &str,
) -> Result<WorkDuplicateAdjudicationReceiptV1, StorageError> {
    let stored: WorkDuplicateAdjudicationReceiptV1 =
        serde_json::from_str(payload).map_err(|_| StorageError::Unavailable)?;
    let canonical = WorkDuplicateAdjudicationReceiptV1::new(
        authority,
        stored.command().clone(),
        stored.revision(),
        stored.canonical_input_digest().clone(),
    )
    .map_err(|_| StorageError::Unavailable)?;
    if canonical == stored {
        Ok(canonical)
    } else {
        Err(StorageError::Unavailable)
    }
}
