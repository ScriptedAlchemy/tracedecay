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
    WorkRunAdmissionV1, WorkRunControlStorageError, WorkRunControlStoragePort,
};
use tracedecay_domain::{
    RunId, TaskId, UtcMicros, WorkAttemptV1, WorkAuthority, WorkRunControlAuthorityV1,
    WorkRunControlStateV1, WorkRunControlV1,
};

use crate::exact_sql::ExactSqlValue;
use crate::work::{
    WorkSqliteStorage, authority_params_owned, exact_sql_integer, exact_sql_statement,
    exact_sql_text, registered_work_query,
};

impl WorkRunControlStoragePort for WorkSqliteStorage {
    fn run_admission(
        &self,
        authority: &WorkAuthority,
        task_id: &TaskId,
        run_id: &RunId,
    ) -> Result<Option<WorkRunAdmissionV1>, WorkRunControlStorageError> {
        let rows = registered_work_query(
            &self.handle,
            "SELECT attempt_payload, terminal FROM work_attempts_v1
             WHERE project_id = ?1 AND repository_id = ?2 AND worktree_id = ?3
               AND actor_id = ?4 AND policy_digest = ?5
               AND task_id = ?6 AND run_id = ?7
             ORDER BY attempt_id",
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
        let mut live_attempts = Vec::new();
        let mut total_attempts = 0u32;
        for row in &rows.rows {
            let payload =
                exact_sql_text(&row.values, 0).ok_or(WorkRunControlStorageError::Unavailable)?;
            let attempt: WorkAttemptV1 = serde_json::from_str(payload)
                .map_err(|_| WorkRunControlStorageError::Unavailable)?;
            let terminal = exact_sql_integer(&row.values, 1)
                .ok_or(WorkRunControlStorageError::Unavailable)?;
            // The earliest attempt in stable order carries the admitted
            // deadline: later attempts of the same run were admitted under the
            // same snapshot, and taking the first one keeps the answer stable
            // as the run grows.
            if deadline.is_none() {
                deadline = Some(attempt.execution().deadline());
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

    fn load_run_control(
        &self,
        authority: &WorkAuthority,
        task_id: &TaskId,
        run_id: &RunId,
    ) -> Result<Option<WorkRunControlV1>, WorkRunControlStorageError> {
        let rows = registered_work_query(
            &self.handle,
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

    fn publish_run_control(
        &self,
        authority: &WorkAuthority,
        expected: Option<WorkRunControlAuthorityV1>,
        next: &WorkRunControlV1,
    ) -> Result<(), WorkRunControlStorageError> {
        let payload =
            serde_json::to_string(next).map_err(|_| WorkRunControlStorageError::Unavailable)?;
        let authority_version = i64::try_from(next.authority().get())
            .map_err(|_| WorkRunControlStorageError::Unavailable)?;
        let transaction = self
            .handle
            .begin_immediate()
            .map_err(|_| WorkRunControlStorageError::Unavailable)?;

        let changed = match expected {
            // First publication: the INSERT itself is the compare-and-swap. A
            // concurrent writer that got there first makes this a conflict
            // rather than an overwrite.
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
            let _ = transaction.rollback();
            return Err(WorkRunControlStorageError::AuthorityConflict);
        }
        transaction
            .commit()
            .map_err(|_| WorkRunControlStorageError::Unavailable)?;
        Ok(())
    }
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
