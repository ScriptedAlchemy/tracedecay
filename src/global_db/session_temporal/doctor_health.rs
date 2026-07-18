use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use libsql::{Builder, Connection, OpenFlags};
use serde::{Deserialize, Serialize};

use crate::global_db::GlobalDb;

const MAX_FINDING_COUNT: u64 = 1_000_000;
const SESSION_TEMPORAL_SCHEMA_VERSION: i64 = 2;
const SQLITE_CORRUPT_VTAB: i32 = 267;
/// `SQLITE_OPEN_URI` — not exposed by libsql's [`OpenFlags`], so we OR the raw
/// bit (matches `sqlite_read_snapshot` / cursor composer immutable opens).
const SQLITE_OPEN_URI: i32 = 0x0000_0040;

const OCCURRENCE_FTS_CHECK_SQL: &str = "SELECT
    (SELECT COUNT(*) FROM (
        SELECT rowid AS id FROM session_occurrences
        EXCEPT SELECT id FROM session_occurrences_fts_docsize
        LIMIT 1000001
    ))
    + (SELECT COUNT(*) FROM (
        SELECT id FROM session_occurrences_fts_docsize
        EXCEPT SELECT rowid AS id FROM session_occurrences
        LIMIT 1000001
    ))
    + COALESCE((
        SELECT 0 FROM session_occurrences_fts
        WHERE session_occurrences_fts MATCH 'tracedecay_health_probe_token'
        LIMIT 1
    ), 0)";
const SUMMARY_FTS_CHECK_SQL: &str = "SELECT
    (SELECT COUNT(*) FROM (
        SELECT rowid AS id FROM session_summary_nodes
        EXCEPT SELECT id FROM session_summary_nodes_fts_docsize
        LIMIT 1000001
    ))
    + (SELECT COUNT(*) FROM (
        SELECT id FROM session_summary_nodes_fts_docsize
        EXCEPT SELECT rowid AS id FROM session_summary_nodes
        LIMIT 1000001
    ))
    + COALESCE((
        SELECT 0 FROM session_summary_nodes_fts
        WHERE session_summary_nodes_fts MATCH 'tracedecay_health_probe_token'
        LIMIT 1
    ), 0)";

const REQUIRED_TABLES: &[&str] = &[
    "lcm_summary_nodes",
    "lcm_summary_sources",
    "observations",
    "retrieval_anchors",
    "sanitization_receipts",
    "session_agent_hierarchy_edges",
    "session_agents",
    "session_assertion_supersession",
    "session_assertions",
    "session_current_entities",
    "session_external_payload_manifests",
    "session_logical_copy_edges",
    "session_occurrences",
    "session_occurrences_fts",
    "session_occurrences_fts_docsize",
    "session_query_cursor_keys",
    "session_refresh_batch_bindings",
    "session_refresh_bindings",
    "session_refresh_operations",
    "session_refresh_progress",
    "session_refresh_receipts",
    "session_summary_availability",
    "session_summary_nodes",
    "session_summary_nodes_fts",
    "session_summary_nodes_fts_docsize",
    "session_summary_sources",
    "session_summary_successors",
    "session_temporal_generations",
    "session_temporal_migration_receipts",
    "session_temporal_observation_effects",
    "session_temporal_projection_receipts",
    "session_temporal_schema_migrations",
    "session_thread_hierarchy_edges",
    "session_threads",
    "session_turn_members",
    "session_turns",
];

const REQUIRED_INDEXES: &[&str] = &[
    "idx_session_agent_hierarchy_edges_child",
    "idx_session_assertion_supersession_successor",
    "idx_session_assertions_generation_order",
    "idx_session_assertions_kind_order",
    "idx_session_assertions_object_order",
    "idx_session_assertions_subject",
    "idx_session_current_entities_assertion",
    "idx_session_current_entities_occurrence",
    "idx_session_external_payload_manifests_session",
    "idx_session_logical_copy_edges_target",
    "idx_session_occurrences_agent",
    "idx_session_occurrences_anchor_order",
    "idx_session_occurrences_generation_order",
    "idx_session_occurrences_message",
    "idx_session_occurrences_root_generation_order",
    "idx_session_occurrences_session_time",
    "idx_session_occurrences_thread",
    "idx_session_occurrences_turn",
    "idx_session_query_cursor_keys_active",
    "idx_session_refresh_operations_join",
    "idx_session_refresh_operations_one_running",
    "idx_session_refresh_operations_state",
    "idx_session_refresh_receipts_session",
    "idx_session_summary_availability_generation",
    "idx_session_summary_nodes_root_created_order",
    "idx_session_summary_nodes_session_created",
    "idx_session_summary_sources_anchor",
    "idx_session_summary_sources_summary",
    "idx_session_summary_successors_successor",
    "idx_session_temporal_generations_one_active",
    "idx_session_temporal_generations_session_state",
    "idx_session_temporal_migration_receipts_source",
    "idx_session_temporal_observation_effects_session",
    "idx_session_thread_hierarchy_edges_child",
    "idx_session_turn_members_occurrence",
];

const REQUIRED_COLUMNS: &[(&str, &[&str])] = &[
    (
        "session_temporal_schema_migrations",
        &["name", "version", "applied_at"],
    ),
    (
        "session_summary_nodes",
        &[
            "summary_id",
            "session_id",
            "summary_anchor_id",
            "summary_text",
            "index_text",
            "source_horizon_json",
            "publication_json",
            "created_at",
        ],
    ),
    (
        "session_summary_sources",
        &[
            "summary_id",
            "source_ordinal",
            "source_kind",
            "source_anchor_id",
            "source_summary_id",
        ],
    ),
    (
        "session_summary_successors",
        &[
            "predecessor_summary_id",
            "successor_summary_id",
            "created_at",
        ],
    ),
    (
        "session_external_payload_manifests",
        &[
            "payload_ref",
            "session_id",
            "payload_digest",
            "manifest_json",
            "receipt_id",
            "created_at",
        ],
    ),
    (
        "session_refresh_operations",
        &[
            "session_id",
            "operation_id",
            "request_digest",
            "target_frontier_json",
            "state",
            "created_at",
            "updated_at",
            "terminal_at",
            "failure_code",
        ],
    ),
    (
        "session_refresh_bindings",
        &[
            "session_id",
            "operation_id",
            "scope_kind",
            "source_frontier",
            "target_frontier",
            "projector_version",
            "config_digest",
            "generation",
            "frozen_watermarks_json",
            "binding_digest",
            "created_at",
        ],
    ),
    (
        "session_refresh_progress",
        &[
            "session_id",
            "operation_id",
            "progress_ordinal",
            "frontier_json",
            "coverage_json",
            "committed_batches",
            "committed_records",
            "recorded_at",
        ],
    ),
    (
        "session_refresh_batch_bindings",
        &[
            "session_id",
            "operation_id",
            "progress_ordinal",
            "generation",
            "batch_ordinal",
        ],
    ),
    (
        "session_refresh_receipts",
        &[
            "session_id",
            "operation_id",
            "terminal_state",
            "frontier_json",
            "coverage_json",
            "failure_code",
            "terminal_at",
        ],
    ),
    (
        "session_query_cursor_keys",
        &[
            "key_id",
            "key_version",
            "key_material",
            "created_at",
            "retired_at",
        ],
    ),
    (
        "session_temporal_generations",
        &[
            "session_id",
            "generation",
            "state",
            "frozen_watermarks_json",
            "created_at",
            "ready_at",
            "activated_at",
            "completed_at",
        ],
    ),
    (
        "session_temporal_projection_receipts",
        &[
            "session_id",
            "generation",
            "batch_ordinal",
            "batch_digest",
            "frozen_watermarks_json",
            "source_through",
            "projection_through",
            "occurrence_count",
            "occurrence_digest",
            "dimension_count",
            "dimension_digest",
            "copy_count",
            "copy_digest",
            "assertion_count",
            "assertion_digest",
            "supersession_count",
            "supersession_digest",
            "current_count",
            "current_digest",
            "fts_count",
            "fts_digest",
            "committed_at",
        ],
    ),
    (
        "session_temporal_observation_effects",
        &[
            "observation_id",
            "observation_sequence",
            "session_id",
            "receipt_id",
            "effect_digest",
            "output_count",
            "recorded_at",
        ],
    ),
    (
        "session_turns",
        &[
            "session_id",
            "generation",
            "turn_id",
            "ordinal",
            "grouping_provenance",
            "created_at",
        ],
    ),
    (
        "session_threads",
        &[
            "session_id",
            "generation",
            "thread_id",
            "grouping_provenance",
            "created_at",
        ],
    ),
    (
        "session_agents",
        &[
            "session_id",
            "generation",
            "agent_id",
            "agent_json",
            "created_at",
        ],
    ),
    (
        "session_occurrences",
        &[
            "session_id",
            "generation",
            "occurrence_id",
            "source_observation_id",
            "projection_output_ordinal",
            "retrieval_anchor_id",
            "thread_id",
            "thread_grouping_json",
            "turn_id",
            "turn_grouping_json",
            "message_id",
            "agent_id",
            "role",
            "knowledge_at",
            "valid_time_json",
            "evidence_json",
            "snippet_text",
            "index_text",
        ],
    ),
    (
        "session_logical_copy_edges",
        &[
            "session_id",
            "generation",
            "occurrence_id",
            "copied_from_occurrence_id",
            "proof_json",
            "created_at",
        ],
    ),
    (
        "session_turn_members",
        &[
            "session_id",
            "generation",
            "turn_id",
            "occurrence_id",
            "ordinal",
        ],
    ),
    (
        "session_thread_hierarchy_edges",
        &[
            "session_id",
            "generation",
            "parent_thread_id",
            "child_thread_id",
            "ordinal",
        ],
    ),
    (
        "session_agent_hierarchy_edges",
        &[
            "session_id",
            "generation",
            "parent_agent_id",
            "child_agent_id",
            "ordinal",
        ],
    ),
    (
        "session_assertions",
        &[
            "session_id",
            "generation",
            "assertion_id",
            "assertion_kind",
            "subject_anchor_id",
            "object_anchor_id",
            "knowledge_at",
            "valid_time_json",
            "evidence_json",
        ],
    ),
    (
        "session_assertion_supersession",
        &[
            "session_id",
            "generation",
            "superseded_assertion_id",
            "superseding_assertion_id",
            "created_at",
        ],
    ),
    (
        "session_current_entities",
        &[
            "session_id",
            "generation",
            "entity_kind",
            "entity_id",
            "current_assertion_id",
            "current_occurrence_id",
            "coverage_json",
        ],
    ),
    (
        "session_summary_availability",
        &[
            "session_id",
            "generation",
            "summary_id",
            "availability",
            "source_horizon_json",
            "reason",
            "checked_at",
        ],
    ),
    (
        "session_temporal_migration_receipts",
        &[
            "session_id",
            "generation",
            "batch_ordinal",
            "source_digest",
            "frozen_watermarks_json",
            "imported_items",
            "committed_at",
        ],
    ),
    ("session_occurrences_fts", &["index_text", "snippet_text"]),
    ("session_summary_nodes_fts", &["summary_text", "index_text"]),
];

const REQUIRED_TRIGGERS: &[(&str, &str)] = &[
    (
        "session_occurrences_fts_insert_v1",
        "CREATE TRIGGER session_occurrences_fts_insert_v1
         AFTER INSERT ON session_occurrences BEGIN
             INSERT INTO session_occurrences_fts(rowid, index_text, snippet_text)
             VALUES (NEW.rowid, NEW.index_text, NEW.snippet_text);
         END",
    ),
    (
        "session_occurrences_fts_delete_v1",
        "CREATE TRIGGER session_occurrences_fts_delete_v1
         AFTER DELETE ON session_occurrences BEGIN
             INSERT INTO session_occurrences_fts(
                 session_occurrences_fts, rowid, index_text, snippet_text
             )
             VALUES ('delete', OLD.rowid, OLD.index_text, OLD.snippet_text);
         END",
    ),
    (
        "session_occurrences_fts_update_v1",
        "CREATE TRIGGER session_occurrences_fts_update_v1
         AFTER UPDATE OF index_text, snippet_text ON session_occurrences BEGIN
             INSERT INTO session_occurrences_fts(
                 session_occurrences_fts, rowid, index_text, snippet_text
             )
             VALUES ('delete', OLD.rowid, OLD.index_text, OLD.snippet_text);
             INSERT INTO session_occurrences_fts(rowid, index_text, snippet_text)
             VALUES (NEW.rowid, NEW.index_text, NEW.snippet_text);
         END",
    ),
    (
        "session_summary_nodes_fts_insert_v1",
        "CREATE TRIGGER session_summary_nodes_fts_insert_v1
         AFTER INSERT ON session_summary_nodes BEGIN
             INSERT INTO session_summary_nodes_fts(rowid, summary_text, index_text)
             VALUES (NEW.rowid, NEW.summary_text, NEW.index_text);
         END",
    ),
    (
        "session_summary_nodes_fts_delete_v1",
        "CREATE TRIGGER session_summary_nodes_fts_delete_v1
         AFTER DELETE ON session_summary_nodes BEGIN
             INSERT INTO session_summary_nodes_fts(
                 session_summary_nodes_fts, rowid, summary_text, index_text
             )
             VALUES ('delete', OLD.rowid, OLD.summary_text, OLD.index_text);
         END",
    ),
    (
        "session_summary_nodes_fts_update_v1",
        "CREATE TRIGGER session_summary_nodes_fts_update_v1
         AFTER UPDATE OF summary_text, index_text ON session_summary_nodes BEGIN
             INSERT INTO session_summary_nodes_fts(
                 session_summary_nodes_fts, rowid, summary_text, index_text
             )
             VALUES ('delete', OLD.rowid, OLD.summary_text, OLD.index_text);
             INSERT INTO session_summary_nodes_fts(rowid, summary_text, index_text)
             VALUES (NEW.rowid, NEW.summary_text, NEW.index_text);
         END",
    ),
];

const CHECKS: &[HealthCheck] = &[
    HealthCheck {
        kind: SessionTemporalHealthFindingKind::OccurrenceFtsCorruption,
        tables: &[
            "session_occurrences",
            "session_occurrences_fts",
            "session_occurrences_fts_docsize",
        ],
        sql: OCCURRENCE_FTS_CHECK_SQL,
    },
    HealthCheck {
        kind: SessionTemporalHealthFindingKind::SummaryFtsCorruption,
        tables: &[
            "session_summary_nodes",
            "session_summary_nodes_fts",
            "session_summary_nodes_fts_docsize",
        ],
        sql: SUMMARY_FTS_CHECK_SQL,
    },
    HealthCheck {
        kind: SessionTemporalHealthFindingKind::SummaryCycle,
        tables: &["session_summary_sources"],
        sql: "WITH RECURSIVE reachable(origin, current) AS (
                SELECT summary_id, source_summary_id
                FROM session_summary_sources
                WHERE source_summary_id IS NOT NULL
                UNION
                SELECT reachable.origin, source.source_summary_id
                FROM reachable
                JOIN session_summary_sources AS source
                  ON source.summary_id = reachable.current
                WHERE source.source_summary_id IS NOT NULL
            )
            SELECT COUNT(*) FROM reachable WHERE origin = current",
    },
    HealthCheck {
        kind: SessionTemporalHealthFindingKind::StaleClosure,
        tables: &[
            "session_summary_availability",
            "session_summary_nodes",
            "session_summary_sources",
            "session_summary_successors",
            "session_temporal_generations",
        ],
        sql: "WITH RECURSIVE expected_stale(session_id, summary_id) AS (
                SELECT predecessor.session_id, dependent.summary_id
                FROM session_summary_successors AS successor
                JOIN session_summary_nodes AS predecessor
                  ON predecessor.summary_id = successor.predecessor_summary_id
                JOIN session_summary_sources AS dependent
                  ON dependent.source_summary_id = successor.predecessor_summary_id
                UNION
                SELECT expected_stale.session_id, dependent.summary_id
                FROM expected_stale
                JOIN session_summary_sources AS dependent
                  ON dependent.source_summary_id = expected_stale.summary_id
            )
            SELECT COUNT(*)
            FROM expected_stale
            JOIN session_temporal_generations AS generation
              ON generation.session_id = expected_stale.session_id
             AND generation.state = 'active'
            LEFT JOIN session_summary_availability AS availability
              ON availability.session_id = expected_stale.session_id
             AND availability.generation = generation.generation
             AND availability.summary_id = expected_stale.summary_id
            WHERE availability.availability IS NULL
               OR availability.availability <> 'stale'",
    },
    HealthCheck {
        kind: SessionTemporalHealthFindingKind::MissingAnchor,
        tables: &[
            "retrieval_anchors",
            "session_assertions",
            "session_occurrences",
            "session_summary_nodes",
            "session_summary_sources",
        ],
        sql: "SELECT
            (SELECT COUNT(*) FROM session_summary_nodes AS node
             LEFT JOIN retrieval_anchors AS anchor
               ON anchor.anchor_id = node.summary_anchor_id
             WHERE anchor.anchor_id IS NULL)
            + (SELECT COUNT(*) FROM session_summary_sources AS source
               LEFT JOIN retrieval_anchors AS anchor
                 ON anchor.anchor_id = source.source_anchor_id
               WHERE source.source_kind = 'anchor' AND anchor.anchor_id IS NULL)
            + (SELECT COUNT(*) FROM session_occurrences AS occurrence
               LEFT JOIN retrieval_anchors AS anchor
                 ON anchor.anchor_id = occurrence.retrieval_anchor_id
               WHERE anchor.anchor_id IS NULL)
            + (SELECT COUNT(*) FROM session_assertions AS assertion
               LEFT JOIN retrieval_anchors AS subject
                 ON subject.anchor_id = assertion.subject_anchor_id
               LEFT JOIN retrieval_anchors AS object
                 ON object.anchor_id = assertion.object_anchor_id
               WHERE subject.anchor_id IS NULL OR object.anchor_id IS NULL)",
    },
    HealthCheck {
        kind: SessionTemporalHealthFindingKind::MissingReceipt,
        tables: &[
            "sanitization_receipts",
            "session_external_payload_manifests",
            "session_refresh_batch_bindings",
            "session_summary_nodes",
            "session_temporal_observation_effects",
            "session_temporal_projection_receipts",
        ],
        sql: "SELECT
            (SELECT COUNT(*) FROM session_external_payload_manifests AS manifest
             LEFT JOIN sanitization_receipts AS receipt
               ON receipt.receipt_id = manifest.receipt_id
             WHERE receipt.receipt_id IS NULL)
            + (SELECT COUNT(*) FROM session_temporal_observation_effects AS effect
               LEFT JOIN sanitization_receipts AS receipt
                 ON receipt.receipt_id = effect.receipt_id
               WHERE receipt.receipt_id IS NULL)
            + (SELECT COUNT(*) FROM session_summary_nodes AS summary
               LEFT JOIN sanitization_receipts AS receipt
                 ON receipt.receipt_id = json_extract(summary.publication_json, '$.receipt_id')
               WHERE summary.publication_json IS NULL OR receipt.receipt_id IS NULL)
            + (SELECT COUNT(*) FROM session_refresh_batch_bindings AS binding
               LEFT JOIN session_temporal_projection_receipts AS receipt
                 ON receipt.session_id = binding.session_id
                AND receipt.generation = binding.generation
                AND receipt.batch_ordinal = binding.batch_ordinal
               WHERE receipt.session_id IS NULL)",
    },
    HealthCheck {
        kind: SessionTemporalHealthFindingKind::InvalidGeneration,
        tables: &["session_temporal_generations"],
        sql: "SELECT COUNT(*) FROM session_temporal_generations
            WHERE generation <= 0
               OR json_valid(frozen_watermarks_json) = 0
               OR CASE WHEN json_valid(frozen_watermarks_json) = 1 THEN (
                    json_type(frozen_watermarks_json, '$.active_generation') IS NOT 'integer'
                    OR CAST(json_extract(
                        frozen_watermarks_json, '$.active_generation'
                    ) AS INTEGER) <= 0
                    OR CAST(json_extract(
                        frozen_watermarks_json, '$.active_generation'
                    ) AS INTEGER) > generation
                    OR json_type(
                        frozen_watermarks_json, '$.source_frontier'
                    ) IS NOT 'integer'
                    OR CAST(json_extract(
                        frozen_watermarks_json, '$.source_frontier'
                    ) AS INTEGER) < 0
                    OR json_type(
                        frozen_watermarks_json, '$.projection_frontier'
                    ) IS NOT 'integer'
                    OR CAST(json_extract(
                        frozen_watermarks_json, '$.projection_frontier'
                    ) AS INTEGER) < 0
                    OR json_type(
                        frozen_watermarks_json, '$.summary_frontier'
                    ) IS NOT 'integer'
                    OR CAST(json_extract(
                        frozen_watermarks_json, '$.summary_frontier'
                    ) AS INTEGER) < 0
                    OR NOT (
                         (state = 'building' AND ready_at IS NULL
                              AND activated_at IS NULL AND completed_at IS NULL)
                      OR (state = 'ready' AND ready_at IS NOT NULL
                              AND activated_at IS NULL AND completed_at IS NULL)
                      OR (state = 'active' AND ready_at IS NOT NULL
                              AND activated_at IS NOT NULL AND completed_at IS NULL)
                      OR (state = 'superseded' AND ready_at IS NOT NULL
                              AND activated_at IS NOT NULL AND completed_at IS NOT NULL)
                      OR (state IN ('failed', 'cancelled') AND completed_at IS NOT NULL)
                    )
               ) ELSE 0 END",
    },
    HealthCheck {
        kind: SessionTemporalHealthFindingKind::MultiActiveGeneration,
        tables: &["session_temporal_generations"],
        sql: "SELECT COUNT(*) FROM (
                SELECT session_id
                FROM session_temporal_generations
                WHERE state = 'active'
                GROUP BY session_id
                HAVING COUNT(*) > 1
            )",
    },
    HealthCheck {
        kind: SessionTemporalHealthFindingKind::CursorChainAbsent,
        tables: &["session_query_cursor_keys", "session_temporal_generations"],
        sql: "SELECT
            (SELECT COUNT(*) FROM session_query_cursor_keys AS key
             WHERE key.key_version > 1
               AND NOT EXISTS (
                   SELECT 1 FROM session_query_cursor_keys AS predecessor
                   WHERE predecessor.key_version = key.key_version - 1
               ))
            + (SELECT CASE
                 WHEN EXISTS(
                     SELECT 1 FROM session_temporal_generations
                     WHERE state = 'active'
                 ) AND (
                     SELECT COUNT(*) FROM session_query_cursor_keys
                     WHERE retired_at IS NULL
                 ) <> 1
                 THEN 1 ELSE 0 END)",
    },
    HealthCheck {
        kind: SessionTemporalHealthFindingKind::CursorKeyAbsent,
        tables: &["session_query_cursor_keys", "session_temporal_generations"],
        sql: "SELECT COUNT(*)
            FROM session_temporal_generations AS generation
            LEFT JOIN session_query_cursor_keys AS key
              ON key.key_id = json_extract(
                    generation.frozen_watermarks_json, '$.cursor_key.key_id'
                 )
             AND key.key_version = CAST(json_extract(
                    generation.frozen_watermarks_json, '$.cursor_key.version'
                 ) AS INTEGER)
             AND key.retired_at IS NULL
            WHERE generation.state = 'active'
              AND (
                  json_type(generation.frozen_watermarks_json, '$.cursor_key') IS NOT 'object'
                  OR key.key_id IS NULL
              )",
    },
    HealthCheck {
        kind: SessionTemporalHealthFindingKind::OwnershipDrift,
        tables: &[
            "session_refresh_batch_bindings",
            "session_refresh_bindings",
            "session_summary_availability",
            "session_summary_nodes",
            "session_summary_sources",
            "session_summary_successors",
        ],
        sql: "SELECT
            (SELECT COUNT(*)
             FROM session_summary_sources AS source
             JOIN session_summary_nodes AS owner
               ON owner.summary_id = source.summary_id
             LEFT JOIN session_summary_nodes AS dependency
               ON dependency.summary_id = source.source_summary_id
             WHERE source.source_kind = 'summary'
               AND (
                   dependency.summary_id IS NULL
                   OR owner.session_id IS NOT dependency.session_id
               ))
            + (SELECT COUNT(*)
               FROM session_summary_successors AS edge
               LEFT JOIN session_summary_nodes AS predecessor
                 ON predecessor.summary_id = edge.predecessor_summary_id
               LEFT JOIN session_summary_nodes AS successor
                 ON successor.summary_id = edge.successor_summary_id
               WHERE predecessor.summary_id IS NULL
                  OR successor.summary_id IS NULL
                  OR predecessor.session_id IS NOT successor.session_id)
            + (SELECT COUNT(*)
               FROM session_summary_availability AS availability
               LEFT JOIN session_summary_nodes AS summary
                 ON summary.summary_id = availability.summary_id
               WHERE summary.summary_id IS NULL
                  OR availability.session_id IS NOT summary.session_id)
            + (SELECT COUNT(*)
               FROM session_refresh_batch_bindings AS batch
               LEFT JOIN session_refresh_bindings AS binding
                 ON binding.session_id = batch.session_id
                AND binding.operation_id = batch.operation_id
               WHERE binding.operation_id IS NULL
                  OR batch.generation IS NOT binding.generation)",
    },
    HealthCheck {
        kind: SessionTemporalHealthFindingKind::StuckRefresh,
        tables: &["session_refresh_operations"],
        sql: "SELECT COUNT(*) FROM session_refresh_operations
            WHERE state = 'running'
              AND updated_at < CAST(strftime('%s', 'now') AS INTEGER) * 1000000 - 900000000",
    },
    HealthCheck {
        kind: SessionTemporalHealthFindingKind::StuckBinding,
        tables: &[
            "session_refresh_bindings",
            "session_refresh_operations",
            "session_temporal_generations",
        ],
        sql: "SELECT COUNT(*)
            FROM session_refresh_operations AS operation
            LEFT JOIN session_refresh_bindings AS binding
              ON binding.session_id = operation.session_id
             AND binding.operation_id = operation.operation_id
            LEFT JOIN session_temporal_generations AS generation
              ON generation.session_id = binding.session_id
             AND generation.generation = binding.generation
            WHERE operation.state = 'running'
              AND (
                  binding.operation_id IS NULL
                  OR generation.session_id IS NULL
                  OR generation.state <> 'building'
              )",
    },
    HealthCheck {
        kind: SessionTemporalHealthFindingKind::StuckProgress,
        tables: &[
            "session_refresh_bindings",
            "session_refresh_operations",
            "session_refresh_progress",
        ],
        sql: "SELECT COUNT(*) FROM (
                SELECT operation.session_id, operation.operation_id
                FROM session_refresh_operations AS operation
                JOIN session_refresh_bindings AS binding
                  ON binding.session_id = operation.session_id
                 AND binding.operation_id = operation.operation_id
                LEFT JOIN session_refresh_progress AS progress
                  ON progress.session_id = operation.session_id
                 AND progress.operation_id = operation.operation_id
                WHERE operation.state = 'running'
                GROUP BY operation.session_id, operation.operation_id
                HAVING (MAX(progress.recorded_at) IS NULL
                        AND MAX(operation.updated_at)
                            < CAST(strftime('%s', 'now') AS INTEGER) * 1000000 - 900000000)
                    OR MAX(progress.recorded_at)
                         < CAST(strftime('%s', 'now') AS INTEGER) * 1000000 - 900000000
            )",
    },
    HealthCheck {
        kind: SessionTemporalHealthFindingKind::StuckReceipt,
        tables: &["session_refresh_operations", "session_refresh_receipts"],
        sql: "SELECT COUNT(*)
            FROM session_refresh_operations AS operation
            LEFT JOIN session_refresh_receipts AS receipt
              ON receipt.session_id = operation.session_id
             AND receipt.operation_id = operation.operation_id
            WHERE (operation.state = 'running' AND receipt.operation_id IS NOT NULL)
               OR (operation.state <> 'running' AND receipt.operation_id IS NULL)
               OR (receipt.operation_id IS NOT NULL
                   AND (
                       receipt.terminal_state <> operation.state
                       OR receipt.terminal_at IS NOT operation.terminal_at
                       OR receipt.failure_code IS NOT operation.failure_code
                   ))",
    },
    HealthCheck {
        kind: SessionTemporalHealthFindingKind::CompatibilityDrift,
        tables: &["lcm_summary_nodes", "session_summary_nodes"],
        sql: "SELECT COUNT(*)
            FROM session_summary_nodes AS canonical
            LEFT JOIN lcm_summary_nodes AS compatibility
              ON compatibility.node_id = canonical.summary_id
            WHERE compatibility.node_id IS NULL
               OR canonical.publication_json IS NULL
               OR json_extract(canonical.publication_json, '$.summary_hash') IS NULL
               OR compatibility.session_id <> canonical.session_id
               OR compatibility.summary_text <> canonical.summary_text
               OR compatibility.summary_hash
                    <> json_extract(canonical.publication_json, '$.summary_hash')",
    },
];

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum SessionTemporalHealthStatus {
    Complete,
    Partial,
    Unavailable,
    Locked,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum SessionTemporalHealthFindingKind {
    TriggerAuditDrift,
    OccurrenceFtsCorruption,
    SummaryFtsCorruption,
    SummaryCycle,
    StaleClosure,
    MissingAnchor,
    MissingReceipt,
    InvalidGeneration,
    MultiActiveGeneration,
    CursorChainAbsent,
    CursorKeyAbsent,
    OwnershipDrift,
    StuckRefresh,
    StuckBinding,
    StuckProgress,
    StuckReceipt,
    MigrationGap,
    CompatibilityDrift,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(crate) struct SessionTemporalHealthFinding {
    kind: SessionTemporalHealthFindingKind,
    count: u64,
}

impl SessionTemporalHealthFinding {
    pub(crate) const fn kind(&self) -> SessionTemporalHealthFindingKind {
        self.kind
    }

    pub(crate) const fn count(&self) -> u64 {
        self.count
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(crate) struct SessionTemporalHealthReport {
    status: SessionTemporalHealthStatus,
    findings: Vec<SessionTemporalHealthFinding>,
}

impl SessionTemporalHealthReport {
    pub(crate) const fn status(&self) -> SessionTemporalHealthStatus {
        self.status
    }

    pub(crate) fn findings(&self) -> &[SessionTemporalHealthFinding] {
        &self.findings
    }

    #[cfg(test)]
    pub(crate) const fn is_fts_virtual_table_error_code_for_test(code: i32) -> bool {
        code == SQLITE_CORRUPT_VTAB
    }

    #[cfg(test)]
    pub(crate) fn is_allowed_fts_quick_check_for_test(
        message: &str,
        repair_occurrences: bool,
        repair_summaries: bool,
    ) -> bool {
        is_allowed_fts_quick_check(message, repair_occurrences, repair_summaries)
    }
}

struct HealthCheck {
    kind: SessionTemporalHealthFindingKind,
    tables: &'static [&'static str],
    sql: &'static str,
}

/// Produces a redacted temporal health snapshot through a truly non-mutating
/// SQLite open.
///
/// The path is opened with `file:…?immutable=1&mode=ro` and `PRAGMA query_only`.
/// It never acquires `DatabaseAuthority` / lock / owner files, never creates
/// WAL/SHM sidecars, never installs schema, and never starts workers. The
/// report contains only fixed finding identities and bounded counts.
pub(crate) async fn session_temporal_doctor_health_at(
    db_path: &Path,
) -> SessionTemporalHealthReport {
    if !db_path.is_file() {
        return unavailable_report(SessionTemporalHealthStatus::Unavailable);
    }
    let read = match open_immutable_doctor_read(db_path).await {
        Ok(read) => read,
        Err(error) => return unavailable_report(classify_error(&error)),
    };
    diagnose_connection(&read.conn).await
}

struct ImmutableDoctorRead {
    _db: libsql::Database,
    conn: Connection,
}

async fn open_immutable_doctor_read(db_path: &Path) -> Result<ImmutableDoctorRead, libsql::Error> {
    let uri = crate::sqlite_read_snapshot::immutable_uri(db_path).map_err(|error| {
        libsql::Error::ConnectionFailed(format!(
            "immutable doctor URI for '{}': {error}",
            db_path.display()
        ))
    })?;
    let flags = OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::from_bits_retain(SQLITE_OPEN_URI);
    let db = Builder::new_local(uri).flags(flags).build().await?;
    let conn = db.connect()?;
    conn.execute_batch("PRAGMA query_only = ON;").await?;
    Ok(ImmutableDoctorRead { _db: db, conn })
}

impl GlobalDb {
    /// Produces a redacted, non-mutating temporal health snapshot for Doctor.
    ///
    /// Delegates to [`session_temporal_doctor_health_at`] so diagnosis never
    /// uses the authority-held writer lane connection.
    pub(crate) async fn session_temporal_doctor_health(&self) -> SessionTemporalHealthReport {
        session_temporal_doctor_health_at(self.db_path()).await
    }

    /// Rebuilds only temporal FTS derived indexes after an explicit request.
    ///
    /// Diagnosis remains non-mutating. A dry run reports the bounded plan
    /// without acquiring the writer lane. Apply mode is the sole effectful
    /// path: it refuses ambiguous database, schema, trigger, or authority
    /// failures and verifies both source preservation and FTS integrity before
    /// committing the single writer-lane transaction.
    pub(crate) async fn repair_session_temporal_fts(
        &self,
        apply: bool,
    ) -> Result<(usize, usize), libsql::Error> {
        let report = session_temporal_doctor_health_at(self.db_path()).await;
        if report.status != SessionTemporalHealthStatus::Complete {
            return Err(repair_refused(
                "temporal health is unavailable, partial, or locked",
            ));
        }
        if report.findings.iter().any(|finding| {
            !matches!(
                finding.kind,
                SessionTemporalHealthFindingKind::OccurrenceFtsCorruption
                    | SessionTemporalHealthFindingKind::SummaryFtsCorruption
            )
        }) {
            return Err(repair_refused(
                "non-FTS temporal findings require daemon-owned recovery",
            ));
        }

        let repair_occurrences = report.findings.iter().any(|finding| {
            finding.kind == SessionTemporalHealthFindingKind::OccurrenceFtsCorruption
        });
        let repair_summaries = report
            .findings
            .iter()
            .any(|finding| finding.kind == SessionTemporalHealthFindingKind::SummaryFtsCorruption);
        let planned = usize::from(repair_occurrences) + usize::from(repair_summaries);
        if !apply || planned == 0 {
            return Ok((planned, 0));
        }

        let transaction = self.begin_write_transaction().await?;
        require_quick_check(&transaction, repair_occurrences, repair_summaries).await?;
        let occurrence_sources = connection_count(&transaction, "session_occurrences").await?;
        let summary_sources = connection_count(&transaction, "session_summary_nodes").await?;

        if repair_occurrences {
            transaction
                .execute(
                    "INSERT INTO session_occurrences_fts(session_occurrences_fts)
                     VALUES ('rebuild')",
                    (),
                )
                .await?;
            verify_fts_repair(
                &transaction,
                "INSERT INTO session_occurrences_fts(session_occurrences_fts, rank)
                 VALUES ('integrity-check', 1)",
                OCCURRENCE_FTS_CHECK_SQL,
            )
            .await?;
        }
        if repair_summaries {
            transaction
                .execute(
                    "INSERT INTO session_summary_nodes_fts(session_summary_nodes_fts)
                     VALUES ('rebuild')",
                    (),
                )
                .await?;
            verify_fts_repair(
                &transaction,
                "INSERT INTO session_summary_nodes_fts(session_summary_nodes_fts, rank)
                 VALUES ('integrity-check', 1)",
                SUMMARY_FTS_CHECK_SQL,
            )
            .await?;
        }

        if occurrence_sources != connection_count(&transaction, "session_occurrences").await?
            || summary_sources != connection_count(&transaction, "session_summary_nodes").await?
        {
            return Err(repair_refused(
                "authoritative temporal sources changed during FTS repair",
            ));
        }
        transaction.commit().await?;
        Ok((planned, planned))
    }
}

async fn diagnose_connection(conn: &Connection) -> SessionTemporalHealthReport {
    let inventory = match schema_inventory(conn).await {
        Ok(inventory) => inventory,
        Err(error) => return unavailable_report(classify_error(&error)),
    };
    let temporal_tables = inventory
        .tables
        .iter()
        .filter(|name| name.starts_with("session_"))
        .count();
    if temporal_tables == 0 {
        return SessionTemporalHealthReport {
            status: SessionTemporalHealthStatus::Unavailable,
            findings: vec![finding(
                SessionTemporalHealthFindingKind::MigrationGap,
                REQUIRED_TABLES.len() as u64,
            )],
        };
    }

    let mut status = SessionTemporalHealthStatus::Complete;
    let mut findings = Vec::new();
    let missing_tables = REQUIRED_TABLES
        .iter()
        .filter(|table| !inventory.tables.contains(**table))
        .count() as u64;
    if missing_tables > 0 {
        status = SessionTemporalHealthStatus::Partial;
        findings.push(finding(
            SessionTemporalHealthFindingKind::MigrationGap,
            missing_tables,
        ));
    } else {
        match schema_version(conn).await {
            Ok(Some(version)) if version == SESSION_TEMPORAL_SCHEMA_VERSION => {}
            Ok(_) => findings.push(finding(SessionTemporalHealthFindingKind::MigrationGap, 1)),
            Err(error) => {
                if is_locked(&error) {
                    return unavailable_report(SessionTemporalHealthStatus::Locked);
                }
                status = SessionTemporalHealthStatus::Partial;
            }
        }
    }

    let missing_triggers = REQUIRED_TRIGGERS
        .iter()
        .filter(|(name, expected)| match inventory.triggers.get(*name) {
            Some(actual) => normalize_sql(actual) != normalize_sql(expected),
            None => true,
        })
        .count() as u64;
    if missing_triggers > 0 {
        findings.push(finding(
            SessionTemporalHealthFindingKind::TriggerAuditDrift,
            missing_triggers,
        ));
    }

    let missing_indexes = REQUIRED_INDEXES
        .iter()
        .filter(|name| !inventory.indexes.contains(**name))
        .count() as u64;
    if missing_indexes > 0 {
        status = SessionTemporalHealthStatus::Partial;
        merge_finding(
            &mut findings,
            SessionTemporalHealthFindingKind::MigrationGap,
            missing_indexes,
        );
    }

    match column_shape_drift(conn, &inventory).await {
        Ok(0) => {}
        Ok(drift) => {
            status = SessionTemporalHealthStatus::Partial;
            merge_finding(
                &mut findings,
                SessionTemporalHealthFindingKind::MigrationGap,
                drift,
            );
        }
        Err(error) if is_locked(&error) => {
            return unavailable_report(SessionTemporalHealthStatus::Locked);
        }
        Err(_) => return unavailable_report(SessionTemporalHealthStatus::Unavailable),
    }

    for check in CHECKS {
        if check
            .tables
            .iter()
            .any(|table| !inventory.tables.contains(*table))
        {
            status = SessionTemporalHealthStatus::Partial;
            continue;
        }
        match count(conn, check.sql).await {
            Ok(0) => {}
            Ok(value) => merge_finding(&mut findings, check.kind, value),
            Err(error) if is_fts_finding(check.kind) && is_fts_virtual_table_corruption(&error) => {
                merge_finding(&mut findings, check.kind, 1);
            }
            Err(error) if is_locked(&error) => {
                return SessionTemporalHealthReport {
                    status: SessionTemporalHealthStatus::Locked,
                    findings,
                };
            }
            Err(_) => return unavailable_report(SessionTemporalHealthStatus::Unavailable),
        }
    }
    findings.sort_by_key(SessionTemporalHealthFinding::kind);
    SessionTemporalHealthReport { status, findings }
}

struct SchemaInventory {
    tables: BTreeSet<String>,
    indexes: BTreeSet<String>,
    triggers: BTreeMap<String, String>,
}

async fn schema_inventory(conn: &Connection) -> Result<SchemaInventory, libsql::Error> {
    let mut rows = conn
        .query(
            "SELECT type, name, COALESCE(sql, '') FROM sqlite_master
             WHERE type IN ('table', 'index', 'trigger')",
            (),
        )
        .await?;
    let mut tables = BTreeSet::new();
    let mut indexes = BTreeSet::new();
    let mut triggers = BTreeMap::new();
    while let Some(row) = rows.next().await? {
        let kind = row.get::<String>(0)?;
        let name = row.get::<String>(1)?;
        match kind.as_str() {
            "table" => {
                tables.insert(name);
            }
            "index" => {
                indexes.insert(name);
            }
            "trigger" => {
                triggers.insert(name, row.get::<String>(2)?);
            }
            _ => {}
        }
    }
    Ok(SchemaInventory {
        tables,
        indexes,
        triggers,
    })
}

async fn column_shape_drift(
    conn: &Connection,
    inventory: &SchemaInventory,
) -> Result<u64, libsql::Error> {
    let mut drift = 0_u64;
    for &(table, expected) in REQUIRED_COLUMNS {
        if !inventory.tables.contains(table) {
            continue;
        }
        let mut rows = conn
            .query(
                "SELECT name FROM pragma_table_info(?1) ORDER BY cid",
                libsql::params![table],
            )
            .await?;
        let mut actual = Vec::new();
        while let Some(row) = rows.next().await? {
            actual.push(row.get::<String>(0)?);
        }
        if actual.as_slice() != expected {
            drift = drift.saturating_add(1).min(MAX_FINDING_COUNT);
        }
    }
    Ok(drift)
}

fn normalize_sql(sql: &str) -> String {
    sql.chars()
        .filter(|character| !character.is_whitespace() && *character != ';')
        .collect()
}

async fn schema_version(conn: &Connection) -> Result<Option<i64>, libsql::Error> {
    let mut rows = conn
        .query(
            "SELECT version FROM session_temporal_schema_migrations
             WHERE name = 'session-temporal'",
            (),
        )
        .await?;
    rows.next().await?.map(|row| row.get(0)).transpose()
}

async fn count(conn: &Connection, sql: &str) -> Result<u64, libsql::Error> {
    let mut rows = conn.query(sql, ()).await?;
    let Some(row) = rows.next().await? else {
        return Ok(0);
    };
    let value = row.get::<i64>(0)?;
    Ok(u64::try_from(value)
        .unwrap_or_default()
        .min(MAX_FINDING_COUNT))
}

fn finding(kind: SessionTemporalHealthFindingKind, count: u64) -> SessionTemporalHealthFinding {
    SessionTemporalHealthFinding {
        kind,
        count: count.min(MAX_FINDING_COUNT),
    }
}

fn merge_finding(
    findings: &mut Vec<SessionTemporalHealthFinding>,
    kind: SessionTemporalHealthFindingKind,
    count: u64,
) {
    if let Some(existing) = findings.iter_mut().find(|finding| finding.kind == kind) {
        existing.count = existing.count.saturating_add(count).min(MAX_FINDING_COUNT);
    } else {
        findings.push(finding(kind, count));
    }
}

fn is_fts_finding(kind: SessionTemporalHealthFindingKind) -> bool {
    matches!(
        kind,
        SessionTemporalHealthFindingKind::OccurrenceFtsCorruption
            | SessionTemporalHealthFindingKind::SummaryFtsCorruption
    )
}

fn is_fts_virtual_table_corruption(error: &libsql::Error) -> bool {
    matches!(
        error,
        libsql::Error::SqliteFailure(code, _) if *code == SQLITE_CORRUPT_VTAB
    )
}

async fn require_quick_check(
    conn: &Connection,
    repair_occurrences: bool,
    repair_summaries: bool,
) -> Result<(), libsql::Error> {
    let mut rows = match conn.query("PRAGMA quick_check", ()).await {
        Ok(rows) => rows,
        Err(error) if is_fts_virtual_table_corruption(&error) => return Ok(()),
        Err(error) => return Err(error),
    };
    let mut saw_result = false;
    while let Some(row) = match rows.next().await {
        Ok(row) => row,
        Err(error) if is_fts_virtual_table_corruption(&error) => return Ok(()),
        Err(error) => return Err(error),
    } {
        saw_result = true;
        let message = row.get::<String>(0)?;
        if message != "ok"
            && !is_allowed_fts_quick_check(&message, repair_occurrences, repair_summaries)
        {
            return Err(repair_refused(
                "whole-database quick check failed; FTS repair is unsafe",
            ));
        }
    }
    if saw_result {
        Ok(())
    } else {
        Err(repair_refused("database quick check returned no result"))
    }
}

fn is_allowed_fts_quick_check(
    message: &str,
    repair_occurrences: bool,
    repair_summaries: bool,
) -> bool {
    (repair_occurrences
        && message == "malformed inverted index for FTS5 table main.session_occurrences_fts")
        || (repair_summaries
            && message == "malformed inverted index for FTS5 table main.session_summary_nodes_fts")
}

async fn connection_count(conn: &Connection, table: &str) -> Result<i64, libsql::Error> {
    let sql = match table {
        "session_occurrences" => "SELECT COUNT(*) FROM session_occurrences",
        "session_summary_nodes" => "SELECT COUNT(*) FROM session_summary_nodes",
        _ => return Err(repair_refused("unrecognized temporal source table")),
    };
    let mut rows = conn.query(sql, ()).await?;
    let Some(row) = rows.next().await? else {
        return Err(repair_refused("temporal source count returned no result"));
    };
    row.get(0)
}

async fn verify_fts_repair(
    conn: &Connection,
    integrity_sql: &str,
    drift_sql: &str,
) -> Result<(), libsql::Error> {
    conn.execute(integrity_sql, ()).await?;
    let mut rows = conn.query(drift_sql, ()).await?;
    let Some(row) = rows.next().await? else {
        return Err(repair_refused(
            "temporal FTS verification returned no result",
        ));
    };
    if row.get::<i64>(0)? == 0 {
        Ok(())
    } else {
        Err(repair_refused(
            "temporal FTS verification still reports derived-index drift",
        ))
    }
}

fn repair_refused(message: &str) -> libsql::Error {
    libsql::Error::Misuse(message.to_string())
}

fn classify_error(error: &libsql::Error) -> SessionTemporalHealthStatus {
    if is_locked(error) {
        SessionTemporalHealthStatus::Locked
    } else {
        SessionTemporalHealthStatus::Unavailable
    }
}

fn is_locked(error: &libsql::Error) -> bool {
    let message = error.to_string().to_ascii_lowercase();
    message.contains("locked") || message.contains("busy")
}

fn unavailable_report(status: SessionTemporalHealthStatus) -> SessionTemporalHealthReport {
    SessionTemporalHealthReport {
        status,
        findings: Vec::new(),
    }
}
