//! Durable Work attempt rows: fenced compare-and-swap transitions over the
//! registered exact-SQL channel.

use serde::{Deserialize, Serialize};
use tracedecay_application::{
    WorkAttemptAdmissionKind, WorkAttemptEvidencePageV1, WorkAttemptEvidenceReadPort,
    WorkAttemptEvidenceRecordV1, WorkAttemptEvidenceRowV1, WorkAttemptInsertOutcome,
    WorkAttemptListPageV1, WorkAttemptStorageError, WorkAttemptStoragePort,
    WorkSynthesisAdmissionRecordV1, WorkSynthesisAdmissionStoragePort, WorkSynthesisInsertOutcome,
};
use tracedecay_domain::{WorkAttemptIdentityV1, WorkAttemptStateV1, WorkAttemptV1, WorkAuthority};

use crate::exact_sql::ExactSqlValue;
use crate::work::{
    WorkSqliteStorage, authority_params_owned, exact_sql_integer, exact_sql_statement,
    exact_sql_text, registered_work_query,
};

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct StoredWorkAttemptV1 {
    attempt: WorkAttemptV1,
    synthesis: Option<WorkSynthesisAdmissionRecordV1>,
}

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
        let payload = serde_json::to_string(&StoredWorkAttemptV1 {
            attempt: attempt.clone(),
            synthesis: None,
        })
        .map_err(|_| WorkAttemptStorageError::Unavailable)?;
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
                let record: StoredWorkAttemptV1 = serde_json::from_str(&existing)
                    .map_err(|_| WorkAttemptStorageError::Unavailable)?;
                Ok(WorkAttemptInsertOutcome::Replayed(Box::new(record.attempt)))
            } else {
                Err(WorkAttemptStorageError::AttemptConflict)
            };
        }
        require_first_run_admission(&transaction, authority, attempt)?;
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
        serde_json::from_str::<StoredWorkAttemptV1>(payload)
            .map(|record| record.attempt)
            .map_err(|_| WorkAttemptStorageError::Unavailable)
    }

    fn load_admission_kind(
        &self,
        authority: &WorkAuthority,
        identity: &WorkAttemptIdentityV1,
    ) -> Result<WorkAttemptAdmissionKind, WorkAttemptStorageError> {
        let payload = load_payload_from_handle(&self.handle, authority, identity)?
            .ok_or(WorkAttemptStorageError::NotFoundOrNotAuthorized)?;
        let record: StoredWorkAttemptV1 =
            serde_json::from_str(&payload).map_err(|_| WorkAttemptStorageError::Unavailable)?;
        Ok(if record.synthesis.is_some() {
            WorkAttemptAdmissionKind::Synthesis
        } else {
            WorkAttemptAdmissionKind::Ordinary
        })
    }

    fn update(
        &self,
        authority: &WorkAuthority,
        expected_fence: &tracedecay_domain::WorkLeaseFenceV1,
        expected_state: WorkAttemptStateV1,
        next: &WorkAttemptV1,
        evidence: Option<&WorkAttemptEvidenceRecordV1>,
    ) -> Result<(), WorkAttemptStorageError> {
        let evidence_payload = evidence
            .map(serde_json::to_string)
            .transpose()
            .map_err(|_| WorkAttemptStorageError::Unavailable)?;
        let transaction = self
            .handle
            .begin_immediate()
            .map_err(|_| WorkAttemptStorageError::Unavailable)?;
        let existing = load_payload(&transaction, authority, next.identity())?
            .ok_or(WorkAttemptStorageError::NotFoundOrNotAuthorized)?;
        let mut record: StoredWorkAttemptV1 =
            serde_json::from_str(&existing).map_err(|_| WorkAttemptStorageError::Unavailable)?;
        record.attempt = next.clone();
        let payload =
            serde_json::to_string(&record).map_err(|_| WorkAttemptStorageError::Unavailable)?;
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
            let _ = transaction.rollback();
            return Err(WorkAttemptStorageError::FenceConflict);
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
                serde_json::from_str::<StoredWorkAttemptV1>(payload)
                    .map(|record| record.attempt)
                    .map_err(|_| WorkAttemptStorageError::Unavailable)
            })
            .collect()
    }

    fn list(
        &self,
        authority: &WorkAuthority,
        start_after: Option<&WorkAttemptIdentityV1>,
        limit: u32,
    ) -> Result<WorkAttemptListPageV1, WorkAttemptStorageError> {
        let authority_filter = "project_id = ?1 AND repository_id = ?2 AND worktree_id = ?3
               AND actor_id = ?4 AND policy_digest = ?5";
        let after_filter = if start_after.is_some() {
            " AND (task_id, run_id, attempt_id) > (?6, ?7, ?8)"
        } else {
            ""
        };
        let mut params = authority_params_owned(authority);
        if let Some(start_after) = start_after {
            params.extend(identity_params(start_after));
        }
        // One deferred transaction keeps the remaining count and the page on
        // the same consistent view of the attempt rows.
        let transaction = self
            .handle
            .begin_deferred()
            .map_err(|_| WorkAttemptStorageError::Unavailable)?;
        let counted = registered_work_query(
            &transaction,
            &format!(
                "SELECT COUNT(*) FROM work_attempts_v1 WHERE {authority_filter}{after_filter}"
            ),
            params.clone(),
        )
        .map_err(|_| WorkAttemptStorageError::Unavailable)?;
        let remaining = counted
            .rows
            .first()
            .and_then(|row| exact_sql_integer(&row.values, 0))
            .and_then(|value| u32::try_from(value).ok())
            .ok_or(WorkAttemptStorageError::Unavailable)?;
        let limit_placeholder = params.len() + 1;
        params.push(ExactSqlValue::Integer(i64::from(limit)));
        let rows = registered_work_query(
            &transaction,
            &format!(
                "SELECT attempt_payload FROM work_attempts_v1
                 WHERE {authority_filter}{after_filter}
                 ORDER BY task_id, run_id, attempt_id
                 LIMIT ?{limit_placeholder}"
            ),
            params,
        )
        .map_err(|_| WorkAttemptStorageError::Unavailable)?;
        transaction
            .commit()
            .map_err(|_| WorkAttemptStorageError::Unavailable)?;
        let attempts = rows
            .rows
            .into_iter()
            .map(|row| {
                let payload =
                    exact_sql_text(&row.values, 0).ok_or(WorkAttemptStorageError::Unavailable)?;
                serde_json::from_str::<StoredWorkAttemptV1>(payload)
                    .map(|record| record.attempt)
                    .map_err(|_| WorkAttemptStorageError::Unavailable)
            })
            .collect::<Result<Vec<WorkAttemptV1>, _>>()?;
        Ok(WorkAttemptListPageV1 {
            attempts,
            remaining,
        })
    }
}

impl WorkSynthesisAdmissionStoragePort for WorkSqliteStorage {
    fn insert_synthesis(
        &self,
        authority: &WorkAuthority,
        record: &WorkSynthesisAdmissionRecordV1,
    ) -> Result<WorkSynthesisInsertOutcome, WorkAttemptStorageError> {
        let attempt = &record.result.attempt;
        let payload = serde_json::to_string(&StoredWorkAttemptV1 {
            attempt: attempt.clone(),
            synthesis: Some(record.clone()),
        })
        .map_err(|_| WorkAttemptStorageError::Unavailable)?;
        let transaction = self
            .handle
            .begin_immediate()
            .map_err(|_| WorkAttemptStorageError::Unavailable)?;
        if let Some(existing) = load_payload(&transaction, authority, attempt.identity())? {
            let _ = transaction.rollback();
            let existing: StoredWorkAttemptV1 = serde_json::from_str(&existing)
                .map_err(|_| WorkAttemptStorageError::Unavailable)?;
            return match existing.synthesis {
                Some(existing) if existing.request_digest == record.request_digest => Ok(
                    WorkSynthesisInsertOutcome::Replayed(Box::new(existing.result)),
                ),
                _ => Err(WorkAttemptStorageError::AttemptConflict),
            };
        }
        require_first_run_admission(&transaction, authority, attempt)?;
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
        Ok(WorkSynthesisInsertOutcome::Inserted)
    }

    fn load_synthesis(
        &self,
        authority: &WorkAuthority,
        identity: &WorkAttemptIdentityV1,
    ) -> Result<WorkSynthesisAdmissionRecordV1, WorkAttemptStorageError> {
        let payload = load_payload_from_handle(&self.handle, authority, identity)?
            .ok_or(WorkAttemptStorageError::NotFoundOrNotAuthorized)?;
        serde_json::from_str::<StoredWorkAttemptV1>(&payload)
            .map_err(|_| WorkAttemptStorageError::Unavailable)?
            .synthesis
            .ok_or(WorkAttemptStorageError::AttemptConflict)
    }
}

impl WorkAttemptEvidenceReadPort for WorkSqliteStorage {
    fn evidence_page(
        &self,
        authority: &WorkAuthority,
        start_after: Option<&WorkAttemptIdentityV1>,
        limit: u32,
    ) -> Result<WorkAttemptEvidencePageV1, WorkAttemptStorageError> {
        let authority_filter = "project_id = ?1 AND repository_id = ?2 AND worktree_id = ?3
               AND actor_id = ?4 AND policy_digest = ?5";
        let after_filter = if start_after.is_some() {
            " AND (task_id, run_id, attempt_id) > (?6, ?7, ?8)"
        } else {
            ""
        };
        let mut params = authority_params_owned(authority);
        if let Some(start_after) = start_after {
            params.extend(identity_params(start_after));
        }
        // One deferred transaction keeps the remaining count and the page on
        // the same consistent view of the attempt rows.
        let transaction = self
            .handle
            .begin_deferred()
            .map_err(|_| WorkAttemptStorageError::Unavailable)?;
        let counted = registered_work_query(
            &transaction,
            &format!(
                "SELECT COUNT(*) FROM work_attempts_v1 WHERE {authority_filter}{after_filter}"
            ),
            params.clone(),
        )
        .map_err(|_| WorkAttemptStorageError::Unavailable)?;
        let remaining = counted
            .rows
            .first()
            .and_then(|row| exact_sql_integer(&row.values, 0))
            .and_then(|value| u32::try_from(value).ok())
            .ok_or(WorkAttemptStorageError::Unavailable)?;
        let limit_placeholder = params.len() + 1;
        params.push(ExactSqlValue::Integer(i64::from(limit)));
        let rows = registered_work_query(
            &transaction,
            &format!(
                "SELECT attempt_payload, evidence_payload FROM work_attempts_v1
                 WHERE {authority_filter}{after_filter}
                 ORDER BY task_id, run_id, attempt_id
                 LIMIT ?{limit_placeholder}"
            ),
            params,
        )
        .map_err(|_| WorkAttemptStorageError::Unavailable)?;
        transaction
            .commit()
            .map_err(|_| WorkAttemptStorageError::Unavailable)?;
        let rows = rows
            .rows
            .into_iter()
            .map(|row| {
                let payload =
                    exact_sql_text(&row.values, 0).ok_or(WorkAttemptStorageError::Unavailable)?;
                let attempt = serde_json::from_str::<StoredWorkAttemptV1>(payload)
                    .map_err(|_| WorkAttemptStorageError::Unavailable)?
                    .attempt;
                let evidence = match exact_sql_text(&row.values, 1) {
                    None => None,
                    Some(evidence_payload) => Some(
                        serde_json::from_str::<WorkAttemptEvidenceRecordV1>(evidence_payload)
                            .map_err(|_| WorkAttemptStorageError::Unavailable)?,
                    ),
                };
                Ok(WorkAttemptEvidenceRowV1 {
                    identity: attempt.identity().clone(),
                    artifacts: attempt.artifacts().to_vec(),
                    evidence,
                })
            })
            .collect::<Result<Vec<_>, WorkAttemptStorageError>>()?;
        Ok(WorkAttemptEvidencePageV1 { rows, remaining })
    }
}

fn load_payload_from_handle(
    handle: &crate::exact_sql::ExactSqlHandle,
    authority: &WorkAuthority,
    identity: &WorkAttemptIdentityV1,
) -> Result<Option<String>, WorkAttemptStorageError> {
    let rows = registered_work_query(
        handle,
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
        .and_then(|row| exact_sql_text(&row.values, 0).map(str::to_owned)))
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

/// A run is admitted by its first durable attempt. Every later attempt is
/// inserted in the same immediate transaction only when it carries the first
/// attempt's immutable deadline and topology, so a caller cannot replace the
/// run authority through a lexically earlier attempt ID.
fn require_first_run_admission(
    transaction: &crate::exact_sql::ExactSqlTransaction,
    authority: &WorkAuthority,
    attempt: &WorkAttemptV1,
) -> Result<(), WorkAttemptStorageError> {
    let rows = registered_work_query(
        transaction,
        "SELECT attempt_payload FROM work_attempts_v1
         WHERE project_id = ?1 AND repository_id = ?2 AND worktree_id = ?3
           AND actor_id = ?4 AND policy_digest = ?5
           AND task_id = ?6 AND run_id = ?7
         ORDER BY rowid
         LIMIT 1",
        authority_params_owned(authority)
            .into_iter()
            .chain([
                ExactSqlValue::Text(attempt.identity().task_id().as_str().to_owned()),
                ExactSqlValue::Text(attempt.identity().run_id().as_str().to_owned()),
            ])
            .collect(),
    )
    .map_err(|_| WorkAttemptStorageError::Unavailable)?;
    let Some(payload) = rows
        .rows
        .first()
        .and_then(|row| exact_sql_text(&row.values, 0))
    else {
        return Ok(());
    };
    let first: StoredWorkAttemptV1 =
        serde_json::from_str(payload).map_err(|_| WorkAttemptStorageError::Unavailable)?;
    if first.attempt.execution().deadline() == attempt.execution().deadline()
        && first.attempt.execution().execution_snapshot().topology()
            == attempt.execution().execution_snapshot().topology()
    {
        return Ok(());
    }
    Err(WorkAttemptStorageError::RunAdmissionConflict)
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
