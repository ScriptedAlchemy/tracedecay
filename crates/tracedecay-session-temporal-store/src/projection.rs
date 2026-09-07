use std::{collections::BTreeMap, sync::Arc};

use tracedecay_domain::SessionId;
use tracedecay_graph_db::NeverCancelled;
use tracedecay_runtime_core::db::engine::params;
use tracedecay_store::{
    SessionRefreshBeginOrJoinRequestV1, SessionRefreshFrontierV1, SessionRefreshProgressV1,
    SessionStoreResult, SessionTemporalProjectionBatchReceiptV1, SessionTemporalProjectionBatchV1,
};
use tracedecay_temporal_query::ports::ExecutionControl;

use super::query::{PERSIST_OPERATION, storage, storage_message};
use super::refresh::{SessionRefreshRecoveryV1, SessionRefreshRestartStateV1};
use super::relations::SessionRelationError;
use crate::handle::{SessionTemporalAccess, SessionTemporalRegisteredDb, SessionTemporalWriteTxn};
use crate::support as hotpath_observe;

mod derived;
mod materialize;
mod persist;
mod receipts;
#[cfg(test)]
mod tests;

use materialize::materialize_session_temporal_refresh_batch_in_transaction;

pub(super) use materialize::canonical_parent_message_resolver;
pub(crate) use persist::observation_envelope_from_payload;
pub(super) use persist::{
    ProjectionProgressBaseline, persist_session_temporal_projection_batch_in_transaction,
    seed_active_projection_in_transaction, session_temporal_projection_record_count,
};
pub(crate) use receipts::digest_bytes;
pub use receipts::record_canonical_observation_effect;
pub(super) use receipts::validate_final_projection_receipt;

const DISCOVER_REFRESH: &str = "discover session temporal refresh";
const MATERIALIZE_REFRESH: &str = "materialize session temporal refresh";
const MAX_BASELINE_RELATION_ITEMS: usize = 100_000;

pub struct SessionTemporalRefreshDiscoveryPage {
    requests: Vec<SessionRefreshBeginOrJoinRequestV1>,
    active_scanned_through: Option<SessionId>,
    active_exhausted: bool,
    active_rows_scanned: usize,
    pending_has_more: bool,
}

impl SessionTemporalRefreshDiscoveryPage {
    pub fn active_rows_scanned(&self) -> usize {
        self.active_rows_scanned
    }

    pub fn into_parts(
        self,
    ) -> (
        Vec<SessionRefreshBeginOrJoinRequestV1>,
        Option<SessionId>,
        bool,
    ) {
        let active_after = if self.active_exhausted {
            None
        } else {
            self.active_scanned_through
        };
        (
            self.requests,
            active_after,
            self.pending_has_more || !self.active_exhausted,
        )
    }
}

impl<D: SessionTemporalRegisteredDb + Sync> SessionTemporalAccess<'_, D> {
    /// Discovers sessions that need temporal projection.
    ///
    #[hotpath::measure(future = true, label = "session_temporal.query.pending_refresh")]
    pub async fn pending_session_temporal_refresh_page_result(
        &self,
        limit: usize,
        active_scan_slots: usize,
        active_after: Option<&SessionId>,
    ) -> SessionStoreResult<SessionTemporalRefreshDiscoveryPage> {
        let snapshot = self
            .read_snapshot()
            .await
            .map_err(|error| storage(DISCOVER_REFRESH, error))?;
        hotpath_observe::record_snapshot_admissions(1);
        if limit == 0 {
            return Ok(SessionTemporalRefreshDiscoveryPage {
                requests: Vec::new(),
                active_scanned_through: active_after.cloned(),
                active_exhausted: true,
                active_rows_scanned: 0,
                pending_has_more: false,
            });
        }
        let active_scan_slots = active_scan_slots.min(limit);
        let pending_limit = limit.saturating_sub(active_scan_slots);
        let query_limit = pending_limit.saturating_add(1);
        let query_limit =
            i64::try_from(query_limit).map_err(|error| storage(DISCOVER_REFRESH, error))?;
        // Visit only output-producing effects past each session's projection
        // frontier. History-only (`output_count = 0`) rows never enter grouping.
        // An active generation created before native relation publication was
        // authoritative may already cover its source frontier but have no
        // relation receipt. Rediscover that exact committed frontier so the
        // ordinary refresh path rebuilds and verifies it; discovery itself
        // never fabricates a receipt or mutates the projection.
        let mut rows = snapshot
            .query(
                "SELECT effect.session_id,
                        MAX(effect.observation_sequence) AS observed_through,
                        COALESCE(active.projection_frontier, 0) AS committed_through
                 FROM session_temporal_observation_effects AS effect
                 LEFT JOIN (
                     SELECT session_id,
                            CAST(json_extract(
                                frozen_watermarks_json,
                                '$.projection_frontier'
                            ) AS INTEGER) AS projection_frontier
                     FROM session_temporal_generations
                     WHERE state = 'active'
                 ) AS active ON active.session_id = effect.session_id
                 WHERE NOT EXISTS (
                     SELECT 1
                     FROM session_refresh_operations AS running
                     WHERE running.session_id = effect.session_id
                       AND running.state = 'running'
                 )
                   AND effect.output_count > 0
                   AND effect.observation_sequence >
                       COALESCE(active.projection_frontier, 0)
                 GROUP BY effect.session_id
                 ORDER BY effect.session_id
                 LIMIT ?1",
                params![query_limit],
            )
            .await
            .map_err(|error| storage(DISCOVER_REFRESH, error))?;
        let mut requests = BTreeMap::new();
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
            requests.insert(
                session_id.as_str().to_owned(),
                SessionRefreshBeginOrJoinRequestV1::new(
                    session_id,
                    SessionRefreshFrontierV1::new(observed_through, committed_through)?,
                ),
            );
        }
        drop(rows);
        let pending_has_more = requests.len() > pending_limit;
        while requests.len() > pending_limit {
            requests.pop_last();
        }

        let mut active_scanned_through = active_after.cloned();
        let mut active_exhausted = false;
        let mut active_rows_scanned = 0;
        if active_scan_slots > 0 {
            let active_limit = i64::try_from(active_scan_slots)
                .map_err(|error| storage(DISCOVER_REFRESH, error))?;
            let mut active_rows = snapshot
                .query(
                    "SELECT active.session_id,
                            CAST(json_extract(
                                active.frozen_watermarks_json,
                                '$.projection_frontier'
                            ) AS INTEGER),
                            EXISTS (
                                SELECT 1
                                FROM session_relation_receipts AS receipt
                                WHERE receipt.session_id = active.session_id
                                  AND receipt.generation = active.generation
                            ),
                            EXISTS (
                                SELECT 1
                                FROM session_refresh_operations AS running
                                WHERE running.session_id = active.session_id
                                  AND running.state = 'running'
                            )
                     FROM session_temporal_generations AS active
                     WHERE active.state = 'active'
                       AND (?1 IS NULL OR active.session_id > ?1)
                     ORDER BY active.session_id
                     LIMIT ?2",
                    params![active_after.map(SessionId::as_str), active_limit],
                )
                .await
                .map_err(|error| storage(DISCOVER_REFRESH, error))?;
            let mut scanned = Vec::new();
            while let Some(row) = active_rows
                .next()
                .await
                .map_err(|error| storage(DISCOVER_REFRESH, error))?
            {
                let session_id = SessionId::new(
                    row.get::<String>(0)
                        .map_err(|error| storage(DISCOVER_REFRESH, error))?,
                )
                .map_err(|error| storage(DISCOVER_REFRESH, error))?;
                let projection_frontier = u64::try_from(
                    row.get::<i64>(1)
                        .map_err(|error| storage(DISCOVER_REFRESH, error))?,
                )
                .map_err(|error| storage(DISCOVER_REFRESH, error))?;
                let has_receipt = row
                    .get::<i64>(2)
                    .map_err(|error| storage(DISCOVER_REFRESH, error))?
                    != 0;
                let has_running = row
                    .get::<i64>(3)
                    .map_err(|error| storage(DISCOVER_REFRESH, error))?
                    != 0;
                scanned.push((session_id, projection_frontier, has_receipt, has_running));
            }
            drop(active_rows);
            active_exhausted = scanned.len() < active_scan_slots;
            active_rows_scanned = scanned.len();
            active_scanned_through = scanned
                .last()
                .map(|(session_id, _, _, _)| session_id.clone());
            for (session_id, projection_frontier, has_receipt, has_running) in scanned {
                if has_receipt || has_running {
                    continue;
                }
                requests.entry(session_id.as_str().to_owned()).or_insert(
                    SessionRefreshBeginOrJoinRequestV1::new(
                        session_id,
                        SessionRefreshFrontierV1::new(projection_frontier, projection_frontier)?,
                    ),
                );
            }
        }

        let requests = requests.into_values().collect::<Vec<_>>();
        hotpath_observe::record_output_sessions(u64::try_from(requests.len()).unwrap_or(u64::MAX));
        Ok(SessionTemporalRefreshDiscoveryPage {
            requests,
            active_scanned_through,
            active_exhausted,
            active_rows_scanned,
            pending_has_more,
        })
    }

    #[hotpath::measure(future = true, label = "session_temporal.projection.materialize")]
    pub async fn materialize_session_temporal_refresh_batch_result(
        &self,
        recovery: &SessionRefreshRecoveryV1,
    ) -> SessionStoreResult<Option<(SessionRefreshProgressV1, SessionTemporalProjectionBatchV1)>>
    {
        let snapshot = self
            .read_snapshot()
            .await
            .map_err(|error| storage(MATERIALIZE_REFRESH, error))?;
        hotpath_observe::record_snapshot_admissions(1);
        let baseline_copy_count =
            if recovery.restart_state() == SessionRefreshRestartStateV1::BeginProjection {
                let (scope, relation_store) = self
                    .session_relation_store()
                    .map_err(|error| storage(MATERIALIZE_REFRESH, error))?;
                match relation_store.load_projection(
                    &scope,
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

    #[hotpath::measure(future = true, label = "session_temporal.txn.persist_projection")]
    pub async fn persist_session_temporal_projection_batch_result(
        &self,
        batch: SessionTemporalProjectionBatchV1,
    ) -> SessionStoreResult<SessionTemporalProjectionBatchReceiptV1> {
        let transaction = hotpath::measure_block!("session_temporal.txn.begin", {
            self.begin_write_transaction()
                .await
                .map_err(|error| storage(PERSIST_OPERATION, error))?
        });
        let receipt = persist_session_temporal_projection_batch_in_transaction(
            &transaction,
            &batch,
            &ExecutionControl::default(),
            ProjectionProgressBaseline::Empty,
        )
        .await?;
        hotpath::measure_block!("session_temporal.txn.commit", {
            transaction
                .commit()
                .await
                .map_err(|error| storage(PERSIST_OPERATION, error))?
        });
        Ok(receipt)
    }
}
