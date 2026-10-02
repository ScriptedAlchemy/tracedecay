//! Generation-scoped retention for the append-only observation evidence stores.
//!
//! The observation store keeps three append-only, forever-growing evidence
//! tables that dominated one observed `sessions.db`:
//!
//! * `observations`, the durable observation payload (`observation_json`,
//!   1.8 GB measured).
//! * `retrieval_anchors`, the immutable retrieval-anchor payload
//!   (`anchor_json`, 1.6 GB measured).
//! * `observation_repository_provenance`, the repository-provenance payload
//!   (`availability_json` + `capture_json`, 1.4 GB measured).
//!
//! Superseded and deleted dispositions release their storage. This module
//! is the retention pass that does that, mirroring the
//! sibling LCM slice ([`tracedecay_lcm::retention`]): a bounded,
//! DryRun/Apply, before/after-measured engine.
//!
//! # The disposition ledger is the governing authority
//!
//! Every anchor's lifecycle is recorded in the append-only
//! `retrieval_anchor_dispositions` ledger, whose *current* state for an anchor
//! is the highest-`sequence` row for its `(anchor_id, owner_json)`. The four
//! states carry different retention meaning:
//!
//! * `active`, live, referenced evidence. **Never** released.
//! * `unavailable`, the source is gone, but the evidence record is retained
//!   as the durable account of what was seen. **Never** released.
//! * `superseded`, a newer generation's anchor replaced this one.
//! * `deleted`, the evidence was retired (user request, retention, redaction,
//!   …).
//!
//! Only `superseded` and `deleted` current states release storage. Live and
//! source-unavailable evidence is never released: the `active`/`unavailable`
//! predicate branch is never selected.
//!
//! # Ledger-vs-payload design decision
//!
//! The ledger, its reverse-lineage, its derivative tombstones, and the anchor
//! *aliases* are all compact and are the audit trail of what happened to each
//! anchor. They are **never** mutated. Their `BEFORE UPDATE/DELETE
//! RAISE(ABORT)` immutability triggers stay in force and this module respects
//! them. The ledger's `FOREIGN KEY(anchor_id, owner_json)
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
//!, all inside one `Immediate` transaction, so immutability is never
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
//! Each pass discovers candidates from the disposition ledger past its own
//! durable cursor (`session_backfill_meta`), then reads only the evidence rows
//! those anchors govern. A tick with nothing newly due reads one ledger page,
//! never the evidence tables.
//!
//! The daemon reaches this engine through
//! [`crate::RegisteredGlobalDb::run_observation_retention`].

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use tracedecay_domain::UtcMicros;
use tracedecay_domain::errors::{Result, TraceDecayError};
use tracedecay_runtime_core::db::{
    Database, DatabaseWriteTransaction,
    engine::{Executor, IntoParams, Params, QueryExecutor, Value, opt_text, params},
};

const MICROS_PER_DAY: i64 = 24 * 60 * 60 * 1_000_000;

const OPERATION: &str = "observation evidence retention";

/// Row count per batched retention `UPDATE`/`DELETE ... WHERE id IN (...)`
/// statement. Keeps bound-parameter counts safely under SQLite's default
/// `SQLITE_LIMIT_VARIABLE_NUMBER` (999) regardless of the configured
/// `max_batch_size`, mirroring `project_registry::delete_code_projects`'s
/// chunking pattern.
const RETENTION_DML_CHUNK: usize = 500;

/// Compact tombstone written over a released `retrieval_anchors.anchor_json`.
const ANCHOR_RELEASED_MARKER: &str = "{\"__retention_released\":\"anchor\"}";
/// Compact tombstone written over a released `observations.observation_json`.
const OBSERVATION_RELEASED_MARKER: &str = "{\"__retention_released\":\"observation\"}";
/// Compact tombstone written over released provenance JSON columns.
pub(super) const PROVENANCE_RELEASED_MARKER: &str = "{\"__retention_released\":\"provenance\"}";

/// SQL fragment (over an anchor aliased `a`, cutoff bound as `?2`) that is true
/// when the anchor's *current* disposition (highest `sequence`) is `superseded`
/// or `deleted` and took effect before the cutoff. `active` and `unavailable`
/// current states never satisfy it, so live and source-unavailable evidence is
/// never released, the plan's non-goal encoded in SQL.
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

mod restore;
pub use restore::replay_current_release_state_for_restore;

fn db_error(source: impl std::error::Error + Send + Sync + 'static) -> TraceDecayError {
    TraceDecayError::database_operation(OPERATION, source)
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
    pub oldest_eligible_at: Option<UtcMicros>,
}

/// Aggregate report for a retention run, including measurable reclaim (row and
/// page/freelist counts before and after).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ObservationRetentionReport {
    /// Projection-generation scope (`None` spans every generation).
    pub generation: Option<String>,
    pub applied: bool,
    pub started_at: UtcMicros,
    pub ended_at: UtcMicros,
    pub anchors_released: ObservationRetentionPhaseReport,
    pub observations_released: ObservationRetentionPhaseReport,
    pub provenance_released: ObservationRetentionPhaseReport,
    /// Database `PRAGMA freelist_count` before/after (freed pages are the
    /// measurable, VACUUM-free signal that space was reclaimed).
    pub freelist_before: u64,
    pub freelist_after: u64,
    /// Database `PRAGMA page_count` before/after.
    pub page_count_before: u64,
    pub page_count_after: u64,
    pub errors: Vec<String>,
}

impl ObservationRetentionReport {
    /// Total payload bytes reclaimed across every pass.
    pub fn bytes_reclaimed(&self) -> u64 {
        self.anchors_released
            .bytes_reclaimed
            .saturating_add(self.observations_released.bytes_reclaimed)
            .saturating_add(self.provenance_released.bytes_reclaimed)
    }
}

/// The instant a released disposition must predate to release its evidence.
/// Dispositions record `effective_at` as [`UtcMicros`], so the window is
/// measured in the same unit.
fn release_cutoff(window_days: u32, now: UtcMicros) -> UtcMicros {
    UtcMicros(
        now.0
            .saturating_sub(i64::from(window_days).saturating_mul(MICROS_PER_DAY)),
    )
}

async fn query_u64(
    conn: &(impl QueryExecutor + ?Sized),
    sql: &str,
    query_params: impl IntoParams,
) -> Result<u64> {
    let mut rows = conn.query(sql, query_params).await.map_err(db_error)?;
    let count = rows
        .next()
        .await
        .map_err(db_error)?
        .ok_or_else(|| TraceDecayError::Database {
            operation: OPERATION.to_string(),
            message: "aggregate query returned no row".to_string(),
        })?
        .get::<i64>(0)
        .map_err(db_error)?;
    u64::try_from(count).map_err(|_| TraceDecayError::Database {
        operation: OPERATION.to_string(),
        message: format!("aggregate count cannot be negative: {count}"),
    })
}

async fn pragma_u64(conn: &(impl QueryExecutor + ?Sized), pragma: &str) -> Result<u64> {
    query_u64(conn, &format!("PRAGMA {pragma}"), ()).await
}

/// Where one release pass resumes reading the disposition ledger.
///
/// A disposition's evidence becomes releasable either when a released
/// disposition is appended already past the window, or when an appended one
/// ages past it. A pass therefore reads the ledger rows appended since its
/// last run (a `sequence` range) and the released rows that aged since (a
/// range of `idx_retrieval_anchor_dispositions_release_due`), never the
/// evidence tables or the whole ledger.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct LedgerCursor {
    appended_through: i64,
    aged_through: (i64, i64),
}

impl LedgerCursor {
    fn key(pass: &str) -> String {
        format!("observation_retention.{pass}.ledger_cursor")
    }

    async fn read(conn: &(impl QueryExecutor + ?Sized), pass: &str) -> Result<Self> {
        let mut rows = conn
            .query(
                "SELECT value FROM session_backfill_meta WHERE key = ?1",
                params![Self::key(pass)],
            )
            .await
            .map_err(db_error)?;
        let Some(row) = rows.next().await.map_err(db_error)? else {
            return Ok(Self {
                appended_through: 0,
                aged_through: (i64::MIN, i64::MIN),
            });
        };
        let value = row.get::<String>(0).map_err(db_error)?;
        let parsed = value.split(':').map(str::parse::<i64>).collect::<Vec<_>>();
        match parsed.as_slice() {
            [Ok(appended), Ok(effective_at), Ok(sequence)] => Ok(Self {
                appended_through: *appended,
                aged_through: (*effective_at, *sequence),
            }),
            _ => Err(TraceDecayError::Database {
                operation: OPERATION.to_string(),
                message: format!("invalid {pass} retention ledger cursor {value:?}"),
            }),
        }
    }

    fn encode(self) -> String {
        let (effective_at, sequence) = self.aged_through;
        format!("{}:{effective_at}:{sequence}", self.appended_through)
    }
}

async fn write_ledger_cursors(database: &Database, cursors: &[(&str, LedgerCursor)]) -> Result<()> {
    let txn = database
        .begin_write_transaction("record observation retention ledger cursors")
        .await
        .map_err(db_error)?;
    for (pass, cursor) in cursors {
        txn.execute(
            "INSERT INTO session_backfill_meta(key, value, updated_at)
             VALUES (?1, ?2, unixepoch())
             ON CONFLICT(key) DO UPDATE SET
                value = excluded.value, updated_at = excluded.updated_at",
            params![LedgerCursor::key(pass), cursor.encode()],
        )
        .await
        .map_err(db_error)?;
    }
    commit_transaction(txn).await
}

/// Ledger rows one disposition page reads.
const DISPOSITION_SCAN_PAGE_ROWS: i64 = 128;

/// Correlated subquery (over a disposition aliased `d`) for the sequence of
/// the current disposition of `d`'s anchor.
const LATEST_DISPOSITION_SQL: &str = "(SELECT MAX(latest.sequence)
    FROM retrieval_anchor_dispositions latest
    WHERE latest.anchor_id = d.anchor_id AND latest.owner_json = d.owner_json)";

/// Anchors whose current disposition was released before `cutoff` and became
/// releasable since `cursor`, at most `limit` of them, and the cursor past
/// every ledger row examined. The release statements re-check each anchor.
async fn released_anchors_since(
    database: &Database,
    cursor: LedgerCursor,
    cutoff: UtcMicros,
    limit: usize,
) -> Result<(Vec<String>, LedgerCursor)> {
    let reader = database.read_connection();
    let mut anchors = BTreeSet::new();
    let appended_through = released_anchors_appended(
        &reader,
        cursor.appended_through,
        cutoff,
        limit,
        &mut anchors,
    )
    .await?;
    let mut next = LedgerCursor {
        appended_through,
        ..cursor
    };
    let remaining = limit.saturating_sub(anchors.len());
    if remaining > 0 {
        next.aged_through = released_anchors_aged(
            &reader,
            cursor.aged_through,
            cutoff,
            remaining,
            &mut anchors,
        )
        .await?;
    }
    Ok((anchors.into_iter().collect(), next))
}

/// Adds the anchors whose released disposition was appended after `after`
/// already past the window, up to `limit` anchors in total, and returns the
/// sequence examined through. A released row not yet due is left for the aged
/// scan of the run that finds it past the window.
async fn released_anchors_appended(
    reader: &(impl QueryExecutor + ?Sized),
    after: i64,
    cutoff: UtcMicros,
    limit: usize,
    anchors: &mut BTreeSet<String>,
) -> Result<i64> {
    let mut examined = after;
    loop {
        let mut rows = reader
            .query(
                &format!(
                    "SELECT d.sequence, d.anchor_id, d.state, d.effective_at,
                            {LATEST_DISPOSITION_SQL}
                     FROM retrieval_anchor_dispositions d
                     WHERE d.sequence > ?1
                     ORDER BY d.sequence
                     LIMIT ?2"
                ),
                params![examined, DISPOSITION_SCAN_PAGE_ROWS],
            )
            .await
            .map_err(db_error)?;
        let mut page_rows = 0_i64;
        while let Some(row) = rows.next().await.map_err(db_error)? {
            page_rows += 1;
            let sequence = row.get::<i64>(0).map_err(db_error)?;
            let state = row.get::<String>(2).map_err(db_error)?;
            if matches!(state.as_str(), "superseded" | "deleted")
                && UtcMicros(row.get::<i64>(3).map_err(db_error)?) < cutoff
                && row.get::<i64>(4).map_err(db_error)? == sequence
            {
                anchors.insert(row.get::<String>(1).map_err(db_error)?);
            }
            examined = sequence;
            if anchors.len() >= limit {
                return Ok(examined);
            }
        }
        if page_rows < DISPOSITION_SCAN_PAGE_ROWS {
            return Ok(examined);
        }
    }
}

/// Adds up to `remaining` anchors whose released disposition aged past the
/// window after `aged_through`, and returns the `(effective_at, sequence)`
/// examined through.
async fn released_anchors_aged(
    reader: &(impl QueryExecutor + ?Sized),
    aged_through: (i64, i64),
    cutoff: UtcMicros,
    remaining: usize,
    anchors: &mut BTreeSet<String>,
) -> Result<(i64, i64)> {
    let (aged_effective_at, aged_sequence) = aged_through;
    let remaining = i64::try_from(remaining).unwrap_or(i64::MAX);
    let mut rows = reader
        .query(
            &format!(
                "SELECT d.sequence, d.anchor_id, d.effective_at, {LATEST_DISPOSITION_SQL}
                 FROM retrieval_anchor_dispositions d
                 WHERE d.state IN ('superseded', 'deleted')
                   AND d.effective_at < ?1
                   AND (d.effective_at > ?2 OR (d.effective_at = ?2 AND d.sequence > ?3))
                 ORDER BY d.effective_at, d.sequence
                 LIMIT ?4"
            ),
            params![cutoff.0, aged_effective_at, aged_sequence, remaining],
        )
        .await
        .map_err(db_error)?;
    let mut aged = 0_i64;
    let mut last_aged = None;
    while let Some(row) = rows.next().await.map_err(db_error)? {
        aged += 1;
        let sequence = row.get::<i64>(0).map_err(db_error)?;
        last_aged = Some((row.get::<i64>(2).map_err(db_error)?, sequence));
        if row.get::<i64>(3).map_err(db_error)? == sequence {
            anchors.insert(row.get::<String>(1).map_err(db_error)?);
        }
    }
    Ok(match last_aged {
        Some(last) if aged == remaining => last,
        // Every released row older than the cutoff has been examined.
        _ => (cutoff.0.saturating_sub(1), i64::MAX),
    })
}

/// `generation` scopes every pass to a single `projection_generation` (`None`
/// spans all generations). In [`RetentionMode::DryRun`] nothing is mutated and
/// each phase reports the candidate count and bytes that *would* be reclaimed.
#[hotpath::measure(future = true, label = "global_db.observation.retention")]
pub async fn run_observation_retention(
    database: &Database,
    generation: Option<&str>,
    config: &ObservationRetentionConfig,
    mode: RetentionMode,
    now: UtcMicros,
) -> Result<ObservationRetentionReport> {
    crate::hotpath_observe::record_snapshot_admissions(1);
    let reader = database.read_connection();
    let freelist_before = pragma_u64(&reader, "freelist_count").await?;
    let page_count_before = pragma_u64(&reader, "page_count").await?;

    let mut report = ObservationRetentionReport {
        generation: generation.map(str::to_string),
        applied: mode.is_apply(),
        started_at: now,
        ended_at: now,
        anchors_released: ObservationRetentionPhaseReport::default(),
        observations_released: ObservationRetentionPhaseReport::default(),
        provenance_released: ObservationRetentionPhaseReport::default(),
        freelist_before,
        freelist_after: freelist_before,
        page_count_before,
        page_count_after: page_count_before,
        errors: Vec::new(),
    };

    if !config.enabled || !config.any_window() {
        report.anchors_released.window_days = config.anchor_release_after_days;
        report.observations_released.window_days = config.observation_release_after_days;
        report.provenance_released.window_days = config.provenance_release_after_days;
        return Ok(report);
    }

    let mut pass = LedgerPass {
        database,
        generation,
        config,
        mode,
        errors: &mut report.errors,
        cursors: Vec::new(),
    };
    let anchors_released = run_anchor_pass(&mut pass, now).await?;
    let observations_released = run_observation_pass(&mut pass, now).await?;
    let provenance_released = run_provenance_pass(&mut pass, now).await?;
    let cursors = pass.cursors;
    report.anchors_released = anchors_released;
    report.observations_released = observations_released;
    report.provenance_released = provenance_released;
    if !cursors.is_empty() {
        write_ledger_cursors(database, &cursors).await?;
    }

    report.ended_at = now;
    let reader = database.read_connection();
    report.freelist_after = pragma_u64(&reader, "freelist_count").await?;
    report.page_count_after = pragma_u64(&reader, "page_count").await?;
    Ok(report)
}

#[hotpath::measure(future = true, label = "global_db.observation.retention.persist")]
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

/// Reclaimed bytes for one released column: the original length minus the
/// compact marker that replaces it (saturating so a payload already smaller
/// than the marker never underflows).
fn reclaimed_bytes(original_len: u64, marker: &str) -> u64 {
    original_len.saturating_sub(marker.len() as u64)
}

/// One row whose payload a pass selected for release.
struct ReleaseTarget {
    id: String,
    original_len: u64,
    effective_at: UtcMicros,
}

/// The in-place payload rewrite one pass applies to its selected batch.
struct PayloadRelease {
    intent: &'static str,
    drop_trigger: &'static str,
    create_trigger: &'static str,
    marker: &'static str,
    /// `UPDATE … RETURNING id` over `{ids}` that re-checks each row's
    /// eligibility (`?1` is the marker, `?2` the disposition cutoff).
    update: String,
    label: &'static str,
    window_days: Option<u32>,
    cutoff: UtcMicros,
}

/// Reads one pass's candidates on the reader. Selection scans the evidence
/// tables, so it never runs inside the write transaction; the release below
/// re-checks each selected row.
async fn select_release_targets(
    database: &Database,
    sql: &str,
    query_params: Params,
) -> Result<Vec<ReleaseTarget>> {
    let reader = database.read_connection();
    let mut rows = reader.query(sql, query_params).await.map_err(db_error)?;
    let mut targets = Vec::new();
    while let Some(row) = rows.next().await.map_err(db_error)? {
        targets.push(ReleaseTarget {
            id: row.get(0).map_err(db_error)?,
            original_len: row.get::<i64>(1).map_err(db_error)?.max(0) as u64,
            effective_at: UtcMicros(row.get(2).map_err(db_error)?),
        });
    }
    Ok(targets)
}

/// Selects a pass's bounded batch and, in apply mode, releases it.
async fn run_release_pass(
    database: &Database,
    selection: (&str, Params),
    release: PayloadRelease,
    mode: RetentionMode,
    errors: &mut Vec<String>,
) -> Result<ObservationRetentionPhaseReport> {
    let mut report = ObservationRetentionPhaseReport {
        window_days: release.window_days,
        ..ObservationRetentionPhaseReport::default()
    };
    let targets = select_release_targets(database, selection.0, selection.1).await?;
    report.eligible = targets.len() as u64;
    report.oldest_eligible_at = targets.iter().map(|target| target.effective_at).min();
    if !mode.is_apply() {
        report.bytes_reclaimed = targets
            .iter()
            .map(|target| reclaimed_bytes(target.original_len, release.marker))
            .sum();
        return Ok(report);
    }
    if !targets.is_empty() {
        release_payload_batch(database, &release, &targets, &mut report, errors).await?;
    }
    Ok(report)
}

/// Rewrites a selected batch's payload column to its released marker in one
/// bounded transaction. The transaction drops only that table's update guard,
/// rewrites the payload, and recreates the identical canonical trigger before
/// commit, so immutability is never observably relaxed and a crash rolls back
/// to the triggered schema. A row that stopped qualifying since it was
/// selected is left untouched.
async fn release_payload_batch(
    database: &Database,
    release: &PayloadRelease,
    targets: &[ReleaseTarget],
    report: &mut ObservationRetentionPhaseReport,
    errors: &mut Vec<String>,
) -> Result<()> {
    let txn = database
        .begin_write_transaction(release.intent)
        .await
        .map_err(db_error)?;
    execute_required(&txn, release.drop_trigger).await?;
    for chunk in targets.chunks(RETENTION_DML_CHUNK) {
        let ids = (0..chunk.len())
            .map(|index| format!("?{}", index + 3))
            .collect::<Vec<_>>()
            .join(",");
        let sql = release.update.replace("{ids}", &ids);
        let mut values = Vec::with_capacity(chunk.len() + 2);
        values.push(Value::Text(release.marker.to_owned()));
        values.push(Value::Integer(release.cutoff.0));
        values.extend(chunk.iter().map(|target| Value::Text(target.id.clone())));
        let mut rows = match txn.query(&sql, values).await {
            Ok(rows) => rows,
            Err(err) => {
                errors.push(format!(
                    "release {} batch ({} ids starting {}): {err}",
                    release.label,
                    chunk.len(),
                    chunk[0].id
                ));
                continue;
            }
        };
        let mut released = std::collections::BTreeSet::new();
        while let Some(row) = rows.next().await.map_err(db_error)? {
            released.insert(row.get::<String>(0).map_err(db_error)?);
        }
        drop(rows);
        report.acted = report.acted.saturating_add(released.len() as u64);
        report.bytes_reclaimed = report.bytes_reclaimed.saturating_add(
            chunk
                .iter()
                .filter(|target| released.contains(&target.id))
                .map(|target| reclaimed_bytes(target.original_len, release.marker))
                .sum(),
        );
    }
    execute_required(&txn, release.create_trigger).await?;
    commit_transaction(txn).await
}

/// Shared inputs of the three ledger-driven release passes, and the ledger
/// cursors an applied, unscoped run advances once its releases committed.
struct LedgerPass<'a> {
    database: &'a Database,
    generation: Option<&'a str>,
    config: &'a ObservationRetentionConfig,
    mode: RetentionMode,
    errors: &'a mut Vec<String>,
    cursors: Vec<(&'static str, LedgerCursor)>,
}

impl LedgerPass<'_> {
    /// Reads the anchors whose release became due since `pass`'s cursor and
    /// releases the rows `sql` selects for them (`?4` binds the anchor ids).
    async fn run(
        &mut self,
        pass: &'static str,
        cutoff: UtcMicros,
        sql: &str,
        release: PayloadRelease,
    ) -> Result<ObservationRetentionPhaseReport> {
        let reader = self.database.read_connection();
        let cursor = LedgerCursor::read(&reader, pass).await?;
        let (anchors, next) = released_anchors_since(
            self.database,
            cursor,
            cutoff,
            self.config.max_batch_size.max(1),
        )
        .await?;
        let errors_before = self.errors.len();
        let report = if anchors.is_empty() {
            ObservationRetentionPhaseReport {
                window_days: release.window_days,
                ..ObservationRetentionPhaseReport::default()
            }
        } else {
            let anchors = serde_json::to_string(&anchors).map_err(db_error)?;
            run_release_pass(
                self.database,
                (
                    sql,
                    params![
                        opt_text(self.generation),
                        cutoff.0,
                        self.config.batch_limit(),
                        anchors
                    ],
                ),
                release,
                self.mode,
                self.errors,
            )
            .await?
        };
        // A generation-scoped run examined only its generation's rows, and a
        // failed release stays behind the cursor so the next run retries it.
        if self.mode.is_apply()
            && self.generation.is_none()
            && next != cursor
            && self.errors.len() == errors_before
        {
            self.cursors.push((pass, next));
        }
        Ok(report)
    }
}

async fn run_anchor_pass(
    pass: &mut LedgerPass<'_>,
    now: UtcMicros,
) -> Result<ObservationRetentionPhaseReport> {
    let window_days = pass.config.anchor_release_after_days;
    let Some(window) = window_days else {
        return Ok(ObservationRetentionPhaseReport::default());
    };
    let cutoff = release_cutoff(window, now);
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
         WHERE a.anchor_id IN (SELECT value FROM json_each(?4))
           AND (?1 IS NULL OR a.projection_generation = ?1)
           AND a.anchor_json <> '{ANCHOR_RELEASED_MARKER}'
           AND {RELEASED_DISPOSITION}
         ORDER BY a.anchor_id ASC
         LIMIT ?3"
    );
    let release = PayloadRelease {
        intent: "begin anchor retention pass",
        drop_trigger: DROP_ANCHOR_UPDATE_TRIGGER,
        create_trigger: CREATE_ANCHOR_UPDATE_TRIGGER,
        marker: ANCHOR_RELEASED_MARKER,
        update: format!(
            "UPDATE retrieval_anchors AS a SET anchor_json = ?1
             WHERE a.anchor_id IN ({{ids}})
               AND a.anchor_json <> ?1
               AND {RELEASED_DISPOSITION}
             RETURNING anchor_id"
        ),
        label: "anchor",
        window_days,
        cutoff,
    };
    pass.run("anchor", cutoff, &sql, release).await
}

/// True when no anchor bound to `{observation}` still keeps its payload live:
/// every binding's current disposition is released past the window (`?2`).
fn no_live_binding(observation: &str) -> String {
    format!(
        "NOT EXISTS (
             SELECT 1
             FROM observation_retrieval_anchors live_binding
             JOIN retrieval_anchors live_anchor
               ON live_anchor.anchor_id = live_binding.anchor_id
             WHERE live_binding.observation_id = {observation}
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
         )"
    )
}

async fn run_observation_pass(
    pass: &mut LedgerPass<'_>,
    now: UtcMicros,
) -> Result<ObservationRetentionPhaseReport> {
    let window_days = pass.config.observation_release_after_days;
    let Some(window) = window_days else {
        return Ok(ObservationRetentionPhaseReport::default());
    };
    let cutoff = release_cutoff(window, now);
    // An observation is released once per observation only when every anchor
    // bound to it has reached a released disposition past the window. One
    // active, unavailable, missing-disposition, or not-yet-due binding keeps
    // the shared payload live, including bindings outside `generation`.
    // The production authority schema makes observations immutable. The
    // maintenance transaction temporarily suspends only its UPDATE guard,
    // rewrites the payload, and restores the exact canonical trigger before
    // commit.
    let sql = format!(
        "SELECT o.observation_id, LENGTH(o.observation_json) AS len,
                released.effective_at
         FROM observations o
         JOIN (
             SELECT b.observation_id, MIN(d.effective_at) AS effective_at
             FROM observation_retrieval_anchors b
             JOIN retrieval_anchors a ON a.anchor_id = b.anchor_id
             JOIN retrieval_anchor_dispositions d
               ON d.anchor_id = a.anchor_id AND d.owner_json = a.owner_json
             WHERE b.anchor_id IN (SELECT value FROM json_each(?4))
               AND (?1 IS NULL OR a.projection_generation = ?1)
               AND d.sequence = (
                   SELECT MAX(d2.sequence)
                   FROM retrieval_anchor_dispositions d2
                   WHERE d2.anchor_id = a.anchor_id
                     AND d2.owner_json = a.owner_json
               )
               AND d.state IN ('superseded', 'deleted')
               AND d.effective_at < ?2
               AND {live}
             GROUP BY b.observation_id
         ) released ON released.observation_id = o.observation_id
         WHERE o.observation_json <> '{OBSERVATION_RELEASED_MARKER}'
         ORDER BY o.sequence ASC
         LIMIT ?3",
        live = no_live_binding("b.observation_id"),
    );
    let release = PayloadRelease {
        intent: "begin observation retention pass",
        drop_trigger: DROP_OBSERVATION_UPDATE_TRIGGER,
        create_trigger: CREATE_OBSERVATION_UPDATE_TRIGGER,
        marker: OBSERVATION_RELEASED_MARKER,
        update: format!(
            "UPDATE observations AS o SET observation_json = ?1
             WHERE o.observation_id IN ({{ids}})
               AND o.observation_json <> ?1
               AND EXISTS (
                   SELECT 1 FROM observation_retrieval_anchors bound
                   WHERE bound.observation_id = o.observation_id
               )
               AND {live}
             RETURNING observation_id",
            live = no_live_binding("o.observation_id"),
        ),
        label: "observation",
        window_days,
        cutoff,
    };
    pass.run("observation", cutoff, &sql, release).await
}

async fn run_provenance_pass(
    pass: &mut LedgerPass<'_>,
    now: UtcMicros,
) -> Result<ObservationRetentionPhaseReport> {
    let window_days = pass.config.provenance_release_after_days;
    let Some(window) = window_days else {
        return Ok(ObservationRetentionPhaseReport::default());
    };
    let cutoff = release_cutoff(window, now);
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
         WHERE p.retrieval_anchor_id IN (SELECT value FROM json_each(?4))
           AND (?1 IS NULL OR a.projection_generation = ?1)
           AND p.retrieval_anchor_id IS NOT NULL
           AND p.availability_json <> '{PROVENANCE_RELEASED_MARKER}'
           AND {RELEASED_DISPOSITION}
         ORDER BY p.observation_id ASC
         LIMIT ?3"
    );
    let release = PayloadRelease {
        intent: "begin provenance retention pass",
        drop_trigger: DROP_PROVENANCE_UPDATE_TRIGGER,
        create_trigger: CREATE_PROVENANCE_UPDATE_TRIGGER,
        marker: PROVENANCE_RELEASED_MARKER,
        update: format!(
            "UPDATE observation_repository_provenance AS p
             SET availability_json = ?1, capture_json = ?1
             WHERE p.observation_id IN ({{ids}})
               AND p.retrieval_anchor_id IS NOT NULL
               AND p.availability_json <> ?1
               AND EXISTS (
                   SELECT 1 FROM retrieval_anchors a
                   WHERE a.anchor_id = p.retrieval_anchor_id
                     AND {RELEASED_DISPOSITION}
               )
             RETURNING observation_id"
        ),
        label: "provenance",
        window_days,
        cutoff,
    };
    pass.run("provenance", cutoff, &sql, release).await
}

#[cfg(test)]
mod tests;
