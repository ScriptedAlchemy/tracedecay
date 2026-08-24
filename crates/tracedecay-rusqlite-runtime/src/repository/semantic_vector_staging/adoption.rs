use std::time::Duration;

use tracedecay_store::{
    GraphPublicationOperationContextV1, SemanticVectorStageAdoptionCursor,
    SemanticVectorStageAdoptionPage, SemanticVectorStageAdoptionPageRequest,
    SemanticVectorStageAdoptionRecord, SemanticVectorStageCensusRevision,
    SemanticVectorStagingStoreError, SemanticVectorStagingStoreResult,
};

use crate::exact_sql::ExactSqlValue;

use super::exact::SemanticVectorStagingExactSqlStorage;
use super::support::{
    begin_read_snapshot, decode_stage, ensure_live, integer, integer_at, json, query, text,
};

const READ_WAIT: Duration = Duration::from_millis(10);

pub(super) fn adoptable_stage_page(
    storage: &SemanticVectorStagingExactSqlStorage,
    request: &SemanticVectorStageAdoptionPageRequest,
    context: &GraphPublicationOperationContextV1<'_>,
) -> SemanticVectorStagingStoreResult<SemanticVectorStageAdoptionPage> {
    ensure_live(context)?;
    if storage.handle.binding() != &request.binding {
        return Err(SemanticVectorStagingStoreError::AuthorityLost);
    }
    let snapshot = begin_read_snapshot(&storage.handle, context, READ_WAIT)?;
    let shard = json(&request.binding.shard_id)?;
    let revision_rows = query(
        &snapshot,
        "SELECT revision FROM semantic_vector_stage_adoption_authority WHERE shard_id=?1",
        vec![text(shard.clone())],
    )?;
    let revision = match revision_rows.rows.as_slice() {
        [] => SemanticVectorStageCensusRevision::INITIAL,
        [row] => SemanticVectorStageCensusRevision::new(
            u64::try_from(integer_at(row, 0)?)
                .map_err(|_| SemanticVectorStagingStoreError::Infrastructure)?,
        )?,
        _ => {
            return Err(SemanticVectorStagingStoreError::Corrupt(
                "semantic vector adoption scan has duplicate revision rows".to_owned(),
            ));
        }
    };
    if let Some(cursor) = request.after.as_ref() {
        if cursor.binding != request.binding {
            return Err(SemanticVectorStagingStoreError::AuthorityLost);
        }
        if cursor.revision != revision {
            return Err(SemanticVectorStagingStoreError::CensusRevisionChanged {
                expected: cursor.revision,
                actual: revision,
            });
        }
    }
    let after = request
        .after
        .as_ref()
        .map_or(Ok(0_i64), |cursor| i64::try_from(cursor.after_stage_id))
        .map_err(|_| SemanticVectorStagingStoreError::Infrastructure)?;
    let rows = query(
        &snapshot,
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
         FROM semantic_vector_stages
         WHERE shard_id=?1 AND state IN ('pending','ready_to_publish')
           AND writer_binding<>?2 AND stage_id>?3
         ORDER BY stage_id ASC LIMIT ?4",
        vec![
            text(shard),
            text(json(&request.binding)?),
            ExactSqlValue::Integer(after),
            integer(u64::from(request.max_records) + 1)?,
        ],
    )?;
    let has_more = rows.rows.len() > usize::from(request.max_records);
    let records = rows
        .rows
        .iter()
        .take(usize::from(request.max_records))
        .map(|row| {
            ensure_live(context)?;
            let stage = decode_stage(row)?;
            let cursor = SemanticVectorStageAdoptionCursor::new(
                request.binding.clone(),
                revision,
                u64::try_from(stage.id).map_err(|_| {
                    SemanticVectorStagingStoreError::Corrupt(
                        "semantic vector adoption scan found invalid stage identity".to_owned(),
                    )
                })?,
            )?;
            Ok(SemanticVectorStageAdoptionRecord {
                cursor,
                stage: stage.record,
            })
        })
        .collect::<SemanticVectorStagingStoreResult<Vec<_>>>()?;
    let continuation = has_more
        .then(|| records.last().map(|record| record.cursor.clone()))
        .flatten();
    ensure_live(context)?;
    SemanticVectorStageAdoptionPage::new(
        request.binding.clone(),
        revision,
        records,
        continuation,
        request.max_records,
    )
    .map_err(Into::into)
}
