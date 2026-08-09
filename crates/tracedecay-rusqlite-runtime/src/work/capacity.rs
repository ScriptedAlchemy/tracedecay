//! Canonical Work attempt capacity counts shared by every admission path.

use std::collections::BTreeMap;

use tracedecay_application::{
    MAX_WORK_ATTEMPT_CAPACITY_TASKS, WorkAttemptCapacityV1, WorkAttemptCapacityVerdictV1,
    WorkAttemptStorageError,
};
use tracedecay_domain::{TaskId, WorkAuthority, configuration::TopologyConcurrencyPolicyV1};

use crate::exact_sql::ExactSqlValue;

use super::{RegisteredWorkQuery, exact_sql_integer, registered_work_query};

pub(crate) fn capacity(
    source: &impl RegisteredWorkQuery,
    authority: &WorkAuthority,
    task_id: &TaskId,
    concurrency: &TopologyConcurrencyPolicyV1,
) -> Result<WorkAttemptCapacityV1, WorkAttemptStorageError> {
    capacities(
        source,
        authority,
        std::slice::from_ref(task_id),
        concurrency,
    )?
    .remove(task_id)
    .ok_or(WorkAttemptStorageError::Unavailable)
}

pub(crate) fn capacities(
    source: &impl RegisteredWorkQuery,
    authority: &WorkAuthority,
    task_ids: &[TaskId],
    concurrency: &TopologyConcurrencyPolicyV1,
) -> Result<BTreeMap<TaskId, WorkAttemptCapacityV1>, WorkAttemptStorageError> {
    if task_ids.len() > MAX_WORK_ATTEMPT_CAPACITY_TASKS
        || task_ids.windows(2).any(|pair| pair[0] >= pair[1])
    {
        return Err(WorkAttemptStorageError::Unavailable);
    }
    if task_ids.is_empty() {
        return Ok(BTreeMap::new());
    }
    let rows = registered_work_query(
        source,
        "SELECT row_kind, global_active, repository_active, task_id, task_active
         FROM (
             SELECT 0 AS row_kind,
                    (SELECT COUNT(*) FROM work_attempts_v1
                     WHERE project_id = ?1 AND terminal = 0) AS global_active,
                    (SELECT COUNT(*) FROM work_attempts_v1
                     WHERE project_id = ?1 AND repository_id = ?2 AND terminal = 0)
                        AS repository_active,
                    '' AS task_id,
                    0 AS task_active
             UNION ALL
             SELECT 1 AS row_kind, 0 AS global_active, 0 AS repository_active,
                    task_id, COUNT(*) AS task_active
             FROM work_attempts_v1
             WHERE project_id = ?1 AND repository_id = ?2 AND terminal = 0
             GROUP BY task_id
         )
         ORDER BY row_kind, task_id",
        vec![
            ExactSqlValue::Text(authority.project_id().as_str().to_owned()),
            ExactSqlValue::Text(authority.repository_id().as_str().to_owned()),
        ],
    )
    .map_err(|_| WorkAttemptStorageError::Unavailable)?;
    let header = rows
        .rows
        .first()
        .ok_or(WorkAttemptStorageError::Unavailable)?;
    if exact_sql_integer(&header.values, 0) != Some(0) {
        return Err(WorkAttemptStorageError::Unavailable);
    }
    let count = |values: &[ExactSqlValue], index| {
        exact_sql_integer(values, index)
            .and_then(|value| u64::try_from(value).ok())
            .ok_or(WorkAttemptStorageError::Unavailable)
    };
    let global_active = count(&header.values, 1)?;
    let repository_active = count(&header.values, 2)?;
    let mut task_counts = BTreeMap::new();
    for row in rows.rows.iter().skip(1) {
        if exact_sql_integer(&row.values, 0) != Some(1) {
            return Err(WorkAttemptStorageError::Unavailable);
        }
        let task_id =
            super::exact_sql_text(&row.values, 3).ok_or(WorkAttemptStorageError::Unavailable)?;
        if task_counts
            .insert(task_id.to_owned(), count(&row.values, 4)?)
            .is_some()
        {
            return Err(WorkAttemptStorageError::Unavailable);
        }
    }
    Ok(task_ids
        .iter()
        .cloned()
        .map(|task_id| {
            let task_active = task_counts.get(task_id.as_str()).copied().unwrap_or(0);
            (
                task_id,
                WorkAttemptCapacityV1::new(
                    global_active,
                    repository_active,
                    task_active,
                    concurrency.clone(),
                ),
            )
        })
        .collect())
}

pub(crate) fn require_capacity(
    source: &impl RegisteredWorkQuery,
    authority: &WorkAuthority,
    task_id: &TaskId,
    concurrency: &TopologyConcurrencyPolicyV1,
) -> Result<(), WorkAttemptStorageError> {
    match capacity(source, authority, task_id, concurrency)?.verdict() {
        WorkAttemptCapacityVerdictV1::Available => Ok(()),
        WorkAttemptCapacityVerdictV1::Exhausted(_) => {
            Err(WorkAttemptStorageError::CapacityExceeded)
        }
    }
}
