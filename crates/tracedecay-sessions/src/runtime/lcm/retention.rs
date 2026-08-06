//! Projection-durability-aware session retention (plan 38 §3 and §4).
//!
//! The session store keeps the *same* conversation content in several places
//! at rest, forever:
//!
//! * `lcm_raw_messages` — the lossless raw ingest of every message.
//! * `session_messages` — the projected/queryable twin of each raw message,
//!   keyed by the same `(provider, message_id)`.
//! * FTS shadow tables over each (`lcm_raw_messages_fts`,
//!   `session_messages_fts`), maintained by triggers.
//!
//! One observed `sessions.db` reached 15 GB carrying both full copies plus their
//! FTS shadows. Plan 38 §4 ("one content copy") makes carrying both raw and
//! projected content indefinitely a defect: the projection must reference the
//! raw content or be superseded once durable. Plan 38 §3 ("session retention
//! policy") makes raw rows retained only until their LCM projection/summary
//! lineage is durable, then payload-offloaded or dropped under a configurable
//! window, with the projected twin obeying the same window.
//!
//! # Projection durability is the safety invariant
//!
//! A raw row is *projection-durable* when a summary node's lineage covers it —
//! i.e. its `store_id` appears as a `raw_message` source in
//! `lcm_summary_sources` (see
//! `global_db::session_temporal::operations::compatibility`, which persists
//! `LcmSourceRef::RawMessage { store_id }` as `('raw_message', store_id)`).
//! Only projection-durable rows are ever acted on. Rows with no summary lineage
//! are live, un-projected evidence and are **never** touched — this is the
//! plan's non-goal ("no lossy deletion of live, referenced evidence") expressed
//! directly in SQL.
//!
//! # One content copy (§4) — supersede-after-durability via content addressing
//!
//! This module supersedes the redundant copies rather than duplicating a fourth
//! storage scheme, because the store already owns a content-addressed external
//! payload lifecycle (`lcm_external_payloads` keyed by a content-hash-derived
//! `payload_ref`, with a full reaping GC in [`super::gc`]). The three passes,
//! in reclaim order:
//!
//! 1. **Drop** (terminal, longest window): projection-durable raw rows past
//!    `drop_after_days` are deleted along with their projected twin. Any now
//!    unreferenced external payload is reaped by the existing payload GC.
//! 2. **Offload** (recoverable, medium window): projection-durable *inline* raw
//!    rows past `offload_after_days` have their bulky `content` externalized to
//!    the content-addressed store (deduplicated by hash) and replaced with a
//!    recoverable placeholder, reclaiming the inline column and its FTS shadow.
//! 3. **Projected dedupe** (shortest window): a projected `session_messages`
//!    row is eligible only when its raw twin is still present and that raw row
//!    has durable summary lineage. It is then pure duplication of the retained
//!    raw copy, so it is dropped — the raw row is the single content copy and
//!    the projected form is reconstructable from it.
//!
//! Every pass is bounded (`max_batch_size`) and incremental so the daemon can
//! schedule it off the hot path without competing with foreground writes, and a
//! dry run counts what would be reclaimed without mutating anything.

use std::path::Path;

use serde::{Deserialize, Serialize};
use tracedecay_application::storage::{
    RetentionBacklogRecordV1, StorageByteSizeV1, StoreKeyV1, TableNameV1,
};
use tracedecay_domain::UtcMicros;

use tracedecay_runtime_core::db::engine::{
    Connection, Executor, Params, QueryExecutor, Transaction, TransactionBehavior, params,
};

use super::payload::{ExternalPayloadWrite, PayloadFileRollback};
use super::{LcmError, payload, schema, util};

const SECONDS_PER_DAY: i64 = 24 * 60 * 60;

/// SQL predicate (over an aliased `lcm_raw_messages` row `r`) that is true when
/// the raw row's `store_id` is covered by a durable summary node's lineage.
/// `source_id` for a `raw_message` source is the `store_id` rendered as text.
const PROJECTION_DURABLE: &str = "EXISTS (
        SELECT 1 FROM lcm_summary_sources s
        WHERE s.source_kind = 'raw_message'
          AND s.source_id = CAST(r.store_id AS TEXT)
    )";

/// Externalization kind recorded on retention-offloaded payloads.
const OFFLOAD_KIND: &str = "retention_offload";

/// Per-table/per-store retention windows for the session store. Defaults keep
/// a six-month recovery horizon for projection-durable raw evidence, offload
/// its bulky inline payload after 30 days, and remove the redundant projected
/// copy after 30 days. Rows without durable summary lineage remain untouched.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct LcmRetentionConfig {
    /// Master switch. When `false`, [`run_session_retention_authorized`] is a
    /// no-op even in [`RetentionMode::Apply`].
    #[serde(default = "default_retention_enabled")]
    pub enabled: bool,
    /// Window after which a projection-durable, still-inline raw row has its
    /// content offloaded to the content-addressed store. `None` disables the
    /// offload pass.
    #[serde(default = "default_offload_after_days")]
    pub offload_after_days: Option<u32>,
    /// Window after which a projection-durable raw row (and its projected twin)
    /// is dropped. `None` disables the drop pass.
    #[serde(default = "default_drop_after_days")]
    pub drop_after_days: Option<u32>,
    /// Window after which a projected `session_messages` row whose raw twin is
    /// still present is dropped as a redundant second content copy. `None`
    /// disables the dedupe pass.
    #[serde(default = "default_dedupe_projected_after_days")]
    pub dedupe_projected_after_days: Option<u32>,
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
fn default_offload_after_days() -> Option<u32> {
    Some(30)
}

#[allow(clippy::unnecessary_wraps)]
fn default_drop_after_days() -> Option<u32> {
    Some(180)
}

#[allow(clippy::unnecessary_wraps)]
fn default_dedupe_projected_after_days() -> Option<u32> {
    Some(30)
}

impl Default for LcmRetentionConfig {
    fn default() -> Self {
        Self {
            enabled: default_retention_enabled(),
            offload_after_days: default_offload_after_days(),
            drop_after_days: default_drop_after_days(),
            dedupe_projected_after_days: default_dedupe_projected_after_days(),
            max_batch_size: default_max_batch_size(),
        }
    }
}

impl LcmRetentionConfig {
    fn batch_limit(&self) -> i64 {
        i64::try_from(self.max_batch_size.max(1)).unwrap_or(i64::MAX)
    }

    /// Whether any pass has a window configured. When false, an enabled pass
    /// still reports zero work rather than scanning.
    fn any_window(&self) -> bool {
        self.offload_after_days.is_some()
            || self.drop_after_days.is_some()
            || self.dedupe_projected_after_days.is_some()
    }
}

/// Whether a retention pass mutates the database.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RetentionMode {
    /// Count what would be reclaimed without deleting or offloading anything.
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
pub struct LcmRetentionPhaseReport {
    /// Configured window in days (`None` when the pass is disabled).
    pub window_days: Option<u32>,
    /// Rows matching the pass predicate within the batch cap (candidates).
    pub eligible: u64,
    /// Rows actually acted on (`0` in a dry run).
    pub acted: u64,
    /// Bytes of message content reclaimed from the database by this pass.
    pub bytes_reclaimed: u64,
    /// Oldest timestamp among the bounded eligible rows, when any.
    #[serde(default)]
    pub oldest_eligible_at: Option<i64>,
}

impl LcmRetentionPhaseReport {
    fn disabled() -> Self {
        Self::default()
    }
}

/// Aggregate report for a retention run, including measurable reclaim
/// (row and page/freelist counts before and after).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LcmRetentionReport {
    pub provider: String,
    pub session_id: Option<String>,
    pub applied: bool,
    pub started_at: i64,
    pub ended_at: i64,
    pub dropped: LcmRetentionPhaseReport,
    pub offloaded: LcmRetentionPhaseReport,
    pub projected_deduped: LcmRetentionPhaseReport,
    /// `lcm_raw_messages` row count before/after the run.
    pub raw_rows_before: u64,
    pub raw_rows_after: u64,
    /// `session_messages` row count before/after the run.
    pub projected_rows_before: u64,
    pub projected_rows_after: u64,
    /// Database `PRAGMA freelist_count` before/after (freed pages are the
    /// measurable, VACUUM-free signal that space was reclaimed).
    pub freelist_before: u64,
    pub freelist_after: u64,
    /// Database `PRAGMA page_count` before/after.
    pub page_count_before: u64,
    pub page_count_after: u64,
    pub errors: Vec<String>,
}

impl LcmRetentionReport {
    /// Total content bytes reclaimed across every pass.
    pub fn bytes_reclaimed(&self) -> u64 {
        self.dropped
            .bytes_reclaimed
            .saturating_add(self.offloaded.bytes_reclaimed)
            .saturating_add(self.projected_deduped.bytes_reclaimed)
    }
}

fn cutoff_secs(window_days: u32, now_secs: i64) -> i64 {
    now_secs.saturating_sub(i64::from(window_days).saturating_mul(SECONDS_PER_DAY))
}

/// Observe retention-eligible session bytes without mutating the store.
///
/// The raw-row query unions the configured drop and offload predicates so a
/// row eligible for both policies is counted once. The projected-row query
/// mirrors the dedupe safety predicate. A configured window emits a zero-byte
/// record when clean, allowing Doctor to distinguish complete clean coverage
/// from an unwired source.
pub async fn read_session_retention_backlog(
    conn: &(impl QueryExecutor + ?Sized),
    store: StoreKeyV1,
    config: &LcmRetentionConfig,
    now: i64,
) -> Result<Vec<RetentionBacklogRecordV1>, LcmError> {
    if !config.enabled {
        return Ok(Vec::new());
    }

    let mut records = Vec::new();
    let drop_cutoff = config.drop_after_days.map(|days| cutoff_secs(days, now));
    let offload_cutoff = config.offload_after_days.map(|days| cutoff_secs(days, now));
    if drop_cutoff.is_some() || offload_cutoff.is_some() {
        let raw_watermark = drop_cutoff
            .into_iter()
            .chain(offload_cutoff)
            .max()
            .unwrap_or(now);
        let sql = format!(
            "SELECT MIN(r.timestamp),
                    COALESCE(SUM(LENGTH(COALESCE(r.content, ''))), 0)
             FROM lcm_raw_messages r
             WHERE r.timestamp IS NOT NULL
               AND (
                    (?1 = 1 AND r.timestamp < ?2 AND {PROJECTION_DURABLE})
                 OR (?3 = 1 AND r.timestamp < ?4
                     AND r.storage_kind = 'inline'
                     AND r.content IS NOT NULL
                     AND LENGTH(r.content) > 0
                     AND {PROJECTION_DURABLE})
               )"
        );
        let mut rows = conn
            .query(
                &sql,
                params![
                    i64::from(drop_cutoff.is_some()),
                    drop_cutoff.unwrap_or(0),
                    i64::from(offload_cutoff.is_some()),
                    offload_cutoff.unwrap_or(0)
                ],
            )
            .await?;
        let row = rows.next().await?.ok_or_else(|| {
            LcmError::Db("retention backlog raw aggregate returned no row".to_string())
        })?;
        let oldest = row.get::<Option<i64>>(0)?.unwrap_or(raw_watermark);
        let bytes = row.get::<i64>(1)?.max(0) as u64;
        records.push(RetentionBacklogRecordV1 {
            store: store.clone(),
            table: TableNameV1::new("lcm_raw_messages")
                .map_err(|error| LcmError::Db(error.to_string()))?,
            past_window_bytes: StorageByteSizeV1(bytes),
            oldest_past_window_at: UtcMicros(oldest.saturating_mul(1_000_000)),
            window_watermark_at: UtcMicros(raw_watermark.saturating_mul(1_000_000)),
        });
    }

    if let Some(days) = config.dedupe_projected_after_days {
        let watermark = cutoff_secs(days, now);
        let sql = format!(
            "SELECT MIN(sm.timestamp),
                    COALESCE(SUM(LENGTH(COALESCE(sm.text, ''))), 0)
             FROM session_messages sm
             WHERE sm.timestamp IS NOT NULL
               AND sm.timestamp < ?1
               AND EXISTS (
                   SELECT 1 FROM lcm_raw_messages r
                   WHERE r.provider = sm.provider
                     AND r.message_id = sm.message_id
                     AND {PROJECTION_DURABLE}
               )"
        );
        let mut rows = conn.query(&sql, params![watermark]).await?;
        let row = rows.next().await?.ok_or_else(|| {
            LcmError::Db("retention backlog projection aggregate returned no row".to_string())
        })?;
        let oldest = row.get::<Option<i64>>(0)?.unwrap_or(watermark);
        let bytes = row.get::<i64>(1)?.max(0) as u64;
        records.push(RetentionBacklogRecordV1 {
            store,
            table: TableNameV1::new("session_messages")
                .map_err(|error| LcmError::Db(error.to_string()))?,
            past_window_bytes: StorageByteSizeV1(bytes),
            oldest_past_window_at: UtcMicros(oldest.saturating_mul(1_000_000)),
            window_watermark_at: UtcMicros(watermark.saturating_mul(1_000_000)),
        });
    }

    Ok(records)
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

async fn scoped_row_count(
    conn: &(impl QueryExecutor + ?Sized),
    table: &str,
    provider: &str,
    session_id: Option<&str>,
) -> u64 {
    let sql = format!(
        "SELECT COUNT(*) FROM {table}
         WHERE (?1 = 'all' OR provider = ?1)
           AND (?2 IS NULL OR session_id = ?2)"
    );
    util::fetch_i64(
        conn,
        &sql,
        params![provider, util::opt_text(session_id)],
        "count",
    )
    .await
    .unwrap_or(0)
    .max(0) as u64
}

/// Runs the configured session-retention passes for `provider`/`session_id`.
///
/// `provider` may be `"all"` to span every provider; `session_id` narrows to a
/// single session. In [`RetentionMode::DryRun`] nothing is mutated and each
/// phase reports the candidate count and bytes that *would* be reclaimed.
/// Apply mode requires `authorize` to admit each mutating intent.
#[allow(clippy::too_many_arguments)]
pub async fn run_session_retention_authorized(
    conn: &Connection,
    storage_root: &Path,
    provider: &str,
    session_id: Option<&str>,
    config: &LcmRetentionConfig,
    mode: RetentionMode,
    now: i64,
    authorize: &(dyn Fn(&str) -> Result<(), LcmError> + Send + Sync),
) -> Result<LcmRetentionReport, LcmError> {
    let raw_rows_before = scoped_row_count(conn, "lcm_raw_messages", provider, session_id).await;
    let projected_rows_before =
        scoped_row_count(conn, "session_messages", provider, session_id).await;
    let freelist_before = pragma_u64(conn, "freelist_count").await;
    let page_count_before = pragma_u64(conn, "page_count").await;

    let mut report = LcmRetentionReport {
        provider: provider.to_string(),
        session_id: session_id.map(str::to_string),
        applied: mode.is_apply(),
        started_at: now,
        ended_at: now,
        dropped: LcmRetentionPhaseReport::disabled(),
        offloaded: LcmRetentionPhaseReport::disabled(),
        projected_deduped: LcmRetentionPhaseReport::disabled(),
        raw_rows_before,
        raw_rows_after: raw_rows_before,
        projected_rows_before,
        projected_rows_after: projected_rows_before,
        freelist_before,
        freelist_after: freelist_before,
        page_count_before,
        page_count_after: page_count_before,
        errors: Vec::new(),
    };

    if !config.enabled || !config.any_window() {
        report.dropped.window_days = config.drop_after_days;
        report.offloaded.window_days = config.offload_after_days;
        report.projected_deduped.window_days = config.dedupe_projected_after_days;
        return Ok(report);
    }

    // Drop first (terminal, longest window) so offload never externalizes a row
    // that is about to be deleted.
    report.dropped = run_drop_pass(
        conn,
        provider,
        session_id,
        config,
        mode,
        now,
        &mut report.errors,
        authorize,
    )
    .await?;
    report.offloaded = run_offload_pass(
        conn,
        storage_root,
        provider,
        session_id,
        config,
        mode,
        now,
        &mut report.errors,
        authorize,
    )
    .await?;
    report.projected_deduped = run_dedupe_pass(
        conn,
        provider,
        session_id,
        config,
        mode,
        now,
        &mut report.errors,
        authorize,
    )
    .await?;

    if mode.is_apply() {
        // Consume the staged GC/reporting meta cards: record the last run so a
        // scheduler and Doctor can report retention backlog without a rescan.
        let acted = report
            .dropped
            .acted
            .saturating_add(report.offloaded.acted)
            .saturating_add(report.projected_deduped.acted);
        authorize("begin session retention metadata")?;
        let transaction = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .await?;
        write_retention_metadata(&transaction, now, acted, report.bytes_reclaimed()).await?;
        commit_authorized(transaction, authorize, "commit session retention metadata").await?;
    }

    report.ended_at = now;
    report.raw_rows_after = scoped_row_count(conn, "lcm_raw_messages", provider, session_id).await;
    report.projected_rows_after =
        scoped_row_count(conn, "session_messages", provider, session_id).await;
    report.freelist_after = pragma_u64(conn, "freelist_count").await;
    report.page_count_after = pragma_u64(conn, "page_count").await;
    Ok(report)
}

async fn write_retention_metadata(
    executor: &(impl Executor + ?Sized),
    now: i64,
    acted: u64,
    bytes_reclaimed: u64,
) -> Result<(), LcmError> {
    for (key, value) in [
        ("last_retention_at", now.to_string()),
        ("last_retention_rows", acted.to_string()),
        ("last_retention_bytes", bytes_reclaimed.to_string()),
    ] {
        schema::set_gc_meta(executor, key, &value).await?;
    }
    Ok(())
}

async fn commit_authorized(
    transaction: Transaction,
    authorize: &(dyn Fn(&str) -> Result<(), LcmError> + Send + Sync),
    intent: &str,
) -> Result<(), LcmError> {
    if let Err(error) = authorize(intent) {
        return match transaction.rollback().await {
            Ok(()) => Err(error),
            Err(rollback_error) => Err(LcmError::Db(format!(
                "{error}; rollback after authority loss failed: {rollback_error}"
            ))),
        };
    }
    transaction.commit().await.map_err(Into::into)
}

enum RetentionQueryExecutor<'a> {
    Connection(&'a Connection),
    Transaction(&'a Transaction),
}

impl RetentionQueryExecutor<'_> {
    async fn query(
        &self,
        sql: &str,
        params: Params,
    ) -> tracedecay_runtime_core::db::engine::Result<tracedecay_runtime_core::db::engine::Rows>
    {
        match self {
            Self::Connection(connection) => connection.query(sql, params).await,
            Self::Transaction(transaction) => transaction.query(sql, params).await,
        }
    }
}

struct DropRow {
    store_id: i64,
    provider: String,
    message_id: String,
    timestamp: i64,
    content_len: u64,
}

#[allow(clippy::too_many_arguments)]
async fn run_drop_pass(
    conn: &Connection,
    provider: &str,
    session_id: Option<&str>,
    config: &LcmRetentionConfig,
    mode: RetentionMode,
    now: i64,
    errors: &mut Vec<String>,
    authorize: &(dyn Fn(&str) -> Result<(), LcmError> + Send + Sync),
) -> Result<LcmRetentionPhaseReport, LcmError> {
    let mut report = LcmRetentionPhaseReport {
        window_days: config.drop_after_days,
        ..LcmRetentionPhaseReport::default()
    };
    let Some(window) = config.drop_after_days else {
        return Ok(report);
    };
    let cutoff = cutoff_secs(window, now);
    let sql = format!(
        "SELECT r.store_id, r.provider, r.message_id, r.timestamp,
                LENGTH(COALESCE(r.content, '')) AS content_len
         FROM lcm_raw_messages r
         WHERE (?1 = 'all' OR r.provider = ?1)
           AND (?2 IS NULL OR r.session_id = ?2)
           AND r.timestamp IS NOT NULL AND r.timestamp < ?3
           AND {PROJECTION_DURABLE}
         ORDER BY r.timestamp ASC, r.store_id ASC
         LIMIT ?4"
    );
    let transaction = if mode.is_apply() {
        authorize("begin session retention drop pass")?;
        Some(
            conn.transaction_with_behavior(TransactionBehavior::Immediate)
                .await?,
        )
    } else {
        None
    };
    let query_executor = transaction.as_ref().map_or(
        RetentionQueryExecutor::Connection(conn),
        RetentionQueryExecutor::Transaction,
    );
    let mut rows = query_executor
        .query(
            &sql,
            params![
                provider,
                util::opt_text(session_id),
                cutoff,
                config.batch_limit()
            ],
        )
        .await?;
    let mut targets = Vec::new();
    while let Some(row) = rows.next().await? {
        targets.push(DropRow {
            store_id: row.get(0)?,
            provider: row.get(1)?,
            message_id: row.get(2)?,
            timestamp: row.get(3)?,
            content_len: row.get::<i64>(4)?.max(0) as u64,
        });
    }
    report.eligible = targets.len() as u64;
    report.oldest_eligible_at = targets.iter().map(|target| target.timestamp).min();
    if !mode.is_apply() {
        report.bytes_reclaimed = targets.iter().map(|t| t.content_len).sum();
        return Ok(report);
    }

    let txn = transaction.ok_or_else(|| {
        LcmError::Db("apply mode did not start a session retention drop transaction".to_owned())
    })?;
    for target in &targets {
        authorize("drop session retention row")?;
        // Drop the projected twin first (its FTS delete trigger fires), then
        // the raw row (its FTS delete trigger fires). Any external payload the
        // raw row referenced becomes unreferenced and is reaped by payload GC.
        if let Err(err) = txn
            .execute(
                "DELETE FROM session_messages WHERE provider = ?1 AND message_id = ?2",
                params![target.provider.as_str(), target.message_id.as_str()],
            )
            .await
        {
            errors.push(format!("drop projected twin {}: {err}", target.message_id));
            continue;
        }
        match txn
            .execute(
                "DELETE FROM lcm_raw_messages WHERE store_id = ?1",
                params![target.store_id],
            )
            .await
        {
            Ok(1) => {
                report.acted += 1;
                report.bytes_reclaimed = report.bytes_reclaimed.saturating_add(target.content_len);
            }
            Ok(changed) => errors.push(format!(
                "drop raw row {} changed {changed} rows",
                target.store_id
            )),
            Err(err) => errors.push(format!("drop raw row {}: {err}", target.store_id)),
        }
    }
    commit_authorized(txn, authorize, "commit session retention drop pass").await?;
    Ok(report)
}

struct OffloadRow {
    store_id: i64,
    provider: String,
    session_id: String,
    message_id: String,
    timestamp: i64,
    content: String,
}

#[allow(clippy::too_many_arguments)]
async fn run_offload_pass(
    conn: &Connection,
    storage_root: &Path,
    provider: &str,
    session_id: Option<&str>,
    config: &LcmRetentionConfig,
    mode: RetentionMode,
    now: i64,
    errors: &mut Vec<String>,
    authorize: &(dyn Fn(&str) -> Result<(), LcmError> + Send + Sync),
) -> Result<LcmRetentionPhaseReport, LcmError> {
    let mut report = LcmRetentionPhaseReport {
        window_days: config.offload_after_days,
        ..LcmRetentionPhaseReport::default()
    };
    let Some(window) = config.offload_after_days else {
        return Ok(report);
    };
    let cutoff = cutoff_secs(window, now);
    let sql = format!(
        "SELECT r.store_id, r.provider, r.session_id, r.message_id, r.timestamp, r.content
         FROM lcm_raw_messages r
         WHERE (?1 = 'all' OR r.provider = ?1)
           AND (?2 IS NULL OR r.session_id = ?2)
           AND r.timestamp IS NOT NULL AND r.timestamp < ?3
           AND r.storage_kind = 'inline'
           AND r.content IS NOT NULL AND LENGTH(r.content) > 0
           AND {PROJECTION_DURABLE}
         ORDER BY r.timestamp ASC, r.store_id ASC
         LIMIT ?4"
    );
    let mut rows = conn
        .query(
            &sql,
            params![
                provider,
                util::opt_text(session_id),
                cutoff,
                config.batch_limit()
            ],
        )
        .await?;
    let mut targets = Vec::new();
    while let Some(row) = rows.next().await? {
        let content: Option<String> = row.get(5)?;
        let Some(content) = content else { continue };
        targets.push(OffloadRow {
            store_id: row.get(0)?,
            provider: row.get(1)?,
            session_id: row.get(2)?,
            message_id: row.get(3)?,
            timestamp: row.get(4)?,
            content,
        });
    }
    report.eligible = targets.len() as u64;
    report.oldest_eligible_at = targets.iter().map(|target| target.timestamp).min();
    if !mode.is_apply() {
        report.bytes_reclaimed = targets.iter().map(|t| t.content.len() as u64).sum();
        return Ok(report);
    }

    // Each row is offloaded atomically: write the content-addressed file, then
    // flip the row to external + placeholder in its own transaction. A crash
    // between file write and commit is cleaned up by the rollback guard.
    for target in targets {
        match offload_one(conn, storage_root, &target, authorize).await {
            Ok(bytes) => {
                report.acted += 1;
                report.bytes_reclaimed = report.bytes_reclaimed.saturating_add(bytes);
            }
            Err(err) => errors.push(format!("offload raw row {}: {err}", target.store_id)),
        }
    }
    Ok(report)
}

async fn offload_one(
    conn: &Connection,
    storage_root: &Path,
    target: &OffloadRow,
    authorize: &(dyn Fn(&str) -> Result<(), LcmError> + Send + Sync),
) -> Result<u64, LcmError> {
    let byte_len = target.content.len() as u64;
    authorize("begin session retention offload payload write")?;
    let mut rollback = PayloadFileRollback::begin_cancellation_safe(storage_root);
    let payload_ref = payload::write_external_payload_tracked(
        storage_root,
        ExternalPayloadWrite {
            provider: &target.provider,
            session_id: &target.session_id,
            message_id: &target.message_id,
            kind: OFFLOAD_KIND,
            content: &target.content,
            metadata_json: None,
        },
        &mut rollback,
    )?;

    // Placeholder mirrors the ingest externalization format so the payload GC's
    // reference scan (`is_external_payload_placeholder` + `ref=`) keeps the
    // payload alive while the raw row references it.
    let placeholder = format!(
        "[Externalized LCM ingest payload: kind={}; field=content; chars={}; bytes={}; ref={}]",
        payload_ref.kind, payload_ref.char_count, payload_ref.byte_count, payload_ref.payload_ref
    );

    authorize("begin session retention offload pass")?;
    let txn = conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .await?;
    authorize("upsert session retention offload metadata")?;
    payload::upsert_payload_metadata(&txn, &payload_ref).await?;
    authorize("compare and swap session retention offload row")?;
    let update_sql = format!(
        "UPDATE lcm_raw_messages AS r
         SET content = NULL,
             content_hash = ?2,
             storage_kind = 'external',
             payload_ref = ?3,
             snippet_text = ?4,
             index_text = ?4
         WHERE r.store_id = ?1
           AND r.provider = ?5
           AND r.session_id = ?6
           AND r.message_id = ?7
           AND r.timestamp = ?8
           AND r.content = ?9
           AND r.storage_kind = 'inline'
           AND r.payload_ref IS NULL
           AND {PROJECTION_DURABLE}"
    );
    let changed = txn
        .execute(
            &update_sql,
            params![
                target.store_id,
                payload_ref.content_hash.as_str(),
                payload_ref.payload_ref.as_str(),
                placeholder.as_str(),
                target.provider.as_str(),
                target.session_id.as_str(),
                target.message_id.as_str(),
                target.timestamp,
                target.content.as_str()
            ],
        )
        .await?;
    if changed != 1 {
        txn.rollback().await?;
        return Err(LcmError::Db(format!(
            "offload compare-and-swap rejected changed row {}",
            target.store_id
        )));
    }
    commit_authorized(txn, authorize, "commit session retention offload pass").await?;
    rollback.disarm();
    Ok(byte_len)
}

#[allow(clippy::too_many_arguments)]
async fn run_dedupe_pass(
    conn: &Connection,
    provider: &str,
    session_id: Option<&str>,
    config: &LcmRetentionConfig,
    mode: RetentionMode,
    now: i64,
    errors: &mut Vec<String>,
    authorize: &(dyn Fn(&str) -> Result<(), LcmError> + Send + Sync),
) -> Result<LcmRetentionPhaseReport, LcmError> {
    let mut report = LcmRetentionPhaseReport {
        window_days: config.dedupe_projected_after_days,
        ..LcmRetentionPhaseReport::default()
    };
    let Some(window) = config.dedupe_projected_after_days else {
        return Ok(report);
    };
    let cutoff = cutoff_secs(window, now);
    // Only dedupe a projected row whose raw twin is still present and covered
    // by durable summary lineage. A projected row without both proofs remains
    // immediately queryable and is never touched here.
    let sql = format!(
        "SELECT sm.provider, sm.message_id, sm.timestamp,
                LENGTH(COALESCE(sm.text, '')) AS text_len
         FROM session_messages sm
         WHERE (?1 = 'all' OR sm.provider = ?1)
           AND (?2 IS NULL OR sm.session_id = ?2)
           AND sm.timestamp IS NOT NULL AND sm.timestamp < ?3
           AND EXISTS (
               SELECT 1 FROM lcm_raw_messages r
               WHERE r.provider = sm.provider
                 AND r.message_id = sm.message_id
                 AND {PROJECTION_DURABLE}
           )
         ORDER BY sm.timestamp ASC, sm.message_id ASC
         LIMIT ?4"
    );
    let transaction = if mode.is_apply() {
        authorize("begin session retention dedupe pass")?;
        Some(
            conn.transaction_with_behavior(TransactionBehavior::Immediate)
                .await?,
        )
    } else {
        None
    };
    let query_executor = transaction.as_ref().map_or(
        RetentionQueryExecutor::Connection(conn),
        RetentionQueryExecutor::Transaction,
    );
    let mut rows = query_executor
        .query(
            &sql,
            params![
                provider,
                util::opt_text(session_id),
                cutoff,
                config.batch_limit()
            ],
        )
        .await?;
    let mut targets: Vec<(String, String, i64, u64)> = Vec::new();
    while let Some(row) = rows.next().await? {
        targets.push((
            row.get(0)?,
            row.get(1)?,
            row.get(2)?,
            row.get::<i64>(3)?.max(0) as u64,
        ));
    }
    report.eligible = targets.len() as u64;
    report.oldest_eligible_at = targets.iter().map(|(_, _, timestamp, _)| *timestamp).min();
    if !mode.is_apply() {
        report.bytes_reclaimed = targets.iter().map(|(_, _, _, len)| *len).sum();
        return Ok(report);
    }

    let txn = transaction.ok_or_else(|| {
        LcmError::Db("apply mode did not start a session retention dedupe transaction".to_owned())
    })?;
    let delete_sql = format!(
        "DELETE FROM session_messages
         WHERE provider = ?1
           AND message_id = ?2
           AND EXISTS (
               SELECT 1 FROM lcm_raw_messages r
               WHERE r.provider = session_messages.provider
                 AND r.message_id = session_messages.message_id
                 AND {PROJECTION_DURABLE}
           )"
    );
    for (provider_val, message_id, _, text_len) in &targets {
        authorize("dedupe session retention projected row")?;
        match txn
            .execute(
                &delete_sql,
                params![provider_val.as_str(), message_id.as_str()],
            )
            .await
        {
            Ok(1) => {
                report.acted += 1;
                report.bytes_reclaimed = report.bytes_reclaimed.saturating_add(*text_len);
            }
            Ok(changed) => errors.push(format!(
                "dedupe projected {message_id} changed {changed} rows"
            )),
            Err(err) => errors.push(format!("dedupe projected {message_id}: {err}")),
        }
    }
    commit_authorized(txn, authorize, "commit session retention dedupe pass").await?;
    Ok(report)
}

#[cfg(test)]
mod tests;
