use tracedecay_store::{
    GraphPublicationOperationContextV1, SemanticVectorReadyPublicationPageRequest,
    SemanticVectorStageBatchPageRequest, SemanticVectorStagePendingEffectPageRequest,
    SemanticVectorStagingStoreError, SemanticVectorStagingStoreResult,
    StorageRuntimeContractErrorV1,
};

use crate::exact_sql::ExactSqlReadSnapshot;

use super::support::{ensure_live, integer, query, receipt_by_ordinal, stage_by_key, text};

pub(super) fn validate_batch_cursor(
    snapshot: &ExactSqlReadSnapshot,
    stage_id: i64,
    request: &SemanticVectorStageBatchPageRequest,
    context: &GraphPublicationOperationContextV1<'_>,
) -> SemanticVectorStagingStoreResult<()> {
    if let Some(cursor) = &request.after
        && receipt_by_ordinal(snapshot, stage_id, cursor.ordinal)?.is_none()
    {
        return invalid_cursor(context, "semantic vector batch page cursor anchor");
    }
    Ok(())
}

pub(super) fn validate_pending_effect_cursor(
    snapshot: &ExactSqlReadSnapshot,
    request: &SemanticVectorStagePendingEffectPageRequest,
    projection: (&str, &str, &str),
    context: &GraphPublicationOperationContextV1<'_>,
) -> SemanticVectorStagingStoreResult<()> {
    let Some(cursor) = &request.after else {
        return Ok(());
    };
    let rows = query(
        snapshot,
        "SELECT 1
         FROM semantic_vector_stage_graph_effects e
         JOIN semantic_vector_stage_batches b ON b.batch_id=e.batch_id
         JOIN semantic_vector_stages s ON s.stage_id=b.stage_id
         WHERE s.shard_id=?1 AND s.namespace=?2 AND s.projection=?3
           AND e.outbox_sequence=?4",
        vec![
            text(projection.0),
            text(projection.1),
            text(projection.2),
            integer(cursor.sequence.get())?,
        ],
    )?;
    if rows.rows.is_empty() {
        return invalid_cursor(context, "semantic vector pending effect cursor anchor");
    }
    Ok(())
}

pub(super) fn validate_ready_cursor(
    snapshot: &ExactSqlReadSnapshot,
    request: &SemanticVectorReadyPublicationPageRequest,
    context: &GraphPublicationOperationContextV1<'_>,
) -> SemanticVectorStagingStoreResult<()> {
    if let Some(cursor) = &request.after
        && stage_by_key(snapshot, &cursor.stage)?.is_none()
    {
        return invalid_cursor(context, "semantic vector ready publication cursor anchor");
    }
    Ok(())
}

fn invalid_cursor(
    context: &GraphPublicationOperationContextV1<'_>,
    field: &'static str,
) -> SemanticVectorStagingStoreResult<()> {
    ensure_live(context)?;
    Err(SemanticVectorStagingStoreError::InvalidRequest(
        StorageRuntimeContractErrorV1::ReceiptBindingMismatch { field },
    ))
}
