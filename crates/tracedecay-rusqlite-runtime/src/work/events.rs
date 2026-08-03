//! Event append: idempotency, version compare-and-append, and the folded
//! projection published with each committed event.

use super::*;

impl WorkStoragePort for WorkSqliteStorage {
    fn load(
        &self,
        authority: &WorkAuthority,
        task_id: &TaskId,
    ) -> Result<Vec<WorkEvent>, WorkStorageError> {
        load_registered_history(&self.handle, authority, task_id)
    }

    fn projection(
        &self,
        authority: &WorkAuthority,
        task_id: &TaskId,
    ) -> Result<WorkProjection, WorkStorageError> {
        load_registered_projection(&self.handle, authority, task_id)
    }

    fn append(&self, request: &WorkAppendRequest) -> Result<WorkAppendOutcome, WorkStorageError> {
        append_registered(&self.handle, request)
    }
}

/// Reads the published projection for one task. The snapshot row is what every
/// append publishes with the fold; it is the ordinary read authority.
pub(crate) fn load_registered_projection(
    handle: &ExactSqlHandle,
    authority: &WorkAuthority,
    task_id: &TaskId,
) -> Result<WorkProjection, WorkStorageError> {
    let rows = registered_work_query(
        handle,
        "SELECT projection_payload FROM work_projection_snapshots_v1
         WHERE project_id = ?1 AND repository_id = ?2 AND worktree_id = ?3
           AND actor_id = ?4 AND policy_digest = ?5 AND task_id = ?6",
        authority_params_owned(authority)
            .into_iter()
            .chain([ExactSqlValue::Text(task_id.as_str().to_owned())])
            .collect(),
    )
    .map_err(|_| WorkStorageError::Unavailable)?;
    let payload = rows
        .rows
        .first()
        .and_then(|row| exact_sql_text(&row.values, 0))
        .ok_or(WorkStorageError::NotFoundOrNotAuthorized)?;
    serde_json::from_str(payload).map_err(|_| WorkStorageError::Unavailable)
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
    decode_registered_events(rows)
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
    decode_registered_events(rows)
}

pub(crate) fn decode_registered_events(
    rows: ExactSqlRows,
) -> Result<Vec<WorkEvent>, WorkStorageError> {
    if rows.rows.is_empty() {
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
    let current = load_fold_state(&transaction, authority, task_id)?;

    if current
        .as_ref()
        .is_some_and(|state| state.command_ids().contains(request.event.command_id()))
    {
        let outcome = match replayed_input_digest(&transaction, authority, task_id, &request.event)?
        {
            Some(digest) if digest == request.event.input_digest().as_str() => {
                let state = current.expect("replay is only reachable with fold state");
                Ok(WorkAppendOutcome::Replayed(state.into_projection()))
            }
            Some(_) => Err(WorkStorageError::IdempotencyConflict),
            None => Err(WorkStorageError::Unavailable),
        };
        let _ = transaction.rollback();
        return outcome;
    }

    // A caller supplying an expected version asserts the task already exists,
    // so its absence is not a losing compare-and-swap. Fold state is absent
    // only for a task with no events, because an unmigrated task rebuilds.
    if current.is_none() && request.expected_version.is_some() {
        let _ = transaction.rollback();
        return Err(WorkStorageError::NotFoundOrNotAuthorized);
    }
    let current_version = current.as_ref().map(WorkProjectionStateV1::version);
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

    let next = match current {
        Some(state) => state.apply(&request.event),
        None => WorkProjectionStateV1::rebuild(std::slice::from_ref(&request.event)),
    }
    .map_err(|_| WorkStorageError::Unavailable)?;

    let owner_sequence = advance_registered_owner_cursor(&transaction, authority)?;
    registered_insert_event(&transaction, &request.event)?;
    registered_publish_projection(&transaction, next.projection(), owner_sequence)?;
    registered_publish_fold_state(&transaction, &next)?;
    transaction
        .commit()
        .map_err(|_| WorkStorageError::Unavailable)?;
    Ok(WorkAppendOutcome::Appended(next.into_projection()))
}

/// Reads the published fold state, falling back to one full rebuild when a
/// task has none yet — an unmigrated task, or one last written before the
/// current state version. That task folds incrementally from then on.
pub(crate) fn load_fold_state(
    transaction: &ExactSqlTransaction,
    authority: &WorkAuthority,
    task_id: &TaskId,
) -> Result<Option<WorkProjectionStateV1>, WorkStorageError> {
    let rows = registered_work_query(
        transaction,
        "SELECT state_payload FROM work_projection_fold_state_v1
         WHERE project_id = ?1 AND repository_id = ?2 AND worktree_id = ?3
           AND actor_id = ?4 AND policy_digest = ?5 AND task_id = ?6
           AND state_version = ?7",
        authority_params_owned(authority)
            .into_iter()
            .chain([
                ExactSqlValue::Text(task_id.as_str().to_owned()),
                ExactSqlValue::Integer(i64::from(WORK_PROJECTION_STATE_VERSION_V1)),
            ])
            .collect(),
    )
    .map_err(|_| WorkStorageError::Unavailable)?;
    if let Some(payload) = rows
        .rows
        .first()
        .and_then(|row| exact_sql_text(&row.values, 0))
    {
        return serde_json::from_str(payload)
            .map(Some)
            .map_err(|_| WorkStorageError::Unavailable);
    }

    let history = load_registered_history_in_transaction(transaction, authority, task_id).or_else(
        |error| match error {
            WorkStorageError::NotFoundOrNotAuthorized => Ok(Vec::new()),
            error => Err(error),
        },
    )?;
    if history.is_empty() {
        return Ok(None);
    }
    WorkProjectionStateV1::rebuild(&history)
        .map(Some)
        .map_err(|_| WorkStorageError::Unavailable)
}

/// Reads the one stored event that already used this command identity.
pub(crate) fn replayed_input_digest(
    transaction: &ExactSqlTransaction,
    authority: &WorkAuthority,
    task_id: &TaskId,
    event: &WorkEvent,
) -> Result<Option<String>, WorkStorageError> {
    let rows = registered_work_query(
        transaction,
        "SELECT input_digest FROM work_events_v1
         WHERE project_id = ?1 AND repository_id = ?2 AND worktree_id = ?3
           AND actor_id = ?4 AND policy_digest = ?5 AND task_id = ?6 AND command_id = ?7",
        authority_params_owned(authority)
            .into_iter()
            .chain([
                ExactSqlValue::Text(task_id.as_str().to_owned()),
                ExactSqlValue::Text(event.command_id().as_str().to_owned()),
            ])
            .collect(),
    )
    .map_err(|_| WorkStorageError::Unavailable)?;
    Ok(rows
        .rows
        .first()
        .and_then(|row| exact_sql_text(&row.values, 0))
        .map(str::to_owned))
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

pub(crate) fn registered_publish_fold_state(
    transaction: &ExactSqlTransaction,
    state: &WorkProjectionStateV1,
) -> Result<(), WorkStorageError> {
    let projection = state.projection();
    let payload = serde_json::to_string(state).map_err(|_| WorkStorageError::Unavailable)?;
    let version =
        i64::try_from(projection.version().get()).map_err(|_| WorkStorageError::Unavailable)?;
    transaction
        .execute(
            exact_sql_statement(
                "INSERT INTO work_projection_fold_state_v1 (
                    project_id, repository_id, worktree_id, actor_id, policy_digest,
                    task_id, version, state_version, state_payload
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
                 ON CONFLICT (
                    project_id, repository_id, worktree_id, actor_id, policy_digest, task_id
                 ) DO UPDATE SET
                    version = excluded.version,
                    state_version = excluded.state_version,
                    state_payload = excluded.state_payload
                 WHERE work_projection_fold_state_v1.version < excluded.version",
                authority_params_owned(projection.authority())
                    .into_iter()
                    .chain([
                        ExactSqlValue::Text(projection.task_id().as_str().to_owned()),
                        ExactSqlValue::Integer(version),
                        ExactSqlValue::Integer(i64::from(state.state_version())),
                        ExactSqlValue::Text(payload),
                    ])
                    .collect(),
            )
            .map_err(|_| WorkStorageError::Unavailable)?,
        )
        .map_err(|_| WorkStorageError::Unavailable)
        .and_then(|result| {
            if result.changed_rows == 1 {
                Ok(())
            } else {
                Err(WorkStorageError::VersionConflict)
            }
        })
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

pub(crate) fn registered_publish_projection(
    transaction: &ExactSqlTransaction,
    projection: &WorkProjection,
    owner_sequence: u64,
) -> Result<(), WorkStorageError> {
    let payload = serde_json::to_string(projection).map_err(|_| WorkStorageError::Unavailable)?;
    let version =
        i64::try_from(projection.version().get()).map_err(|_| WorkStorageError::Unavailable)?;
    let sequence = i64::try_from(owner_sequence).map_err(|_| WorkStorageError::Unavailable)?;
    let changed = transaction
        .execute(
            exact_sql_statement(
                "INSERT INTO work_projection_snapshots_v1 (
                    project_id, repository_id, worktree_id, actor_id, policy_digest,
                    task_id, version, owner_sequence, accepted_proposal_id,
                    execution_admitted, task_accepted, projection_payload
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)
                 ON CONFLICT (
                    project_id, repository_id, worktree_id, actor_id, policy_digest, task_id
                 ) DO UPDATE SET
                    version = excluded.version, owner_sequence = excluded.owner_sequence,
                    accepted_proposal_id = excluded.accepted_proposal_id,
                    execution_admitted = excluded.execution_admitted,
                    task_accepted = excluded.task_accepted,
                    projection_payload = excluded.projection_payload
                 WHERE work_projection_snapshots_v1.version + 1 = excluded.version",
                authority_params_owned(projection.authority())
                    .into_iter()
                    .chain([
                        ExactSqlValue::Text(projection.task_id().as_str().to_owned()),
                        ExactSqlValue::Integer(version),
                        ExactSqlValue::Integer(sequence),
                        projection
                            .accepted_proposal()
                            .map(|proposal| ExactSqlValue::Text(proposal.as_str().to_owned()))
                            .unwrap_or(ExactSqlValue::Null),
                        ExactSqlValue::Integer(if projection.is_execution_admitted() {
                            1
                        } else {
                            0
                        }),
                        ExactSqlValue::Integer(if projection.is_task_accepted() { 1 } else { 0 }),
                        ExactSqlValue::Text(payload.clone()),
                    ])
                    .collect(),
            )
            .map_err(|_| WorkStorageError::Unavailable)?,
        )
        .map_err(|_| WorkStorageError::Unavailable)?;
    if changed.changed_rows != 1 {
        return Err(WorkStorageError::VersionConflict);
    }
    transaction
        .execute(
            exact_sql_statement(
                "INSERT INTO work_projection_deltas_v1 (
                    project_id, repository_id, worktree_id, actor_id, policy_digest,
                    owner_sequence, task_id, version, projection_payload
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                authority_params_owned(projection.authority())
                    .into_iter()
                    .chain([
                        ExactSqlValue::Integer(sequence),
                        ExactSqlValue::Text(projection.task_id().as_str().to_owned()),
                        ExactSqlValue::Integer(version),
                        ExactSqlValue::Text(payload),
                    ])
                    .collect(),
            )
            .map_err(|_| WorkStorageError::Unavailable)?,
        )
        .map_err(|_| WorkStorageError::Unavailable)?;
    Ok(())
}
