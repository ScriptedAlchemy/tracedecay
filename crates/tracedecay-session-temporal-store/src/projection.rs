use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
};

use tracedecay_domain::SessionId;
use tracedecay_graph_db::NeverCancelled;
use tracedecay_runtime_core::db::engine::params;
use tracedecay_store::{
    SessionRefreshBeginOrJoinRequestV1, SessionRefreshFrontierV1, SessionRefreshProgressV1,
    SessionStoreResult, SessionTemporalProjectionBatchReceiptV1, SessionTemporalProjectionBatchV1,
};
use tracedecay_temporal_query::execution::ExecutionControl;

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
    session_temporal_projection_record_count,
};
pub(crate) use receipts::digest_bytes;
pub use receipts::record_canonical_observation_effect;
pub(super) use receipts::validate_final_projection_receipt;

const DISCOVER_REFRESH: &str = "discover session temporal refresh";
const MATERIALIZE_REFRESH: &str = "materialize session temporal refresh";
/// Output-producing effect rows one discovery pass reads past its cursor.
const DISCOVERY_EFFECT_PAGE: i64 = 1_024;

/// Where refresh discovery resumes.
///
/// Observation effects are insert-only, so their rowids order them by commit
/// and a pass reads only the effects past the last row an earlier pass
/// accounted for. The cursor also names that row's observation: a store whose
/// rows were replaced or renumbered restarts from its first effect instead of
/// skipping any.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SessionTemporalRefreshDiscoveryCursor {
    effects_through: Option<(i64, String)>,
    /// Sessions whose new effects arrived while their refresh was running.
    /// The effect cursor has moved past those rows, so each pass rechecks the
    /// sessions themselves until the refresh ends.
    running_sessions: BTreeSet<String>,
    active_after: Option<SessionId>,
    /// Set once the relation-receipt repair sweep has visited every active
    /// generation. Refresh completion records the receipt of every later
    /// generation, so the sweep does not restart on each wake.
    active_swept: bool,
}

impl SessionTemporalRefreshDiscoveryCursor {
    #[cfg(any(test, feature = "test-helpers"))]
    pub fn after_active_session(session_id: SessionId) -> Self {
        Self {
            active_after: Some(session_id),
            ..Self::default()
        }
    }
}

pub struct SessionTemporalRefreshDiscoveryPage {
    requests: Vec<SessionRefreshBeginOrJoinRequestV1>,
    cursor: SessionTemporalRefreshDiscoveryCursor,
    active_rows_scanned: usize,
    has_more: bool,
}

impl SessionTemporalRefreshDiscoveryPage {
    pub fn active_rows_scanned(&self) -> usize {
        self.active_rows_scanned
    }

    pub fn into_parts(
        self,
    ) -> (
        Vec<SessionRefreshBeginOrJoinRequestV1>,
        SessionTemporalRefreshDiscoveryCursor,
        bool,
    ) {
        (self.requests, self.cursor, self.has_more)
    }
}

/// One session's refresh state as discovery sees it.
enum DiscoveredSession {
    Pending(SessionRefreshBeginOrJoinRequestV1),
    Running,
    Current,
}

async fn discover_session(
    conn: &impl crate::handle::SessionTemporalQuery,
    session_id: &str,
) -> SessionStoreResult<DiscoveredSession> {
    let mut rows = conn
        .query(
            "SELECT
                 (SELECT effect.observation_sequence
                  FROM session_temporal_observation_effects AS effect
                  WHERE effect.session_id = ?1 AND effect.output_count > 0
                  ORDER BY effect.observation_sequence DESC
                  LIMIT 1),
                 COALESCE((
                     SELECT CAST(json_extract(
                         active.frozen_watermarks_json,
                         '$.projection_frontier'
                     ) AS INTEGER)
                     FROM session_temporal_generations AS active
                     WHERE active.session_id = ?1 AND active.state = 'active'
                 ), 0),
                 EXISTS (
                     SELECT 1
                     FROM session_refresh_operations AS running
                     WHERE running.session_id = ?1 AND running.state = 'running'
                 )",
            params![session_id],
        )
        .await
        .map_err(|error| storage(DISCOVER_REFRESH, error))?;
    let row = rows
        .next()
        .await
        .map_err(|error| storage(DISCOVER_REFRESH, error))?
        .ok_or_else(|| storage_message(DISCOVER_REFRESH, "session discovery returned no row"))?;
    if row
        .get::<i64>(2)
        .map_err(|error| storage(DISCOVER_REFRESH, error))?
        != 0
    {
        return Ok(DiscoveredSession::Running);
    }
    let Some(observed_through) = row
        .get::<Option<i64>>(0)
        .map_err(|error| storage(DISCOVER_REFRESH, error))?
    else {
        return Ok(DiscoveredSession::Current);
    };
    let observed_through =
        u64::try_from(observed_through).map_err(|error| storage(DISCOVER_REFRESH, error))?;
    let committed_through = u64::try_from(
        row.get::<i64>(1)
            .map_err(|error| storage(DISCOVER_REFRESH, error))?,
    )
    .map_err(|error| storage(DISCOVER_REFRESH, error))?;
    if observed_through <= committed_through {
        return Ok(DiscoveredSession::Current);
    }
    Ok(DiscoveredSession::Pending(
        SessionRefreshBeginOrJoinRequestV1::new(
            SessionId::new(session_id).map_err(|error| storage(DISCOVER_REFRESH, error))?,
            SessionRefreshFrontierV1::new(observed_through, committed_through)?,
        ),
    ))
}

/// Reads the output-producing effects past `cursor`, adding a request for
/// every session they leave behind its projection frontier. Returns whether
/// more effects may remain past the advanced cursor.
async fn discover_pending_effects(
    conn: &impl crate::handle::SessionTemporalQuery,
    pending_limit: usize,
    cursor: &mut SessionTemporalRefreshDiscoveryCursor,
    requests: &mut BTreeMap<String, SessionRefreshBeginOrJoinRequestV1>,
) -> SessionStoreResult<bool> {
    if let Some((rowid, observation_id)) = &cursor.effects_through {
        let mut rows = conn
            .query(
                "SELECT observation_id = ?2
                 FROM session_temporal_observation_effects WHERE rowid = ?1",
                params![*rowid, observation_id.as_str()],
            )
            .await
            .map_err(|error| storage(DISCOVER_REFRESH, error))?;
        let same_row = match rows
            .next()
            .await
            .map_err(|error| storage(DISCOVER_REFRESH, error))?
        {
            Some(row) => {
                row.get::<i64>(0)
                    .map_err(|error| storage(DISCOVER_REFRESH, error))?
                    != 0
            }
            None => false,
        };
        if !same_row {
            cursor.effects_through = None;
        }
    }
    let after = cursor
        .effects_through
        .as_ref()
        .map_or(0, |(rowid, _)| *rowid);
    if pending_limit == 0 {
        let mut rows = conn
            .query(
                "SELECT 1 FROM session_temporal_observation_effects
                 WHERE rowid > ?1 AND output_count > 0
                 LIMIT 1",
                params![after],
            )
            .await
            .map_err(|error| storage(DISCOVER_REFRESH, error))?;
        return Ok(rows
            .next()
            .await
            .map_err(|error| storage(DISCOVER_REFRESH, error))?
            .is_some());
    }
    let mut decided = BTreeSet::new();
    for session_id in std::mem::take(&mut cursor.running_sessions) {
        if requests.len() >= pending_limit {
            cursor.running_sessions.insert(session_id);
            continue;
        }
        match discover_session(conn, &session_id).await? {
            DiscoveredSession::Pending(request) => {
                requests.insert(session_id.clone(), request);
            }
            DiscoveredSession::Running => {
                cursor.running_sessions.insert(session_id.clone());
            }
            DiscoveredSession::Current => {}
        }
        decided.insert(session_id);
    }
    let mut rows = conn
        .query(
            "SELECT rowid, observation_id, session_id
             FROM session_temporal_observation_effects
             WHERE rowid > ?1 AND output_count > 0
             ORDER BY rowid
             LIMIT ?2",
            params![after, DISCOVERY_EFFECT_PAGE],
        )
        .await
        .map_err(|error| storage(DISCOVER_REFRESH, error))?;
    let mut effects = Vec::new();
    while let Some(row) = rows
        .next()
        .await
        .map_err(|error| storage(DISCOVER_REFRESH, error))?
    {
        effects.push((
            row.get::<i64>(0)
                .map_err(|error| storage(DISCOVER_REFRESH, error))?,
            row.get::<String>(1)
                .map_err(|error| storage(DISCOVER_REFRESH, error))?,
            row.get::<String>(2)
                .map_err(|error| storage(DISCOVER_REFRESH, error))?,
        ));
    }
    drop(rows);
    let page_full = i64::try_from(effects.len()).is_ok_and(|len| len >= DISCOVERY_EFFECT_PAGE);
    for (rowid, observation_id, session_id) in effects {
        if !decided.contains(&session_id) {
            if requests.len() >= pending_limit {
                return Ok(true);
            }
            match discover_session(conn, &session_id).await? {
                DiscoveredSession::Pending(request) => {
                    requests.insert(session_id.clone(), request);
                }
                DiscoveredSession::Running => {
                    cursor.running_sessions.insert(session_id.clone());
                }
                DiscoveredSession::Current => {}
            }
            decided.insert(session_id);
        }
        cursor.effects_through = Some((rowid, observation_id));
    }
    if page_full {
        return Ok(true);
    }
    // History-only effects past the last output row need no refresh; move
    // the cursor over them so the next pass does not read them again.
    let mut rows = conn
        .query(
            "SELECT rowid, observation_id
             FROM session_temporal_observation_effects
             ORDER BY rowid DESC
             LIMIT 1",
            (),
        )
        .await
        .map_err(|error| storage(DISCOVER_REFRESH, error))?;
    if let Some(row) = rows
        .next()
        .await
        .map_err(|error| storage(DISCOVER_REFRESH, error))?
    {
        let rowid = row
            .get::<i64>(0)
            .map_err(|error| storage(DISCOVER_REFRESH, error))?;
        if cursor
            .effects_through
            .as_ref()
            .is_none_or(|(through, _)| *through < rowid)
        {
            cursor.effects_through = Some((
                rowid,
                row.get::<String>(1)
                    .map_err(|error| storage(DISCOVER_REFRESH, error))?,
            ));
        }
    }
    Ok(false)
}

impl<D: SessionTemporalRegisteredDb + Sync> SessionTemporalAccess<'_, D> {
    /// Discovers sessions that need temporal projection: sessions whose
    /// output-producing effects committed past `cursor` left them behind their
    /// projection frontier, plus active generations that lack an applied
    /// relation receipt.
    #[hotpath::measure(future = true, label = "session_temporal.query.pending_refresh")]
    pub async fn pending_session_temporal_refresh_page_result(
        &self,
        limit: usize,
        active_scan_slots: usize,
        cursor: &SessionTemporalRefreshDiscoveryCursor,
    ) -> SessionStoreResult<SessionTemporalRefreshDiscoveryPage> {
        let snapshot = self
            .read_snapshot()
            .await
            .map_err(|error| storage(DISCOVER_REFRESH, error))?;
        hotpath_observe::record_snapshot_admissions(1);
        if limit == 0 {
            return Ok(SessionTemporalRefreshDiscoveryPage {
                requests: Vec::new(),
                cursor: SessionTemporalRefreshDiscoveryCursor {
                    active_after: None,
                    ..cursor.clone()
                },
                active_rows_scanned: 0,
                has_more: false,
            });
        }
        let active_scan_slots = active_scan_slots.min(limit);
        let pending_limit = limit.saturating_sub(active_scan_slots);
        let mut next = cursor.clone();
        let mut requests = BTreeMap::new();
        let pending_has_more =
            discover_pending_effects(&snapshot, pending_limit, &mut next, &mut requests).await?;

        // An active generation created before native relation publication was
        // authoritative may already cover its source frontier but have no
        // relation receipt. Rediscover that exact committed frontier so the
        // ordinary refresh path rebuilds and verifies it; discovery itself
        // never fabricates a receipt or mutates the projection.
        let mut active_scanned_through = cursor.active_after.clone();
        let mut active_exhausted = cursor.active_swept;
        let mut active_rows_scanned = 0;
        if active_scan_slots > 0 && !cursor.active_swept {
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
                    params![
                        cursor.active_after.as_ref().map(SessionId::as_str),
                        active_limit
                    ],
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
        next.active_swept = active_exhausted;
        next.active_after = if active_exhausted {
            None
        } else {
            active_scanned_through
        };
        Ok(SessionTemporalRefreshDiscoveryPage {
            requests,
            cursor: next,
            active_rows_scanned,
            has_more: pending_has_more || !active_exhausted,
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
                match relation_store.logical_copy_count(
                    &scope,
                    recovery.session_id(),
                    recovery.frozen_watermarks().active_generation().value(),
                    Arc::new(NeverCancelled),
                ) {
                    Ok(copies) => copies,
                    Err(SessionRelationError::NotFound) => {
                        // No native graph was applied for this generation. Reconstruct
                        // the copy count from the sealed rows instead of retrying the
                        // absence as a busy source.
                        crate::relation_projection::count_canonical_logical_copies(
                            &snapshot,
                            recovery.session_id(),
                            recovery.frozen_watermarks().active_generation(),
                        )
                        .await?
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
