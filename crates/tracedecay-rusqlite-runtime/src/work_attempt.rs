//! Durable Work attempt rows: fenced compare-and-swap transitions over the
//! registered exact-SQL channel.

use tracedecay_application::{
    WorkAttemptEvidenceRecordV1, WorkAttemptInsertOutcome, WorkAttemptStorageError,
    WorkAttemptStoragePort,
};
use tracedecay_domain::{WorkAttemptIdentityV1, WorkAttemptStateV1, WorkAttemptV1, WorkAuthority};

use crate::exact_sql::ExactSqlValue;
use crate::work::{
    WorkSqliteStorage, authority_params_owned, exact_sql_integer, exact_sql_statement,
    exact_sql_text, registered_work_query,
};

impl WorkAttemptStoragePort for WorkSqliteStorage {
    fn next_fence_epoch(&self, authority: &WorkAuthority) -> Result<u64, WorkAttemptStorageError> {
        let transaction = self
            .handle
            .begin_immediate()
            .map_err(|_| WorkAttemptStorageError::Unavailable)?;
        transaction
            .execute(
                exact_sql_statement(
                    "INSERT INTO work_attempt_fences_v1 (
                        project_id, repository_id, worktree_id, actor_id, policy_digest, epoch
                     ) VALUES (?1, ?2, ?3, ?4, ?5, 1)
                     ON CONFLICT (project_id, repository_id, worktree_id, actor_id, policy_digest)
                     DO UPDATE SET epoch = epoch + 1",
                    authority_params_owned(authority),
                )
                .map_err(|_| WorkAttemptStorageError::Unavailable)?,
            )
            .map_err(|_| WorkAttemptStorageError::Unavailable)?;
        let rows = registered_work_query(
            &transaction,
            "SELECT epoch FROM work_attempt_fences_v1
             WHERE project_id = ?1 AND repository_id = ?2 AND worktree_id = ?3
               AND actor_id = ?4 AND policy_digest = ?5",
            authority_params_owned(authority),
        )
        .map_err(|_| WorkAttemptStorageError::Unavailable)?;
        let epoch = rows
            .rows
            .first()
            .and_then(|row| exact_sql_integer(&row.values, 0))
            .and_then(|value| u64::try_from(value).ok())
            .ok_or(WorkAttemptStorageError::Unavailable)?;
        transaction
            .commit()
            .map_err(|_| WorkAttemptStorageError::Unavailable)?;
        Ok(epoch)
    }

    fn insert(
        &self,
        authority: &WorkAuthority,
        attempt: &WorkAttemptV1,
    ) -> Result<WorkAttemptInsertOutcome, WorkAttemptStorageError> {
        let payload =
            serde_json::to_string(attempt).map_err(|_| WorkAttemptStorageError::Unavailable)?;
        let transaction = self
            .handle
            .begin_immediate()
            .map_err(|_| WorkAttemptStorageError::Unavailable)?;
        let existing = load_payload(&transaction, authority, attempt.identity())?;
        if let Some(existing) = existing {
            let _ = transaction.rollback();
            // Idempotent replay only when the stored row is byte-identical to
            // the new admission; the same identity with different content is
            // a conflict, never a refresh.
            return if existing == payload {
                let attempt = serde_json::from_str(&existing)
                    .map_err(|_| WorkAttemptStorageError::Unavailable)?;
                Ok(WorkAttemptInsertOutcome::Replayed(attempt))
            } else {
                Err(WorkAttemptStorageError::AttemptConflict)
            };
        }
        transaction
            .execute(
                exact_sql_statement(
                    "INSERT INTO work_attempts_v1 (
                        project_id, repository_id, worktree_id, actor_id, policy_digest,
                        task_id, run_id, attempt_id, state, lease_id, fence_epoch,
                        terminal, attempt_payload, evidence_payload
                     ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, NULL)",
                    authority_params_owned(authority)
                        .into_iter()
                        .chain(identity_params(attempt.identity()))
                        .chain([
                            ExactSqlValue::Text(state_text(attempt.state())),
                            ExactSqlValue::Text(attempt.lease().lease_id().as_str().to_owned()),
                            ExactSqlValue::Integer(
                                i64::try_from(attempt.lease().epoch().get())
                                    .map_err(|_| WorkAttemptStorageError::Unavailable)?,
                            ),
                            ExactSqlValue::Integer(i64::from(attempt.is_terminal())),
                            ExactSqlValue::Text(payload),
                        ])
                        .collect(),
                )
                .map_err(|_| WorkAttemptStorageError::Unavailable)?,
            )
            .map_err(|_| WorkAttemptStorageError::Unavailable)?;
        transaction
            .commit()
            .map_err(|_| WorkAttemptStorageError::Unavailable)?;
        Ok(WorkAttemptInsertOutcome::Inserted)
    }

    fn load(
        &self,
        authority: &WorkAuthority,
        identity: &WorkAttemptIdentityV1,
    ) -> Result<WorkAttemptV1, WorkAttemptStorageError> {
        let rows = registered_work_query(
            &self.handle,
            "SELECT attempt_payload FROM work_attempts_v1
             WHERE project_id = ?1 AND repository_id = ?2 AND worktree_id = ?3
               AND actor_id = ?4 AND policy_digest = ?5
               AND task_id = ?6 AND run_id = ?7 AND attempt_id = ?8",
            authority_params_owned(authority)
                .into_iter()
                .chain(identity_params(identity))
                .collect(),
        )
        .map_err(|_| WorkAttemptStorageError::Unavailable)?;
        let payload = rows
            .rows
            .first()
            .and_then(|row| exact_sql_text(&row.values, 0))
            .ok_or(WorkAttemptStorageError::NotFoundOrNotAuthorized)?;
        serde_json::from_str(payload).map_err(|_| WorkAttemptStorageError::Unavailable)
    }

    fn update(
        &self,
        authority: &WorkAuthority,
        expected_fence: &tracedecay_domain::WorkLeaseFenceV1,
        expected_state: WorkAttemptStateV1,
        next: &WorkAttemptV1,
        evidence: Option<&WorkAttemptEvidenceRecordV1>,
    ) -> Result<(), WorkAttemptStorageError> {
        let payload =
            serde_json::to_string(next).map_err(|_| WorkAttemptStorageError::Unavailable)?;
        let evidence_payload = evidence
            .map(serde_json::to_string)
            .transpose()
            .map_err(|_| WorkAttemptStorageError::Unavailable)?;
        let transaction = self
            .handle
            .begin_immediate()
            .map_err(|_| WorkAttemptStorageError::Unavailable)?;
        let changed = transaction
            .execute(
                exact_sql_statement(
                    "UPDATE work_attempts_v1 SET
                        state = ?9, lease_id = ?10, fence_epoch = ?11, terminal = ?12,
                        attempt_payload = ?13,
                        evidence_payload = COALESCE(?14, evidence_payload)
                     WHERE project_id = ?1 AND repository_id = ?2 AND worktree_id = ?3
                       AND actor_id = ?4 AND policy_digest = ?5
                       AND task_id = ?6 AND run_id = ?7 AND attempt_id = ?8
                       AND lease_id = ?15 AND fence_epoch = ?16 AND state = ?17",
                    authority_params_owned(authority)
                        .into_iter()
                        .chain(identity_params(next.identity()))
                        .chain([
                            ExactSqlValue::Text(state_text(next.state())),
                            ExactSqlValue::Text(next.lease().lease_id().as_str().to_owned()),
                            ExactSqlValue::Integer(
                                i64::try_from(next.lease().epoch().get())
                                    .map_err(|_| WorkAttemptStorageError::Unavailable)?,
                            ),
                            ExactSqlValue::Integer(i64::from(next.is_terminal())),
                            ExactSqlValue::Text(payload),
                            evidence_payload
                                .map(ExactSqlValue::Text)
                                .unwrap_or(ExactSqlValue::Null),
                            ExactSqlValue::Text(expected_fence.lease_id().as_str().to_owned()),
                            ExactSqlValue::Integer(
                                i64::try_from(expected_fence.epoch().get())
                                    .map_err(|_| WorkAttemptStorageError::Unavailable)?,
                            ),
                            ExactSqlValue::Text(state_text(expected_state)),
                        ])
                        .collect(),
                )
                .map_err(|_| WorkAttemptStorageError::Unavailable)?,
            )
            .map_err(|_| WorkAttemptStorageError::Unavailable)?;
        if changed.changed_rows != 1 {
            let exists = load_payload(&transaction, authority, next.identity())?.is_some();
            let _ = transaction.rollback();
            return Err(if exists {
                WorkAttemptStorageError::FenceConflict
            } else {
                WorkAttemptStorageError::NotFoundOrNotAuthorized
            });
        }
        transaction
            .commit()
            .map_err(|_| WorkAttemptStorageError::Unavailable)?;
        Ok(())
    }

    fn open_attempts(
        &self,
        authority: &WorkAuthority,
    ) -> Result<Vec<WorkAttemptV1>, WorkAttemptStorageError> {
        let rows = registered_work_query(
            &self.handle,
            "SELECT attempt_payload FROM work_attempts_v1
             WHERE project_id = ?1 AND repository_id = ?2 AND worktree_id = ?3
               AND actor_id = ?4 AND policy_digest = ?5 AND terminal = 0
             ORDER BY task_id, run_id, attempt_id",
            authority_params_owned(authority),
        )
        .map_err(|_| WorkAttemptStorageError::Unavailable)?;
        rows.rows
            .into_iter()
            .map(|row| {
                let payload =
                    exact_sql_text(&row.values, 0).ok_or(WorkAttemptStorageError::Unavailable)?;
                serde_json::from_str(payload).map_err(|_| WorkAttemptStorageError::Unavailable)
            })
            .collect()
    }
}

fn load_payload(
    transaction: &crate::exact_sql::ExactSqlTransaction,
    authority: &WorkAuthority,
    identity: &WorkAttemptIdentityV1,
) -> Result<Option<String>, WorkAttemptStorageError> {
    let rows = registered_work_query(
        transaction,
        "SELECT attempt_payload FROM work_attempts_v1
         WHERE project_id = ?1 AND repository_id = ?2 AND worktree_id = ?3
           AND actor_id = ?4 AND policy_digest = ?5
           AND task_id = ?6 AND run_id = ?7 AND attempt_id = ?8",
        authority_params_owned(authority)
            .into_iter()
            .chain(identity_params(identity))
            .collect(),
    )
    .map_err(|_| WorkAttemptStorageError::Unavailable)?;
    Ok(rows
        .rows
        .first()
        .and_then(|row| exact_sql_text(&row.values, 0))
        .map(str::to_owned))
}

fn identity_params(identity: &WorkAttemptIdentityV1) -> [ExactSqlValue; 3] {
    [
        ExactSqlValue::Text(identity.task_id().as_str().to_owned()),
        ExactSqlValue::Text(identity.run_id().as_str().to_owned()),
        ExactSqlValue::Text(identity.attempt_id().as_str().to_owned()),
    ]
}

fn state_text(state: WorkAttemptStateV1) -> String {
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
    .to_owned()
}
