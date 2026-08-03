//! Execution attempt persistence: leases, transitions, artifacts, and
//! exactly-once terminal evidence.

use super::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AttemptStoreError {
    Conflict,
    TerminalAlreadyPublished,
    Unavailable,
    InvalidRequest,
}

pub(crate) type AttemptStoreResult<T> = Result<T, AttemptStoreError>;

impl WorkSqliteStorage {
    pub fn execution_attempt(
        &self,
        authority: &WorkAuthority,
        identity: &WorkAttemptIdentityV1,
    ) -> Result<Option<WorkAttemptV1>, WorkExecutionPersistenceError> {
        load_registered_attempt_snapshot(&self.handle, authority, identity)
            .map_err(map_execution_persistence)
    }

    pub fn execution_attempt_history(
        &self,
        authority: &WorkAuthority,
        identity: &WorkAttemptIdentityV1,
    ) -> Result<Vec<WorkAttemptV1>, WorkExecutionPersistenceError> {
        load_registered_attempt_history(&self.handle, authority, identity)
            .map_err(map_execution_persistence)
    }

    pub fn recovery_candidates(
        &self,
        authority: &WorkAuthority,
    ) -> Result<Vec<WorkAttemptV1>, WorkExecutionPersistenceError> {
        registered_recovery_candidates(&self.handle, authority).map_err(map_execution_persistence)
    }

    fn append_execution_attempt(
        &self,
        authority: &WorkAuthority,
        command_id: &WorkCommandId,
        input_digest: &ManifestDigest,
        expected: Option<&WorkAttemptV1>,
        attempt: &WorkAttemptV1,
    ) -> AttemptStoreResult<()> {
        if expected.is_none() && attempt.state() != WorkAttemptStateV1::Leased {
            return Err(AttemptStoreError::InvalidRequest);
        }
        if expected.is_some_and(|expected| expected.identity() != attempt.identity()) {
            return Err(AttemptStoreError::InvalidRequest);
        }
        append_registered_attempt(
            &self.handle,
            authority,
            command_id,
            input_digest,
            expected,
            attempt,
        )
    }
}

impl WorkAttemptPersistencePort for WorkSqliteStorage {
    fn load(
        &self,
        authority: &WorkAuthority,
        identity: &WorkAttemptIdentityV1,
    ) -> Result<Option<WorkAttemptV1>, WorkExecutionPersistenceError> {
        self.execution_attempt(authority, identity)
    }

    fn insert(
        &self,
        authority: &WorkAuthority,
        attempt: &WorkAttemptV1,
    ) -> Result<(), WorkExecutionPersistenceError> {
        let (command_id, digest) = application_attempt_material(authority, None, attempt)?;
        self.append_execution_attempt(authority, &command_id, &digest, None, attempt)
            .map_err(map_execution_persistence)
    }

    fn compare_and_swap(
        &self,
        authority: &WorkAuthority,
        expected: &WorkAttemptV1,
        replacement: &WorkAttemptV1,
    ) -> Result<(), WorkExecutionPersistenceError> {
        let (command_id, digest) =
            application_attempt_material(authority, Some(expected), replacement)?;
        self.append_execution_attempt(authority, &command_id, &digest, Some(expected), replacement)
            .map_err(map_execution_persistence)
    }
}

pub(crate) fn attempt_params_owned(
    authority: &WorkAuthority,
    identity: &WorkAttemptIdentityV1,
) -> Vec<ExactSqlValue> {
    authority_params_owned(authority)
        .into_iter()
        .chain([
            ExactSqlValue::Text(identity.task_id().as_str().to_owned()),
            ExactSqlValue::Text(identity.run_id().as_str().to_owned()),
            ExactSqlValue::Text(identity.attempt_id().as_str().to_owned()),
        ])
        .collect()
}

pub(crate) fn load_registered_attempt_snapshot(
    source: &impl RegisteredWorkQuery,
    authority: &WorkAuthority,
    identity: &WorkAttemptIdentityV1,
) -> AttemptStoreResult<Option<WorkAttemptV1>> {
    let rows = registered_work_query(
        source,
        "SELECT attempt_payload
         FROM work_attempt_snapshots_v1
         WHERE project_id = ?1
           AND repository_id = ?2
           AND worktree_id = ?3
           AND actor_id = ?4
           AND policy_digest = ?5
           AND task_id = ?6
           AND run_id = ?7
           AND attempt_id = ?8",
        attempt_params_owned(authority, identity),
    )
    .map_err(|_| AttemptStoreError::Unavailable)?;
    let Some(row) = rows.rows.first() else {
        return Ok(None);
    };
    let payload = exact_sql_text(&row.values, 0).ok_or(AttemptStoreError::Unavailable)?;
    let attempt = decode_attempt(payload)?;
    if attempt.identity() != identity {
        return Err(AttemptStoreError::Unavailable);
    }
    Ok(Some(attempt))
}

pub(crate) fn load_registered_attempt_history(
    source: &impl RegisteredWorkQuery,
    authority: &WorkAuthority,
    identity: &WorkAttemptIdentityV1,
) -> AttemptStoreResult<Vec<WorkAttemptV1>> {
    let rows = registered_work_query(
        source,
        "SELECT revision, attempt_payload
         FROM work_attempt_events_v1
         WHERE project_id = ?1
           AND repository_id = ?2
           AND worktree_id = ?3
           AND actor_id = ?4
           AND policy_digest = ?5
           AND task_id = ?6
           AND run_id = ?7
           AND attempt_id = ?8
         ORDER BY revision",
        attempt_params_owned(authority, identity),
    )
    .map_err(|_| AttemptStoreError::Unavailable)?;
    let mut history = Vec::new();
    for row in rows.rows {
        let revision = exact_sql_integer(&row.values, 0).ok_or(AttemptStoreError::Unavailable)?;
        if usize::try_from(revision).ok() != Some(history.len() + 1) {
            return Err(AttemptStoreError::Unavailable);
        }
        let payload = exact_sql_text(&row.values, 1).ok_or(AttemptStoreError::Unavailable)?;
        let attempt = decode_attempt(payload)?;
        if attempt.identity() != identity {
            return Err(AttemptStoreError::Unavailable);
        }
        if let Some(previous) = history.last() {
            validate_attempt_transition(previous, &attempt)
                .map_err(|_| AttemptStoreError::Unavailable)?;
        } else if attempt.state() != WorkAttemptStateV1::Leased {
            return Err(AttemptStoreError::Unavailable);
        }
        history.push(attempt);
    }
    Ok(history)
}

pub(crate) fn registered_recovery_candidates(
    handle: &ExactSqlHandle,
    authority: &WorkAuthority,
) -> AttemptStoreResult<Vec<WorkAttemptV1>> {
    let rows = registered_work_query(
        handle,
        "SELECT attempt_payload
         FROM work_attempt_snapshots_v1
         WHERE project_id = ?1
           AND repository_id = ?2
           AND worktree_id = ?3
           AND actor_id = ?4
           AND policy_digest = ?5
           AND state = 'recovery_required'
         ORDER BY task_id, run_id, attempt_id",
        authority_params_owned(authority),
    )
    .map_err(|_| AttemptStoreError::Unavailable)?;
    rows.rows
        .into_iter()
        .map(|row| {
            let payload = exact_sql_text(&row.values, 0).ok_or(AttemptStoreError::Unavailable)?;
            decode_attempt(payload)
        })
        .collect()
}

pub(crate) fn append_registered_attempt(
    handle: &ExactSqlHandle,
    authority: &WorkAuthority,
    command_id: &WorkCommandId,
    input_digest: &ManifestDigest,
    expected: Option<&WorkAttemptV1>,
    attempt: &WorkAttemptV1,
) -> AttemptStoreResult<()> {
    let transaction = handle
        .begin_immediate()
        .map_err(|_| AttemptStoreError::Unavailable)?;
    if let Some((digest, _payload)) =
        load_registered_attempt_idempotency(&transaction, authority, command_id.as_str())?
    {
        let result = if digest == input_digest.as_str() {
            Ok(())
        } else {
            Err(AttemptStoreError::Conflict)
        };
        let _ = transaction.rollback();
        return result;
    }
    validate_registered_attempt_projection(&transaction, authority, attempt)?;
    let identity = attempt.identity();
    let current = load_registered_attempt_snapshot(&transaction, authority, identity)?;
    let revision = match current.as_ref() {
        None => {
            if expected.is_some() || attempt.state() != WorkAttemptStateV1::Leased {
                let _ = transaction.rollback();
                return Err(AttemptStoreError::Conflict);
            }
            1
        }
        Some(current) => {
            if current.is_terminal() {
                let _ = transaction.rollback();
                return Err(AttemptStoreError::TerminalAlreadyPublished);
            }
            if expected != Some(current) {
                let _ = transaction.rollback();
                return Err(AttemptStoreError::Conflict);
            }
            validate_attempt_transition(current, attempt)?;
            u64::try_from(load_registered_attempt_history(&transaction, authority, identity)?.len())
                .ok()
                .and_then(|value| value.checked_add(1))
                .ok_or(AttemptStoreError::Unavailable)?
        }
    };
    persist_registered_attempt_event(
        &transaction,
        authority,
        command_id,
        input_digest,
        attempt,
        revision,
    )?;
    persist_registered_attempt_snapshot(
        &transaction,
        authority,
        current.as_ref(),
        attempt,
        revision,
    )?;
    persist_registered_attempt_artifacts(&transaction, authority, attempt, revision)?;
    persist_registered_terminal_evidence(&transaction, authority, attempt, revision)?;
    persist_registered_attempt_idempotency(
        &transaction,
        authority,
        command_id,
        input_digest,
        attempt,
        revision,
    )?;
    transaction
        .commit()
        .map_err(|_| AttemptStoreError::Unavailable)?;
    Ok(())
}

pub(crate) fn load_registered_attempt_idempotency(
    source: &impl RegisteredWorkQuery,
    authority: &WorkAuthority,
    command_id: &str,
) -> AttemptStoreResult<Option<(String, String)>> {
    let rows = registered_work_query(
        source,
        "SELECT input_digest, attempt_payload
         FROM work_attempt_idempotency_v1
         WHERE project_id = ?1
           AND repository_id = ?2
           AND worktree_id = ?3
           AND actor_id = ?4
           AND policy_digest = ?5
           AND command_id = ?6",
        authority_params_owned(authority)
            .into_iter()
            .chain([ExactSqlValue::Text(command_id.to_owned())])
            .collect(),
    )
    .map_err(|_| AttemptStoreError::Unavailable)?;
    let Some(row) = rows.rows.first() else {
        return Ok(None);
    };
    let digest = exact_sql_text(&row.values, 0)
        .ok_or(AttemptStoreError::Unavailable)?
        .to_owned();
    let payload = exact_sql_text(&row.values, 1)
        .ok_or(AttemptStoreError::Unavailable)?
        .to_owned();
    Ok(Some((digest, payload)))
}

pub(crate) fn validate_registered_attempt_projection(
    source: &impl RegisteredWorkQuery,
    authority: &WorkAuthority,
    attempt: &WorkAttemptV1,
) -> AttemptStoreResult<()> {
    let expected_generation =
        projection_generation(authority).map_err(|_| AttemptStoreError::Unavailable)?;
    if attempt.projection_binding().generation_id() != &expected_generation {
        // Generation is authority-derived. A forged binding must not insert.
        return Err(AttemptStoreError::InvalidRequest);
    }
    let rows = registered_work_query(
        source,
        "SELECT owner_sequence, projection_payload
         FROM work_projection_snapshots_v1
         WHERE project_id = ?1
           AND repository_id = ?2
           AND worktree_id = ?3
           AND actor_id = ?4
           AND policy_digest = ?5
           AND task_id = ?6",
        authority_params_owned(authority)
            .into_iter()
            .chain([ExactSqlValue::Text(
                attempt.identity().task_id().as_str().to_owned(),
            )])
            .collect(),
    )
    .map_err(|_| AttemptStoreError::Unavailable)?;
    let row = rows.rows.first().ok_or(AttemptStoreError::Conflict)?;
    let owner_sequence = exact_sql_integer(&row.values, 0).ok_or(AttemptStoreError::Unavailable)?;
    let payload = exact_sql_text(&row.values, 1).ok_or(AttemptStoreError::Unavailable)?;
    let projection: WorkProjection =
        serde_json::from_str(payload).map_err(|_| AttemptStoreError::Unavailable)?;
    if projection.authority() != authority {
        return Err(AttemptStoreError::InvalidRequest);
    }
    let owner_sequence =
        u64::try_from(owner_sequence).map_err(|_| AttemptStoreError::Unavailable)?;
    if attempt.projection_binding().sequence().get() > owner_sequence {
        return Err(AttemptStoreError::InvalidRequest);
    }
    attempt
        .validate_projection(&projection)
        .map_err(|_| AttemptStoreError::InvalidRequest)
}

pub(crate) fn persist_registered_attempt_event(
    transaction: &ExactSqlTransaction,
    authority: &WorkAuthority,
    command_id: &WorkCommandId,
    input_digest: &ManifestDigest,
    attempt: &WorkAttemptV1,
    revision: u64,
) -> AttemptStoreResult<()> {
    let payload = serde_json::to_string(attempt).map_err(|_| AttemptStoreError::Unavailable)?;
    transaction
        .execute(
            exact_sql_statement(
                "INSERT INTO work_attempt_events_v1 (
                    project_id, repository_id, worktree_id, actor_id, policy_digest,
                    task_id, run_id, attempt_id, revision, command_id, input_digest, attempt_payload
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
                attempt_params_owned(authority, attempt.identity())
                    .into_iter()
                    .chain([
                        ExactSqlValue::Integer(to_sql_u64(revision)?),
                        ExactSqlValue::Text(command_id.as_str().to_owned()),
                        ExactSqlValue::Text(input_digest.as_str().to_owned()),
                        ExactSqlValue::Text(payload),
                    ])
                    .collect(),
            )
            .map_err(|_| AttemptStoreError::Unavailable)?,
        )
        .map_err(|_| AttemptStoreError::Unavailable)?;
    Ok(())
}

pub(crate) fn persist_registered_attempt_snapshot(
    transaction: &ExactSqlTransaction,
    authority: &WorkAuthority,
    previous: Option<&WorkAttemptV1>,
    attempt: &WorkAttemptV1,
    revision: u64,
) -> AttemptStoreResult<()> {
    let payload = serde_json::to_string(attempt).map_err(|_| AttemptStoreError::Unavailable)?;
    let mut values = attempt_params_owned(authority, attempt.identity());
    values.extend([
        ExactSqlValue::Integer(to_sql_u64(revision)?),
        ExactSqlValue::Text(attempt.lease().lease_id().as_str().to_owned()),
        ExactSqlValue::Integer(to_sql_u64(attempt.lease().epoch().get())?),
        ExactSqlValue::Text(attempt_state(attempt.state()).to_owned()),
        ExactSqlValue::Text(payload),
    ]);
    let (sql, values) = match previous {
        None => (
            "INSERT INTO work_attempt_snapshots_v1 (
                project_id, repository_id, worktree_id, actor_id, policy_digest,
                task_id, run_id, attempt_id, revision, lease_id, fence_epoch, state, attempt_payload
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
            values,
        ),
        Some(previous) => {
            values.extend([
                ExactSqlValue::Integer(to_sql_u64(revision - 1)?),
                ExactSqlValue::Text(previous.lease().lease_id().as_str().to_owned()),
                ExactSqlValue::Integer(to_sql_u64(previous.lease().epoch().get())?),
            ]);
            (
                "UPDATE work_attempt_snapshots_v1
                 SET revision = ?9,
                     lease_id = ?10,
                     fence_epoch = ?11,
                     state = ?12,
                     attempt_payload = ?13
                 WHERE project_id = ?1
                   AND repository_id = ?2
                   AND worktree_id = ?3
                   AND actor_id = ?4
                   AND policy_digest = ?5
                   AND task_id = ?6
                   AND run_id = ?7
                   AND attempt_id = ?8
                   AND revision = ?14
                   AND lease_id = ?15
                   AND fence_epoch = ?16",
                values,
            )
        }
    };
    let changed = transaction
        .execute(exact_sql_statement(sql, values).map_err(|_| AttemptStoreError::Unavailable)?)
        .map_err(|_| AttemptStoreError::Unavailable)?;
    if changed.changed_rows != 1 {
        return Err(AttemptStoreError::Conflict);
    }
    Ok(())
}

pub(crate) fn persist_registered_attempt_artifacts(
    transaction: &ExactSqlTransaction,
    authority: &WorkAuthority,
    attempt: &WorkAttemptV1,
    revision: u64,
) -> AttemptStoreResult<()> {
    for artifact in attempt.artifacts() {
        let params = attempt_params_owned(authority, attempt.identity())
            .into_iter()
            .chain([
                ExactSqlValue::Text(artifact.artifact_id().as_str().to_owned()),
                ExactSqlValue::Text(artifact.digest().as_str().to_owned()),
                ExactSqlValue::Integer(to_sql_u64(artifact.byte_length())?),
                ExactSqlValue::Integer(to_sql_u64(revision)?),
            ])
            .collect();
        let outcome = transaction
            .execute(
                exact_sql_statement(
                    "INSERT INTO work_attempt_artifacts_v1 (
                        project_id, repository_id, worktree_id, actor_id, policy_digest,
                        task_id, run_id, attempt_id, artifact_id, digest, byte_length, first_revision
                     ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)
                     ON CONFLICT (
                        project_id, repository_id, worktree_id, actor_id, policy_digest,
                        task_id, run_id, attempt_id, artifact_id
                     ) DO NOTHING",
                    params,
                )
                .map_err(|_| AttemptStoreError::Unavailable)?,
            )
            .map_err(|_| AttemptStoreError::Unavailable)?;
        // A fresh insert stored this exact `(artifact_id, digest, byte_length)`
        // row, so the verify below would count it and pass; only a swallowed
        // conflict — a prior row under the same key — needs the round trip to
        // confirm its digest and length still match this replay.
        if outcome.changed_rows == 1 {
            continue;
        }
        let rows = registered_work_query(
            transaction,
            "SELECT COUNT(*)
             FROM work_attempt_artifacts_v1
             WHERE project_id = ?1
               AND repository_id = ?2
               AND worktree_id = ?3
               AND actor_id = ?4
               AND policy_digest = ?5
               AND task_id = ?6
               AND run_id = ?7
               AND attempt_id = ?8
               AND artifact_id = ?9
               AND digest = ?10
               AND byte_length = ?11",
            attempt_params_owned(authority, attempt.identity())
                .into_iter()
                .chain([
                    ExactSqlValue::Text(artifact.artifact_id().as_str().to_owned()),
                    ExactSqlValue::Text(artifact.digest().as_str().to_owned()),
                    ExactSqlValue::Integer(to_sql_u64(artifact.byte_length())?),
                ])
                .collect(),
        )
        .map_err(|_| AttemptStoreError::Unavailable)?;
        if rows
            .rows
            .first()
            .and_then(|row| exact_sql_integer(&row.values, 0))
            != Some(1)
        {
            return Err(AttemptStoreError::InvalidRequest);
        }
    }
    Ok(())
}

pub(crate) fn persist_registered_terminal_evidence(
    transaction: &ExactSqlTransaction,
    authority: &WorkAuthority,
    attempt: &WorkAttemptV1,
    revision: u64,
) -> AttemptStoreResult<()> {
    let Some(terminal) = attempt.terminal() else {
        return Ok(());
    };
    let payload = serde_json::to_string(terminal).map_err(|_| AttemptStoreError::Unavailable)?;
    transaction
        .execute(
            exact_sql_statement(
                "INSERT INTO work_attempt_terminal_evidence_v1 (
                    project_id, repository_id, worktree_id, actor_id, policy_digest,
                    task_id, run_id, attempt_id, revision, terminal_payload
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
                attempt_params_owned(authority, attempt.identity())
                    .into_iter()
                    .chain([
                        ExactSqlValue::Integer(to_sql_u64(revision)?),
                        ExactSqlValue::Text(payload),
                    ])
                    .collect(),
            )
            .map_err(|_| AttemptStoreError::Unavailable)?,
        )
        .map_err(|_| AttemptStoreError::Unavailable)?;
    Ok(())
}

pub(crate) fn persist_registered_attempt_idempotency(
    transaction: &ExactSqlTransaction,
    authority: &WorkAuthority,
    command_id: &WorkCommandId,
    input_digest: &ManifestDigest,
    attempt: &WorkAttemptV1,
    revision: u64,
) -> AttemptStoreResult<()> {
    let payload = serde_json::to_string(attempt).map_err(|_| AttemptStoreError::Unavailable)?;
    transaction
        .execute(
            exact_sql_statement(
                "INSERT INTO work_attempt_idempotency_v1 (
                    project_id, repository_id, worktree_id, actor_id, policy_digest,
                    command_id, input_digest, task_id, run_id, attempt_id, revision, attempt_payload
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
                authority_params_owned(authority)
                    .into_iter()
                    .chain([
                        ExactSqlValue::Text(command_id.as_str().to_owned()),
                        ExactSqlValue::Text(input_digest.as_str().to_owned()),
                        ExactSqlValue::Text(attempt.identity().task_id().as_str().to_owned()),
                        ExactSqlValue::Text(attempt.identity().run_id().as_str().to_owned()),
                        ExactSqlValue::Text(attempt.identity().attempt_id().as_str().to_owned()),
                        ExactSqlValue::Integer(to_sql_u64(revision)?),
                        ExactSqlValue::Text(payload),
                    ])
                    .collect(),
            )
            .map_err(|_| AttemptStoreError::Unavailable)?,
        )
        .map_err(|_| AttemptStoreError::Unavailable)?;
    Ok(())
}

pub(crate) fn validate_attempt_transition(
    previous: &WorkAttemptV1,
    candidate: &WorkAttemptV1,
) -> AttemptStoreResult<()> {
    let rebuilt = if previous.state() == candidate.state() {
        if candidate.lease().lease_id() != previous.lease().lease_id()
            || candidate.lease().epoch() < previous.lease().epoch()
            || previous.progress().is_some_and(|progress| {
                candidate.progress().is_none_or(|candidate| {
                    progress.total() != candidate.total()
                        || progress.completed() > candidate.completed()
                })
            })
            || previous
                .artifacts()
                .iter()
                .any(|artifact| !candidate.artifacts().contains(artifact))
            || candidate.cancellation() != previous.cancellation()
            || candidate.recovery() != previous.recovery()
            || candidate.execution() != previous.execution()
            || candidate.actual_route() != previous.actual_route()
            || candidate.terminal() != previous.terminal()
        {
            return Err(AttemptStoreError::InvalidRequest);
        }
        WorkAttemptV1::new(
            previous.identity().clone(),
            previous.projection_binding().clone(),
            previous.execution().clone(),
            candidate.lease().clone(),
            candidate.state(),
            candidate.progress(),
            candidate.artifacts().to_vec(),
            candidate.cancellation().clone(),
            candidate.recovery().clone(),
            previous.requested_route().clone(),
            candidate.actual_route().cloned(),
            candidate.terminal().cloned(),
        )
        .map_err(|_| AttemptStoreError::InvalidRequest)?
    } else {
        previous
            .transition(
                candidate.state(),
                candidate.progress(),
                candidate.artifacts().to_vec(),
                candidate.cancellation().clone(),
                candidate.recovery().clone(),
                candidate.actual_route().cloned(),
                candidate.terminal().cloned(),
                candidate.lease().clone(),
            )
            .map_err(|_| AttemptStoreError::InvalidRequest)?
    };
    if &rebuilt != candidate {
        return Err(AttemptStoreError::InvalidRequest);
    }
    Ok(())
}

pub(crate) fn application_attempt_material(
    authority: &WorkAuthority,
    expected: Option<&WorkAttemptV1>,
    attempt: &WorkAttemptV1,
) -> Result<(WorkCommandId, ManifestDigest), WorkExecutionPersistenceError> {
    let digest = canonical_sha256(&("work-attempt-persistence-v1", authority, expected, attempt))
        .map_err(|error| WorkExecutionPersistenceError::Unavailable(error.to_string()))?;
    let command_suffix = digest
        .as_str()
        .strip_prefix("sha256:")
        .unwrap_or(digest.as_str());
    let command_id = WorkCommandId::new(format!("work-attempt.persistence.{command_suffix}"))
        .map_err(|error| WorkExecutionPersistenceError::Unavailable(error.to_string()))?;
    Ok((command_id, digest))
}

pub(crate) fn map_execution_persistence(error: AttemptStoreError) -> WorkExecutionPersistenceError {
    match error {
        AttemptStoreError::Conflict | AttemptStoreError::TerminalAlreadyPublished => {
            WorkExecutionPersistenceError::Conflict
        }
        AttemptStoreError::InvalidRequest => WorkExecutionPersistenceError::InvalidRequest,
        AttemptStoreError::Unavailable => WorkExecutionPersistenceError::Unavailable(
            "SQLite Work runtime store failed".to_owned(),
        ),
    }
}

pub(crate) fn decode_attempt(payload: &str) -> AttemptStoreResult<WorkAttemptV1> {
    serde_json::from_str(payload).map_err(|_| AttemptStoreError::Unavailable)
}

pub(crate) fn attempt_state(state: WorkAttemptStateV1) -> &'static str {
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

pub(crate) fn to_sql_u64(value: u64) -> AttemptStoreResult<i64> {
    i64::try_from(value).map_err(|_| AttemptStoreError::InvalidRequest)
}
