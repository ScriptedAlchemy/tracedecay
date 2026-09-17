//! Conservative, opt-in retention for the largest append-only telemetry
//! tables.
//!
//! Three tables grow without bound and had no scheduled pruning:
//!
//! * `analytics_events` — hook/tool/skill telemetry. Derived, reconstructable
//!   signal, so it carries a **safe default retention of 180 days**.
//! * `session_messages` and `lcm_raw_messages` — legacy session copies retained
//!   for a six-month recovery horizon. Current session stores additionally use
//!   projection-durability-aware retention in `tracedecay_lcm::retention`.
//!
//! Every window is expressed in whole days. Rows are pruned only when their
//! timestamp is both present and strictly older than the cutoff, so rows with
//! an unknown timestamp are always kept.
//!
//! A pass captures one cutoff per table and drains eligible rows in slices of
//! at most [`RETENTION_SLICE_ROWS`], each committed in its own registered write
//! transaction so foreground writers interleave between slices and an
//! interrupted pass keeps every committed slice. Reports count committed rows
//! only.

use std::fmt;

use serde::Serialize;
pub use tracedecay_automation::config::{
    DEFAULT_ANALYTICS_EVENTS_RETENTION_DAYS, DEFAULT_LEGACY_SESSION_RETENTION_DAYS, RetentionConfig,
};

use tracedecay_domain::errors::{Result, TraceDecayError};
use tracedecay_global_db::RegisteredGlobalDb;
use tracedecay_runtime_core::db::engine::{Executor, params};

/// Free-page compaction for tracked branch databases, off the hot path
/// (plan 38, §6).
pub mod branch_compaction;
/// Bounded retention for unmounted profile-sharded stores.
pub mod cold_store;
/// Read-only diagnostics over retention-owned state.
pub mod diagnostics;
/// Exact-liveness mark-and-sweep for immutable derived code generations.
/// Store-owned quarantine and collection for corruption/recovery artifacts
/// found beside live databases (plan 38, §5).
pub mod incident_debris;
/// Bounded compaction for stores retained by live runtime authorities.
pub mod live_compaction;
/// Store-level (whole-directory) orphan detection and collection. Row-level
/// pruning below stays inside a live store; `orphan_stores` collects entire
/// profile-sharded store directories whose project identity no longer resolves
/// to a live repository root (plan 38, §2).
pub mod orphan_stores;
/// Registered session-store retention across its canonical owner kernels.
pub mod registered_store;
/// Read-only, cheap-to-query per-store size and free-page-ratio reporting,
/// reachable from a command without a live daemon (plan 38, §7).
pub mod storage_report;

const SECONDS_PER_DAY: i64 = 24 * 60 * 60;

/// Every prunable table stores its event time in a nullable `timestamp`
/// column (unix seconds). Pruning compares against it with a
/// `IS NOT NULL AND < cutoff` predicate so unknown-timestamp rows are kept.
const TIMESTAMP_COLUMN: &str = "timestamp";

/// A prunable telemetry table. The variants map to a fixed table/column pair,
/// so the SQL never interpolates untrusted identifiers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RetentionTable {
    /// `analytics_events` (global DB), pruned by `timestamp`.
    AnalyticsEvents,
    /// `session_messages` (global DB), pruned by `timestamp`.
    SessionMessages,
    /// `lcm_raw_messages` (per-store LCM DB), pruned by `timestamp`.
    LcmRawMessages,
}

fn retention_window_days(config: &RetentionConfig, table: RetentionTable) -> Option<u32> {
    match table {
        RetentionTable::AnalyticsEvents => config.analytics_events_days,
        RetentionTable::SessionMessages => config.session_messages_days,
        RetentionTable::LcmRawMessages => config.lcm_raw_messages_days,
    }
}

impl RetentionTable {
    /// The three tables that live in the global database.
    pub const GLOBAL_TABLES: [RetentionTable; 3] = [
        Self::AnalyticsEvents,
        Self::SessionMessages,
        Self::LcmRawMessages,
    ];

    pub fn table_name(self) -> &'static str {
        match self {
            Self::AnalyticsEvents => "analytics_events",
            Self::SessionMessages => "session_messages",
            Self::LcmRawMessages => "lcm_raw_messages",
        }
    }
}

/// Outcome of evaluating retention for a single table.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct RetentionTableReport {
    pub table: &'static str,
    /// Configured window in days, or `None` when retention is disabled.
    pub window_days: Option<u32>,
    /// Whether rows were actually deleted (`false` for a dry run or a disabled
    /// window).
    pub applied: bool,
    /// Rows matching the cutoff. In a dry run this is what *would* be deleted;
    /// when applied it is the number deleted.
    pub rows: u64,
}

impl RetentionTableReport {
    fn skipped(table: RetentionTable) -> Self {
        Self {
            table: table.table_name(),
            window_days: None,
            applied: false,
            rows: 0,
        }
    }
}

/// Computes the cutoff unix-second timestamp for a `window_days` retention
/// window relative to `now_secs`. Rows strictly older than the cutoff are
/// eligible for pruning.
fn cutoff_secs(window_days: u32, now_secs: i64) -> i64 {
    now_secs.saturating_sub(i64::from(window_days).saturating_mul(SECONDS_PER_DAY))
}

/// Upper bound on rows removed by one committed retention slice.
///
/// Every slice is its own registered write transaction, so a large backlog
/// drains across many short writer holds instead of one transaction spanning
/// every eligible row of every table.
pub const RETENTION_SLICE_ROWS: u64 = 1_000;

/// Deletes at most [`RETENTION_SLICE_ROWS`] rows of `table` that are older
/// than `cutoff`, re-evaluating the table's eligibility predicate inside the
/// calling transaction. Fewer deleted rows than a full slice means the table
/// holds no further eligible rows for this cutoff.
async fn delete_slice(
    executor: &(impl Executor + ?Sized),
    table: RetentionTable,
    cutoff: i64,
) -> Result<u64> {
    let name = table.table_name();
    let eligibility = retention_eligibility(table);
    let sql = format!(
        "DELETE FROM {name} WHERE rowid IN (
             SELECT rowid FROM {name}
             WHERE {TIMESTAMP_COLUMN} IS NOT NULL AND {TIMESTAMP_COLUMN} < ?1
               AND {eligibility}
             LIMIT ?2
         )"
    );
    executor
        .execute(&sql, params![cutoff, RETENTION_SLICE_ROWS])
        .await
        .map_err(|error| retention_error(name, "delete", &error))
}

/// Legacy session windows still obey the current projection-durability
/// authority. Age alone never makes lossless content eligible.
fn retention_eligibility(table: RetentionTable) -> &'static str {
    match table {
        RetentionTable::AnalyticsEvents => "1 = 1",
        RetentionTable::SessionMessages => {
            "EXISTS (
                SELECT 1
                FROM lcm_raw_messages AS raw
                JOIN lcm_summary_sources AS source
                  ON source.source_kind = 'raw_message'
                 AND source.source_id = CAST(raw.store_id AS TEXT)
                WHERE raw.provider = session_messages.provider
                  AND raw.message_id = session_messages.message_id
            )"
        }
        RetentionTable::LcmRawMessages => {
            "EXISTS (
                SELECT 1 FROM lcm_summary_sources AS source
                WHERE source.source_kind = 'raw_message'
                  AND source.source_id = CAST(lcm_raw_messages.store_id AS TEXT)
            )"
        }
    }
}

/// Deletes one slice of `table` in its own registered write transaction and
/// commits it, so the writer is released before the next slice begins.
async fn commit_slice(
    database: &RegisteredGlobalDb,
    table: RetentionTable,
    cutoff: i64,
) -> Result<u64> {
    let transaction = database.begin_write_transaction().await?;
    let deleted = delete_slice(&transaction, table, cutoff).await?;
    transaction.commit().await?;
    Ok(deleted)
}

/// A retention pass that stopped before every enabled table drained.
///
/// `committed` lists exactly the tables whose slices reached a durable commit
/// before `error`; rows of the slice that failed were rolled back and are not
/// counted anywhere.
#[derive(Debug)]
pub struct RetentionPassInterruption {
    pub committed: Vec<RetentionTableReport>,
    pub error: TraceDecayError,
}

impl fmt::Display for RetentionPassInterruption {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "retention pass interrupted: {}", self.error)
    }
}

impl std::error::Error for RetentionPassInterruption {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.error)
    }
}

/// Applies global-database retention for [`RetentionTable::GLOBAL_TABLES`] in
/// bounded, separately committed slices.
///
/// Tables run in declaration order so `session_messages` is evaluated while
/// its `lcm_raw_messages` lineage still exists. The cutoff is captured once
/// per table; each slice re-checks eligibility in its own transaction, so
/// rows that gain or lose lineage between slices are judged by the current
/// authority. Disabled windows never acquire the writer.
#[hotpath::measure(label = "maintenance.retention.prune_global", future = true)]
pub async fn prune_global_retention(
    database: &RegisteredGlobalDb,
    config: &RetentionConfig,
    now_secs: i64,
) -> std::result::Result<Vec<RetentionTableReport>, RetentionPassInterruption> {
    let mut committed = Vec::with_capacity(RetentionTable::GLOBAL_TABLES.len());
    for table in RetentionTable::GLOBAL_TABLES {
        let Some(window_days) = retention_window_days(config, table) else {
            committed.push(RetentionTableReport::skipped(table));
            continue;
        };
        let cutoff = cutoff_secs(window_days, now_secs);
        let mut report = RetentionTableReport {
            table: table.table_name(),
            window_days: Some(window_days),
            applied: true,
            rows: 0,
        };
        loop {
            let deleted = match commit_slice(database, table, cutoff).await {
                Ok(deleted) => deleted,
                Err(error) => {
                    // Every committed slice before this one was full, so a
                    // zero count means no slice of this table committed.
                    if report.rows > 0 {
                        committed.push(report);
                    }
                    return Err(RetentionPassInterruption { committed, error });
                }
            };
            report.rows += deleted;
            if deleted < RETENTION_SLICE_ROWS {
                break;
            }
        }
        committed.push(report);
    }
    Ok(committed)
}

fn retention_error(
    table: &str,
    op: &str,
    err: &tracedecay_runtime_core::db::engine::Error,
) -> TraceDecayError {
    TraceDecayError::Database {
        message: format!("retention {op} on '{table}' failed: {err}"),
        operation: format!("retention::{op}"),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use std::time::Duration;

    use super::*;
    use tracedecay_global_db::tests::harness::RegisteredGlobalDbHarness;
    use tracedecay_runtime_core::db::engine::{Connection, TestConnection};

    fn test_conn(directory: &tempfile::TempDir) -> TestConnection {
        TestConnection::open(&directory.path().join("retention.db"))
    }

    async fn seed_analytics(conn: &Connection, ts: &[Option<i64>]) {
        conn.execute(
            "CREATE TABLE analytics_events (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                provider TEXT NOT NULL,
                project_id TEXT NOT NULL,
                timestamp INTEGER,
                event_kind TEXT NOT NULL
            )",
            (),
        )
        .await
        .unwrap();
        for (i, t) in ts.iter().enumerate() {
            conn.execute(
                "INSERT INTO analytics_events (provider, project_id, timestamp, event_kind)
                 VALUES ('claude', 'p', ?1, 'k')",
                params![*t],
            )
            .await
            .unwrap();
            let _ = i;
        }
    }

    async fn count(conn: &Connection) -> i64 {
        let mut rows = conn
            .query("SELECT COUNT(*) FROM analytics_events", ())
            .await
            .unwrap();
        rows.next().await.unwrap().unwrap().get::<i64>(0).unwrap()
    }

    async fn count_message(conn: &Connection, table: &str, message_id: &str) -> i64 {
        let sql = format!("SELECT COUNT(*) FROM {table} WHERE message_id = ?1");
        let mut rows = conn.query(&sql, params![message_id]).await.unwrap();
        rows.next().await.unwrap().unwrap().get::<i64>(0).unwrap()
    }

    fn config_days(days: Option<u32>) -> RetentionConfig {
        RetentionConfig {
            analytics_events_days: days,
            session_messages_days: None,
            lcm_raw_messages_days: None,
        }
    }

    async fn registered_analytics_count(database: &RegisteredGlobalDb) -> i64 {
        let mut rows = database
            .read_connection()
            .query("SELECT COUNT(*) FROM analytics_events", ())
            .await
            .unwrap();
        rows.next().await.unwrap().unwrap().get::<i64>(0).unwrap()
    }

    /// Seeds `old_rows` analytics events far outside a 180-day window plus one
    /// current event on the production schema.
    async fn seed_registered_analytics_backlog(
        database: &RegisteredGlobalDb,
        old_rows: u64,
        now: i64,
    ) {
        let old = now - 400 * SECONDS_PER_DAY;
        database
            .writer_connection()
            .unwrap()
            .execute_batch(&format!(
                "WITH RECURSIVE seq(n) AS (SELECT 1 UNION ALL SELECT n + 1 FROM seq WHERE n < {old_rows})
                 INSERT INTO analytics_events (provider, project_id, timestamp, event_kind)
                 SELECT 'claude', 'p', {old}, 'k' FROM seq;
                 INSERT INTO analytics_events (provider, project_id, timestamp, event_kind)
                 VALUES ('claude', 'p', {now}, 'k');"
            ))
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn disabled_retention_never_acquires_the_registered_writer() {
        let harness = RegisteredGlobalDbHarness::open("retention-disabled-no-writer").await;
        let database = harness.registered.as_ref();
        let now = 1_000_000_000;
        seed_registered_analytics_backlog(database, 3, now).await;
        // Any writer acquisition would park behind this open transaction.
        let held_writer = database.begin_write_transaction().await.unwrap();

        let reports = tokio::time::timeout(
            Duration::from_secs(2),
            prune_global_retention(database, &config_days(None), now),
        )
        .await
        .expect("disabled retention must finish without waiting for the writer")
        .unwrap();

        assert_eq!(reports.len(), RetentionTable::GLOBAL_TABLES.len());
        assert!(
            reports
                .iter()
                .all(|report| !report.applied && report.rows == 0),
            "disabled windows report skipped tables only: {reports:?}"
        );
        held_writer.rollback().await.unwrap();
        assert_eq!(registered_analytics_count(database).await, 4);
    }

    #[tokio::test]
    async fn backlog_drains_in_bounded_commits_and_interruption_keeps_committed_slices() {
        let harness = RegisteredGlobalDbHarness::open("retention-bounded-slices").await;
        let database = harness.registered.as_ref();
        let now = 1_000_000_000;
        let tail = 5;
        let eligible = 3 * RETENTION_SLICE_ROWS + tail;
        seed_registered_analytics_backlog(database, eligible, now).await;
        // Receipts observe every deleted row; the abort trigger fails the first
        // delete of the third slice, after two slices have committed.
        let abort_after = 2 * RETENTION_SLICE_ROWS;
        database
            .writer_connection()
            .unwrap()
            .execute_batch(&format!(
                "CREATE TABLE retention_delete_receipts (deleted_id INTEGER NOT NULL);
                 CREATE TRIGGER retention_delete_receipt AFTER DELETE ON analytics_events
                 BEGIN INSERT INTO retention_delete_receipts(deleted_id) VALUES (OLD.id); END;
                 CREATE TRIGGER retention_abort_third_slice BEFORE DELETE ON analytics_events
                 WHEN (SELECT COUNT(*) FROM retention_delete_receipts) >= {abort_after}
                 BEGIN SELECT RAISE(ABORT, 'interrupt retention between slices'); END;"
            ))
            .await
            .unwrap();
        let config = config_days(Some(180));

        let interruption = prune_global_retention(database, &config, now)
            .await
            .expect_err("the third slice must fail");
        assert_eq!(
            interruption.committed,
            vec![RetentionTableReport {
                table: "analytics_events",
                window_days: Some(180),
                applied: true,
                rows: abort_after,
            }],
            "only the two committed slices are reported"
        );
        assert!(
            interruption
                .error
                .to_string()
                .contains("interrupt retention between slices"),
            "{interruption}"
        );
        assert_eq!(
            registered_analytics_count(database).await,
            i64::try_from(eligible - abort_after + 1).unwrap(),
            "committed slices are durable and the rolled-back slice deleted nothing"
        );

        database
            .writer_connection()
            .unwrap()
            .execute_batch("DROP TRIGGER retention_abort_third_slice;")
            .await
            .unwrap();
        let reports = prune_global_retention(database, &config, now)
            .await
            .unwrap();
        assert_eq!(
            reports,
            vec![
                RetentionTableReport {
                    table: "analytics_events",
                    window_days: Some(180),
                    applied: true,
                    rows: eligible - abort_after,
                },
                RetentionTableReport::skipped(RetentionTable::SessionMessages),
                RetentionTableReport::skipped(RetentionTable::LcmRawMessages),
            ],
            "the resumed pass drains exactly the remainder without double counting"
        );
        assert_eq!(
            registered_analytics_count(database).await,
            1,
            "the in-window row survives every slice"
        );
    }

    #[tokio::test]
    async fn slice_deletes_only_rows_older_than_window_and_keeps_null_timestamps() {
        let directory = tempfile::tempdir().unwrap();
        let conn = test_conn(&directory);
        let now = 1_000_000_000;
        seed_analytics(
            &conn,
            &[
                Some(now - 200 * SECONDS_PER_DAY), // pruned
                Some(now - 181 * SECONDS_PER_DAY), // pruned
                Some(now - 179 * SECONDS_PER_DAY), // kept (inside window)
                Some(now),                         // kept
                None,                              // kept (unknown timestamp)
            ],
        )
        .await;

        let deleted = delete_slice(
            &*conn,
            RetentionTable::AnalyticsEvents,
            cutoff_secs(180, now),
        )
        .await
        .unwrap();
        assert_eq!(deleted, 2);
        assert_eq!(
            count(&conn).await,
            3,
            "rows inside the window and NULL-timestamp rows are retained"
        );
    }

    #[tokio::test]
    async fn legacy_windows_require_durable_summary_lineage() {
        let directory = tempfile::tempdir().unwrap();
        let conn = test_conn(&directory);
        let now = 1_000_000_000;
        conn.execute_batch(
            "CREATE TABLE session_messages (
                provider TEXT NOT NULL,
                message_id TEXT NOT NULL,
                timestamp INTEGER
             );
             CREATE TABLE lcm_raw_messages (
                store_id INTEGER PRIMARY KEY,
                provider TEXT NOT NULL,
                message_id TEXT NOT NULL,
                timestamp INTEGER
             );
             CREATE TABLE lcm_summary_sources (
                source_kind TEXT NOT NULL,
                source_id TEXT NOT NULL
             );
             INSERT INTO session_messages VALUES
                ('claude', 'durable', 1),
                ('claude', 'live', 1);
             INSERT INTO lcm_raw_messages VALUES
                (1, 'claude', 'durable', 1),
                (2, 'claude', 'live', 1);
             INSERT INTO lcm_summary_sources VALUES ('raw_message', '1');",
        )
        .await
        .unwrap();

        let config = RetentionConfig::default();
        for table in [
            RetentionTable::SessionMessages,
            RetentionTable::LcmRawMessages,
        ] {
            let window = retention_window_days(&config, table).unwrap();
            delete_slice(&*conn, table, cutoff_secs(window, now))
                .await
                .unwrap();
        }

        assert_eq!(count_message(&conn, "session_messages", "durable").await, 0);
        assert_eq!(count_message(&conn, "lcm_raw_messages", "durable").await, 0);
        assert_eq!(count_message(&conn, "session_messages", "live").await, 1);
        assert_eq!(count_message(&conn, "lcm_raw_messages", "live").await, 1);
    }
}
