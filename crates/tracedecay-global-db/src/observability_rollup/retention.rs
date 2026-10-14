use crate::RegisteredGlobalDbWriteTransaction;

use super::{
    OBSERVABILITY_ROLLUP_RETENTION_DAYS_V1, ObservabilityRollupRetentionReceiptV1, SECONDS_PER_DAY,
};

/// Expires daily rollup generations, their rebuild journal, and dirty-day
/// markers older than the rollup horizon inside the caller's retention
/// transaction, and moves any frontier still behind the horizon to the first
/// retained day. Without that, a scope idle past the horizon would close each
/// expired day as known-empty from already-pruned events. A query reaching
/// past the horizon reports partial coverage.
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
    // The day containing the cutoff lost events before it, so it is expired too.
    let cutoff_day = cutoff.div_euclid(SECONDS_PER_DAY) * SECONDS_PER_DAY;
    let first_retained_day = if cutoff_day < cutoff {
        cutoff_day + SECONDS_PER_DAY
    } else {
        cutoff_day
    };
    let advanced_frontiers = transaction
        .execute(
            "UPDATE observability_rollup_frontiers
             SET next_day_start_seconds = ?1, claimant_id = NULL, lease_until_seconds = NULL
             WHERE next_day_start_seconds < ?1",
            tracedecay_runtime_core::db::engine::params![first_retained_day],
        )
        .await
        .map_err(|error| format!("failed to advance expired observability frontiers: {error}"))?;
    Ok(ObservabilityRollupRetentionReceiptV1 {
        expired_generations,
        expired_journal_entries,
        expired_dirty_days,
        advanced_frontiers,
    })
}
