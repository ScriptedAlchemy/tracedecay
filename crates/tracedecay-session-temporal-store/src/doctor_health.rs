//! Session-temporal Doctor health lane.
//!
//! Diagnosis is production-mounted and strictly read-only. Recovery belongs to
//! separately admitted storage operations, never to Doctor.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::handle::{SessionTemporalAccess, SessionTemporalRegisteredDb};
use tracedecay_domain::errors::TraceDecayError;
use tracedecay_runtime_core::db::engine::Error as EngineError;

use crate::schema_constants::{SESSION_TEMPORAL_SCHEMA_VERSION, TEMPORAL_TABLE_COLUMNS};

mod relation_health;

const MAX_FINDING_COUNT: u64 = 1_000_000;
const SQLITE_CORRUPT_VTAB: i32 = 267;
const SESSION_TEMPORAL_HEALTH_CACHE_TTL: Duration = Duration::from_secs(2);
const MAX_CACHED_SESSION_TEMPORAL_STORES: usize = 64;
const HEALTH_PROBE_PAGE_SIZE: i64 = 512;
const HEALTH_PROBE_QUERY_LIMIT: i64 = HEALTH_PROBE_PAGE_SIZE + 1;

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

// Both aliases resolve to the plain std/tokio mutex until the binary selects
// the profiler backend; naming them through `hotpath` keeps the type in step
// with what the unconditional `hotpath::mutex!` wrappers below return.
type SessionDoctorCacheLock<T> = hotpath::mutexes::Mutex<T>;
type SessionDoctorLaneLock<T> = hotpath::wrap::tokio::sync::Mutex<T>;

type SessionTemporalHealthCacheCell =
    Arc<SessionDoctorLaneLock<Option<CachedSessionTemporalHealth>>>;

static SESSION_TEMPORAL_HEALTH_CACHE: OnceLock<
    SessionDoctorCacheLock<HashMap<PathBuf, SessionTemporalHealthCacheCell>>,
> = OnceLock::new();

fn session_temporal_health_cache_cell(path: &Path) -> SessionTemporalHealthCacheCell {
    let cache = SESSION_TEMPORAL_HEALTH_CACHE.get_or_init(|| {
        hotpath::mutex!(
            Mutex::new(HashMap::new()),
            label = "session_temporal.doctor.cache"
        )
    });
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
    Arc::clone(cache.entry(path.to_path_buf()).or_insert_with(|| {
        Arc::new(hotpath::mutex!(
            tokio::sync::Mutex::new(None),
            label = "session_temporal.doctor.lane"
        ))
    }))
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
    hotpath::measure_block!("session_temporal.doctor.fingerprint", {
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
    })
}

const REQUIRED_BASE_TABLES: &[&str] =
    &["observations", "retrieval_anchors", "sanitization_receipts"];

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
    "idx_session_assertion_supersession_successor",
    "idx_session_assertions_generation_order",
    "idx_session_assertions_kind_order",
    "idx_session_assertions_object_order",
    "idx_session_assertions_subject",
    "idx_session_current_entities_assertion",
    "idx_session_current_entities_occurrence",
    "idx_session_external_payload_manifests_session",
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
    "idx_session_summary_nodes_depth_tokens",
    "idx_session_summary_nodes_root_created_order",
    "idx_session_summary_nodes_session_created",
    "idx_session_summary_nodes_session_depth_time",
    "idx_session_summary_sources_source",
    "idx_session_temporal_generations_one_active",
    "idx_session_temporal_generations_session_state",
    "idx_session_temporal_observation_effects_session",
    "idx_session_turn_members_occurrence",
];

const REQUIRED_TRIGGERS: &[(&str, &str)] = &[
    (
        "session_occurrences_fts_insert_v1",
        "CREATE TRIGGER session_occurrences_fts_insert_v1
         AFTER INSERT ON session_occurrences BEGIN
             INSERT INTO session_occurrences_fts(rowid, index_text)
             VALUES (NEW.rowid, NEW.index_text);
         END",
    ),
    (
        "session_occurrences_fts_delete_v1",
        "CREATE TRIGGER session_occurrences_fts_delete_v1
         AFTER DELETE ON session_occurrences BEGIN
             INSERT INTO session_occurrences_fts(session_occurrences_fts, rowid, index_text)
             VALUES ('delete', OLD.rowid, OLD.index_text);
         END",
    ),
    (
        "session_occurrences_fts_update_v1",
        "CREATE TRIGGER session_occurrences_fts_update_v1
         AFTER UPDATE OF index_text ON session_occurrences BEGIN
             INSERT INTO session_occurrences_fts(session_occurrences_fts, rowid, index_text)
             VALUES ('delete', OLD.rowid, OLD.index_text);
             INSERT INTO session_occurrences_fts(rowid, index_text)
             VALUES (NEW.rowid, NEW.index_text);
         END",
    ),
    (
        "session_summary_nodes_fts_insert_v1",
        "CREATE TRIGGER session_summary_nodes_fts_insert_v1
         AFTER INSERT ON session_summary_nodes BEGIN
             INSERT INTO session_summary_nodes_fts(rowid, summary_text)
             VALUES (NEW.rowid, NEW.summary_text);
         END",
    ),
    (
        "session_summary_nodes_fts_delete_v1",
        "CREATE TRIGGER session_summary_nodes_fts_delete_v1
         AFTER DELETE ON session_summary_nodes BEGIN
             INSERT INTO session_summary_nodes_fts(
                 session_summary_nodes_fts, rowid, summary_text
             )
             VALUES ('delete', OLD.rowid, OLD.summary_text);
         END",
    ),
    (
        "session_summary_nodes_fts_update_v1",
        "CREATE TRIGGER session_summary_nodes_fts_update_v1
         AFTER UPDATE OF summary_text ON session_summary_nodes BEGIN
             INSERT INTO session_summary_nodes_fts(
                 session_summary_nodes_fts, rowid, summary_text
             )
             VALUES ('delete', OLD.rowid, OLD.summary_text);
             INSERT INTO session_summary_nodes_fts(rowid, summary_text)
             VALUES (NEW.rowid, NEW.summary_text);
         END",
    ),
];

const INVALID_GENERATION_TAIL: &str = "WHERE candidate.generation <= 0
    OR json_valid(candidate.frozen_watermarks_json) = 0
    OR CASE WHEN json_valid(candidate.frozen_watermarks_json) = 1 THEN (
         json_type(candidate.frozen_watermarks_json, '$.active_generation') IS NOT 'integer'
         OR CAST(json_extract(
             candidate.frozen_watermarks_json, '$.active_generation'
         ) AS INTEGER) <= 0
         OR CAST(json_extract(
             candidate.frozen_watermarks_json, '$.active_generation'
         ) AS INTEGER) > candidate.generation
         OR json_type(candidate.frozen_watermarks_json, '$.source_frontier') IS NOT 'integer'
         OR CAST(json_extract(
             candidate.frozen_watermarks_json, '$.source_frontier'
         ) AS INTEGER) < 0
         OR json_type(candidate.frozen_watermarks_json, '$.projection_frontier') IS NOT 'integer'
         OR CAST(json_extract(
             candidate.frozen_watermarks_json, '$.projection_frontier'
         ) AS INTEGER) < 0
         OR json_type(candidate.frozen_watermarks_json, '$.summary_frontier') IS NOT 'integer'
         OR CAST(json_extract(
             candidate.frozen_watermarks_json, '$.summary_frontier'
         ) AS INTEGER) < 0
         OR NOT (
              (candidate.state = 'building' AND candidate.ready_at IS NULL
                   AND candidate.activated_at IS NULL AND candidate.completed_at IS NULL)
           OR (candidate.state = 'ready' AND candidate.ready_at IS NOT NULL
                   AND candidate.activated_at IS NULL AND candidate.completed_at IS NULL)
           OR (candidate.state = 'active' AND candidate.ready_at IS NOT NULL
                   AND candidate.activated_at IS NOT NULL AND candidate.completed_at IS NULL)
           OR (candidate.state = 'superseded' AND candidate.ready_at IS NOT NULL
                   AND candidate.activated_at IS NOT NULL
                   AND candidate.completed_at IS NOT NULL)
           OR (candidate.state IN ('failed', 'cancelled')
                   AND candidate.completed_at IS NOT NULL)
         )
    ) ELSE 0 END";

const MULTI_ACTIVE_GENERATION_TAIL: &str = "WHERE candidate.state = 'active'
    AND NOT EXISTS (
        SELECT 1
        FROM session_temporal_generations AS earlier
        WHERE earlier.session_id = candidate.session_id
          AND earlier.state = 'active'
          AND earlier.rowid < candidate.source_rowid
    )
    AND EXISTS (
        SELECT 1
        FROM session_temporal_generations AS later
        WHERE later.session_id = candidate.session_id
          AND later.state = 'active'
          AND later.rowid > candidate.source_rowid
    )";

const CURSOR_KEY_ABSENT_TAIL: &str = "LEFT JOIN session_query_cursor_keys AS key
    ON key.key_id = json_extract(candidate.frozen_watermarks_json, '$.cursor_key.key_id')
   AND key.key_version = CAST(json_extract(
          candidate.frozen_watermarks_json, '$.cursor_key.version'
       ) AS INTEGER)
   AND key.retired_at IS NULL
  WHERE candidate.state = 'active'
    AND (
        json_type(candidate.frozen_watermarks_json, '$.cursor_key') IS NOT 'object'
        OR key.key_id IS NULL
    )";

const STUCK_BINDING_TAIL: &str = "LEFT JOIN session_refresh_bindings AS binding
    ON binding.session_id = candidate.session_id
   AND binding.operation_id = candidate.operation_id
  LEFT JOIN session_temporal_generations AS generation
    ON generation.session_id = binding.session_id
   AND generation.generation = binding.generation
  WHERE candidate.state = 'running'
    AND (
        binding.operation_id IS NULL
        OR generation.session_id IS NULL
        OR generation.state <> 'building'
    )";

const STUCK_PROGRESS_SQL: &str = "WITH operation_source AS MATERIALIZED (
        SELECT rowid AS source_rowid, session_id, operation_id, state, updated_at
        FROM session_refresh_operations
        ORDER BY rowid
        LIMIT ?1
    ),
    operation_page AS MATERIALIZED (
        SELECT * FROM operation_source ORDER BY source_rowid LIMIT ?2
    ),
    operation_progress AS MATERIALIZED (
        SELECT operation.*,
               (
                   SELECT MAX(progress.recorded_at)
                   FROM session_refresh_progress AS progress
                   WHERE progress.session_id = operation.session_id
                     AND progress.operation_id = operation.operation_id
                     AND progress.progress_ordinal < ?2
               ) AS latest_progress
        FROM operation_page AS operation
    )
    SELECT
      (SELECT COUNT(*)
         FROM operation_progress AS operation
         JOIN session_refresh_bindings AS binding
           ON binding.session_id = operation.session_id
          AND binding.operation_id = operation.operation_id
         WHERE operation.state = 'running'
           AND NOT EXISTS(
               SELECT 1
               FROM session_refresh_progress AS progress
               WHERE progress.session_id = operation.session_id
                 AND progress.operation_id = operation.operation_id
                 AND progress.progress_ordinal >= ?2
           )
           AND (
               (operation.latest_progress IS NULL
                AND operation.updated_at
                    < CAST(strftime('%s', 'now') AS INTEGER) * 1000000 - 900000000)
               OR operation.latest_progress
                    < CAST(strftime('%s', 'now') AS INTEGER) * 1000000 - 900000000
           )),
      EXISTS(SELECT 1 FROM operation_source LIMIT 1 OFFSET ?2)
      OR EXISTS(
          SELECT 1
          FROM operation_page AS operation
          WHERE EXISTS(
              SELECT 1
              FROM session_refresh_progress AS progress
              WHERE progress.session_id = operation.session_id
                AND progress.operation_id = operation.operation_id
                AND progress.progress_ordinal >= ?2
          )
      )";

const STUCK_RECEIPT_TAIL: &str = "LEFT JOIN session_refresh_receipts AS receipt
    ON receipt.session_id = candidate.session_id
   AND receipt.operation_id = candidate.operation_id
  WHERE (candidate.state = 'running' AND receipt.operation_id IS NOT NULL)
     OR (candidate.state <> 'running' AND receipt.operation_id IS NULL)
     OR (receipt.operation_id IS NOT NULL
         AND (
             receipt.terminal_state <> candidate.state
             OR receipt.terminal_at IS NOT candidate.terminal_at
             OR receipt.failure_code IS NOT candidate.failure_code
         ))";

/// The promoted summary columns and the frozen publication manifest describe
/// the same publication; a row where they disagree was not written by the
/// publication path.
const COMPATIBILITY_DRIFT_TAIL: &str = "WHERE candidate.publication_json IS NULL
     OR json_extract(candidate.publication_json, '$.summary_hash') IS NOT candidate.summary_hash
     OR json_extract(candidate.publication_json, '$.provider') IS NOT candidate.provider
     OR json_extract(candidate.publication_json, '$.session_id') IS NOT candidate.session_id
     OR json_extract(candidate.publication_json, '$.depth') IS NOT candidate.depth";

macro_rules! row_health_check {
    (
        $kind:ident,
        $tables:expr,
        $source_table:literal,
        $source_columns:literal,
        $count:literal,
        $tail:expr
    ) => {
        HealthCheck {
            kind: SessionTemporalHealthFindingKind::$kind,
            tables: $tables,
            probe: HealthProbe::Rows {
                source_table: $source_table,
                source_columns: $source_columns,
                count: $count,
                tail: $tail,
            },
        }
    };
}

const CHECKS: &[HealthCheck] = &[
    row_health_check!(
        OccurrenceFtsCorruption,
        &["session_occurrences", "session_occurrences_fts_docsize"],
        "session_occurrences",
        "",
        "COUNT(*)",
        "LEFT JOIN session_occurrences_fts_docsize AS docsize
           ON docsize.id = candidate.source_rowid
         WHERE docsize.id IS NULL"
    ),
    row_health_check!(
        OccurrenceFtsCorruption,
        &["session_occurrences", "session_occurrences_fts_docsize"],
        "session_occurrences_fts_docsize",
        ", id",
        "COUNT(*)",
        "LEFT JOIN session_occurrences AS occurrence
           ON occurrence.rowid = candidate.id
         WHERE occurrence.rowid IS NULL"
    ),
    HealthCheck {
        kind: SessionTemporalHealthFindingKind::OccurrenceFtsCorruption,
        tables: &["session_occurrences_fts"],
        probe: HealthProbe::Sql(
            "SELECT COALESCE((
                 SELECT 0 FROM session_occurrences_fts
                 WHERE session_occurrences_fts MATCH 'tracedecay_health_probe_token'
                 LIMIT 1
             ), 0), (?1 - ?1) + (?2 - ?2)",
        ),
    },
    row_health_check!(
        SummaryFtsCorruption,
        &["session_summary_nodes", "session_summary_nodes_fts_docsize"],
        "session_summary_nodes",
        "",
        "COUNT(*)",
        "LEFT JOIN session_summary_nodes_fts_docsize AS docsize
           ON docsize.id = candidate.source_rowid
         WHERE docsize.id IS NULL"
    ),
    row_health_check!(
        SummaryFtsCorruption,
        &["session_summary_nodes", "session_summary_nodes_fts_docsize"],
        "session_summary_nodes_fts_docsize",
        ", id",
        "COUNT(*)",
        "LEFT JOIN session_summary_nodes AS summary
           ON summary.rowid = candidate.id
         WHERE summary.rowid IS NULL"
    ),
    HealthCheck {
        kind: SessionTemporalHealthFindingKind::SummaryFtsCorruption,
        tables: &["session_summary_nodes_fts"],
        probe: HealthProbe::Sql(
            "SELECT COALESCE((
                 SELECT 0 FROM session_summary_nodes_fts
                 WHERE session_summary_nodes_fts MATCH 'tracedecay_health_probe_token'
                 LIMIT 1
             ), 0), (?1 - ?1) + (?2 - ?2)",
        ),
    },
    row_health_check!(
        MissingAnchor,
        &["retrieval_anchors", "session_summary_nodes"],
        "session_summary_nodes",
        ", summary_anchor_id",
        "COUNT(*)",
        "LEFT JOIN retrieval_anchors AS anchor
           ON anchor.anchor_id = candidate.summary_anchor_id
         WHERE anchor.anchor_id IS NULL"
    ),
    row_health_check!(
        MissingAnchor,
        &["retrieval_anchors", "session_occurrences"],
        "session_occurrences",
        ", retrieval_anchor_id",
        "COUNT(*)",
        "LEFT JOIN retrieval_anchors AS anchor
           ON anchor.anchor_id = candidate.retrieval_anchor_id
         WHERE anchor.anchor_id IS NULL"
    ),
    row_health_check!(
        MissingAnchor,
        &["retrieval_anchors", "session_assertions"],
        "session_assertions",
        ", subject_anchor_id, object_anchor_id",
        "COUNT(*)",
        "LEFT JOIN retrieval_anchors AS subject
           ON subject.anchor_id = candidate.subject_anchor_id
         LEFT JOIN retrieval_anchors AS object
           ON object.anchor_id = candidate.object_anchor_id
         WHERE subject.anchor_id IS NULL OR object.anchor_id IS NULL"
    ),
    row_health_check!(
        MissingReceipt,
        &[
            "sanitization_receipts",
            "session_external_payload_manifests"
        ],
        "session_external_payload_manifests",
        ", receipt_id",
        "COUNT(*)",
        "LEFT JOIN sanitization_receipts AS receipt
           ON receipt.receipt_id = candidate.receipt_id
         WHERE receipt.receipt_id IS NULL"
    ),
    row_health_check!(
        MissingReceipt,
        &[
            "sanitization_receipts",
            "session_temporal_observation_effects"
        ],
        "session_temporal_observation_effects",
        ", receipt_id",
        "COUNT(*)",
        "LEFT JOIN sanitization_receipts AS receipt
           ON receipt.receipt_id = candidate.receipt_id
         WHERE receipt.receipt_id IS NULL"
    ),
    row_health_check!(
        MissingReceipt,
        &["sanitization_receipts", "session_summary_nodes"],
        "session_summary_nodes",
        ", publication_json",
        "COUNT(*)",
        "LEFT JOIN sanitization_receipts AS receipt
           ON receipt.receipt_id =
              json_extract(candidate.publication_json, '$.receipt_id')
         WHERE candidate.publication_json IS NULL OR receipt.receipt_id IS NULL"
    ),
    row_health_check!(
        MissingReceipt,
        &[
            "session_refresh_batch_bindings",
            "session_temporal_projection_receipts"
        ],
        "session_refresh_batch_bindings",
        ", session_id, generation, batch_ordinal",
        "COUNT(*)",
        "LEFT JOIN session_temporal_projection_receipts AS receipt
           ON receipt.session_id = candidate.session_id
          AND receipt.generation = candidate.generation
          AND receipt.batch_ordinal = candidate.batch_ordinal
         WHERE receipt.session_id IS NULL"
    ),
    row_health_check!(
        InvalidGeneration,
        &["session_temporal_generations"],
        "session_temporal_generations",
        ", generation, state, frozen_watermarks_json, ready_at, activated_at, completed_at",
        "COUNT(*)",
        INVALID_GENERATION_TAIL
    ),
    row_health_check!(
        MultiActiveGeneration,
        &["session_temporal_generations"],
        "session_temporal_generations",
        ", session_id, state",
        "COUNT(*)",
        MULTI_ACTIVE_GENERATION_TAIL
    ),
    row_health_check!(
        CursorChainAbsent,
        &["session_query_cursor_keys"],
        "session_query_cursor_keys",
        ", key_version",
        "COUNT(*)",
        "WHERE candidate.key_version > 1
           AND NOT EXISTS (
               SELECT 1
               FROM session_query_cursor_keys AS predecessor
               WHERE predecessor.key_version = candidate.key_version - 1
           )"
    ),
    row_health_check!(
        CursorChainAbsent,
        &["session_query_cursor_keys", "session_temporal_generations"],
        "session_temporal_generations",
        ", state",
        "CASE WHEN COUNT(*) > 0 THEN 1 ELSE 0 END",
        "WHERE candidate.state = 'active'
           AND (
               SELECT COUNT(*)
               FROM (
                   SELECT 1
                   FROM session_query_cursor_keys
                   WHERE retired_at IS NULL
                   LIMIT 2
               )
           ) <> 1"
    ),
    row_health_check!(
        CursorKeyAbsent,
        &["session_query_cursor_keys", "session_temporal_generations"],
        "session_temporal_generations",
        ", state, frozen_watermarks_json",
        "COUNT(*)",
        CURSOR_KEY_ABSENT_TAIL
    ),
    row_health_check!(
        OwnershipDrift,
        &["session_summary_availability", "session_summary_nodes"],
        "session_summary_availability",
        ", session_id, summary_id",
        "COUNT(*)",
        "LEFT JOIN session_summary_nodes AS summary
           ON summary.summary_id = candidate.summary_id
         WHERE summary.summary_id IS NULL
            OR candidate.session_id IS NOT summary.session_id"
    ),
    row_health_check!(
        OwnershipDrift,
        &["session_refresh_batch_bindings", "session_refresh_bindings"],
        "session_refresh_batch_bindings",
        ", session_id, operation_id, generation",
        "COUNT(*)",
        "LEFT JOIN session_refresh_bindings AS binding
           ON binding.session_id = candidate.session_id
          AND binding.operation_id = candidate.operation_id
         WHERE binding.operation_id IS NULL
            OR candidate.generation IS NOT binding.generation"
    ),
    row_health_check!(
        StuckRefresh,
        &["session_refresh_operations"],
        "session_refresh_operations",
        ", state, updated_at",
        "COUNT(*)",
        "WHERE candidate.state = 'running'
           AND candidate.updated_at
               < CAST(strftime('%s', 'now') AS INTEGER) * 1000000 - 900000000"
    ),
    row_health_check!(
        StuckBinding,
        &[
            "session_refresh_bindings",
            "session_refresh_operations",
            "session_temporal_generations"
        ],
        "session_refresh_operations",
        ", session_id, operation_id, state",
        "COUNT(*)",
        STUCK_BINDING_TAIL
    ),
    HealthCheck {
        kind: SessionTemporalHealthFindingKind::StuckProgress,
        tables: &[
            "session_refresh_bindings",
            "session_refresh_operations",
            "session_refresh_progress",
        ],
        probe: HealthProbe::Sql(STUCK_PROGRESS_SQL),
    },
    row_health_check!(
        StuckReceipt,
        &["session_refresh_operations", "session_refresh_receipts"],
        "session_refresh_operations",
        ", session_id, operation_id, state, terminal_at, failure_code",
        "COUNT(*)",
        STUCK_RECEIPT_TAIL
    ),
    row_health_check!(
        CompatibilityDrift,
        &["session_summary_nodes"],
        "session_summary_nodes",
        ", summary_id, session_id, provider, depth, summary_hash, publication_json",
        "COUNT(*)",
        COMPATIBILITY_DRIFT_TAIL
    ),
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
    RelationGraphUnavailable,
    RelationGraphCorruption,
    RelationGraphCycle,
    StaleSummaryClosure,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SessionTemporalHealthFinding {
    kind: SessionTemporalHealthFindingKind,
    count: u64,
}

impl SessionTemporalHealthFinding {
    #[hotpath::skip]
    pub const fn kind(&self) -> SessionTemporalHealthFindingKind {
        self.kind
    }

    #[hotpath::skip]
    pub const fn count(&self) -> u64 {
        self.count
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SessionTemporalHealthReport {
    status: SessionTemporalHealthStatus,
    findings: Vec<SessionTemporalHealthFinding>,
    /// Why diagnosis could not complete: a fixed bounded-probe reason or a
    /// `<probe>: <storage error>` detail naming the read that failed.
    /// Omitted when diagnosis ran to completion against an immutable snapshot.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    reason: Option<String>,
}

impl SessionTemporalHealthReport {
    #[hotpath::skip]
    pub const fn status(&self) -> SessionTemporalHealthStatus {
        self.status
    }

    pub fn findings(&self) -> &[SessionTemporalHealthFinding] {
        &self.findings
    }

    pub fn reason(&self) -> Option<&str> {
        self.reason.as_deref()
    }
}

struct HealthCheck {
    kind: SessionTemporalHealthFindingKind,
    tables: &'static [&'static str],
    probe: HealthProbe,
}

enum HealthProbe {
    Rows {
        source_table: &'static str,
        source_columns: &'static str,
        count: &'static str,
        tail: &'static str,
    },
    Sql(&'static str),
}

impl<D: SessionTemporalRegisteredDb + Sync> SessionTemporalAccess<'_, D> {
    /// Produces a redacted, non-mutating temporal health snapshot through the
    /// retained registered reader pool. Identical requests coalesce behind one
    /// per-store lane and reuse a very short-lived result only while the exact
    /// database/WAL fingerprint remains unchanged.
    #[hotpath::measure(future = true, label = "session_temporal.doctor.query")]
    pub async fn session_temporal_doctor_health(&self) -> SessionTemporalHealthReport {
        let database_path = self.db_path();
        let cache = session_temporal_health_cache_cell(database_path);
        let mut cached = cache.lock().await;
        let before = session_temporal_store_fingerprint(database_path).ok();
        if let (Some(fingerprint), Some(observed)) = (before, cached.as_ref())
            && observed.fingerprint == fingerprint
            && observed.observed_at.elapsed() <= SESSION_TEMPORAL_HEALTH_CACHE_TTL
        {
            record_session_doctor_cache_hit();
            return self
                .with_relation_graph_health(observed.report.clone())
                .await;
        }
        record_session_doctor_cache_miss();
        let snapshot = match self.health_read_snapshot().await {
            Ok(snapshot) => snapshot,
            Err(error) => {
                return unavailable_report_with_detail(
                    classify_database_error(&error),
                    "read_snapshot",
                    &error,
                );
            }
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
        self.with_relation_graph_health(report).await
    }
}

#[hotpath::measure(future = true, label = "session_temporal.doctor.diagnose")]
async fn diagnose_snapshot(
    conn: &impl crate::handle::SessionTemporalQuery,
) -> SessionTemporalHealthReport {
    let inventory = match snapshot_schema_inventory(conn).await {
        Ok(inventory) => inventory,
        Err(error) => {
            return unavailable_report_with_detail(
                classify_engine_error(&error),
                "schema_inventory",
                &error,
            );
        }
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
    let mut partial_reasons = BTreeSet::new();
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
                    return unavailable_report_with_detail(
                        SessionTemporalHealthStatus::Locked,
                        "schema_version",
                        &error,
                    );
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
        Err(error) => {
            return unavailable_report_with_detail(
                classify_engine_error(&error),
                "column_shape",
                &error,
            );
        }
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
        record_session_doctor_check();
        if diagnose_health_check(
            conn,
            check,
            &mut status,
            &mut findings,
            &mut partial_reasons,
        )
        .await
        {
            return SessionTemporalHealthReport {
                status: SessionTemporalHealthStatus::Locked,
                findings,
                reason: None,
            };
        }
    }
    findings.sort_by_key(SessionTemporalHealthFinding::kind);
    SessionTemporalHealthReport {
        status,
        findings,
        reason: (!partial_reasons.is_empty())
            .then(|| partial_reasons.into_iter().collect::<Vec<_>>().join("; ")),
    }
}

async fn diagnose_health_check(
    conn: &impl crate::handle::SessionTemporalQuery,
    check: &HealthCheck,
    status: &mut SessionTemporalHealthStatus,
    findings: &mut Vec<SessionTemporalHealthFinding>,
    partial_reasons: &mut BTreeSet<String>,
) -> bool {
    let probe_name = check_probe_name(check.kind);
    match snapshot_count(conn, check).await {
        Ok(outcome) => {
            if outcome.count > 0 || !outcome.complete {
                merge_finding(findings, check.kind, outcome.count);
            }
            if !outcome.complete {
                *status = SessionTemporalHealthStatus::Partial;
                partial_reasons.insert(format!("{probe_name}: bounded_row_probe_incomplete"));
            }
            false
        }
        Err(error) if is_fts_finding(check.kind) && is_fts_virtual_table_corruption(&error) => {
            merge_finding(findings, check.kind, 1);
            false
        }
        Err(error) if is_engine_locked(&error) => true,
        Err(error) => {
            *status = SessionTemporalHealthStatus::Partial;
            merge_finding(findings, check.kind, 0);
            partial_reasons.insert(format!("{probe_name}: {error}"));
            false
        }
    }
}

struct SchemaInventory {
    tables: BTreeSet<String>,
    indexes: BTreeSet<String>,
    triggers: BTreeMap<String, String>,
}

#[hotpath::measure(future = true, label = "session_temporal.doctor.query.inventory")]
async fn snapshot_schema_inventory(
    conn: &impl crate::handle::SessionTemporalQuery,
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

#[hotpath::measure(future = true, label = "session_temporal.doctor.query.column_shape")]
async fn snapshot_column_shape_drift(
    conn: &impl crate::handle::SessionTemporalQuery,
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

#[hotpath::measure(future = true, label = "session_temporal.doctor.query.schema_version")]
async fn snapshot_schema_version(
    conn: &impl crate::handle::SessionTemporalQuery,
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

#[hotpath::measure(future = true, label = "session_temporal.doctor.query.count")]
async fn snapshot_count(
    conn: &impl crate::handle::SessionTemporalQuery,
    check: &HealthCheck,
) -> tracedecay_runtime_core::db::engine::Result<HealthProbeOutcome> {
    let sql = match &check.probe {
        HealthProbe::Rows {
            source_table,
            source_columns,
            count,
            tail,
        } => format!(
            "WITH source AS MATERIALIZED (
                 SELECT rowid AS source_rowid{source_columns}
                 FROM {source_table}
                 ORDER BY rowid
                 LIMIT ?1
             ),
             page AS MATERIALIZED (
                 SELECT * FROM source ORDER BY source_rowid LIMIT ?2
             )
             SELECT {count},
                    EXISTS(SELECT 1 FROM source LIMIT 1 OFFSET ?2)
             FROM page AS candidate
             {tail}"
        ),
        HealthProbe::Sql(sql) => (*sql).to_owned(),
    };
    let mut rows = conn
        .query(&sql, [HEALTH_PROBE_QUERY_LIMIT, HEALTH_PROBE_PAGE_SIZE])
        .await?;
    let Some(row) = rows.next().await? else {
        return Ok(HealthProbeOutcome {
            count: 0,
            complete: true,
        });
    };
    let value = row.get::<Option<i64>>(0)?;
    let incomplete = row.get::<i64>(1)? != 0;
    Ok(HealthProbeOutcome {
        count: value
            .and_then(|value| u64::try_from(value).ok())
            .unwrap_or(0)
            .min(MAX_FINDING_COUNT),
        complete: !incomplete,
    })
}

struct HealthProbeOutcome {
    count: u64,
    complete: bool,
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

fn classify_engine_error(error: &EngineError) -> SessionTemporalHealthStatus {
    if is_engine_locked(error) {
        SessionTemporalHealthStatus::Locked
    } else {
        SessionTemporalHealthStatus::Unavailable
    }
}

fn classify_database_error(error: &TraceDecayError) -> SessionTemporalHealthStatus {
    let message = error.to_string().to_ascii_lowercase();
    if message.contains("locked") || message.contains("busy") {
        SessionTemporalHealthStatus::Locked
    } else {
        SessionTemporalHealthStatus::Unavailable
    }
}

fn is_engine_locked(error: &EngineError) -> bool {
    let message = error.to_string().to_ascii_lowercase();
    message.contains("locked") || message.contains("busy")
}

/// The probe a failed check names in its `reason`, so an unavailable report
/// says which read failed rather than a bare unavailable.
fn check_probe_name(kind: SessionTemporalHealthFindingKind) -> &'static str {
    match kind {
        SessionTemporalHealthFindingKind::TriggerAuditDrift => "trigger_audit",
        SessionTemporalHealthFindingKind::OccurrenceFtsCorruption => "occurrence_fts",
        SessionTemporalHealthFindingKind::SummaryFtsCorruption => "summary_fts",
        SessionTemporalHealthFindingKind::MissingAnchor => "missing_anchor",
        SessionTemporalHealthFindingKind::MissingReceipt => "missing_receipt",
        SessionTemporalHealthFindingKind::InvalidGeneration => "invalid_generation",
        SessionTemporalHealthFindingKind::MultiActiveGeneration => "multi_active_generation",
        SessionTemporalHealthFindingKind::CursorChainAbsent => "cursor_chain",
        SessionTemporalHealthFindingKind::CursorKeyAbsent => "cursor_key",
        SessionTemporalHealthFindingKind::OwnershipDrift => "ownership",
        SessionTemporalHealthFindingKind::StuckRefresh => "stuck_refresh",
        SessionTemporalHealthFindingKind::StuckBinding => "stuck_binding",
        SessionTemporalHealthFindingKind::StuckProgress => "stuck_progress",
        SessionTemporalHealthFindingKind::StuckReceipt => "stuck_receipt",
        SessionTemporalHealthFindingKind::MigrationGap => "migration_gap",
        SessionTemporalHealthFindingKind::CompatibilityDrift => "compatibility_drift",
        SessionTemporalHealthFindingKind::RelationGraphUnavailable => "relation_graph",
        SessionTemporalHealthFindingKind::RelationGraphCorruption => "relation_graph_corruption",
        SessionTemporalHealthFindingKind::RelationGraphCycle => "relation_graph_cycle",
        SessionTemporalHealthFindingKind::StaleSummaryClosure => "stale_summary_closure",
    }
}

/// An unavailable or locked report that names the probe that failed and the
/// storage error it failed with; the operator otherwise cannot tell a
/// refused schema from a busy writer or a missing table.
fn unavailable_report_with_detail(
    status: SessionTemporalHealthStatus,
    probe: &'static str,
    error: &impl std::fmt::Display,
) -> SessionTemporalHealthReport {
    SessionTemporalHealthReport {
        status,
        findings: Vec::new(),
        reason: Some(format!("{probe}: {error}")),
    }
}

#[inline(always)]
fn record_session_doctor_cache_hit() {
    #[cfg(feature = "hotpath")]
    hotpath::gauge!("session_temporal.doctor.cache_hits").inc(1_u64);
}

#[inline(always)]
fn record_session_doctor_cache_miss() {
    #[cfg(feature = "hotpath")]
    hotpath::gauge!("session_temporal.doctor.cache_misses").inc(1_u64);
}

#[inline(always)]
fn record_session_doctor_check() {
    #[cfg(feature = "hotpath")]
    hotpath::gauge!("session_temporal.doctor.checks").inc(1_u64);
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
}

#[cfg(test)]
mod probe_tests {
    use super::*;
    use crate::handle::SessionTemporalExec;
    use tracedecay_runtime_core::db::engine::TestConnection;

    #[tokio::test]
    async fn row_probe_reports_observed_findings_when_its_page_is_incomplete() {
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let connection = TestConnection::open(&tmp.path().join("doctor-page.db"));
        SessionTemporalExec::execute_batch(
            &connection,
            &format!(
                "CREATE TABLE retrieval_anchors (anchor_id TEXT PRIMARY KEY);
                 CREATE TABLE session_summary_nodes (summary_anchor_id TEXT NOT NULL);
                 CREATE TABLE session_occurrences (retrieval_anchor_id TEXT NOT NULL);
                 CREATE TABLE session_assertions (
                     subject_anchor_id TEXT NOT NULL,
                     object_anchor_id TEXT NOT NULL
                 );
                 WITH RECURSIVE sequence(value) AS (
                     VALUES(0)
                     UNION ALL
                     SELECT value + 1 FROM sequence WHERE value < {}
                 )
                 INSERT INTO session_summary_nodes (summary_anchor_id)
                 SELECT printf('missing-%d', value) FROM sequence;",
                HEALTH_PROBE_PAGE_SIZE
            ),
        )
        .await
        .expect("seed oversized health probe");
        let check = CHECKS
            .iter()
            .find(|check| check.kind == SessionTemporalHealthFindingKind::MissingAnchor)
            .expect("missing-anchor check");

        let mut status = SessionTemporalHealthStatus::Complete;
        let mut findings = Vec::new();
        let mut partial_reasons = BTreeSet::new();

        assert!(
            !diagnose_health_check(
                &connection,
                check,
                &mut status,
                &mut findings,
                &mut partial_reasons,
            )
            .await
        );
        assert_eq!(status, SessionTemporalHealthStatus::Partial);
        assert_eq!(
            findings,
            vec![finding(
                SessionTemporalHealthFindingKind::MissingAnchor,
                HEALTH_PROBE_PAGE_SIZE as u64,
            )]
        );
        assert_eq!(
            partial_reasons,
            BTreeSet::from(["missing_anchor: bounded_row_probe_incomplete".to_owned()])
        );
    }
}

#[cfg(test)]
mod registered_tests {
    use super::*;
    use crate::handle::{SessionTemporalAccess, SessionTemporalExec, SessionTemporalRegisteredDb};
    use tracedecay_global_db::tests::harness::RegisteredGlobalDbHarness;

    #[tokio::test]
    async fn oversized_session_temporal_store_still_reports_schema_findings() {
        const OLD_SYNCHRONOUS_HEALTH_BUDGET_BYTES: u64 = 64 * 1024 * 1024;
        let harness =
            RegisteredGlobalDbHarness::open_without_relation_graph("doctor-oversized-store").await;
        let writer = harness.registered.writer_connection().expect("writer");
        SessionTemporalExec::execute(
            &writer,
            "DROP INDEX idx_session_occurrences_generation_order",
            (),
        )
        .await
        .expect("drop required index");

        let database = SessionTemporalRegisteredDb::db_path(&harness.registered);
        std::fs::OpenOptions::new()
            .write(true)
            .open(database)
            .expect("open session database")
            .set_len(OLD_SYNCHRONOUS_HEALTH_BUDGET_BYTES + 4096)
            .expect("grow session database past the former admission budget");

        let report = SessionTemporalAccess::new(&harness.registered)
            .session_temporal_doctor_health()
            .await;

        assert_eq!(report.status(), SessionTemporalHealthStatus::Partial);
        assert_ne!(
            report.reason(),
            Some("synchronous_diagnosis_size_budget_exceeded")
        );
        assert!(report.findings().iter().any(|finding| {
            finding.kind() == SessionTemporalHealthFindingKind::MigrationGap && finding.count() >= 1
        }));
    }

    #[tokio::test]
    async fn registered_doctor_reports_an_unbound_relation_graph_as_partial() {
        let harness =
            RegisteredGlobalDbHarness::open_without_relation_graph("doctor-unbound-relation-graph")
                .await;

        let report = SessionTemporalAccess::new(&harness.registered)
            .session_temporal_doctor_health()
            .await;

        assert_eq!(report.status(), SessionTemporalHealthStatus::Partial);
        assert!(report.findings().contains(&SessionTemporalHealthFinding {
            kind: SessionTemporalHealthFindingKind::RelationGraphUnavailable,
            count: 1,
        }));
    }
}
