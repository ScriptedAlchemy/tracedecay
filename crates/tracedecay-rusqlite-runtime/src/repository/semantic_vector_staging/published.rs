use tracedecay_store::{
    SemanticVectorPublishedGenerationKey, SemanticVectorStagePublicationPrepareRequest,
    SemanticVectorStageState, SemanticVectorStagingStoreResult,
};

use crate::exact_sql::ExactSqlTransaction;

use super::support::{Query, Stage, corrupt, map_graph, projection_parts, stage_query, text};

pub(super) fn published_stage_for(
    authority: &impl Query,
    key: &SemanticVectorPublishedGenerationKey,
) -> SemanticVectorStagingStoreResult<Option<Stage>> {
    let (shard, namespace, projection) = projection_parts(&key.projection)?;
    stage_query(
        authority,
        "WHERE shard_id=?1 AND namespace=?2 AND projection=?3
           AND semantic_generation_id=?4 AND state='published'",
        vec![
            text(shard),
            text(namespace),
            text(projection),
            text(key.semantic_generation_id.as_digest().as_str()),
        ],
    )
}

pub(super) fn published_stage_evidence(
    authority: &ExactSqlTransaction,
    stage: &Stage,
) -> SemanticVectorStagingStoreResult<tracedecay_store::GraphVerifiedHeadV1> {
    if stage.record.state != SemanticVectorStageState::Published {
        return Err(corrupt(
            "semantic vector published-generation lookup found a non-published stage",
        ));
    }
    let replay = crate::repository::graph_publication::active_replay_in_transaction(
        authority,
        &stage.record.plan.publication_key,
    )
    .map_err(map_graph)?
    .ok_or_else(|| corrupt("published semantic vector generation lost its active replay"))?;
    let intent =
        stage.record.publication_intent.as_ref().ok_or_else(|| {
            corrupt("published semantic vector generation has no publication intent")
        })?;
    let prepare = SemanticVectorStagePublicationPrepareRequest::new(
        stage.record.plan.key.clone(),
        replay.publication.clone(),
        stage.record.checkpoint_digest.clone(),
    )?;
    if intent.publication_key != replay.publication.key
        || intent.expected_recovered_digest != replay.publication.expected_recovered_digest
        || intent.publication_intent_digest != prepare.publication_intent_digest
        || replay.publication.key != stage.record.plan.publication_key
        || replay.publication.expected_prior_head != stage.record.plan.expected_prior_verified_head
    {
        return Err(corrupt(
            "published semantic vector generation replay intent mismatch",
        ));
    }
    let verified_head = tracedecay_store::GraphVerifiedHeadV1::from_replay(
        &replay,
        replay.publication.expected_recovered_digest.clone(),
    )?;
    Ok(verified_head)
}
