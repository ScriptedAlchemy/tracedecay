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

-- One durable run-control aggregate per admitted run (Plan 32, \"One runtime,
-- run control, and effect budget\"). `authority_version` is the monotonic
-- control authority: every publication is a compare-and-swap against the
-- version the caller read, which is what makes a pause/resume race resolvable
-- without a second store. The aggregate itself lives in `control_payload`; the
-- columns beside it exist only so the fence can be evaluated in SQL.
CREATE TABLE IF NOT EXISTS work_run_controls_v1 (
    project_id TEXT NOT NULL,
    repository_id TEXT NOT NULL,
    worktree_id TEXT NOT NULL,
    actor_id TEXT NOT NULL,
    policy_digest TEXT NOT NULL,
    task_id TEXT NOT NULL,
    run_id TEXT NOT NULL,
    state TEXT NOT NULL CHECK (state IN ('running', 'paused')),
    authority_version INTEGER NOT NULL CHECK (authority_version > 0),
    control_payload TEXT NOT NULL,
    PRIMARY KEY (
        project_id, repository_id, worktree_id, actor_id, policy_digest,
        task_id, run_id
    )
) STRICT;

-- Revisioned receipts for pauses that actually fenced a workflow-bound
-- provider attempt. The payload carries the canonical task/run/attempt/step
-- identity and cause authority; the indexed columns are only the durable
-- recovery/outbox scan state. A terminal attempt CAS and a resume control CAS
-- close this same row transactionally.
CREATE TABLE IF NOT EXISTS work_blocked_intervals_v1 (
    project_id TEXT NOT NULL,
    repository_id TEXT NOT NULL,
    worktree_id TEXT NOT NULL,
    actor_id TEXT NOT NULL,
    policy_digest TEXT NOT NULL,
    task_id TEXT NOT NULL,
    run_id TEXT NOT NULL,
    attempt_id TEXT NOT NULL,
    step_id TEXT NOT NULL,
    cause_authority_version INTEGER NOT NULL CHECK (cause_authority_version > 0),
    started_at INTEGER NOT NULL,
    interval_revision INTEGER NOT NULL CHECK (interval_revision > 0),
    settled INTEGER NOT NULL CHECK (settled IN (0, 1)),
    observability_durable INTEGER NOT NULL CHECK (observability_durable IN (0, 1)),
    receipt_payload TEXT NOT NULL,
    PRIMARY KEY (
        project_id, repository_id, worktree_id, actor_id, policy_digest,
        task_id, run_id, attempt_id, step_id, cause_authority_version
    )
) STRICT;
CREATE INDEX IF NOT EXISTS work_blocked_intervals_observation_scan_v1
    ON work_blocked_intervals_v1 (
        project_id, repository_id, worktree_id, actor_id, policy_digest,
        settled, observability_durable, started_at, task_id, run_id, attempt_id, step_id,
        cause_authority_version
    );

-- The cursor schedules bounded scans only. A receipt leaves those scans only
-- after the retained producer durably claims its exact owner fact; queue
-- admission alone leaves it eligible, and the cursor wraps on all unclaimed
-- rows so older receipts cannot starve newer ones.
CREATE TABLE IF NOT EXISTS work_blocked_interval_observation_cursors_v1 (
    project_id TEXT NOT NULL,
    repository_id TEXT NOT NULL,
    worktree_id TEXT NOT NULL,
    actor_id TEXT NOT NULL,
    policy_digest TEXT NOT NULL,
    started_at INTEGER NOT NULL,
    task_id TEXT NOT NULL,
    run_id TEXT NOT NULL,
    attempt_id TEXT NOT NULL,
    step_id TEXT NOT NULL,
    cause_authority_version INTEGER NOT NULL CHECK (cause_authority_version > 0),
    PRIMARY KEY (project_id, repository_id, worktree_id, actor_id, policy_digest)
) STRICT;

-- One durable placement relation per admitted run (Plan 32, \"Placement,
-- topology, and safe Git effects\"). `target_root` is denormalized out of the
-- payload for exactly one reason: the partial unique index below is what makes
-- linked and isolated placements *exclusive*, and an exclusivity rule enforced
-- only in application code is one a crash can leave broken.
CREATE TABLE IF NOT EXISTS work_placements_v1 (
    project_id TEXT NOT NULL,
    repository_id TEXT NOT NULL,
    worktree_id TEXT NOT NULL,
    actor_id TEXT NOT NULL,
    policy_digest TEXT NOT NULL,
    task_id TEXT NOT NULL,
    run_id TEXT NOT NULL,
    kind TEXT NOT NULL CHECK (
        kind IN ('no_managed_placement', 'clean_in_place', 'linked_worktree', 'isolated_clone')
    ),
    target_root TEXT,
    state TEXT NOT NULL CHECK (state IN ('admitted', 'released', 'quarantined')),
    authority_version INTEGER NOT NULL CHECK (authority_version > 0),
    placement_payload TEXT NOT NULL,
    PRIMARY KEY (
        project_id, repository_id, worktree_id, actor_id, policy_digest,
        task_id, run_id
    )
) STRICT;

-- A released placement no longer holds its root, so it is excluded: the index
-- constrains holders, not history.
CREATE UNIQUE INDEX IF NOT EXISTS work_placements_v1_exclusive_root
    ON work_placements_v1 (
        project_id, repository_id, worktree_id, actor_id, policy_digest, target_root
    )
    WHERE target_root IS NOT NULL AND state IN ('admitted', 'quarantined');

-- Explicit duplicate-effort adjudications are revisioned owner facts. They
-- share the exact Work authority and transaction channel with the attempts
-- they bind; no similarity scan or observability projection can write here.
CREATE TABLE IF NOT EXISTS work_duplicate_adjudications_v1 (
    project_id TEXT NOT NULL,
    repository_id TEXT NOT NULL,
    worktree_id TEXT NOT NULL,
    actor_id TEXT NOT NULL,
    policy_digest TEXT NOT NULL,
    relation_digest TEXT NOT NULL,
    revision INTEGER NOT NULL CHECK (revision > 0),
    command_id TEXT NOT NULL,
    canonical_input_digest TEXT NOT NULL,
    work_generation TEXT NOT NULL,
    topology_generation TEXT NOT NULL,
    occurred_at INTEGER NOT NULL,
    receipt_digest TEXT NOT NULL,
    observation_state TEXT NOT NULL DEFAULT 'pending'
        CHECK (observation_state IN ('pending', 'durable')),
    receipt_payload TEXT NOT NULL,
    PRIMARY KEY (
        project_id, repository_id, worktree_id, actor_id, policy_digest,
        relation_digest, revision
    ),
    UNIQUE (
        project_id, repository_id, worktree_id, actor_id, policy_digest,
        command_id
    )
) STRICT;

-- A retry receipt and the exact new attempt it names commit together. A
-- command ID has one immutable input and a new attempt can have only one
-- predecessor, so replay cannot manufacture another retry.
CREATE TABLE IF NOT EXISTS work_retry_receipts_v1 (
    project_id TEXT NOT NULL,
    repository_id TEXT NOT NULL,
    worktree_id TEXT NOT NULL,
    actor_id TEXT NOT NULL,
    policy_digest TEXT NOT NULL,
    command_id TEXT NOT NULL,
    canonical_input_digest TEXT NOT NULL,
    task_id TEXT NOT NULL,
    run_id TEXT NOT NULL,
    original_attempt_id TEXT NOT NULL,
    new_attempt_id TEXT NOT NULL,
    restarted_at INTEGER NOT NULL,
    receipt_digest TEXT NOT NULL,
    observation_state TEXT NOT NULL DEFAULT 'pending'
        CHECK (observation_state IN ('pending', 'durable')),
    receipt_payload TEXT NOT NULL,
    PRIMARY KEY (
        project_id, repository_id, worktree_id, actor_id, policy_digest,
        command_id
    ),
    UNIQUE (
        project_id, repository_id, worktree_id, actor_id, policy_digest,
        task_id, run_id, new_attempt_id
    )
) STRICT;

-- A provider dispatch owns this receipt. It is deliberately distinct from a
-- process holder: it survives daemon restart and retains whether the source
-- could prove no effect or only an unknown terminal reconciliation.
CREATE TABLE IF NOT EXISTS work_attempt_effect_holders_v1 (
    project_id TEXT NOT NULL,
    repository_id TEXT NOT NULL,
    worktree_id TEXT NOT NULL,
    actor_id TEXT NOT NULL,
    policy_digest TEXT NOT NULL,
    task_id TEXT NOT NULL,
    run_id TEXT NOT NULL,
    attempt_id TEXT NOT NULL,
    effect_state TEXT NOT NULL CHECK (
        effect_state IN ('observational', 'intercepted', 'compound_non_repeatable')
    ),
    dispatched_at INTEGER NOT NULL CHECK (dispatched_at > 0),
    deadline INTEGER NOT NULL CHECK (deadline > dispatched_at),
    resolution TEXT NOT NULL CHECK (resolution IN ('pending', 'no_effect', 'unknown')),
    resolved_at INTEGER,
    holder_payload TEXT NOT NULL,
    PRIMARY KEY (
        project_id, repository_id, worktree_id, actor_id, policy_digest,
        task_id, run_id, attempt_id
    ),
    CHECK (
        (resolution = 'pending' AND resolved_at IS NULL)
        OR (resolution != 'pending' AND resolved_at IS NOT NULL AND resolved_at >= dispatched_at)
    )
) STRICT;

CREATE INDEX IF NOT EXISTS work_attempt_effect_holders_leak_scan_v1
ON work_attempt_effect_holders_v1 (
    project_id, repository_id, worktree_id, actor_id, policy_digest,
    resolution, deadline, dispatched_at, task_id, run_id, attempt_id
);

-- Leak verdicts are explicit revisioned facts produced by a bounded evidence
-- scan. Corrections append a new revision; prior verdicts remain replayable.
CREATE TABLE IF NOT EXISTS work_leak_adjudications_v1 (
    project_id TEXT NOT NULL,
    repository_id TEXT NOT NULL,
    worktree_id TEXT NOT NULL,
    actor_id TEXT NOT NULL,
    policy_digest TEXT NOT NULL,
    adjudication_id TEXT NOT NULL,
    revision INTEGER NOT NULL CHECK (revision > 0),
    command_id TEXT NOT NULL,
    canonical_input_digest TEXT NOT NULL,
    task_id TEXT NOT NULL,
    run_id TEXT NOT NULL,
    attempt_id TEXT NOT NULL,
    observed_at INTEGER NOT NULL,
    receipt_digest TEXT NOT NULL,
    observation_state TEXT NOT NULL DEFAULT 'pending'
        CHECK (observation_state IN ('pending', 'durable')),
    receipt_payload TEXT NOT NULL,
    PRIMARY KEY (
        project_id, repository_id, worktree_id, actor_id, policy_digest,
        adjudication_id, revision
    ),
    UNIQUE (
        project_id, repository_id, worktree_id, actor_id, policy_digest,
        command_id
    )
) STRICT;

CREATE INDEX IF NOT EXISTS work_retry_observation_pending_v1
ON work_retry_receipts_v1 (observation_state, restarted_at, command_id);

CREATE INDEX IF NOT EXISTS work_leak_observation_pending_v1
ON work_leak_adjudications_v1 (observation_state, observed_at, command_id);

CREATE INDEX IF NOT EXISTS work_duplicate_observation_pending_v1
ON work_duplicate_adjudications_v1 (observation_state, occurred_at, command_id);

CREATE INDEX IF NOT EXISTS work_duplicate_adjudications_generations_v1
ON work_duplicate_adjudications_v1 (
    project_id, repository_id, worktree_id, actor_id, policy_digest,
    work_generation, topology_generation, relation_digest, revision
);
";

/// The canonical Work product graph authority: its immutable event journal,
/// and the verified graph versions committed atomically with it.
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
