pub(crate) const OBSERVABILITY_ROLLUP_SCHEMA_V1: &str = r#"
CREATE TABLE IF NOT EXISTS observability_rollup_generations (
    scope_ref TEXT NOT NULL,
    day_start_seconds INTEGER NOT NULL,
    generation INTEGER NOT NULL CHECK(generation > 0),
    projector_revision TEXT NOT NULL,
    source_watermark INTEGER NOT NULL CHECK(source_watermark >= 0),
    coverage TEXT NOT NULL CHECK(coverage IN ('known','partial','stale','unknown','sampled','capped')),
    content_digest TEXT NOT NULL CHECK(length(content_digest) = 64),
    fragment_json TEXT NOT NULL CHECK(json_valid(fragment_json)),
    retention_checked_at_seconds INTEGER,
    published_at_seconds INTEGER NOT NULL DEFAULT (unixepoch()),
    PRIMARY KEY(scope_ref, day_start_seconds),
    CHECK(day_start_seconds >= 0 AND day_start_seconds % 86400 = 0)
) STRICT;
CREATE INDEX IF NOT EXISTS idx_observability_rollup_generations_retention
    ON observability_rollup_generations(day_start_seconds);

CREATE TABLE IF NOT EXISTS observability_rollup_rebuild_journal (
    scope_ref TEXT NOT NULL,
    day_start_seconds INTEGER NOT NULL,
    idempotency_key TEXT NOT NULL,
    request_digest TEXT NOT NULL CHECK(length(request_digest) = 64),
    generation INTEGER NOT NULL CHECK(generation > 0),
    projector_revision TEXT NOT NULL,
    source_watermark INTEGER NOT NULL CHECK(source_watermark >= 0),
    coverage TEXT NOT NULL CHECK(coverage IN ('known','partial','stale','unknown','sampled','capped')),
    content_digest TEXT NOT NULL CHECK(length(content_digest) = 64),
    late_correction INTEGER NOT NULL CHECK(late_correction IN (0, 1)),
    published_at_seconds INTEGER NOT NULL DEFAULT (unixepoch()),
    PRIMARY KEY(scope_ref, day_start_seconds, idempotency_key),
    CHECK(day_start_seconds >= 0 AND day_start_seconds % 86400 = 0)
) STRICT;
CREATE INDEX IF NOT EXISTS idx_observability_rollup_journal_retention
    ON observability_rollup_rebuild_journal(day_start_seconds);

CREATE TABLE IF NOT EXISTS observability_rollup_dirty_days (
    scope_ref TEXT NOT NULL,
    day_start_seconds INTEGER NOT NULL,
    source_watermark INTEGER NOT NULL CHECK(source_watermark > 0),
    claimant_id TEXT,
    lease_until_seconds INTEGER,
    PRIMARY KEY(scope_ref, day_start_seconds),
    CHECK(day_start_seconds >= 0 AND day_start_seconds % 86400 = 0),
    CHECK (
        (claimant_id IS NULL AND lease_until_seconds IS NULL)
        OR (claimant_id IS NOT NULL AND lease_until_seconds IS NOT NULL)
    )
) STRICT;
CREATE INDEX IF NOT EXISTS idx_observability_rollup_dirty_days_claim
    ON observability_rollup_dirty_days(scope_ref, lease_until_seconds, day_start_seconds);
CREATE INDEX IF NOT EXISTS idx_observability_rollup_dirty_days_retention
    ON observability_rollup_dirty_days(day_start_seconds);

CREATE TABLE IF NOT EXISTS observability_rollup_frontiers (
    scope_ref TEXT PRIMARY KEY,
    coverage_start_day_seconds INTEGER NOT NULL,
    next_day_start_seconds INTEGER NOT NULL,
    claimant_id TEXT,
    lease_until_seconds INTEGER,
    CHECK(coverage_start_day_seconds >= 0 AND coverage_start_day_seconds % 86400 = 0),
    CHECK(next_day_start_seconds >= coverage_start_day_seconds
          AND next_day_start_seconds % 86400 = 0),
    CHECK (
        (claimant_id IS NULL AND lease_until_seconds IS NULL)
        OR (claimant_id IS NOT NULL AND lease_until_seconds IS NOT NULL)
    )
) STRICT;

CREATE TRIGGER IF NOT EXISTS observability_rollup_mark_topology_day_dirty
AFTER INSERT ON analytics_events
WHEN NEW.provider = 'tracedecay-observability'
 AND NEW.timestamp >= 0
 AND NEW.event_kind IN (
    'work.execution_topology.sampled.v1',
    'work.conflict_prediction.observed.v1',
    'work.conflict_outcome.linked.v1',
    'work.integration.transition.observed.v1',
    'work.github_stack_capability.observed.v1',
    'work.duplicate_effort.observed.v1',
    'work.blocked_interval.observed.v1',
    'work.rerun.observed.v1',
    'work.execution_leak.observed.v1',
    'work.delivery_fanout.observed.v1',
    'telemetry.drop.observed.v1'
 )
BEGIN
    INSERT INTO observability_rollup_dirty_days
        (scope_ref, day_start_seconds, source_watermark)
    VALUES (NEW.project_id, (NEW.timestamp / 86400) * 86400, NEW.id)
    ON CONFLICT(scope_ref, day_start_seconds) DO UPDATE SET
        source_watermark = MAX(source_watermark, excluded.source_watermark),
        claimant_id = NULL,
        lease_until_seconds = NULL;
    UPDATE observability_rollup_frontiers
    SET claimant_id = NULL, lease_until_seconds = NULL
    WHERE scope_ref = NEW.project_id
      AND next_day_start_seconds = (NEW.timestamp / 86400) * 86400;
END;
"#;
