//! Concrete SQLite persistence for the application-owned Work authority.

use std::collections::BTreeSet;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use tracedecay_application::{
    WorkAppendOutcome, WorkAppendRequest, WorkAttemptPersistencePort,
    WorkExecutionPersistenceError, WorkProjectionPortError, WorkProjectionReadPort,
    WorkStorageError, WorkStoragePort,
};
use tracedecay_domain::{
    ManifestDigest, ProjectionGenerationId, TaskId, WorkAttemptIdentityV1, WorkAttemptStateV1,
    WorkAttemptV1, WorkAuthority, WorkCommandId, WorkEvent, WorkProjection,
    WorkProjectionCoverageV1, WorkProjectionDeltaV1, WorkProjectionResumeCursorV1,
    WorkProjectionSequenceRangeV1, WorkProjectionSequenceV1, WorkProjectionSnapshotV1, WorkVersion,
    canonical_sha256,
};

use crate::migration_sql::{
    MigrationSqlHandle, MigrationSqlRows, MigrationSqlStatement, MigrationSqlTransaction,
    MigrationSqlValue,
};

pub const WORK_SCHEMA_V1: &str = "
CREATE TABLE IF NOT EXISTS work_owner_cursors_v1 (
    project_id TEXT NOT NULL,
    repository_id TEXT NOT NULL,
    worktree_id TEXT NOT NULL,
    actor_id TEXT NOT NULL,
    policy_digest TEXT NOT NULL,
    sequence INTEGER NOT NULL CHECK (sequence > 0),
    PRIMARY KEY (project_id, repository_id, worktree_id, actor_id, policy_digest)
) STRICT;

CREATE TABLE IF NOT EXISTS work_events_v1 (
    project_id TEXT NOT NULL,
    repository_id TEXT NOT NULL,
    worktree_id TEXT NOT NULL,
    actor_id TEXT NOT NULL,
    policy_digest TEXT NOT NULL,
    task_id TEXT NOT NULL,
    version INTEGER NOT NULL CHECK (version > 0),
    command_id TEXT NOT NULL,
    input_digest TEXT NOT NULL,
    occurred_at INTEGER NOT NULL,
    event_payload TEXT NOT NULL,
    PRIMARY KEY (
        project_id, repository_id, worktree_id, actor_id, policy_digest, task_id, version
    ),
    UNIQUE (
        project_id, repository_id, worktree_id, actor_id, policy_digest, task_id, command_id
    )
) STRICT;

CREATE TABLE IF NOT EXISTS work_projection_snapshots_v1 (
    project_id TEXT NOT NULL,
    repository_id TEXT NOT NULL,
    worktree_id TEXT NOT NULL,
    actor_id TEXT NOT NULL,
    policy_digest TEXT NOT NULL,
    task_id TEXT NOT NULL,
    version INTEGER NOT NULL CHECK (version > 0),
    owner_sequence INTEGER NOT NULL CHECK (owner_sequence > 0),
    accepted_proposal_id TEXT,
    execution_admitted INTEGER NOT NULL CHECK (execution_admitted IN (0, 1)),
    task_accepted INTEGER NOT NULL CHECK (task_accepted IN (0, 1)),
    projection_payload TEXT NOT NULL,
    PRIMARY KEY (project_id, repository_id, worktree_id, actor_id, policy_digest, task_id),
    UNIQUE (
        project_id, repository_id, worktree_id, actor_id, policy_digest, owner_sequence
    )
) STRICT;

CREATE TABLE IF NOT EXISTS work_projection_deltas_v1 (
    project_id TEXT NOT NULL,
    repository_id TEXT NOT NULL,
    worktree_id TEXT NOT NULL,
    actor_id TEXT NOT NULL,
    policy_digest TEXT NOT NULL,
    owner_sequence INTEGER NOT NULL CHECK (owner_sequence > 0),
    task_id TEXT NOT NULL,
    version INTEGER NOT NULL CHECK (version > 0),
    projection_payload TEXT NOT NULL,
    PRIMARY KEY (
        project_id, repository_id, worktree_id, actor_id, policy_digest, owner_sequence
    ),
    UNIQUE (
        project_id, repository_id, worktree_id, actor_id, policy_digest, task_id, version
    )
) STRICT;

CREATE TABLE IF NOT EXISTS work_attempt_events_v1 (
    project_id TEXT NOT NULL,
    repository_id TEXT NOT NULL,
    worktree_id TEXT NOT NULL,
    actor_id TEXT NOT NULL,
    policy_digest TEXT NOT NULL,
    task_id TEXT NOT NULL,
    run_id TEXT NOT NULL,
    attempt_id TEXT NOT NULL,
    revision INTEGER NOT NULL CHECK (revision > 0),
    command_id TEXT NOT NULL,
    input_digest TEXT NOT NULL,
    attempt_payload TEXT NOT NULL,
    PRIMARY KEY (
        project_id, repository_id, worktree_id, actor_id, policy_digest,
        task_id, run_id, attempt_id, revision
    ),
    UNIQUE (
        project_id, repository_id, worktree_id, actor_id, policy_digest, command_id
    )
) STRICT;

CREATE TABLE IF NOT EXISTS work_attempt_snapshots_v1 (
    project_id TEXT NOT NULL,
    repository_id TEXT NOT NULL,
    worktree_id TEXT NOT NULL,
    actor_id TEXT NOT NULL,
    policy_digest TEXT NOT NULL,
    task_id TEXT NOT NULL,
    run_id TEXT NOT NULL,
    attempt_id TEXT NOT NULL,
    revision INTEGER NOT NULL CHECK (revision > 0),
    lease_id TEXT NOT NULL,
    fence_epoch INTEGER NOT NULL CHECK (fence_epoch > 0),
    state TEXT NOT NULL,
    attempt_payload TEXT NOT NULL,
    PRIMARY KEY (
        project_id, repository_id, worktree_id, actor_id, policy_digest,
        task_id, run_id, attempt_id
    )
) STRICT;

CREATE TABLE IF NOT EXISTS work_attempt_idempotency_v1 (
    project_id TEXT NOT NULL,
    repository_id TEXT NOT NULL,
    worktree_id TEXT NOT NULL,
    actor_id TEXT NOT NULL,
    policy_digest TEXT NOT NULL,
    command_id TEXT NOT NULL,
    input_digest TEXT NOT NULL,
    task_id TEXT NOT NULL,
    run_id TEXT NOT NULL,
    attempt_id TEXT NOT NULL,
    revision INTEGER NOT NULL CHECK (revision > 0),
    attempt_payload TEXT NOT NULL,
    PRIMARY KEY (
        project_id, repository_id, worktree_id, actor_id, policy_digest, command_id
    )
) STRICT;

CREATE TABLE IF NOT EXISTS work_attempt_artifacts_v1 (
    project_id TEXT NOT NULL,
    repository_id TEXT NOT NULL,
    worktree_id TEXT NOT NULL,
    actor_id TEXT NOT NULL,
    policy_digest TEXT NOT NULL,
    task_id TEXT NOT NULL,
    run_id TEXT NOT NULL,
    attempt_id TEXT NOT NULL,
    artifact_id TEXT NOT NULL,
    digest TEXT NOT NULL,
    byte_length INTEGER NOT NULL CHECK (byte_length > 0),
    first_revision INTEGER NOT NULL CHECK (first_revision > 0),
    PRIMARY KEY (
        project_id, repository_id, worktree_id, actor_id, policy_digest,
        task_id, run_id, attempt_id, artifact_id
    )
) STRICT;

CREATE TABLE IF NOT EXISTS work_attempt_terminal_evidence_v1 (
    project_id TEXT NOT NULL,
    repository_id TEXT NOT NULL,
    worktree_id TEXT NOT NULL,
    actor_id TEXT NOT NULL,
    policy_digest TEXT NOT NULL,
    task_id TEXT NOT NULL,
    run_id TEXT NOT NULL,
    attempt_id TEXT NOT NULL,
    revision INTEGER NOT NULL CHECK (revision > 0),
    terminal_payload TEXT NOT NULL,
    PRIMARY KEY (
        project_id, repository_id, worktree_id, actor_id, policy_digest,
        task_id, run_id, attempt_id
    )
) STRICT;
";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum AttemptStoreError {
    Conflict,
    TerminalAlreadyPublished,
    Unavailable,
    InvalidRequest,
}

type AttemptStoreResult<T> = Result<T, AttemptStoreError>;

pub fn install_work_schema(connection: &Connection) -> rusqlite::Result<()> {
    connection.execute_batch(WORK_SCHEMA_V1)
}

#[derive(Clone)]
pub struct WorkSqliteStorage {
    backend: WorkSqliteBackend,
}

#[derive(Clone)]
enum WorkSqliteBackend {
    Direct(Arc<Mutex<Connection>>),
    Registered(MigrationSqlHandle),
}

impl WorkSqliteStorage {
    pub fn new(connection: Arc<Mutex<Connection>>) -> Self {
        Self {
            backend: WorkSqliteBackend::Direct(connection),
        }
    }

    pub fn from_registered(handle: MigrationSqlHandle) -> Self {
        Self {
            backend: WorkSqliteBackend::Registered(handle),
        }
    }

    pub fn owner_cursor(
        connection: &Connection,
        authority: &WorkAuthority,
    ) -> rusqlite::Result<u64> {
        let sequence = connection
            .query_row(
                "SELECT sequence
                 FROM work_owner_cursors_v1
                 WHERE project_id = ?1
                   AND repository_id = ?2
                   AND worktree_id = ?3
                   AND actor_id = ?4
                   AND policy_digest = ?5",
                authority_params(authority),
                |row| row.get::<_, i64>(0),
            )
            .optional()?
            .unwrap_or(0);
        u64::try_from(sequence).map_err(|_| invalid_storage("negative Work owner cursor"))
    }

    pub fn resume_cursor(
        snapshot: &WorkProjectionSnapshotV1,
    ) -> Result<WorkProjectionResumeCursorV1, WorkProjectionPortError> {
        projection_cursor(snapshot.generation_id().clone(), snapshot.sequence())
    }
}

impl WorkStoragePort for WorkSqliteStorage {
    fn load(
        &self,
        authority: &WorkAuthority,
        task_id: &TaskId,
    ) -> Result<Vec<WorkEvent>, WorkStorageError> {
        match &self.backend {
            WorkSqliteBackend::Direct(connection) => {
                let connection = connection
                    .lock()
                    .map_err(|_| WorkStorageError::Unavailable)?;
                load_history(&connection, authority, task_id)
            }
            WorkSqliteBackend::Registered(handle) => {
                load_registered_history(handle, authority, task_id)
            }
        }
    }

    fn append(&self, request: &WorkAppendRequest) -> Result<WorkAppendOutcome, WorkStorageError> {
        match &self.backend {
            WorkSqliteBackend::Direct(connection) => append_direct(connection, request),
            WorkSqliteBackend::Registered(handle) => append_registered(handle, request),
        }
    }
}

fn append_direct(
    connection: &Arc<Mutex<Connection>>,
    request: &WorkAppendRequest,
) -> Result<WorkAppendOutcome, WorkStorageError> {
    let mut connection = connection
        .lock()
        .map_err(|_| WorkStorageError::Unavailable)?;
    let transaction = connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(map_sqlite)?;
    let authority = request.event.authority();
    let task_id = request.event.task_id();
    let mut history =
        load_history(&transaction, authority, task_id).or_else(|error| match error {
            WorkStorageError::NotFoundOrNotAuthorized => Ok(Vec::new()),
            error => Err(error),
        })?;

    if let Some(prior) = history
        .iter()
        .find(|event| event.command_id() == request.event.command_id())
    {
        return if prior.input_digest() == request.event.input_digest() {
            Ok(WorkAppendOutcome::Replayed(history))
        } else {
            Err(WorkStorageError::IdempotencyConflict)
        };
    }

    let current_version = history.last().map(WorkEvent::version);
    if current_version != request.expected_version {
        return Err(WorkStorageError::VersionConflict);
    }
    let expected_event_version = current_version
        .map(WorkVersion::next)
        .transpose()
        .map_err(|_| WorkStorageError::Unavailable)?
        .unwrap_or_else(WorkVersion::initial);
    if request.event.version() != expected_event_version {
        return Err(WorkStorageError::VersionConflict);
    }

    history.push(request.event.clone());
    let projection =
        WorkProjection::rebuild(&history).map_err(|_| WorkStorageError::Unavailable)?;
    let owner_sequence = advance_owner_cursor(&transaction, authority)?;
    insert_event(&transaction, &request.event)?;
    publish_projection(&transaction, &projection, owner_sequence)?;
    transaction.commit().map_err(map_sqlite)?;
    Ok(WorkAppendOutcome::Appended(history))
}

fn load_registered_history(
    handle: &MigrationSqlHandle,
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
            .chain([MigrationSqlValue::Text(task_id.as_str().to_owned())])
            .collect(),
    )
    .map_err(|_| WorkStorageError::Unavailable)?;
    decode_registered_events(rows)
}

fn load_registered_history_in_transaction(
    transaction: &MigrationSqlTransaction,
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
            .chain([MigrationSqlValue::Text(task_id.as_str().to_owned())])
            .collect(),
    )
    .map_err(|_| WorkStorageError::Unavailable)?;
    decode_registered_events(rows)
}

fn decode_registered_events(rows: MigrationSqlRows) -> Result<Vec<WorkEvent>, WorkStorageError> {
    if rows.rows.is_empty() {
        return Err(WorkStorageError::NotFoundOrNotAuthorized);
    }
    rows.rows
        .into_iter()
        .map(|row| {
            let payload = migration_text(&row.values, 0).ok_or(WorkStorageError::Unavailable)?;
            serde_json::from_str(payload).map_err(|_| WorkStorageError::Unavailable)
        })
        .collect()
}

fn append_registered(
    handle: &MigrationSqlHandle,
    request: &WorkAppendRequest,
) -> Result<WorkAppendOutcome, WorkStorageError> {
    let transaction = handle
        .begin_immediate()
        .map_err(|_| WorkStorageError::Unavailable)?;
    let authority = request.event.authority();
    let task_id = request.event.task_id();
    let mut history = load_registered_history_in_transaction(&transaction, authority, task_id)
        .or_else(|error| match error {
            WorkStorageError::NotFoundOrNotAuthorized => Ok(Vec::new()),
            error => Err(error),
        })?;
    if let Some(prior) = history
        .iter()
        .find(|event| event.command_id() == request.event.command_id())
    {
        let outcome = if prior.input_digest() == request.event.input_digest() {
            Ok(WorkAppendOutcome::Replayed(history))
        } else {
            Err(WorkStorageError::IdempotencyConflict)
        };
        let _ = transaction.rollback();
        return outcome;
    }
    let current_version = history.last().map(WorkEvent::version);
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
    history.push(request.event.clone());
    let projection =
        WorkProjection::rebuild(&history).map_err(|_| WorkStorageError::Unavailable)?;
    transaction
        .execute(
            migration_statement(
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
        &transaction,
        "SELECT sequence FROM work_owner_cursors_v1
         WHERE project_id = ?1 AND repository_id = ?2 AND worktree_id = ?3
           AND actor_id = ?4 AND policy_digest = ?5",
        authority_params_owned(authority),
    )
    .map_err(|_| WorkStorageError::Unavailable)?;
    let owner_sequence = cursor_rows
        .rows
        .first()
        .and_then(|row| migration_integer(&row.values, 0))
        .and_then(|value| u64::try_from(value).ok())
        .ok_or(WorkStorageError::Unavailable)?;
    registered_insert_event(&transaction, &request.event)?;
    registered_publish_projection(&transaction, &projection, owner_sequence)?;
    transaction
        .commit()
        .map_err(|_| WorkStorageError::Unavailable)?;
    Ok(WorkAppendOutcome::Appended(history))
}

fn registered_insert_event(
    transaction: &MigrationSqlTransaction,
    event: &WorkEvent,
) -> Result<(), WorkStorageError> {
    let payload = serde_json::to_string(event).map_err(|_| WorkStorageError::Unavailable)?;
    transaction
        .execute(
            migration_statement(
                "INSERT INTO work_events_v1 (
                    project_id, repository_id, worktree_id, actor_id, policy_digest,
                    task_id, version, command_id, input_digest, occurred_at, event_payload
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
                authority_params_owned(event.authority())
                    .into_iter()
                    .chain([
                        MigrationSqlValue::Text(event.task_id().as_str().to_owned()),
                        MigrationSqlValue::Integer(
                            i64::try_from(event.version().get())
                                .map_err(|_| WorkStorageError::Unavailable)?,
                        ),
                        MigrationSqlValue::Text(event.command_id().as_str().to_owned()),
                        MigrationSqlValue::Text(event.input_digest().as_str().to_owned()),
                        MigrationSqlValue::Integer(event.occurred_at().0),
                        MigrationSqlValue::Text(payload),
                    ])
                    .collect(),
            )
            .map_err(|_| WorkStorageError::Unavailable)?,
        )
        .map_err(|_| WorkStorageError::Unavailable)?;
    Ok(())
}

fn registered_publish_projection(
    transaction: &MigrationSqlTransaction,
    projection: &WorkProjection,
    owner_sequence: u64,
) -> Result<(), WorkStorageError> {
    let payload = serde_json::to_string(projection).map_err(|_| WorkStorageError::Unavailable)?;
    let version =
        i64::try_from(projection.version().get()).map_err(|_| WorkStorageError::Unavailable)?;
    let sequence = i64::try_from(owner_sequence).map_err(|_| WorkStorageError::Unavailable)?;
    let changed = transaction
        .execute(
            migration_statement(
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
                        MigrationSqlValue::Text(projection.task_id().as_str().to_owned()),
                        MigrationSqlValue::Integer(version),
                        MigrationSqlValue::Integer(sequence),
                        projection
                            .accepted_proposal()
                            .map(|proposal| MigrationSqlValue::Text(proposal.as_str().to_owned()))
                            .unwrap_or(MigrationSqlValue::Null),
                        MigrationSqlValue::Integer(if projection.is_execution_admitted() {
                            1
                        } else {
                            0
                        }),
                        MigrationSqlValue::Integer(if projection.is_task_accepted() {
                            1
                        } else {
                            0
                        }),
                        MigrationSqlValue::Text(payload.clone()),
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
            migration_statement(
                "INSERT INTO work_projection_deltas_v1 (
                    project_id, repository_id, worktree_id, actor_id, policy_digest,
                    owner_sequence, task_id, version, projection_payload
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                authority_params_owned(projection.authority())
                    .into_iter()
                    .chain([
                        MigrationSqlValue::Integer(sequence),
                        MigrationSqlValue::Text(projection.task_id().as_str().to_owned()),
                        MigrationSqlValue::Integer(version),
                        MigrationSqlValue::Text(payload),
                    ])
                    .collect(),
            )
            .map_err(|_| WorkStorageError::Unavailable)?,
        )
        .map_err(|_| WorkStorageError::Unavailable)?;
    Ok(())
}

impl WorkProjectionReadPort for WorkSqliteStorage {
    fn snapshot(
        &self,
        authority: &WorkAuthority,
        page_size: u32,
    ) -> Result<WorkProjectionSnapshotV1, WorkProjectionPortError> {
        let connection = match &self.backend {
            WorkSqliteBackend::Direct(connection) => connection,
            WorkSqliteBackend::Registered(handle) => {
                return snapshot_registered(handle, authority, page_size);
            }
        };
        let connection = connection
            .lock()
            .map_err(|_| WorkProjectionPortError::Unavailable)?;
        let generation_id = projection_generation(authority)?;
        let sequence = WorkProjectionSequenceV1::new(
            Self::owner_cursor(&connection, authority)
                .map_err(|_| WorkProjectionPortError::Unavailable)?,
        );
        let total = connection
            .query_row(
                "SELECT COUNT(*)
                 FROM work_projection_snapshots_v1
                 WHERE project_id = ?1
                   AND repository_id = ?2
                   AND worktree_id = ?3
                   AND actor_id = ?4
                   AND policy_digest = ?5",
                authority_params(authority),
                |row| row.get::<_, u32>(0),
            )
            .map_err(|_| WorkProjectionPortError::Unavailable)?;
        let mut statement = connection
            .prepare(
                "SELECT projection_payload
                 FROM work_projection_snapshots_v1
                 WHERE project_id = ?1
                   AND repository_id = ?2
                   AND worktree_id = ?3
                   AND actor_id = ?4
                   AND policy_digest = ?5
                 ORDER BY task_id
                 LIMIT ?6",
            )
            .map_err(|_| WorkProjectionPortError::Unavailable)?;
        let rows = statement
            .query_map(
                params![
                    authority.project_id().as_str(),
                    authority.repository_id().as_str(),
                    authority.worktree_id().as_str(),
                    authority.actor_id().as_str(),
                    authority.policy_digest().as_str(),
                    page_size,
                ],
                |row| row.get::<_, String>(0),
            )
            .map_err(|_| WorkProjectionPortError::Unavailable)?;
        let mut projections = Vec::new();
        for row in rows {
            let payload = row.map_err(|_| WorkProjectionPortError::Unavailable)?;
            projections.push(
                serde_json::from_str(&payload).map_err(|_| WorkProjectionPortError::Unavailable)?,
            );
        }
        let returned =
            u32::try_from(projections.len()).map_err(|_| WorkProjectionPortError::Unavailable)?;
        let coverage = if returned == total {
            WorkProjectionCoverageV1::complete(returned, total)
                .map_err(|_| WorkProjectionPortError::Unavailable)?
        } else {
            let range =
                WorkProjectionSequenceRangeV1::new(WorkProjectionSequenceV1::new(0), sequence)
                    .map_err(|_| WorkProjectionPortError::Unavailable)?;
            let cursor = projection_cursor(generation_id.clone(), sequence)?;
            WorkProjectionCoverageV1::capped(returned, total, page_size, range, cursor)
                .map_err(|_| WorkProjectionPortError::Unavailable)?
        };
        WorkProjectionSnapshotV1::new(generation_id, sequence, projections, coverage)
            .map_err(|_| WorkProjectionPortError::Unavailable)
    }

    fn delta(
        &self,
        authority: &WorkAuthority,
        cursor: &WorkProjectionResumeCursorV1,
        page_size: u32,
    ) -> Result<WorkProjectionDeltaV1, WorkProjectionPortError> {
        if let WorkSqliteBackend::Registered(handle) = &self.backend {
            return delta_registered(handle, authority, cursor, page_size);
        }
        let generation_id = projection_generation(authority)?;
        if cursor.generation_id() != &generation_id {
            return Err(WorkProjectionPortError::StaleCursor);
        }
        let from_sequence = parse_projection_cursor(cursor)?;
        let from_sequence_sql =
            i64::try_from(from_sequence).map_err(|_| WorkProjectionPortError::StaleCursor)?;
        let WorkSqliteBackend::Direct(connection) = &self.backend else {
            unreachable!();
        };
        let connection = connection
            .lock()
            .map_err(|_| WorkProjectionPortError::Unavailable)?;
        let current_sequence = Self::owner_cursor(&connection, authority)
            .map_err(|_| WorkProjectionPortError::Unavailable)?;
        if from_sequence >= current_sequence {
            return Err(WorkProjectionPortError::StaleCursor);
        }
        let total = connection
            .query_row(
                "SELECT COUNT(DISTINCT task_id)
                 FROM work_projection_deltas_v1
                 WHERE project_id = ?1
                   AND repository_id = ?2
                   AND worktree_id = ?3
                   AND actor_id = ?4
                   AND policy_digest = ?5
                   AND owner_sequence > ?6",
                params![
                    authority.project_id().as_str(),
                    authority.repository_id().as_str(),
                    authority.worktree_id().as_str(),
                    authority.actor_id().as_str(),
                    authority.policy_digest().as_str(),
                    from_sequence_sql,
                ],
                |row| row.get::<_, u32>(0),
            )
            .map_err(|_| WorkProjectionPortError::Unavailable)?;
        let mut statement = connection
            .prepare(
                "WITH latest AS (
                    SELECT task_id, MAX(owner_sequence) AS owner_sequence
                    FROM work_projection_deltas_v1
                    WHERE project_id = ?1
                      AND repository_id = ?2
                      AND worktree_id = ?3
                      AND actor_id = ?4
                      AND policy_digest = ?5
                      AND owner_sequence > ?6
                    GROUP BY task_id
                 )
                 SELECT delta.projection_payload, latest.owner_sequence
                 FROM latest
                 JOIN work_projection_deltas_v1 AS delta
                   ON delta.project_id = ?1
                  AND delta.repository_id = ?2
                  AND delta.worktree_id = ?3
                  AND delta.actor_id = ?4
                  AND delta.policy_digest = ?5
                  AND delta.task_id = latest.task_id
                  AND delta.owner_sequence = latest.owner_sequence
                 ORDER BY latest.owner_sequence
                 LIMIT ?7",
            )
            .map_err(|_| WorkProjectionPortError::Unavailable)?;
        let rows = statement
            .query_map(
                params![
                    authority.project_id().as_str(),
                    authority.repository_id().as_str(),
                    authority.worktree_id().as_str(),
                    authority.actor_id().as_str(),
                    authority.policy_digest().as_str(),
                    from_sequence_sql,
                    page_size,
                ],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?)),
            )
            .map_err(|_| WorkProjectionPortError::Unavailable)?;
        let mut changed = Vec::new();
        let mut to_sequence = from_sequence;
        for row in rows {
            let (payload, owner_sequence) =
                row.map_err(|_| WorkProjectionPortError::Unavailable)?;
            let owner_sequence =
                u64::try_from(owner_sequence).map_err(|_| WorkProjectionPortError::Unavailable)?;
            changed.push(
                serde_json::from_str(&payload).map_err(|_| WorkProjectionPortError::Unavailable)?,
            );
            to_sequence = to_sequence.max(owner_sequence);
        }
        let returned =
            u32::try_from(changed.len()).map_err(|_| WorkProjectionPortError::Unavailable)?;
        if returned == total {
            to_sequence = current_sequence;
        }
        let from_sequence = WorkProjectionSequenceV1::new(from_sequence);
        let to_sequence = WorkProjectionSequenceV1::new(to_sequence);
        let coverage = if returned == total {
            WorkProjectionCoverageV1::complete(returned, total)
                .map_err(|_| WorkProjectionPortError::Unavailable)?
        } else {
            let range = WorkProjectionSequenceRangeV1::new(from_sequence, to_sequence)
                .map_err(|_| WorkProjectionPortError::Unavailable)?;
            let cursor = projection_cursor(generation_id.clone(), to_sequence)?;
            WorkProjectionCoverageV1::capped(returned, total, page_size, range, cursor)
                .map_err(|_| WorkProjectionPortError::Unavailable)?
        };
        WorkProjectionDeltaV1::new(
            generation_id,
            from_sequence,
            to_sequence,
            changed,
            BTreeSet::new(),
            coverage,
        )
        .map_err(|_| WorkProjectionPortError::Unavailable)
    }
}

fn snapshot_registered(
    handle: &MigrationSqlHandle,
    authority: &WorkAuthority,
    page_size: u32,
) -> Result<WorkProjectionSnapshotV1, WorkProjectionPortError> {
    let generation_id = projection_generation(authority)?;
    let sequence = WorkProjectionSequenceV1::new(registered_owner_cursor(handle, authority)?);
    let total = registered_count(
        &registered_work_query(
            handle,
            "SELECT COUNT(*) FROM work_projection_snapshots_v1
             WHERE project_id = ?1 AND repository_id = ?2 AND worktree_id = ?3
               AND actor_id = ?4 AND policy_digest = ?5",
            authority_params_owned(authority),
        )
        .map_err(|_| WorkProjectionPortError::Unavailable)?,
    )?;
    let rows = registered_work_query(
        handle,
        "SELECT projection_payload FROM work_projection_snapshots_v1
         WHERE project_id = ?1 AND repository_id = ?2 AND worktree_id = ?3
           AND actor_id = ?4 AND policy_digest = ?5
         ORDER BY task_id LIMIT ?6",
        authority_params_owned(authority)
            .into_iter()
            .chain([MigrationSqlValue::Integer(i64::from(page_size))])
            .collect(),
    )
    .map_err(|_| WorkProjectionPortError::Unavailable)?;
    let projections = decode_registered_projections(rows)?;
    let returned =
        u32::try_from(projections.len()).map_err(|_| WorkProjectionPortError::Unavailable)?;
    let coverage = if returned == total {
        WorkProjectionCoverageV1::complete(returned, total)
            .map_err(|_| WorkProjectionPortError::Unavailable)?
    } else {
        let range = WorkProjectionSequenceRangeV1::new(WorkProjectionSequenceV1::new(0), sequence)
            .map_err(|_| WorkProjectionPortError::Unavailable)?;
        WorkProjectionCoverageV1::capped(
            returned,
            total,
            page_size,
            range,
            projection_cursor(generation_id.clone(), sequence)?,
        )
        .map_err(|_| WorkProjectionPortError::Unavailable)?
    };
    WorkProjectionSnapshotV1::new(generation_id, sequence, projections, coverage)
        .map_err(|_| WorkProjectionPortError::Unavailable)
}

fn delta_registered(
    handle: &MigrationSqlHandle,
    authority: &WorkAuthority,
    cursor: &WorkProjectionResumeCursorV1,
    page_size: u32,
) -> Result<WorkProjectionDeltaV1, WorkProjectionPortError> {
    let generation_id = projection_generation(authority)?;
    if cursor.generation_id() != &generation_id {
        return Err(WorkProjectionPortError::StaleCursor);
    }
    let from = parse_projection_cursor(cursor)?;
    let from_sql = i64::try_from(from).map_err(|_| WorkProjectionPortError::StaleCursor)?;
    let current = registered_owner_cursor(handle, authority)?;
    if from >= current {
        return Err(WorkProjectionPortError::StaleCursor);
    }
    let total = registered_count(
        &registered_work_query(
            handle,
            "SELECT COUNT(DISTINCT task_id) FROM work_projection_deltas_v1
             WHERE project_id = ?1 AND repository_id = ?2 AND worktree_id = ?3
               AND actor_id = ?4 AND policy_digest = ?5 AND owner_sequence > ?6",
            authority_params_owned(authority)
                .into_iter()
                .chain([MigrationSqlValue::Integer(from_sql)])
                .collect(),
        )
        .map_err(|_| WorkProjectionPortError::Unavailable)?,
    )?;
    let rows = registered_work_query(
        handle,
        "WITH latest AS (
            SELECT task_id, MAX(owner_sequence) AS owner_sequence
            FROM work_projection_deltas_v1
            WHERE project_id = ?1 AND repository_id = ?2 AND worktree_id = ?3
              AND actor_id = ?4 AND policy_digest = ?5 AND owner_sequence > ?6
            GROUP BY task_id
         )
         SELECT delta.projection_payload, latest.owner_sequence
         FROM latest JOIN work_projection_deltas_v1 AS delta
           ON delta.project_id = ?1 AND delta.repository_id = ?2 AND delta.worktree_id = ?3
          AND delta.actor_id = ?4 AND delta.policy_digest = ?5
          AND delta.task_id = latest.task_id
          AND delta.owner_sequence = latest.owner_sequence
         ORDER BY latest.owner_sequence LIMIT ?7",
        authority_params_owned(authority)
            .into_iter()
            .chain([
                MigrationSqlValue::Integer(from_sql),
                MigrationSqlValue::Integer(i64::from(page_size)),
            ])
            .collect(),
    )
    .map_err(|_| WorkProjectionPortError::Unavailable)?;
    let mut changed = Vec::new();
    let mut to = from;
    for row in rows.rows {
        let payload = migration_text(&row.values, 0).ok_or(WorkProjectionPortError::Unavailable)?;
        changed
            .push(serde_json::from_str(payload).map_err(|_| WorkProjectionPortError::Unavailable)?);
        to = to.max(
            u64::try_from(
                migration_integer(&row.values, 1).ok_or(WorkProjectionPortError::Unavailable)?,
            )
            .map_err(|_| WorkProjectionPortError::Unavailable)?,
        );
    }
    let returned =
        u32::try_from(changed.len()).map_err(|_| WorkProjectionPortError::Unavailable)?;
    if returned == total {
        to = current;
    }
    let from = WorkProjectionSequenceV1::new(from);
    let to = WorkProjectionSequenceV1::new(to);
    let coverage = if returned == total {
        WorkProjectionCoverageV1::complete(returned, total)
            .map_err(|_| WorkProjectionPortError::Unavailable)?
    } else {
        let range = WorkProjectionSequenceRangeV1::new(from, to)
            .map_err(|_| WorkProjectionPortError::Unavailable)?;
        WorkProjectionCoverageV1::capped(
            returned,
            total,
            page_size,
            range,
            projection_cursor(generation_id.clone(), to)?,
        )
        .map_err(|_| WorkProjectionPortError::Unavailable)?
    };
    WorkProjectionDeltaV1::new(generation_id, from, to, changed, BTreeSet::new(), coverage)
        .map_err(|_| WorkProjectionPortError::Unavailable)
}

fn registered_owner_cursor(
    handle: &MigrationSqlHandle,
    authority: &WorkAuthority,
) -> Result<u64, WorkProjectionPortError> {
    let rows = registered_work_query(
        handle,
        "SELECT sequence FROM work_owner_cursors_v1
         WHERE project_id = ?1 AND repository_id = ?2 AND worktree_id = ?3
           AND actor_id = ?4 AND policy_digest = ?5",
        authority_params_owned(authority),
    )
    .map_err(|_| WorkProjectionPortError::Unavailable)?;
    match rows.rows.first() {
        None => Ok(0),
        Some(row) => u64::try_from(
            migration_integer(&row.values, 0).ok_or(WorkProjectionPortError::Unavailable)?,
        )
        .map_err(|_| WorkProjectionPortError::Unavailable),
    }
}

fn registered_count(rows: &MigrationSqlRows) -> Result<u32, WorkProjectionPortError> {
    u32::try_from(
        rows.rows
            .first()
            .and_then(|row| migration_integer(&row.values, 0))
            .ok_or(WorkProjectionPortError::Unavailable)?,
    )
    .map_err(|_| WorkProjectionPortError::Unavailable)
}

fn decode_registered_projections(
    rows: MigrationSqlRows,
) -> Result<Vec<WorkProjection>, WorkProjectionPortError> {
    rows.rows
        .into_iter()
        .map(|row| {
            serde_json::from_str(
                migration_text(&row.values, 0).ok_or(WorkProjectionPortError::Unavailable)?,
            )
            .map_err(|_| WorkProjectionPortError::Unavailable)
        })
        .collect()
}

fn projection_generation(
    authority: &WorkAuthority,
) -> Result<ProjectionGenerationId, WorkProjectionPortError> {
    let digest = canonical_sha256(&("tracedecay.work.projection.generation.v1", authority))
        .map_err(|_| WorkProjectionPortError::Unavailable)?;
    ProjectionGenerationId::try_from(format!(
        "generation.work.{}",
        digest.as_str().trim_start_matches("sha256:")
    ))
    .map_err(|_| WorkProjectionPortError::Unavailable)
}

fn projection_cursor(
    generation_id: ProjectionGenerationId,
    sequence: WorkProjectionSequenceV1,
) -> Result<WorkProjectionResumeCursorV1, WorkProjectionPortError> {
    WorkProjectionResumeCursorV1::new(
        generation_id,
        format!("work-projection-sequence.v1:{}", sequence.get()),
    )
    .map_err(|_| WorkProjectionPortError::Unavailable)
}

fn parse_projection_cursor(
    cursor: &WorkProjectionResumeCursorV1,
) -> Result<u64, WorkProjectionPortError> {
    cursor
        .token()
        .strip_prefix("work-projection-sequence.v1:")
        .and_then(|sequence| sequence.parse::<u64>().ok())
        .ok_or(WorkProjectionPortError::StaleCursor)
}

impl WorkSqliteStorage {
    pub fn execution_attempt(
        &self,
        authority: &WorkAuthority,
        identity: &WorkAttemptIdentityV1,
    ) -> Result<Option<WorkAttemptV1>, WorkExecutionPersistenceError> {
        match &self.backend {
            WorkSqliteBackend::Direct(connection) => {
                let connection = connection.lock().map_err(|_| {
                    WorkExecutionPersistenceError::Unavailable(
                        "SQLite Work runtime store lock failed".to_owned(),
                    )
                })?;
                load_attempt_snapshot(&connection, authority, identity)
                    .map_err(map_execution_persistence)
            }
            WorkSqliteBackend::Registered(handle) => {
                load_registered_attempt_snapshot(handle, authority, identity)
                    .map_err(map_execution_persistence)
            }
        }
    }

    pub fn execution_attempt_history(
        &self,
        authority: &WorkAuthority,
        identity: &WorkAttemptIdentityV1,
    ) -> Result<Vec<WorkAttemptV1>, WorkExecutionPersistenceError> {
        match &self.backend {
            WorkSqliteBackend::Direct(connection) => {
                let connection = connection.lock().map_err(|_| {
                    WorkExecutionPersistenceError::Unavailable(
                        "SQLite Work runtime store lock failed".to_owned(),
                    )
                })?;
                load_attempt_history(&connection, authority, identity)
                    .map_err(map_execution_persistence)
            }
            WorkSqliteBackend::Registered(handle) => {
                load_registered_attempt_history(handle, authority, identity)
                    .map_err(map_execution_persistence)
            }
        }
    }

    pub fn recovery_candidates(
        &self,
        authority: &WorkAuthority,
    ) -> Result<Vec<WorkAttemptV1>, WorkExecutionPersistenceError> {
        if let WorkSqliteBackend::Registered(handle) = &self.backend {
            return registered_recovery_candidates(handle, authority)
                .map_err(map_execution_persistence);
        }
        let WorkSqliteBackend::Direct(connection) = &self.backend else {
            unreachable!();
        };
        let connection = connection.lock().map_err(|_| {
            WorkExecutionPersistenceError::Unavailable(
                "SQLite Work runtime store lock failed".to_owned(),
            )
        })?;
        let mut statement = connection
            .prepare(
                "SELECT attempt_payload
                 FROM work_attempt_snapshots_v1
                 WHERE project_id = ?1
                   AND repository_id = ?2
                   AND worktree_id = ?3
                   AND actor_id = ?4
                   AND policy_digest = ?5
                   AND state = 'recovery_required'
                 ORDER BY task_id, run_id, attempt_id",
            )
            .map_err(map_runtime_sqlite)
            .map_err(map_execution_persistence)?;
        let rows = statement
            .query_map(authority_params(authority), |row| row.get::<_, String>(0))
            .map_err(map_runtime_sqlite)
            .map_err(map_execution_persistence)?;
        let mut attempts = Vec::new();
        for row in rows {
            let payload = row
                .map_err(map_runtime_sqlite)
                .map_err(map_execution_persistence)?;
            attempts.push(decode_attempt(&payload).map_err(map_execution_persistence)?);
        }
        Ok(attempts)
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
        if let WorkSqliteBackend::Registered(handle) = &self.backend {
            return append_registered_attempt(
                handle,
                authority,
                command_id,
                input_digest,
                expected,
                attempt,
            );
        }
        let WorkSqliteBackend::Direct(connection) = &self.backend else {
            unreachable!();
        };
        let mut connection = connection
            .lock()
            .map_err(|_| AttemptStoreError::Unavailable)?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(map_runtime_sqlite)?;

        if let Some((digest, _payload)) =
            load_attempt_idempotency(&transaction, authority, command_id.as_str())?
        {
            if digest != input_digest.as_str() {
                return Err(AttemptStoreError::Conflict);
            }
            return Ok(());
        }

        validate_attempt_projection(&transaction, authority, attempt)?;
        let identity = attempt.identity();
        let current = load_attempt_snapshot(&transaction, authority, identity)?;
        let revision = match current.as_ref() {
            None => {
                if expected.is_some() || attempt.state() != WorkAttemptStateV1::Leased {
                    return Err(AttemptStoreError::Conflict);
                }
                1
            }
            Some(current) => {
                if current.is_terminal() {
                    return Err(AttemptStoreError::TerminalAlreadyPublished);
                }
                if expected != Some(current) {
                    return Err(AttemptStoreError::Conflict);
                }
                validate_attempt_transition(current, attempt)?;
                let history_len = load_attempt_history(&transaction, authority, identity)?.len();
                u64::try_from(history_len)
                    .ok()
                    .and_then(|value| value.checked_add(1))
                    .ok_or(AttemptStoreError::Unavailable)?
            }
        };

        persist_attempt_event(
            &transaction,
            authority,
            command_id,
            input_digest,
            attempt,
            revision,
        )?;
        persist_attempt_snapshot(&transaction, authority, current.as_ref(), attempt, revision)?;
        persist_attempt_artifacts(&transaction, authority, attempt, revision)?;
        persist_terminal_evidence(&transaction, authority, attempt, revision)?;
        persist_attempt_idempotency(
            &transaction,
            authority,
            command_id,
            input_digest,
            attempt,
            revision,
        )?;
        transaction.commit().map_err(map_runtime_sqlite)?;
        Ok(())
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

fn attempt_params_owned(
    authority: &WorkAuthority,
    identity: &WorkAttemptIdentityV1,
) -> Vec<MigrationSqlValue> {
    authority_params_owned(authority)
        .into_iter()
        .chain([
            MigrationSqlValue::Text(identity.task_id().as_str().to_owned()),
            MigrationSqlValue::Text(identity.run_id().as_str().to_owned()),
            MigrationSqlValue::Text(identity.attempt_id().as_str().to_owned()),
        ])
        .collect()
}

fn load_registered_attempt_snapshot(
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
    let payload = migration_text(&row.values, 0).ok_or(AttemptStoreError::Unavailable)?;
    let attempt = decode_attempt(payload)?;
    if attempt.identity() != identity {
        return Err(AttemptStoreError::Unavailable);
    }
    Ok(Some(attempt))
}

fn load_registered_attempt_history(
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
        let revision = migration_integer(&row.values, 0).ok_or(AttemptStoreError::Unavailable)?;
        if usize::try_from(revision).ok() != Some(history.len() + 1) {
            return Err(AttemptStoreError::Unavailable);
        }
        let payload = migration_text(&row.values, 1).ok_or(AttemptStoreError::Unavailable)?;
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

fn registered_recovery_candidates(
    handle: &MigrationSqlHandle,
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
            let payload = migration_text(&row.values, 0).ok_or(AttemptStoreError::Unavailable)?;
            decode_attempt(payload)
        })
        .collect()
}

fn append_registered_attempt(
    handle: &MigrationSqlHandle,
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

fn load_registered_attempt_idempotency(
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
            .chain([MigrationSqlValue::Text(command_id.to_owned())])
            .collect(),
    )
    .map_err(|_| AttemptStoreError::Unavailable)?;
    let Some(row) = rows.rows.first() else {
        return Ok(None);
    };
    let digest = migration_text(&row.values, 0)
        .ok_or(AttemptStoreError::Unavailable)?
        .to_owned();
    let payload = migration_text(&row.values, 1)
        .ok_or(AttemptStoreError::Unavailable)?
        .to_owned();
    Ok(Some((digest, payload)))
}

fn validate_registered_attempt_projection(
    source: &impl RegisteredWorkQuery,
    authority: &WorkAuthority,
    attempt: &WorkAttemptV1,
) -> AttemptStoreResult<()> {
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
            .chain([MigrationSqlValue::Text(
                attempt.identity().task_id().as_str().to_owned(),
            )])
            .collect(),
    )
    .map_err(|_| AttemptStoreError::Unavailable)?;
    let row = rows.rows.first().ok_or(AttemptStoreError::Conflict)?;
    let owner_sequence = migration_integer(&row.values, 0).ok_or(AttemptStoreError::Unavailable)?;
    let payload = migration_text(&row.values, 1).ok_or(AttemptStoreError::Unavailable)?;
    let projection: WorkProjection =
        serde_json::from_str(payload).map_err(|_| AttemptStoreError::Unavailable)?;
    let owner_sequence =
        u64::try_from(owner_sequence).map_err(|_| AttemptStoreError::Unavailable)?;
    if attempt.projection_binding().sequence().get() > owner_sequence {
        return Err(AttemptStoreError::InvalidRequest);
    }
    attempt
        .validate_projection(&projection)
        .map_err(|_| AttemptStoreError::InvalidRequest)
}

fn persist_registered_attempt_event(
    transaction: &MigrationSqlTransaction,
    authority: &WorkAuthority,
    command_id: &WorkCommandId,
    input_digest: &ManifestDigest,
    attempt: &WorkAttemptV1,
    revision: u64,
) -> AttemptStoreResult<()> {
    let payload = serde_json::to_string(attempt).map_err(|_| AttemptStoreError::Unavailable)?;
    transaction
        .execute(
            migration_statement(
                "INSERT INTO work_attempt_events_v1 (
                    project_id, repository_id, worktree_id, actor_id, policy_digest,
                    task_id, run_id, attempt_id, revision, command_id, input_digest, attempt_payload
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
                attempt_params_owned(authority, attempt.identity())
                    .into_iter()
                    .chain([
                        MigrationSqlValue::Integer(to_sql_u64(revision)?),
                        MigrationSqlValue::Text(command_id.as_str().to_owned()),
                        MigrationSqlValue::Text(input_digest.as_str().to_owned()),
                        MigrationSqlValue::Text(payload),
                    ])
                    .collect(),
            )
            .map_err(|_| AttemptStoreError::Unavailable)?,
        )
        .map_err(|_| AttemptStoreError::Unavailable)?;
    Ok(())
}

fn persist_registered_attempt_snapshot(
    transaction: &MigrationSqlTransaction,
    authority: &WorkAuthority,
    previous: Option<&WorkAttemptV1>,
    attempt: &WorkAttemptV1,
    revision: u64,
) -> AttemptStoreResult<()> {
    let payload = serde_json::to_string(attempt).map_err(|_| AttemptStoreError::Unavailable)?;
    let mut values = attempt_params_owned(authority, attempt.identity());
    values.extend([
        MigrationSqlValue::Integer(to_sql_u64(revision)?),
        MigrationSqlValue::Text(attempt.lease().lease_id().as_str().to_owned()),
        MigrationSqlValue::Integer(to_sql_u64(attempt.lease().epoch().get())?),
        MigrationSqlValue::Text(attempt_state(attempt.state()).to_owned()),
        MigrationSqlValue::Text(payload),
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
                MigrationSqlValue::Integer(to_sql_u64(revision - 1)?),
                MigrationSqlValue::Text(previous.lease().lease_id().as_str().to_owned()),
                MigrationSqlValue::Integer(to_sql_u64(previous.lease().epoch().get())?),
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
        .execute(migration_statement(sql, values).map_err(|_| AttemptStoreError::Unavailable)?)
        .map_err(|_| AttemptStoreError::Unavailable)?;
    if changed.changed_rows != 1 {
        return Err(AttemptStoreError::Conflict);
    }
    Ok(())
}

fn persist_registered_attempt_artifacts(
    transaction: &MigrationSqlTransaction,
    authority: &WorkAuthority,
    attempt: &WorkAttemptV1,
    revision: u64,
) -> AttemptStoreResult<()> {
    for artifact in attempt.artifacts() {
        let params = attempt_params_owned(authority, attempt.identity())
            .into_iter()
            .chain([
                MigrationSqlValue::Text(artifact.artifact_id().as_str().to_owned()),
                MigrationSqlValue::Text(artifact.digest().as_str().to_owned()),
                MigrationSqlValue::Integer(to_sql_u64(artifact.byte_length())?),
                MigrationSqlValue::Integer(to_sql_u64(revision)?),
            ])
            .collect();
        transaction
            .execute(
                migration_statement(
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
                    MigrationSqlValue::Text(artifact.artifact_id().as_str().to_owned()),
                    MigrationSqlValue::Text(artifact.digest().as_str().to_owned()),
                    MigrationSqlValue::Integer(to_sql_u64(artifact.byte_length())?),
                ])
                .collect(),
        )
        .map_err(|_| AttemptStoreError::Unavailable)?;
        if rows
            .rows
            .first()
            .and_then(|row| migration_integer(&row.values, 0))
            != Some(1)
        {
            return Err(AttemptStoreError::InvalidRequest);
        }
    }
    Ok(())
}

fn persist_registered_terminal_evidence(
    transaction: &MigrationSqlTransaction,
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
            migration_statement(
                "INSERT INTO work_attempt_terminal_evidence_v1 (
                    project_id, repository_id, worktree_id, actor_id, policy_digest,
                    task_id, run_id, attempt_id, revision, terminal_payload
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
                attempt_params_owned(authority, attempt.identity())
                    .into_iter()
                    .chain([
                        MigrationSqlValue::Integer(to_sql_u64(revision)?),
                        MigrationSqlValue::Text(payload),
                    ])
                    .collect(),
            )
            .map_err(|_| AttemptStoreError::Unavailable)?,
        )
        .map_err(|_| AttemptStoreError::TerminalAlreadyPublished)?;
    Ok(())
}

fn persist_registered_attempt_idempotency(
    transaction: &MigrationSqlTransaction,
    authority: &WorkAuthority,
    command_id: &WorkCommandId,
    input_digest: &ManifestDigest,
    attempt: &WorkAttemptV1,
    revision: u64,
) -> AttemptStoreResult<()> {
    let payload = serde_json::to_string(attempt).map_err(|_| AttemptStoreError::Unavailable)?;
    transaction
        .execute(
            migration_statement(
                "INSERT INTO work_attempt_idempotency_v1 (
                    project_id, repository_id, worktree_id, actor_id, policy_digest,
                    command_id, input_digest, task_id, run_id, attempt_id, revision, attempt_payload
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
                authority_params_owned(authority)
                    .into_iter()
                    .chain([
                        MigrationSqlValue::Text(command_id.as_str().to_owned()),
                        MigrationSqlValue::Text(input_digest.as_str().to_owned()),
                        MigrationSqlValue::Text(attempt.identity().task_id().as_str().to_owned()),
                        MigrationSqlValue::Text(attempt.identity().run_id().as_str().to_owned()),
                        MigrationSqlValue::Text(
                            attempt.identity().attempt_id().as_str().to_owned(),
                        ),
                        MigrationSqlValue::Integer(to_sql_u64(revision)?),
                        MigrationSqlValue::Text(payload),
                    ])
                    .collect(),
            )
            .map_err(|_| AttemptStoreError::Unavailable)?,
        )
        .map_err(|_| AttemptStoreError::Unavailable)?;
    Ok(())
}

fn load_attempt_snapshot(
    connection: &Connection,
    authority: &WorkAuthority,
    identity: &WorkAttemptIdentityV1,
) -> AttemptStoreResult<Option<WorkAttemptV1>> {
    let payload = connection
        .query_row(
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
            attempt_params(authority, identity),
            |row| row.get::<_, String>(0),
        )
        .optional()
        .map_err(map_runtime_sqlite)?;
    payload
        .map(|payload| {
            let attempt = decode_attempt(&payload)?;
            if attempt.identity() != identity {
                return Err(AttemptStoreError::Unavailable);
            }
            Ok(attempt)
        })
        .transpose()
}

fn load_attempt_history(
    connection: &Connection,
    authority: &WorkAuthority,
    identity: &WorkAttemptIdentityV1,
) -> AttemptStoreResult<Vec<WorkAttemptV1>> {
    let mut statement = connection
        .prepare(
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
        )
        .map_err(map_runtime_sqlite)?;
    let rows = statement
        .query_map(attempt_params(authority, identity), |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
        })
        .map_err(map_runtime_sqlite)?;
    let mut history = Vec::new();
    for row in rows {
        let (revision, payload) = row.map_err(map_runtime_sqlite)?;
        if usize::try_from(revision).ok() != Some(history.len() + 1) {
            return Err(AttemptStoreError::Unavailable);
        }
        let attempt = decode_attempt(&payload)?;
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

fn load_attempt_idempotency(
    connection: &Connection,
    authority: &WorkAuthority,
    command_id: &str,
) -> AttemptStoreResult<Option<(String, String)>> {
    connection
        .query_row(
            "SELECT input_digest, attempt_payload
             FROM work_attempt_idempotency_v1
             WHERE project_id = ?1
               AND repository_id = ?2
               AND worktree_id = ?3
               AND actor_id = ?4
               AND policy_digest = ?5
               AND command_id = ?6",
            params![
                authority.project_id().as_str(),
                authority.repository_id().as_str(),
                authority.worktree_id().as_str(),
                authority.actor_id().as_str(),
                authority.policy_digest().as_str(),
                command_id,
            ],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()
        .map_err(map_runtime_sqlite)
}

fn validate_attempt_projection(
    connection: &Connection,
    authority: &WorkAuthority,
    attempt: &WorkAttemptV1,
) -> AttemptStoreResult<()> {
    let stored = connection
        .query_row(
            "SELECT owner_sequence, projection_payload
             FROM work_projection_snapshots_v1
             WHERE project_id = ?1
               AND repository_id = ?2
               AND worktree_id = ?3
               AND actor_id = ?4
               AND policy_digest = ?5
               AND task_id = ?6",
            params![
                authority.project_id().as_str(),
                authority.repository_id().as_str(),
                authority.worktree_id().as_str(),
                authority.actor_id().as_str(),
                authority.policy_digest().as_str(),
                attempt.identity().task_id().as_str(),
            ],
            |row| Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?)),
        )
        .optional()
        .map_err(map_runtime_sqlite)?
        .ok_or(AttemptStoreError::Conflict)?;
    let projection: WorkProjection =
        serde_json::from_str(&stored.1).map_err(|_| AttemptStoreError::Unavailable)?;
    let owner_sequence = u64::try_from(stored.0).map_err(|_| AttemptStoreError::Unavailable)?;
    if attempt.projection_binding().sequence().get() > owner_sequence {
        return Err(AttemptStoreError::InvalidRequest);
    }
    attempt
        .validate_projection(&projection)
        .map_err(|_| AttemptStoreError::InvalidRequest)
}

fn validate_attempt_transition(
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
            || candidate.actual_route() != previous.actual_route()
            || candidate.terminal() != previous.terminal()
        {
            return Err(AttemptStoreError::InvalidRequest);
        }
        WorkAttemptV1::new(
            previous.identity().clone(),
            previous.projection_binding().clone(),
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

fn persist_attempt_event(
    connection: &Connection,
    authority: &WorkAuthority,
    command_id: &WorkCommandId,
    input_digest: &ManifestDigest,
    attempt: &WorkAttemptV1,
    revision: u64,
) -> AttemptStoreResult<()> {
    let identity = attempt.identity();
    let payload = serde_json::to_string(attempt).map_err(|_| AttemptStoreError::Unavailable)?;
    connection
        .execute(
            "INSERT INTO work_attempt_events_v1 (
                project_id, repository_id, worktree_id, actor_id, policy_digest,
                task_id, run_id, attempt_id, revision, command_id, input_digest, attempt_payload
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
            params![
                authority.project_id().as_str(),
                authority.repository_id().as_str(),
                authority.worktree_id().as_str(),
                authority.actor_id().as_str(),
                authority.policy_digest().as_str(),
                identity.task_id().as_str(),
                identity.run_id().as_str(),
                identity.attempt_id().as_str(),
                to_sql_u64(revision)?,
                command_id.as_str(),
                input_digest.as_str(),
                payload,
            ],
        )
        .map_err(map_runtime_sqlite)?;
    Ok(())
}

fn persist_attempt_snapshot(
    connection: &Connection,
    authority: &WorkAuthority,
    previous: Option<&WorkAttemptV1>,
    attempt: &WorkAttemptV1,
    revision: u64,
) -> AttemptStoreResult<()> {
    let identity = attempt.identity();
    let payload = serde_json::to_string(attempt).map_err(|_| AttemptStoreError::Unavailable)?;
    let changed = match previous {
        None => connection.execute(
            "INSERT INTO work_attempt_snapshots_v1 (
                project_id, repository_id, worktree_id, actor_id, policy_digest,
                task_id, run_id, attempt_id, revision, lease_id, fence_epoch, state, attempt_payload
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
            params![
                authority.project_id().as_str(),
                authority.repository_id().as_str(),
                authority.worktree_id().as_str(),
                authority.actor_id().as_str(),
                authority.policy_digest().as_str(),
                identity.task_id().as_str(),
                identity.run_id().as_str(),
                identity.attempt_id().as_str(),
                to_sql_u64(revision)?,
                attempt.lease().lease_id().as_str(),
                to_sql_u64(attempt.lease().epoch().get())?,
                attempt_state(attempt.state()),
                payload,
            ],
        ),
        Some(previous) => connection.execute(
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
            params![
                authority.project_id().as_str(),
                authority.repository_id().as_str(),
                authority.worktree_id().as_str(),
                authority.actor_id().as_str(),
                authority.policy_digest().as_str(),
                identity.task_id().as_str(),
                identity.run_id().as_str(),
                identity.attempt_id().as_str(),
                to_sql_u64(revision)?,
                attempt.lease().lease_id().as_str(),
                to_sql_u64(attempt.lease().epoch().get())?,
                attempt_state(attempt.state()),
                payload,
                to_sql_u64(revision - 1)?,
                previous.lease().lease_id().as_str(),
                to_sql_u64(previous.lease().epoch().get())?,
            ],
        ),
    }
    .map_err(map_runtime_sqlite)?;
    if changed != 1 {
        return Err(AttemptStoreError::Conflict);
    }
    Ok(())
}

fn persist_attempt_artifacts(
    connection: &Connection,
    authority: &WorkAuthority,
    attempt: &WorkAttemptV1,
    revision: u64,
) -> AttemptStoreResult<()> {
    let identity = attempt.identity();
    for artifact in attempt.artifacts() {
        connection
            .execute(
                "INSERT INTO work_attempt_artifacts_v1 (
                    project_id, repository_id, worktree_id, actor_id, policy_digest,
                    task_id, run_id, attempt_id, artifact_id, digest, byte_length, first_revision
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)
                 ON CONFLICT (
                    project_id, repository_id, worktree_id, actor_id, policy_digest,
                    task_id, run_id, attempt_id, artifact_id
                 ) DO NOTHING",
                params![
                    authority.project_id().as_str(),
                    authority.repository_id().as_str(),
                    authority.worktree_id().as_str(),
                    authority.actor_id().as_str(),
                    authority.policy_digest().as_str(),
                    identity.task_id().as_str(),
                    identity.run_id().as_str(),
                    identity.attempt_id().as_str(),
                    artifact.artifact_id().as_str(),
                    artifact.digest().as_str(),
                    to_sql_u64(artifact.byte_length())?,
                    to_sql_u64(revision)?,
                ],
            )
            .map_err(map_runtime_sqlite)?;
        let exact: i64 = connection
            .query_row(
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
                params![
                    authority.project_id().as_str(),
                    authority.repository_id().as_str(),
                    authority.worktree_id().as_str(),
                    authority.actor_id().as_str(),
                    authority.policy_digest().as_str(),
                    identity.task_id().as_str(),
                    identity.run_id().as_str(),
                    identity.attempt_id().as_str(),
                    artifact.artifact_id().as_str(),
                    artifact.digest().as_str(),
                    to_sql_u64(artifact.byte_length())?,
                ],
                |row| row.get(0),
            )
            .map_err(map_runtime_sqlite)?;
        if exact != 1 {
            return Err(AttemptStoreError::InvalidRequest);
        }
    }
    Ok(())
}

fn persist_terminal_evidence(
    connection: &Connection,
    authority: &WorkAuthority,
    attempt: &WorkAttemptV1,
    revision: u64,
) -> AttemptStoreResult<()> {
    let Some(terminal) = attempt.terminal() else {
        return Ok(());
    };
    let identity = attempt.identity();
    let payload = serde_json::to_string(terminal).map_err(|_| AttemptStoreError::Unavailable)?;
    connection
        .execute(
            "INSERT INTO work_attempt_terminal_evidence_v1 (
                project_id, repository_id, worktree_id, actor_id, policy_digest,
                task_id, run_id, attempt_id, revision, terminal_payload
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![
                authority.project_id().as_str(),
                authority.repository_id().as_str(),
                authority.worktree_id().as_str(),
                authority.actor_id().as_str(),
                authority.policy_digest().as_str(),
                identity.task_id().as_str(),
                identity.run_id().as_str(),
                identity.attempt_id().as_str(),
                to_sql_u64(revision)?,
                payload,
            ],
        )
        .map_err(|error| {
            if matches!(
                error,
                rusqlite::Error::SqliteFailure(
                    rusqlite::ffi::Error {
                        code: rusqlite::ErrorCode::ConstraintViolation,
                        ..
                    },
                    _
                )
            ) {
                AttemptStoreError::TerminalAlreadyPublished
            } else {
                map_runtime_sqlite(error)
            }
        })?;
    Ok(())
}

fn persist_attempt_idempotency(
    connection: &Connection,
    authority: &WorkAuthority,
    command_id: &WorkCommandId,
    input_digest: &ManifestDigest,
    attempt: &WorkAttemptV1,
    revision: u64,
) -> AttemptStoreResult<()> {
    let identity = attempt.identity();
    let payload = serde_json::to_string(attempt).map_err(|_| AttemptStoreError::Unavailable)?;
    connection
        .execute(
            "INSERT INTO work_attempt_idempotency_v1 (
                project_id, repository_id, worktree_id, actor_id, policy_digest,
                command_id, input_digest, task_id, run_id, attempt_id, revision, attempt_payload
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
            params![
                authority.project_id().as_str(),
                authority.repository_id().as_str(),
                authority.worktree_id().as_str(),
                authority.actor_id().as_str(),
                authority.policy_digest().as_str(),
                command_id.as_str(),
                input_digest.as_str(),
                identity.task_id().as_str(),
                identity.run_id().as_str(),
                identity.attempt_id().as_str(),
                to_sql_u64(revision)?,
                payload,
            ],
        )
        .map_err(map_runtime_sqlite)?;
    Ok(())
}

fn application_attempt_material(
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

fn map_execution_persistence(error: AttemptStoreError) -> WorkExecutionPersistenceError {
    match error {
        AttemptStoreError::Conflict
        | AttemptStoreError::TerminalAlreadyPublished
        | AttemptStoreError::InvalidRequest => WorkExecutionPersistenceError::Conflict,
        AttemptStoreError::Unavailable => WorkExecutionPersistenceError::Unavailable(
            "SQLite Work runtime store failed".to_owned(),
        ),
    }
}

fn decode_attempt(payload: &str) -> AttemptStoreResult<WorkAttemptV1> {
    serde_json::from_str(payload).map_err(|_| AttemptStoreError::Unavailable)
}

fn attempt_state(state: WorkAttemptStateV1) -> &'static str {
    match state {
        WorkAttemptStateV1::Leased => "leased",
        WorkAttemptStateV1::Running => "running",
        WorkAttemptStateV1::CancellationRequested => "cancellation_requested",
        WorkAttemptStateV1::CancellationAcknowledged => "cancellation_acknowledged",
        WorkAttemptStateV1::CancellationEscalated => "cancellation_escalated",
        WorkAttemptStateV1::RecoveryRequired => "recovery_required",
        WorkAttemptStateV1::Succeeded => "succeeded",
        WorkAttemptStateV1::Failed => "failed",
        WorkAttemptStateV1::Cancelled => "cancelled",
    }
}

fn attempt_params<'a>(
    authority: &'a WorkAuthority,
    identity: &'a WorkAttemptIdentityV1,
) -> [&'a str; 8] {
    [
        authority.project_id().as_str(),
        authority.repository_id().as_str(),
        authority.worktree_id().as_str(),
        authority.actor_id().as_str(),
        authority.policy_digest().as_str(),
        identity.task_id().as_str(),
        identity.run_id().as_str(),
        identity.attempt_id().as_str(),
    ]
}

fn to_sql_u64(value: u64) -> AttemptStoreResult<i64> {
    i64::try_from(value).map_err(|_| AttemptStoreError::InvalidRequest)
}

fn map_runtime_sqlite(_error: rusqlite::Error) -> AttemptStoreError {
    AttemptStoreError::Unavailable
}

fn load_history(
    connection: &Connection,
    authority: &WorkAuthority,
    task_id: &TaskId,
) -> Result<Vec<WorkEvent>, WorkStorageError> {
    let mut statement = connection
        .prepare(
            "SELECT version, event_payload
             FROM work_events_v1
             WHERE project_id = ?1
               AND repository_id = ?2
               AND worktree_id = ?3
               AND actor_id = ?4
               AND policy_digest = ?5
               AND task_id = ?6
             ORDER BY version",
        )
        .map_err(map_sqlite)?;
    let rows = statement
        .query_map(
            params![
                authority.project_id().as_str(),
                authority.repository_id().as_str(),
                authority.worktree_id().as_str(),
                authority.actor_id().as_str(),
                authority.policy_digest().as_str(),
                task_id.as_str(),
            ],
            |row| Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?)),
        )
        .map_err(map_sqlite)?;
    let mut history = Vec::new();
    for row in rows {
        let (stored_version, payload) = row.map_err(map_sqlite)?;
        let event: WorkEvent =
            serde_json::from_str(&payload).map_err(|_| WorkStorageError::Unavailable)?;
        if event.authority() != authority
            || event.task_id() != task_id
            || i64::try_from(event.version().get()).ok() != Some(stored_version)
        {
            return Err(WorkStorageError::Unavailable);
        }
        history.push(event);
    }
    if history.is_empty() {
        return Err(WorkStorageError::NotFoundOrNotAuthorized);
    }
    WorkProjection::rebuild(&history).map_err(|_| WorkStorageError::Unavailable)?;
    Ok(history)
}

fn advance_owner_cursor(
    connection: &Connection,
    authority: &WorkAuthority,
) -> Result<u64, WorkStorageError> {
    let current = WorkSqliteStorage::owner_cursor(connection, authority).map_err(map_sqlite)?;
    let next = current
        .checked_add(1)
        .ok_or(WorkStorageError::Unavailable)?;
    if current == 0 {
        connection
            .execute(
                "INSERT INTO work_owner_cursors_v1 (
                    project_id, repository_id, worktree_id, actor_id, policy_digest, sequence
                 ) VALUES (?1, ?2, ?3, ?4, ?5, 1)",
                authority_params(authority),
            )
            .map_err(map_sqlite)?;
    } else {
        let changed = connection
            .execute(
                "UPDATE work_owner_cursors_v1
                 SET sequence = ?6
                 WHERE project_id = ?1
                   AND repository_id = ?2
                   AND worktree_id = ?3
                   AND actor_id = ?4
                   AND policy_digest = ?5
                   AND sequence = ?7",
                params![
                    authority.project_id().as_str(),
                    authority.repository_id().as_str(),
                    authority.worktree_id().as_str(),
                    authority.actor_id().as_str(),
                    authority.policy_digest().as_str(),
                    i64::try_from(next).map_err(|_| WorkStorageError::Unavailable)?,
                    i64::try_from(current).map_err(|_| WorkStorageError::Unavailable)?,
                ],
            )
            .map_err(map_sqlite)?;
        if changed != 1 {
            return Err(WorkStorageError::VersionConflict);
        }
    }
    Ok(next)
}

fn insert_event(connection: &Connection, event: &WorkEvent) -> Result<(), WorkStorageError> {
    let payload = serde_json::to_string(event).map_err(|_| WorkStorageError::Unavailable)?;
    connection
        .execute(
            "INSERT INTO work_events_v1 (
                project_id, repository_id, worktree_id, actor_id, policy_digest,
                task_id, version, command_id, input_digest, occurred_at, event_payload
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            params![
                event.authority().project_id().as_str(),
                event.authority().repository_id().as_str(),
                event.authority().worktree_id().as_str(),
                event.authority().actor_id().as_str(),
                event.authority().policy_digest().as_str(),
                event.task_id().as_str(),
                i64::try_from(event.version().get()).map_err(|_| WorkStorageError::Unavailable)?,
                event.command_id().as_str(),
                event.input_digest().as_str(),
                event.occurred_at().0,
                payload,
            ],
        )
        .map_err(map_sqlite)?;
    Ok(())
}

fn publish_projection(
    connection: &Connection,
    projection: &WorkProjection,
    owner_sequence: u64,
) -> Result<(), WorkStorageError> {
    let authority = projection.authority();
    let payload = serde_json::to_string(projection).map_err(|_| WorkStorageError::Unavailable)?;
    let version =
        i64::try_from(projection.version().get()).map_err(|_| WorkStorageError::Unavailable)?;
    let sequence = i64::try_from(owner_sequence).map_err(|_| WorkStorageError::Unavailable)?;
    let changed = connection
        .execute(
            "INSERT INTO work_projection_snapshots_v1 (
                project_id, repository_id, worktree_id, actor_id, policy_digest,
                task_id, version, owner_sequence, accepted_proposal_id,
                execution_admitted, task_accepted, projection_payload
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)
             ON CONFLICT (
                project_id, repository_id, worktree_id, actor_id, policy_digest, task_id
             ) DO UPDATE SET
                version = excluded.version,
                owner_sequence = excluded.owner_sequence,
                accepted_proposal_id = excluded.accepted_proposal_id,
                execution_admitted = excluded.execution_admitted,
                task_accepted = excluded.task_accepted,
                projection_payload = excluded.projection_payload
             WHERE work_projection_snapshots_v1.version + 1 = excluded.version",
            params![
                authority.project_id().as_str(),
                authority.repository_id().as_str(),
                authority.worktree_id().as_str(),
                authority.actor_id().as_str(),
                authority.policy_digest().as_str(),
                projection.task_id().as_str(),
                version,
                sequence,
                projection
                    .accepted_proposal()
                    .map(|proposal| proposal.as_str()),
                projection.is_execution_admitted(),
                projection.is_task_accepted(),
                payload,
            ],
        )
        .map_err(map_sqlite)?;
    if changed != 1 {
        return Err(WorkStorageError::VersionConflict);
    }
    connection
        .execute(
            "INSERT INTO work_projection_deltas_v1 (
                project_id, repository_id, worktree_id, actor_id, policy_digest,
                owner_sequence, task_id, version, projection_payload
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                authority.project_id().as_str(),
                authority.repository_id().as_str(),
                authority.worktree_id().as_str(),
                authority.actor_id().as_str(),
                authority.policy_digest().as_str(),
                sequence,
                projection.task_id().as_str(),
                version,
                payload,
            ],
        )
        .map_err(map_sqlite)?;
    Ok(())
}

fn authority_params(authority: &WorkAuthority) -> [&str; 5] {
    [
        authority.project_id().as_str(),
        authority.repository_id().as_str(),
        authority.worktree_id().as_str(),
        authority.actor_id().as_str(),
        authority.policy_digest().as_str(),
    ]
}

fn authority_params_owned(authority: &WorkAuthority) -> Vec<MigrationSqlValue> {
    authority_params(authority)
        .into_iter()
        .map(|value| MigrationSqlValue::Text(value.to_owned()))
        .collect()
}

fn migration_statement(
    sql: &str,
    params: Vec<MigrationSqlValue>,
) -> Result<MigrationSqlStatement, crate::migration_sql::MigrationSqlError> {
    MigrationSqlStatement::new(sql.to_owned(), params)
}

trait RegisteredWorkQuery {
    fn work_query(
        &self,
        statement: MigrationSqlStatement,
    ) -> Result<MigrationSqlRows, crate::migration_sql::MigrationSqlError>;
}

impl RegisteredWorkQuery for MigrationSqlHandle {
    fn work_query(
        &self,
        statement: MigrationSqlStatement,
    ) -> Result<MigrationSqlRows, crate::migration_sql::MigrationSqlError> {
        self.query(statement, Duration::from_secs(5))
    }
}

impl RegisteredWorkQuery for MigrationSqlTransaction {
    fn work_query(
        &self,
        statement: MigrationSqlStatement,
    ) -> Result<MigrationSqlRows, crate::migration_sql::MigrationSqlError> {
        self.query(statement)
    }
}

fn registered_work_query(
    source: &impl RegisteredWorkQuery,
    sql: &str,
    params: Vec<MigrationSqlValue>,
) -> Result<MigrationSqlRows, crate::migration_sql::MigrationSqlError> {
    source.work_query(migration_statement(sql, params)?)
}

fn migration_text(values: &[MigrationSqlValue], index: usize) -> Option<&str> {
    match values.get(index)? {
        MigrationSqlValue::Text(value) => Some(value),
        _ => None,
    }
}

fn migration_integer(values: &[MigrationSqlValue], index: usize) -> Option<i64> {
    match values.get(index)? {
        MigrationSqlValue::Integer(value) => Some(*value),
        _ => None,
    }
}

fn map_sqlite(_error: rusqlite::Error) -> WorkStorageError {
    WorkStorageError::Unavailable
}

fn invalid_storage(message: &str) -> rusqlite::Error {
    rusqlite::Error::InvalidParameterName(message.to_owned())
}
