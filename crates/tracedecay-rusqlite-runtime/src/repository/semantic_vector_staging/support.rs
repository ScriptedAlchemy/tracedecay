use tracedecay_store::{
    GraphProjectionIdentityV1, GraphPublicationOperationContextV1,
    SemanticVectorChunkManifestAccumulator, SemanticVectorChunkManifestMember,
    SemanticVectorOutboxSequence, SemanticVectorStageBatchReceipt,
    SemanticVectorStageChunkOperation, SemanticVectorStageEffectState,
    SemanticVectorStageEffectTerminal, SemanticVectorStageGraphBatchEffect, SemanticVectorStageKey,
    SemanticVectorStagePlan, SemanticVectorStagePublicationIntent, SemanticVectorStageRecord,
    SemanticVectorStageState, SemanticVectorStagingStoreError, SemanticVectorStagingStoreResult,
    SemanticVectorWriterFence,
};

use crate::exact_sql::{
    ExactSqlError, ExactSqlHandle, ExactSqlReadSnapshot, ExactSqlRow, ExactSqlRows,
    ExactSqlStatement, ExactSqlTransaction, ExactSqlValue,
};
use std::{collections::BTreeSet, time::Duration};

pub(super) struct Stage {
    pub id: i64,
    pub record: SemanticVectorStageRecord,
}

pub(super) trait Query {
    fn run(&self, statement: ExactSqlStatement) -> Result<ExactSqlRows, ExactSqlError>;
}

impl Query for ExactSqlTransaction {
    fn run(&self, statement: ExactSqlStatement) -> Result<ExactSqlRows, ExactSqlError> {
        self.query(statement)
    }
}

impl Query for ExactSqlReadSnapshot {
    fn run(&self, statement: ExactSqlStatement) -> Result<ExactSqlRows, ExactSqlError> {
        self.query(statement)
    }
}

pub(super) fn stage_by_key(
    authority: &impl Query,
    key: &SemanticVectorStageKey,
) -> SemanticVectorStagingStoreResult<Option<Stage>> {
    let (shard, namespace, projection) = projection_parts(&key.projection)?;
    stage_query(
        authority,
        "WHERE shard_id=?1 AND namespace=?2 AND projection=?3
           AND build_id=?4 AND plan_digest=?5",
        vec![
            text(shard),
            text(namespace),
            text(projection),
            text(key.build_id.as_str()),
            text(key.plan_digest.as_str()),
        ],
    )
}

pub(super) fn pending_stage_for(
    authority: &impl Query,
    projection: &GraphProjectionIdentityV1,
) -> SemanticVectorStagingStoreResult<Option<Stage>> {
    let (shard, namespace, projection) = projection_parts(projection)?;
    stage_query(
        authority,
        "WHERE shard_id=?1 AND namespace=?2 AND projection=?3
           AND state IN ('pending','ready_to_publish')",
        vec![text(shard), text(namespace), text(projection)],
    )
}

pub(super) fn stage_query(
    authority: &impl Query,
    predicate: &str,
    params: Vec<ExactSqlValue>,
) -> SemanticVectorStagingStoreResult<Option<Stage>> {
    let rows = query(
        authority,
        &format!(
            "SELECT stage_id,plan_json,state,next_ordinal,checkpoint_digest,
                recorded_chunk_count,expected_recovered_digest,publication_intent_digest,
                applied_ordinal,applied_receipt_digest,applied_checkpoint_digest,
                applied_graph_batch_digest,shard_id,namespace,projection,build_id,
                plan_digest,semantic_generation_id,base_generation,
                publication_generation,publication_idempotency_key,
                source_scope,source_generation,source_dependency,source_manifest_digest,
                embedding_projection_digest,embedding_dimension,model_artifact_digest,
                projection_manifest_digest,privacy_domain_digest,privacy_key_epoch,
                expected_chunk_manifest_digest,expected_chunk_count,
                expected_prior_verified_head,writer_binding,code_scope_hash
             FROM semantic_vector_stages {predicate} LIMIT 1"
        ),
        params,
    )?;
    rows.rows.first().map(decode_stage).transpose()
}

pub(super) fn decode_stage(row: &ExactSqlRow) -> SemanticVectorStagingStoreResult<Stage> {
    let plan: SemanticVectorStagePlan = decode_json(text_at(row, 1)?)?;
    plan.validate()
        .map_err(|error| corrupt(error.to_string()))?;
    validate_stage_columns(row, &plan)?;
    let state = match text_at(row, 2)? {
        "pending" => SemanticVectorStageState::Pending,
        "ready_to_publish" => SemanticVectorStageState::ReadyToPublish,
        "published" => SemanticVectorStageState::Published,
        "cancelled" => SemanticVectorStageState::Cancelled,
        _ => return Err(corrupt("unknown semantic vector stage state")),
    };
    let recovered = optional_text_at(row, 6)?
        .map(tracedecay_store::GraphRecoveredGenerationDigestV1::new)
        .transpose()?;
    let intent = optional_text_at(row, 7)?
        .map(tracedecay_store::SemanticVectorPublicationIntentDigest::new)
        .transpose()?;
    let publication_intent = match (recovered, intent) {
        (Some(expected_recovered_digest), Some(publication_intent_digest)) => {
            Some(SemanticVectorStagePublicationIntent {
                publication_key: plan.publication_key.clone(),
                expected_recovered_digest,
                publication_intent_digest,
            })
        }
        (None, None) => None,
        _ => return Err(corrupt("partial semantic vector publication intent")),
    };
    let record = SemanticVectorStageRecord {
        plan,
        state,
        next_ordinal: u64_at(row, 3)?,
        checkpoint_digest: tracedecay_store::SemanticVectorCheckpointDigest::new(text_at(row, 4)?)?,
        recorded_chunk_count: u64_at(row, 5)?,
        publication_intent,
        applied_ordinal: optional_u64_at(row, 8)?,
        applied_receipt_digest: optional_text_at(row, 9)?
            .map(tracedecay_store::SemanticVectorBatchReceiptDigest::new)
            .transpose()?,
        applied_checkpoint_digest: optional_text_at(row, 10)?
            .map(tracedecay_store::SemanticVectorCheckpointDigest::new)
            .transpose()?,
        applied_graph_batch_digest: optional_text_at(row, 11)?
            .map(tracedecay_store::SemanticVectorGraphBatchDigest::new)
            .transpose()?,
    };
    validate_stage_record(&record)?;
    Ok(Stage {
        id: integer_at(row, 0)?,
        record,
    })
}

pub(super) fn receipt_by_ordinal(
    authority: &impl Query,
    stage_id: i64,
    ordinal: u64,
) -> SemanticVectorStagingStoreResult<Option<(i64, SemanticVectorStageBatchReceipt)>> {
    let rows = query(
        authority,
        "SELECT batch_id,receipt_json,ordinal,expected_checkpoint_digest,input_digest,
                output_digest,receipt_digest,checkpoint_digest,chunk_count
         FROM semantic_vector_stage_batches
         WHERE stage_id=?1 AND ordinal=?2",
        vec![ExactSqlValue::Integer(stage_id), integer(ordinal)?],
    )?;
    rows.rows
        .first()
        .map(|row| {
            let batch_id = integer_at(row, 0)?;
            let receipt: SemanticVectorStageBatchReceipt = decode_json(text_at(row, 1)?)?;
            receipt
                .validate()
                .map_err(|error| corrupt(error.to_string()))?;
            validate_receipt_columns(row, &receipt)?;
            validate_receipt_chunks(authority, stage_id, batch_id, &receipt)?;
            Ok((batch_id, receipt))
        })
        .transpose()
}

fn validate_stage_columns(
    row: &ExactSqlRow,
    plan: &SemanticVectorStagePlan,
) -> SemanticVectorStagingStoreResult<()> {
    let (shard, namespace, projection) = projection_parts(&plan.key.projection)?;
    let values = [
        (12, shard),
        (13, namespace),
        (14, projection),
        (15, plan.key.build_id.as_str().to_owned()),
        (16, plan.key.plan_digest.as_str().to_owned()),
        (
            17,
            plan.semantic_generation_id.as_digest().as_str().to_owned(),
        ),
        (19, plan.publication_key.generation.as_str().to_owned()),
        (20, plan.publication_key.idempotency_key.as_str().to_owned()),
        (21, json(&plan.source_scope)?),
        (22, plan.source_generation.as_str().to_owned()),
        (23, json(&plan.source_dependency)?),
        (24, plan.recipe.source_manifest_digest.as_str().to_owned()),
        (
            25,
            plan.recipe.embedding_projection_digest.as_str().to_owned(),
        ),
        (27, plan.recipe.model_artifact_digest.as_str().to_owned()),
        (
            28,
            plan.recipe.projection_manifest_digest.as_str().to_owned(),
        ),
        (29, plan.recipe.privacy_domain_digest.as_str().to_owned()),
        (
            31,
            plan.recipe
                .expected_chunk_manifest_digest
                .as_str()
                .to_owned(),
        ),
    ];
    for (index, expected) in values {
        if text_at(row, index)? != expected {
            return Err(corrupt("semantic vector stage normalized column mismatch"));
        }
    }
    if optional_text_at(row, 18)?
        != plan
            .base_generation
            .as_ref()
            .map(|generation| generation.as_digest().as_str())
        || u64_at(row, 26)? != u64::from(plan.recipe.embedding_dimension)
        || u64_at(row, 30)? != plan.recipe.privacy_key_epoch
        || u64_at(row, 32)? != plan.expected_chunk_count
        || optional_text_at(row, 33)?
            != plan
                .expected_prior_verified_head
                .as_ref()
                .map(json)
                .transpose()?
                .as_deref()
        || text_at(row, 34)? != json(&plan.writer_fence.binding)?
        || text_at(row, 35)? != plan.code_scope_hash.as_str()
    {
        return Err(corrupt("semantic vector stage normalized column mismatch"));
    }
    Ok(())
}

fn validate_stage_record(
    record: &SemanticVectorStageRecord,
) -> SemanticVectorStagingStoreResult<()> {
    let applied = [
        record.applied_ordinal.is_some(),
        record.applied_receipt_digest.is_some(),
        record.applied_checkpoint_digest.is_some(),
        record.applied_graph_batch_digest.is_some(),
    ];
    if applied.iter().any(|present| *present != applied[0])
        || record.recorded_chunk_count > record.plan.expected_chunk_count
        || record
            .applied_ordinal
            .is_some_and(|ordinal| ordinal >= record.next_ordinal)
        || (record.next_ordinal == 0
            && record.checkpoint_digest != record.plan.initial_checkpoint_digest)
        || matches!(
            record.state,
            SemanticVectorStageState::ReadyToPublish | SemanticVectorStageState::Published
        ) != record.publication_intent.is_some()
        || record
            .publication_intent
            .as_ref()
            .is_some_and(|intent| intent.publication_key != record.plan.publication_key)
    {
        return Err(corrupt("semantic vector stage record invariant violation"));
    }
    Ok(())
}

fn validate_receipt_columns(
    row: &ExactSqlRow,
    receipt: &SemanticVectorStageBatchReceipt,
) -> SemanticVectorStagingStoreResult<()> {
    let chunk_count = u64::try_from(receipt.chunks.len())
        .map_err(|_| corrupt("semantic vector batch chunk count exceeds u64"))?;
    if u64_at(row, 2)? != receipt.key.ordinal
        || text_at(row, 3)? != receipt.expected_checkpoint_digest.as_str()
        || text_at(row, 4)? != receipt.input_digest.as_str()
        || text_at(row, 5)? != receipt.output_digest.as_str()
        || text_at(row, 6)? != receipt.receipt_digest.as_str()
        || text_at(row, 7)? != receipt.checkpoint_digest.as_str()
        || u64_at(row, 8)? != chunk_count
    {
        return Err(corrupt("semantic vector batch normalized column mismatch"));
    }
    Ok(())
}

fn validate_receipt_chunks(
    authority: &impl Query,
    stage_id: i64,
    batch_id: i64,
    receipt: &SemanticVectorStageBatchReceipt,
) -> SemanticVectorStagingStoreResult<()> {
    let rows = query(
        authority,
        "SELECT effect_ordinal,chunk_id,chunk_digest,operation,output_digest
         FROM semantic_vector_stage_chunk_receipts
         WHERE stage_id=?1 AND batch_id=?2 ORDER BY effect_ordinal ASC",
        vec![
            ExactSqlValue::Integer(stage_id),
            ExactSqlValue::Integer(batch_id),
        ],
    )?;
    if rows.rows.len() != receipt.chunks.len() {
        return Err(corrupt("semantic vector batch chunk child count mismatch"));
    }
    for (row, chunk) in rows.rows.iter().zip(&receipt.chunks) {
        let operation = chunk.operation.as_str();
        if u64_at(row, 0)? != u64::from(chunk.effect_ordinal)
            || text_at(row, 1)? != chunk.chunk_id.as_str()
            || text_at(row, 2)? != chunk.chunk_digest.as_str()
            || text_at(row, 3)? != operation
            || optional_text_at(row, 4)? != chunk.output_digest.as_ref().map(|value| value.as_str())
        {
            return Err(corrupt("semantic vector batch chunk child mismatch"));
        }
    }
    Ok(())
}

pub(super) fn effect_by_batch(
    authority: &impl Query,
    batch_id: i64,
    receipt: SemanticVectorStageBatchReceipt,
) -> SemanticVectorStagingStoreResult<SemanticVectorStageGraphBatchEffect> {
    let rows = query(
        authority,
        "SELECT outbox_sequence,state,terminal_digest
         FROM semantic_vector_stage_graph_effects WHERE batch_id=?1",
        vec![ExactSqlValue::Integer(batch_id)],
    )?;
    let row = rows
        .rows
        .first()
        .ok_or_else(|| corrupt("semantic vector graph effect is missing"))?;
    let state = match text_at(row, 1)? {
        "pending" => SemanticVectorStageEffectState::Pending,
        "applied" => SemanticVectorStageEffectState::Applied,
        "failed" => SemanticVectorStageEffectState::Failed,
        "cancelled" => SemanticVectorStageEffectState::Cancelled,
        _ => return Err(corrupt("unknown semantic vector effect state")),
    };
    Ok(SemanticVectorStageGraphBatchEffect {
        sequence: SemanticVectorOutboxSequence::new(u64_at(row, 0)?)?,
        receipt,
        state,
        terminal_digest: optional_text_at(row, 2)?.map(str::to_owned),
    })
}

pub(super) fn validate_stage_history(
    authority: &impl Query,
    stage: &Stage,
    context: &GraphPublicationOperationContextV1<'_>,
) -> SemanticVectorStagingStoreResult<()> {
    let mut checkpoint = stage.record.plan.initial_checkpoint_digest.clone();
    let mut chunks = 0_u64;
    let mut after_ordinal = -1_i64;
    let mut after_effect = -1_i64;
    let mut seen_batches = 0_u64;
    let mut active: Option<(
        SemanticVectorStageBatchReceipt,
        SemanticVectorStageEffectState,
        Option<String>,
        usize,
    )> = None;
    loop {
        ensure_live(context)?;
        let rows = query(
            authority,
            "SELECT b.batch_id,
                    CASE WHEN c.effect_ordinal=0 OR c.effect_ordinal IS NULL
                         THEN b.receipt_json END,
                    b.ordinal,b.expected_checkpoint_digest,
                    b.input_digest,b.output_digest,b.receipt_digest,b.checkpoint_digest,
                    b.chunk_count,COALESCE(c.effect_ordinal,-1),c.chunk_id,c.chunk_digest,c.operation,
                    c.output_digest,e.outbox_sequence,e.state,e.terminal_digest
             FROM semantic_vector_stage_batches b
             LEFT JOIN semantic_vector_stage_chunk_receipts c ON c.batch_id=b.batch_id
             JOIN semantic_vector_stage_graph_effects e ON e.batch_id=b.batch_id
             WHERE b.stage_id=?1
               AND (b.ordinal>?2
                    OR (b.ordinal=?2 AND COALESCE(c.effect_ordinal,-1)>?3))
             ORDER BY b.ordinal ASC,COALESCE(c.effect_ordinal,-1) ASC LIMIT 512",
            vec![
                ExactSqlValue::Integer(stage.id),
                ExactSqlValue::Integer(after_ordinal),
                ExactSqlValue::Integer(after_effect),
            ],
        )?;
        if rows.rows.is_empty() {
            break;
        }
        for row in &rows.rows {
            ensure_live(context)?;
            let ordinal = u64_at(row, 2)?;
            if active
                .as_ref()
                .is_some_and(|(receipt, _, _, _)| receipt.key.ordinal != ordinal)
            {
                let (receipt, state, terminal, child_count) = active
                    .take()
                    .ok_or_else(|| corrupt("semantic vector history batch disappeared"))?;
                finalize_history_batch(
                    stage,
                    &receipt,
                    state,
                    terminal.as_deref(),
                    child_count,
                    &mut checkpoint,
                    &mut chunks,
                )?;
                seen_batches += 1;
            }
            if active.is_none() {
                if ordinal != seen_batches {
                    return Err(corrupt(
                        "semantic vector stage batch sequence is not contiguous",
                    ));
                }
                let receipt: SemanticVectorStageBatchReceipt = decode_json(text_at(row, 1)?)?;
                receipt
                    .validate()
                    .map_err(|error| corrupt(error.to_string()))?;
                validate_receipt_columns(row, &receipt)?;
                if receipt.key.stage != stage.record.plan.key
                    || receipt.expected_checkpoint_digest != checkpoint
                {
                    return Err(corrupt("semantic vector stage batch chain mismatch"));
                }
                SemanticVectorOutboxSequence::new(u64_at(row, 14)?)?;
                let state = decode_effect_state(text_at(row, 15)?)?;
                active = Some((
                    receipt,
                    state,
                    optional_text_at(row, 16)?.map(str::to_owned),
                    0,
                ));
            }
            let (receipt, state, terminal, child_count) = active
                .as_mut()
                .ok_or_else(|| corrupt("semantic vector history batch is missing"))?;
            validate_receipt_columns(row, receipt)?;
            if decode_effect_state(text_at(row, 15)?)? != *state
                || optional_text_at(row, 16)? != terminal.as_deref()
            {
                return Err(corrupt("semantic vector batch effect row mismatch"));
            }
            let stored_effect_ordinal = signed_integer_at(row, 9)?;
            if receipt.chunks.is_empty() {
                if stored_effect_ordinal != -1
                    || optional_text_at(row, 10)?.is_some()
                    || optional_text_at(row, 11)?.is_some()
                    || optional_text_at(row, 12)?.is_some()
                    || optional_text_at(row, 13)?.is_some()
                {
                    return Err(corrupt(
                        "empty semantic vector control batch has chunk rows",
                    ));
                }
                after_ordinal = integer_at(row, 2)?;
                after_effect = stored_effect_ordinal;
                continue;
            }
            let effect_ordinal = usize::try_from(stored_effect_ordinal)
                .map_err(|_| corrupt("semantic vector effect ordinal is negative"))?;
            if effect_ordinal != *child_count {
                return Err(corrupt(
                    "semantic vector batch chunk sequence is not contiguous",
                ));
            }
            let chunk = receipt
                .chunks
                .get(effect_ordinal)
                .ok_or_else(|| corrupt("semantic vector batch has excess chunk child"))?;
            let operation = chunk.operation.as_str();
            if text_at(row, 10)? != chunk.chunk_id.as_str()
                || text_at(row, 11)? != chunk.chunk_digest.as_str()
                || text_at(row, 12)? != operation
                || optional_text_at(row, 13)?
                    != chunk.output_digest.as_ref().map(|value| value.as_str())
            {
                return Err(corrupt("semantic vector batch chunk child mismatch"));
            }
            *child_count += 1;
            after_ordinal = integer_at(row, 2)?;
            after_effect = stored_effect_ordinal;
        }
    }
    if let Some((receipt, state, terminal, child_count)) = active {
        finalize_history_batch(
            stage,
            &receipt,
            state,
            terminal.as_deref(),
            child_count,
            &mut checkpoint,
            &mut chunks,
        )?;
        seen_batches += 1;
    }
    if seen_batches != stage.record.next_ordinal {
        return Err(corrupt("semantic vector stage batch frontier mismatch"));
    }
    if chunks != stage.record.recorded_chunk_count || checkpoint != stage.record.checkpoint_digest {
        return Err(corrupt("semantic vector stage chunk head mismatch"));
    }
    if stage.record.state == SemanticVectorStageState::ReadyToPublish
        && (chunks != stage.record.plan.expected_chunk_count
            || chunk_manifest_digest(authority, stage.id, context)?
                != stage.record.plan.recipe.expected_chunk_manifest_digest)
    {
        return Err(corrupt("ready semantic vector stage manifest mismatch"));
    }
    Ok(())
}

fn decode_effect_state(
    value: &str,
) -> SemanticVectorStagingStoreResult<SemanticVectorStageEffectState> {
    match value {
        "pending" => Ok(SemanticVectorStageEffectState::Pending),
        "applied" => Ok(SemanticVectorStageEffectState::Applied),
        "failed" => Ok(SemanticVectorStageEffectState::Failed),
        "cancelled" => Ok(SemanticVectorStageEffectState::Cancelled),
        _ => Err(corrupt("unknown semantic vector effect state")),
    }
}

#[allow(clippy::too_many_arguments)]
fn finalize_history_batch(
    stage: &Stage,
    receipt: &SemanticVectorStageBatchReceipt,
    state: SemanticVectorStageEffectState,
    terminal: Option<&str>,
    child_count: usize,
    checkpoint: &mut tracedecay_store::SemanticVectorCheckpointDigest,
    chunks: &mut u64,
) -> SemanticVectorStagingStoreResult<()> {
    if child_count != receipt.chunks.len() {
        return Err(corrupt("semantic vector batch chunk child count mismatch"));
    }
    if !matches!(
        (state, terminal),
        (SemanticVectorStageEffectState::Pending, None)
            | (SemanticVectorStageEffectState::Cancelled, None)
            | (SemanticVectorStageEffectState::Applied, Some(_))
            | (SemanticVectorStageEffectState::Failed, Some(_))
    ) {
        return Err(corrupt("semantic vector batch terminal digest mismatch"));
    }
    let should_be_applied = stage
        .record
        .applied_ordinal
        .is_some_and(|applied| receipt.key.ordinal <= applied);
    if should_be_applied != (state == SemanticVectorStageEffectState::Applied) {
        return Err(corrupt("semantic vector stage applied frontier mismatch"));
    }
    if stage.record.applied_ordinal == Some(receipt.key.ordinal)
        && (stage.record.applied_receipt_digest.as_ref() != Some(&receipt.receipt_digest)
            || stage.record.applied_checkpoint_digest.as_ref() != Some(&receipt.checkpoint_digest)
            || stage
                .record
                .applied_graph_batch_digest
                .as_ref()
                .map(|value| value.as_str())
                != terminal)
    {
        return Err(corrupt("semantic vector stage applied receipt mismatch"));
    }
    *chunks = chunks
        .checked_add(
            u64::try_from(receipt.chunks.len())
                .map_err(|_| corrupt("semantic vector stage chunk count exceeds u64"))?,
        )
        .ok_or_else(|| corrupt("semantic vector stage chunk count overflow"))?;
    *checkpoint = receipt.checkpoint_digest.clone();
    Ok(())
}

pub(super) fn ensure_binding(
    handle: &ExactSqlHandle,
    fence: &SemanticVectorWriterFence,
) -> SemanticVectorStagingStoreResult<()> {
    if handle.binding() != &fence.binding {
        return Err(SemanticVectorStagingStoreError::InvalidRequest(
            tracedecay_store::StorageRuntimeContractErrorV1::ReceiptBindingMismatch {
                field: "semantic vector live writer binding",
            },
        ));
    }
    Ok(())
}

pub(super) fn ensure_projection_binding(
    handle: &ExactSqlHandle,
    projection: &GraphProjectionIdentityV1,
) -> SemanticVectorStagingStoreResult<()> {
    if handle.binding().shard_id != projection.shard_id {
        return Err(SemanticVectorStagingStoreError::InvalidRequest(
            tracedecay_store::StorageRuntimeContractErrorV1::ShardMismatch {
                field: "semantic vector read projection",
            },
        ));
    }
    Ok(())
}

pub(super) fn ensure_live(
    context: &GraphPublicationOperationContextV1<'_>,
) -> SemanticVectorStagingStoreResult<()> {
    context.interruption().map_or(Ok(()), |interruption| {
        Err(SemanticVectorStagingStoreError::Interrupted(interruption))
    })
}

pub(super) fn begin_read_snapshot(
    handle: &ExactSqlHandle,
    context: &GraphPublicationOperationContextV1<'_>,
    wait: Duration,
) -> SemanticVectorStagingStoreResult<ExactSqlReadSnapshot> {
    match handle.begin_read_snapshot(wait) {
        Ok(snapshot) => Ok(snapshot),
        Err(error) => {
            ensure_live(context)?;
            Err(map_exact(error))
        }
    }
}

pub(super) fn begin_commit(
    context: &GraphPublicationOperationContextV1<'_>,
) -> SemanticVectorStagingStoreResult<()> {
    if context.try_begin_semantic_vector_stage_commit() {
        Ok(())
    } else if let Some(interruption) = context.interruption() {
        Err(SemanticVectorStagingStoreError::Interrupted(interruption))
    } else {
        Err(SemanticVectorStagingStoreError::ReusedOperationContext)
    }
}

pub(super) fn begin(
    handle: &ExactSqlHandle,
) -> SemanticVectorStagingStoreResult<ExactSqlTransaction> {
    handle.begin_immediate().map_err(map_exact)
}

pub(super) fn commit(tx: ExactSqlTransaction) -> SemanticVectorStagingStoreResult<()> {
    tx.commit().map(|_| ()).map_err(map_exact)
}

pub(super) fn rollback(tx: ExactSqlTransaction) -> SemanticVectorStagingStoreResult<()> {
    tx.rollback().map(|_| ()).map_err(map_exact)
}

pub(super) fn execute(
    tx: &ExactSqlTransaction,
    sql: &str,
    params: Vec<ExactSqlValue>,
) -> SemanticVectorStagingStoreResult<crate::exact_sql::ExactSqlExecuteResult> {
    tx.execute(statement(sql, params)?).map_err(map_exact)
}

pub(super) fn query(
    authority: &impl Query,
    sql: &str,
    params: Vec<ExactSqlValue>,
) -> SemanticVectorStagingStoreResult<ExactSqlRows> {
    authority.run(statement(sql, params)?).map_err(map_exact)
}

fn statement(
    sql: &str,
    params: Vec<ExactSqlValue>,
) -> SemanticVectorStagingStoreResult<ExactSqlStatement> {
    ExactSqlStatement::new(sql.to_owned(), params)
        .map_err(|_| SemanticVectorStagingStoreError::Infrastructure)
}

pub(super) fn projection_parts(
    projection: &GraphProjectionIdentityV1,
) -> SemanticVectorStagingStoreResult<(String, String, String)> {
    Ok((
        json(&projection.shard_id)?,
        projection.namespace.as_str().to_owned(),
        projection.projection.as_str().to_owned(),
    ))
}

pub(super) fn json<T: serde::Serialize + ?Sized>(
    value: &T,
) -> SemanticVectorStagingStoreResult<String> {
    serde_json::to_string(value).map_err(|error| corrupt(error.to_string()))
}

pub(super) fn decode_json<T: serde::de::DeserializeOwned>(
    value: &str,
) -> SemanticVectorStagingStoreResult<T> {
    serde_json::from_str(value).map_err(|error| corrupt(error.to_string()))
}

pub(super) fn text(value: impl Into<String>) -> ExactSqlValue {
    ExactSqlValue::Text(value.into())
}

pub(super) fn optional_text(value: Option<String>) -> ExactSqlValue {
    value.map_or(ExactSqlValue::Null, ExactSqlValue::Text)
}

pub(super) fn integer(value: u64) -> SemanticVectorStagingStoreResult<ExactSqlValue> {
    i64::try_from(value)
        .map(ExactSqlValue::Integer)
        .map_err(|_| invalid("semantic vector integer exceeds SQLite range"))
}

pub(super) fn text_at(row: &ExactSqlRow, index: usize) -> SemanticVectorStagingStoreResult<&str> {
    match row.values.get(index) {
        Some(ExactSqlValue::Text(value)) => Ok(value),
        _ => Err(corrupt(
            "semantic vector text column has wrong storage class",
        )),
    }
}

pub(super) fn optional_text_at(
    row: &ExactSqlRow,
    index: usize,
) -> SemanticVectorStagingStoreResult<Option<&str>> {
    match row.values.get(index) {
        Some(ExactSqlValue::Text(value)) => Ok(Some(value)),
        Some(ExactSqlValue::Null) => Ok(None),
        _ => Err(corrupt(
            "semantic vector optional text column has wrong storage class",
        )),
    }
}

pub(super) fn integer_at(row: &ExactSqlRow, index: usize) -> SemanticVectorStagingStoreResult<i64> {
    match row.values.get(index) {
        Some(ExactSqlValue::Integer(value)) if *value >= 0 => Ok(*value),
        _ => Err(corrupt("semantic vector integer column is invalid")),
    }
}

fn signed_integer_at(row: &ExactSqlRow, index: usize) -> SemanticVectorStagingStoreResult<i64> {
    match row.values.get(index) {
        Some(ExactSqlValue::Integer(value)) => Ok(*value),
        _ => Err(corrupt(
            "semantic vector signed integer column has wrong storage class",
        )),
    }
}

fn optional_integer_at(
    row: &ExactSqlRow,
    index: usize,
) -> SemanticVectorStagingStoreResult<Option<i64>> {
    match row.values.get(index) {
        Some(ExactSqlValue::Integer(value)) if *value >= 0 => Ok(Some(*value)),
        Some(ExactSqlValue::Null) => Ok(None),
        _ => Err(corrupt(
            "semantic vector optional integer column is invalid",
        )),
    }
}

pub(super) fn u64_at(row: &ExactSqlRow, index: usize) -> SemanticVectorStagingStoreResult<u64> {
    u64::try_from(integer_at(row, index)?)
        .map_err(|_| corrupt("semantic vector integer exceeds u64"))
}

pub(super) fn checked_u64(
    value: i64,
    field: &'static str,
) -> SemanticVectorStagingStoreResult<u64> {
    u64::try_from(value).map_err(|_| corrupt(format!("{field} is negative")))
}

fn optional_u64_at(
    row: &ExactSqlRow,
    index: usize,
) -> SemanticVectorStagingStoreResult<Option<u64>> {
    optional_integer_at(row, index)?
        .map(u64::try_from)
        .transpose()
        .map_err(|_| corrupt("semantic vector optional integer exceeds u64"))
}

pub(super) fn terminal(
    terminal: &SemanticVectorStageEffectTerminal,
) -> (SemanticVectorStageEffectState, &str) {
    match terminal {
        SemanticVectorStageEffectTerminal::Applied { graph_batch_digest } => (
            SemanticVectorStageEffectState::Applied,
            graph_batch_digest.as_str(),
        ),
        SemanticVectorStageEffectTerminal::Failed { failure_digest } => (
            SemanticVectorStageEffectState::Failed,
            failure_digest.as_str(),
        ),
    }
}

pub(super) fn effect_state(state: SemanticVectorStageEffectState) -> &'static str {
    match state {
        SemanticVectorStageEffectState::Pending => "pending",
        SemanticVectorStageEffectState::Applied => "applied",
        SemanticVectorStageEffectState::Failed => "failed",
        SemanticVectorStageEffectState::Cancelled => "cancelled",
    }
}

pub(super) fn invalid(message: &'static str) -> SemanticVectorStagingStoreError {
    SemanticVectorStagingStoreError::Corrupt(message.to_owned())
}

pub(super) fn corrupt(message: impl Into<String>) -> SemanticVectorStagingStoreError {
    SemanticVectorStagingStoreError::Corrupt(message.into())
}

pub(super) fn map_exact(error: ExactSqlError) -> SemanticVectorStagingStoreError {
    match error {
        ExactSqlError::AuthorityDenied(_) | ExactSqlError::AuthorityMismatch => {
            SemanticVectorStagingStoreError::AuthorityLost
        }
        ExactSqlError::Busy => SemanticVectorStagingStoreError::Busy,
        _ => SemanticVectorStagingStoreError::Infrastructure,
    }
}

pub(super) fn map_graph(
    error: tracedecay_store::GraphPublicationStoreErrorV1,
) -> SemanticVectorStagingStoreError {
    match error {
        tracedecay_store::GraphPublicationStoreErrorV1::InvalidRequest(error) => {
            SemanticVectorStagingStoreError::InvalidRequest(error)
        }
        tracedecay_store::GraphPublicationStoreErrorV1::Interrupted(interruption) => {
            SemanticVectorStagingStoreError::Interrupted(interruption)
        }
        tracedecay_store::GraphPublicationStoreErrorV1::Infrastructure => {
            SemanticVectorStagingStoreError::Infrastructure
        }
        tracedecay_store::GraphPublicationStoreErrorV1::Corrupt(message) => {
            SemanticVectorStagingStoreError::Corrupt(message)
        }
    }
}

pub(super) fn duplicate_chunk(
    authority: &impl Query,
    stage_id: i64,
    chunks: &[tracedecay_store::SemanticVectorStageChunkReceipt],
) -> SemanticVectorStagingStoreResult<Option<tracedecay_store::SemanticVectorChunkId>> {
    if chunks.is_empty() {
        return Ok(None);
    }
    let placeholders = (2..=chunks.len() + 1)
        .map(|index| format!("?{index}"))
        .collect::<Vec<_>>()
        .join(",");
    let mut params = Vec::with_capacity(chunks.len() + 1);
    params.push(ExactSqlValue::Integer(stage_id));
    params.extend(chunks.iter().map(|chunk| text(chunk.chunk_id.as_str())));
    let rows = query(
        authority,
        &format!(
            "SELECT chunk_id FROM semantic_vector_stage_chunk_receipts
             WHERE stage_id=?1 AND chunk_id IN ({placeholders})"
        ),
        params,
    )?;
    let existing = rows
        .rows
        .iter()
        .map(|row| text_at(row, 0).map(str::to_owned))
        .collect::<SemanticVectorStagingStoreResult<BTreeSet<_>>>()?;
    Ok(chunks
        .iter()
        .find(|chunk| existing.contains(chunk.chunk_id.as_str()))
        .map(|chunk| chunk.chunk_id.clone()))
}

pub(super) fn chunk_manifest_digest(
    authority: &impl Query,
    stage_id: i64,
    context: &GraphPublicationOperationContextV1<'_>,
) -> SemanticVectorStagingStoreResult<tracedecay_store::SemanticVectorChunkManifestDigest> {
    let mut after = String::new();
    let mut accumulator = SemanticVectorChunkManifestAccumulator::new();
    loop {
        ensure_live(context)?;
        let rows = query(
            authority,
            "SELECT chunk_id,chunk_digest,operation
             FROM semantic_vector_stage_chunk_receipts
             WHERE stage_id=?1 AND chunk_id>?2
             ORDER BY chunk_id ASC LIMIT 512",
            vec![
                ExactSqlValue::Integer(stage_id),
                ExactSqlValue::Text(after.clone()),
            ],
        )?;
        if rows.rows.is_empty() {
            break;
        }
        for row in &rows.rows {
            ensure_live(context)?;
            let operation = SemanticVectorStageChunkOperation::parse(text_at(row, 2)?)
                .map_err(|_| corrupt("unknown semantic vector chunk operation"))?;
            let member = SemanticVectorChunkManifestMember {
                chunk_id: tracedecay_store::SemanticVectorChunkId::new(text_at(row, 0)?)?,
                chunk_digest: tracedecay_store::SemanticVectorChunkDigest::new(text_at(row, 1)?)?,
                operation,
            };
            accumulator.push(&member)?;
            after = member.chunk_id.as_str().to_owned();
        }
    }
    ensure_live(context)?;
    accumulator.finish().map_err(Into::into)
}

pub(super) fn authoritative_verified_head(
    authority: &ExactSqlTransaction,
    projection_identity: &GraphProjectionIdentityV1,
) -> SemanticVectorStagingStoreResult<Option<tracedecay_store::GraphVerifiedHeadV1>> {
    crate::repository::graph_publication::authoritative_verified_head_in_transaction(
        authority,
        projection_identity,
    )
    .map_err(map_graph)
}

pub(super) fn publication_replay_conflict(
    authority: &impl Query,
    plan: &SemanticVectorStagePlan,
) -> SemanticVectorStagingStoreResult<bool> {
    let (shard, namespace, projection) = projection_parts(&plan.key.projection)?;
    let rows = query(
        authority,
        "SELECT 1 FROM (
            SELECT generation,idempotency_key
            FROM graph_publication_replay_v1
            WHERE shard_id=?1 AND namespace=?2 AND projection=?3
            UNION ALL
            SELECT generation,idempotency_key
            FROM graph_publication_replay_tombstones_v1
            WHERE shard_id=?1 AND namespace=?2 AND projection=?3
         )
         WHERE generation=?4 OR idempotency_key=?5
         LIMIT 1",
        vec![
            text(shard),
            text(namespace),
            text(projection),
            text(plan.publication_key.generation.as_str()),
            text(plan.publication_key.idempotency_key.as_str()),
        ],
    )?;
    Ok(!rows.rows.is_empty())
}
