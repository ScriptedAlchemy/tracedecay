//! Projection-durability-aware session retention (plan 38 §3 and §4).
//!
//! `lcm_raw_messages` is the one stored copy of each message body; session
//! message readers and the FTS index read that same row. Plan 38 §3
//! ("session retention policy") keeps raw rows only until their LCM
//! summary lineage is durable, then offloads or drops them under a
//! configurable window.
//!
//! # Projection durability is the safety invariant
//!
//! A raw row is *projection-durable* when a summary node's lineage covers it,
//! i.e. its `store_id` appears as a `raw_message` source in
//! `session_summary_sources` (see
//! `tracedecay_session_temporal_store::operations::publication`, which persists
//! `LcmSourceRef::RawMessage { store_id }` as `('raw_message', store_id)`).
//! Only projection-durable rows are ever acted on. Rows with no summary lineage
//! are live, un-projected evidence and are **never** touched, this is the
//! plan's non-goal ("no lossy deletion of live, referenced evidence") expressed
//! directly in SQL.
//!
//! # Passes
//!
//! Both passes reuse the store's content-addressed external payload lifecycle
//! (`lcm_external_payloads` keyed by a content-hash-derived `payload_ref`,
//! with a full reaping GC in [`super::gc`]). In reclaim order:
//!
//! 1. **Drop** (terminal, longest window): projection-durable raw rows past
//!    `drop_after_days` are deleted. Any now unreferenced external payload is
//!    reaped by the existing payload GC.
//! 2. **Offload** (recoverable, shorter window): projection-durable *inline*
//!    raw rows past `offload_after_days` have their bulky `content`
//!    externalized to the content-addressed store (deduplicated by hash) and
//!    replaced with a recoverable placeholder, reclaiming the inline column and
//!    its FTS shadow.
//!
//! Every pass is bounded (`max_batch_size`) and incremental so the daemon can
//! schedule it off the hot path without competing with foreground writes, and a
//! dry run counts what would be reclaimed without mutating anything.

use std::collections::BTreeSet;
use std::path::Path;

use serde::{Deserialize, Serialize};
use tracedecay_contracts::storage::{
    RetentionBacklogRecordV1, StorageByteSizeV1, StoreKeyV1, TableNameV1,
};
use tracedecay_domain::UtcMicros;

#[cfg(test)]
use tracedecay_runtime_core::db::engine::{Connection, Transaction, TransactionBehavior};
use tracedecay_runtime_core::db::engine::{Executor, QueryExecutor, Row, params};
use tracedecay_runtime_core::db::{
    Database, DatabaseEngineReadConnection, DatabaseMemoryTransaction,
};

use super::payload::{ExternalPayloadWrite, PayloadFileRollback};
use super::{LcmError, payload, schema, util};

const SECONDS_PER_DAY: i64 = 24 * 60 * 60;

/// SQL predicate (over an aliased `lcm_raw_messages` row `r`) that is true when
/// the raw row's `store_id` is covered by a durable summary node's lineage.
/// `source_id` for a `raw_message` source is the `store_id` rendered as text.
const PROJECTION_DURABLE: &str = "EXISTS (
        SELECT 1 FROM session_summary_sources s
        WHERE s.source_kind = 'raw_message'
          AND s.source_id = CAST(r.store_id AS TEXT)
    )";

/// SQL predicate (over `r`) that reaches the raw rows a durable summary covers
/// through the summary lineage index. Retention candidates must be durable, so
/// driving the scan from the lineage instead of every row past the window keeps
/// rows no summary covers yet, whose content retention would otherwise read on
/// every pass, out of the scan. The unary `+` keeps the window bound from
/// displacing the rowid lookups.
const DURABLE_ROW_IDS: &str = "r.store_id IN (
        SELECT CAST(s.source_id AS INTEGER) FROM session_summary_sources s
        WHERE s.source_kind = 'raw_message'
    )";

/// Externalization kind recorded on retention-offloaded payloads.
const OFFLOAD_KIND: &str = "retention_offload";

/// Per-table/per-store retention windows for the session store. Defaults keep
/// a six-month recovery horizon for projection-durable raw evidence and
/// offload its bulky inline payload after 30 days. Rows without durable
/// summary lineage remain untouched.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct LcmRetentionConfig {
    /// Master switch. When `false`, [`run_session_retention`] is a
    /// no-op even in [`RetentionMode::Apply`].
    #[serde(default = "default_retention_enabled")]
    pub enabled: bool,
    /// Window after which a projection-durable, still-inline raw row has its
    /// content offloaded to the content-addressed store. `None` disables the
    /// offload pass.
    #[serde(default = "default_offload_after_days")]
    pub offload_after_days: Option<u32>,
    /// Window after which a projection-durable raw row is dropped. `None`
    /// disables the drop pass.
    #[serde(default = "default_drop_after_days")]
    pub drop_after_days: Option<u32>,
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

impl Default for LcmRetentionConfig {
    fn default() -> Self {
        Self {
            enabled: default_retention_enabled(),
            offload_after_days: default_offload_after_days(),
            drop_after_days: default_drop_after_days(),
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
        self.offload_after_days.is_some() || self.drop_after_days.is_some()
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
/// (page/freelist counts before and after).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LcmRetentionReport {
    pub provider: String,
    pub session_id: Option<String>,
    pub applied: bool,
    pub started_at: i64,
    pub ended_at: i64,
    pub dropped: LcmRetentionPhaseReport,
    pub offloaded: LcmRetentionPhaseReport,
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
    }
}

fn cutoff_secs(window_days: u32, now_secs: i64) -> i64 {
    now_secs.saturating_sub(i64::from(window_days).saturating_mul(SECONDS_PER_DAY))
}

/// Observe retention-eligible session bytes without mutating the store.
///
/// The raw-row query unions the configured drop and offload predicates so a
/// row eligible for both policies is counted once. A configured window emits a zero-byte
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
             WHERE {DURABLE_ROW_IDS}
               AND r.timestamp IS NOT NULL
               AND (
                    (?1 = 1 AND +r.timestamp < ?2)
                 OR (?3 = 1 AND +r.timestamp < ?4
                     AND r.storage_kind = 'inline'
                     AND r.content IS NOT NULL
                     AND LENGTH(r.content) > 0)
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

/// Runs the configured session-retention passes for `provider`/`session_id`.
///
/// `provider` may be `"all"` to span every provider; `session_id` narrows to a
/// single session. In [`RetentionMode::DryRun`] nothing is mutated and each
/// phase reports the candidate count and bytes that *would* be reclaimed.
/// Apply mode obtains every writer through the supplied database's guarded
/// transaction capability, so revocation is checked at transaction admission
/// and commit without exposing a raw database authority.
#[allow(clippy::too_many_arguments)]
pub async fn run_session_retention(
    database: &Database,
    storage_root: &Path,
    provider: &str,
    session_id: Option<&str>,
    config: &LcmRetentionConfig,
    mode: RetentionMode,
    now: i64,
) -> Result<LcmRetentionReport, LcmError> {
    run_session_retention_inner(
        RetentionStore::Database(database),
        storage_root,
        provider,
        session_id,
        config,
        mode,
        now,
        None,
    )
    .await
}

/// Standalone engine fixtures retain the historical raw-connection seam only
/// in unit tests. Production retention always enters through
/// [`run_session_retention`] and therefore cannot receive a raw connection or
/// authority object.
#[cfg(test)]
#[allow(clippy::too_many_arguments)]
pub async fn run_session_retention_authorized(
    conn: &Connection,
    storage_root: &Path,
    provider: &str,
    session_id: Option<&str>,
    config: &LcmRetentionConfig,
    mode: RetentionMode,
    now: i64,
    authorize: &RetentionAuthorization<'_>,
) -> Result<LcmRetentionReport, LcmError> {
    run_session_retention_inner(
        RetentionStore::Connection(conn),
        storage_root,
        provider,
        session_id,
        config,
        mode,
        now,
        Some(authorize),
    )
    .await
}

type RetentionAuthorization<'a> = dyn Fn(&str) -> Result<(), LcmError> + Send + Sync + 'a;

#[allow(clippy::too_many_arguments)]
#[tracing::instrument(name = "sessions.lcm.retention", level = "trace", skip_all)]
async fn run_session_retention_inner(
    store: RetentionStore<'_>,
    storage_root: &Path,
    provider: &str,
    session_id: Option<&str>,
    config: &LcmRetentionConfig,
    mode: RetentionMode,
    now: i64,
    authorize: Option<&RetentionAuthorization<'_>>,
) -> Result<LcmRetentionReport, LcmError> {
    let read = store.read_connection();
    let freelist_before = pragma_u64(&read, "freelist_count").await;
    let page_count_before = pragma_u64(&read, "page_count").await;

    let mut report = LcmRetentionReport {
        provider: provider.to_string(),
        session_id: session_id.map(str::to_string),
        applied: mode.is_apply(),
        started_at: now,
        ended_at: now,
        dropped: LcmRetentionPhaseReport::disabled(),
        offloaded: LcmRetentionPhaseReport::disabled(),
        freelist_before,
        freelist_after: freelist_before,
        page_count_before,
        page_count_after: page_count_before,
        errors: Vec::new(),
    };

    if !config.enabled || !config.any_window() {
        report.dropped.window_days = config.drop_after_days;
        report.offloaded.window_days = config.offload_after_days;
        return Ok(report);
    }

    let scope = RetentionScope {
        provider,
        session_id,
    };
    // Drop first (terminal, longest window) so offload never externalizes a row
    // that is about to be deleted.
    let (dropped, drop_cursor) = run_drop_pass(
        store,
        scope,
        config,
        mode,
        now,
        &mut report.errors,
        authorize,
    )
    .await?;
    report.dropped = dropped;
    let (offloaded, offload_cursor) = run_offload_pass(
        store,
        storage_root,
        scope,
        config,
        mode,
        now,
        &mut report.errors,
        authorize,
    )
    .await?;
    report.offloaded = offloaded;
    if mode.is_apply() {
        // Consume the staged GC/reporting meta cards: record the last run so a
        // scheduler and Doctor can report retention backlog without a rescan.
        let acted = report.dropped.acted.saturating_add(report.offloaded.acted);
        let transaction = store
            .begin_memory_write_transaction("begin session retention metadata", authorize)
            .await?;
        write_retention_metadata(&transaction, now, acted, report.bytes_reclaimed()).await?;
        // A scoped run examined only its own rows, so only an unscoped run may
        // move the cursors past what it examined.
        if scope.is_unscoped() {
            for (pass, cursor) in [("drop", drop_cursor), ("offload", offload_cursor)] {
                if let Some(cursor) = cursor {
                    cursor.persist(&transaction, pass).await?;
                }
            }
        }
        commit_authorized(transaction, authorize, "commit session retention metadata").await?;
    }

    report.ended_at = now;
    let read = store.read_connection();
    report.freelist_after = pragma_u64(&read, "freelist_count").await;
    report.page_count_after = pragma_u64(&read, "page_count").await;
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
    transaction: RetentionWriteTransaction<'_>,
    authorize: Option<&RetentionAuthorization<'_>>,
    intent: &str,
) -> Result<(), LcmError> {
    if let Some(authorize) = authorize
        && let Err(error) = authorize(intent)
    {
        return match transaction.rollback().await {
            Ok(()) => Err(error),
            Err(rollback_error) => Err(LcmError::Db(format!(
                "{error}; rollback after authority loss failed: {rollback_error}"
            ))),
        };
    }
    transaction.commit().await
}

#[derive(Clone)]
enum RetentionReadConnection {
    Database(DatabaseEngineReadConnection),
    #[cfg(test)]
    Connection(Connection),
}

impl QueryExecutor for RetentionReadConnection {
    async fn query<P>(
        &self,
        sql: &str,
        params: P,
    ) -> tracedecay_runtime_core::db::engine::Result<tracedecay_runtime_core::db::engine::Rows>
    where
        P: tracedecay_runtime_core::db::engine::IntoParams,
    {
        match self {
            Self::Database(connection) => connection.query(sql, params).await,
            #[cfg(test)]
            Self::Connection(connection) => connection.query(sql, params).await,
        }
    }
}

enum RetentionWriteTransaction<'a> {
    Database(DatabaseMemoryTransaction<'a>),
    #[cfg(test)]
    Connection(Transaction),
}

impl RetentionWriteTransaction<'_> {
    async fn commit(self) -> Result<(), LcmError> {
        match self {
            Self::Database(transaction) => transaction
                .commit()
                .await
                .map_err(|error| LcmError::Db(error.to_string())),
            #[cfg(test)]
            Self::Connection(transaction) => transaction.commit().await.map_err(Into::into),
        }
    }

    async fn rollback(self) -> Result<(), LcmError> {
        match self {
            Self::Database(transaction) => transaction
                .rollback()
                .await
                .map_err(|error| LcmError::Db(error.to_string())),
            #[cfg(test)]
            Self::Connection(transaction) => transaction.rollback().await.map_err(Into::into),
        }
    }
}

impl QueryExecutor for RetentionWriteTransaction<'_> {
    async fn query<P>(
        &self,
        sql: &str,
        params: P,
    ) -> tracedecay_runtime_core::db::engine::Result<tracedecay_runtime_core::db::engine::Rows>
    where
        P: tracedecay_runtime_core::db::engine::IntoParams,
    {
        match self {
            Self::Database(transaction) => transaction.query(sql, params).await,
            #[cfg(test)]
            Self::Connection(transaction) => transaction.query(sql, params).await,
        }
    }
}

impl Executor for RetentionWriteTransaction<'_> {
    async fn execute<P>(
        &self,
        sql: &str,
        params: P,
    ) -> tracedecay_runtime_core::db::engine::Result<u64>
    where
        P: tracedecay_runtime_core::db::engine::IntoParams,
    {
        match self {
            Self::Database(transaction) => transaction.execute(sql, params).await,
            #[cfg(test)]
            Self::Connection(transaction) => transaction.execute(sql, params).await,
        }
    }

    async fn execute_batch(&self, sql: &str) -> tracedecay_runtime_core::db::engine::Result<()> {
        match self {
            Self::Database(transaction) => transaction.execute_batch(sql).await,
            #[cfg(test)]
            Self::Connection(transaction) => transaction.execute_batch(sql).await,
        }
    }
}

#[derive(Clone, Copy)]
enum RetentionStore<'a> {
    Database(&'a Database),
    #[cfg(test)]
    Connection(&'a Connection),
}

impl<'a> RetentionStore<'a> {
    fn read_connection(self) -> RetentionReadConnection {
        match self {
            Self::Database(database) => {
                RetentionReadConnection::Database(database.read_connection())
            }
            #[cfg(test)]
            Self::Connection(connection) => {
                RetentionReadConnection::Connection((*connection).clone())
            }
        }
    }

    async fn begin_memory_write_transaction(
        self,
        intent: &str,
        authorize: Option<&RetentionAuthorization<'_>>,
    ) -> Result<RetentionWriteTransaction<'a>, LcmError> {
        if let Some(authorize) = authorize {
            authorize(intent)?;
        }
        match self {
            Self::Database(database) => database
                .begin_memory_write_transaction(intent)
                .await
                .map(RetentionWriteTransaction::Database)
                .map_err(|error| LcmError::Db(error.to_string())),
            #[cfg(test)]
            Self::Connection(connection) => connection
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .await
                .map(RetentionWriteTransaction::Connection)
                .map_err(Into::into),
        }
    }
}

#[derive(Clone, Copy)]
struct RetentionScope<'a> {
    provider: &'a str,
    session_id: Option<&'a str>,
}

impl RetentionScope<'_> {
    fn is_unscoped(self) -> bool {
        self.provider == "all" && self.session_id.is_none()
    }
}

/// Where one pass resumes discovering candidates.
///
/// A durable row becomes eligible either when it ages past the window or when
/// a summary first covers it after it already had. A pass therefore reads only
/// the rows that aged since its last run (a `(timestamp, store_id)` range of
/// `idx_lcm_raw_timestamp`) and the summary sources recorded since (a rowid
/// range of `session_summary_sources`, which is append-only), never every
/// durable row or every old row.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct RetentionCursor {
    aged_through: (i64, i64),
    durable_through: i64,
}

impl RetentionCursor {
    fn keys(pass: &str) -> (String, String) {
        (
            format!("retention_{pass}_aged_through"),
            format!("retention_{pass}_durable_through"),
        )
    }

    async fn read(conn: &(impl QueryExecutor + ?Sized), pass: &str) -> Result<Self, LcmError> {
        let (aged_key, durable_key) = Self::keys(pass);
        let aged_through = match schema::get_gc_meta(conn, &aged_key).await? {
            Some(value) => value
                .split_once(':')
                .and_then(|(timestamp, store_id)| {
                    Some((timestamp.parse().ok()?, store_id.parse().ok()?))
                })
                .ok_or_else(|| LcmError::Db(format!("invalid {aged_key} cursor {value:?}")))?,
            None => (i64::MIN, i64::MIN),
        };
        let durable_through = match schema::get_gc_meta(conn, &durable_key).await? {
            Some(value) => value
                .parse()
                .map_err(|_| LcmError::Db(format!("invalid {durable_key} cursor {value:?}")))?,
            // The aging scan starts from the oldest row, so it examines every
            // row the summary sources recorded so far cover.
            None => {
                util::fetch_i64(
                    conn,
                    "SELECT COALESCE(MAX(rowid), 0) FROM session_summary_sources",
                    (),
                    "summary source watermark",
                )
                .await?
            }
        };
        Ok(Self {
            aged_through,
            durable_through,
        })
    }

    async fn persist(&self, conn: &(impl Executor + ?Sized), pass: &str) -> Result<(), LcmError> {
        let (aged_key, durable_key) = Self::keys(pass);
        let (timestamp, store_id) = self.aged_through;
        schema::set_gc_meta(conn, &aged_key, &format!("{timestamp}:{store_id}")).await?;
        schema::set_gc_meta(conn, &durable_key, &self.durable_through.to_string()).await
    }
}

/// Reads up to `limit` durable rows past `cutoff` that became eligible since
/// `cursor`: first the rows that aged, then the rows a new summary covers.
/// `columns` start with `r.store_id, r.timestamp`; `eligible` narrows the
/// rows a pass can act on. Returns the rows and the cursor past everything
/// examined.
#[allow(clippy::too_many_arguments)]
async fn select_retention_candidates<T>(
    read: &(impl QueryExecutor + ?Sized),
    scope: RetentionScope<'_>,
    cursor: RetentionCursor,
    cutoff: i64,
    limit: i64,
    columns: &str,
    eligible: &str,
    decode: impl Fn(&Row) -> Result<T, LcmError>,
) -> Result<(Vec<T>, RetentionCursor), LcmError> {
    let (aged_timestamp, aged_store_id) = cursor.aged_through;
    let mut rows = read
        .query(
            &format!(
                "SELECT {columns}
                 FROM lcm_raw_messages r
                 WHERE r.timestamp IS NOT NULL AND r.timestamp < ?3
                   AND (r.timestamp > ?4 OR (r.timestamp = ?4 AND r.store_id > ?5))
                   AND {PROJECTION_DURABLE}
                   AND (?1 = 'all' OR r.provider = ?1)
                   AND (?2 IS NULL OR r.session_id = ?2)
                   AND {eligible}
                 ORDER BY r.timestamp, r.store_id
                 LIMIT ?6"
            ),
            params![
                scope.provider,
                util::opt_text(scope.session_id),
                cutoff,
                aged_timestamp,
                aged_store_id,
                limit
            ],
        )
        .await?;
    let mut candidates = Vec::new();
    let mut aged_store_ids = BTreeSet::new();
    let mut last_aged = None;
    while let Some(row) = rows.next().await? {
        let store_id = row.get::<i64>(0)?;
        last_aged = Some((row.get::<i64>(1)?, store_id));
        aged_store_ids.insert(store_id);
        candidates.push(decode(&row)?);
    }
    drop(rows);
    let mut next = cursor;
    let returned = i64::try_from(candidates.len()).unwrap_or(i64::MAX);
    next.aged_through = match last_aged {
        Some(last) if returned == limit => last,
        // Every row older than the cutoff has been examined.
        _ => (cutoff.saturating_sub(1), i64::MAX),
    };
    let remaining = limit.saturating_sub(returned);
    if remaining == 0 {
        return Ok((candidates, next));
    }
    let mut sources = read
        .query(
            "SELECT rowid, CAST(source_id AS INTEGER) FROM session_summary_sources
             WHERE rowid > ?1 AND source_kind = 'raw_message'
             ORDER BY rowid LIMIT ?2",
            params![cursor.durable_through, remaining],
        )
        .await?;
    let mut covered = Vec::new();
    while let Some(row) = sources.next().await? {
        next.durable_through = row.get(0)?;
        covered.push(row.get::<i64>(1)?);
    }
    drop(sources);
    if covered.is_empty() {
        return Ok((candidates, next));
    }
    let covered_json = serde_json::to_string(&covered)
        .map_err(|error| LcmError::Db(format!("encode summary-covered rows: {error}")))?;
    let mut rows = read
        .query(
            &format!(
                "SELECT {columns}
                 FROM json_each(?4) AS covered
                 JOIN lcm_raw_messages r ON r.store_id = covered.value
                 WHERE r.timestamp IS NOT NULL AND r.timestamp < ?3
                   AND (?1 = 'all' OR r.provider = ?1)
                   AND (?2 IS NULL OR r.session_id = ?2)
                   AND {eligible}
                 ORDER BY r.timestamp, r.store_id"
            ),
            params![
                scope.provider,
                util::opt_text(scope.session_id),
                cutoff,
                covered_json
            ],
        )
        .await?;
    while let Some(row) = rows.next().await? {
        // A row that aged in this same pass is already a candidate.
        if !aged_store_ids.contains(&row.get::<i64>(0)?) {
            candidates.push(decode(&row)?);
        }
    }
    Ok((candidates, next))
}

struct DropRow {
    store_id: i64,
    timestamp: i64,
    content_len: u64,
}

#[allow(clippy::too_many_arguments)]
async fn run_drop_pass(
    store: RetentionStore<'_>,
    scope: RetentionScope<'_>,
    config: &LcmRetentionConfig,
    mode: RetentionMode,
    now: i64,
    errors: &mut Vec<String>,
    authorize: Option<&RetentionAuthorization<'_>>,
) -> Result<(LcmRetentionPhaseReport, Option<RetentionCursor>), LcmError> {
    let mut report = LcmRetentionPhaseReport {
        window_days: config.drop_after_days,
        ..LcmRetentionPhaseReport::default()
    };
    let Some(window) = config.drop_after_days else {
        return Ok((report, None));
    };
    let cutoff = cutoff_secs(window, now);
    // Candidates are read outside the write transaction; the delete below
    // re-checks each row, so the transaction holds only the bounded batch.
    let read = store.read_connection();
    let cursor = RetentionCursor::read(&read, "drop").await?;
    let (targets, next) = select_retention_candidates(
        &read,
        scope,
        cursor,
        cutoff,
        config.batch_limit(),
        "r.store_id, r.timestamp, LENGTH(COALESCE(r.content, ''))",
        "1 = 1",
        |row| {
            Ok(DropRow {
                store_id: row.get(0)?,
                timestamp: row.get(1)?,
                content_len: row.get::<i64>(2)?.max(0) as u64,
            })
        },
    )
    .await?;
    report.eligible = targets.len() as u64;
    report.oldest_eligible_at = targets.iter().map(|target| target.timestamp).min();
    if !mode.is_apply() {
        report.bytes_reclaimed = targets.iter().map(|t| t.content_len).sum();
        return Ok((report, None));
    }
    if targets.is_empty() {
        return Ok((report, Some(next)));
    }

    let txn = store
        .begin_memory_write_transaction("begin session retention drop pass", authorize)
        .await?;
    let delete_sql = format!(
        "DELETE FROM lcm_raw_messages AS r
         WHERE r.store_id = ?1
           AND r.timestamp IS NOT NULL AND r.timestamp < ?2
           AND {PROJECTION_DURABLE}"
    );
    let errors_before = errors.len();
    for target in &targets {
        if let Some(authorize) = authorize {
            authorize("drop session retention row")?;
        }
        // The FTS delete trigger fires with the row, and the payload GC
        // candidate trigger records any payload the row owned. A row that
        // stopped qualifying since it was read changes nothing.
        match txn
            .execute(&delete_sql, params![target.store_id, cutoff])
            .await
        {
            Ok(1) => {
                report.acted += 1;
                report.bytes_reclaimed = report.bytes_reclaimed.saturating_add(target.content_len);
            }
            Ok(0) => {}
            Ok(changed) => errors.push(format!(
                "drop raw row {} changed {changed} rows",
                target.store_id
            )),
            Err(err) => errors.push(format!("drop raw row {}: {err}", target.store_id)),
        }
    }
    commit_authorized(txn, authorize, "commit session retention drop pass").await?;
    // A failed row stays behind the cursor so the next pass retries it.
    Ok((report, (errors.len() == errors_before).then_some(next)))
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
    store: RetentionStore<'_>,
    storage_root: &Path,
    scope: RetentionScope<'_>,
    config: &LcmRetentionConfig,
    mode: RetentionMode,
    now: i64,
    errors: &mut Vec<String>,
    authorize: Option<&RetentionAuthorization<'_>>,
) -> Result<(LcmRetentionPhaseReport, Option<RetentionCursor>), LcmError> {
    let mut report = LcmRetentionPhaseReport {
        window_days: config.offload_after_days,
        ..LcmRetentionPhaseReport::default()
    };
    let Some(window) = config.offload_after_days else {
        return Ok((report, None));
    };
    let cutoff = cutoff_secs(window, now);
    let read = store.read_connection();
    let cursor = RetentionCursor::read(&read, "offload").await?;
    let (targets, next) = select_retention_candidates(
        &read,
        scope,
        cursor,
        cutoff,
        config.batch_limit(),
        "r.store_id, r.timestamp, r.provider, r.session_id, r.message_id, r.content",
        "r.storage_kind = 'inline' AND r.content IS NOT NULL AND LENGTH(r.content) > 0",
        |row| {
            Ok(OffloadRow {
                store_id: row.get(0)?,
                timestamp: row.get(1)?,
                provider: row.get(2)?,
                session_id: row.get(3)?,
                message_id: row.get(4)?,
                content: row.get(5)?,
            })
        },
    )
    .await?;
    report.eligible = targets.len() as u64;
    report.oldest_eligible_at = targets.iter().map(|target| target.timestamp).min();
    if !mode.is_apply() {
        report.bytes_reclaimed = targets.iter().map(|t| t.content.len() as u64).sum();
        return Ok((report, None));
    }

    // Each row is offloaded atomically: write the content-addressed file, then
    // flip the row to external + placeholder in its own transaction. A crash
    // between file write and commit is cleaned up by the rollback guard.
    let errors_before = errors.len();
    for target in targets {
        match offload_one(store, storage_root, &target, authorize).await {
            Ok(bytes) => {
                report.acted += 1;
                report.bytes_reclaimed = report.bytes_reclaimed.saturating_add(bytes);
            }
            Err(err) => errors.push(format!("offload raw row {}: {err}", target.store_id)),
        }
    }
    // A failed row stays behind the cursor so the next pass retries it.
    Ok((report, (errors.len() == errors_before).then_some(next)))
}

async fn offload_one(
    store: RetentionStore<'_>,
    storage_root: &Path,
    target: &OffloadRow,
    authorize: Option<&RetentionAuthorization<'_>>,
) -> Result<u64, LcmError> {
    let byte_len = target.content.len() as u64;
    if let Some(authorize) = authorize {
        authorize("begin session retention offload payload write")?;
    }
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

    let txn = store
        .begin_memory_write_transaction("begin session retention offload pass", authorize)
        .await?;
    if let Some(authorize) = authorize {
        authorize("upsert session retention offload metadata")?;
    }
    payload::upsert_payload_metadata(&txn, &payload_ref).await?;
    if let Some(authorize) = authorize {
        authorize("compare and swap session retention offload row")?;
    }
    let update_sql = format!(
        "UPDATE lcm_raw_messages AS r
         SET content = NULL,
             content_hash = ?2,
             storage_kind = 'external',
             payload_ref = ?3,
             placeholder_text = ?4
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

#[cfg(test)]
mod tests;
