use std::time::Duration;

use tracedecay_store::{
    GraphProjectionIdentityV1, GraphPublicationOperationContextV1, SemanticVectorOutboxSequence,
    SemanticVectorStageBatchCursor, SemanticVectorStageBatchKey, SemanticVectorStageBatchPage,
    SemanticVectorStageBatchPageRequest, SemanticVectorStageBatchReceiptLookup,
    SemanticVectorStageEffectState, SemanticVectorStageGraphBatchEffect, SemanticVectorStageKey,
    SemanticVectorStagePendingEffectCursor, SemanticVectorStagePendingEffectPage,
    SemanticVectorStagePendingEffectPageRequest, SemanticVectorStageRecord,
    SemanticVectorStagingStoreResult,
};

use crate::exact_sql::ExactSqlValue;

use super::exact::SemanticVectorStagingExactSqlStorage;
use super::support::{
    begin_read_snapshot, corrupt, ensure_live, ensure_projection_binding, integer, integer_at,
    invalid, pending_stage_for, projection_parts, query, receipt_by_ordinal, stage_by_key, text,
    u64_at,
};

const READ_WAIT: Duration = Duration::from_millis(10);

pub(super) fn stage(
    storage: &SemanticVectorStagingExactSqlStorage,
    key: &SemanticVectorStageKey,
    context: &GraphPublicationOperationContextV1<'_>,
) -> SemanticVectorStagingStoreResult<Option<SemanticVectorStageRecord>> {
    ensure_live(context)?;
    ensure_projection_binding(&storage.handle, &key.projection)?;
    let snapshot = begin_read_snapshot(&storage.handle, context, READ_WAIT)?;
    let result = stage_by_key(&snapshot, key)?.map(|stage| stage.record);
    ensure_live(context)?;
    Ok(result)
}

pub(super) fn pending_stage(
    storage: &SemanticVectorStagingExactSqlStorage,
    projection: &GraphProjectionIdentityV1,
    context: &GraphPublicationOperationContextV1<'_>,
) -> SemanticVectorStagingStoreResult<Option<SemanticVectorStageRecord>> {
    ensure_live(context)?;
    ensure_projection_binding(&storage.handle, projection)?;
    let snapshot = begin_read_snapshot(&storage.handle, context, READ_WAIT)?;
    let result = pending_stage_for(&snapshot, projection)?.map(|stage| stage.record);
    ensure_live(context)?;
    Ok(result)
}

pub(super) fn batch_receipt(
    storage: &SemanticVectorStagingExactSqlStorage,
    key: &SemanticVectorStageBatchKey,
    context: &GraphPublicationOperationContextV1<'_>,
) -> SemanticVectorStagingStoreResult<SemanticVectorStageBatchReceiptLookup> {
    key.validate()?;
    ensure_live(context)?;
    ensure_projection_binding(&storage.handle, &key.stage.projection)?;
    let snapshot = begin_read_snapshot(&storage.handle, context, READ_WAIT)?;
    let Some(stage) = stage_by_key(&snapshot, &key.stage)? else {
        ensure_live(context)?;
        return Ok(SemanticVectorStageBatchReceiptLookup::Missing);
    };
    let result = receipt_by_ordinal(&snapshot, stage.id, key.ordinal)?
        .map(|(_, receipt)| SemanticVectorStageBatchReceiptLookup::Found(Box::new(receipt)))
        .unwrap_or(SemanticVectorStageBatchReceiptLookup::Missing);
    ensure_live(context)?;
    Ok(result)
}

pub(super) fn batch_page(
    storage: &SemanticVectorStagingExactSqlStorage,
    request: &SemanticVectorStageBatchPageRequest,
    context: &GraphPublicationOperationContextV1<'_>,
) -> SemanticVectorStagingStoreResult<SemanticVectorStageBatchPage> {
    request.validate()?;
    ensure_live(context)?;
    ensure_projection_binding(&storage.handle, &request.stage.projection)?;
    let snapshot = begin_read_snapshot(&storage.handle, context, READ_WAIT)?;
    let Some(stage) = stage_by_key(&snapshot, &request.stage)? else {
        ensure_live(context)?;
        return Ok(SemanticVectorStageBatchPage {
            receipts: Vec::new(),
            continuation: None,
        });
    };
    super::cursors::validate_batch_cursor(&snapshot, stage.id, request, context)?;
    let after = request
        .after
        .as_ref()
        .map(|cursor| i64::try_from(cursor.ordinal))
        .transpose()
        .map_err(|_| invalid("semantic vector batch cursor exceeds SQLite range"))?
        .unwrap_or(-1);
    let rows = query(
        &snapshot,
        "SELECT ordinal FROM semantic_vector_stage_batches
         WHERE stage_id=?1 AND ordinal>?2 ORDER BY ordinal ASC LIMIT ?3",
        vec![
            ExactSqlValue::Integer(stage.id),
            ExactSqlValue::Integer(after),
            ExactSqlValue::Integer(i64::from(request.max_records) + 1),
        ],
    )?;
    let mut receipts = rows
        .rows
        .iter()
        .map(|row| {
            receipt_by_ordinal(&snapshot, stage.id, u64_at(row, 0)?)?
                .map(|(_, receipt)| receipt)
                .ok_or_else(|| corrupt("enumerated semantic vector batch is missing"))
        })
        .collect::<SemanticVectorStagingStoreResult<Vec<_>>>()?;
    let more = receipts.len() > usize::from(request.max_records);
    if more {
        receipts.pop();
    }
    let continuation =
        more.then(|| receipts.last())
            .flatten()
            .map(|receipt| SemanticVectorStageBatchCursor {
                stage: request.stage.clone(),
                ordinal: receipt.key.ordinal,
            });
    ensure_live(context)?;
    Ok(SemanticVectorStageBatchPage {
        receipts,
        continuation,
    })
}

pub(super) fn pending_effects(
    storage: &SemanticVectorStagingExactSqlStorage,
    request: &SemanticVectorStagePendingEffectPageRequest,
    context: &GraphPublicationOperationContextV1<'_>,
) -> SemanticVectorStagingStoreResult<SemanticVectorStagePendingEffectPage> {
    request.validate()?;
    ensure_live(context)?;
    ensure_projection_binding(&storage.handle, &request.projection)?;
    let snapshot = begin_read_snapshot(&storage.handle, context, READ_WAIT)?;
    let (shard, namespace, projection) = projection_parts(&request.projection)?;
    let after = request
        .after
        .as_ref()
        .map_or(0, |cursor| cursor.sequence.get());
    super::cursors::validate_pending_effect_cursor(
        &snapshot,
        request,
        (&shard, &namespace, &projection),
        context,
    )?;
    let rows = query(
        &snapshot,
        "SELECT e.outbox_sequence,b.stage_id,b.ordinal
         FROM semantic_vector_stage_graph_effects e
         JOIN semantic_vector_stage_batches b ON b.batch_id=e.batch_id
         JOIN semantic_vector_stages s ON s.stage_id=b.stage_id
         WHERE s.shard_id=?1 AND s.namespace=?2 AND s.projection=?3
           AND s.state='pending' AND e.state='pending'
           AND e.outbox_sequence>?4
         ORDER BY e.outbox_sequence ASC LIMIT ?5",
        vec![
            text(shard),
            text(namespace),
            text(projection),
            integer(after)?,
            ExactSqlValue::Integer(i64::from(request.max_records) + 1),
        ],
    )?;
    let mut effects = rows
        .rows
        .iter()
        .map(|row| {
            Ok(SemanticVectorStageGraphBatchEffect {
                sequence: SemanticVectorOutboxSequence::new(u64_at(row, 0)?)?,
                receipt: receipt_by_ordinal(&snapshot, integer_at(row, 1)?, u64_at(row, 2)?)?
                    .map(|(_, receipt)| receipt)
                    .ok_or_else(|| corrupt("pending semantic vector batch is missing"))?,
                state: SemanticVectorStageEffectState::Pending,
                terminal_digest: None,
            })
        })
        .collect::<SemanticVectorStagingStoreResult<Vec<_>>>()?;
    let more = effects.len() > usize::from(request.max_records);
    if more {
        effects.pop();
    }
    let continuation = more.then(|| effects.last()).flatten().map(|effect| {
        SemanticVectorStagePendingEffectCursor {
            sequence: effect.sequence,
        }
    });
    ensure_live(context)?;
    Ok(SemanticVectorStagePendingEffectPage {
        effects,
        continuation,
    })
}
