use std::sync::Arc;

use tracedecay_domain::{SessionId, SessionProjectionGenerationV1};
use tracedecay_graph_db::{GraphCancellation, GraphWatermark};
use tracedecay_runtime_core::db::engine::{Executor, QueryExecutor, params};
use tracedecay_store::SessionStoreResult;

use super::query::{generation_i64, storage, storage_message};
use super::relations::{SessionRelationProjection, projection_watermark};
use crate::RegisteredGlobalDb;

const RECEIPT_OPERATION: &str = "publish native session relation receipt";

pub(crate) async fn record_relation_receipt(
    conn: &impl Executor,
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
        return Err(storage_message(
            RECEIPT_OPERATION,
            "immutable relation receipt rejected different graph identity",
        ));
    }
    record_pending_effect_journal(conn, projection, now).await?;
    Ok(watermark)
}

async fn record_pending_effect_journal(
    conn: &impl Executor,
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
    if changed != 1 {
        return Err(storage_message(
            RECEIPT_OPERATION,
            "immutable relation effect journal rejected different projection",
        ));
    }
    Ok(())
}

pub(crate) async fn apply_relation_projection(
    database: &RegisteredGlobalDb,
    projection: &SessionRelationProjection,
    cancellation: Arc<dyn GraphCancellation>,
) -> SessionStoreResult<GraphWatermark> {
    let (expected, was_pending) = {
        let snapshot = database
            .read_snapshot()
            .await
            .map_err(|error| storage(RECEIPT_OPERATION, error))?;
        expected_receipt(
            &snapshot,
            &projection.session_id,
            SessionProjectionGenerationV1::new(projection.generation)
                .map_err(|error| storage(RECEIPT_OPERATION, error))?,
        )
        .await?
    };
    let actual =
        projection_watermark(projection).map_err(|error| storage(RECEIPT_OPERATION, error))?;
    if actual != expected {
        return Err(storage_message(
            RECEIPT_OPERATION,
            "reconstructed relations do not match the durable graph receipt",
        ));
    }
    let (scope, store) = database
        .session_relation_store()
        .map_err(|error| storage(RECEIPT_OPERATION, error))?;
    if scope != &projection.scope {
        return Err(storage_message(
            RECEIPT_OPERATION,
            "relation receipt project does not match mounted graph identity",
        ));
    }
    let applied = store
        .replace_with_cancellation(projection, cancellation)
        .map_err(|error| storage(RECEIPT_OPERATION, error))?;
    if applied != expected {
        return Err(storage_message(
            RECEIPT_OPERATION,
            "native graph acknowledged a different relation watermark",
        ));
    }
    let transaction = database
        .begin_write_transaction()
        .await
        .map_err(|error| storage(RECEIPT_OPERATION, error))?;
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
                now_micros()?,
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
    transaction
        .commit()
        .await
        .map_err(|error| storage(RECEIPT_OPERATION, error))?;
    Ok(applied)
}

/// Reports whether a peer applier already settled this generation's receipt.
///
/// Concurrent post-commit appliers of the same generation race the effect
/// journal cleanup; the loser consults this before retrying so an applied
/// receipt reads as progress rather than corruption.
pub(crate) async fn relation_receipt_applied(
    database: &RegisteredGlobalDb,
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
    conn: &impl QueryExecutor,
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

fn now_micros() -> SessionStoreResult<i64> {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|error| storage(RECEIPT_OPERATION, error))
        .and_then(|duration| {
            i64::try_from(duration.as_micros()).map_err(|error| storage(RECEIPT_OPERATION, error))
        })
}
