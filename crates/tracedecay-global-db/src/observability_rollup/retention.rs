use crate::RegisteredGlobalDbWriteTransaction;

use super::{
    OBSERVABILITY_ROLLUP_RETENTION_DAYS_V1, ObservabilityRollupRetentionReceiptV1, SECONDS_PER_DAY,
};

/// Expires daily rollup generations, their rebuild journal, and dirty-day
/// markers older than the rollup horizon inside the caller's retention
/// transaction. The frontier only advances, so an expired day is never
/// re-closed; a query reaching past the horizon reports partial coverage.
pub(crate) async fn expire_observability_rollups(
    transaction: &RegisteredGlobalDbWriteTransaction<'_>,
    now_seconds: i64,
) -> Result<ObservabilityRollupRetentionReceiptV1, String> {
    let cutoff =
        now_seconds.saturating_sub(OBSERVABILITY_ROLLUP_RETENTION_DAYS_V1 * SECONDS_PER_DAY);
    let expired_generations = transaction
        .execute(
            "DELETE FROM observability_rollup_generations WHERE day_start_seconds < ?1",
            tracedecay_runtime_core::db::engine::params![cutoff],
        )
        .await
        .map_err(|error| format!("failed to expire observability rollup cells: {error}"))?;
    let expired_journal_entries = transaction
        .execute(
            "DELETE FROM observability_rollup_rebuild_journal WHERE day_start_seconds < ?1",
            tracedecay_runtime_core::db::engine::params![cutoff],
        )
        .await
        .map_err(|error| format!("failed to expire observability rollup journal: {error}"))?;
    let expired_dirty_days = transaction
        .execute(
            "DELETE FROM observability_rollup_dirty_days WHERE day_start_seconds < ?1",
            tracedecay_runtime_core::db::engine::params![cutoff],
        )
        .await
        .map_err(|error| format!("failed to expire observability dirty days: {error}"))?;
    Ok(ObservabilityRollupRetentionReceiptV1 {
        expired_generations,
        expired_journal_entries,
        expired_dirty_days,
    })
}
