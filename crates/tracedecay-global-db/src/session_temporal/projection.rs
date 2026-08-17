use std::sync::Arc;

use tracedecay_domain::SessionId;
use tracedecay_graph_db::NeverCancelled;
use tracedecay_runtime_core::db::engine::params;
use tracedecay_store::{
    SessionRefreshBeginOrJoinRequestV1, SessionRefreshFrontierV1, SessionRefreshProgressV1,
    SessionStoreResult, SessionTemporalProjectionBatchReceiptV1, SessionTemporalProjectionBatchV1,
};

use super::super::RegisteredGlobalDb;
use super::query::{PERSIST_OPERATION, storage, storage_message};
use super::refresh::{SessionRefreshRecoveryV1, SessionRefreshRestartStateV1};
use super::relations::SessionRelationError;

mod derived;
mod materialize;
mod persist;
mod receipts;
#[cfg(test)]
mod tests;

use materialize::materialize_session_temporal_refresh_batch_in_transaction;

pub(super) use materialize::canonical_parent_message_resolver;
pub(super) use persist::{
    persist_session_temporal_projection_batch_in_transaction,
    seed_active_projection_in_transaction, session_temporal_projection_record_count,
};
pub use receipts::record_canonical_observation_effect;
pub(in crate::session_temporal) use receipts::digest_bytes;
pub(super) use receipts::validate_final_projection_receipt;

const DISCOVER_REFRESH: &str = "discover session temporal refresh";
const MATERIALIZE_REFRESH: &str = "materialize session temporal refresh";
const MAX_BASELINE_RELATION_ITEMS: usize = 100_000;

impl RegisteredGlobalDb {
    pub async fn pending_session_temporal_refresh_requests_result(
        &self,
        limit: usize,
    ) -> SessionStoreResult<Vec<SessionRefreshBeginOrJoinRequestV1>> {
        let snapshot = self
            .read_snapshot()
            .await
            .map_err(|error| storage(DISCOVER_REFRESH, error))?;
        let limit = i64::try_from(limit).map_err(|error| storage(DISCOVER_REFRESH, error))?;
        let mut rows = snapshot
            .query(
                "WITH active AS (
                    SELECT session_id, frozen_watermarks_json
                    FROM session_temporal_generations
                    WHERE state = 'active'
                 ),
                 running AS (
                    SELECT session_id
                    FROM session_refresh_operations
                    WHERE state = 'running'
                 )
                 SELECT effect.session_id,
                        MAX(effect.observation_sequence),
                        COALESCE(
                            CAST(json_extract(
                                active.frozen_watermarks_json,
                                '$.projection_frontier'
                            ) AS INTEGER),
                            0
                        )
                 FROM session_temporal_observation_effects AS effect
                 LEFT JOIN active ON active.session_id = effect.session_id
                 LEFT JOIN running ON running.session_id = effect.session_id
                 WHERE running.session_id IS NULL
                 GROUP BY effect.session_id
                 HAVING MAX(CASE
                     WHEN effect.output_count > 0 THEN effect.observation_sequence
                     ELSE NULL
                 END) > COALESCE(
                    CAST(json_extract(
                        active.frozen_watermarks_json,
                        '$.projection_frontier'
                    ) AS INTEGER),
                    0
                )
                 ORDER BY effect.session_id
                 LIMIT ?1",
                params![limit],
            )
            .await
            .map_err(|error| storage(DISCOVER_REFRESH, error))?;
        let mut requests = Vec::new();
        while let Some(row) = rows
            .next()
            .await
            .map_err(|error| storage(DISCOVER_REFRESH, error))?
        {
            let session_id = SessionId::new(
                row.get::<String>(0)
                    .map_err(|error| storage(DISCOVER_REFRESH, error))?,
            )
            .map_err(|error| storage(DISCOVER_REFRESH, error))?;
            let observed_through = u64::try_from(
                row.get::<i64>(1)
                    .map_err(|error| storage(DISCOVER_REFRESH, error))?,
            )
            .map_err(|error| storage(DISCOVER_REFRESH, error))?;
            let committed_through = u64::try_from(
                row.get::<i64>(2)
                    .map_err(|error| storage(DISCOVER_REFRESH, error))?,
            )
            .map_err(|error| storage(DISCOVER_REFRESH, error))?;
            requests.push(SessionRefreshBeginOrJoinRequestV1::new(
                session_id,
                SessionRefreshFrontierV1::new(observed_through, committed_through)?,
            ));
        }
        Ok(requests)
    }

    pub async fn materialize_session_temporal_refresh_batch_result(
        &self,
        recovery: &SessionRefreshRecoveryV1,
    ) -> SessionStoreResult<Option<(SessionRefreshProgressV1, SessionTemporalProjectionBatchV1)>>
    {
        let snapshot = self
            .read_snapshot()
            .await
            .map_err(|error| storage(MATERIALIZE_REFRESH, error))?;
        let baseline_copy_count =
            if recovery.restart_state() == SessionRefreshRestartStateV1::BeginProjection {
                let (scope, relation_store) = self
                    .session_relation_store()
                    .map_err(|error| storage(MATERIALIZE_REFRESH, error))?;
                match relation_store.load_projection(
                    scope,
                    recovery.session_id(),
                    recovery.frozen_watermarks().active_generation().value(),
                    MAX_BASELINE_RELATION_ITEMS,
                    MAX_BASELINE_RELATION_ITEMS,
                    Arc::new(NeverCancelled),
                ) {
                    Ok(projection) => u64::try_from(projection.logical_copies.len())
                        .map_err(|error| storage(MATERIALIZE_REFRESH, error))?,
                    Err(SessionRelationError::NotFound) => {
                        let mut rows = snapshot
                            .query(
                                "SELECT COUNT(*)
                             FROM session_occurrences
                             WHERE session_id = ?1 AND generation = ?2",
                                params![
                                    recovery.session_id().as_str(),
                                    i64::try_from(
                                        recovery.frozen_watermarks().active_generation().value()
                                    )
                                    .map_err(|error| storage(MATERIALIZE_REFRESH, error))?,
                                ],
                            )
                            .await
                            .map_err(|error| storage(MATERIALIZE_REFRESH, error))?;
                        let retained: i64 = rows
                            .next()
                            .await
                            .map_err(|error| storage(MATERIALIZE_REFRESH, error))?
                            .ok_or_else(|| {
                                storage_message(
                                    MATERIALIZE_REFRESH,
                                    "active projection count returned no row",
                                )
                            })?
                            .get(0)
                            .map_err(|error| storage(MATERIALIZE_REFRESH, error))?;
                        if retained == 0 {
                            0
                        } else {
                            return Err(storage_message(
                                MATERIALIZE_REFRESH,
                                "active native relation projection is unavailable",
                            ));
                        }
                    }
                    Err(error) => return Err(storage(MATERIALIZE_REFRESH, error)),
                }
            } else {
                0
            };
        materialize_session_temporal_refresh_batch_in_transaction(
            &snapshot,
            recovery,
            baseline_copy_count,
        )
        .await
    }

    pub async fn persist_session_temporal_projection_batch_result(
        &self,
        batch: SessionTemporalProjectionBatchV1,
    ) -> SessionStoreResult<SessionTemporalProjectionBatchReceiptV1> {
        let transaction = self
            .begin_write_transaction()
            .await
            .map_err(|error| storage(PERSIST_OPERATION, error))?;
        let receipt =
            persist_session_temporal_projection_batch_in_transaction(&transaction, &batch).await?;
        transaction
            .commit()
            .await
            .map_err(|error| storage(PERSIST_OPERATION, error))?;
        Ok(receipt)
    }
}
