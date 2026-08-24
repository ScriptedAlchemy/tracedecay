use tracedecay_runtime_core::db::engine::QueryExecutor;

use crate::RegisteredGlobalDb;

use super::{ObservabilityRollupDirtyDayClaimV1, validate_day, validate_identifier};

impl RegisteredGlobalDb {
    /// Leases the oldest dirty execution-topology day for one exact scope.
    /// At most one bounded day is returned; expired leases are retryable and
    /// a later accepted source event revokes the lease atomically.
    pub async fn claim_observability_rollup_dirty_day(
        &self,
        authorized_scope_ref: &str,
        claimant_id: &str,
        lease_seconds: u32,
    ) -> Result<Option<ObservabilityRollupDirtyDayClaimV1>, String> {
        validate_identifier("scope", authorized_scope_ref)?;
        validate_identifier("claimant", claimant_id)?;
        if lease_seconds == 0 || lease_seconds > 300 {
            return Err("invalid observability rollup dirty-day lease".to_owned());
        }
        let transaction = self
            .begin_write_transaction()
            .await
            .map_err(|error| format!("failed to begin observability dirty-day claim: {error}"))?;
        let mut rows = transaction
            .query(
                "UPDATE observability_rollup_dirty_days
                 SET claimant_id = ?2, lease_until_seconds = unixepoch() + ?3
                 WHERE rowid = (
                    SELECT rowid FROM observability_rollup_dirty_days
                    WHERE scope_ref = ?1
                      AND (claimant_id IS NULL OR lease_until_seconds <= unixepoch())
                    ORDER BY day_start_seconds
                    LIMIT 1
                 )
                 RETURNING scope_ref, day_start_seconds, source_watermark,
                           claimant_id, lease_until_seconds",
                tracedecay_runtime_core::db::engine::params![
                    authorized_scope_ref,
                    claimant_id,
                    i64::from(lease_seconds)
                ],
            )
            .await
            .map_err(|error| format!("failed to claim observability dirty day: {error}"))?;
        let claim = match rows
            .next()
            .await
            .map_err(|error| format!("failed to read observability dirty-day claim: {error}"))?
        {
            Some(row) => Some(ObservabilityRollupDirtyDayClaimV1 {
                authorized_scope_ref: row
                    .get(0)
                    .map_err(|error| format!("failed to decode dirty-day scope: {error}"))?,
                day_start_seconds: row
                    .get(1)
                    .map_err(|error| format!("failed to decode dirty-day bucket: {error}"))?,
                source_watermark: row
                    .get(2)
                    .map_err(|error| format!("failed to decode dirty-day watermark: {error}"))?,
                claimant_id: row
                    .get(3)
                    .map_err(|error| format!("failed to decode dirty-day claimant: {error}"))?,
                lease_until_seconds: row
                    .get(4)
                    .map_err(|error| format!("failed to decode dirty-day lease: {error}"))?,
            }),
            None => None,
        };
        drop(rows);
        transaction
            .commit()
            .await
            .map_err(|error| format!("failed to commit observability dirty-day claim: {error}"))?;
        Ok(claim)
    }

    /// Releases one exact lease after a bounded rebuild attempt could not
    /// produce a complete fragment. The dirty marker and watermark remain.
    pub async fn release_observability_rollup_dirty_day(
        &self,
        claim: &ObservabilityRollupDirtyDayClaimV1,
    ) -> Result<bool, String> {
        validate_dirty_claim(claim)?;
        let transaction = self
            .begin_write_transaction()
            .await
            .map_err(|error| format!("failed to begin observability dirty-day release: {error}"))?;
        let changed = transaction
            .execute(
                "UPDATE observability_rollup_dirty_days
                 SET claimant_id = NULL, lease_until_seconds = NULL
                 WHERE scope_ref = ?1 AND day_start_seconds = ?2
                   AND source_watermark = ?3 AND claimant_id = ?4
                   AND lease_until_seconds = ?5",
                tracedecay_runtime_core::db::engine::params![
                    claim.authorized_scope_ref.as_str(),
                    claim.day_start_seconds,
                    claim.source_watermark,
                    claim.claimant_id.as_str(),
                    claim.lease_until_seconds
                ],
            )
            .await
            .map_err(|error| format!("failed to release observability dirty day: {error}"))?;
        transaction.commit().await.map_err(|error| {
            format!("failed to commit observability dirty-day release: {error}")
        })?;
        Ok(changed == 1)
    }
}

pub(super) fn validate_dirty_claim(
    claim: &ObservabilityRollupDirtyDayClaimV1,
) -> Result<(), String> {
    validate_identifier("scope", &claim.authorized_scope_ref)?;
    validate_identifier("claimant", &claim.claimant_id)?;
    validate_day(claim.day_start_seconds)?;
    if claim.source_watermark <= 0 || claim.lease_until_seconds < 0 {
        return Err("invalid observability rollup dirty-day claim".to_owned());
    }
    Ok(())
}

pub(super) async fn dirty_claim_is_current(
    executor: &impl QueryExecutor,
    claim: &ObservabilityRollupDirtyDayClaimV1,
) -> Result<bool, String> {
    let mut rows = executor
        .query(
            "SELECT 1 FROM observability_rollup_dirty_days
             WHERE scope_ref = ?1 AND day_start_seconds = ?2
               AND source_watermark = ?3 AND claimant_id = ?4
               AND lease_until_seconds = ?5
               AND lease_until_seconds > unixepoch()
             LIMIT 1",
            tracedecay_runtime_core::db::engine::params![
                claim.authorized_scope_ref.as_str(),
                claim.day_start_seconds,
                claim.source_watermark,
                claim.claimant_id.as_str(),
                claim.lease_until_seconds
            ],
        )
        .await
        .map_err(|error| format!("failed to verify observability dirty-day claim: {error}"))?;
    rows.next()
        .await
        .map(|row| row.is_some())
        .map_err(|error| format!("failed to read observability dirty-day claim: {error}"))
}

pub(super) async fn range_has_dirty_day(
    executor: &impl QueryExecutor,
    authorized_scope_ref: &str,
    since_day_start_seconds: i64,
    until_day_start_seconds: i64,
) -> Result<bool, String> {
    let mut rows = executor
        .query(
            "SELECT 1 FROM observability_rollup_dirty_days
             WHERE scope_ref = ?1 AND day_start_seconds >= ?2 AND day_start_seconds < ?3
             LIMIT 1",
            tracedecay_runtime_core::db::engine::params![
                authorized_scope_ref,
                since_day_start_seconds,
                until_day_start_seconds
            ],
        )
        .await
        .map_err(|error| format!("failed to inspect dirty observability rollups: {error}"))?;
    rows.next()
        .await
        .map(|row| row.is_some())
        .map_err(|error| format!("failed to read dirty observability rollup: {error}"))
}
