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

CREATE TABLE IF NOT EXISTS workflow_definitions (
    definition_id TEXT NOT NULL,
    definition_version INTEGER NOT NULL CHECK (definition_version > 0),
    payload TEXT NOT NULL,
    payload_digest TEXT NOT NULL,
    PRIMARY KEY (definition_id, definition_version)
) STRICT;

CREATE TABLE IF NOT EXISTS workflow_activations (
    definition_id TEXT NOT NULL PRIMARY KEY,
    active_version INTEGER NOT NULL CHECK (active_version > 0)
) STRICT;

CREATE TABLE IF NOT EXISTS workflow_handoffs (
    token_digest TEXT NOT NULL PRIMARY KEY,
    scope_payload TEXT NOT NULL,
    issued_at INTEGER NOT NULL,
    expires_at INTEGER NOT NULL CHECK (expires_at > issued_at),
    consumed INTEGER NOT NULL CHECK (consumed IN (0, 1))
) STRICT;

CREATE TABLE IF NOT EXISTS workflow_run_events (
    run_id TEXT NOT NULL,
    sequence INTEGER NOT NULL CHECK (sequence > 0),
    command_id TEXT NOT NULL,
    input_digest TEXT NOT NULL,
    occurred_at INTEGER NOT NULL,
    event_payload TEXT NOT NULL,
    event_digest TEXT NOT NULL,
    PRIMARY KEY (run_id, sequence),
    UNIQUE (run_id, command_id)
) STRICT;

CREATE TABLE IF NOT EXISTS workflow_run_heads (
    run_id TEXT NOT NULL PRIMARY KEY,
    sequence INTEGER NOT NULL CHECK (sequence > 0),
    projection_payload TEXT NOT NULL,
    projection_digest TEXT NOT NULL,
    last_event_digest TEXT NOT NULL
) STRICT;

CREATE TABLE IF NOT EXISTS workflow_schema (
    singleton INTEGER NOT NULL PRIMARY KEY CHECK (singleton = 1),
    schema_version INTEGER NOT NULL CHECK (schema_version = 1),
    definition_digest TEXT NOT NULL
) STRICT;

INSERT OR IGNORE INTO workflow_schema (singleton, schema_version, definition_digest)
VALUES (
    1,
    1,
    'sha256:8e61c252fbcb854975c11b29b52d04a1d9209a16e036237c21a54d3b21ad5190'
);
";

pub fn install_work_schema(connection: &Connection) -> rusqlite::Result<()> {
    connection.execute_batch(WORK_SCHEMA_V1)
}
