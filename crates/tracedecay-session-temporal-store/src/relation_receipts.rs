use std::sync::Arc;

use tracedecay_domain::{SessionId, SessionProjectionGenerationV1};
use tracedecay_graph_db::{GraphCancellation, GraphWatermark};
use tracedecay_runtime_core::db::engine::params;
use tracedecay_store::{SessionStoreError, SessionStoreResult};
use tracing::Instrument as _;

use super::query::{generation_i64, now_micros, storage, storage_message};
use super::relations::{SessionRelationProjection, projection_watermark};
use crate::handle::{SessionTemporalExec, SessionTemporalRegisteredDb, SessionTemporalWriteTxn};

const RECEIPT_OPERATION: &str = "publish native session relation receipt";

pub async fn record_relation_receipt(
    conn: &impl crate::handle::SessionTemporalExec,
    projection: &SessionRelationProjection,
    now: i64,
) -> SessionStoreResult<GraphWatermark> {
    super::relations::validate_projection(projection)
        .map_err(|error| storage(RECEIPT_OPERATION, error))?;
    let watermark =
        projection_watermark(projection).map_err(|error| storage(RECEIPT_OPERATION, error))?;
    let changed = conn
        .execute(
            "INSERT INTO session_relation_receipts (
                 session_id, generation, scope_kind, scope_id, expected_graph_watermark,
                 state, graph_watermark, created_at, applied_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, 'pending', NULL, ?6, NULL)
             ON CONFLICT(session_id, generation) DO UPDATE SET
                 expected_graph_watermark = excluded.expected_graph_watermark
             WHERE session_relation_receipts.scope_kind = excluded.scope_kind
               AND session_relation_receipts.scope_id = excluded.scope_id
               AND session_relation_receipts.expected_graph_watermark =
                   excluded.expected_graph_watermark",
            params![
                projection.session_id.as_str(),
                i64::try_from(projection.generation)
                    .map_err(|error| storage(RECEIPT_OPERATION, error))?,
                match &projection.scope {
                    super::relations::SessionRelationScope::ProjectSessions { .. } => {
                        "project_sessions"
                    }
                    super::relations::SessionRelationScope::ProfileSessions { .. } => {
                        "profile_sessions"
                    }
                },
                projection.scope.identity(),
                watermark.as_str(),
                now,
            ],
        )
        .await
        .map_err(|error| storage(RECEIPT_OPERATION, error))?;
    if changed != 1 {
        // The stored receipt already names a different graph. Replaying this
        // write cannot succeed, so this is not a storage retry.
        return Err(SessionStoreError::ReceiptIdentityMismatch {
            context: "immutable relation receipt graph identity",
        });
    }
    record_pending_effect_journal(conn, projection, now).await?;
    Ok(watermark)
}

async fn record_pending_effect_journal(
    conn: &impl crate::handle::SessionTemporalExec,
    projection: &SessionRelationProjection,
    now: i64,
) -> SessionStoreResult<()> {
    let projection_json =
        serde_json::to_string(projection).map_err(|error| storage(RECEIPT_OPERATION, error))?;
    let changed = conn
        .execute(
            "INSERT INTO session_relation_effect_journal (
                 session_id, generation, projection_json, created_at
             )
             SELECT ?1, ?2, ?3, ?4
             WHERE EXISTS (
                 SELECT 1
                 FROM session_relation_receipts
                 WHERE session_id = ?1 AND generation = ?2 AND state = 'pending'
             )
             ON CONFLICT(session_id, generation) DO UPDATE SET
                 projection_json = excluded.projection_json
             WHERE session_relation_effect_journal.projection_json =
                   excluded.projection_json",
            params![
                projection.session_id.as_str(),
                i64::try_from(projection.generation)
                    .map_err(|error| storage(RECEIPT_OPERATION, error))?,
                projection_json,
                now,
            ],
        )
        .await
        .map_err(|error| storage(RECEIPT_OPERATION, error))?;
    if changed == 1 {
        return Ok(());
    }
    let generation = SessionProjectionGenerationV1::new(projection.generation)
        .map_err(|error| storage(RECEIPT_OPERATION, error))?;
    let (_, pending) = expected_receipt(conn, &projection.session_id, generation).await?;
    if !pending {
        // The receipt upsert matched this projection's watermark, so an
        // applied receipt means the graph already holds it: a retried
        // completion replays the apply instead of journaling it again.
        return Ok(());
    }
    // The journal row for this generation already holds a different
    // projection. The next pass would submit the same row and be refused
    // again, so retire the refresh instead of retrying it.
    Err(SessionStoreError::ReceiptIdentityMismatch {
        context: "immutable relation effect journal",
    })
}

#[tracing::instrument(
    name = "session_temporal.txn.apply_relation",
    level = "trace",
    skip_all
)]
pub async fn apply_relation_projection(
    database: &impl SessionTemporalRegisteredDb,
    projection: &SessionRelationProjection,
    cancellation: Arc<dyn GraphCancellation>,
) -> SessionStoreResult<GraphWatermark> {
    let applied = write_relation_projection(database, projection, cancellation).await?;
    let transaction = database
        .begin_write_transaction()
        .instrument(tracing::trace_span!("session_temporal.txn.begin"))
        .await
        .map_err(|error| storage(RECEIPT_OPERATION, error))?;
    acknowledge_relation_receipt(&transaction, projection).await?;
    transaction
        .commit()
        .instrument(tracing::trace_span!("session_temporal.txn.commit"))
        .await
        .map_err(|error| storage(RECEIPT_OPERATION, error))?;
    Ok(applied)
}

/// Replaces the native relation graph with `projection` and leaves its
/// receipt pending; [`acknowledge_relation_receipt`] settles it.
pub(crate) async fn write_relation_projection(
    database: &impl SessionTemporalRegisteredDb,
    projection: &SessionRelationProjection,
    cancellation: Arc<dyn GraphCancellation>,
) -> SessionStoreResult<GraphWatermark> {
    let snapshot = database
        .read_snapshot()
        .await
        .map_err(|error| storage(RECEIPT_OPERATION, error))?;
    let (expected, _) = expected_receipt(
        &snapshot,
        &projection.session_id,
        SessionProjectionGenerationV1::new(projection.generation)
            .map_err(|error| storage(RECEIPT_OPERATION, error))?,
    )
    .await?;
    // Native graph I/O does not read this snapshot. Release its WAL slot
    // before publishing; the completion writer rechecks the exact receipt.
    drop(snapshot);
    write_relation_projection_with_receipt(database, projection, expected, cancellation)
}

fn write_relation_projection_with_receipt(
    database: &impl SessionTemporalRegisteredDb,
    projection: &SessionRelationProjection,
    expected: GraphWatermark,
    cancellation: Arc<dyn GraphCancellation>,
) -> SessionStoreResult<GraphWatermark> {
    let actual =
        projection_watermark(projection).map_err(|error| storage(RECEIPT_OPERATION, error))?;
    if actual != expected {
        return Err(SessionStoreError::ReceiptIdentityMismatch {
            context: "relation projection watermark",
        });
    }
    let (scope, store) = database
        .session_relation_store()
        .map_err(|error| storage(RECEIPT_OPERATION, error))?;
    if scope != projection.scope {
        return Err(SessionStoreError::SessionMismatch {
            context: "relation receipt graph scope",
        });
    }
    let applied = store
        .replace_with_cancellation(projection, cancellation)
        .map_err(|error| match error {
            super::relations::SessionRelationError::Cancelled => SessionStoreError::Cancelled,
            super::relations::SessionRelationError::DeadlineExceeded => {
                SessionStoreError::DeadlineExceeded
            }
            error => storage(RECEIPT_OPERATION, error),
        })?;
    if applied != expected {
        return Err(SessionStoreError::ReceiptIdentityMismatch {
            context: "native relation graph watermark",
        });
    }
    Ok(applied)
}

/// Marks the receipt applied once the native graph holds `projection`.
///
/// The receipt state is read under the writer lock, so a peer applier that
/// settled the receipt first leaves no journal row to remove.
pub(crate) async fn acknowledge_relation_receipt(
    transaction: &impl SessionTemporalExec,
    projection: &SessionRelationProjection,
) -> SessionStoreResult<()> {
    let generation = SessionProjectionGenerationV1::new(projection.generation)
        .map_err(|error| storage(RECEIPT_OPERATION, error))?;
    let (_, was_pending) =
        expected_receipt(transaction, &projection.session_id, generation).await?;
    let applied =
        projection_watermark(projection).map_err(|error| storage(RECEIPT_OPERATION, error))?;
    let changed = transaction
        .execute(
            "UPDATE session_relation_receipts
             SET state = 'applied', graph_watermark = ?3, applied_at = ?4
             WHERE session_id = ?1 AND generation = ?2
               AND expected_graph_watermark = ?3
               AND state IN ('pending', 'applied')",
            params![
                projection.session_id.as_str(),
                i64::try_from(projection.generation)
                    .map_err(|error| storage(RECEIPT_OPERATION, error))?,
                applied.as_str(),
                now_micros(RECEIPT_OPERATION)?.0,
            ],
        )
        .await
        .map_err(|error| storage(RECEIPT_OPERATION, error))?;
    if changed != 1 {
        return Err(storage_message(
            RECEIPT_OPERATION,
            "relation receipt changed during native graph acknowledgement",
        ));
    }
    let removed = transaction
        .execute(
            "DELETE FROM session_relation_effect_journal
             WHERE session_id = ?1 AND generation = ?2",
            params![
                projection.session_id.as_str(),
                i64::try_from(projection.generation)
                    .map_err(|error| storage(RECEIPT_OPERATION, error))?,
            ],
        )
        .await
        .map_err(|error| storage(RECEIPT_OPERATION, error))?;
    if was_pending && removed != 1 {
        return Err(storage_message(
            RECEIPT_OPERATION,
            "relation effect journal changed during native graph acknowledgement",
        ));
    }
    Ok(())
}

/// Reports whether a peer applier already settled this generation's receipt.
///
/// Concurrent post-commit appliers of the same generation race the effect
/// journal cleanup; the loser consults this before retrying so an applied
/// receipt reads as progress rather than corruption.
pub(crate) async fn relation_receipt_applied(
    database: &impl SessionTemporalRegisteredDb,
    session_id: &SessionId,
    generation: SessionProjectionGenerationV1,
) -> SessionStoreResult<bool> {
    let snapshot = database
        .read_snapshot()
        .await
        .map_err(|error| storage(RECEIPT_OPERATION, error))?;
    let mut rows = snapshot
        .query(
            "SELECT state
             FROM session_relation_receipts
             WHERE session_id = ?1 AND generation = ?2",
            params![
                session_id.as_str(),
                generation_i64(generation, RECEIPT_OPERATION)?
            ],
        )
        .await
        .map_err(|error| storage(RECEIPT_OPERATION, error))?;
    let state = rows
        .next()
        .await
        .map_err(|error| storage(RECEIPT_OPERATION, error))?
        .map(|row| row.get::<String>(0))
        .transpose()
        .map_err(|error| storage(RECEIPT_OPERATION, error))?;
    Ok(state.as_deref() == Some("applied"))
}

async fn expected_receipt(
    conn: &impl crate::handle::SessionTemporalQuery,
    session_id: &SessionId,
    generation: SessionProjectionGenerationV1,
) -> SessionStoreResult<(GraphWatermark, bool)> {
    let mut rows = conn
        .query(
            "SELECT expected_graph_watermark, state
             FROM session_relation_receipts
             WHERE session_id = ?1 AND generation = ?2",
            params![
                session_id.as_str(),
                generation_i64(generation, RECEIPT_OPERATION)?
            ],
        )
        .await
        .map_err(|error| storage(RECEIPT_OPERATION, error))?;
    let row = rows
        .next()
        .await
        .map_err(|error| storage(RECEIPT_OPERATION, error))?
        .ok_or_else(|| storage_message(RECEIPT_OPERATION, "relation receipt is unavailable"))?;
    let watermark = GraphWatermark::new(
        row.get::<String>(0)
            .map_err(|error| storage(RECEIPT_OPERATION, error))?,
    )
    .map_err(|error| storage(RECEIPT_OPERATION, error))?;
    let state: String = row
        .get(1)
        .map_err(|error| storage(RECEIPT_OPERATION, error))?;
    match state.as_str() {
        "pending" => Ok((watermark, true)),
        "applied" => Ok((watermark, false)),
        _ => Err(storage_message(
            RECEIPT_OPERATION,
            "relation receipt has an invalid state",
        )),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use tracedecay_global_db::tests::harness::{HostAdmissionScope, HostAdmissionTestRuntimeV1};
    use tracedecay_graph_db::NeverCancelled;
    use tracedecay_runtime_core::db::DatabaseEngineReadConnection;
    use tracedecay_store::{SessionRefreshBeginOrJoinRequestV1, SessionRefreshFrontierV1};
    use tracedecay_temporal_query::execution::ExecutionControl;

    use super::*;
    use crate::SessionTemporalAccess;

    struct ReaderReleaseProbe {
        reader: DatabaseEngineReadConnection,
        checkpoints: AtomicUsize,
    }

    impl GraphCancellation for ReaderReleaseProbe {
        fn is_cancelled(&self) -> bool {
            self.checkpoints.fetch_add(1, Ordering::Relaxed);
            let occupancy = self.reader.reader_pool_occupancy().unwrap();
            assert_eq!(
                occupancy.leased_general, 0,
                "native relation work must not retain the receipt's SQL snapshot"
            );
            false
        }
    }

    #[test]
    fn receipt_reader_is_released_before_each_native_relation_publication() {
        tracedecay_runtime_core::native_runtime::default_builder()
            .worker_threads(2)
            .build()
            .unwrap()
            .block_on(async {
                let directory = tempfile::tempdir().unwrap();
                let runtime = HostAdmissionTestRuntimeV1::profile(directory.path())
                    .await
                    .unwrap();
                let database = runtime
                    .registered_database(HostAdmissionScope::Profile)
                    .unwrap();
                let (scope, relations) =
                    SessionTemporalRegisteredDb::session_relation_store(database).unwrap();
                let access = SessionTemporalAccess::new(database);
                let mut projections = Vec::new();
                for id in ["receipt-reader-a", "receipt-reader-b"] {
                    let session_id = SessionId::new(id).unwrap();
                    let request = SessionRefreshBeginOrJoinRequestV1::new(
                        session_id.clone(),
                        SessionRefreshFrontierV1::new(0, 0).unwrap(),
                    );
                    access
                        .begin_or_join_session_refresh_result(request)
                        .await
                        .unwrap();
                    let recovery = access
                        .session_refresh_recovery_result(&session_id)
                        .await
                        .unwrap()
                        .unwrap();
                    projections.push(SessionRelationProjection {
                        scope: scope.clone(),
                        session_id,
                        generation: recovery.candidate_generation().value(),
                        summaries: Vec::new(),
                        logical_copies: Vec::new(),
                        thread_hierarchy: Vec::new(),
                        agent_hierarchy: Vec::new(),
                        parent_session_id: None,
                        workflow_agents: Vec::new(),
                    });
                }
                let expected = access
                    .record_relation_receipts_group(&projections, &ExecutionControl::default())
                    .await
                    .unwrap();
                let probe = Arc::new(ReaderReleaseProbe {
                    reader: database.read_connection(),
                    checkpoints: AtomicUsize::new(0),
                });
                for (projection, expected) in projections.iter().zip(expected) {
                    let before = probe.checkpoints.load(Ordering::Relaxed);
                    let applied = apply_relation_projection(
                        database,
                        projection,
                        Arc::clone(&probe) as Arc<dyn GraphCancellation>,
                    )
                    .await
                    .unwrap();
                    assert_eq!(applied, expected.unwrap());
                    assert!(probe.checkpoints.load(Ordering::Relaxed) > before);
                    assert!(
                        relation_receipt_applied(
                            database,
                            &projection.session_id,
                            SessionProjectionGenerationV1::new(projection.generation).unwrap(),
                        )
                        .await
                        .unwrap()
                    );
                    assert_eq!(
                        relations
                            .load_projection(
                                &scope,
                                &projection.session_id,
                                projection.generation,
                                100,
                                100,
                                Arc::new(NeverCancelled),
                            )
                            .unwrap(),
                        *projection
                    );
                }
            });
    }
}
