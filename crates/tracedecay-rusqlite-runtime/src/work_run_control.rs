//! Durable Work run-control rows: compare-and-swap publication of the
//! monotonic control authority over the registered exact-SQL channel.
//!
//! The run-control aggregate is the only Work row that is *derived* from
//! another table before it exists: a run is known through its attempts, so
//! [`run_admission`](WorkRunControlStoragePort::run_admission) reads
//! `work_attempts_v1` to answer whether the run is real, what deadline it was
//! admitted under, and which of its attempts are still live. Nothing here
//! invents a deadline; it is read back out of the attempt's own pinned
//! execution snapshot.

use tracedecay_application::{
    WorkAttemptStorageError, WorkRunAdmissionV1, WorkRunControlFrontierV1,
    WorkRunControlStorageError, WorkRunControlStoragePort, WorkRunLiveAttemptV1,
    WorkflowRunStorageError, WorkflowRunStoragePort,
};
use tracedecay_domain::{
    RunId, TaskId, UtcMicros, WorkAttemptV1, WorkAuthority, WorkBlockedIntervalClosureV1,
    WorkBlockedIntervalReceiptV1, WorkRunControlAuthorityV1, WorkRunControlStateV1,
    WorkRunControlV1,
};

use crate::exact_sql::{ExactSqlTransaction, ExactSqlValue};
use crate::work::{
    RegisteredWorkQuery, WorkSqliteStorage, authority_params_owned, exact_sql_integer,
    exact_sql_statement, exact_sql_text, registered_work_query,
};
use crate::workflow::WorkflowSqliteAuthority;

impl WorkRunControlStoragePort for WorkSqliteStorage {
    fn run_control_frontier(
        &self,
        authority: &WorkAuthority,
        task_id: &TaskId,
        run_id: &RunId,
    ) -> Result<Option<WorkRunControlFrontierV1>, WorkRunControlStorageError> {
        let transaction = self
            .handle
            .begin_immediate()
            .map_err(|_| WorkRunControlStorageError::Unavailable)?;
        let frontier = run_control_frontier_from(&transaction, authority, task_id, run_id);
        let _ = transaction.rollback();
        frontier
    }

    fn run_admission(
        &self,
        authority: &WorkAuthority,
        task_id: &TaskId,
        run_id: &RunId,
    ) -> Result<Option<WorkRunAdmissionV1>, WorkRunControlStorageError> {
        run_admission_from(&self.handle, authority, task_id, run_id)
    }

    fn workflow_bound_live_attempts(
        &self,
        authority: &WorkAuthority,
        task_id: &TaskId,
        run_id: &RunId,
    ) -> Result<Vec<WorkRunLiveAttemptV1>, WorkRunControlStorageError> {
        let rows = registered_work_query(
            &self.handle,
            "SELECT attempt_payload FROM work_attempts_v1
             WHERE project_id = ?1 AND repository_id = ?2 AND worktree_id = ?3
               AND actor_id = ?4 AND policy_digest = ?5
               AND task_id = ?6 AND run_id = ?7
               AND terminal = 0
             ORDER BY rowid",
            authority_params_owned(authority)
                .into_iter()
                .chain(run_params(task_id, run_id))
                .collect(),
        )
        .map_err(|_| WorkRunControlStorageError::Unavailable)?;
        let attempts = rows
            .rows
            .iter()
            .map(|row| {
                let payload = exact_sql_text(&row.values, 0)
                    .ok_or(WorkRunControlStorageError::Unavailable)?;
                attempt_from_payload(payload)
            })
            .collect::<Result<Vec<_>, _>>()?;
        let workflow = WorkflowSqliteAuthority::from_registered(self.handle.clone())
            .map_err(|_| WorkRunControlStorageError::Unavailable)?;
        let projection = match WorkflowRunStoragePort::projection(&workflow, run_id) {
            Ok(projection) if projection.run_id() == run_id => Some(projection),
            Ok(_) => return Err(WorkRunControlStorageError::Unavailable),
            Err(WorkflowRunStorageError::NotFound) => None,
            Err(
                WorkflowRunStorageError::VersionConflict
                | WorkflowRunStorageError::IdempotencyConflict
                | WorkflowRunStorageError::InvalidHistory
                | WorkflowRunStorageError::Unavailable,
            ) => return Err(WorkRunControlStorageError::Unavailable),
        };
        attempts
            .into_iter()
            .map(|attempt| {
                let step_id = match projection.as_ref() {
                    None => None,
                    Some(projection) => {
                        let mut matching_steps = projection
                            .fan_out_plans()
                            .values()
                            .filter(|plan| {
                                plan.children
                                    .iter()
                                    .any(|child| &child.attempt_identity == attempt.identity())
                            })
                            .map(|plan| plan.step_id.clone());
                        let step = matching_steps.next();
                        if matching_steps.next().is_some() {
                            return Err(WorkRunControlStorageError::Unavailable);
                        }
                        step
                    }
                };
                Ok(WorkRunLiveAttemptV1 {
                    attempt_id: attempt.identity().attempt_id().clone(),
                    step_id,
                })
            })
            .collect()
    }

    fn load_run_control(
        &self,
        authority: &WorkAuthority,
        task_id: &TaskId,
        run_id: &RunId,
    ) -> Result<Option<WorkRunControlV1>, WorkRunControlStorageError> {
        load_run_control_from(&self.handle, authority, task_id, run_id)
    }

    fn publish_run_control(
        &self,
        authority: &WorkAuthority,
        expected: Option<WorkRunControlAuthorityV1>,
        next: &WorkRunControlV1,
        blocked_intervals: &[WorkBlockedIntervalReceiptV1],
    ) -> Result<(), WorkRunControlStorageError> {
        let transaction = self
            .handle
            .begin_immediate()
            .map_err(|_| WorkRunControlStorageError::Unavailable)?;
        if let Err(error) =
            publish_run_control_tx(&transaction, authority, expected, next, blocked_intervals)
        {
            let _ = transaction.rollback();
            return Err(error);
        }
        transaction
            .commit()
            .map_err(|_| WorkRunControlStorageError::Unavailable)?;
        Ok(())
    }

    fn publish_run_control_at_frontier(
        &self,
        authority: &WorkAuthority,
        expected: &WorkRunControlFrontierV1,
        next: &WorkRunControlV1,
        blocked_intervals: &[WorkBlockedIntervalReceiptV1],
    ) -> Result<(), WorkRunControlStorageError> {
        let transaction = self
            .handle
            .begin_immediate()
            .map_err(|_| WorkRunControlStorageError::Unavailable)?;
        let current =
            run_control_frontier_from(&transaction, authority, next.task_id(), next.run_id())?;
        if current.as_ref() != Some(expected) {
            let _ = transaction.rollback();
            return Err(WorkRunControlStorageError::AuthorityConflict);
        }
        if let Err(error) = publish_run_control_tx(
            &transaction,
            authority,
            expected.control.as_ref().map(WorkRunControlV1::authority),
            next,
            blocked_intervals,
        ) {
            let _ = transaction.rollback();
            return Err(error);
        }
        transaction
            .commit()
            .map_err(|_| WorkRunControlStorageError::Unavailable)?;
        Ok(())
    }

    fn open_blocked_intervals(
        &self,
        authority: &WorkAuthority,
        task_id: &TaskId,
        run_id: &RunId,
    ) -> Result<Vec<WorkBlockedIntervalReceiptV1>, WorkRunControlStorageError> {
        open_blocked_intervals_from(&self.handle, authority, task_id, run_id)
    }

    fn next_settled_blocked_intervals_for_observation(
        &self,
        authority: &WorkAuthority,
        limit: u32,
    ) -> Result<Vec<WorkBlockedIntervalReceiptV1>, WorkRunControlStorageError> {
        let transaction = self
            .handle
            .begin_immediate()
            .map_err(|_| WorkRunControlStorageError::Unavailable)?;
        let cursor = load_blocked_interval_observation_cursor(&transaction, authority)?;
        let mut receipts = settled_blocked_interval_observation_page(
            &transaction,
            authority,
            cursor.as_ref(),
            limit,
        )?;
        if receipts.is_empty() && cursor.is_some() {
            receipts =
                settled_blocked_interval_observation_page(&transaction, authority, None, limit)?;
        }
        let Some(last) = receipts.last() else {
            let _ = transaction.rollback();
            return Ok(Vec::new());
        };
        persist_blocked_interval_observation_cursor(&transaction, authority, last)?;
        transaction
            .commit()
            .map_err(|_| WorkRunControlStorageError::Unavailable)?;
        Ok(receipts)
    }

    fn mark_settled_blocked_interval_durable(
        &self,
        authority: &WorkAuthority,
        receipt: &WorkBlockedIntervalReceiptV1,
    ) -> Result<(), WorkRunControlStorageError> {
        let payload =
            serde_json::to_string(receipt).map_err(|_| WorkRunControlStorageError::Unavailable)?;
        let transaction = self
            .handle
            .begin_immediate()
            .map_err(|_| WorkRunControlStorageError::Unavailable)?;
        let changed = transaction
            .execute(
                exact_sql_statement(
                    "UPDATE work_blocked_intervals_v1
                     SET observability_durable = 1
                     WHERE project_id = ?1 AND repository_id = ?2 AND worktree_id = ?3
                       AND actor_id = ?4 AND policy_digest = ?5
                       AND task_id = ?6 AND run_id = ?7 AND attempt_id = ?8 AND step_id = ?9
                       AND cause_authority_version = ?10 AND interval_revision = ?11
                       AND settled = 1 AND observability_durable = 0 AND receipt_payload = ?12",
                    authority_params_owned(authority)
                        .into_iter()
                        .chain(blocked_identity_params(receipt))
                        .chain([
                            ExactSqlValue::Integer(
                                i64::try_from(receipt.cause().authority().get())
                                    .map_err(|_| WorkRunControlStorageError::Unavailable)?,
                            ),
                            ExactSqlValue::Integer(i64::from(receipt.interval_revision())),
                            ExactSqlValue::Text(payload),
                        ])
                        .collect(),
                )
                .map_err(|_| WorkRunControlStorageError::Unavailable)?,
            )
            .map_err(|_| WorkRunControlStorageError::Unavailable)?;
        if changed.changed_rows != 1 {
            let _ = transaction.rollback();
            return Err(WorkRunControlStorageError::AuthorityConflict);
        }
        transaction
            .commit()
            .map(|_| ())
            .map_err(|_| WorkRunControlStorageError::Unavailable)
    }
}

fn run_control_frontier_from(
    source: &impl RegisteredWorkQuery,
    authority: &WorkAuthority,
    task_id: &TaskId,
    run_id: &RunId,
) -> Result<Option<WorkRunControlFrontierV1>, WorkRunControlStorageError> {
    let Some(admission) = run_admission_from(source, authority, task_id, run_id)? else {
        return Ok(None);
    };
    Ok(Some(WorkRunControlFrontierV1 {
        admission,
        control: load_run_control_from(source, authority, task_id, run_id)?,
        open_blocked_intervals: open_blocked_intervals_from(source, authority, task_id, run_id)?,
    }))
}

fn run_admission_from(
    source: &impl RegisteredWorkQuery,
    authority: &WorkAuthority,
    task_id: &TaskId,
    run_id: &RunId,
) -> Result<Option<WorkRunAdmissionV1>, WorkRunControlStorageError> {
    let rows = registered_work_query(
        source,
        "SELECT attempt_payload, terminal FROM work_attempts_v1
         WHERE project_id = ?1 AND repository_id = ?2 AND worktree_id = ?3
           AND actor_id = ?4 AND policy_digest = ?5
           AND task_id = ?6 AND run_id = ?7
         ORDER BY rowid",
        authority_params_owned(authority)
            .into_iter()
            .chain(run_params(task_id, run_id))
            .collect(),
    )
    .map_err(|_| WorkRunControlStorageError::Unavailable)?;
    if rows.rows.is_empty() {
        return Ok(None);
    }
    let mut deadline: Option<UtcMicros> = None;
    let mut topology = None;
    let mut live_attempts = Vec::new();
    let mut total_attempts = 0u32;
    for row in &rows.rows {
        let payload =
            exact_sql_text(&row.values, 0).ok_or(WorkRunControlStorageError::Unavailable)?;
        let attempt = attempt_from_payload(payload)?;
        let terminal =
            exact_sql_integer(&row.values, 1).ok_or(WorkRunControlStorageError::Unavailable)?;
        match (&deadline, &topology) {
            (None, None) => {
                deadline = Some(attempt.execution().deadline());
                topology = Some(attempt.execution().execution_snapshot().topology().clone());
            }
            (Some(admitted_deadline), Some(admitted_topology))
                if admitted_deadline == &attempt.execution().deadline()
                    && admitted_topology == attempt.execution().execution_snapshot().topology() => {
            }
            (Some(_), Some(_)) => return Err(WorkRunControlStorageError::AuthorityConflict),
            _ => return Err(WorkRunControlStorageError::Unavailable),
        }
        if terminal == 0 {
            live_attempts.push(attempt.identity().attempt_id().clone());
        }
        total_attempts = total_attempts
            .checked_add(1)
            .ok_or(WorkRunControlStorageError::Unavailable)?;
    }
    Ok(Some(WorkRunAdmissionV1 {
        deadline: deadline.ok_or(WorkRunControlStorageError::Unavailable)?,
        live_attempts,
        total_attempts,
    }))
}

fn load_run_control_from(
    source: &impl RegisteredWorkQuery,
    authority: &WorkAuthority,
    task_id: &TaskId,
    run_id: &RunId,
) -> Result<Option<WorkRunControlV1>, WorkRunControlStorageError> {
    let rows = registered_work_query(
        source,
        "SELECT control_payload FROM work_run_controls_v1
         WHERE project_id = ?1 AND repository_id = ?2 AND worktree_id = ?3
           AND actor_id = ?4 AND policy_digest = ?5
           AND task_id = ?6 AND run_id = ?7",
        authority_params_owned(authority)
            .into_iter()
            .chain(run_params(task_id, run_id))
            .collect(),
    )
    .map_err(|_| WorkRunControlStorageError::Unavailable)?;
    let Some(payload) = rows
        .rows
        .first()
        .and_then(|row| exact_sql_text(&row.values, 0))
    else {
        return Ok(None);
    };
    serde_json::from_str(payload)
        .map(Some)
        .map_err(|_| WorkRunControlStorageError::Unavailable)
}

fn open_blocked_intervals_from(
    source: &impl RegisteredWorkQuery,
    authority: &WorkAuthority,
    task_id: &TaskId,
    run_id: &RunId,
) -> Result<Vec<WorkBlockedIntervalReceiptV1>, WorkRunControlStorageError> {
    let rows = registered_work_query(
        source,
        "SELECT receipt_payload FROM work_blocked_intervals_v1
         WHERE project_id = ?1 AND repository_id = ?2 AND worktree_id = ?3
           AND actor_id = ?4 AND policy_digest = ?5
           AND task_id = ?6 AND run_id = ?7 AND settled = 0
         ORDER BY started_at, attempt_id, step_id",
        authority_params_owned(authority)
            .into_iter()
            .chain(run_params(task_id, run_id))
            .collect(),
    )
    .map_err(|_| WorkRunControlStorageError::Unavailable)?;
    rows.rows
        .iter()
        .map(|row| decode_blocked_interval(row.values.first()))
        .collect()
}

fn publish_run_control_tx(
    transaction: &ExactSqlTransaction,
    authority: &WorkAuthority,
    expected: Option<WorkRunControlAuthorityV1>,
    next: &WorkRunControlV1,
    blocked_intervals: &[WorkBlockedIntervalReceiptV1],
) -> Result<(), WorkRunControlStorageError> {
    let payload =
        serde_json::to_string(next).map_err(|_| WorkRunControlStorageError::Unavailable)?;
    let authority_version = i64::try_from(next.authority().get())
        .map_err(|_| WorkRunControlStorageError::Unavailable)?;
    let changed = match expected {
        None => transaction
            .execute(
                exact_sql_statement(
                    "INSERT OR IGNORE INTO work_run_controls_v1 (
                        project_id, repository_id, worktree_id, actor_id, policy_digest,
                        task_id, run_id, state, authority_version, control_payload
                     ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
                    authority_params_owned(authority)
                        .into_iter()
                        .chain(run_params(next.task_id(), next.run_id()))
                        .chain([
                            ExactSqlValue::Text(state_text(next.state())),
                            ExactSqlValue::Integer(authority_version),
                            ExactSqlValue::Text(payload),
                        ])
                        .collect(),
                )
                .map_err(|_| WorkRunControlStorageError::Unavailable)?,
            )
            .map_err(|_| WorkRunControlStorageError::Unavailable)?,
        Some(expected) => {
            let expected_version = i64::try_from(expected.get())
                .map_err(|_| WorkRunControlStorageError::Unavailable)?;
            transaction
                .execute(
                    exact_sql_statement(
                        "UPDATE work_run_controls_v1 SET
                            state = ?8, authority_version = ?9, control_payload = ?10
                         WHERE project_id = ?1 AND repository_id = ?2 AND worktree_id = ?3
                           AND actor_id = ?4 AND policy_digest = ?5
                           AND task_id = ?6 AND run_id = ?7
                           AND authority_version = ?11",
                        authority_params_owned(authority)
                            .into_iter()
                            .chain(run_params(next.task_id(), next.run_id()))
                            .chain([
                                ExactSqlValue::Text(state_text(next.state())),
                                ExactSqlValue::Integer(authority_version),
                                ExactSqlValue::Text(payload),
                                ExactSqlValue::Integer(expected_version),
                            ])
                            .collect(),
                    )
                    .map_err(|_| WorkRunControlStorageError::Unavailable)?,
                )
                .map_err(|_| WorkRunControlStorageError::Unavailable)?
        }
    };
    if changed.changed_rows != 1 {
        return Err(WorkRunControlStorageError::AuthorityConflict);
    }
    persist_blocked_intervals(
        transaction,
        authority,
        next.task_id(),
        next.run_id(),
        blocked_intervals,
    )
}

fn load_blocked_interval_observation_cursor(
    transaction: &ExactSqlTransaction,
    authority: &WorkAuthority,
) -> Result<Option<BlockedIntervalObservationCursor>, WorkRunControlStorageError> {
    let rows = registered_work_query(
        transaction,
        "SELECT started_at, task_id, run_id, attempt_id, step_id, cause_authority_version
         FROM work_blocked_interval_observation_cursors_v1
         WHERE project_id = ?1 AND repository_id = ?2 AND worktree_id = ?3
           AND actor_id = ?4 AND policy_digest = ?5",
        authority_params_owned(authority),
    )
    .map_err(|_| WorkRunControlStorageError::Unavailable)?;
    let Some(row) = rows.rows.first() else {
        return Ok(None);
    };
    BlockedIntervalObservationCursor::from_values(&row.values)
        .map(Some)
        .ok_or(WorkRunControlStorageError::Unavailable)
}

fn settled_blocked_interval_observation_page(
    transaction: &ExactSqlTransaction,
    authority: &WorkAuthority,
    after: Option<&BlockedIntervalObservationCursor>,
    limit: u32,
) -> Result<Vec<WorkBlockedIntervalReceiptV1>, WorkRunControlStorageError> {
    let (predicate, parameters) = match after {
        Some(after) => (
            " AND (
                    started_at > ?6
                 OR (started_at = ?6 AND task_id > ?7)
                 OR (started_at = ?6 AND task_id = ?7 AND run_id > ?8)
                 OR (started_at = ?6 AND task_id = ?7 AND run_id = ?8 AND attempt_id > ?9)
                 OR (started_at = ?6 AND task_id = ?7 AND run_id = ?8 AND attempt_id = ?9 AND step_id > ?10)
                 OR (started_at = ?6 AND task_id = ?7 AND run_id = ?8 AND attempt_id = ?9 AND step_id = ?10 AND cause_authority_version > ?11)
                )",
            after.values(),
        ),
        None => ("", Vec::new()),
    };
    let parameter_index = if after.is_some() { 12 } else { 6 };
    let statement = format!(
        "SELECT receipt_payload FROM work_blocked_intervals_v1
         WHERE project_id = ?1 AND repository_id = ?2 AND worktree_id = ?3
           AND actor_id = ?4 AND policy_digest = ?5
           AND settled = 1 AND observability_durable = 0{predicate}
         ORDER BY started_at, task_id, run_id, attempt_id, step_id, cause_authority_version
         LIMIT ?{parameter_index}"
    );
    let rows = registered_work_query(
        transaction,
        &statement,
        authority_params_owned(authority)
            .into_iter()
            .chain(parameters)
            .chain([ExactSqlValue::Integer(i64::from(limit))])
            .collect(),
    )
    .map_err(|_| WorkRunControlStorageError::Unavailable)?;
    rows.rows
        .iter()
        .map(|row| decode_blocked_interval(row.values.first()))
        .collect()
}

fn persist_blocked_interval_observation_cursor(
    transaction: &ExactSqlTransaction,
    authority: &WorkAuthority,
    receipt: &WorkBlockedIntervalReceiptV1,
) -> Result<(), WorkRunControlStorageError> {
    let cursor = BlockedIntervalObservationCursor::from_receipt(receipt)?;
    transaction
        .execute(
            exact_sql_statement(
                "INSERT INTO work_blocked_interval_observation_cursors_v1 (
                    project_id, repository_id, worktree_id, actor_id, policy_digest,
                    started_at, task_id, run_id, attempt_id, step_id, cause_authority_version
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
                 ON CONFLICT(project_id, repository_id, worktree_id, actor_id, policy_digest)
                 DO UPDATE SET
                    started_at = excluded.started_at,
                    task_id = excluded.task_id,
                    run_id = excluded.run_id,
                    attempt_id = excluded.attempt_id,
                    step_id = excluded.step_id,
                    cause_authority_version = excluded.cause_authority_version",
                authority_params_owned(authority)
                    .into_iter()
                    .chain(cursor.values())
                    .collect(),
            )
            .map_err(|_| WorkRunControlStorageError::Unavailable)?,
        )
        .map_err(|_| WorkRunControlStorageError::Unavailable)?;
    Ok(())
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct BlockedIntervalObservationCursor {
    started_at: i64,
    task_id: String,
    run_id: String,
    attempt_id: String,
    step_id: String,
    cause_authority_version: i64,
}

impl BlockedIntervalObservationCursor {
    fn from_receipt(
        receipt: &WorkBlockedIntervalReceiptV1,
    ) -> Result<Self, WorkRunControlStorageError> {
        let cause_authority_version = i64::try_from(receipt.cause().authority().get())
            .map_err(|_| WorkRunControlStorageError::Unavailable)?;
        Ok(Self {
            started_at: receipt.started_at().0,
            task_id: receipt.identity().task_id().as_str().to_owned(),
            run_id: receipt.identity().run_id().as_str().to_owned(),
            attempt_id: receipt.identity().attempt_id().as_str().to_owned(),
            step_id: receipt.identity().step_id().as_str().to_owned(),
            cause_authority_version,
        })
    }

    fn from_values(values: &[ExactSqlValue]) -> Option<Self> {
        Some(Self {
            started_at: exact_sql_integer(values, 0)?,
            task_id: exact_sql_text(values, 1)?.to_owned(),
            run_id: exact_sql_text(values, 2)?.to_owned(),
            attempt_id: exact_sql_text(values, 3)?.to_owned(),
            step_id: exact_sql_text(values, 4)?.to_owned(),
            cause_authority_version: exact_sql_integer(values, 5)?,
        })
    }

    fn values(&self) -> Vec<ExactSqlValue> {
        vec![
            ExactSqlValue::Integer(self.started_at),
            ExactSqlValue::Text(self.task_id.clone()),
            ExactSqlValue::Text(self.run_id.clone()),
            ExactSqlValue::Text(self.attempt_id.clone()),
            ExactSqlValue::Text(self.step_id.clone()),
            ExactSqlValue::Integer(self.cause_authority_version),
        ]
    }
}

fn persist_blocked_intervals(
    transaction: &ExactSqlTransaction,
    authority: &WorkAuthority,
    task_id: &TaskId,
    run_id: &RunId,
    receipts: &[WorkBlockedIntervalReceiptV1],
) -> Result<(), WorkRunControlStorageError> {
    for receipt in receipts {
        if receipt.identity().task_id() != task_id || receipt.identity().run_id() != run_id {
            return Err(WorkRunControlStorageError::AuthorityConflict);
        }
        let payload =
            serde_json::to_string(receipt).map_err(|_| WorkRunControlStorageError::Unavailable)?;
        let cause_authority = i64::try_from(receipt.cause().authority().get())
            .map_err(|_| WorkRunControlStorageError::Unavailable)?;
        let started_at = receipt.started_at().0;
        let changed = if receipt.is_settled() {
            let previous_revision = receipt
                .interval_revision()
                .checked_sub(1)
                .ok_or(WorkRunControlStorageError::AuthorityConflict)?;
            transaction
                .execute(
                    exact_sql_statement(
                        "UPDATE work_blocked_intervals_v1 SET
                            interval_revision = ?12, settled = 1, observability_durable = 0,
                            receipt_payload = ?13
                         WHERE project_id = ?1 AND repository_id = ?2 AND worktree_id = ?3
                           AND actor_id = ?4 AND policy_digest = ?5
                           AND task_id = ?6 AND run_id = ?7 AND attempt_id = ?8 AND step_id = ?9
                           AND cause_authority_version = ?10 AND started_at = ?11
                           AND settled = 0 AND interval_revision = ?14",
                        authority_params_owned(authority)
                            .into_iter()
                            .chain(blocked_identity_params(receipt))
                            .chain([
                                ExactSqlValue::Integer(cause_authority),
                                ExactSqlValue::Integer(started_at),
                                ExactSqlValue::Integer(i64::from(receipt.interval_revision())),
                                ExactSqlValue::Text(payload),
                                ExactSqlValue::Integer(i64::from(previous_revision)),
                            ])
                            .collect(),
                    )
                    .map_err(|_| WorkRunControlStorageError::Unavailable)?,
                )
                .map_err(|_| WorkRunControlStorageError::Unavailable)?
        } else {
            if receipt.interval_revision() != 1 {
                return Err(WorkRunControlStorageError::AuthorityConflict);
            }
            transaction
                .execute(
                    exact_sql_statement(
                        "INSERT INTO work_blocked_intervals_v1 (
                            project_id, repository_id, worktree_id, actor_id, policy_digest,
                            task_id, run_id, attempt_id, step_id, cause_authority_version,
                            started_at, interval_revision, settled, observability_durable,
                            receipt_payload
                         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, 0, 0, ?13)",
                        authority_params_owned(authority)
                            .into_iter()
                            .chain(blocked_identity_params(receipt))
                            .chain([
                                ExactSqlValue::Integer(cause_authority),
                                ExactSqlValue::Integer(started_at),
                                ExactSqlValue::Integer(i64::from(receipt.interval_revision())),
                                ExactSqlValue::Text(payload),
                            ])
                            .collect(),
                    )
                    .map_err(|_| WorkRunControlStorageError::Unavailable)?,
                )
                .map_err(|_| WorkRunControlStorageError::Unavailable)?
        };
        if changed.changed_rows != 1 {
            return Err(WorkRunControlStorageError::AuthorityConflict);
        }
    }
    Ok(())
}

fn decode_blocked_interval(
    value: Option<&ExactSqlValue>,
) -> Result<WorkBlockedIntervalReceiptV1, WorkRunControlStorageError> {
    let Some(ExactSqlValue::Text(payload)) = value else {
        return Err(WorkRunControlStorageError::Unavailable);
    };
    serde_json::from_str(payload).map_err(|_| WorkRunControlStorageError::Unavailable)
}

fn blocked_identity_params(receipt: &WorkBlockedIntervalReceiptV1) -> [ExactSqlValue; 4] {
    [
        ExactSqlValue::Text(receipt.identity().task_id().as_str().to_owned()),
        ExactSqlValue::Text(receipt.identity().run_id().as_str().to_owned()),
        ExactSqlValue::Text(receipt.identity().attempt_id().as_str().to_owned()),
        ExactSqlValue::Text(receipt.identity().step_id().as_str().to_owned()),
    ]
}

/// Closes every open interval for an attempt inside that attempt's own fenced
/// terminal CAS. The interval receipt cannot survive a terminal attempt with
/// no end instant, and a crash can commit neither half independently.
pub(crate) fn close_blocked_intervals_on_terminal_attempt(
    transaction: &ExactSqlTransaction,
    authority: &WorkAuthority,
    next: &WorkAttemptV1,
) -> Result<(), WorkAttemptStorageError> {
    if !next.is_terminal() {
        return Ok(());
    }
    let ended_at = next
        .terminal()
        .map(|terminal| terminal.observed_at())
        .ok_or(WorkAttemptStorageError::Unavailable)?;
    let identity = next.identity();
    let rows = registered_work_query(
        transaction,
        "SELECT receipt_payload FROM work_blocked_intervals_v1
         WHERE project_id = ?1 AND repository_id = ?2 AND worktree_id = ?3
           AND actor_id = ?4 AND policy_digest = ?5
           AND task_id = ?6 AND run_id = ?7 AND attempt_id = ?8
           AND settled = 0
         ORDER BY started_at, step_id",
        authority_params_owned(authority)
            .into_iter()
            .chain([
                ExactSqlValue::Text(identity.task_id().as_str().to_owned()),
                ExactSqlValue::Text(identity.run_id().as_str().to_owned()),
                ExactSqlValue::Text(identity.attempt_id().as_str().to_owned()),
            ])
            .collect(),
    )
    .map_err(|_| WorkAttemptStorageError::Unavailable)?;
    let settled = rows
        .rows
        .iter()
        .map(|row| {
            decode_blocked_interval(row.values.first())
                .map_err(|_| WorkAttemptStorageError::Unavailable)?
                .close(ended_at, WorkBlockedIntervalClosureV1::AttemptTerminal)
                .map_err(|_| WorkAttemptStorageError::Unavailable)
        })
        .collect::<Result<Vec<_>, _>>()?;
    persist_blocked_intervals(
        transaction,
        authority,
        identity.task_id(),
        identity.run_id(),
        &settled,
    )
    .map_err(|error| match error {
        WorkRunControlStorageError::AuthorityConflict => WorkAttemptStorageError::FenceConflict,
        WorkRunControlStorageError::NotFoundOrNotAuthorized
        | WorkRunControlStorageError::Unavailable => WorkAttemptStorageError::Unavailable,
    })
}

/// The composite attempt payload is the canonical Task 1 persistence shape:
/// the live attempt is stored with an optional immutable synthesis admission.
/// Run control deliberately reads only the live attempt, because synthesis
/// replay material cannot change a run's deadline or topology authority.
fn attempt_from_payload(payload: &str) -> Result<WorkAttemptV1, WorkRunControlStorageError> {
    #[derive(serde::Deserialize)]
    #[serde(deny_unknown_fields)]
    struct StoredAttempt {
        attempt: WorkAttemptV1,
        #[serde(rename = "synthesis")]
        _synthesis: Option<serde_json::Value>,
    }

    serde_json::from_str::<StoredAttempt>(payload)
        .map(|record| record.attempt)
        .map_err(|_| WorkRunControlStorageError::Unavailable)
}

fn run_params(task_id: &TaskId, run_id: &RunId) -> [ExactSqlValue; 2] {
    [
        ExactSqlValue::Text(task_id.as_str().to_owned()),
        ExactSqlValue::Text(run_id.as_str().to_owned()),
    ]
}

fn state_text(state: WorkRunControlStateV1) -> String {
    match state {
        WorkRunControlStateV1::Running => "running",
        WorkRunControlStateV1::Paused => "paused",
    }
    .to_owned()
}
