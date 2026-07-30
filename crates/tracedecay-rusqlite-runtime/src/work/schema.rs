//! The Work tables, installed as one idempotent batch.

use super::*;

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

CREATE TABLE IF NOT EXISTS work_projection_fold_state_v1 (
    project_id TEXT NOT NULL,
    repository_id TEXT NOT NULL,
    worktree_id TEXT NOT NULL,
    actor_id TEXT NOT NULL,
    policy_digest TEXT NOT NULL,
    task_id TEXT NOT NULL,
    version INTEGER NOT NULL CHECK (version > 0),
    state_version INTEGER NOT NULL CHECK (state_version > 0),
    state_payload TEXT NOT NULL,
    PRIMARY KEY (project_id, repository_id, worktree_id, actor_id, policy_digest, task_id)
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

pub fn install_work_schema(connection: &Connection) -> rusqlite::Result<()> {
    connection.execute_batch(WORK_SCHEMA_V1)
}
