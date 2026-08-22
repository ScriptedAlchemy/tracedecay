//! Generation-scoped retention for the append-only observation evidence stores
//! (plan 38 §3, final clause).
//!
//! The observation store keeps three append-only, forever-growing evidence
//! tables that dominated one observed `sessions.db`:
//!
//! * `observations` — the durable observation payload (`observation_json`,
//!   1.8 GB measured).
//! * `retrieval_anchors` — the immutable retrieval-anchor payload
//!   (`anchor_json`, 1.6 GB measured).
//! * `observation_repository_provenance` — the repository-provenance payload
//!   (`availability_json` + `capture_json`, 1.4 GB measured).
//!
//! Plan 38 §3 makes these gain "generation-scoped retention tied to anchor
//! dispositions — superseded and deleted dispositions release their storage."
//! This module is the retention pass that does exactly that, mirroring the
//! sibling LCM slice ([`tracedecay_sessions::runtime::lcm::retention`]): a bounded,
//! DryRun/Apply, before/after-measured engine.
//!
//! # The disposition ledger is the governing authority
//!
//! Every anchor's lifecycle is recorded in the append-only
//! `retrieval_anchor_dispositions` ledger, whose *current* state for an anchor
//! is the highest-`sequence` row for its `(anchor_id, owner_json)`. The four
//! states carry different retention meaning:
//!
//! * `active` — live, referenced evidence. **Never** released.
//! * `unavailable` — the source is gone, but the evidence record is retained
//!   as the durable account of what was seen. **Never** released.
//! * `superseded` — a newer generation's anchor replaced this one.
//! * `deleted` — the evidence was retired (user request, retention, redaction,
//!   …).
//!
//! Only `superseded` and `deleted` current states release storage. This is the
//! plan's non-goal ("no lossy deletion of live, referenced evidence") expressed
//! directly in SQL: the `active`/`unavailable` predicate branch is simply never
//! selected.
//!
//! # Ledger-vs-payload design decision
//!
//! The ledger, its reverse-lineage, its derivative tombstones, and the anchor
//! *aliases* are all compact and are the audit trail of what happened to each
//! anchor. They are **never** mutated — their `BEFORE UPDATE/DELETE
//! RAISE(ABORT)` immutability triggers stay in force and this module respects
//! them. Additionally, the ledger's `FOREIGN KEY(anchor_id, owner_json)
//! REFERENCES retrieval_anchors(...)` means the anchor *skeleton row* (its
//! identity columns) must survive for the ledger to remain valid.
//!
//! Storage is therefore reclaimed by **releasing the fat payload columns in
//! place** rather than deleting rows: the bulky `anchor_json`,
//! `observation_json`, `availability_json`, and `capture_json` are overwritten
//! with a compact `{"__retention_released": …}` tombstone marker. The skeleton
//! rows, every foreign key, and the entire disposition ledger stay intact and
//! fully queryable; only the released-evidence payload leaves the database.
//! This is what "retaining the compact ledger and deleting the fat payload rows
//! it governs" means when referential integrity forbids deleting the rows
//! themselves.
//!
//! `retrieval_anchors`, `observations`, and
//! `observation_repository_provenance` carry `BEFORE UPDATE` immutability
//! triggers. Each releasing transaction drops only its relevant update trigger,
//! rewrites the payload column, and recreates the identical canonical trigger
//! — all inside one `Immediate` transaction, so immutability is never
//! observably relaxed and a crash mid-batch rolls back to the fully-triggered
//! schema.
//!
//! # Three passes, generation-scoped, and bounded
//!
//! Each pass has its own window (`None` = disabled) and is scoped to an
//! optional `projection_generation`. Every pass is capped by `max_batch_size`
//! and re-run-idempotent (already-released rows carry the marker and are
//! skipped), so the daemon can schedule it incrementally off the hot path. A
//! dry run counts eligible rows and the bytes that *would* be reclaimed without
//! mutating anything.
//!
//! The daemon reaches this engine through
//! [`crate::RegisteredGlobalDb::run_observation_retention`].

use serde::{Deserialize, Serialize};
use tracedecay_domain::ObservationSourceCursorV1;
use tracedecay_store::observation::ObservationCoverageV1;

use tracedecay_runtime_core::db::{
    Database, DatabaseEngineReadConnection, DatabaseWriteTransaction,
    engine::{Executor, Params, QueryExecutor, Value, params},
};
use tracedecay_runtime_core::errors::{Result, TraceDecayError};

const SECONDS_PER_DAY: i64 = 24 * 60 * 60;

const OPERATION: &str = "observation evidence retention";

/// Row count per batched retention `UPDATE`/`DELETE ... WHERE id IN (...)`
/// statement. Keeps bound-parameter counts safely under SQLite's default
/// `SQLITE_LIMIT_VARIABLE_NUMBER` (999) regardless of the configured
/// `max_batch_size`, mirroring `project_registry::delete_code_projects`'s
/// chunking pattern.
const RETENTION_DML_CHUNK: usize = 500;
const CURSOR_ADVANCE_SCAN_PAGE_ROWS: i64 = 500;

/// Compact tombstone written over a released `retrieval_anchors.anchor_json`.
const ANCHOR_RELEASED_MARKER: &str = "{\"__retention_released\":\"anchor\"}";
/// Compact tombstone written over a released `observations.observation_json`.
const OBSERVATION_RELEASED_MARKER: &str = "{\"__retention_released\":\"observation\"}";
/// Compact tombstone written over released provenance JSON columns.
const PROVENANCE_RELEASED_MARKER: &str = "{\"__retention_released\":\"provenance\"}";

/// SQL fragment (over an anchor aliased `a`, cutoff bound as `?2`) that is true
/// when the anchor's *current* disposition (highest `sequence`) is `superseded`
/// or `deleted` and took effect before the cutoff. `active` and `unavailable`
/// current states never satisfy it, so live and source-unavailable evidence is
/// never released — the plan's non-goal encoded in SQL.
const RELEASED_DISPOSITION: &str = "EXISTS (
        SELECT 1 FROM retrieval_anchor_dispositions d
        WHERE d.anchor_id = a.anchor_id AND d.owner_json = a.owner_json
          AND d.sequence = (
              SELECT MAX(d2.sequence) FROM retrieval_anchor_dispositions d2
              WHERE d2.anchor_id = a.anchor_id AND d2.owner_json = a.owner_json
          )
          AND d.state IN ('superseded', 'deleted')
          AND d.effective_at < ?2
    )";

const DROP_ANCHOR_UPDATE_TRIGGER: &str =
    "DROP TRIGGER IF EXISTS retrieval_anchors_immutable_update";
const CREATE_ANCHOR_UPDATE_TRIGGER: &str = "CREATE TRIGGER IF NOT EXISTS \
     retrieval_anchors_immutable_update BEFORE UPDATE ON retrieval_anchors BEGIN \
     SELECT RAISE(ABORT, 'retrieval anchors are immutable'); END";

const DROP_OBSERVATION_UPDATE_TRIGGER: &str =
    "DROP TRIGGER IF EXISTS observations_immutable_update";
const CREATE_OBSERVATION_UPDATE_TRIGGER: &str = "CREATE TRIGGER \
     observations_immutable_update BEFORE UPDATE ON observations BEGIN \
     SELECT RAISE(ABORT, 'observations are immutable'); END";

const DROP_PROVENANCE_UPDATE_TRIGGER: &str =
    "DROP TRIGGER IF EXISTS observation_repository_provenance_immutable_update";
const CREATE_PROVENANCE_UPDATE_TRIGGER: &str = "CREATE TRIGGER IF NOT EXISTS \
     observation_repository_provenance_immutable_update BEFORE UPDATE ON \
     observation_repository_provenance BEGIN SELECT RAISE(ABORT, \
     'observation repository provenance is immutable'); END";

const DROP_CURSOR_ADVANCE_DELETE_TRIGGER: &str =
    "DROP TRIGGER IF EXISTS source_cursor_advances_immutable_delete_v1";
const CREATE_CURSOR_ADVANCE_DELETE_TRIGGER: &str = "CREATE TRIGGER \
     source_cursor_advances_immutable_delete_v1 BEFORE DELETE ON \
     source_cursor_advances BEGIN SELECT RAISE(ABORT, \
     'source cursor advances are immutable'); END";

mod restore;
pub use restore::replay_current_release_state_for_restore;

fn db_error(source: impl std::error::Error + Send + Sync + 'static) -> TraceDecayError {
    TraceDecayError::database_operation(OPERATION, source)
}

fn require_apply_transaction<T>(transaction: Option<T>, message: &'static str) -> Result<T> {
    transaction.ok_or_else(|| TraceDecayError::Database {
        operation: OPERATION.to_string(),
        message: message.to_string(),
    })
}

fn opt_text(value: Option<&str>) -> Value {
    value.map_or(Value::Null, |text| Value::Text(text.to_string()))
}

/// Per-table retention windows for the observation evidence stores. Released
/// dispositions are no longer live evidence; their bulky payloads default to a
/// conservative 30-day recovery horizon while the immutable identity and
/// disposition ledgers remain durable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ObservationRetentionConfig {
    /// Master switch. When `false`, [`run_observation_retention`] is a no-op
    /// even in [`RetentionMode::Apply`].
    #[serde(default = "default_retention_enabled")]
    pub enabled: bool,
    /// Window (days since the governing disposition took effect) after which a
    /// superseded/deleted anchor's `anchor_json` payload is released. `None`
    /// disables the anchor pass.
    #[serde(default = "default_evidence_release_after_days")]
    pub anchor_release_after_days: Option<u32>,
    /// Window after which an observation whose bound anchor is superseded/
    /// deleted has its `observation_json` payload released. `None` disables the
    /// observation pass.
    #[serde(default = "default_evidence_release_after_days")]
    pub observation_release_after_days: Option<u32>,
    /// Window after which a provenance row whose anchor is superseded/deleted
    /// has its `availability_json`/`capture_json` payload released. `None`
    /// disables the provenance pass.
    #[serde(default = "default_evidence_release_after_days")]
    pub provenance_release_after_days: Option<u32>,
    /// Reclaim cursor-advance receipts that are strictly superseded by the
    /// current source frontier. The exact receipt supporting the current
    /// frontier is always retained.
    #[serde(default = "default_reclaim_superseded_cursor_advances")]
    pub reclaim_superseded_cursor_advances: bool,
    /// Upper bound on rows touched per pass, keeping each run incremental and
    /// off the hot path.
    #[serde(default = "default_max_batch_size")]
    pub max_batch_size: usize,
}

fn default_max_batch_size() -> usize {
    500
}

fn default_retention_enabled() -> bool {
    true
}

fn default_reclaim_superseded_cursor_advances() -> bool {
    true
}

#[allow(clippy::unnecessary_wraps)]
fn default_evidence_release_after_days() -> Option<u32> {
    Some(30)
}

impl Default for ObservationRetentionConfig {
    fn default() -> Self {
        Self {
            enabled: default_retention_enabled(),
            anchor_release_after_days: default_evidence_release_after_days(),
            observation_release_after_days: default_evidence_release_after_days(),
            provenance_release_after_days: default_evidence_release_after_days(),
            reclaim_superseded_cursor_advances: default_reclaim_superseded_cursor_advances(),
            max_batch_size: default_max_batch_size(),
        }
    }
}

impl ObservationRetentionConfig {
    fn batch_limit(&self) -> i64 {
        i64::try_from(self.max_batch_size.max(1)).unwrap_or(i64::MAX)
    }

    /// Whether any pass has a window configured. When false, an enabled run
    /// still reports zero work rather than scanning.
    fn any_window(&self) -> bool {
        self.anchor_release_after_days.is_some()
            || self.observation_release_after_days.is_some()
            || self.provenance_release_after_days.is_some()
            || self.reclaim_superseded_cursor_advances
    }
}

/// Whether a retention pass mutates the database.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RetentionMode {
    /// Count what would be released without mutating anything.
    DryRun,
    /// Apply the retention passes.
    Apply,
}

impl RetentionMode {
    fn is_apply(self) -> bool {
        matches!(self, Self::Apply)
    }
}

/// Outcome of a single retention pass.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ObservationRetentionPhaseReport {
    /// Configured window in days (`None` when the pass is disabled).
    pub window_days: Option<u32>,
    /// Rows matching the pass predicate within the batch cap (candidates).
    pub eligible: u64,
    /// Rows actually released (`0` in a dry run).
    pub acted: u64,
    /// Bytes of payload reclaimed from the database by this pass.
    pub bytes_reclaimed: u64,
    /// Oldest governing disposition timestamp among the bounded eligible rows.
    #[serde(default)]
    pub oldest_eligible_at: Option<i64>,
}

/// Accumulated admission-work telemetry across every refusal marker: how much
/// stored-row decode, identity-derivation, payload-digest, and runtime-command
/// work refusal-answering admission passes have performed in total. Each pass
/// lands its typed `AdmissionWorkV1` receipt on its marker row; this rollup is
/// the operator-facing sum, reported on the retention report the daemon
/// maintenance tick already reads, so collision re-admission churn is visible
/// in-product instead of only through `perf(1)`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ObservationAdmissionWorkRollupV1 {
    /// Retained terminal refusal markers carrying work telemetry.
    pub refusal_markers: u64,
    pub stored_rows_decoded: u64,
    pub identity_derivations: u64,
    pub payload_digests: u64,
    pub runtime_commands: u64,
}

/// Aggregate report for a retention run, including measurable reclaim (row and
/// page/freelist counts before and after).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ObservationRetentionReport {
    /// Projection-generation scope (`None` spans every generation).
    pub generation: Option<String>,
    pub applied: bool,
    pub started_at: i64,
    pub ended_at: i64,
    pub anchors_released: ObservationRetentionPhaseReport,
    pub observations_released: ObservationRetentionPhaseReport,
    pub provenance_released: ObservationRetentionPhaseReport,
    /// Superseded append-only cursor-advance receipts reclaimed by the
    /// daemon-authorized maintenance transaction.
    pub cursor_advances_reclaimed: ObservationRetentionPhaseReport,
    /// Count of `retrieval_anchors` whose `anchor_json` still carries a payload
    /// (not yet released), before/after the run.
    pub anchor_payloads_before: u64,
    pub anchor_payloads_after: u64,
    /// Count of `observations` whose `observation_json` still carries a payload,
    /// before/after the run.
    pub observation_payloads_before: u64,
    pub observation_payloads_after: u64,
    pub cursor_advances_before: u64,
    pub cursor_advances_after: u64,
    /// Database `PRAGMA freelist_count` before/after (freed pages are the
    /// measurable, VACUUM-free signal that space was reclaimed).
    pub freelist_before: u64,
    pub freelist_after: u64,
    /// Database `PRAGMA page_count` before/after.
    pub page_count_before: u64,
    pub page_count_after: u64,
    /// Admission-work telemetry accumulated on the retained refusal markers.
    pub admission_work: ObservationAdmissionWorkRollupV1,
    pub errors: Vec<String>,
}

impl ObservationRetentionReport {
    /// Total payload bytes reclaimed across every pass.
    pub fn bytes_reclaimed(&self) -> u64 {
        self.anchors_released
            .bytes_reclaimed
            .saturating_add(self.observations_released.bytes_reclaimed)
            .saturating_add(self.provenance_released.bytes_reclaimed)
            .saturating_add(self.cursor_advances_reclaimed.bytes_reclaimed)
    }
}

fn cutoff_secs(window_days: u32, now_secs: i64) -> i64 {
    now_secs.saturating_sub(i64::from(window_days).saturating_mul(SECONDS_PER_DAY))
}

async fn pragma_u64(conn: &(impl QueryExecutor + ?Sized), pragma: &str) -> u64 {
    let sql = format!("PRAGMA {pragma}");
    let Ok(mut rows) = conn.query(&sql, ()).await else {
        return 0;
    };
    match rows.next().await {
        Ok(Some(row)) => row.get::<i64>(0).unwrap_or(0).max(0) as u64,
        _ => 0,
    }
}

/// Sums the per-pass admission-work receipts persisted on the refusal
/// markers. A read failure is not silent: it lands in the report's error list
/// and the rollup stays at its zero default.
async fn read_admission_work_rollup(
    conn: &(impl QueryExecutor + ?Sized),
    errors: &mut Vec<String>,
) -> ObservationAdmissionWorkRollupV1 {
    const SQL: &str = "SELECT COUNT(*),
                COALESCE(SUM(stored_rows_decoded), 0),
                COALESCE(SUM(identity_derivations), 0),
                COALESCE(SUM(payload_digests), 0),
                COALESCE(SUM(runtime_commands), 0)
         FROM observation_admission_refusals";
    let decoded: std::result::Result<ObservationAdmissionWorkRollupV1, String> = async {
        let mut rows = conn
            .query(SQL, ())
            .await
            .map_err(|error| format!("admission work rollup query failed: {error}"))?;
        let Some(row) = rows
            .next()
            .await
            .map_err(|error| format!("admission work rollup read failed: {error}"))?
        else {
            return Err("admission work rollup returned no aggregate row".to_string());
        };
        let column = |index: i32, field: &str| {
            row.get::<i64>(index)
                .map_err(|error| format!("admission work rollup {field} failed: {error}"))
                .and_then(|value| {
                    u64::try_from(value)
                        .map_err(|_| format!("admission work rollup {field} was negative"))
                })
        };
        Ok(ObservationAdmissionWorkRollupV1 {
            refusal_markers: column(0, "marker count")?,
            stored_rows_decoded: column(1, "stored-row decode sum")?,
            identity_derivations: column(2, "identity derivation sum")?,
            payload_digests: column(3, "payload digest sum")?,
            runtime_commands: column(4, "runtime command sum")?,
        })
    }
    .await;
    match decoded {
        Ok(rollup) => rollup,
        Err(error) => {
            errors.push(error);
            ObservationAdmissionWorkRollupV1::default()
        }
    }
}

async fn row_count(conn: &(impl QueryExecutor + ?Sized), sql: &str) -> u64 {
    let Ok(mut rows) = conn.query(sql, ()).await else {
        return 0;
    };
    match rows.next().await {
        Ok(Some(row)) => row.get::<i64>(0).unwrap_or(0).max(0) as u64,
        _ => 0,
    }
}

/// Count of rows in `table` whose `column` still carries a live payload (i.e.
/// has not been rewritten to a `{"__retention_released": …}` marker), optionally
/// scoped through an anchor join to a projection generation.
async fn live_payload_count(
    conn: &(impl QueryExecutor + ?Sized),
    sql: &str,
    generation: Option<&str>,
) -> u64 {
    let Ok(mut rows) = conn.query(sql, params![opt_text(generation)]).await else {
        return 0;
    };
    match rows.next().await {
        Ok(Some(row)) => row.get::<i64>(0).unwrap_or(0).max(0) as u64,
        _ => 0,
    }
}

const ANCHOR_PAYLOAD_COUNT_SQL: &str = "SELECT COUNT(*) FROM retrieval_anchors a
     WHERE (?1 IS NULL OR a.projection_generation = ?1)
       AND json_extract(a.anchor_json, '$.__retention_released') IS NULL";

const OBSERVATION_PAYLOAD_COUNT_SQL: &str = "SELECT COUNT(*) FROM observations o
     WHERE (?1 IS NULL OR EXISTS (
         SELECT 1 FROM observation_retrieval_anchors b
         JOIN retrieval_anchors a ON a.anchor_id = b.anchor_id
         WHERE b.observation_id = o.observation_id
           AND a.projection_generation = ?1
     ))
       AND json_extract(o.observation_json, '$.__retention_released') IS NULL";

const CURSOR_ADVANCE_COUNT_SQL: &str = "SELECT COUNT(*) FROM source_cursor_advances";

/// `generation` scopes every pass to a single `projection_generation` (`None`
/// spans all generations). In [`RetentionMode::DryRun`] nothing is mutated and
/// each phase reports the candidate count and bytes that *would* be reclaimed.
pub async fn run_observation_retention(
    database: &Database,
    generation: Option<&str>,
    config: &ObservationRetentionConfig,
    mode: RetentionMode,
    now: i64,
) -> Result<ObservationRetentionReport> {
    let reader = database.read_connection();
    let anchor_payloads_before =
        live_payload_count(&reader, ANCHOR_PAYLOAD_COUNT_SQL, generation).await;
    let observation_payloads_before =
        live_payload_count(&reader, OBSERVATION_PAYLOAD_COUNT_SQL, generation).await;
    let cursor_advances_before = row_count(&reader, CURSOR_ADVANCE_COUNT_SQL).await;
    let freelist_before = pragma_u64(&reader, "freelist_count").await;
    let page_count_before = pragma_u64(&reader, "page_count").await;

    let mut report = ObservationRetentionReport {
        generation: generation.map(str::to_string),
        applied: mode.is_apply(),
        started_at: now,
        ended_at: now,
        anchors_released: ObservationRetentionPhaseReport::default(),
        observations_released: ObservationRetentionPhaseReport::default(),
        provenance_released: ObservationRetentionPhaseReport::default(),
        cursor_advances_reclaimed: ObservationRetentionPhaseReport::default(),
        anchor_payloads_before,
        anchor_payloads_after: anchor_payloads_before,
        observation_payloads_before,
        observation_payloads_after: observation_payloads_before,
        cursor_advances_before,
        cursor_advances_after: cursor_advances_before,
        freelist_before,
        freelist_after: freelist_before,
        page_count_before,
        page_count_after: page_count_before,
        admission_work: ObservationAdmissionWorkRollupV1::default(),
        errors: Vec::new(),
    };
    report.admission_work = read_admission_work_rollup(&reader, &mut report.errors).await;

    if !config.enabled || !config.any_window() {
        report.anchors_released.window_days = config.anchor_release_after_days;
        report.observations_released.window_days = config.observation_release_after_days;
        report.provenance_released.window_days = config.provenance_release_after_days;
        return Ok(report);
    }

    report.anchors_released =
        run_anchor_pass(database, generation, config, mode, now, &mut report.errors).await?;
    report.observations_released =
        run_observation_pass(database, generation, config, mode, now, &mut report.errors).await?;
    report.provenance_released =
        run_provenance_pass(database, generation, config, mode, now, &mut report.errors).await?;
    report.cursor_advances_reclaimed =
        run_cursor_advance_pass(database, config, mode, &mut report.errors).await?;

    report.ended_at = now;
    let reader = database.read_connection();
    report.anchor_payloads_after =
        live_payload_count(&reader, ANCHOR_PAYLOAD_COUNT_SQL, generation).await;
    report.observation_payloads_after =
        live_payload_count(&reader, OBSERVATION_PAYLOAD_COUNT_SQL, generation).await;
    report.cursor_advances_after = row_count(&reader, CURSOR_ADVANCE_COUNT_SQL).await;
    report.freelist_after = pragma_u64(&reader, "freelist_count").await;
    report.page_count_after = pragma_u64(&reader, "page_count").await;
    Ok(report)
}

async fn commit_transaction(transaction: DatabaseWriteTransaction<'_>) -> Result<()> {
    transaction.commit().await.map_err(db_error)
}

async fn execute_required(executor: &(impl Executor + ?Sized), sql: &str) -> Result<()> {
    executor
        .execute(sql, ())
        .await
        .map(|_| ())
        .map_err(db_error)
}

enum RetentionQueryExecutor<'reader, 'database> {
    Read(&'reader DatabaseEngineReadConnection),
    Transaction(&'reader DatabaseWriteTransaction<'database>),
}

impl RetentionQueryExecutor<'_, '_> {
    async fn query(
        &self,
        sql: &str,
        params: Params,
    ) -> tracedecay_runtime_core::db::engine::Result<tracedecay_runtime_core::db::engine::Rows>
    {
        match self {
            Self::Read(connection) => connection.query(sql, params).await,
            Self::Transaction(transaction) => transaction.query(sql, params).await,
        }
    }
}

/// Reclaimed bytes for one released column: the original length minus the
/// compact marker that replaces it (saturating so a payload already smaller
/// than the marker never underflows).
fn reclaimed_bytes(original_len: u64, marker: &str) -> u64 {
    original_len.saturating_sub(marker.len() as u64)
}

struct AnchorTarget {
    anchor_id: String,
    original_len: u64,
    effective_at: i64,
}

async fn run_anchor_pass(
    database: &Database,
    generation: Option<&str>,
    config: &ObservationRetentionConfig,
    mode: RetentionMode,
    now: i64,
    errors: &mut Vec<String>,
) -> Result<ObservationRetentionPhaseReport> {
    let mut report = ObservationRetentionPhaseReport {
        window_days: config.anchor_release_after_days,
        ..ObservationRetentionPhaseReport::default()
    };
    let Some(window) = config.anchor_release_after_days else {
        return Ok(report);
    };
    let cutoff = cutoff_secs(window, now);
    let sql = format!(
        "SELECT a.anchor_id, LENGTH(a.anchor_json) AS len,
                (
                    SELECT d.effective_at
                    FROM retrieval_anchor_dispositions d
                    WHERE d.anchor_id = a.anchor_id AND d.owner_json = a.owner_json
                    ORDER BY d.sequence DESC
                    LIMIT 1
                ) AS effective_at
         FROM retrieval_anchors a
         WHERE (?1 IS NULL OR a.projection_generation = ?1)
           AND json_extract(a.anchor_json, '$.__retention_released') IS NULL
           AND {RELEASED_DISPOSITION}
         ORDER BY a.anchor_id ASC
         LIMIT ?3"
    );
    let transaction = if mode.is_apply() {
        Some(
            database
                .begin_write_transaction("begin anchor retention pass")
                .await
                .map_err(db_error)?,
        )
    } else {
        None
    };
    let reader = database.read_connection();
    let query_executor = transaction.as_ref().map_or(
        RetentionQueryExecutor::Read(&reader),
        RetentionQueryExecutor::Transaction,
    );
    let mut rows = query_executor
        .query(
            &sql,
            params![opt_text(generation), cutoff, config.batch_limit()],
        )
        .await
        .map_err(db_error)?;
    let mut targets = Vec::new();
    while let Some(row) = rows.next().await.map_err(db_error)? {
        targets.push(AnchorTarget {
            anchor_id: row.get(0).map_err(db_error)?,
            original_len: row.get::<i64>(1).map_err(db_error)?.max(0) as u64,
            effective_at: row.get(2).map_err(db_error)?,
        });
    }
    report.eligible = targets.len() as u64;
    report.oldest_eligible_at = targets.iter().map(|target| target.effective_at).min();
    if !mode.is_apply() {
        report.bytes_reclaimed = targets
            .iter()
            .map(|t| reclaimed_bytes(t.original_len, ANCHOR_RELEASED_MARKER))
            .sum();
        return Ok(report);
    }

    // Drop the update trigger, rewrite the fat column to the compact marker,
    // then recreate the identical trigger — atomically, so immutability is
    // never observably relaxed and a crash rolls back to the triggered schema.
    let txn = require_apply_transaction(
        transaction,
        "apply mode requires an open anchor retention transaction",
    )?;
    execute_required(&txn, DROP_ANCHOR_UPDATE_TRIGGER).await?;
    for chunk in targets.chunks(RETENTION_DML_CHUNK) {
        let placeholders = vec!["?"; chunk.len()].join(",");
        let sql = format!(
            "UPDATE retrieval_anchors SET anchor_json = ? WHERE anchor_id IN ({placeholders})"
        );
        let mut values = Vec::with_capacity(chunk.len() + 1);
        values.push(Value::Text(ANCHOR_RELEASED_MARKER.to_string()));
        values.extend(
            chunk
                .iter()
                .map(|target| Value::Text(target.anchor_id.clone())),
        );
        match txn.execute(&sql, values).await {
            Ok(count) => {
                report.acted = report.acted.saturating_add(count);
                let reclaimed: u64 = chunk
                    .iter()
                    .map(|target| reclaimed_bytes(target.original_len, ANCHOR_RELEASED_MARKER))
                    .sum();
                report.bytes_reclaimed = report.bytes_reclaimed.saturating_add(reclaimed);
            }
            Err(err) => errors.push(format!(
                "release anchor batch ({} ids starting {}): {err}",
                chunk.len(),
                chunk[0].anchor_id
            )),
        }
    }
    execute_required(&txn, CREATE_ANCHOR_UPDATE_TRIGGER).await?;
    commit_transaction(txn).await?;
    Ok(report)
}

struct ObservationTarget {
    observation_id: String,
    original_len: u64,
    effective_at: i64,
}

async fn run_observation_pass(
    database: &Database,
    generation: Option<&str>,
    config: &ObservationRetentionConfig,
    mode: RetentionMode,
    now: i64,
    errors: &mut Vec<String>,
) -> Result<ObservationRetentionPhaseReport> {
    let mut report = ObservationRetentionPhaseReport {
        window_days: config.observation_release_after_days,
        ..ObservationRetentionPhaseReport::default()
    };
    let Some(window) = config.observation_release_after_days else {
        return Ok(report);
    };
    let cutoff = cutoff_secs(window, now);
    // An observation is released once per observation only when every anchor
    // bound to it has reached a released disposition past the window. One
    // active, unavailable, missing-disposition, or not-yet-due binding keeps
    // the shared payload live, including bindings outside `generation`.
    // The production authority schema makes observations immutable. The
    // maintenance transaction temporarily suspends only its UPDATE guard,
    // rewrites the payload, and restores the exact canonical trigger before
    // commit.
    let sql = "SELECT o.observation_id, LENGTH(o.observation_json) AS len,
                released.effective_at
         FROM observations o
         JOIN (
             SELECT b.observation_id, MIN(d.effective_at) AS effective_at
             FROM observation_retrieval_anchors b
             JOIN retrieval_anchors a ON a.anchor_id = b.anchor_id
             JOIN retrieval_anchor_dispositions d
               ON d.anchor_id = a.anchor_id AND d.owner_json = a.owner_json
             WHERE (?1 IS NULL OR a.projection_generation = ?1)
               AND d.sequence = (
                   SELECT MAX(d2.sequence)
                   FROM retrieval_anchor_dispositions d2
                   WHERE d2.anchor_id = a.anchor_id
                     AND d2.owner_json = a.owner_json
               )
               AND d.state IN ('superseded', 'deleted')
               AND d.effective_at < ?2
               AND NOT EXISTS (
                   SELECT 1
                   FROM observation_retrieval_anchors live_binding
                   JOIN retrieval_anchors live_anchor
                     ON live_anchor.anchor_id = live_binding.anchor_id
                   WHERE live_binding.observation_id = b.observation_id
                     AND NOT EXISTS (
                         SELECT 1
                         FROM retrieval_anchor_dispositions live_disposition
                         WHERE live_disposition.anchor_id = live_anchor.anchor_id
                           AND live_disposition.owner_json = live_anchor.owner_json
                           AND live_disposition.sequence = (
                               SELECT MAX(live_latest.sequence)
                               FROM retrieval_anchor_dispositions live_latest
                               WHERE live_latest.anchor_id = live_anchor.anchor_id
                                 AND live_latest.owner_json = live_anchor.owner_json
                           )
                           AND live_disposition.state IN ('superseded', 'deleted')
                           AND live_disposition.effective_at < ?2
                     )
               )
             GROUP BY b.observation_id
         ) released ON released.observation_id = o.observation_id
         WHERE json_extract(o.observation_json, '$.__retention_released') IS NULL
         ORDER BY o.sequence ASC
         LIMIT ?3"
        .to_string();
    let transaction = if mode.is_apply() {
        Some(
            database
                .begin_write_transaction("begin observation retention pass")
                .await
                .map_err(db_error)?,
        )
    } else {
        None
    };
    let reader = database.read_connection();
    let query_executor = transaction.as_ref().map_or(
        RetentionQueryExecutor::Read(&reader),
        RetentionQueryExecutor::Transaction,
    );
    let mut rows = query_executor
        .query(
            &sql,
            params![opt_text(generation), cutoff, config.batch_limit()],
        )
        .await
        .map_err(db_error)?;
    let mut targets = Vec::new();
    while let Some(row) = rows.next().await.map_err(db_error)? {
        targets.push(ObservationTarget {
            observation_id: row.get(0).map_err(db_error)?,
            original_len: row.get::<i64>(1).map_err(db_error)?.max(0) as u64,
            effective_at: row.get(2).map_err(db_error)?,
        });
    }
    report.eligible = targets.len() as u64;
    report.oldest_eligible_at = targets.iter().map(|target| target.effective_at).min();
    if !mode.is_apply() {
        report.bytes_reclaimed = targets
            .iter()
            .map(|t| reclaimed_bytes(t.original_len, OBSERVATION_RELEASED_MARKER))
            .sum();
        return Ok(report);
    }

    let txn = require_apply_transaction(
        transaction,
        "apply mode requires an open observation retention transaction",
    )?;
    execute_required(&txn, DROP_OBSERVATION_UPDATE_TRIGGER).await?;
    for chunk in targets.chunks(RETENTION_DML_CHUNK) {
        let placeholders = vec!["?"; chunk.len()].join(",");
        let sql = format!(
            "UPDATE observations SET observation_json = ? WHERE observation_id IN ({placeholders})"
        );
        let mut values = Vec::with_capacity(chunk.len() + 1);
        values.push(Value::Text(OBSERVATION_RELEASED_MARKER.to_string()));
        values.extend(
            chunk
                .iter()
                .map(|target| Value::Text(target.observation_id.clone())),
        );
        match txn.execute(&sql, values).await {
            Ok(count) => {
                report.acted = report.acted.saturating_add(count);
                let reclaimed: u64 = chunk
                    .iter()
                    .map(|target| reclaimed_bytes(target.original_len, OBSERVATION_RELEASED_MARKER))
                    .sum();
                report.bytes_reclaimed = report.bytes_reclaimed.saturating_add(reclaimed);
            }
            Err(err) => errors.push(format!(
                "release observation batch ({} ids starting {}): {err}",
                chunk.len(),
                chunk[0].observation_id
            )),
        }
    }
    execute_required(&txn, CREATE_OBSERVATION_UPDATE_TRIGGER).await?;
    commit_transaction(txn).await?;
    Ok(report)
}

struct ProvenanceTarget {
    observation_id: String,
    original_len: u64,
    effective_at: i64,
}

async fn run_provenance_pass(
    database: &Database,
    generation: Option<&str>,
    config: &ObservationRetentionConfig,
    mode: RetentionMode,
    now: i64,
    errors: &mut Vec<String>,
) -> Result<ObservationRetentionPhaseReport> {
    let mut report = ObservationRetentionPhaseReport {
        window_days: config.provenance_release_after_days,
        ..ObservationRetentionPhaseReport::default()
    };
    let Some(window) = config.provenance_release_after_days else {
        return Ok(report);
    };
    let cutoff = cutoff_secs(window, now);
    // Only rows that carry a provenance anchor are released; the anchor linkage
    // (`retrieval_anchor_id`/`owner_json`) is preserved so the row's CHECK
    // couplings and foreign key stay valid. `capture_json` is rewritten to a
    // non-null marker, keeping `(capture_json IS NULL) = (retrieval_anchor_id
    // IS NULL)` satisfied.
    let sql = format!(
        "SELECT p.observation_id,
                LENGTH(p.availability_json) + LENGTH(COALESCE(p.capture_json, '')) AS len,
                (
                    SELECT d.effective_at
                    FROM retrieval_anchor_dispositions d
                    WHERE d.anchor_id = a.anchor_id AND d.owner_json = a.owner_json
                    ORDER BY d.sequence DESC
                    LIMIT 1
                ) AS effective_at
         FROM observation_repository_provenance p
         JOIN retrieval_anchors a ON a.anchor_id = p.retrieval_anchor_id
         WHERE (?1 IS NULL OR a.projection_generation = ?1)
           AND p.retrieval_anchor_id IS NOT NULL
           AND json_extract(p.availability_json, '$.__retention_released') IS NULL
           AND {RELEASED_DISPOSITION}
         ORDER BY p.observation_id ASC
         LIMIT ?3"
    );
    let transaction = if mode.is_apply() {
        Some(
            database
                .begin_write_transaction("begin provenance retention pass")
                .await
                .map_err(db_error)?,
        )
    } else {
        None
    };
    let reader = database.read_connection();
    let query_executor = transaction.as_ref().map_or(
        RetentionQueryExecutor::Read(&reader),
        RetentionQueryExecutor::Transaction,
    );
    let mut rows = query_executor
        .query(
            &sql,
            params![opt_text(generation), cutoff, config.batch_limit()],
        )
        .await
        .map_err(db_error)?;
    let mut targets = Vec::new();
    while let Some(row) = rows.next().await.map_err(db_error)? {
        targets.push(ProvenanceTarget {
            observation_id: row.get(0).map_err(db_error)?,
            original_len: row.get::<i64>(1).map_err(db_error)?.max(0) as u64,
            effective_at: row.get(2).map_err(db_error)?,
        });
    }
    report.eligible = targets.len() as u64;
    report.oldest_eligible_at = targets.iter().map(|target| target.effective_at).min();
    if !mode.is_apply() {
        report.bytes_reclaimed = targets
            .iter()
            .map(|t| reclaimed_bytes(t.original_len, PROVENANCE_RELEASED_MARKER))
            .sum();
        return Ok(report);
    }

    let txn = require_apply_transaction(
        transaction,
        "apply mode requires an open provenance retention transaction",
    )?;
    execute_required(&txn, DROP_PROVENANCE_UPDATE_TRIGGER).await?;
    for chunk in targets.chunks(RETENTION_DML_CHUNK) {
        // ?1 is the shared marker (bound once, reused for both fat columns);
        // the `IN` list starts at ?2 so the marker index isn't reused for ids.
        let placeholders = (0..chunk.len())
            .map(|index| format!("?{}", index + 2))
            .collect::<Vec<_>>()
            .join(",");
        let sql = format!(
            "UPDATE observation_repository_provenance
             SET availability_json = ?1, capture_json = ?1
             WHERE observation_id IN ({placeholders})"
        );
        let mut values = Vec::with_capacity(chunk.len() + 1);
        values.push(Value::Text(PROVENANCE_RELEASED_MARKER.to_string()));
        values.extend(
            chunk
                .iter()
                .map(|target| Value::Text(target.observation_id.clone())),
        );
        match txn.execute(&sql, values).await {
            Ok(count) => {
                report.acted = report.acted.saturating_add(count);
                let reclaimed: u64 = chunk
                    .iter()
                    .map(|target| reclaimed_bytes(target.original_len, PROVENANCE_RELEASED_MARKER))
                    .sum();
                report.bytes_reclaimed = report.bytes_reclaimed.saturating_add(reclaimed);
            }
            Err(err) => errors.push(format!(
                "release provenance batch ({} ids starting {}): {err}",
                chunk.len(),
                chunk[0].observation_id
            )),
        }
    }
    execute_required(&txn, CREATE_PROVENANCE_UPDATE_TRIGGER).await?;
    commit_transaction(txn).await?;
    Ok(report)
}

struct CursorAdvanceTarget {
    rowid: i64,
    original_len: u64,
}

/// Reclaims only advance receipts that the current cursor frontier strictly
/// supersedes. An advance at the exact current generation/domain/end is kept:
/// it may be the sole durable authority for a non-observation cursor advance.
/// Older generations and lower positions in the current generation are no
/// longer replayable frontiers and can be removed without weakening recovery.
async fn run_cursor_advance_pass(
    database: &Database,
    config: &ObservationRetentionConfig,
    mode: RetentionMode,
    errors: &mut Vec<String>,
) -> Result<ObservationRetentionPhaseReport> {
    let mut report = ObservationRetentionPhaseReport::default();
    if !config.reclaim_superseded_cursor_advances {
        return Ok(report);
    }
    let sql = "SELECT advance.rowid,
                LENGTH(advance.source_json) + LENGTH(advance.scope_json)
                + LENGTH(advance.coverage_json) + LENGTH(advance.reason)
                + LENGTH(COALESCE(advance.receipt_id, '')) AS payload_len,
                current.cursor_json, advance.coverage_json
         FROM source_cursor_advances AS advance
         JOIN source_cursors AS current
           ON current.source_json = advance.source_json
          AND current.scope_json = advance.scope_json
         WHERE advance.rowid > ?1
         ORDER BY advance.rowid
         LIMIT ?2";
    let transaction = if mode.is_apply() {
        Some(
            database
                .begin_write_transaction("begin source cursor advance retention pass")
                .await
                .map_err(db_error)?,
        )
    } else {
        None
    };
    let reader = database.read_connection();
    let query_executor = transaction.as_ref().map_or(
        RetentionQueryExecutor::Read(&reader),
        RetentionQueryExecutor::Transaction,
    );
    let mut targets = Vec::new();
    let target_limit = config.max_batch_size.max(1);
    let mut scan_cursor = 0_i64;
    loop {
        let mut rows = query_executor
            .query(sql, params![scan_cursor, CURSOR_ADVANCE_SCAN_PAGE_ROWS])
            .await
            .map_err(db_error)?;
        let mut page_rows = 0_i64;
        while let Some(row) = rows.next().await.map_err(db_error)? {
            page_rows += 1;
            scan_cursor = row.get(0).map_err(db_error)?;
            let current_json = row.get::<String>(2).map_err(db_error)?;
            let coverage_json = row.get::<String>(3).map_err(db_error)?;
            let current: ObservationSourceCursorV1 =
                serde_json::from_str(&current_json).map_err(db_error)?;
            let coverage: ObservationCoverageV1 =
                serde_json::from_str(&coverage_json).map_err(db_error)?;
            let superseded = current.generation() != coverage.generation()
                || (current.ordering_domain() == coverage.ordering_domain()
                    && current.position() > coverage.range().end());
            if superseded {
                targets.push(CursorAdvanceTarget {
                    rowid: scan_cursor,
                    original_len: row.get::<i64>(1).map_err(db_error)?.max(0) as u64,
                });
                if targets.len() == target_limit {
                    break;
                }
            }
        }
        drop(rows);
        if targets.len() == target_limit || page_rows < CURSOR_ADVANCE_SCAN_PAGE_ROWS {
            break;
        }
    }
    report.eligible = targets.len() as u64;
    report.bytes_reclaimed = targets.iter().map(|target| target.original_len).sum();
    if !mode.is_apply() || targets.is_empty() {
        return Ok(report);
    }

    let txn = require_apply_transaction(
        transaction,
        "apply mode requires an open cursor advance retention transaction",
    )?;
    execute_required(&txn, DROP_CURSOR_ADVANCE_DELETE_TRIGGER).await?;
    for chunk in targets.chunks(RETENTION_DML_CHUNK) {
        let placeholders = vec!["?"; chunk.len()].join(",");
        let sql = format!("DELETE FROM source_cursor_advances WHERE rowid IN ({placeholders})");
        let values = chunk
            .iter()
            .map(|target| Value::Integer(target.rowid))
            .collect::<Vec<_>>();
        match txn.execute(&sql, values).await {
            Ok(count) if count as usize == chunk.len() => {
                report.acted = report.acted.saturating_add(count);
            }
            Ok(count) => {
                report.acted = report.acted.saturating_add(count);
                errors.push(format!(
                    "reclaim source cursor advance batch ({} ids starting rowid {}): {} of {} rows disappeared",
                    chunk.len(),
                    chunk[0].rowid,
                    chunk.len() as u64 - count,
                    chunk.len()
                ));
            }
            Err(error) => errors.push(format!(
                "reclaim source cursor advance batch ({} ids starting rowid {}): {error}",
                chunk.len(),
                chunk[0].rowid
            )),
        }
    }
    execute_required(&txn, CREATE_CURSOR_ADVANCE_DELETE_TRIGGER).await?;
    commit_transaction(txn).await?;
    Ok(report)
}

#[cfg(test)]
mod tests;
