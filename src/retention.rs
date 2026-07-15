//! Conservative, opt-in retention for the largest append-only telemetry
//! tables.
//!
//! Three tables grow without bound and had no scheduled pruning:
//!
//! * `analytics_events` — hook/tool/skill telemetry. Derived, reconstructable
//!   signal, so it carries a **safe default retention of 180 days**.
//! * `session_messages` and `lcm_raw_messages` — the lossless record of every
//!   ingested session transcript. These are **never pruned by default**
//!   (window defaults to `None` = unlimited); an operator must explicitly opt
//!   in per table.
//!
//! Every window is expressed in whole days. Rows are pruned only when their
//! timestamp is both present and strictly older than the cutoff, so rows with
//! an unknown timestamp are always kept. A [dry-run][`RetentionPlan`] counts
//! what would be removed without mutating anything.

use libsql::{Connection, params};
use serde::{Deserialize, Serialize};

use crate::errors::{Result, TraceDecayError};

const SECONDS_PER_DAY: i64 = 24 * 60 * 60;

/// Every prunable table stores its event time in a nullable `timestamp`
/// column (unix seconds). Pruning compares against it with a
/// `IS NOT NULL AND < cutoff` predicate so unknown-timestamp rows are kept.
const TIMESTAMP_COLUMN: &str = "timestamp";

/// Default retention window for `analytics_events`, in days. Analytics rows
/// are a derived signal, so a generous six-month window loses nothing that
/// cannot be recomputed from the source transcripts.
pub const DEFAULT_ANALYTICS_EVENTS_RETENTION_DAYS: u32 = 180;

/// Per-table retention windows. A `None` window disables pruning for that
/// table (unlimited retention).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct RetentionConfig {
    /// Retention window for `analytics_events`. Defaults to
    /// [`DEFAULT_ANALYTICS_EVENTS_RETENTION_DAYS`].
    #[serde(default = "default_analytics_events_days")]
    pub analytics_events_days: Option<u32>,
    /// Retention window for `session_messages`. Defaults to `None`
    /// (unlimited): this is part of the lossless session record.
    #[serde(default)]
    pub session_messages_days: Option<u32>,
    /// Retention window for `lcm_raw_messages`. Defaults to `None`
    /// (unlimited): this is part of the lossless session record.
    #[serde(default)]
    pub lcm_raw_messages_days: Option<u32>,
}

// Serde's field default callback must return the field's `Option<u32>` type.
#[allow(clippy::unnecessary_wraps)]
fn default_analytics_events_days() -> Option<u32> {
    Some(DEFAULT_ANALYTICS_EVENTS_RETENTION_DAYS)
}

impl Default for RetentionConfig {
    fn default() -> Self {
        Self {
            analytics_events_days: default_analytics_events_days(),
            session_messages_days: None,
            lcm_raw_messages_days: None,
        }
    }
}

impl RetentionConfig {
    /// Window configured for `table`, in days (`None` = unlimited).
    pub fn window_days(&self, table: RetentionTable) -> Option<u32> {
        match table {
            RetentionTable::AnalyticsEvents => self.analytics_events_days,
            RetentionTable::SessionMessages => self.session_messages_days,
            RetentionTable::LcmRawMessages => self.lcm_raw_messages_days,
        }
    }
}

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

/// Whether a retention pass mutates the database.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RetentionMode {
    /// Count matching rows without deleting anything.
    DryRun,
    /// Delete matching rows.
    Apply,
}

impl RetentionMode {
    fn is_apply(self) -> bool {
        matches!(self, Self::Apply)
    }
}

/// Computes the cutoff unix-second timestamp for a `window_days` retention
/// window relative to `now_secs`. Rows strictly older than the cutoff are
/// eligible for pruning.
fn cutoff_secs(window_days: u32, now_secs: i64) -> i64 {
    now_secs.saturating_sub(i64::from(window_days).saturating_mul(SECONDS_PER_DAY))
}

/// Prunes (or, in [`RetentionMode::DryRun`], counts) rows in `table` older
/// than its configured window. A disabled window is a no-op that reports
/// `rows = 0`.
pub async fn prune_table(
    conn: &Connection,
    table: RetentionTable,
    window_days: Option<u32>,
    mode: RetentionMode,
    now_secs: i64,
) -> Result<RetentionTableReport> {
    let Some(window_days) = window_days else {
        return Ok(RetentionTableReport::skipped(table));
    };
    let cutoff = cutoff_secs(window_days, now_secs);
    let column = TIMESTAMP_COLUMN;
    let name = table.table_name();

    let rows = if mode.is_apply() {
        let sql = format!("DELETE FROM {name} WHERE {column} IS NOT NULL AND {column} < ?1");
        conn.execute(&sql, params![cutoff])
            .await
            .map_err(|e| retention_error(name, "delete", &e))?
    } else {
        let sql =
            format!("SELECT COUNT(*) FROM {name} WHERE {column} IS NOT NULL AND {column} < ?1");
        let mut result = conn
            .query(&sql, params![cutoff])
            .await
            .map_err(|e| retention_error(name, "count", &e))?;
        let row = result
            .next()
            .await
            .map_err(|e| retention_error(name, "count", &e))?;
        row.and_then(|row| row.get::<i64>(0).ok())
            .unwrap_or(0)
            .max(0) as u64
    };

    Ok(RetentionTableReport {
        table: name,
        window_days: Some(window_days),
        applied: mode.is_apply(),
        rows,
    })
}

/// Runs retention for the global-database tables
/// ([`RetentionTable::GLOBAL_TABLES`]) using `config`, returning a per-table
/// report. Session data is only touched when the operator has explicitly
/// configured a window for it.
pub async fn prune_global_tables(
    conn: &Connection,
    config: &RetentionConfig,
    mode: RetentionMode,
    now_secs: i64,
) -> Result<Vec<RetentionTableReport>> {
    let mut reports = Vec::with_capacity(RetentionTable::GLOBAL_TABLES.len());
    for table in RetentionTable::GLOBAL_TABLES {
        reports.push(prune_table(conn, table, config.window_days(table), mode, now_secs).await?);
    }
    Ok(reports)
}

/// A ready-to-log summary of a retention pass.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RetentionPlan {
    pub reports: Vec<RetentionTableReport>,
}

impl RetentionPlan {
    /// Total rows across all tables (matched in a dry run, deleted when
    /// applied).
    pub fn total_rows(&self) -> u64 {
        self.reports.iter().map(|report| report.rows).sum()
    }
}

fn retention_error(table: &str, op: &str, err: &libsql::Error) -> TraceDecayError {
    TraceDecayError::Database {
        message: format!("retention {op} on '{table}' failed: {err}"),
        operation: format!("retention::{op}"),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    async fn memory_conn() -> Connection {
        let db = libsql::Builder::new_local(":memory:")
            .build()
            .await
            .unwrap();
        db.connect().unwrap()
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

    fn config_days(days: Option<u32>) -> RetentionConfig {
        RetentionConfig {
            analytics_events_days: days,
            session_messages_days: None,
            lcm_raw_messages_days: None,
        }
    }

    #[test]
    fn defaults_keep_session_data_and_prune_analytics() {
        let config = RetentionConfig::default();
        assert_eq!(config.analytics_events_days, Some(180));
        assert_eq!(config.session_messages_days, None);
        assert_eq!(config.lcm_raw_messages_days, None);
    }

    #[test]
    fn config_deserializes_partial_toml_without_touching_session_defaults() {
        // Only analytics is set; session tables must stay unlimited.
        let config: RetentionConfig =
            serde_json::from_str(r#"{"analytics_events_days": 30}"#).unwrap();
        assert_eq!(config.analytics_events_days, Some(30));
        assert_eq!(config.session_messages_days, None);
        assert_eq!(config.lcm_raw_messages_days, None);

        // An empty object falls back to the safe defaults.
        let empty: RetentionConfig = serde_json::from_str("{}").unwrap();
        assert_eq!(empty, RetentionConfig::default());
    }

    #[tokio::test]
    async fn disabled_window_is_a_no_op() {
        let conn = memory_conn().await;
        let now = 1_000_000_000;
        seed_analytics(&conn, &[Some(now - 10 * SECONDS_PER_DAY), Some(now)]).await;

        let report = prune_table(
            &conn,
            RetentionTable::AnalyticsEvents,
            None,
            RetentionMode::Apply,
            now,
        )
        .await
        .unwrap();
        assert_eq!(report.rows, 0);
        assert!(!report.applied);
        assert_eq!(count(&conn).await, 2, "disabled window must delete nothing");
    }

    #[tokio::test]
    async fn dry_run_counts_but_does_not_delete() {
        let conn = memory_conn().await;
        let now = 1_000_000_000;
        seed_analytics(
            &conn,
            &[
                Some(now - 200 * SECONDS_PER_DAY),
                Some(now - 181 * SECONDS_PER_DAY),
                Some(now - 5 * SECONDS_PER_DAY),
                None,
            ],
        )
        .await;

        let report = prune_table(
            &conn,
            RetentionTable::AnalyticsEvents,
            Some(180),
            RetentionMode::DryRun,
            now,
        )
        .await
        .unwrap();
        assert_eq!(report.rows, 2, "two rows are older than 180 days");
        assert!(!report.applied);
        assert_eq!(count(&conn).await, 4, "dry run must not mutate");
    }

    #[tokio::test]
    async fn apply_deletes_only_rows_older_than_window_and_keeps_null_timestamps() {
        let conn = memory_conn().await;
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

        let report = prune_table(
            &conn,
            RetentionTable::AnalyticsEvents,
            Some(180),
            RetentionMode::Apply,
            now,
        )
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
    async fn prune_global_tables_reports_each_table() {
        let conn = memory_conn().await;
        let now = 1_000_000_000;
        seed_analytics(&conn, &[Some(now - 400 * SECONDS_PER_DAY)]).await;
        // session_messages must exist for the (disabled) count/skip path; with
        // a None window it is never queried, so no table is required.
        let reports =
            prune_global_tables(&conn, &config_days(Some(180)), RetentionMode::Apply, now)
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
