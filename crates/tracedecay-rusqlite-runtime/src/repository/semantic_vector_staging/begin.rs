use tracedecay_store::{
    GraphPublicationOperationContextV1, SemanticVectorStageBeginOutcome, SemanticVectorStagePlan,
    SemanticVectorStagingStoreResult,
};

use crate::exact_sql::{ExactSqlTransaction, ExactSqlValue};

use super::exact::SemanticVectorStagingExactSqlStorage;
use super::published::*;
use super::support::*;

pub(super) fn begin_stage(
    storage: &SemanticVectorStagingExactSqlStorage,
    plan: &SemanticVectorStagePlan,
    context: &GraphPublicationOperationContextV1<'_>,
) -> SemanticVectorStagingStoreResult<SemanticVectorStageBeginOutcome> {
    plan.validate()?;
    ensure_live(context)?;
    ensure_binding(&storage.handle, &plan.writer_fence)?;
    let tx = begin(&storage.handle)?;
    let published_key = tracedecay_store::SemanticVectorPublishedGenerationKey {
        projection: plan.key.projection.clone(),
        semantic_generation_id: plan.semantic_generation_id.clone(),
    };
    if let Some(existing) = published_stage_for(&tx, &published_key)? {
        validate_stage_history(&tx, &existing, context)?;
        let exact_semantic_plan = existing.record.plan.source_scope == plan.source_scope
            && existing.record.plan.code_scope_hash == plan.code_scope_hash
            && existing.record.plan.source_generation == plan.source_generation
            && existing.record.plan.source_dependency == plan.source_dependency
            && existing.record.plan.recipe == plan.recipe
            && existing.record.plan.expected_chunk_count == plan.expected_chunk_count;
        let verified_head = published_stage_evidence(&tx, &existing)?;
        let record = existing.record;
        rollback(tx)?;
        return Ok(if exact_semantic_plan {
            SemanticVectorStageBeginOutcome::Published {
                record,
                verified_head,
            }
        } else {
            SemanticVectorStageBeginOutcome::SemanticGenerationConflict { existing: record }
        });
    }
    if let Some(existing) = stage_by_key(&tx, &plan.key)? {
        let outcome = if existing.record.plan == *plan {
            SemanticVectorStageBeginOutcome::ExactReplay(existing.record)
        } else {
            SemanticVectorStageBeginOutcome::InputConflict {
                existing: existing.record,
            }
        };
        rollback(tx)?;
        return Ok(outcome);
    }
    let actual_head = authoritative_verified_head(&tx, &plan.key.projection)?;
    if actual_head != plan.expected_prior_verified_head {
        rollback(tx)?;
        return Ok(SemanticVectorStageBeginOutcome::PriorVerifiedHeadConflict {
            actual: actual_head,
        });
    }
    if publication_identity_conflict(&tx, plan)? {
        rollback(tx)?;
        return Ok(SemanticVectorStageBeginOutcome::PublicationConflict);
    }
    if let Some(existing) = pending_stage_for(&tx, &plan.key.projection)? {
        rollback(tx)?;
        return Ok(SemanticVectorStageBeginOutcome::InputConflict {
            existing: existing.record,
        });
    }
    begin_commit(context)?;
    ensure_binding(&storage.handle, &plan.writer_fence)?;
    let (shard, namespace, projection) = projection_parts(&plan.key.projection)?;
    execute(
        &tx,
        "INSERT INTO semantic_vector_stages (
            shard_id, namespace, projection, build_id, plan_digest, semantic_generation_id,
            base_generation, publication_generation, publication_idempotency_key,
            source_scope, source_generation, source_dependency, source_manifest_digest,
            embedding_projection_digest, embedding_dimension, model_artifact_digest,
            projection_manifest_digest, privacy_domain_digest,
            privacy_key_epoch, expected_chunk_manifest_digest,
            expected_chunk_count, expected_prior_verified_head,
            writer_binding, code_scope_hash, plan_json, state,
            next_ordinal, checkpoint_digest, recorded_chunk_count
         ) VALUES (
            ?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,
            ?17,?18,?19,?20,?21,?22,?23,?24,?25,'pending',0,?26,0
         )",
        vec![
            text(shard),
            text(namespace),
            text(projection),
            text(plan.key.build_id.as_str()),
            text(plan.key.plan_digest.as_str()),
            text(plan.semantic_generation_id.as_digest().as_str()),
            optional_text(
                plan.base_generation
                    .as_ref()
                    .map(|generation| generation.as_digest().as_str().to_owned()),
            ),
            text(plan.publication_key.generation.as_str()),
            text(plan.publication_key.idempotency_key.as_str()),
            text(json(&plan.source_scope)?),
            text(plan.source_generation.as_str()),
            text(json(&plan.source_dependency)?),
            text(plan.recipe.source_manifest_digest.as_str()),
            text(plan.recipe.embedding_projection_digest.as_str()),
            ExactSqlValue::Integer(i64::from(plan.recipe.embedding_dimension)),
            text(plan.recipe.model_artifact_digest.as_str()),
            text(plan.recipe.projection_manifest_digest.as_str()),
            text(plan.recipe.privacy_domain_digest.as_str()),
            integer(plan.recipe.privacy_key_epoch)?,
            text(plan.recipe.expected_chunk_manifest_digest.as_str()),
            integer(plan.expected_chunk_count)?,
            optional_text(
                plan.expected_prior_verified_head
                    .as_ref()
                    .map(json)
                    .transpose()?,
            ),
            text(json(&plan.writer_fence.binding)?),
            text(plan.code_scope_hash.as_str()),
            text(json(plan)?),
            text(plan.initial_checkpoint_digest.as_str()),
        ],
    )?;
    admit_source_scope_binding(&tx, plan)?;
    let record = stage_by_key(&tx, &plan.key)?
        .ok_or_else(|| corrupt("inserted semantic vector stage is missing"))?
        .record;
    commit(tx)?;
    Ok(SemanticVectorStageBeginOutcome::Begun(record))
}

fn admit_source_scope_binding(
    tx: &ExactSqlTransaction,
    plan: &SemanticVectorStagePlan,
) -> SemanticVectorStagingStoreResult<()> {
    let shard_id = json(&plan.key.projection.shard_id)?;
    let source_scope = json(&plan.source_scope)?;
    execute(
        tx,
        "INSERT OR IGNORE INTO semantic_vector_source_scope_bindings (
            shard_id,code_scope_hash,source_scope
         ) VALUES (?1,?2,?3)",
        vec![
            text(shard_id.clone()),
            text(plan.code_scope_hash.as_str()),
            text(source_scope.clone()),
        ],
    )?;
    let rows = query(
        tx,
        "SELECT code_scope_hash,source_scope
         FROM semantic_vector_source_scope_bindings
         WHERE shard_id=?1 AND (code_scope_hash=?2 OR source_scope=?3)
         ORDER BY code_scope_hash ASC LIMIT 2",
        vec![
            text(shard_id),
            text(plan.code_scope_hash.as_str()),
            text(source_scope.clone()),
        ],
    )?;
    if rows.rows.as_slice().len() != 1
        || text_at(&rows.rows[0], 0)? != plan.code_scope_hash.as_str()
        || text_at(&rows.rows[0], 1)? != source_scope
    {
        return Err(corrupt(
            "semantic vector code scope has a conflicting durable source binding",
        ));
    }
    Ok(())
}

fn publication_identity_conflict(
    authority: &impl Query,
    plan: &SemanticVectorStagePlan,
) -> SemanticVectorStagingStoreResult<bool> {
    let (shard, namespace, projection) = projection_parts(&plan.key.projection)?;
    let rows = query(
        authority,
        "SELECT 1 FROM (
            SELECT publication_generation AS generation,
                   publication_idempotency_key AS idempotency_key
            FROM semantic_vector_stages
            WHERE shard_id=?1 AND namespace=?2 AND projection=?3
            UNION ALL
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
