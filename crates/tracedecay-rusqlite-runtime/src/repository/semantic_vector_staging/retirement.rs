use tracedecay_store::{
    GraphPublicationOperationContextV1, GraphReplayRetirementOutcomeV1,
    SemanticVectorCancelledRetirement, SemanticVectorCancelledRetirementOutcome,
    SemanticVectorCensusDependencyV1, SemanticVectorPublishedGenerationDependencyLookup,
    SemanticVectorPublishedRetirement, SemanticVectorPublishedRetirementOutcome,
    SemanticVectorRetirementCleanupCursor, SemanticVectorRetirementCleanupRecord,
    SemanticVectorStageCensusRevision, SemanticVectorStagePlan, SemanticVectorStageState,
    SemanticVectorStagingStoreError, SemanticVectorStagingStoreResult,
};

use crate::exact_sql::{ExactSqlTransaction, ExactSqlValue};

use super::exact::SemanticVectorStagingExactSqlStorage;
use super::support::{
    begin, begin_commit, commit, corrupt, decode_json, ensure_binding, ensure_live, execute, json,
    map_graph, projection_parts, query, rollback, stage_by_key, text, text_at,
    validate_stage_history,
};

pub(super) fn retire_published_generation(
    storage: &SemanticVectorStagingExactSqlStorage,
    request: &SemanticVectorPublishedRetirement,
    context: &GraphPublicationOperationContextV1<'_>,
) -> SemanticVectorStagingStoreResult<SemanticVectorPublishedRetirementOutcome> {
    request.validate()?;
    ensure_live(context)?;
    ensure_binding(&storage.handle, &request.writer_fence)?;
    let tx = begin(&storage.handle)?;
    let stage = stage_by_key(&tx, &request.stage)?;
    if let Some(stage) = &stage {
        if stage.record.state != SemanticVectorStageState::Published {
            rollback(tx)?;
            return Ok(SemanticVectorPublishedRetirementOutcome::Conflict);
        }
        if stage.record.plan.semantic_generation_id != request.semantic_generation_id
            || stage.record.plan.publication_key != request.replay.key
        {
            rollback(tx)?;
            return Ok(SemanticVectorPublishedRetirementOutcome::Conflict);
        }
        validate_stage_history(&tx, stage, context)?;
    }
    let graph_outcome =
        crate::repository::graph_publication::retire_replay_in_transaction(&tx, &request.replay)
            .map_err(map_graph)?;
    let outcome = match graph_outcome {
        GraphReplayRetirementOutcomeV1::Retired(tombstone) => {
            let Some(stage) = stage else {
                rollback(tx)?;
                return Err(corrupt(
                    "semantic vector replay retired without its published stage",
                ));
            };
            insert_cleanup(&tx, request)?;
            delete_stage_descendants(&tx, stage.id)?;
            SemanticVectorPublishedRetirementOutcome::Retired(tombstone)
        }
        GraphReplayRetirementOutcomeV1::ExactReplay(tombstone) => {
            if let Some(stage) = stage {
                insert_cleanup(&tx, request)?;
                delete_stage_descendants(&tx, stage.id)?;
            } else {
                rollback(tx)?;
                ensure_live(context)?;
                return Ok(SemanticVectorPublishedRetirementOutcome::ExactReplay(
                    tombstone,
                ));
            }
            SemanticVectorPublishedRetirementOutcome::ExactReplay(tombstone)
        }
        GraphReplayRetirementOutcomeV1::CurrentVerifiedHead { head } => {
            rollback(tx)?;
            return Ok(SemanticVectorPublishedRetirementOutcome::CurrentVerifiedHead { head });
        }
        GraphReplayRetirementOutcomeV1::PendingReplay { .. } => {
            rollback(tx)?;
            return Ok(SemanticVectorPublishedRetirementOutcome::PendingReplay);
        }
        GraphReplayRetirementOutcomeV1::Conflict => {
            rollback(tx)?;
            return Ok(SemanticVectorPublishedRetirementOutcome::Conflict);
        }
        GraphReplayRetirementOutcomeV1::Missing => {
            rollback(tx)?;
            return Ok(SemanticVectorPublishedRetirementOutcome::Missing);
        }
    };
    ensure_live(context)?;
    ensure_binding(&storage.handle, &request.writer_fence)?;
    begin_commit(context)?;
    commit(tx)?;
    Ok(outcome)
}

pub(super) fn generation_has_live_base_reference(
    storage: &SemanticVectorStagingExactSqlStorage,
    shard_id: &tracedecay_store::StoreShardIdV1,
    generation: &tracedecay_domain::VectorGenerationIdV1,
    expected_revision: SemanticVectorStageCensusRevision,
    context: &GraphPublicationOperationContextV1<'_>,
) -> SemanticVectorStagingStoreResult<bool> {
    ensure_live(context)?;
    if &storage.handle.binding().shard_id != shard_id {
        return Err(SemanticVectorStagingStoreError::AuthorityLost);
    }
    let tx = begin(&storage.handle)?;
    require_census_revision(&tx, shard_id, expected_revision)?;
    let rows = query(
        &tx,
        "SELECT 1 FROM semantic_vector_stages
         WHERE shard_id=?1 AND base_generation=?2
           AND state IN ('pending','ready_to_publish','published') LIMIT 1",
        vec![text(json(shard_id)?), text(generation.as_digest().as_str())],
    )?;
    let found = !rows.rows.is_empty();
    rollback(tx)?;
    ensure_live(context)?;
    Ok(found)
}

pub(super) fn published_generation_exists(
    storage: &SemanticVectorStagingExactSqlStorage,
    shard_id: &tracedecay_store::StoreShardIdV1,
    generation: &tracedecay_domain::VectorGenerationIdV1,
    context: &GraphPublicationOperationContextV1<'_>,
) -> SemanticVectorStagingStoreResult<bool> {
    live_reference_exists(
        storage,
        shard_id,
        "semantic_generation_id",
        generation.as_digest().as_str(),
        "state='published'",
        None,
        context,
    )
}

pub(super) fn source_generation_has_live_reference(
    storage: &SemanticVectorStagingExactSqlStorage,
    shard_id: &tracedecay_store::StoreShardIdV1,
    generation: &tracedecay_store::SemanticVectorSourceGenerationId,
    expected_revision: SemanticVectorStageCensusRevision,
    context: &GraphPublicationOperationContextV1<'_>,
) -> SemanticVectorStagingStoreResult<bool> {
    live_reference_exists(
        storage,
        shard_id,
        "source_generation",
        generation.as_str(),
        "state IN ('pending','ready_to_publish','published')",
        Some(expected_revision),
        context,
    )
}

pub(super) fn source_scope_has_live_reference(
    storage: &SemanticVectorStagingExactSqlStorage,
    shard_id: &tracedecay_store::StoreShardIdV1,
    source_scope: &tracedecay_store::StoreShardIdV1,
    expected_revision: SemanticVectorStageCensusRevision,
    context: &GraphPublicationOperationContextV1<'_>,
) -> SemanticVectorStagingStoreResult<bool> {
    live_reference_exists(
        storage,
        shard_id,
        "source_scope",
        &json(source_scope)?,
        "state IN ('pending','ready_to_publish','published')",
        Some(expected_revision),
        context,
    )
}

fn live_reference_exists(
    storage: &SemanticVectorStagingExactSqlStorage,
    shard_id: &tracedecay_store::StoreShardIdV1,
    column: &str,
    value: &str,
    state_predicate: &str,
    expected_revision: Option<SemanticVectorStageCensusRevision>,
    context: &GraphPublicationOperationContextV1<'_>,
) -> SemanticVectorStagingStoreResult<bool> {
    ensure_live(context)?;
    if &storage.handle.binding().shard_id != shard_id {
        return Err(SemanticVectorStagingStoreError::AuthorityLost);
    }
    let tx = begin(&storage.handle)?;
    if let Some(expected_revision) = expected_revision {
        require_census_revision(&tx, shard_id, expected_revision)?;
    }
    let rows = query(
        &tx,
        &format!(
            "SELECT 1 FROM semantic_vector_stages
             WHERE shard_id=?1 AND {column}=?2 AND {state_predicate} LIMIT 1"
        ),
        vec![text(json(shard_id)?), text(value)],
    )?;
    let found = !rows.rows.is_empty();
    rollback(tx)?;
    ensure_live(context)?;
    Ok(found)
}

pub(super) fn published_generation_dependency(
    storage: &SemanticVectorStagingExactSqlStorage,
    shard_id: &tracedecay_store::StoreShardIdV1,
    generation: &tracedecay_domain::VectorGenerationIdV1,
    expected_revision: SemanticVectorStageCensusRevision,
    context: &GraphPublicationOperationContextV1<'_>,
) -> SemanticVectorStagingStoreResult<SemanticVectorPublishedGenerationDependencyLookup> {
    ensure_live(context)?;
    if &storage.handle.binding().shard_id != shard_id {
        return Err(SemanticVectorStagingStoreError::AuthorityLost);
    }
    let tx = begin(&storage.handle)?;
    require_census_revision(&tx, shard_id, expected_revision)?;
    let rows = query(
        &tx,
        "SELECT plan_json,state,source_scope,source_generation,source_dependency,code_scope_hash
         FROM semantic_vector_stages
         WHERE shard_id=?1 AND semantic_generation_id=?2 AND state='published'
         ORDER BY stage_id ASC LIMIT 2",
        vec![text(json(shard_id)?), text(generation.as_digest().as_str())],
    )?;
    let outcome = match rows.rows.as_slice() {
        [] => SemanticVectorPublishedGenerationDependencyLookup::Missing,
        [row] => {
            if text_at(row, 1)? != "published" {
                rollback(tx)?;
                return Err(corrupt(
                    "semantic vector published dependency has non-published state",
                ));
            }
            let plan: SemanticVectorStagePlan = decode_json(text_at(row, 0)?)?;
            plan.validate()?;
            if plan.key.projection.shard_id != *shard_id
                || plan.semantic_generation_id != *generation
                || plan.source_scope != decode_json(text_at(row, 2)?)?
                || plan.source_generation.as_str() != text_at(row, 3)?
                || plan.source_dependency
                    != decode_json::<tracedecay_store::SemanticVectorSourceDependencyV1>(text_at(
                        row, 4,
                    )?)?
                || plan.code_scope_hash.as_str() != text_at(row, 5)?
            {
                rollback(tx)?;
                return Err(corrupt(
                    "semantic vector published dependency identity is inconsistent",
                ));
            }
            SemanticVectorPublishedGenerationDependencyLookup::Published(Box::new(
                SemanticVectorCensusDependencyV1 {
                    semantic_generation_id: plan.semantic_generation_id,
                    source_scope: plan.source_scope,
                    code_scope_hash: plan.code_scope_hash,
                    source_generation: plan.source_generation,
                    source_dependency: plan.source_dependency,
                    stage_state: SemanticVectorStageState::Published,
                },
            ))
        }
        _ => {
            rollback(tx)?;
            return Err(corrupt(
                "semantic vector generation has multiple published dependencies",
            ));
        }
    };
    rollback(tx)?;
    ensure_live(context)?;
    Ok(outcome)
}

pub(super) fn validate_project_census_revision(
    storage: &SemanticVectorStagingExactSqlStorage,
    shard_id: &tracedecay_store::StoreShardIdV1,
    expected_revision: SemanticVectorStageCensusRevision,
    context: &GraphPublicationOperationContextV1<'_>,
) -> SemanticVectorStagingStoreResult<()> {
    ensure_live(context)?;
    if &storage.handle.binding().shard_id != shard_id {
        return Err(SemanticVectorStagingStoreError::AuthorityLost);
    }
    let tx = begin(&storage.handle)?;
    require_census_revision(&tx, shard_id, expected_revision)?;
    rollback(tx)?;
    ensure_live(context)
}

pub(super) fn source_scope_binding(
    storage: &SemanticVectorStagingExactSqlStorage,
    shard_id: &tracedecay_store::StoreShardIdV1,
    code_scope_hash: &tracedecay_store::SemanticVectorCodeScopeHash,
    expected_revision: SemanticVectorStageCensusRevision,
    context: &GraphPublicationOperationContextV1<'_>,
) -> SemanticVectorStagingStoreResult<tracedecay_store::SemanticVectorSourceScopeBindingLookup> {
    ensure_live(context)?;
    if &storage.handle.binding().shard_id != shard_id {
        return Err(SemanticVectorStagingStoreError::AuthorityLost);
    }
    let tx = begin(&storage.handle)?;
    require_census_revision(&tx, shard_id, expected_revision)?;
    let rows = query(
        &tx,
        "SELECT source_scope FROM semantic_vector_source_scope_bindings
         WHERE shard_id=?1 AND code_scope_hash=?2
         ORDER BY source_scope ASC LIMIT 2",
        vec![text(json(shard_id)?), text(code_scope_hash.as_str())],
    )?;
    let binding = match rows.rows.as_slice() {
        [] => tracedecay_store::SemanticVectorSourceScopeBindingLookup::Missing,
        [row] => {
            let source_scope = decode_json(text_at(row, 0)?)?;
            tracedecay_store::SemanticVectorSourceScopeBindingLookup::Exact(source_scope)
        }
        _ => tracedecay_store::SemanticVectorSourceScopeBindingLookup::Conflict,
    };
    rollback(tx)?;
    ensure_live(context)?;
    Ok(binding)
}

pub(super) fn remove_source_scope_binding(
    storage: &SemanticVectorStagingExactSqlStorage,
    shard_id: &tracedecay_store::StoreShardIdV1,
    code_scope_hash: &tracedecay_store::SemanticVectorCodeScopeHash,
    source_scope: &tracedecay_store::StoreShardIdV1,
    expected_revision: SemanticVectorStageCensusRevision,
    context: &GraphPublicationOperationContextV1<'_>,
) -> SemanticVectorStagingStoreResult<bool> {
    ensure_live(context)?;
    if &storage.handle.binding().shard_id != shard_id {
        return Err(SemanticVectorStagingStoreError::AuthorityLost);
    }
    let tx = begin(&storage.handle)?;
    require_census_revision(&tx, shard_id, expected_revision)?;
    let live = query(
        &tx,
        "SELECT 1 FROM semantic_vector_stages
         WHERE shard_id=?1 AND source_scope=?2
           AND state IN ('pending','ready_to_publish','published') LIMIT 1",
        vec![text(json(shard_id)?), text(json(source_scope)?)],
    )?;
    if !live.rows.is_empty() {
        rollback(tx)?;
        return Ok(false);
    }
    let deleted = execute(
        &tx,
        "DELETE FROM semantic_vector_source_scope_bindings
         WHERE shard_id=?1 AND code_scope_hash=?2 AND source_scope=?3",
        vec![
            text(json(shard_id)?),
            text(code_scope_hash.as_str()),
            text(json(source_scope)?),
        ],
    )?;
    if deleted.changed_rows > 1 {
        rollback(tx)?;
        return Err(corrupt(
            "semantic vector source-scope cleanup removed multiple bindings",
        ));
    }
    if deleted.changed_rows == 0 {
        rollback(tx)?;
        return Ok(false);
    }
    ensure_live(context)?;
    begin_commit(context)?;
    commit(tx)?;
    Ok(true)
}

fn require_census_revision(
    tx: &ExactSqlTransaction,
    shard_id: &tracedecay_store::StoreShardIdV1,
    expected: SemanticVectorStageCensusRevision,
) -> SemanticVectorStagingStoreResult<()> {
    let rows = query(
        tx,
        "SELECT revision FROM semantic_vector_stage_census_authority WHERE shard_id=?1",
        vec![text(json(shard_id)?)],
    )?;
    let actual = match rows.rows.as_slice() {
        [] => SemanticVectorStageCensusRevision::INITIAL,
        [row] => SemanticVectorStageCensusRevision::new(
            u64::try_from(super::support::integer_at(row, 0)?)
                .map_err(|_| corrupt("semantic vector census revision exceeds u64"))?,
        )?,
        _ => {
            return Err(corrupt(
                "semantic vector census has duplicate project revision rows",
            ));
        }
    };
    if actual != expected {
        return Err(SemanticVectorStagingStoreError::CensusRevisionChanged { expected, actual });
    }
    Ok(())
}

pub(super) fn pending_retirement_cleanup(
    storage: &SemanticVectorStagingExactSqlStorage,
    shard_id: &tracedecay_store::StoreShardIdV1,
    context: &GraphPublicationOperationContextV1<'_>,
) -> SemanticVectorStagingStoreResult<Option<SemanticVectorRetirementCleanupRecord>> {
    ensure_live(context)?;
    if &storage.handle.binding().shard_id != shard_id {
        return Err(SemanticVectorStagingStoreError::AuthorityLost);
    }
    let tx = begin(&storage.handle)?;
    let rows = query(
        &tx,
        "SELECT cleanup_id,retirement_json FROM semantic_vector_retirement_cleanup
         WHERE shard_id=?1 ORDER BY cleanup_id ASC LIMIT 1",
        vec![text(json(shard_id)?)],
    )?;
    let record = rows
        .rows
        .first()
        .map(
            |row| -> SemanticVectorStagingStoreResult<SemanticVectorRetirementCleanupRecord> {
                let cleanup_id =
                    u64::try_from(super::support::integer_at(row, 0)?).map_err(|_| {
                        corrupt("semantic vector retirement cleanup identity is not positive")
                    })?;
                let retirement: SemanticVectorPublishedRetirement =
                    serde_json::from_str(text_at(row, 1)?)
                        .map_err(|_| corrupt("semantic vector retirement cleanup is malformed"))?;
                retirement.validate()?;
                Ok(SemanticVectorRetirementCleanupRecord {
                    cursor: SemanticVectorRetirementCleanupCursor::new(cleanup_id)?,
                    retirement,
                })
            },
        )
        .transpose()?;
    rollback(tx)?;
    ensure_live(context)?;
    Ok(record)
}

pub(super) fn complete_retirement_cleanup(
    storage: &SemanticVectorStagingExactSqlStorage,
    retirement: &SemanticVectorPublishedRetirement,
    context: &GraphPublicationOperationContextV1<'_>,
) -> SemanticVectorStagingStoreResult<bool> {
    retirement.validate()?;
    ensure_live(context)?;
    ensure_binding(&storage.handle, &retirement.writer_fence)?;
    let tx = begin(&storage.handle)?;
    let (shard, namespace, projection) = projection_parts(&retirement.stage.projection)?;
    let deleted = execute(
        &tx,
        "DELETE FROM semantic_vector_retirement_cleanup
         WHERE shard_id=?1 AND namespace=?2 AND projection=?3
           AND semantic_generation_id=?4
           AND publication_generation=?5 AND publication_idempotency_key=?6",
        vec![
            text(shard),
            text(namespace),
            text(projection),
            text(retirement.semantic_generation_id.as_digest().as_str()),
            text(retirement.replay.key.generation.as_str()),
            text(retirement.replay.key.idempotency_key.as_str()),
        ],
    )?;
    if deleted.changed_rows == 0 {
        rollback(tx)?;
        return Ok(false);
    }
    if deleted.changed_rows != 1 {
        rollback(tx)?;
        return Err(corrupt(
            "semantic vector retirement cleanup removed multiple rows",
        ));
    }
    begin_commit(context)?;
    commit(tx)?;
    Ok(true)
}

fn insert_cleanup(
    tx: &ExactSqlTransaction,
    request: &SemanticVectorPublishedRetirement,
) -> SemanticVectorStagingStoreResult<()> {
    let (shard, namespace, projection) = projection_parts(&request.stage.projection)?;
    let inserted = execute(
        tx,
        "INSERT OR IGNORE INTO semantic_vector_retirement_cleanup (
            shard_id,namespace,projection,semantic_generation_id,
            publication_generation,publication_idempotency_key,retirement_json
         ) VALUES (?1,?2,?3,?4,?5,?6,?7)",
        vec![
            text(shard),
            text(namespace),
            text(projection),
            text(request.semantic_generation_id.as_digest().as_str()),
            text(request.replay.key.generation.as_str()),
            text(request.replay.key.idempotency_key.as_str()),
            text(json(request)?),
        ],
    )?;
    if inserted.changed_rows == 0 {
        let rows = query(
            tx,
            "SELECT retirement_json FROM semantic_vector_retirement_cleanup
             WHERE shard_id=?1 AND namespace=?2 AND projection=?3
               AND semantic_generation_id=?4",
            vec![
                text(json(&request.stage.projection.shard_id)?),
                text(request.stage.projection.namespace.as_str()),
                text(request.stage.projection.projection.as_str()),
                text(request.semantic_generation_id.as_digest().as_str()),
            ],
        )?;
        if rows
            .rows
            .first()
            .map(|row| text_at(row, 0))
            .transpose()?
            .as_deref()
            != Some(json(request)?.as_str())
        {
            return Err(corrupt(
                "semantic vector retirement cleanup identity conflict",
            ));
        }
    }
    Ok(())
}

pub(super) fn remove_cancelled_generation(
    storage: &SemanticVectorStagingExactSqlStorage,
    request: &SemanticVectorCancelledRetirement,
    context: &GraphPublicationOperationContextV1<'_>,
) -> SemanticVectorStagingStoreResult<SemanticVectorCancelledRetirementOutcome> {
    request
        .writer_fence
        .validate_for(&request.stage.projection)?;
    ensure_live(context)?;
    ensure_binding(&storage.handle, &request.writer_fence)?;
    let tx = begin(&storage.handle)?;
    let Some(stage) = stage_by_key(&tx, &request.stage)? else {
        rollback(tx)?;
        return Ok(SemanticVectorCancelledRetirementOutcome::ExactMissing);
    };
    if stage.record.state != SemanticVectorStageState::Cancelled {
        let record = stage.record;
        rollback(tx)?;
        return Ok(SemanticVectorCancelledRetirementOutcome::NotCancelled(
            Box::new(record),
        ));
    }
    validate_stage_history(&tx, &stage, context)?;
    delete_stage_descendants(&tx, stage.id)?;
    ensure_live(context)?;
    ensure_binding(&storage.handle, &request.writer_fence)?;
    begin_commit(context)?;
    commit(tx)?;
    Ok(SemanticVectorCancelledRetirementOutcome::Removed)
}

fn delete_stage_descendants(
    tx: &ExactSqlTransaction,
    stage_id: i64,
) -> SemanticVectorStagingStoreResult<()> {
    let id = || vec![ExactSqlValue::Integer(stage_id)];
    execute(
        tx,
        "DELETE FROM semantic_vector_stage_graph_effects
         WHERE batch_id IN (
             SELECT batch_id FROM semantic_vector_stage_batches WHERE stage_id=?1
         )",
        id(),
    )?;
    execute(
        tx,
        "DELETE FROM semantic_vector_stage_chunk_receipts WHERE stage_id=?1",
        id(),
    )?;
    execute(
        tx,
        "DELETE FROM semantic_vector_stage_batches WHERE stage_id=?1",
        id(),
    )?;
    let deleted = execute(
        tx,
        "DELETE FROM semantic_vector_stages WHERE stage_id=?1",
        id(),
    )?;
    if deleted.changed_rows != 1 {
        return Err(corrupt(
            "semantic vector retirement did not remove exactly one stage",
        ));
    }
    Ok(())
}
