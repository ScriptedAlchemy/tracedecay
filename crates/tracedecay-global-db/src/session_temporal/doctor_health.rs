//! Session-temporal Doctor health lane.
//!
//! Diagnosis is production-mounted; repair helpers remain available for Doctor
//! tests and exclusive-maintenance callers that are still landing.
#![allow(dead_code)] // Doctor repair lane still landing; see module doc (Plan 23)

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant, UNIX_EPOCH};

use rusqlite::{Connection as RusqliteConnection, OptionalExtension};
use serde::{Deserialize, Serialize};

use crate::RegisteredGlobalDb;
use tracedecay_runtime_core::db::engine::{Error as EngineError, Executor, QueryExecutor};

use super::schema::{SESSION_TEMPORAL_SCHEMA_VERSION, TEMPORAL_TABLE_COLUMNS};

const MAX_FINDING_COUNT: u64 = 1_000_000;
const SQLITE_CORRUPT_VTAB: i32 = 267;
const SESSION_TEMPORAL_HEALTH_CACHE_TTL: Duration = Duration::from_secs(2);
const MAX_CACHED_SESSION_TEMPORAL_STORES: usize = 64;
const MAX_SYNCHRONOUS_SESSION_TEMPORAL_HEALTH_BYTES: u64 = 64 * 1024 * 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct SessionTemporalStoreFileFingerprint {
    bytes: u64,
    modified_nanos: u128,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct SessionTemporalStoreFingerprint {
    database: SessionTemporalStoreFileFingerprint,
    wal: Option<SessionTemporalStoreFileFingerprint>,
}

#[derive(Clone)]
struct CachedSessionTemporalHealth {
    fingerprint: SessionTemporalStoreFingerprint,
    observed_at: Instant,
    report: SessionTemporalHealthReport,
}

type SessionTemporalHealthCacheCell = Arc<tokio::sync::Mutex<Option<CachedSessionTemporalHealth>>>;

static SESSION_TEMPORAL_HEALTH_CACHE: OnceLock<
    Mutex<HashMap<PathBuf, SessionTemporalHealthCacheCell>>,
> = OnceLock::new();

fn session_temporal_health_cache_cell(path: &Path) -> SessionTemporalHealthCacheCell {
    let cache = SESSION_TEMPORAL_HEALTH_CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    let mut cache = cache
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if !cache.contains_key(path)
        && cache.len() >= MAX_CACHED_SESSION_TEMPORAL_STORES
        && let Some(evict) = cache
            .iter()
            .find(|(_, cell)| Arc::strong_count(cell) == 1)
            .map(|(path, _)| path.clone())
    {
        cache.remove(&evict);
    }
    Arc::clone(
        cache
            .entry(path.to_path_buf())
            .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(None))),
    )
}

fn store_file_fingerprint(
    path: &Path,
) -> std::io::Result<Option<SessionTemporalStoreFileFingerprint>> {
    match std::fs::metadata(path) {
        Ok(metadata) => Ok(Some(SessionTemporalStoreFileFingerprint {
            bytes: metadata.len(),
            modified_nanos: metadata
                .modified()?
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos(),
        })),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

fn session_temporal_store_fingerprint(
    database_path: &Path,
) -> std::io::Result<SessionTemporalStoreFingerprint> {
    let Some(database) = store_file_fingerprint(database_path)? else {
        return Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "session temporal database is absent",
        ));
    };
    let mut wal_path = database_path.as_os_str().to_os_string();
    wal_path.push("-wal");
    Ok(SessionTemporalStoreFingerprint {
        database,
        wal: store_file_fingerprint(&PathBuf::from(wal_path))?,
    })
}

fn session_temporal_store_family_bytes(database_path: &Path) -> std::io::Result<u64> {
    let database = std::fs::metadata(database_path)?.len();
    let mut wal_path = database_path.as_os_str().to_os_string();
    wal_path.push("-wal");
    match std::fs::metadata(PathBuf::from(wal_path)) {
        Ok(wal) => database.checked_add(wal.len()).ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "session temporal store size overflowed",
            )
        }),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(database),
        Err(error) => Err(error),
    }
}

fn permits_synchronous_session_temporal_health(database_path: &Path) -> bool {
    session_temporal_store_family_bytes(database_path)
        .is_ok_and(|bytes| bytes <= MAX_SYNCHRONOUS_SESSION_TEMPORAL_HEALTH_BYTES)
}

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

const REQUIRED_BASE_TABLES: &[&str] = &[
    "lcm_summary_nodes",
    "lcm_summary_sources",
    "observations",
    "retrieval_anchors",
    "sanitization_receipts",
];

const REQUIRED_FTS_SHADOW_TABLES: &[&str] = &[
    "session_occurrences_fts_docsize",
    "session_summary_nodes_fts_docsize",
];

fn required_table_names() -> impl Iterator<Item = &'static str> {
    REQUIRED_BASE_TABLES
        .iter()
        .copied()
        .chain(TEMPORAL_TABLE_COLUMNS.iter().map(|(table, _)| *table))
        .chain(REQUIRED_FTS_SHADOW_TABLES.iter().copied())
}

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
pub enum SessionTemporalHealthStatus {
    Complete,
    Partial,
    Unavailable,
    Locked,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionTemporalHealthFindingKind {
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
pub struct SessionTemporalHealthFinding {
    kind: SessionTemporalHealthFindingKind,
    count: u64,
}

impl SessionTemporalHealthFinding {
    pub const fn kind(&self) -> SessionTemporalHealthFindingKind {
        self.kind
    }

    pub const fn count(&self) -> u64 {
        self.count
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SessionTemporalHealthReport {
    status: SessionTemporalHealthStatus,
    findings: Vec<SessionTemporalHealthFinding>,
    /// Fixed machine reason for path-API unavailability (for example
    /// `uncheckpointed_wal`). Omitted when diagnosis ran against a
    /// checkpointed immutable snapshot.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    reason: Option<String>,
}

impl SessionTemporalHealthReport {
    pub const fn status(&self) -> SessionTemporalHealthStatus {
        self.status
    }

    pub fn findings(&self) -> &[SessionTemporalHealthFinding] {
        &self.findings
    }

    pub fn reason(&self) -> Option<&str> {
        self.reason.as_deref()
    }

    #[cfg(any(test, feature = "test-helpers"))]
    pub const fn is_fts_virtual_table_error_code_for_test(code: i32) -> bool {
        code == SQLITE_CORRUPT_VTAB
    }

    #[cfg(any(test, feature = "test-helpers"))]
    pub fn is_allowed_fts_quick_check_for_test(
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
/// `SQLite` open.
///
/// The path is opened with `file:…?immutable=1&mode=ro` and `PRAGMA query_only`.
/// It never acquires `DatabaseAuthority` / lock / owner files, never creates
/// WAL/SHM sidecars, never installs schema, and never starts workers. The
/// report contains only fixed finding identities and bounded counts.
///
/// `immutable=1` intentionally ignores an existing WAL. A non-empty `-wal`
/// sidecar therefore prevents a complete immutable snapshot: this API returns
/// [`SessionTemporalHealthStatus::Unavailable`] with reason
/// `uncheckpointed_wal` instead of opening `mode=ro` (which creates `-shm`)
/// or copying the family. Empty / non-SQLite placeholders return
/// `session_store_uninitialized`.
pub async fn session_temporal_doctor_health_at(db_path: &Path) -> SessionTemporalHealthReport {
    if !db_path.is_file() {
        return unavailable_report(SessionTemporalHealthStatus::Unavailable);
    }
    if !tracedecay_runtime_core::storage::has_sqlite_database_header(db_path).unwrap_or(false) {
        return unavailable_report_with_reason(
            SessionTemporalHealthStatus::Unavailable,
            Some("session_store_uninitialized"),
        );
    }
    if !permits_synchronous_session_temporal_health(db_path) {
        return unavailable_report_with_reason(
            SessionTemporalHealthStatus::Unavailable,
            Some("synchronous_diagnosis_size_budget_exceeded"),
        );
    }
    if non_empty_wal_sidecar(db_path) {
        return unavailable_report_with_reason(
            SessionTemporalHealthStatus::Unavailable,
            Some("uncheckpointed_wal"),
        );
    }
    let connection = match tracedecay_rusqlite_runtime::open_immutable_health_reader(db_path) {
        Ok(connection) => connection,
        Err(error) => {
            return unavailable_report_with_reason(
                classify_rusqlite_error(&error),
                Some("session_store_unavailable"),
            );
        }
    };
    diagnose_connection(&connection)
}

impl RegisteredGlobalDb {
    /// Produces a redacted, non-mutating temporal health snapshot through the
    /// retained registered reader pool. Identical requests coalesce behind one
    /// per-store lane and reuse a very short-lived result only while the exact
    /// database/WAL fingerprint remains unchanged.
    pub async fn session_temporal_doctor_health(&self) -> SessionTemporalHealthReport {
        let database_path = self.db_path();
        if !permits_synchronous_session_temporal_health(database_path) {
            return unavailable_report_with_reason(
                SessionTemporalHealthStatus::Unavailable,
                Some("synchronous_diagnosis_size_budget_exceeded"),
            );
        }
        let cache = session_temporal_health_cache_cell(database_path);
        let mut cached = cache.lock().await;
        let before = session_temporal_store_fingerprint(database_path).ok();
        if let (Some(fingerprint), Some(observed)) = (before, cached.as_ref())
            && observed.fingerprint == fingerprint
            && observed.observed_at.elapsed() <= SESSION_TEMPORAL_HEALTH_CACHE_TTL
        {
            return observed.report.clone();
        }
        let snapshot = match self.read_snapshot().await {
            Ok(snapshot) => snapshot,
            Err(error) => return unavailable_report(classify_engine_error(&error)),
        };
        let report = diagnose_snapshot(&snapshot).await;
        let after = session_temporal_store_fingerprint(database_path).ok();
        if let Some(fingerprint) = after.filter(|fingerprint| before == Some(*fingerprint)) {
            *cached = Some(CachedSessionTemporalHealth {
                fingerprint,
                observed_at: Instant::now(),
                report: report.clone(),
            });
        } else {
            *cached = None;
        }
        report
    }

    /// Rebuilds only temporal FTS derived indexes after an explicit request.
    ///
    /// Diagnosis remains non-mutating. A dry run reports the bounded plan
    /// without acquiring the writer lane. Apply mode is the sole effectful
    /// path: it refuses ambiguous database, schema, trigger, or authority
    /// failures and verifies both source preservation and FTS integrity before
    /// committing the single writer-lane transaction.
    pub async fn repair_session_temporal_fts(
        &self,
        apply: bool,
    ) -> tracedecay_runtime_core::db::engine::Result<(usize, usize)> {
        let report = self.session_temporal_doctor_health().await;
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

        let transaction = self
            .begin_write_transaction()
            .await
            .map_err(|error| EngineError::Runtime(error.to_string()))?;
        require_quick_check(&transaction, repair_occurrences, repair_summaries).await?;
        let occurrence_sources = connection_count(&transaction, "session_occurrences").await?;
        let summary_sources = connection_count(&transaction, "session_summary_nodes").await?;

        if repair_occurrences {
            Executor::execute(
                &transaction,
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
            Executor::execute(
                &transaction,
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
        transaction
            .commit()
            .await
            .map_err(|error| EngineError::Runtime(error.to_string()))?;
        self.checkpoint_result()
            .await
            .map_err(|_| repair_refused("temporal FTS repair checkpoint did not complete"))?;
        Ok((planned, planned))
    }
}

fn diagnose_connection(conn: &RusqliteConnection) -> SessionTemporalHealthReport {
    let inventory = match schema_inventory(conn) {
        Ok(inventory) => inventory,
        Err(error) => return unavailable_report(classify_rusqlite_error_raw(&error)),
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
                required_table_names().count() as u64,
            )],
            reason: None,
        };
    }

    let mut status = SessionTemporalHealthStatus::Complete;
    let mut findings = Vec::new();
    let missing_tables = required_table_names()
        .filter(|table| !inventory.tables.contains(*table))
        .count() as u64;
    if missing_tables > 0 {
        status = SessionTemporalHealthStatus::Partial;
        findings.push(finding(
            SessionTemporalHealthFindingKind::MigrationGap,
            missing_tables,
        ));
    } else {
        match schema_version(conn) {
            Ok(Some(version)) if version == SESSION_TEMPORAL_SCHEMA_VERSION => {}
            Ok(_) => findings.push(finding(SessionTemporalHealthFindingKind::MigrationGap, 1)),
            Err(error) => {
                if is_rusqlite_locked(&error) {
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

    match column_shape_drift(conn, &inventory) {
        Ok(0) => {}
        Ok(drift) => {
            status = SessionTemporalHealthStatus::Partial;
            merge_finding(
                &mut findings,
                SessionTemporalHealthFindingKind::MigrationGap,
                drift,
            );
        }
        Err(error) if is_rusqlite_locked(&error) => {
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
        match count(conn, check.sql) {
            Ok(0) => {}
            Ok(value) => merge_finding(&mut findings, check.kind, value),
            Err(error)
                if is_fts_finding(check.kind)
                    && is_rusqlite_fts_virtual_table_corruption(&error) =>
            {
                merge_finding(&mut findings, check.kind, 1);
            }
            Err(error) if is_rusqlite_locked(&error) => {
                return SessionTemporalHealthReport {
                    status: SessionTemporalHealthStatus::Locked,
                    findings,
                    reason: None,
                };
            }
            Err(_) => return unavailable_report(SessionTemporalHealthStatus::Unavailable),
        }
    }
    findings.sort_by_key(SessionTemporalHealthFinding::kind);
    SessionTemporalHealthReport {
        status,
        findings,
        reason: None,
    }
}

async fn diagnose_snapshot(conn: &impl QueryExecutor) -> SessionTemporalHealthReport {
    let inventory = match snapshot_schema_inventory(conn).await {
        Ok(inventory) => inventory,
        Err(error) => return unavailable_report(classify_engine_error(&error)),
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
                required_table_names().count() as u64,
            )],
            reason: None,
        };
    }

    let mut status = SessionTemporalHealthStatus::Complete;
    let mut findings = Vec::new();
    let missing_tables = required_table_names()
        .filter(|table| !inventory.tables.contains(*table))
        .count() as u64;
    if missing_tables > 0 {
        status = SessionTemporalHealthStatus::Partial;
        findings.push(finding(
            SessionTemporalHealthFindingKind::MigrationGap,
            missing_tables,
        ));
    } else {
        match snapshot_schema_version(conn).await {
            Ok(Some(version)) if version == SESSION_TEMPORAL_SCHEMA_VERSION => {}
            Ok(_) => findings.push(finding(SessionTemporalHealthFindingKind::MigrationGap, 1)),
            Err(error) => {
                if is_engine_locked(&error) {
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

    match snapshot_column_shape_drift(conn, &inventory).await {
        Ok(0) => {}
        Ok(drift) => {
            status = SessionTemporalHealthStatus::Partial;
            merge_finding(
                &mut findings,
                SessionTemporalHealthFindingKind::MigrationGap,
                drift,
            );
        }
        Err(error) if is_engine_locked(&error) => {
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
        match snapshot_count(conn, check.sql).await {
            Ok(0) => {}
            Ok(value) => merge_finding(&mut findings, check.kind, value),
            Err(error) if is_fts_finding(check.kind) && is_fts_virtual_table_corruption(&error) => {
                merge_finding(&mut findings, check.kind, 1);
            }
            Err(error) if is_engine_locked(&error) => {
                return SessionTemporalHealthReport {
                    status: SessionTemporalHealthStatus::Locked,
                    findings,
                    reason: None,
                };
            }
            Err(_) => return unavailable_report(SessionTemporalHealthStatus::Unavailable),
        }
    }
    findings.sort_by_key(SessionTemporalHealthFinding::kind);
    SessionTemporalHealthReport {
        status,
        findings,
        reason: None,
    }
}

struct SchemaInventory {
    tables: BTreeSet<String>,
    indexes: BTreeSet<String>,
    triggers: BTreeMap<String, String>,
}

fn schema_inventory(conn: &RusqliteConnection) -> Result<SchemaInventory, rusqlite::Error> {
    let mut statement = conn.prepare(
        "SELECT type, name, COALESCE(sql, '') FROM sqlite_master
         WHERE type IN ('table', 'index', 'trigger')",
    )?;
    let rows = statement.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
        ))
    })?;
    let mut tables = BTreeSet::new();
    let mut indexes = BTreeSet::new();
    let mut triggers = BTreeMap::new();
    for row in rows {
        let (kind, name, sql) = row?;
        match kind.as_str() {
            "table" => {
                tables.insert(name);
            }
            "index" => {
                indexes.insert(name);
            }
            "trigger" => {
                triggers.insert(name, sql);
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

async fn snapshot_schema_inventory(
    conn: &impl QueryExecutor,
) -> tracedecay_runtime_core::db::engine::Result<SchemaInventory> {
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
        let kind: String = row.get(0)?;
        let name: String = row.get(1)?;
        let sql: String = row.get(2)?;
        match kind.as_str() {
            "table" => {
                tables.insert(name);
            }
            "index" => {
                indexes.insert(name);
            }
            "trigger" => {
                triggers.insert(name, sql);
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

fn column_shape_drift(
    conn: &RusqliteConnection,
    inventory: &SchemaInventory,
) -> Result<u64, rusqlite::Error> {
    let mut drift = 0_u64;
    for &(table, expected) in TEMPORAL_TABLE_COLUMNS {
        if !inventory.tables.contains(table) {
            continue;
        }
        let mut statement = conn.prepare("SELECT name FROM pragma_table_info(?1) ORDER BY cid")?;
        let rows = statement.query_map([table], |row| row.get::<_, String>(0))?;
        let mut actual = Vec::new();
        for row in rows {
            actual.push(row?);
        }
        if actual.as_slice() != expected {
            drift = drift.saturating_add(1).min(MAX_FINDING_COUNT);
        }
    }
    Ok(drift)
}

async fn snapshot_column_shape_drift(
    conn: &impl QueryExecutor,
    inventory: &SchemaInventory,
) -> tracedecay_runtime_core::db::engine::Result<u64> {
    let mut drift = 0_u64;
    for &(table, expected) in TEMPORAL_TABLE_COLUMNS {
        if !inventory.tables.contains(table) {
            continue;
        }
        let mut rows = conn
            .query(
                "SELECT name FROM pragma_table_info(?1) ORDER BY cid",
                [table],
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

fn schema_version(conn: &RusqliteConnection) -> Result<Option<i64>, rusqlite::Error> {
    conn.query_row(
        "SELECT version FROM session_temporal_schema_migrations
         WHERE name = 'session-temporal'",
        [],
        |row| row.get(0),
    )
    .optional()
}

async fn snapshot_schema_version(
    conn: &impl QueryExecutor,
) -> tracedecay_runtime_core::db::engine::Result<Option<i64>> {
    let mut rows = conn
        .query(
            "SELECT version FROM session_temporal_schema_migrations
             WHERE name = 'session-temporal'",
            (),
        )
        .await?;
    rows.next().await?.map(|row| row.get(0)).transpose()
}

fn count(conn: &RusqliteConnection, sql: &str) -> Result<u64, rusqlite::Error> {
    let value: Option<i64> = conn.query_row(sql, [], |row| row.get(0)).optional()?;
    Ok(value
        .and_then(|value| u64::try_from(value).ok())
        .unwrap_or(0)
        .min(MAX_FINDING_COUNT))
}

async fn snapshot_count(
    conn: &impl QueryExecutor,
    sql: &str,
) -> tracedecay_runtime_core::db::engine::Result<u64> {
    let mut rows = conn.query(sql, ()).await?;
    let value = rows
        .next()
        .await?
        .map(|row| row.get::<Option<i64>>(0))
        .transpose()?
        .flatten();
    Ok(value
        .and_then(|value| u64::try_from(value).ok())
        .unwrap_or(0)
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

fn is_fts_virtual_table_corruption(error: &EngineError) -> bool {
    error.sqlite_extended_code() == Some(SQLITE_CORRUPT_VTAB)
        || error.sqlite_code() == Some(SQLITE_CORRUPT_VTAB)
}

fn is_rusqlite_fts_virtual_table_corruption(error: &rusqlite::Error) -> bool {
    error
        .sqlite_error()
        .is_some_and(|err| err.extended_code == SQLITE_CORRUPT_VTAB)
}

async fn require_quick_check(
    conn: &impl Executor,
    repair_occurrences: bool,
    repair_summaries: bool,
) -> tracedecay_runtime_core::db::engine::Result<()> {
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
            return Err(repair_refused(&format!(
                "whole-database quick check failed; FTS repair is unsafe: {message}"
            )));
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
        && (message == "malformed inverted index for FTS5 table main.session_occurrences_fts"
            || is_exact_fts_blob_corruption(message, "session_occurrences_fts")))
        || (repair_summaries
            && (message
                == "malformed inverted index for FTS5 table main.session_summary_nodes_fts"
                || is_exact_fts_blob_corruption(message, "session_summary_nodes_fts")))
}

fn is_exact_fts_blob_corruption(message: &str, expected_table: &str) -> bool {
    let Some(message) = message.strip_prefix("fts5: corruption found reading blob ") else {
        return false;
    };
    let Some((blob, table)) = message.split_once(" from table \"") else {
        return false;
    };
    blob.parse::<u64>().is_ok() && table.strip_suffix('"') == Some(expected_table)
}

async fn connection_count(
    conn: &impl Executor,
    table: &str,
) -> tracedecay_runtime_core::db::engine::Result<i64> {
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
    conn: &impl Executor,
    integrity_sql: &str,
    drift_sql: &str,
) -> tracedecay_runtime_core::db::engine::Result<()> {
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

fn repair_refused(message: &str) -> EngineError {
    EngineError::invalid_operation(message)
}

fn classify_rusqlite_error(
    error: &tracedecay_rusqlite_runtime::ConnectionPolicyError,
) -> SessionTemporalHealthStatus {
    let message = error.to_string().to_ascii_lowercase();
    if message.contains("locked") || message.contains("busy") {
        SessionTemporalHealthStatus::Locked
    } else {
        SessionTemporalHealthStatus::Unavailable
    }
}

fn classify_rusqlite_error_raw(error: &rusqlite::Error) -> SessionTemporalHealthStatus {
    if is_rusqlite_locked(error) {
        SessionTemporalHealthStatus::Locked
    } else {
        SessionTemporalHealthStatus::Unavailable
    }
}

fn classify_engine_error(error: &EngineError) -> SessionTemporalHealthStatus {
    if is_engine_locked(error) {
        SessionTemporalHealthStatus::Locked
    } else {
        SessionTemporalHealthStatus::Unavailable
    }
}

fn is_engine_locked(error: &EngineError) -> bool {
    let message = error.to_string().to_ascii_lowercase();
    message.contains("locked") || message.contains("busy")
}

fn is_rusqlite_locked(error: &rusqlite::Error) -> bool {
    let message = error.to_string().to_ascii_lowercase();
    message.contains("locked") || message.contains("busy")
}

fn unavailable_report(status: SessionTemporalHealthStatus) -> SessionTemporalHealthReport {
    unavailable_report_with_reason(status, None)
}

fn unavailable_report_with_reason(
    status: SessionTemporalHealthStatus,
    reason: Option<&'static str>,
) -> SessionTemporalHealthReport {
    SessionTemporalHealthReport {
        status,
        findings: Vec::new(),
        reason: reason.map(str::to_string),
    }
}

fn non_empty_wal_sidecar(db_path: &Path) -> bool {
    let mut wal = db_path.as_os_str().to_os_string();
    wal.push("-wal");
    std::fs::metadata(PathBuf::from(wal)).is_ok_and(|metadata| metadata.len() > 0)
}

#[cfg(test)]
mod cache_tests {
    use super::*;

    #[test]
    fn session_temporal_fingerprint_tracks_database_and_wal_changes() {
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let database = tmp.path().join("sessions.db");
        std::fs::write(&database, b"database").expect("database");
        let initial = session_temporal_store_fingerprint(&database).expect("initial fingerprint");

        let wal = tmp.path().join("sessions.db-wal");
        std::fs::write(&wal, b"wal").expect("wal");
        let with_wal = session_temporal_store_fingerprint(&database).expect("wal fingerprint");
        assert_ne!(initial, with_wal);

        std::fs::write(&wal, b"wal-expanded").expect("expanded wal");
        let expanded = session_temporal_store_fingerprint(&database).expect("expanded fingerprint");
        assert_ne!(with_wal, expanded);
    }

    #[test]
    fn session_temporal_size_budget_includes_wal_bytes() {
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let database = tmp.path().join("sessions.db");
        std::fs::write(&database, b"database").expect("database");
        assert!(permits_synchronous_session_temporal_health(&database));

        let wal = tmp.path().join("sessions.db-wal");
        std::fs::File::create(wal)
            .expect("wal")
            .set_len(MAX_SYNCHRONOUS_SESSION_TEMPORAL_HEALTH_BYTES)
            .expect("wal size");
        assert!(!permits_synchronous_session_temporal_health(&database));
    }
}
