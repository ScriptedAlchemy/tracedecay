//! Conservative, opt-in retention for the largest append-only telemetry
//! tables.
//!
//! Three tables grow without bound and had no scheduled pruning:
//!
//! * `analytics_events` — hook/tool/skill telemetry. Derived, reconstructable
//!   signal, so it carries a **safe default retention of 180 days**.
//! * `session_messages` and `lcm_raw_messages` — legacy session copies retained
//!   for a six-month recovery horizon. Current session stores additionally use
//!   projection-durability-aware retention in [`tracedecay_sessions::runtime::lcm`].
//!
//! Every window is expressed in whole days. Rows are pruned only when their
//! timestamp is both present and strictly older than the cutoff, so rows with
//! an unknown timestamp are always kept. A dry run reports the same
//! [`RetentionTableReport`] rows it would delete, without mutating anything.

use serde::Serialize;
pub use tracedecay_automation::config::{
    DEFAULT_ANALYTICS_EVENTS_RETENTION_DAYS, DEFAULT_LEGACY_SESSION_RETENTION_DAYS, RetentionConfig,
};

use crate::db::engine::Executor;
use crate::errors::{Result, TraceDecayError};

/// Free-page compaction for tracked branch databases, off the hot path
/// (plan 38, §6).
pub mod branch_compaction;
/// Exact-liveness mark-and-sweep for immutable derived code generations.
pub mod code_index_generations;
/// Store-owned quarantine and collection for corruption/recovery artifacts
/// found beside live databases (plan 38, §5).
pub mod incident_debris;
/// Store-level (whole-directory) orphan detection and collection. Row-level
/// pruning below stays inside a live store; `orphan_stores` collects entire
/// profile-sharded store directories whose project identity no longer resolves
/// to a live repository root (plan 38, §2).
pub mod orphan_stores;
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

mod backend {
    pub trait Sealed {}
}

/// Driver-neutral execution surface for retention.
///
/// The trait is sealed because retention admits only daemon-owned database
/// capabilities whose writer and snapshot lifetimes are already enforced.
#[allow(async_fn_in_trait)]
pub trait RetentionBackend: backend::Sealed {
    #[doc(hidden)]
    async fn delete_before(&self, table: RetentionTable, cutoff: i64) -> Result<u64>;
}

async fn delete_before(
    executor: &(impl Executor + ?Sized),
    table: RetentionTable,
    cutoff: i64,
) -> Result<u64> {
    let name = table.table_name();
    let eligibility = retention_eligibility(table);
    let sql = format!(
        "DELETE FROM {name} WHERE {TIMESTAMP_COLUMN} IS NOT NULL
         AND {TIMESTAMP_COLUMN} < ?1 AND {eligibility}"
    );
    executor
        .execute(&sql, crate::db::engine::params![cutoff])
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

macro_rules! retention_backend {
    ($($executor:ty),+ $(,)?) => {
        $(
            impl backend::Sealed for $executor {}

            impl RetentionBackend for $executor {
                async fn delete_before(
                    &self,
                    table: RetentionTable,
                    cutoff: i64,
                ) -> Result<u64> {
                    delete_before(self, table, cutoff).await
                }
            }
        )+
    };
}

retention_backend!(crate::global_db::RegisteredGlobalDbWriteTransaction<'_>,);

#[cfg(test)]
retention_backend!(
    crate::db::engine::Connection,
    crate::db::engine::Transaction,
);

/// Prunes rows in `table` older than its configured window. A disabled
/// window is a no-op that reports `rows = 0`.
pub async fn prune_table<E>(
    conn: &E,
    table: RetentionTable,
    window_days: Option<u32>,
    now_secs: i64,
) -> Result<RetentionTableReport>
where
    E: RetentionBackend + ?Sized,
{
    let Some(window_days) = window_days else {
        return Ok(RetentionTableReport::skipped(table));
    };
    let cutoff = cutoff_secs(window_days, now_secs);
    let name = table.table_name();
    let rows = conn.delete_before(table, cutoff).await?;

    Ok(RetentionTableReport {
        table: name,
        window_days: Some(window_days),
        applied: true,
        rows,
    })
}

/// Runs retention for the global-database tables
/// ([`RetentionTable::GLOBAL_TABLES`]) using `config`, returning a per-table
/// report.
pub async fn prune_global_tables<E>(
    conn: &E,
    config: &RetentionConfig,
    now_secs: i64,
) -> Result<Vec<RetentionTableReport>>
where
    E: RetentionBackend + ?Sized,
{
    let mut reports = Vec::with_capacity(RetentionTable::GLOBAL_TABLES.len());
    for table in RetentionTable::GLOBAL_TABLES {
        reports
            .push(prune_table(conn, table, retention_window_days(config, table), now_secs).await?);
    }
    Ok(reports)
}

/// Applies global-database retention in one registered write transaction.
pub async fn prune_global_retention(
    database: &crate::global_db::RegisteredGlobalDb,
    config: &RetentionConfig,
    now_secs: i64,
) -> Result<Vec<RetentionTableReport>> {
    let transaction = database.begin_write_transaction().await?;
    let reports = prune_global_tables(&transaction, config, now_secs).await?;
    transaction.commit().await?;
    Ok(reports)
}

fn retention_error(table: &str, op: &str, err: &crate::db::engine::Error) -> TraceDecayError {
    TraceDecayError::Database {
        message: format!("retention {op} on '{table}' failed: {err}"),
        operation: format!("retention::{op}"),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use crate::db::engine::{Connection, TestConnection, params};

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

    #[test]
    fn defaults_bound_legacy_session_data_and_prune_analytics() {
        let config = RetentionConfig::default();
        assert_eq!(config.analytics_events_days, Some(180));
        assert_eq!(config.session_messages_days, Some(180));
        assert_eq!(config.lcm_raw_messages_days, Some(180));
    }

    #[test]
    fn config_deserializes_partial_toml_with_bounded_session_defaults() {
        let config: RetentionConfig =
            serde_json::from_str(r#"{"analytics_events_days": 30}"#).unwrap();
        assert_eq!(config.analytics_events_days, Some(30));
        assert_eq!(config.session_messages_days, Some(180));
        assert_eq!(config.lcm_raw_messages_days, Some(180));

        // An empty object falls back to the safe defaults.
        let empty: RetentionConfig = serde_json::from_str("{}").unwrap();
        assert_eq!(empty, RetentionConfig::default());
    }

    #[tokio::test]
    async fn disabled_window_is_a_no_op() {
        let directory = tempfile::tempdir().unwrap();
        let conn = test_conn(&directory);
        let now = 1_000_000_000;
        seed_analytics(&conn, &[Some(now - 10 * SECONDS_PER_DAY), Some(now)]).await;

        let report = prune_table(&*conn, RetentionTable::AnalyticsEvents, None, now)
            .await
            .unwrap();
        assert_eq!(report.rows, 0);
        assert!(!report.applied);
        assert_eq!(count(&conn).await, 2, "disabled window must delete nothing");
    }

    #[tokio::test]
    async fn apply_deletes_only_rows_older_than_window_and_keeps_null_timestamps() {
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

        let report = prune_table(&*conn, RetentionTable::AnalyticsEvents, Some(180), now)
            .await
            .unwrap();
        assert_eq!(report.rows, 2);
        assert!(report.applied);
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
        prune_table(
            &*conn,
            RetentionTable::SessionMessages,
            config.session_messages_days,
            now,
        )
        .await
        .unwrap();
        prune_table(
            &*conn,
            RetentionTable::LcmRawMessages,
            config.lcm_raw_messages_days,
            now,
        )
        .await
        .unwrap();

        assert_eq!(count_message(&conn, "session_messages", "durable").await, 0);
        assert_eq!(count_message(&conn, "lcm_raw_messages", "durable").await, 0);
        assert_eq!(count_message(&conn, "session_messages", "live").await, 1);
        assert_eq!(count_message(&conn, "lcm_raw_messages", "live").await, 1);
    }

    #[tokio::test]
    async fn prune_global_tables_reports_each_table() {
        let directory = tempfile::tempdir().unwrap();
        let conn = test_conn(&directory);
        let now = 1_000_000_000;
        seed_analytics(&conn, &[Some(now - 400 * SECONDS_PER_DAY)]).await;
        // session_messages must exist for the (disabled) count/skip path; with
        // a None window it is never queried, so no table is required.
        let reports = prune_global_tables(&*conn, &config_days(Some(180)), now)
            .await
            .unwrap();
        assert_eq!(reports.len(), 3);
        let analytics = reports
            .iter()
            .find(|r| r.table == "analytics_events")
            .unwrap();
        assert_eq!(analytics.rows, 1);
        let sessions = reports
            .iter()
            .find(|r| r.table == "session_messages")
            .unwrap();
        assert_eq!(sessions.rows, 0, "session retention is disabled by default");
        assert_eq!(sessions.window_days, None);
        // Review-fix guard: lcm_raw_messages participates in every global
        // pass — an operator-set lcm_raw_messages_days must never be
        // silently ignored. Disabled by default (lossless).
        let lcm = reports
            .iter()
            .find(|r| r.table == "lcm_raw_messages")
            .expect("lcm_raw_messages must be reported in global passes");
        assert_eq!(lcm.rows, 0, "lcm retention is disabled by default");
        assert_eq!(lcm.window_days, None);
    }
}
