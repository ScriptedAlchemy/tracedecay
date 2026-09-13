use std::time::Duration;

use tracedecay_store::{
    GraphPublicationOperationContextV1, SemanticVectorProjectCensusReceipt,
    SemanticVectorStageCensusCounts, SemanticVectorStageCensusCursor,
    SemanticVectorStageCensusPage, SemanticVectorStageCensusRecord,
    SemanticVectorStageCensusRequest, SemanticVectorStageCensusRevision,
    SemanticVectorStagingStoreError, SemanticVectorStagingStoreResult,
};

use crate::exact_sql::ExactSqlValue;

use super::exact::SemanticVectorStagingExactSqlStorage;
use super::support::{
    begin_read_snapshot, decode_stage, ensure_live, ensure_projection_binding, integer, integer_at,
    projection_parts, query, text,
};

const READ_WAIT: Duration = Duration::from_millis(10);

pub(super) fn stage_census(
    storage: &SemanticVectorStagingExactSqlStorage,
    request: &SemanticVectorStageCensusRequest,
    context: &GraphPublicationOperationContextV1<'_>,
) -> SemanticVectorStagingStoreResult<SemanticVectorStageCensusPage> {
    ensure_live(context)?;
    if storage.handle.binding().shard_id != request.shard_id {
        return Err(SemanticVectorStagingStoreError::AuthorityLost);
    }
    if let Some(projection) = &request.projection {
        ensure_projection_binding(&storage.handle, projection)?;
    }
    let snapshot = begin_read_snapshot(&storage.handle, context, READ_WAIT)?;
    let shard = serde_json::to_string(&request.shard_id)
        .map_err(|_| SemanticVectorStagingStoreError::Infrastructure)?;
    let revision_rows = query(
        &snapshot,
        "SELECT revision FROM semantic_vector_stage_census_authority WHERE shard_id=?1",
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
                "semantic vector census has duplicate project revision rows".to_owned(),
            ));
        }
    };
    if let Some(cursor) = request.after.as_ref() {
        if cursor.shard_id != request.shard_id || cursor.projection != request.projection {
            return Err(SemanticVectorStagingStoreError::AuthorityLost);
        }
        if cursor.revision != revision {
            return Err(SemanticVectorStagingStoreError::CensusRevisionChanged {
                expected: cursor.revision,
                actual: revision,
            });
        }
    }
    let limit = u64::from(request.max_records)
        .checked_add(1)
        .ok_or(SemanticVectorStagingStoreError::Infrastructure)?;
    let after = request
        .after
        .as_ref()
        .map_or(Ok(0_i64), |cursor| i64::try_from(cursor.after_stage_id))
        .map_err(|_| SemanticVectorStagingStoreError::Infrastructure)?;
    let (mut cumulative_counts, mut cumulative_digest) = request.after.as_ref().map_or_else(
        || {
            tracedecay_domain::canonical_sha256(&"tracedecay.semantic-vector-project-census.v2")
                .map(|digest| (SemanticVectorStageCensusCounts::default(), digest))
                .map_err(|error| SemanticVectorStagingStoreError::Corrupt(error.to_string()))
        },
        |cursor| Ok((cursor.counts, cursor.record_digest.clone())),
    )?;
    let columns = "SELECT stage_id,plan_json,state,next_ordinal,checkpoint_digest,
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
                FROM semantic_vector_stages";
    let (sql, params) = if let Some(projection) = &request.projection {
        let (_, namespace, projection) = projection_parts(projection)?;
        (
            format!(
                "{columns} WHERE shard_id=?1 AND namespace=?2 AND projection=?3
                 AND stage_id>?4 ORDER BY stage_id ASC LIMIT ?5"
            ),
            vec![
                text(shard),
                text(namespace),
                text(projection),
                ExactSqlValue::Integer(after),
                integer(limit)?,
            ],
        )
    } else {
        (
            format!(
                "{columns} WHERE shard_id=?1 AND stage_id>?2
                 ORDER BY stage_id ASC LIMIT ?3"
            ),
            vec![text(shard), ExactSqlValue::Integer(after), integer(limit)?],
        )
    };
    let rows = query(&snapshot, &sql, params)?;
    let has_more = rows.rows.len() > usize::from(request.max_records);
    let records = rows
        .rows
        .iter()
        .take(usize::from(request.max_records))
        .map(|row| {
            let stage = decode_stage(row)?;
            cumulative_counts.checked_add_record(stage.record.state)?;
            cumulative_digest = tracedecay_domain::canonical_sha256(&(
                "tracedecay.semantic-vector-project-census-record.v2",
                &cumulative_digest,
                &stage.record,
            ))
            .map_err(|error| SemanticVectorStagingStoreError::Corrupt(error.to_string()))?;
            let cursor = SemanticVectorStageCensusCursor::new(
                request.shard_id.clone(),
                request.projection.clone(),
                revision,
                u64::try_from(stage.id).map_err(|_| {
                    SemanticVectorStagingStoreError::Corrupt(
                        "semantic vector census found a non-positive stage identity".to_owned(),
                    )
                })?,
                cumulative_counts,
                cumulative_digest.clone(),
            )?;
            Ok(SemanticVectorStageCensusRecord {
                cursor,
                stage: stage.record,
            })
        })
        .collect::<SemanticVectorStagingStoreResult<Vec<_>>>()?;
    let continuation = has_more
        .then(|| records.last().map(|record| record.cursor.clone()))
        .flatten();
    let complete_receipt = (!has_more).then(|| SemanticVectorProjectCensusReceipt {
        shard_id: request.shard_id.clone(),
        revision,
        counts: cumulative_counts,
        record_digest: cumulative_digest,
    });
    ensure_live(context)?;
    SemanticVectorStageCensusPage::new(
        request.shard_id.clone(),
        request.projection.clone(),
        revision,
        records,
        continuation,
        complete_receipt,
        request.max_records,
    )
    .map_err(Into::into)
}
