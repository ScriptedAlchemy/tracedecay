//! Canonical event append, idempotency, and deterministic projection replay.

use super::*;

impl WorkStoragePort for WorkSqliteStorage {
    fn load(
        &self,
        authority: &WorkAuthority,
        task_id: &TaskId,
    ) -> Result<Vec<WorkEvent>, WorkStorageError> {
        load_registered_history(self.handle(), authority, task_id)
    }

    fn projection(
        &self,
        authority: &WorkAuthority,
        task_id: &TaskId,
    ) -> Result<WorkProjection, WorkStorageError> {
        let history = load_registered_history(self.handle(), authority, task_id)?;
        WorkProjection::rebuild(&history).map_err(|_| WorkStorageError::Unavailable)
    }

    fn append(&self, request: &WorkAppendRequest) -> Result<WorkAppendOutcome, WorkStorageError> {
        append_registered(self.handle(), request)
    }
}

pub(crate) fn load_registered_history(
    handle: &ExactSqlHandle,
    authority: &WorkAuthority,
    task_id: &TaskId,
) -> Result<Vec<WorkEvent>, WorkStorageError> {
    let rows = registered_work_query(
        handle,
        "SELECT event_payload FROM work_events_v1
         WHERE project_id = ?1 AND repository_id = ?2 AND worktree_id = ?3
           AND actor_id = ?4 AND policy_digest = ?5 AND task_id = ?6
         ORDER BY version",
        authority_params_owned(authority)
            .into_iter()
            .chain([ExactSqlValue::Text(task_id.as_str().to_owned())])
            .collect(),
    )
    .map_err(|_| WorkStorageError::Unavailable)?;
    decode_registered_events(rows, true)
}

pub(crate) fn load_registered_authority_events(
    handle: &ExactSqlHandle,
    authority: &WorkAuthority,
) -> Result<Vec<WorkEvent>, WorkStorageError> {
    let rows = registered_work_query(
        handle,
        "SELECT event_payload FROM work_events_v1
         WHERE project_id = ?1 AND repository_id = ?2 AND worktree_id = ?3
           AND actor_id = ?4 AND policy_digest = ?5
         ORDER BY task_id, version",
        authority_params_owned(authority),
    )
    .map_err(|_| WorkStorageError::Unavailable)?;
    decode_registered_events(rows, false)
}

pub(crate) fn load_registered_history_in_transaction(
    transaction: &ExactSqlTransaction,
    authority: &WorkAuthority,
    task_id: &TaskId,
) -> Result<Vec<WorkEvent>, WorkStorageError> {
    let rows = registered_work_query(
        transaction,
        "SELECT event_payload FROM work_events_v1
         WHERE project_id = ?1 AND repository_id = ?2 AND worktree_id = ?3
           AND actor_id = ?4 AND policy_digest = ?5 AND task_id = ?6
         ORDER BY version",
        authority_params_owned(authority)
            .into_iter()
            .chain([ExactSqlValue::Text(task_id.as_str().to_owned())])
            .collect(),
    )
    .map_err(|_| WorkStorageError::Unavailable)?;
    decode_registered_events(rows, true)
}

pub(crate) fn decode_registered_events(
    rows: ExactSqlRows,
    empty_is_not_found: bool,
) -> Result<Vec<WorkEvent>, WorkStorageError> {
    if empty_is_not_found && rows.rows.is_empty() {
        return Err(WorkStorageError::NotFoundOrNotAuthorized);
    }
    rows.rows
        .into_iter()
        .map(|row| {
            let payload = exact_sql_text(&row.values, 0).ok_or(WorkStorageError::Unavailable)?;
            serde_json::from_str(payload).map_err(|_| WorkStorageError::Unavailable)
        })
        .collect()
}

pub(crate) fn append_registered(
    handle: &ExactSqlHandle,
    request: &WorkAppendRequest,
) -> Result<WorkAppendOutcome, WorkStorageError> {
    let transaction = handle
        .begin_immediate()
        .map_err(|_| WorkStorageError::Unavailable)?;
    let authority = request.event.authority();
    let task_id = request.event.task_id();
    let history = load_registered_history_in_transaction(&transaction, authority, task_id)
        .or_else(|error| match error {
            WorkStorageError::NotFoundOrNotAuthorized => Ok(Vec::new()),
            error => Err(error),
        })?;
    let current = if history.is_empty() {
        None
    } else {
        Some(WorkProjection::rebuild(&history).map_err(|_| WorkStorageError::Unavailable)?)
    };

    if let Some(replayed) = history
        .iter()
        .find(|event| event.command_id() == request.event.command_id())
    {
        let outcome = if replayed.input_digest() == request.event.input_digest() {
            current
                .map(WorkAppendOutcome::Replayed)
                .ok_or(WorkStorageError::Unavailable)
        } else {
            Err(WorkStorageError::IdempotencyConflict)
        };
        let _ = transaction.rollback();
        return outcome;
    }

    // A caller supplying an expected version asserts the task already exists,
    // so no canonical events is not a losing compare-and-swap.
    if current.is_none() && request.expected_version.is_some() {
        let _ = transaction.rollback();
        return Err(WorkStorageError::NotFoundOrNotAuthorized);
    }
    let current_version = current.as_ref().map(WorkProjection::version);
    if current_version != request.expected_version {
        let _ = transaction.rollback();
        return Err(WorkStorageError::VersionConflict);
    }
    let expected_event_version = current_version
        .map(WorkVersion::next)
        .transpose()
        .map_err(|_| WorkStorageError::Unavailable)?
        .unwrap_or_else(WorkVersion::initial);
    if request.event.version() != expected_event_version {
        let _ = transaction.rollback();
        return Err(WorkStorageError::VersionConflict);
    }

    advance_registered_owner_cursor(&transaction, authority)?;
    registered_insert_event(&transaction, &request.event)?;
    let next_history = load_registered_history_in_transaction(&transaction, authority, task_id)?;
    let next = WorkProjection::rebuild(&next_history).map_err(|_| WorkStorageError::Unavailable)?;
    transaction
        .commit()
        .map_err(|_| WorkStorageError::Unavailable)?;
    Ok(WorkAppendOutcome::Appended(next))
}

pub(crate) fn advance_registered_owner_cursor(
    transaction: &ExactSqlTransaction,
    authority: &WorkAuthority,
) -> Result<u64, WorkStorageError> {
    transaction
        .execute(
            exact_sql_statement(
                "INSERT INTO work_owner_cursors_v1 (
                    project_id, repository_id, worktree_id, actor_id, policy_digest, sequence
                 ) VALUES (?1, ?2, ?3, ?4, ?5, 1)
                 ON CONFLICT (project_id, repository_id, worktree_id, actor_id, policy_digest)
                 DO UPDATE SET sequence = sequence + 1",
                authority_params_owned(authority),
            )
            .map_err(|_| WorkStorageError::Unavailable)?,
        )
        .map_err(|_| WorkStorageError::Unavailable)?;
    let cursor_rows = registered_work_query(
        transaction,
        "SELECT sequence FROM work_owner_cursors_v1
         WHERE project_id = ?1 AND repository_id = ?2 AND worktree_id = ?3
           AND actor_id = ?4 AND policy_digest = ?5",
        authority_params_owned(authority),
    )
    .map_err(|_| WorkStorageError::Unavailable)?;
    cursor_rows
        .rows
        .first()
        .and_then(|row| exact_sql_integer(&row.values, 0))
        .and_then(|value| u64::try_from(value).ok())
        .ok_or(WorkStorageError::Unavailable)
}

pub(crate) fn registered_insert_event(
    transaction: &ExactSqlTransaction,
    event: &WorkEvent,
) -> Result<(), WorkStorageError> {
    let payload = serde_json::to_string(event).map_err(|_| WorkStorageError::Unavailable)?;
    transaction
        .execute(
            exact_sql_statement(
                "INSERT INTO work_events_v1 (
                    project_id, repository_id, worktree_id, actor_id, policy_digest,
                    task_id, version, command_id, input_digest, occurred_at, event_payload
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
                authority_params_owned(event.authority())
                    .into_iter()
                    .chain([
                        ExactSqlValue::Text(event.task_id().as_str().to_owned()),
                        ExactSqlValue::Integer(
                            i64::try_from(event.version().get())
                                .map_err(|_| WorkStorageError::Unavailable)?,
                        ),
                        ExactSqlValue::Text(event.command_id().as_str().to_owned()),
                        ExactSqlValue::Text(event.input_digest().as_str().to_owned()),
                        ExactSqlValue::Integer(event.occurred_at().0),
                        ExactSqlValue::Text(payload),
                    ])
                    .collect(),
            )
            .map_err(|_| WorkStorageError::Unavailable)?,
        )
        .map_err(|_| WorkStorageError::Unavailable)?;
    Ok(())
}
