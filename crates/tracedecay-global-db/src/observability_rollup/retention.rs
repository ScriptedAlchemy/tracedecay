use tracedecay_runtime_core::db::engine::QueryExecutor;

use crate::RegisteredGlobalDb;

use super::{
    OBSERVABILITY_ROLLUP_RETENTION_DAYS_V1, ObservabilityRollupRetentionReceiptV1, SECONDS_PER_DAY,
};

impl RegisteredGlobalDb {
    pub async fn prune_observability_rollups(
        &self,
        now_seconds: i64,
    ) -> Result<ObservabilityRollupRetentionReceiptV1, String> {
        if now_seconds < 0 {
            return Err("invalid observability rollup retention time".to_owned());
        }
        let cutoff =
            now_seconds.saturating_sub(OBSERVABILITY_ROLLUP_RETENTION_DAYS_V1 * SECONDS_PER_DAY);
        let transaction = self
            .begin_write_transaction()
            .await
            .map_err(|error| format!("failed to begin observability rollup retention: {error}"))?;
        let expired_generations =
            count_before_day(&transaction, "observability_rollup_generations", cutoff).await?;
        let expired_journal_entries =
            count_before_day(&transaction, "observability_rollup_rebuild_journal", cutoff).await?;
        let expired_dirty_days =
            count_before_day(&transaction, "observability_rollup_dirty_days", cutoff).await?;
        transaction
            .execute(
                "DELETE FROM observability_rollup_generations WHERE day_start_seconds < ?1",
                tracedecay_runtime_core::db::engine::params![cutoff],
            )
            .await
            .map_err(|error| format!("failed to expire observability rollup cells: {error}"))?;
        transaction
            .execute(
                "DELETE FROM observability_rollup_rebuild_journal WHERE day_start_seconds < ?1",
                tracedecay_runtime_core::db::engine::params![cutoff],
            )
            .await
            .map_err(|error| format!("failed to expire observability rollup journal: {error}"))?;
        transaction
            .execute(
                "DELETE FROM observability_rollup_dirty_days WHERE day_start_seconds < ?1",
                tracedecay_runtime_core::db::engine::params![cutoff],
            )
            .await
            .map_err(|error| format!("failed to expire observability dirty days: {error}"))?;
        transaction
            .commit()
            .await
            .map_err(|error| format!("failed to commit observability rollup retention: {error}"))?;
        Ok(ObservabilityRollupRetentionReceiptV1 {
            expired_generations,
            expired_journal_entries,
            expired_dirty_days,
        })
    }
}

async fn count_before_day(
    executor: &impl QueryExecutor,
    table: &str,
    cutoff: i64,
) -> Result<u64, String> {
    let mut rows = executor
        .query(
            &format!("SELECT COUNT(*) FROM {table} WHERE day_start_seconds < ?1"),
            tracedecay_runtime_core::db::engine::params![cutoff],
        )
        .await
        .map_err(|error| format!("failed to count expired observability rollups: {error}"))?;
    let row = rows
        .next()
        .await
        .map_err(|error| format!("failed to read expired observability rollups: {error}"))?
        .ok_or_else(|| "observability rollup retention count returned no row".to_owned())?;
    let value = row
        .get::<i64>(0)
        .map_err(|error| format!("failed to decode rollup retention count: {error}"))?;
    u64::try_from(value).map_err(|_| "rollup retention count was negative".to_owned())
}
