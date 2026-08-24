use tracedecay_store::{
    GraphPublicationOperationContextV1, SemanticVectorPublishedGenerationKey,
    SemanticVectorStagePublishOutcome, SemanticVectorStagePublishSettlement,
    SemanticVectorStageState, SemanticVectorStagingStoreResult, SemanticVectorWriterFence,
};

use crate::exact_sql::ExactSqlValue;

use super::exact::SemanticVectorStagingExactSqlStorage;
use super::published::{published_stage_evidence, published_stage_for};
use super::support::*;

pub(super) fn settle_published(
    storage: &SemanticVectorStagingExactSqlStorage,
    settlement: &SemanticVectorStagePublishSettlement,
    fence: &SemanticVectorWriterFence,
    context: &GraphPublicationOperationContextV1<'_>,
) -> SemanticVectorStagingStoreResult<SemanticVectorStagePublishOutcome> {
    ensure_live(context)?;
    ensure_binding(&storage.handle, fence)?;
    let tx = begin(&storage.handle)?;
    let Some(stage) = stage_by_key(&tx, &settlement.stage)? else {
        rollback(tx)?;
        return Ok(SemanticVectorStagePublishOutcome::MissingStage);
    };
    if stage.record.plan.writer_fence != *fence {
        let actual = stage.record.plan.writer_fence;
        rollback(tx)?;
        return Ok(SemanticVectorStagePublishOutcome::StaleFence { actual });
    }
    if stage.record.state == SemanticVectorStageState::Published {
        validate_stage_history(&tx, &stage, context)?;
        let intent_exact = stage
            .record
            .publication_intent
            .as_ref()
            .is_some_and(|intent| {
                settlement.verified_head.key == intent.publication_key
                    && settlement.verified_head.recovered_digest == intent.expected_recovered_digest
            });
        let exact =
            intent_exact && published_stage_evidence(&tx, &stage)? == settlement.verified_head;
        let record = stage.record;
        rollback(tx)?;
        return Ok(if exact {
            SemanticVectorStagePublishOutcome::ExactReplay(record)
        } else {
            SemanticVectorStagePublishOutcome::VerifiedHeadConflict
        });
    }
    if stage.record.state != SemanticVectorStageState::ReadyToPublish {
        let record = stage.record;
        rollback(tx)?;
        return Ok(SemanticVectorStagePublishOutcome::NotReady(record));
    }
    let published_key = SemanticVectorPublishedGenerationKey {
        projection: stage.record.plan.key.projection.clone(),
        semantic_generation_id: stage.record.plan.semantic_generation_id.clone(),
    };
    if let Some(existing) = published_stage_for(&tx, &published_key)? {
        let record = existing.record;
        rollback(tx)?;
        return Ok(
            SemanticVectorStagePublishOutcome::SemanticGenerationConflict { existing: record },
        );
    }
    let Some(intent) = stage.record.publication_intent.as_ref() else {
        rollback(tx)?;
        return Err(corrupt(
            "ready semantic vector stage has no publication intent",
        ));
    };
    if settlement.verified_head.key != intent.publication_key
        || settlement.verified_head.recovered_digest != intent.expected_recovered_digest
    {
        rollback(tx)?;
        return Ok(SemanticVectorStagePublishOutcome::VerifiedHeadConflict);
    }
    let actual_head = authoritative_verified_head(&tx, &stage.record.plan.key.projection)?;
    if actual_head.as_ref() != Some(&settlement.verified_head) {
        rollback(tx)?;
        return Ok(SemanticVectorStagePublishOutcome::VerifiedHeadConflict);
    }
    begin_commit(context)?;
    ensure_binding(&storage.handle, fence)?;
    let updated = execute(
        &tx,
        "UPDATE semantic_vector_stages SET state='published'
         WHERE stage_id=?1 AND state='ready_to_publish'",
        vec![ExactSqlValue::Integer(stage.id)],
    )?;
    if updated.changed_rows != 1 {
        rollback(tx)?;
        return Err(corrupt(
            "semantic vector publication settlement did not update one ready stage",
        ));
    }
    let record = stage_by_key(&tx, &settlement.stage)?
        .ok_or_else(|| corrupt("published semantic vector stage is missing"))?
        .record;
    commit(tx)?;
    Ok(SemanticVectorStagePublishOutcome::Published(record))
}
