//! Concrete SQLite persistence for the application-owned Work authority.

use std::sync::{Arc, Mutex};

use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use tracedecay_application::{
    WorkAppendOutcome, WorkAppendRequest, WorkStorageError, WorkStoragePort,
};
use tracedecay_domain::{TaskId, WorkAuthority, WorkEvent, WorkProjection, WorkVersion};

const WORK_SCHEMA_V1: &str = "
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
";

pub fn install_work_schema(connection: &Connection) -> rusqlite::Result<()> {
    connection.execute_batch(WORK_SCHEMA_V1)
}

#[derive(Clone)]
pub struct WorkSqliteStorage {
    connection: Arc<Mutex<Connection>>,
}

impl WorkSqliteStorage {
    pub fn new(connection: Arc<Mutex<Connection>>) -> Self {
        Self { connection }
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
}

impl WorkStoragePort for WorkSqliteStorage {
    fn load(
        &self,
        authority: &WorkAuthority,
        task_id: &TaskId,
    ) -> Result<Vec<WorkEvent>, WorkStorageError> {
        let connection = self
            .connection
            .lock()
            .map_err(|_| WorkStorageError::Unavailable)?;
        load_history(&connection, authority, task_id)
    }

    fn append(&self, request: &WorkAppendRequest) -> Result<WorkAppendOutcome, WorkStorageError> {
        let mut connection = self
            .connection
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

fn map_sqlite(_error: rusqlite::Error) -> WorkStorageError {
    WorkStorageError::Unavailable
}

fn invalid_storage(message: &str) -> rusqlite::Error {
    rusqlite::Error::InvalidParameterName(message.to_owned())
}
