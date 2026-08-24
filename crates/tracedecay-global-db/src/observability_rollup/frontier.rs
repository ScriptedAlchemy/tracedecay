use tracedecay_runtime_core::db::engine::{Executor, QueryExecutor};

use crate::RegisteredGlobalDb;

use super::{
    ObservabilityRollupEmptyDayClaimOutcomeV1, ObservabilityRollupEmptyDayClaimV1,
    ObservabilityRollupFrontierV1, SECONDS_PER_DAY, validate_day, validate_identifier,
};

impl RegisteredGlobalDb {
    /// Records the first full UTC day this mounted scope can truthfully cover.
    /// Repeated mounts return the original boundary unchanged.
    pub async fn initialize_observability_rollup_frontier(
        &self,
        authorized_scope_ref: &str,
    ) -> Result<ObservabilityRollupFrontierV1, String> {
        validate_identifier("scope", authorized_scope_ref)?;
        let transaction = self.begin_write_transaction().await.map_err(|error| {
            format!("failed to begin empty-day frontier initialization: {error}")
        })?;
        transaction
            .execute(
                "INSERT OR IGNORE INTO observability_rollup_frontiers
                     (scope_ref, coverage_start_day_seconds, next_day_start_seconds)
                 VALUES (?1,
                         ((unixepoch() / 86400) + 1) * 86400,
                         ((unixepoch() / 86400) + 1) * 86400)",
                tracedecay_runtime_core::db::engine::params![authorized_scope_ref],
            )
            .await
            .map_err(|error| format!("failed to initialize empty-day frontier: {error}"))?;
        let mut rows = transaction
            .query(
                "SELECT coverage_start_day_seconds, next_day_start_seconds
                 FROM observability_rollup_frontiers WHERE scope_ref = ?1",
                tracedecay_runtime_core::db::engine::params![authorized_scope_ref],
            )
            .await
            .map_err(|error| {
                format!("failed to inspect initialized empty-day frontier: {error}")
            })?;
        let row = rows
            .next()
            .await
            .map_err(|error| format!("failed to read initialized empty-day frontier: {error}"))?
            .ok_or_else(|| "empty-day frontier initialization returned no row".to_owned())?;
        let frontier = ObservabilityRollupFrontierV1 {
            coverage_start_day_seconds: row
                .get(0)
                .map_err(|error| format!("failed to decode coverage start day: {error}"))?,
            next_day_start_seconds: row
                .get(1)
                .map_err(|error| format!("failed to decode next empty day: {error}"))?,
        };
        drop(rows);
        transaction.commit().await.map_err(|error| {
            format!("failed to commit empty-day frontier initialization: {error}")
        })?;
        Ok(frontier)
    }

    /// Claims at most one completed quiet UTC day at or after this scope's
    /// first observable day. First mount starts at tomorrow's boundary, so
    /// startup never fabricates coverage for the partial current day or any
    /// pre-installation history.
    pub async fn claim_observability_rollup_empty_day(
        &self,
        authorized_scope_ref: &str,
        claimant_id: &str,
        lease_seconds: u32,
    ) -> Result<ObservabilityRollupEmptyDayClaimOutcomeV1, String> {
        validate_identifier("scope", authorized_scope_ref)?;
        validate_identifier("claimant", claimant_id)?;
        if lease_seconds == 0 || lease_seconds > 300 {
            return Err("invalid observability rollup empty-day lease".to_owned());
        }
        let transaction = self
            .begin_write_transaction()
            .await
            .map_err(|error| format!("failed to begin empty-day frontier claim: {error}"))?;
        transaction
            .execute(
                "INSERT OR IGNORE INTO observability_rollup_frontiers
                     (scope_ref, coverage_start_day_seconds, next_day_start_seconds)
                 VALUES (?1,
                         ((unixepoch() / 86400) + 1) * 86400,
                         ((unixepoch() / 86400) + 1) * 86400)",
                tracedecay_runtime_core::db::engine::params![authorized_scope_ref],
            )
            .await
            .map_err(|error| format!("failed to initialize empty-day frontier: {error}"))?;
        let mut rows = transaction
            .query(
                "SELECT coverage_start_day_seconds, next_day_start_seconds,
                        claimant_id, lease_until_seconds,
                        (unixepoch() / 86400) * 86400, unixepoch()
                 FROM observability_rollup_frontiers WHERE scope_ref = ?1",
                tracedecay_runtime_core::db::engine::params![authorized_scope_ref],
            )
            .await
            .map_err(|error| format!("failed to inspect empty-day frontier: {error}"))?;
        let row = rows
            .next()
            .await
            .map_err(|error| format!("failed to read empty-day frontier: {error}"))?
            .ok_or_else(|| "empty-day frontier initialization returned no row".to_owned())?;
        let coverage_start_day_seconds = row
            .get::<i64>(0)
            .map_err(|error| format!("failed to decode coverage start day: {error}"))?;
        let next_day_start_seconds = row
            .get::<i64>(1)
            .map_err(|error| format!("failed to decode next empty day: {error}"))?;
        let active_claimant = row
            .get::<Option<String>>(2)
            .map_err(|error| format!("failed to decode empty-day claimant: {error}"))?;
        let lease_until_seconds = row
            .get::<Option<i64>>(3)
            .map_err(|error| format!("failed to decode empty-day lease: {error}"))?;
        let current_day_start_seconds = row
            .get::<i64>(4)
            .map_err(|error| format!("failed to decode current UTC day: {error}"))?;
        let now_seconds = row
            .get::<i64>(5)
            .map_err(|error| format!("failed to decode current time: {error}"))?;
        drop(rows);

        if next_day_start_seconds >= current_day_start_seconds {
            transaction.commit().await.map_err(|error| {
                format!("failed to close not-ready empty-day frontier: {error}")
            })?;
            return Ok(ObservabilityRollupEmptyDayClaimOutcomeV1::NotReady {
                coverage_start_day_seconds,
                next_day_start_seconds,
            });
        }
        if active_claimant.is_some() && lease_until_seconds.is_some_and(|lease| lease > now_seconds)
        {
            transaction
                .commit()
                .await
                .map_err(|error| format!("failed to close leased empty-day frontier: {error}"))?;
            return Ok(ObservabilityRollupEmptyDayClaimOutcomeV1::Leased {
                day_start_seconds: next_day_start_seconds,
            });
        }
        if day_is_dirty(&transaction, authorized_scope_ref, next_day_start_seconds).await? {
            transaction
                .commit()
                .await
                .map_err(|error| format!("failed to close dirty empty-day frontier: {error}"))?;
            return Ok(ObservabilityRollupEmptyDayClaimOutcomeV1::DirtyDay {
                day_start_seconds: next_day_start_seconds,
            });
        }
        if day_has_generation(&transaction, authorized_scope_ref, next_day_start_seconds).await? {
            let following_day = next_day_start_seconds
                .checked_add(SECONDS_PER_DAY)
                .ok_or_else(|| "empty-day frontier overflow".to_owned())?;
            transaction
                .execute(
                    "UPDATE observability_rollup_frontiers
                     SET next_day_start_seconds = ?2,
                         claimant_id = NULL, lease_until_seconds = NULL
                     WHERE scope_ref = ?1 AND next_day_start_seconds = ?3",
                    tracedecay_runtime_core::db::engine::params![
                        authorized_scope_ref,
                        following_day,
                        next_day_start_seconds
                    ],
                )
                .await
                .map_err(|error| format!("failed to advance existing-day frontier: {error}"))?;
            transaction.commit().await.map_err(|error| {
                format!("failed to commit existing-day frontier advance: {error}")
            })?;
            return Ok(
                ObservabilityRollupEmptyDayClaimOutcomeV1::AdvancedExisting {
                    day_start_seconds: next_day_start_seconds,
                },
            );
        }

        let mut claimed = transaction
            .query(
                "UPDATE observability_rollup_frontiers
                 SET claimant_id = ?2, lease_until_seconds = unixepoch() + ?3
                 WHERE scope_ref = ?1 AND next_day_start_seconds = ?4
                   AND (claimant_id IS NULL OR lease_until_seconds <= unixepoch())
                 RETURNING lease_until_seconds",
                tracedecay_runtime_core::db::engine::params![
                    authorized_scope_ref,
                    claimant_id,
                    i64::from(lease_seconds),
                    next_day_start_seconds
                ],
            )
            .await
            .map_err(|error| format!("failed to claim empty-day frontier: {error}"))?;
        let lease_until_seconds = claimed
            .next()
            .await
            .map_err(|error| format!("failed to read empty-day claim: {error}"))?
            .ok_or_else(|| "empty-day frontier claim changed concurrently".to_owned())?
            .get::<i64>(0)
            .map_err(|error| format!("failed to decode claimed empty-day lease: {error}"))?;
        drop(claimed);
        transaction
            .commit()
            .await
            .map_err(|error| format!("failed to commit empty-day claim: {error}"))?;
        Ok(ObservabilityRollupEmptyDayClaimOutcomeV1::Claimed(
            ObservabilityRollupEmptyDayClaimV1 {
                authorized_scope_ref: authorized_scope_ref.to_owned(),
                day_start_seconds: next_day_start_seconds,
                claimant_id: claimant_id.to_owned(),
                lease_until_seconds,
            },
        ))
    }

    pub async fn release_observability_rollup_empty_day(
        &self,
        claim: &ObservabilityRollupEmptyDayClaimV1,
    ) -> Result<bool, String> {
        validate_empty_day_claim(claim)?;
        let transaction = self
            .begin_write_transaction()
            .await
            .map_err(|error| format!("failed to begin empty-day release: {error}"))?;
        let changed = transaction
            .execute(
                "UPDATE observability_rollup_frontiers
                 SET claimant_id = NULL, lease_until_seconds = NULL
                 WHERE scope_ref = ?1 AND next_day_start_seconds = ?2
                   AND claimant_id = ?3 AND lease_until_seconds = ?4",
                tracedecay_runtime_core::db::engine::params![
                    claim.authorized_scope_ref.as_str(),
                    claim.day_start_seconds,
                    claim.claimant_id.as_str(),
                    claim.lease_until_seconds
                ],
            )
            .await
            .map_err(|error| format!("failed to release empty-day claim: {error}"))?;
        transaction
            .commit()
            .await
            .map_err(|error| format!("failed to commit empty-day release: {error}"))?;
        Ok(changed == 1)
    }
}

pub(super) fn validate_empty_day_claim(
    claim: &ObservabilityRollupEmptyDayClaimV1,
) -> Result<(), String> {
    validate_identifier("scope", &claim.authorized_scope_ref)?;
    validate_identifier("claimant", &claim.claimant_id)?;
    validate_day(claim.day_start_seconds)?;
    if claim.lease_until_seconds < 0 {
        return Err("invalid observability rollup empty-day claim".to_owned());
    }
    Ok(())
}

pub(super) async fn empty_day_claim_is_current(
    executor: &impl QueryExecutor,
    claim: &ObservabilityRollupEmptyDayClaimV1,
) -> Result<bool, String> {
    let mut rows = executor
        .query(
            "SELECT 1 FROM observability_rollup_frontiers AS frontier
             WHERE frontier.scope_ref = ?1 AND frontier.next_day_start_seconds = ?2
               AND frontier.claimant_id = ?3 AND frontier.lease_until_seconds = ?4
               AND frontier.lease_until_seconds > unixepoch()
               AND NOT EXISTS (
                   SELECT 1 FROM observability_rollup_dirty_days AS dirty
                   WHERE dirty.scope_ref = frontier.scope_ref
                     AND dirty.day_start_seconds = frontier.next_day_start_seconds
               )
             LIMIT 1",
            tracedecay_runtime_core::db::engine::params![
                claim.authorized_scope_ref.as_str(),
                claim.day_start_seconds,
                claim.claimant_id.as_str(),
                claim.lease_until_seconds
            ],
        )
        .await
        .map_err(|error| format!("failed to verify empty-day claim: {error}"))?;
    rows.next()
        .await
        .map(|row| row.is_some())
        .map_err(|error| format!("failed to read empty-day claim: {error}"))
}

pub(super) async fn settle_empty_day_claim(
    executor: &impl Executor,
    claim: &ObservabilityRollupEmptyDayClaimV1,
) -> Result<bool, String> {
    let next_day = claim
        .day_start_seconds
        .checked_add(SECONDS_PER_DAY)
        .ok_or_else(|| "observability rollup empty-day frontier overflow".to_owned())?;
    executor
        .execute(
            "UPDATE observability_rollup_frontiers
             SET next_day_start_seconds = ?3,
                 claimant_id = NULL, lease_until_seconds = NULL
             WHERE scope_ref = ?1 AND next_day_start_seconds = ?2
               AND claimant_id = ?4 AND lease_until_seconds = ?5",
            tracedecay_runtime_core::db::engine::params![
                claim.authorized_scope_ref.as_str(),
                claim.day_start_seconds,
                next_day,
                claim.claimant_id.as_str(),
                claim.lease_until_seconds
            ],
        )
        .await
        .map(|changed| changed == 1)
        .map_err(|error| format!("failed to advance empty-day frontier: {error}"))
}

async fn day_is_dirty(
    executor: &impl QueryExecutor,
    scope_ref: &str,
    day_start_seconds: i64,
) -> Result<bool, String> {
    row_exists(
        executor,
        "SELECT 1 FROM observability_rollup_dirty_days
         WHERE scope_ref = ?1 AND day_start_seconds = ?2 LIMIT 1",
        scope_ref,
        day_start_seconds,
        "dirty empty-day frontier",
    )
    .await
}

async fn day_has_generation(
    executor: &impl QueryExecutor,
    scope_ref: &str,
    day_start_seconds: i64,
) -> Result<bool, String> {
    row_exists(
        executor,
        "SELECT 1 FROM observability_rollup_generations
         WHERE scope_ref = ?1 AND day_start_seconds = ?2 LIMIT 1",
        scope_ref,
        day_start_seconds,
        "existing rollup generation",
    )
    .await
}

async fn row_exists(
    executor: &impl QueryExecutor,
    sql: &str,
    scope_ref: &str,
    day_start_seconds: i64,
    operation: &str,
) -> Result<bool, String> {
    let mut rows = executor
        .query(
            sql,
            tracedecay_runtime_core::db::engine::params![scope_ref, day_start_seconds],
        )
        .await
        .map_err(|error| format!("failed to inspect {operation}: {error}"))?;
    rows.next()
        .await
        .map(|row| row.is_some())
        .map_err(|error| format!("failed to read {operation}: {error}"))
}
