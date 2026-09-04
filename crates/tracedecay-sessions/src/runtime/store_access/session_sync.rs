use tracedecay_domain::errors::TraceDecayError;
use tracedecay_runtime_core::db::engine::{Value, params};
use tracedecay_runtime_core::db::{
    DatabaseEngineReadSnapshot, collect_rowid_pages, collect_rowid_pages_with,
};

use super::super::registered_db::{SessionExec, SessionRegisteredDb, SessionStoreAccess};

const SESSION_SYNC_RECOVERY_PAGE_ROWS: i64 = 8;

fn store_operation_error(
    operation: &'static str,
    source: impl std::error::Error + Send + Sync + 'static,
) -> TraceDecayError {
    TraceDecayError::database_operation(operation, source)
}

fn store_operation_message(operation: &'static str, message: impl Into<String>) -> TraceDecayError {
    TraceDecayError::Database {
        message: message.into(),
        operation: operation.to_string(),
    }
}

impl<D: SessionRegisteredDb + Sync> SessionStoreAccess<'_, D> {
    #[hotpath::measure(future = true, label = "global_db.registered.session_sync.frontiers")]
    /// Reads every committed source cursor through bounded `rowid` keyset
    /// pages.
    ///
    /// A session store keeps one cursor row per observed source, so a
    /// long-lived profile holds far more rows than the `SQLite` runtime
    /// materializes for a single exact-SQL query; an unbounded read here
    /// degraded every project full-upgrade on such profiles.
    pub async fn list_session_sync_source_frontiers(
        &self,
    ) -> Result<Vec<(String, String, String)>, TraceDecayError> {
        let snapshot = self
            .read_snapshot()
            .await
            .map_err(|error| store_operation_error("open session source frontiers", error))?;
        let mut frontiers = collect_rowid_pages(
            &snapshot,
            "SELECT source_json, scope_json, cursor_json, rowid
             FROM source_cursors
             WHERE rowid > ?1
             ORDER BY rowid
             LIMIT ?2",
            3,
            |row| {
                Ok((
                    row.get::<String>(0)?,
                    row.get::<String>(1)?,
                    row.get::<String>(2)?,
                ))
            },
            "list session source frontiers",
        )
        .await?;
        frontiers.sort_unstable();
        Ok(frontiers)
    }

    #[hotpath::skip]
    pub async fn read_session_sync_journal(
        &self,
        key: &str,
    ) -> Result<Option<String>, TraceDecayError> {
        let snapshot = self
            .read_snapshot()
            .await
            .map_err(|error| store_operation_error("open session sync journal", error))?;
        let mut rows = snapshot
            .query(
                "SELECT value FROM session_backfill_meta WHERE key = ?1",
                params![key],
            )
            .await
            .map_err(|error| store_operation_error("read session sync journal", error))?;
        let Some(row) = rows
            .next()
            .await
            .map_err(|error| store_operation_error("step session sync journal", error))?
        else {
            return Ok(None);
        };
        row.get(0)
            .map(Some)
            .map_err(|error| store_operation_error("decode session sync journal", error))
    }

    #[hotpath::skip]
    pub async fn list_session_sync_journals(
        &self,
        key_prefix: &str,
    ) -> Result<Vec<(String, String)>, TraceDecayError> {
        let snapshot = self
            .read_snapshot()
            .await
            .map_err(|error| store_operation_error("open session sync journals", error))?;
        let mut keys = collect_rowid_pages_with(
            &snapshot,
            "SELECT key, rowid
             FROM session_backfill_meta
             WHERE key >= ?1 AND key < ?2 AND rowid > ?3
             ORDER BY rowid
             LIMIT ?4",
            &[
                Value::Text(key_prefix.to_owned()),
                Value::Text(format!("{key_prefix}\u{10ffff}")),
            ],
            1,
            |row| row.get::<String>(0),
            "list session sync journals",
        )
        .await?;
        keys.sort_unstable();
        read_journal_values_for_keys(&snapshot, keys).await
    }

    #[hotpath::measure(
        future = true,
        label = "global_db.registered.session_sync.recovery_page"
    )]
    /// Reads one keyset page of journals that can still require recovery.
    /// Journals may each retain multi-megabyte source frontiers, so the page
    /// query returns keys only — a page of values could exceed the exact-SQL
    /// byte budget together even though each value fits alone — and every
    /// value then arrives through its own single-row read.
    #[hotpath::skip]
    pub async fn list_incomplete_session_sync_journal_page(
        &self,
        key_prefix: &str,
        after_key: Option<&str>,
    ) -> Result<Vec<(String, String)>, TraceDecayError> {
        self.list_incomplete_session_sync_journal_page_through(
            key_prefix,
            after_key,
            &format!("{key_prefix}\u{10ffff}"),
        )
        .await
    }

    #[hotpath::skip]
    pub async fn list_incomplete_session_sync_journal_page_through(
        &self,
        key_prefix: &str,
        after_key: Option<&str>,
        through_key: &str,
    ) -> Result<Vec<(String, String)>, TraceDecayError> {
        let snapshot = self.read_snapshot().await.map_err(|error| {
            store_operation_error("open incomplete session sync journals", error)
        })?;
        let mut rows = snapshot
            .query(
                "SELECT key
                 FROM session_backfill_meta
                 WHERE key >= ?1 AND key < ?2 AND key > ?3
                   AND key <= ?4
                   AND CASE
                       WHEN json_valid(value)
                       THEN COALESCE(json_extract(value, '$.status') != 'complete', 1)
                       ELSE 1
                   END
                 ORDER BY key
                 LIMIT ?5",
                params![
                    key_prefix,
                    format!("{key_prefix}\u{10ffff}"),
                    after_key.unwrap_or(""),
                    through_key,
                    SESSION_SYNC_RECOVERY_PAGE_ROWS,
                ],
            )
            .await
            .map_err(|error| {
                store_operation_error("list incomplete session sync journals", error)
            })?;
        let mut keys = Vec::new();
        while let Some(row) = rows.next().await.map_err(|error| {
            store_operation_error("step incomplete session sync journals", error)
        })? {
            keys.push(row.get(0).map_err(|error| {
                store_operation_error("decode incomplete session sync key", error)
            })?);
        }
        read_journal_values_for_keys(&snapshot, keys).await
    }

    #[hotpath::skip]
    pub async fn session_sync_journal_high_water(
        &self,
        key_prefix: &str,
    ) -> Result<Option<String>, TraceDecayError> {
        let snapshot = self.read_snapshot().await.map_err(|error| {
            store_operation_error("open session sync journal high water", error)
        })?;
        let mut rows = snapshot
            .query(
                "SELECT MAX(key)
                 FROM session_backfill_meta
                 WHERE key >= ?1 AND key < ?2",
                params![key_prefix, format!("{key_prefix}\u{10ffff}")],
            )
            .await
            .map_err(|error| {
                store_operation_error("read session sync journal high water", error)
            })?;
        let Some(row) = rows.next().await.map_err(|error| {
            store_operation_error("step session sync journal high water", error)
        })?
        else {
            return Err(store_operation_message(
                "read session sync journal high water",
                "aggregate query returned no row",
            ));
        };
        row.get(0)
            .map_err(|error| store_operation_error("decode session sync journal high water", error))
    }

    #[hotpath::measure(future = true, label = "global_db.registered.session_sync.insert")]
    pub async fn insert_session_sync_journal(
        &self,
        key: &str,
        value: &str,
    ) -> Result<bool, TraceDecayError> {
        let writer = self
            .writer_connection()
            .map_err(|error| store_operation_error("open session sync journal writer", error))?;
        SessionExec::execute(
            &writer,
            "INSERT OR IGNORE INTO session_backfill_meta(key, value, updated_at)
                 VALUES (?1, ?2, unixepoch())",
            params![key, value],
        )
        .await
        .map(|changed| changed == 1)
        .map_err(|error| store_operation_error("insert session sync journal", error))
    }

    #[hotpath::measure(future = true, label = "global_db.registered.session_sync.cas")]
    pub async fn compare_and_swap_session_sync_journal(
        &self,
        key: &str,
        expected: &str,
        replacement: &str,
    ) -> Result<bool, TraceDecayError> {
        let writer = self
            .writer_connection()
            .map_err(|error| store_operation_error("open session sync journal writer", error))?;
        SessionExec::execute(
            &writer,
            "UPDATE session_backfill_meta
                 SET value = ?3, updated_at = unixepoch()
                 WHERE key = ?1 AND value = ?2",
            params![key, expected, replacement],
        )
        .await
        .map(|changed| changed == 1)
        .map_err(|error| store_operation_error("update session sync journal", error))
    }

    #[hotpath::measure(future = true, label = "global_db.registered.session_sync.cas_delete")]
    pub async fn compare_and_delete_session_sync_journal(
        &self,
        key: &str,
        expected: &str,
    ) -> Result<bool, TraceDecayError> {
        let writer = self
            .writer_connection()
            .map_err(|error| store_operation_error("open session sync journal writer", error))?;
        SessionExec::execute(
            &writer,
            "DELETE FROM session_backfill_meta WHERE key = ?1 AND value = ?2",
            params![key, expected],
        )
        .await
        .map(|changed| changed == 1)
        .map_err(|error| store_operation_error("delete session sync journal", error))
    }
}

/// Reads each journal value through its own single-row query within one read
/// snapshot, so a page stays byte-bounded no matter how large its journals
/// grow together. A single value past the exact-SQL budget still refuses
/// typed: that row is genuinely over-limit on its own.
async fn read_journal_values_for_keys(
    snapshot: &DatabaseEngineReadSnapshot,
    keys: Vec<String>,
) -> Result<Vec<(String, String)>, TraceDecayError> {
    let mut journals = Vec::with_capacity(keys.len());
    for key in keys {
        let mut rows = snapshot
            .query(
                "SELECT value FROM session_backfill_meta WHERE key = ?1",
                params![key.as_str()],
            )
            .await
            .map_err(|error| store_operation_error("read session sync journal value", error))?;
        let Some(row) = rows
            .next()
            .await
            .map_err(|error| store_operation_error("step session sync journal value", error))?
        else {
            // The key came from the same snapshot, so its row cannot vanish.
            return Err(store_operation_message(
                "read session sync journal value",
                format!("session sync journal '{key}' disappeared within a read snapshot"),
            ));
        };
        let value = row
            .get(0)
            .map_err(|error| store_operation_error("decode session sync journal value", error))?;
        journals.push((key, value));
    }
    Ok(journals)
}
