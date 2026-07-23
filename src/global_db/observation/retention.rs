//! Generation-scoped retention for the append-only observation evidence stores
//! (plan 38 §3, final clause).
//!
//! The observation store keeps three append-only, forever-growing evidence
//! tables that dominated one dogfood `sessions.db`:
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
//! sibling LCM slice ([`crate::sessions::lcm::retention`]): a bounded,
//! DryRun/Apply, before/after-measured, inert-by-default engine.
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
//! Because `retrieval_anchors` and `observation_repository_provenance` carry
//! their own `BEFORE UPDATE` immutability triggers, each releasing transaction
//! drops the relevant update trigger, rewrites the payload column, and
//! recreates the identical trigger — all inside one `Immediate` transaction, so
//! immutability is never observably relaxed and a crash mid-batch rolls back to
//! the fully-triggered schema. (`observations` carries no update trigger in the
//! canonical schema, so its payload is released with a plain `UPDATE`.)
//!
//! # Three passes, generation-scoped, bounded, inert by default
//!
//! Each pass has its own window (`None` = disabled) and is scoped to an
//! optional `projection_generation`. Every pass is capped by `max_batch_size`
//! and re-run-idempotent (already-released rows carry the marker and are
//! skipped), so the daemon can schedule it incrementally off the hot path. A
//! dry run counts eligible rows and the bytes that *would* be reclaimed without
//! mutating anything.
//!
//! Until the daemon seam is wired (see
//! [`super::GlobalDb::run_observation_retention`]), the engine is reachable
//! only from the retention tests, so its items are `dead_code` in a
//! non-`cfg(test)` library build. The allow is removed the moment a scheduler
//! calls the entry point.
#![allow(dead_code)]

use libsql::{Connection, TransactionBehavior, Value, params};
use serde::{Deserialize, Serialize};

use crate::errors::{Result, TraceDecayError};

const SECONDS_PER_DAY: i64 = 24 * 60 * 60;

const OPERATION: &str = "observation evidence retention";

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

const DROP_PROVENANCE_UPDATE_TRIGGER: &str =
    "DROP TRIGGER IF EXISTS observation_repository_provenance_immutable_update";
const CREATE_PROVENANCE_UPDATE_TRIGGER: &str = "CREATE TRIGGER IF NOT EXISTS \
     observation_repository_provenance_immutable_update BEFORE UPDATE ON \
     observation_repository_provenance BEGIN SELECT RAISE(ABORT, \
     'observation repository provenance is immutable'); END";

fn db_error(source: impl std::error::Error + Send + Sync + 'static) -> TraceDecayError {
    TraceDecayError::database_operation(OPERATION, source)
}

fn opt_text(value: Option<&str>) -> Value {
    value.map_or(Value::Null, |text| Value::Text(text.to_string()))
}

/// Per-table retention windows for the observation evidence stores. Every
/// window is `None` by default (unlimited): the evidence record is lossless
/// unless an operator explicitly opts a store in, matching
/// [`crate::sessions::lcm::retention::LcmRetentionConfig`] and
/// [`crate::retention`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ObservationRetentionConfig {
    /// Master switch. When `false`, [`run_observation_retention`] is a no-op
    /// even in [`RetentionMode::Apply`].
    #[serde(default)]
    pub enabled: bool,
    /// Window (days since the governing disposition took effect) after which a
    /// superseded/deleted anchor's `anchor_json` payload is released. `None`
    /// disables the anchor pass.
    #[serde(default)]
    pub anchor_release_after_days: Option<u32>,
    /// Window after which an observation whose bound anchor is superseded/
    /// deleted has its `observation_json` payload released. `None` disables the
    /// observation pass.
    #[serde(default)]
    pub observation_release_after_days: Option<u32>,
    /// Window after which a provenance row whose anchor is superseded/deleted
    /// has its `availability_json`/`capture_json` payload released. `None`
    /// disables the provenance pass.
    #[serde(default)]
    pub provenance_release_after_days: Option<u32>,
    /// Upper bound on rows touched per pass, keeping each run incremental and
    /// off the hot path.
    #[serde(default = "default_max_batch_size")]
    pub max_batch_size: usize,
}

fn default_max_batch_size() -> usize {
    500
}

impl Default for ObservationRetentionConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            anchor_release_after_days: None,
            observation_release_after_days: None,
            provenance_release_after_days: None,
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
    /// Count of `retrieval_anchors` whose `anchor_json` still carries a payload
    /// (not yet released), before/after the run.
    pub anchor_payloads_before: u64,
    pub anchor_payloads_after: u64,
    /// Count of `observations` whose `observation_json` still carries a payload,
    /// before/after the run.
    pub observation_payloads_before: u64,
    pub observation_payloads_after: u64,
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

fn cutoff_secs(window_days: u32, now_secs: i64) -> i64 {
    now_secs.saturating_sub(i64::from(window_days).saturating_mul(SECONDS_PER_DAY))
}

async fn pragma_u64(conn: &Connection, pragma: &str) -> u64 {
    let sql = format!("PRAGMA {pragma}");
    let Ok(mut rows) = conn.query(&sql, ()).await else {
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
async fn live_payload_count(conn: &Connection, sql: &str, generation: Option<&str>) -> u64 {
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

/// Runs the configured observation-evidence retention passes.
///
/// `generation` scopes every pass to a single `projection_generation` (`None`
/// spans all generations). In [`RetentionMode::DryRun`] nothing is mutated and
/// each phase reports the candidate count and bytes that *would* be reclaimed.
pub async fn run_observation_retention(
    conn: &Connection,
    generation: Option<&str>,
    config: &ObservationRetentionConfig,
    mode: RetentionMode,
    now: i64,
) -> Result<ObservationRetentionReport> {
    let anchor_payloads_before =
        live_payload_count(conn, ANCHOR_PAYLOAD_COUNT_SQL, generation).await;
    let observation_payloads_before =
        live_payload_count(conn, OBSERVATION_PAYLOAD_COUNT_SQL, generation).await;
    let freelist_before = pragma_u64(conn, "freelist_count").await;
    let page_count_before = pragma_u64(conn, "page_count").await;

    let mut report = ObservationRetentionReport {
        generation: generation.map(str::to_string),
        applied: mode.is_apply(),
        started_at: now,
        ended_at: now,
        anchors_released: ObservationRetentionPhaseReport::default(),
        observations_released: ObservationRetentionPhaseReport::default(),
        provenance_released: ObservationRetentionPhaseReport::default(),
        anchor_payloads_before,
        anchor_payloads_after: anchor_payloads_before,
        observation_payloads_before,
        observation_payloads_after: observation_payloads_before,
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

    report.anchors_released =
        run_anchor_pass(conn, generation, config, mode, now, &mut report.errors).await?;
    report.observations_released =
        run_observation_pass(conn, generation, config, mode, now, &mut report.errors).await?;
    report.provenance_released =
        run_provenance_pass(conn, generation, config, mode, now, &mut report.errors).await?;

    report.ended_at = now;
    report.anchor_payloads_after =
        live_payload_count(conn, ANCHOR_PAYLOAD_COUNT_SQL, generation).await;
    report.observation_payloads_after =
        live_payload_count(conn, OBSERVATION_PAYLOAD_COUNT_SQL, generation).await;
    report.freelist_after = pragma_u64(conn, "freelist_count").await;
    report.page_count_after = pragma_u64(conn, "page_count").await;
    Ok(report)
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
}

async fn run_anchor_pass(
    conn: &Connection,
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
        "SELECT a.anchor_id, LENGTH(a.anchor_json) AS len
         FROM retrieval_anchors a
         WHERE (?1 IS NULL OR a.projection_generation = ?1)
           AND json_extract(a.anchor_json, '$.__retention_released') IS NULL
           AND {RELEASED_DISPOSITION}
         ORDER BY a.anchor_id ASC
         LIMIT ?3"
    );
    let mut rows = conn
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
        });
    }
    report.eligible = targets.len() as u64;
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
    let txn = conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .await
        .map_err(db_error)?;
    txn.execute(DROP_ANCHOR_UPDATE_TRIGGER, ())
        .await
        .map_err(db_error)?;
    for target in &targets {
        match txn
            .execute(
                "UPDATE retrieval_anchors SET anchor_json = ?2 WHERE anchor_id = ?1",
                params![target.anchor_id.as_str(), ANCHOR_RELEASED_MARKER],
            )
            .await
        {
            Ok(_) => {
                report.acted += 1;
                report.bytes_reclaimed = report
                    .bytes_reclaimed
                    .saturating_add(reclaimed_bytes(target.original_len, ANCHOR_RELEASED_MARKER));
            }
            Err(err) => errors.push(format!("release anchor {}: {err}", target.anchor_id)),
        }
    }
    txn.execute(CREATE_ANCHOR_UPDATE_TRIGGER, ())
        .await
        .map_err(db_error)?;
    txn.commit().await.map_err(db_error)?;
    Ok(report)
}

struct ObservationTarget {
    observation_id: String,
    original_len: u64,
}

async fn run_observation_pass(
    conn: &Connection,
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
    // An observation is released when the anchor it is bound to (via
    // observation_retrieval_anchors) is superseded/deleted. `observations`
    // carries no update trigger in the canonical schema, so a plain UPDATE
    // releases its payload.
    let sql = format!(
        "SELECT o.observation_id, LENGTH(o.observation_json) AS len
         FROM observations o
         JOIN observation_retrieval_anchors b ON b.observation_id = o.observation_id
         JOIN retrieval_anchors a ON a.anchor_id = b.anchor_id
         WHERE (?1 IS NULL OR a.projection_generation = ?1)
           AND json_extract(o.observation_json, '$.__retention_released') IS NULL
           AND {RELEASED_DISPOSITION}
         ORDER BY o.sequence ASC
         LIMIT ?3"
    );
    let mut rows = conn
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
        });
    }
    report.eligible = targets.len() as u64;
    if !mode.is_apply() {
        report.bytes_reclaimed = targets
            .iter()
            .map(|t| reclaimed_bytes(t.original_len, OBSERVATION_RELEASED_MARKER))
            .sum();
        return Ok(report);
    }

    let txn = conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .await
        .map_err(db_error)?;
    for target in &targets {
        match txn
            .execute(
                "UPDATE observations SET observation_json = ?2 WHERE observation_id = ?1",
                params![
                    target.observation_id.as_str(),
                    OBSERVATION_RELEASED_MARKER
                ],
            )
            .await
        {
            Ok(_) => {
                report.acted += 1;
                report.bytes_reclaimed = report.bytes_reclaimed.saturating_add(reclaimed_bytes(
                    target.original_len,
                    OBSERVATION_RELEASED_MARKER,
                ));
            }
            Err(err) => errors.push(format!(
                "release observation {}: {err}",
                target.observation_id
            )),
        }
    }
    txn.commit().await.map_err(db_error)?;
    Ok(report)
}

struct ProvenanceTarget {
    observation_id: String,
    original_len: u64,
}

async fn run_provenance_pass(
    conn: &Connection,
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
                LENGTH(p.availability_json) + LENGTH(COALESCE(p.capture_json, '')) AS len
         FROM observation_repository_provenance p
         JOIN retrieval_anchors a ON a.anchor_id = p.retrieval_anchor_id
         WHERE (?1 IS NULL OR a.projection_generation = ?1)
           AND p.retrieval_anchor_id IS NOT NULL
           AND json_extract(p.availability_json, '$.__retention_released') IS NULL
           AND {RELEASED_DISPOSITION}
         ORDER BY p.observation_id ASC
         LIMIT ?3"
    );
    let mut rows = conn
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
        });
    }
    report.eligible = targets.len() as u64;
    if !mode.is_apply() {
        report.bytes_reclaimed = targets
            .iter()
            .map(|t| reclaimed_bytes(t.original_len, PROVENANCE_RELEASED_MARKER))
            .sum();
        return Ok(report);
    }

    let txn = conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .await
        .map_err(db_error)?;
    txn.execute(DROP_PROVENANCE_UPDATE_TRIGGER, ())
        .await
        .map_err(db_error)?;
    for target in &targets {
        match txn
            .execute(
                "UPDATE observation_repository_provenance
                 SET availability_json = ?2, capture_json = ?2
                 WHERE observation_id = ?1",
                params![
                    target.observation_id.as_str(),
                    PROVENANCE_RELEASED_MARKER
                ],
            )
            .await
        {
            Ok(_) => {
                report.acted += 1;
                report.bytes_reclaimed = report.bytes_reclaimed.saturating_add(reclaimed_bytes(
                    target.original_len,
                    PROVENANCE_RELEASED_MARKER,
                ));
            }
            Err(err) => errors.push(format!(
                "release provenance {}: {err}",
                target.observation_id
            )),
        }
    }
    txn.execute(CREATE_PROVENANCE_UPDATE_TRIGGER, ())
        .await
        .map_err(db_error)?;
    txn.commit().await.map_err(db_error)?;
    Ok(report)
}

#[cfg(test)]
mod tests;
