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
CREATE TABLE IF NOT EXISTS work_attempt_fences_v1 (
    project_id TEXT NOT NULL,
    repository_id TEXT NOT NULL,
    worktree_id TEXT NOT NULL,
    actor_id TEXT NOT NULL,
    policy_digest TEXT NOT NULL,
    epoch INTEGER NOT NULL CHECK (epoch > 0),
    PRIMARY KEY (project_id, repository_id, worktree_id, actor_id, policy_digest)
) STRICT;

CREATE TABLE IF NOT EXISTS work_attempts_v1 (
    project_id TEXT NOT NULL,
    repository_id TEXT NOT NULL,
    worktree_id TEXT NOT NULL,
    actor_id TEXT NOT NULL,
    policy_digest TEXT NOT NULL,
    task_id TEXT NOT NULL,
    run_id TEXT NOT NULL,
    attempt_id TEXT NOT NULL,
    state TEXT NOT NULL,
    lease_id TEXT NOT NULL,
    fence_epoch INTEGER NOT NULL CHECK (fence_epoch > 0),
    terminal INTEGER NOT NULL CHECK (terminal IN (0, 1)),
    attempt_payload TEXT NOT NULL,
    evidence_payload TEXT,
    PRIMARY KEY (
        project_id, repository_id, worktree_id, actor_id, policy_digest,
        task_id, run_id, attempt_id
    )
) STRICT;
";

/// The canonical Work product graph authority: its immutable event journal,
/// its publication outbox, and the verified graph versions a read may serve.
///
/// This is a second, deliberately separate Work authority. `work_events_v1`
/// above is scoped by [`WorkAuthority`](tracedecay_domain::WorkAuthority)
/// (project/repository/worktree/actor/policy) and carries the task command
/// history; the product journal is scoped by the registered profile OWNER
/// (brain + profile), because that is the scope
/// `WorkProductEventV1::owner_scope` declares and the only scope its
/// authorization port resolves. The two are never joined: correlating a task
/// row with a product item would invent a correspondence neither authority
/// records.
///
/// Every measurement the product projections expose — item effort, declared
/// causal candidates, scheduled_at, deadline — lives inside `event_payload`
/// exactly as the caller declared it in the event. Nothing in this schema
/// derives, estimates, or backfills one.
pub const WORK_PRODUCT_SCHEMA_V1: &str = "
CREATE TABLE IF NOT EXISTS work_product_events_v1 (
    owner_brain_id TEXT NOT NULL,
    owner_profile_id TEXT NOT NULL,
    sequence INTEGER NOT NULL CHECK (sequence > 0),
    event_id TEXT NOT NULL,
    command_id TEXT NOT NULL,
    canonical_input_digest TEXT NOT NULL,
    expected_graph_version INTEGER
        CHECK (expected_graph_version IS NULL OR expected_graph_version > 0),
    result_graph_version INTEGER NOT NULL CHECK (result_graph_version > 0),
    occurred_at INTEGER NOT NULL,
    event_payload TEXT NOT NULL,
    PRIMARY KEY (owner_brain_id, owner_profile_id, sequence),
    UNIQUE (owner_brain_id, owner_profile_id, event_id),
    UNIQUE (owner_brain_id, owner_profile_id, command_id),
    UNIQUE (owner_brain_id, owner_profile_id, result_graph_version)
) STRICT;

CREATE TABLE IF NOT EXISTS work_product_event_outbox_v1 (
    owner_brain_id TEXT NOT NULL,
    owner_profile_id TEXT NOT NULL,
    sequence INTEGER NOT NULL CHECK (sequence > 0),
    enqueued_at INTEGER NOT NULL,
    published_at INTEGER CHECK (published_at IS NULL OR published_at >= enqueued_at),
    PRIMARY KEY (owner_brain_id, owner_profile_id, sequence)
) STRICT;

CREATE TABLE IF NOT EXISTS work_product_graph_versions_v1 (
    owner_brain_id TEXT NOT NULL,
    owner_profile_id TEXT NOT NULL,
    graph_version INTEGER NOT NULL CHECK (graph_version > 0),
    event_sequence INTEGER NOT NULL CHECK (event_sequence > 0),
    valid_at INTEGER NOT NULL,
    observed_at INTEGER NOT NULL CHECK (observed_at >= valid_at),
    source_watermark TEXT NOT NULL,
    recovered_graph_digest TEXT NOT NULL,
    PRIMARY KEY (owner_brain_id, owner_profile_id, graph_version),
    UNIQUE (owner_brain_id, owner_profile_id, event_sequence)
) STRICT;
";

pub fn install_work_schema(connection: &Connection) -> rusqlite::Result<()> {
    connection.execute_batch(WORK_SCHEMA_V1)?;
    connection.execute_batch(WORK_PRODUCT_SCHEMA_V1)
}
