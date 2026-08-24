use std::time::Duration;

use tracedecay_store::{
    GraphProjectionIdentityV1, GraphPublicationOperationContextV1,
    SemanticVectorCancelledRetirement, SemanticVectorCancelledRetirementOutcome,
    SemanticVectorOutboxSequence, SemanticVectorPublishedRetirement,
    SemanticVectorPublishedRetirementOutcome, SemanticVectorReadyPublicationCursor,
    SemanticVectorReadyPublicationPage, SemanticVectorReadyPublicationPageRequest,
    SemanticVectorRetirementCleanupRecord, SemanticVectorStageAdoptionPage,
    SemanticVectorStageAdoptionPageRequest, SemanticVectorStageAppendOutcome,
    SemanticVectorStageBatchKey, SemanticVectorStageBatchPage, SemanticVectorStageBatchPageRequest,
    SemanticVectorStageBatchReceipt, SemanticVectorStageBatchReceiptLookup,
    SemanticVectorStageBeginOutcome, SemanticVectorStageCancelOutcome,
    SemanticVectorStageCensusPage, SemanticVectorStageCensusRequest,
    SemanticVectorStageEffectState, SemanticVectorStageGraphBatchEffect,
    SemanticVectorStageIncomplete, SemanticVectorStageKey, SemanticVectorStagePendingEffectPage,
    SemanticVectorStagePendingEffectPageRequest, SemanticVectorStagePlan,
    SemanticVectorStagePublicationPrepareOutcome, SemanticVectorStagePublicationPrepareRequest,
    SemanticVectorStagePublishOutcome, SemanticVectorStagePublishSettlement,
    SemanticVectorStageRecord, SemanticVectorStageSettlement, SemanticVectorStageSettlementOutcome,
    SemanticVectorStageState, SemanticVectorStageWriterAdoption,
    SemanticVectorStageWriterAdoptionOutcome, SemanticVectorStagingStore,
    SemanticVectorStagingStoreError, SemanticVectorStagingStoreResult, SemanticVectorWriterFence,
};

use crate::exact_sql::{ExactSqlHandle, ExactSqlValue};

use super::super::graph_publication::GraphPublicationExactSqlStorage;
use super::published::*;
use super::support::*;

const READ_WAIT: Duration = Duration::from_millis(10);

#[derive(Clone)]
pub struct SemanticVectorStagingExactSqlStorage {
    pub(super) handle: ExactSqlHandle,
    pub(super) graph_publication: GraphPublicationExactSqlStorage,
}

impl SemanticVectorStagingExactSqlStorage {
    pub fn from_authorized_handle(
        handle: ExactSqlHandle,
    ) -> SemanticVectorStagingStoreResult<Self> {
        Self::from_authorized_handle_with_guard(handle, ())
    }

    pub fn from_authorized_handle_with_guard<Guard>(
        handle: ExactSqlHandle,
        guard: Guard,
    ) -> SemanticVectorStagingStoreResult<Self>
    where
        Guard: Send + Sync + 'static,
    {
        Ok(Self {
            graph_publication: GraphPublicationExactSqlStorage::from_authorized_handle_with_guard(
                handle.clone(),
                guard,
            )
            .map_err(map_graph)?,
            handle,
        })
    }
}

impl SemanticVectorStagingStore for SemanticVectorStagingExactSqlStorage {
    fn retire_published_generation(
        &mut self,
        request: &SemanticVectorPublishedRetirement,
        context: &GraphPublicationOperationContextV1<'_>,
    ) -> SemanticVectorStagingStoreResult<SemanticVectorPublishedRetirementOutcome> {
        super::retirement::retire_published_generation(self, request, context)
    }

    fn remove_cancelled_generation(
        &mut self,
        request: &SemanticVectorCancelledRetirement,
        context: &GraphPublicationOperationContextV1<'_>,
    ) -> SemanticVectorStagingStoreResult<SemanticVectorCancelledRetirementOutcome> {
        super::retirement::remove_cancelled_generation(self, request, context)
    }

    fn generation_has_live_base_reference(
        &mut self,
        shard_id: &tracedecay_store::StoreShardIdV1,
        generation: &tracedecay_domain::VectorGenerationIdV1,
        expected_revision: tracedecay_store::SemanticVectorStageCensusRevision,
        context: &GraphPublicationOperationContextV1<'_>,
    ) -> SemanticVectorStagingStoreResult<bool> {
        super::retirement::generation_has_live_base_reference(
            self,
            shard_id,
            generation,
            expected_revision,
            context,
        )
    }

    fn published_generation_exists(
        &mut self,
        shard_id: &tracedecay_store::StoreShardIdV1,
        generation: &tracedecay_domain::VectorGenerationIdV1,
        context: &GraphPublicationOperationContextV1<'_>,
    ) -> SemanticVectorStagingStoreResult<bool> {
        super::retirement::published_generation_exists(self, shard_id, generation, context)
    }

    fn source_generation_has_live_reference(
        &mut self,
        shard_id: &tracedecay_store::StoreShardIdV1,
        generation: &tracedecay_store::SemanticVectorSourceGenerationId,
        expected_revision: tracedecay_store::SemanticVectorStageCensusRevision,
        context: &GraphPublicationOperationContextV1<'_>,
    ) -> SemanticVectorStagingStoreResult<bool> {
        super::retirement::source_generation_has_live_reference(
            self,
            shard_id,
            generation,
            expected_revision,
            context,
        )
    }

    fn source_scope_has_live_reference(
        &mut self,
        shard_id: &tracedecay_store::StoreShardIdV1,
        source_scope: &tracedecay_store::StoreShardIdV1,
        expected_revision: tracedecay_store::SemanticVectorStageCensusRevision,
        context: &GraphPublicationOperationContextV1<'_>,
    ) -> SemanticVectorStagingStoreResult<bool> {
        super::retirement::source_scope_has_live_reference(
            self,
            shard_id,
            source_scope,
            expected_revision,
            context,
        )
    }

    fn published_generation_dependency(
        &mut self,
        shard_id: &tracedecay_store::StoreShardIdV1,
        generation: &tracedecay_domain::VectorGenerationIdV1,
        expected_revision: tracedecay_store::SemanticVectorStageCensusRevision,
        context: &GraphPublicationOperationContextV1<'_>,
    ) -> SemanticVectorStagingStoreResult<
        tracedecay_store::SemanticVectorPublishedGenerationDependencyLookup,
    > {
        super::retirement::published_generation_dependency(
            self,
            shard_id,
            generation,
            expected_revision,
            context,
        )
    }

    fn validate_project_census_revision(
        &mut self,
        shard_id: &tracedecay_store::StoreShardIdV1,
        expected_revision: tracedecay_store::SemanticVectorStageCensusRevision,
        context: &GraphPublicationOperationContextV1<'_>,
    ) -> SemanticVectorStagingStoreResult<()> {
        super::retirement::validate_project_census_revision(
            self,
            shard_id,
            expected_revision,
            context,
        )
    }

    fn source_scope_binding(
        &mut self,
        shard_id: &tracedecay_store::StoreShardIdV1,
        code_scope_hash: &tracedecay_store::SemanticVectorCodeScopeHash,
        expected_revision: tracedecay_store::SemanticVectorStageCensusRevision,
        context: &GraphPublicationOperationContextV1<'_>,
    ) -> SemanticVectorStagingStoreResult<tracedecay_store::SemanticVectorSourceScopeBindingLookup>
    {
        super::retirement::source_scope_binding(
            self,
            shard_id,
            code_scope_hash,
            expected_revision,
            context,
        )
    }

    fn remove_source_scope_binding(
        &mut self,
        shard_id: &tracedecay_store::StoreShardIdV1,
        code_scope_hash: &tracedecay_store::SemanticVectorCodeScopeHash,
        source_scope: &tracedecay_store::StoreShardIdV1,
        expected_revision: tracedecay_store::SemanticVectorStageCensusRevision,
        context: &GraphPublicationOperationContextV1<'_>,
    ) -> SemanticVectorStagingStoreResult<bool> {
        super::retirement::remove_source_scope_binding(
            self,
            shard_id,
            code_scope_hash,
            source_scope,
            expected_revision,
            context,
        )
    }

    fn pending_retirement_cleanup(
        &mut self,
        shard_id: &tracedecay_store::StoreShardIdV1,
        context: &GraphPublicationOperationContextV1<'_>,
    ) -> SemanticVectorStagingStoreResult<Option<SemanticVectorRetirementCleanupRecord>> {
        super::retirement::pending_retirement_cleanup(self, shard_id, context)
    }

    fn complete_retirement_cleanup(
        &mut self,
        retirement: &SemanticVectorPublishedRetirement,
        context: &GraphPublicationOperationContextV1<'_>,
    ) -> SemanticVectorStagingStoreResult<bool> {
        super::retirement::complete_retirement_cleanup(self, retirement, context)
    }

    fn stage_census(
        &mut self,
        request: &SemanticVectorStageCensusRequest,
        context: &GraphPublicationOperationContextV1<'_>,
    ) -> SemanticVectorStagingStoreResult<SemanticVectorStageCensusPage> {
        super::census::stage_census(self, request, context)
    }

    fn adoptable_stage_page(
        &mut self,
        request: &SemanticVectorStageAdoptionPageRequest,
        context: &GraphPublicationOperationContextV1<'_>,
    ) -> SemanticVectorStagingStoreResult<SemanticVectorStageAdoptionPage> {
        super::adoption::adoptable_stage_page(self, request, context)
    }

    fn begin_stage(
        &mut self,
        plan: &SemanticVectorStagePlan,
        context: &GraphPublicationOperationContextV1<'_>,
    ) -> SemanticVectorStagingStoreResult<SemanticVectorStageBeginOutcome> {
        super::begin::begin_stage(self, plan, context)
    }

    fn append_stage_batch(
        &mut self,
        receipt: &SemanticVectorStageBatchReceipt,
        fence: &SemanticVectorWriterFence,
        context: &GraphPublicationOperationContextV1<'_>,
    ) -> SemanticVectorStagingStoreResult<SemanticVectorStageAppendOutcome> {
        receipt.validate()?;
        ensure_live(context)?;
        ensure_binding(&self.handle, fence)?;
        let tx = begin(&self.handle)?;
        let Some(stage) = stage_by_key(&tx, &receipt.key.stage)? else {
            rollback(tx)?;
            return Ok(SemanticVectorStageAppendOutcome::MissingStage);
        };
        if stage.record.plan.writer_fence != *fence {
            let actual = stage.record.plan.writer_fence;
            rollback(tx)?;
            return Ok(SemanticVectorStageAppendOutcome::StaleFence { actual });
        }
        if stage.record.state != SemanticVectorStageState::Pending {
            let state = stage.record.state;
            let record = stage.record;
            rollback(tx)?;
            return Ok(if state == SemanticVectorStageState::Cancelled {
                SemanticVectorStageAppendOutcome::Cancelled(record)
            } else {
                SemanticVectorStageAppendOutcome::ReadyToPublish(record)
            });
        }
        if let Some((batch_id, existing)) = receipt_by_ordinal(&tx, stage.id, receipt.key.ordinal)?
        {
            let effect = effect_by_batch(&tx, batch_id, existing.clone())?;
            rollback(tx)?;
            return Ok(if existing == *receipt {
                SemanticVectorStageAppendOutcome::ExactReplay {
                    receipt: existing,
                    effect,
                }
            } else {
                SemanticVectorStageAppendOutcome::InputConflict { existing }
            });
        }
        if receipt.key.ordinal != stage.record.next_ordinal {
            let next_ordinal = stage.record.next_ordinal;
            rollback(tx)?;
            return Ok(SemanticVectorStageAppendOutcome::StaleOrdinal { next_ordinal });
        }
        if receipt.expected_checkpoint_digest != stage.record.checkpoint_digest {
            let actual = stage.record.checkpoint_digest;
            rollback(tx)?;
            return Ok(SemanticVectorStageAppendOutcome::StaleCheckpoint { actual });
        }
        let control_batch = receipt.chunks.is_empty();
        if (control_batch
            && (stage.record.plan.expected_chunk_count != 0 || stage.record.next_ordinal != 0))
            || (!control_batch && stage.record.plan.expected_chunk_count == 0)
        {
            rollback(tx)?;
            return Err(SemanticVectorStagingStoreError::InvalidRequest(
                tracedecay_store::StorageRuntimeContractErrorV1::ReceiptBindingMismatch {
                    field: "semantic vector empty-corpus control batch",
                },
            ));
        }
        let receipt_chunk_count = u64::try_from(receipt.chunks.len())
            .map_err(|_| corrupt("semantic vector receipt chunk count exceeds u64"))?;
        let next_chunks = stage
            .record
            .recorded_chunk_count
            .checked_add(receipt_chunk_count)
            .ok_or_else(|| corrupt("semantic vector chunk count overflow"))?;
        if next_chunks > stage.record.plan.expected_chunk_count {
            rollback(tx)?;
            return Err(SemanticVectorStagingStoreError::InvalidRequest(
                tracedecay_store::StorageRuntimeContractErrorV1::LimitExceeded {
                    field: "semantic vector recorded chunks",
                    actual: next_chunks,
                    max: stage.record.plan.expected_chunk_count,
                },
            ));
        }
        if let Some(chunk_id) = duplicate_chunk(&tx, stage.id, &receipt.chunks)? {
            rollback(tx)?;
            return Ok(SemanticVectorStageAppendOutcome::DuplicateChunk { chunk_id });
        }
        begin_commit(context)?;
        ensure_binding(&self.handle, fence)?;
        let inserted = execute(
            &tx,
            "INSERT INTO semantic_vector_stage_batches (
                stage_id, ordinal, expected_checkpoint_digest, input_digest,
                output_digest, receipt_digest, checkpoint_digest, chunk_count,
                receipt_json
             ) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9)",
            vec![
                ExactSqlValue::Integer(stage.id),
                integer(receipt.key.ordinal)?,
                text(receipt.expected_checkpoint_digest.as_str()),
                text(receipt.input_digest.as_str()),
                text(receipt.output_digest.as_str()),
                text(receipt.receipt_digest.as_str()),
                text(receipt.checkpoint_digest.as_str()),
                integer(receipt_chunk_count)?,
                text(json(receipt)?),
            ],
        )?;
        let batch_id = inserted.last_insert_rowid;
        if batch_id <= 0 {
            rollback(tx)?;
            return Err(corrupt("semantic vector batch rowid is not positive"));
        }
        for chunk in &receipt.chunks {
            execute(
                &tx,
                "INSERT INTO semantic_vector_stage_chunk_receipts (
                    stage_id,batch_id,effect_ordinal,chunk_id,chunk_digest,operation,output_digest
                 ) VALUES (?1,?2,?3,?4,?5,?6,?7)",
                vec![
                    ExactSqlValue::Integer(stage.id),
                    ExactSqlValue::Integer(batch_id),
                    ExactSqlValue::Integer(i64::from(chunk.effect_ordinal)),
                    text(chunk.chunk_id.as_str()),
                    text(chunk.chunk_digest.as_str()),
                    text(chunk.operation.as_str()),
                    optional_text(
                        chunk
                            .output_digest
                            .as_ref()
                            .map(|digest| digest.as_str().to_owned()),
                    ),
                ],
            )?;
        }
        let effect_insert = execute(
            &tx,
            "INSERT INTO semantic_vector_stage_graph_effects (batch_id,state)
             VALUES (?1,'pending')",
            vec![ExactSqlValue::Integer(batch_id)],
        )?;
        execute(
            &tx,
            "UPDATE semantic_vector_stages
             SET next_ordinal=next_ordinal+1,checkpoint_digest=?2,
                 recorded_chunk_count=?3 WHERE stage_id=?1",
            vec![
                ExactSqlValue::Integer(stage.id),
                text(receipt.checkpoint_digest.as_str()),
                integer(next_chunks)?,
            ],
        )?;
        let next = stage_by_key(&tx, &receipt.key.stage)?
            .ok_or_else(|| corrupt("advanced semantic vector stage is missing"))?
            .record;
        let effect = SemanticVectorStageGraphBatchEffect {
            sequence: SemanticVectorOutboxSequence::new(checked_u64(
                effect_insert.last_insert_rowid,
                "semantic vector outbox sequence",
            )?)?,
            receipt: receipt.clone(),
            state: SemanticVectorStageEffectState::Pending,
            terminal_digest: None,
        };
        commit(tx)?;
        Ok(SemanticVectorStageAppendOutcome::Appended {
            stage: Box::new(next),
            effect,
        })
    }

    fn stage(
        &mut self,
        key: &SemanticVectorStageKey,
        context: &GraphPublicationOperationContextV1<'_>,
    ) -> SemanticVectorStagingStoreResult<Option<SemanticVectorStageRecord>> {
        super::read::stage(self, key, context)
    }

    fn pending_stage(
        &mut self,
        projection: &GraphProjectionIdentityV1,
        context: &GraphPublicationOperationContextV1<'_>,
    ) -> SemanticVectorStagingStoreResult<Option<SemanticVectorStageRecord>> {
        super::read::pending_stage(self, projection, context)
    }

    fn batch_receipt(
        &mut self,
        key: &SemanticVectorStageBatchKey,
        context: &GraphPublicationOperationContextV1<'_>,
    ) -> SemanticVectorStagingStoreResult<SemanticVectorStageBatchReceiptLookup> {
        super::read::batch_receipt(self, key, context)
    }

    fn batch_page(
        &mut self,
        request: &SemanticVectorStageBatchPageRequest,
        context: &GraphPublicationOperationContextV1<'_>,
    ) -> SemanticVectorStagingStoreResult<SemanticVectorStageBatchPage> {
        super::read::batch_page(self, request, context)
    }

    fn pending_effects(
        &mut self,
        request: &SemanticVectorStagePendingEffectPageRequest,
        context: &GraphPublicationOperationContextV1<'_>,
    ) -> SemanticVectorStagingStoreResult<SemanticVectorStagePendingEffectPage> {
        super::read::pending_effects(self, request, context)
    }

    fn settle_stage_batch(
        &mut self,
        settlement: &SemanticVectorStageSettlement,
        fence: &SemanticVectorWriterFence,
        context: &GraphPublicationOperationContextV1<'_>,
    ) -> SemanticVectorStagingStoreResult<SemanticVectorStageSettlementOutcome> {
        ensure_live(context)?;
        ensure_binding(&self.handle, fence)?;
        let tx = begin(&self.handle)?;
        let Some(stage) = stage_by_key(&tx, &settlement.batch.stage)? else {
            rollback(tx)?;
            return Ok(SemanticVectorStageSettlementOutcome::MissingBatch);
        };
        if stage.record.plan.writer_fence != *fence {
            let actual = stage.record.plan.writer_fence;
            rollback(tx)?;
            return Ok(SemanticVectorStageSettlementOutcome::StaleFence { actual });
        }
        if stage.record.state == SemanticVectorStageState::Cancelled {
            let record = stage.record;
            rollback(tx)?;
            return Ok(SemanticVectorStageSettlementOutcome::Cancelled(Box::new(
                record,
            )));
        }
        let Some((batch_id, receipt)) =
            receipt_by_ordinal(&tx, stage.id, settlement.batch.ordinal)?
        else {
            rollback(tx)?;
            return Ok(SemanticVectorStageSettlementOutcome::MissingBatch);
        };
        let existing = effect_by_batch(&tx, batch_id, receipt.clone())?;
        if receipt.receipt_digest != settlement.expected_receipt_digest {
            rollback(tx)?;
            return Ok(SemanticVectorStageSettlementOutcome::Conflict(existing));
        }
        let (state, terminal) = terminal(&settlement.terminal);
        if existing.state != SemanticVectorStageEffectState::Pending {
            rollback(tx)?;
            return Ok(
                if existing.state == state && existing.terminal_digest.as_deref() == Some(terminal)
                {
                    SemanticVectorStageSettlementOutcome::ExactReplay(existing)
                } else {
                    SemanticVectorStageSettlementOutcome::Conflict(existing)
                },
            );
        }
        let next_applied = stage
            .record
            .applied_ordinal
            .map_or(0, |ordinal| ordinal + 1);
        if settlement.batch.ordinal != next_applied {
            rollback(tx)?;
            return Ok(SemanticVectorStageSettlementOutcome::StaleOrdinal {
                next_applied_ordinal: next_applied,
            });
        }
        begin_commit(context)?;
        ensure_binding(&self.handle, fence)?;
        execute(
            &tx,
            "UPDATE semantic_vector_stage_graph_effects
             SET state=?2,terminal_digest=?3 WHERE batch_id=?1",
            vec![
                ExactSqlValue::Integer(batch_id),
                text(effect_state(state)),
                text(terminal),
            ],
        )?;
        if state == SemanticVectorStageEffectState::Applied {
            execute(
                &tx,
                "UPDATE semantic_vector_stages
                 SET applied_ordinal=?2,applied_receipt_digest=?3,
                     applied_checkpoint_digest=?4,applied_graph_batch_digest=?5
                 WHERE stage_id=?1",
                vec![
                    ExactSqlValue::Integer(stage.id),
                    integer(settlement.batch.ordinal)?,
                    text(receipt.receipt_digest.as_str()),
                    text(receipt.checkpoint_digest.as_str()),
                    text(terminal),
                ],
            )?;
        }
        let effect = SemanticVectorStageGraphBatchEffect {
            sequence: existing.sequence,
            receipt,
            state,
            terminal_digest: Some(terminal.to_owned()),
        };
        commit(tx)?;
        Ok(SemanticVectorStageSettlementOutcome::Settled(effect))
    }

    fn cancel_stage(
        &mut self,
        key: &SemanticVectorStageKey,
        fence: &SemanticVectorWriterFence,
        context: &GraphPublicationOperationContextV1<'_>,
    ) -> SemanticVectorStagingStoreResult<SemanticVectorStageCancelOutcome> {
        ensure_live(context)?;
        ensure_binding(&self.handle, fence)?;
        let tx = begin(&self.handle)?;
        let Some(stage) = stage_by_key(&tx, key)? else {
            rollback(tx)?;
            return Ok(SemanticVectorStageCancelOutcome::MissingStage);
        };
        if stage.record.plan.writer_fence != *fence {
            let actual = stage.record.plan.writer_fence;
            rollback(tx)?;
            return Ok(SemanticVectorStageCancelOutcome::StaleFence { actual });
        }
        if stage.record.state != SemanticVectorStageState::Pending {
            let state = stage.record.state;
            let record = stage.record;
            rollback(tx)?;
            return Ok(if state == SemanticVectorStageState::Cancelled {
                SemanticVectorStageCancelOutcome::ExactReplay(record)
            } else {
                SemanticVectorStageCancelOutcome::ReadyToPublish(record)
            });
        }
        begin_commit(context)?;
        ensure_binding(&self.handle, fence)?;
        execute(
            &tx,
            "UPDATE semantic_vector_stage_graph_effects SET state='cancelled'
             WHERE state='pending' AND batch_id IN (
                SELECT batch_id FROM semantic_vector_stage_batches WHERE stage_id=?1
             )",
            vec![ExactSqlValue::Integer(stage.id)],
        )?;
        let cancelled = execute(
            &tx,
            "UPDATE semantic_vector_stages SET state='cancelled'
             WHERE stage_id=?1 AND state='pending'",
            vec![ExactSqlValue::Integer(stage.id)],
        )?;
        if cancelled.changed_rows != 1 {
            rollback(tx)?;
            return Err(corrupt(
                "semantic vector cancellation did not terminalize one pending stage",
            ));
        }
        let record = stage_by_key(&tx, key)?
            .ok_or_else(|| corrupt("cancelled semantic vector stage is missing"))?
            .record;
        commit(tx)?;
        Ok(SemanticVectorStageCancelOutcome::Cancelled(record))
    }

    fn adopt_stage_writer(
        &mut self,
        request: &SemanticVectorStageWriterAdoption,
        context: &GraphPublicationOperationContextV1<'_>,
    ) -> SemanticVectorStagingStoreResult<SemanticVectorStageWriterAdoptionOutcome> {
        request.expected.validate_for(&request.stage.projection)?;
        request
            .replacement
            .validate_for(&request.stage.projection)?;
        ensure_live(context)?;
        ensure_binding(&self.handle, &request.replacement)?;
        let tx = begin(&self.handle)?;
        let Some(stage) = stage_by_key(&tx, &request.stage)? else {
            rollback(tx)?;
            return Ok(SemanticVectorStageWriterAdoptionOutcome::MissingStage);
        };
        let exact_replay = stage.record.plan.writer_fence == request.replacement;
        if !exact_replay && stage.record.plan.writer_fence != request.expected {
            let actual = stage.record.plan.writer_fence;
            rollback(tx)?;
            return Ok(SemanticVectorStageWriterAdoptionOutcome::StaleFence { actual });
        }
        if !matches!(
            stage.record.state,
            SemanticVectorStageState::Pending | SemanticVectorStageState::ReadyToPublish
        ) {
            let record = stage.record;
            rollback(tx)?;
            return Ok(SemanticVectorStageWriterAdoptionOutcome::NotAdoptable(
                record,
            ));
        }
        match (
            stage.record.state,
            request.ready_publication_replay.as_ref(),
        ) {
            (SemanticVectorStageState::Pending, None) => {
                let actual = authoritative_verified_head(&tx, &stage.record.plan.key.projection)?;
                if actual != stage.record.plan.expected_prior_verified_head {
                    rollback(tx)?;
                    return Ok(
                        SemanticVectorStageWriterAdoptionOutcome::VerifiedHeadConflict { actual },
                    );
                }
                if publication_replay_conflict(&tx, &stage.record.plan)? {
                    rollback(tx)?;
                    return Err(corrupt(
                        "pending semantic vector stage already has a publication replay",
                    ));
                }
            }
            (SemanticVectorStageState::ReadyToPublish, Some(replay))
                if replay.key == stage.record.plan.publication_key
                    && replay.expected_prior_head
                        == stage.record.plan.expected_prior_verified_head
                    && stage
                        .record
                        .publication_intent
                        .as_ref()
                        .is_some_and(|intent| {
                            intent.publication_key == replay.key
                                && intent.expected_recovered_digest
                                    == replay.expected_recovered_digest
                                && SemanticVectorStagePublicationPrepareRequest::new(
                                    stage.record.plan.key.clone(),
                                    replay.clone(),
                                    stage.record.checkpoint_digest.clone(),
                                )
                                .is_ok_and(|request| {
                                    request.publication_intent_digest
                                        == intent.publication_intent_digest
                                })
                        }) =>
            {
                let actual = authoritative_verified_head(&tx, &stage.record.plan.key.projection)?;
                let outcome =
                    crate::repository::graph_publication::append_replay_in_transaction(&tx, replay)
                        .map_err(map_graph)?;
                let exact = match outcome {
                    tracedecay_store::GraphReplayAppendOutcomeV1::ExactReplay(_) => {
                        actual == stage.record.plan.expected_prior_verified_head
                    }
                    tracedecay_store::GraphReplayAppendOutcomeV1::ExactVerifiedReplay {
                        receipt,
                        ..
                    } => actual.as_ref() == Some(receipt.as_ref()),
                    _ => false,
                };
                if !exact {
                    rollback(tx)?;
                    return Ok(
                        SemanticVectorStageWriterAdoptionOutcome::VerifiedHeadConflict { actual },
                    );
                }
            }
            _ => {
                rollback(tx)?;
                return Err(SemanticVectorStagingStoreError::InvalidRequest(
                    tracedecay_store::StorageRuntimeContractErrorV1::ReceiptBindingMismatch {
                        field: "semantic vector writer adoption replay",
                    },
                ));
            }
        }
        validate_stage_history(&tx, &stage, context)?;
        if exact_replay {
            let record = stage.record;
            rollback(tx)?;
            return Ok(SemanticVectorStageWriterAdoptionOutcome::ExactReplay(
                record,
            ));
        }
        begin_commit(context)?;
        ensure_binding(&self.handle, &request.replacement)?;
        let mut plan = stage.record.plan;
        plan.writer_fence = request.replacement.clone();
        plan.validate()?;
        let adopted = execute(
            &tx,
            "UPDATE semantic_vector_stages SET writer_binding=?2,plan_json=?3
             WHERE stage_id=?1 AND writer_binding=?4",
            vec![
                ExactSqlValue::Integer(stage.id),
                text(json(&request.replacement.binding)?),
                text(json(&plan)?),
                text(json(&request.expected.binding)?),
            ],
        )?;
        if adopted.changed_rows != 1 {
            rollback(tx)?;
            return Err(corrupt("semantic vector writer adoption CAS failed"));
        }
        let record = stage_by_key(&tx, &request.stage)?
            .ok_or_else(|| corrupt("adopted semantic vector stage is missing"))?
            .record;
        commit(tx)?;
        Ok(SemanticVectorStageWriterAdoptionOutcome::Adopted(record))
    }

    fn prepare_stage_publication(
        &mut self,
        request: &SemanticVectorStagePublicationPrepareRequest,
        fence: &SemanticVectorWriterFence,
        context: &GraphPublicationOperationContextV1<'_>,
    ) -> SemanticVectorStagingStoreResult<SemanticVectorStagePublicationPrepareOutcome> {
        request.validate()?;
        ensure_live(context)?;
        ensure_binding(&self.handle, fence)?;
        let tx = begin(&self.handle)?;
        let Some(stage) = stage_by_key(&tx, &request.stage)? else {
            rollback(tx)?;
            return Ok(SemanticVectorStagePublicationPrepareOutcome::MissingStage);
        };
        if stage.record.plan.writer_fence != *fence {
            let actual = stage.record.plan.writer_fence;
            rollback(tx)?;
            return Ok(SemanticVectorStagePublicationPrepareOutcome::StaleFence { actual });
        }
        if stage.record.state == SemanticVectorStageState::Cancelled {
            let record = stage.record;
            rollback(tx)?;
            return Ok(SemanticVectorStagePublicationPrepareOutcome::Cancelled(
                record,
            ));
        }
        if request.publication_replay.key != stage.record.plan.publication_key
            || request.publication_replay.expected_prior_head
                != stage.record.plan.expected_prior_verified_head
        {
            rollback(tx)?;
            return Ok(SemanticVectorStagePublicationPrepareOutcome::PublicationConflict);
        }
        let published_key = tracedecay_store::SemanticVectorPublishedGenerationKey {
            projection: stage.record.plan.key.projection.clone(),
            semantic_generation_id: stage.record.plan.semantic_generation_id.clone(),
        };
        if let Some(existing) = published_stage_for(&tx, &published_key)? {
            let record = existing.record;
            rollback(tx)?;
            return Ok(
                SemanticVectorStagePublicationPrepareOutcome::SemanticGenerationConflict {
                    existing: record,
                },
            );
        }
        if stage.record.state == SemanticVectorStageState::ReadyToPublish {
            let intent_exact =
                stage
                    .record
                    .publication_intent
                    .as_ref()
                    .is_some_and(|publication_intent| {
                        publication_intent.expected_recovered_digest
                            == request.publication_replay.expected_recovered_digest
                            && publication_intent.publication_intent_digest
                                == request.publication_intent_digest
                    });
            let replay_exact = if intent_exact {
                matches!(
                    crate::repository::graph_publication::append_replay_in_transaction(
                        &tx,
                        &request.publication_replay,
                    )
                    .map_err(map_graph)?,
                    tracedecay_store::GraphReplayAppendOutcomeV1::ExactReplay(_)
                        | tracedecay_store::GraphReplayAppendOutcomeV1::ExactVerifiedReplay { .. }
                )
            } else {
                false
            };
            if replay_exact {
                validate_stage_history(&tx, &stage, context)?;
            }
            let record = stage.record;
            rollback(tx)?;
            return Ok(if replay_exact {
                SemanticVectorStagePublicationPrepareOutcome::ExactReplay(record)
            } else {
                SemanticVectorStagePublicationPrepareOutcome::PublicationConflict
            });
        }
        if request.expected_checkpoint_digest != stage.record.checkpoint_digest {
            let actual = stage.record.checkpoint_digest;
            rollback(tx)?;
            return Ok(SemanticVectorStagePublicationPrepareOutcome::StaleCheckpoint { actual });
        }
        let rows = query(
            &tx,
            "SELECT
                COALESCE(SUM(CASE WHEN e.state='pending' THEN 1 ELSE 0 END),0),
                COALESCE(SUM(CASE WHEN e.state IN ('failed','cancelled') THEN 1 ELSE 0 END),0)
             FROM semantic_vector_stage_graph_effects e
             JOIN semantic_vector_stage_batches b ON b.batch_id=e.batch_id
             WHERE b.stage_id=?1",
            vec![ExactSqlValue::Integer(stage.id)],
        )?;
        let pending = u64_at(&rows.rows[0], 0)?;
        let failed = u64_at(&rows.rows[0], 1)?;
        if stage.record.recorded_chunk_count != stage.record.plan.expected_chunk_count
            || pending != 0
            || failed != 0
            || stage.record.applied_ordinal.map(|ordinal| ordinal + 1)
                != Some(stage.record.next_ordinal)
        {
            let incomplete = SemanticVectorStageIncomplete {
                expected_chunks: stage.record.plan.expected_chunk_count,
                recorded_chunks: stage.record.recorded_chunk_count,
                pending_batches: pending,
                failed_batches: failed,
            };
            rollback(tx)?;
            return Ok(SemanticVectorStagePublicationPrepareOutcome::Incomplete(
                incomplete,
            ));
        }
        validate_stage_history(&tx, &stage, context)?;
        let actual_manifest = chunk_manifest_digest(&tx, stage.id, context)?;
        if actual_manifest != stage.record.plan.recipe.expected_chunk_manifest_digest {
            rollback(tx)?;
            return Ok(
                SemanticVectorStagePublicationPrepareOutcome::ChunkManifestConflict {
                    actual_digest: actual_manifest.as_str().to_owned(),
                },
            );
        }
        begin_commit(context)?;
        ensure_binding(&self.handle, fence)?;
        let transitioned = execute(
            &tx,
            "UPDATE semantic_vector_stages SET state='ready_to_publish',
                expected_recovered_digest=?2,publication_intent_digest=?3
             WHERE stage_id=?1 AND state='pending'",
            vec![
                ExactSqlValue::Integer(stage.id),
                text(
                    request
                        .publication_replay
                        .expected_recovered_digest
                        .as_str(),
                ),
                text(request.publication_intent_digest.as_str()),
            ],
        )?;
        if transitioned.changed_rows != 1 {
            rollback(tx)?;
            return Err(corrupt(
                "semantic vector publication readiness transition did not update one stage",
            ));
        }
        let replay_outcome = crate::repository::graph_publication::append_replay_in_transaction(
            &tx,
            &request.publication_replay,
        )
        .map_err(map_graph)?;
        if !matches!(
            replay_outcome,
            tracedecay_store::GraphReplayAppendOutcomeV1::Appended(_)
                | tracedecay_store::GraphReplayAppendOutcomeV1::ExactReplay(_)
                | tracedecay_store::GraphReplayAppendOutcomeV1::ExactVerifiedReplay { .. }
        ) {
            rollback(tx)?;
            return Ok(SemanticVectorStagePublicationPrepareOutcome::PublicationConflict);
        }
        let record = stage_by_key(&tx, &request.stage)?
            .ok_or_else(|| corrupt("ready_to_publish semantic vector stage is missing"))?
            .record;
        commit(tx)?;
        Ok(SemanticVectorStagePublicationPrepareOutcome::ReadyToPublish(record))
    }

    fn ready_publications(
        &mut self,
        request: &SemanticVectorReadyPublicationPageRequest,
        context: &GraphPublicationOperationContextV1<'_>,
    ) -> SemanticVectorStagingStoreResult<SemanticVectorReadyPublicationPage> {
        request.validate()?;
        ensure_live(context)?;
        ensure_projection_binding(&self.handle, &request.projection)?;
        let snapshot = begin_read_snapshot(&self.handle, context, READ_WAIT)?;
        super::cursors::validate_ready_cursor(&snapshot, request, context)?;
        let (shard, namespace, projection) = projection_parts(&request.projection)?;
        let (after_build, after_plan) =
            request
                .after
                .as_ref()
                .map_or(("".to_owned(), "".to_owned()), |cursor| {
                    (
                        cursor.stage.build_id.as_str().to_owned(),
                        cursor.stage.plan_digest.as_str().to_owned(),
                    )
                });
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
             WHERE shard_id=?1 AND namespace=?2 AND projection=?3
               AND state='ready_to_publish'
               AND (build_id>?4 OR (build_id=?4 AND plan_digest>?5))
             ORDER BY build_id ASC,plan_digest ASC LIMIT ?6",
            vec![
                text(shard),
                text(namespace),
                text(projection),
                text(after_build),
                text(after_plan),
                ExactSqlValue::Integer(i64::from(request.max_records) + 1),
            ],
        )?;
        let mut stages = rows
            .rows
            .iter()
            .map(decode_stage)
            .map(|result| result.map(|stage| stage.record))
            .collect::<SemanticVectorStagingStoreResult<Vec<_>>>()?;
        let more = stages.len() > usize::from(request.max_records);
        if more {
            stages.pop();
        }
        let continuation = more.then(|| stages.last()).flatten().map(|stage| {
            SemanticVectorReadyPublicationCursor {
                stage: stage.plan.key.clone(),
            }
        });
        ensure_live(context)?;
        Ok(SemanticVectorReadyPublicationPage {
            stages,
            continuation,
        })
    }

    fn settle_published(
        &mut self,
        settlement: &SemanticVectorStagePublishSettlement,
        fence: &SemanticVectorWriterFence,
        context: &GraphPublicationOperationContextV1<'_>,
    ) -> SemanticVectorStagingStoreResult<SemanticVectorStagePublishOutcome> {
        super::settle_publication::settle_published(self, settlement, fence, context)
    }
}
