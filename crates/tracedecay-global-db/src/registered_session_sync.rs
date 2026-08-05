use tracedecay_runtime_core::db::engine::params;
use tracedecay_runtime_core::errors::TraceDecayError;

use crate::{RegisteredGlobalDb, global_db_operation_error};

impl RegisteredGlobalDb {
    pub async fn read_session_sync_journal(
        &self,
        key: &str,
    ) -> Result<Option<String>, TraceDecayError> {
        let snapshot = self
            .read_snapshot()
            .await
            .map_err(|error| global_db_operation_error("open session sync journal", error))?;
        let mut rows = snapshot
            .query(
                "SELECT value FROM session_backfill_meta WHERE key = ?1",
                params![key],
            )
            .await
            .map_err(|error| global_db_operation_error("read session sync journal", error))?;
        let Some(row) = rows
            .next()
            .await
            .map_err(|error| global_db_operation_error("step session sync journal", error))?
        else {
            return Ok(None);
        };
        row.get(0)
            .map(Some)
            .map_err(|error| global_db_operation_error("decode session sync journal", error))
    }

    pub async fn list_session_sync_journals(
        &self,
        key_prefix: &str,
    ) -> Result<Vec<(String, String)>, TraceDecayError> {
        let snapshot = self
            .read_snapshot()
            .await
            .map_err(|error| global_db_operation_error("open session sync journals", error))?;
        let mut rows = snapshot
            .query(
                "SELECT key, value
                 FROM session_backfill_meta
                 WHERE key >= ?1 AND key < ?2
                 ORDER BY key",
                params![key_prefix, format!("{key_prefix}\u{10ffff}")],
            )
            .await
            .map_err(|error| global_db_operation_error("list session sync journals", error))?;
        let mut journals = Vec::new();
        while let Some(row) = rows
            .next()
            .await
            .map_err(|error| global_db_operation_error("step session sync journals", error))?
        {
            let key = row
                .get(0)
                .map_err(|error| global_db_operation_error("decode session sync key", error))?;
            let value = row
                .get(1)
                .map_err(|error| global_db_operation_error("decode session sync value", error))?;
            journals.push((key, value));
        }
        Ok(journals)
    }

    pub async fn insert_session_sync_journal(
        &self,
        key: &str,
        value: &str,
    ) -> Result<bool, TraceDecayError> {
        let writer = self.writer_connection().map_err(|error| {
            global_db_operation_error("open session sync journal writer", error)
        })?;
        writer
            .execute(
                "INSERT OR IGNORE INTO session_backfill_meta(key, value, updated_at)
                 VALUES (?1, ?2, unixepoch())",
                params![key, value],
            )
            .await
            .map(|changed| changed == 1)
            .map_err(|error| global_db_operation_error("insert session sync journal", error))
    }

    pub async fn compare_and_swap_session_sync_journal(
        &self,
        key: &str,
        expected: &str,
        replacement: &str,
    ) -> Result<bool, TraceDecayError> {
        let writer = self.writer_connection().map_err(|error| {
            global_db_operation_error("open session sync journal writer", error)
        })?;
        writer
            .execute(
                "UPDATE session_backfill_meta
                 SET value = ?3, updated_at = unixepoch()
                 WHERE key = ?1 AND value = ?2",
                params![key, expected, replacement],
            )
            .await
            .map(|changed| changed == 1)
            .map_err(|error| global_db_operation_error("update session sync journal", error))
    }
}
