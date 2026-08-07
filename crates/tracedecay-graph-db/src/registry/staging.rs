use tracedecay_store::{
    GraphPublicationOperationContextV1, GraphPublicationReplayLookupV1,
    GraphPublicationReplayRecordV1, GraphVerifiedHeadV1, MAX_SEMANTIC_VECTOR_STAGE_CHUNKS,
    RuntimeInterruptionV1, SemanticVectorBatchReceiptDigest, SemanticVectorGraphBatchDigest,
    SemanticVectorPublicationAuthority, SemanticVectorPublishedGenerationKey,
    SemanticVectorPublishedGenerationLookup, SemanticVectorStageBatchKey,
    SemanticVectorStageBatchReceiptLookup, SemanticVectorStageEffectState,
    SemanticVectorStageEffectTerminal, SemanticVectorStageGraphBatchEffect,
    SemanticVectorStagePlan, SemanticVectorStagePublicationPrepareRequest,
    SemanticVectorStageRecord, SemanticVectorStageResumeOutcome, SemanticVectorStageSettlement,
    SemanticVectorStageSettlementOutcome, SemanticVectorStageState,
    SemanticVectorStageWriterAdoption, SemanticVectorStageWriterAdoptionOutcome,
    SemanticVectorStagingStoreError, SemanticVectorWriterFence,
};

use super::publication_support::{check_all, map_publication_error, require_publication_binding};
use super::{GraphDbRegistration, GraphDbRegistry};
use crate::{GraphCommit, GraphDbError, GraphWriteBatch, VerifiedGraphCommit};

#[derive(Clone, Debug)]
pub struct VerifiedGenerationBatchCommit {
    pub commit: GraphCommit,
    pub effect: SemanticVectorStageGraphBatchEffect,
}

#[derive(Clone, Debug)]
pub struct VerifiedGenerationBatchApply {
    pub commit: GraphCommit,
}

impl GraphDbRegistry {
    pub fn begin_verified_generation(
        &self,
        registration: GraphDbRegistration,
        authority: &mut dyn SemanticVectorPublicationAuthority,
        context: &GraphPublicationOperationContextV1<'_>,
        plan: &SemanticVectorStagePlan,
    ) -> Result<SemanticVectorStageRecord, GraphDbError> {
        check_all(&registration, context)?;
        require_authority_binding(&registration, authority)?;
        require_publication_binding(&registration, &plan.publication_key)?;
        require_plan_binding(&registration, plan)?;
        if plan.expected_chunk_count > MAX_SEMANTIC_VECTOR_STAGE_CHUNKS {
            return Err(GraphDbError::BudgetExhausted);
        }
        plan.validate()
            .map_err(|error| GraphDbError::invalid(error.to_string()))?;
        self.resolve(registration.clone())?;
        let (record, recovered_publication) = match authority
            .begin_stage(plan, context)
            .map_err(map_staging_error)?
        {
            tracedecay_store::SemanticVectorStageBeginOutcome::Begun(record)
            | tracedecay_store::SemanticVectorStageBeginOutcome::ExactReplay(record) => {
                (record, false)
            }
            tracedecay_store::SemanticVectorStageBeginOutcome::Published {
                record,
                verified_head: _,
            } => (*record, true),
            tracedecay_store::SemanticVectorStageBeginOutcome::InputConflict { .. }
            | tracedecay_store::SemanticVectorStageBeginOutcome::SemanticGenerationConflict {
                ..
            }
            | tracedecay_store::SemanticVectorStageBeginOutcome::PublicationConflict
            | tracedecay_store::SemanticVectorStageBeginOutcome::PriorVerifiedHeadConflict {
                ..
            } => return Err(GraphDbError::Conflict),
        };
        if recovered_publication {
            require_same_semantic_generation(&record.plan, plan)?;
        } else {
            require_stage_plan(&record, plan)?;
            require_plan_binding(&registration, &record.plan)?;
        }
        Ok(record)
    }

    pub fn published_semantic_generation(
        &self,
        registration: GraphDbRegistration,
        authority: &mut dyn SemanticVectorPublicationAuthority,
        context: &GraphPublicationOperationContextV1<'_>,
        key: &SemanticVectorPublishedGenerationKey,
    ) -> Result<SemanticVectorPublishedGenerationLookup, GraphDbError> {
        check_all(&registration, context)?;
        require_authority_binding(&registration, authority)?;
        if registration.binding().shard_id != key.projection.shard_id {
            return Err(GraphDbError::Conflict);
        }
        self.resolve(registration)?;
        authority
            .published_semantic_generation(key, context)
            .map_err(map_staging_error)
    }

    pub fn resume_generation_stage(
        &self,
        registration: GraphDbRegistration,
        authority: &mut dyn SemanticVectorPublicationAuthority,
        context: &GraphPublicationOperationContextV1<'_>,
        stage: &tracedecay_store::SemanticVectorStageKey,
    ) -> Result<SemanticVectorStageResumeOutcome, GraphDbError> {
        check_all(&registration, context)?;
        require_authority_binding(&registration, authority)?;
        require_stage_binding(&registration, stage)?;
        let database = self.resolve(registration.clone())?;
        let Some(mut record) = authority.stage(stage, context).map_err(map_staging_error)? else {
            return Ok(SemanticVectorStageResumeOutcome::Missing);
        };
        require_stage_key(&record, stage)?;
        match record.state {
            SemanticVectorStageState::Published => {
                let key = SemanticVectorPublishedGenerationKey {
                    projection: record.plan.key.projection.clone(),
                    semantic_generation_id: record.plan.semantic_generation_id.clone(),
                };
                return match authority
                    .published_semantic_generation(&key, context)
                    .map_err(map_staging_error)?
                {
                    SemanticVectorPublishedGenerationLookup::Published {
                        record: verified_record,
                        verified_head,
                    } if *verified_record == record => {
                        Ok(SemanticVectorStageResumeOutcome::Published {
                            record: verified_record,
                            verified_head,
                        })
                    }
                    SemanticVectorPublishedGenerationLookup::Published { .. }
                    | SemanticVectorPublishedGenerationLookup::Missing => {
                        Err(GraphDbError::Conflict)
                    }
                };
            }
            SemanticVectorStageState::Cancelled => {
                cleanup_cancelled_generation(
                    &database,
                    authority,
                    context,
                    &registration,
                    &record,
                )?;
                return Ok(SemanticVectorStageResumeOutcome::Cancelled(record));
            }
            SemanticVectorStageState::Pending | SemanticVectorStageState::ReadyToPublish => {}
        }
        if record.plan.writer_fence.binding != *registration.binding() {
            let ready_publication_replay =
                if record.state == SemanticVectorStageState::ReadyToPublish {
                    let replay = require_active_stage_replay(authority, context, &record)?;
                    require_stage_replay_intent(&record, &replay)?;
                    Some(replay.publication)
                } else {
                    None
                };
            let adoption = SemanticVectorStageWriterAdoption {
                stage: stage.clone(),
                expected: record.plan.writer_fence.clone(),
                replacement: SemanticVectorWriterFence {
                    binding: registration.binding().clone(),
                },
                ready_publication_replay,
            };
            record = match authority
                .adopt_stage_writer(&adoption, context)
                .map_err(map_staging_error)?
            {
                SemanticVectorStageWriterAdoptionOutcome::Adopted(record)
                | SemanticVectorStageWriterAdoptionOutcome::ExactReplay(record) => record,
                SemanticVectorStageWriterAdoptionOutcome::StaleFence { .. }
                | SemanticVectorStageWriterAdoptionOutcome::VerifiedHeadConflict { .. }
                | SemanticVectorStageWriterAdoptionOutcome::NotAdoptable(_) => {
                    return Err(GraphDbError::Conflict);
                }
                SemanticVectorStageWriterAdoptionOutcome::MissingStage => {
                    return Err(GraphDbError::ResetRequired {
                        message: "semantic vector stage disappeared before writer adoption"
                            .to_owned(),
                    });
                }
            };
            require_stage_key(&record, stage)?;
        }
        require_plan_binding(&registration, &record.plan)?;
        let pending = authority
            .pending_stage(&stage.projection, context)
            .map_err(map_staging_error)?;
        if pending.as_ref() != Some(&record) {
            return Err(GraphDbError::Conflict);
        }
        match record.state {
            SemanticVectorStageState::Pending => {
                Ok(SemanticVectorStageResumeOutcome::Pending(record))
            }
            SemanticVectorStageState::ReadyToPublish => {
                let replay = require_active_stage_replay(authority, context, &record)?;
                require_stage_replay_intent(&record, &replay)?;
                Ok(SemanticVectorStageResumeOutcome::Ready(record))
            }
            SemanticVectorStageState::Published | SemanticVectorStageState::Cancelled => {
                Err(GraphDbError::Corrupt {
                    message: "terminal semantic vector stage changed during resume".to_owned(),
                })
            }
        }
    }

    pub fn apply_verified_generation_batch(
        &self,
        registration: GraphDbRegistration,
        authority: &mut dyn SemanticVectorPublicationAuthority,
        context: &GraphPublicationOperationContextV1<'_>,
        batch_key: &SemanticVectorStageBatchKey,
        expected_receipt_digest: &SemanticVectorBatchReceiptDigest,
        batch: GraphWriteBatch,
    ) -> Result<VerifiedGenerationBatchApply, GraphDbError> {
        check_all(&registration, context)?;
        require_authority_binding(&registration, authority)?;
        require_stage_binding(&registration, &batch_key.stage)?;
        let database = self.resolve(registration.clone())?;
        let record = authority
            .stage(&batch_key.stage, context)
            .map_err(map_staging_error)?
            .ok_or_else(|| GraphDbError::ResetRequired {
                message: "semantic vector stage is missing".to_owned(),
            })?;
        require_stage_key(&record, &batch_key.stage)?;
        require_plan_binding(&registration, &record.plan)?;
        let pending = authority
            .pending_stage(&batch_key.stage.projection, context)
            .map_err(map_staging_error)?;
        if pending.as_ref() != Some(&record) {
            return Err(GraphDbError::Conflict);
        }
        if record.state != SemanticVectorStageState::Pending {
            return Err(GraphDbError::Conflict);
        }
        let actual_head = authority
            .verified_head(&batch_key.stage.projection, context)
            .map_err(map_publication_error)?;
        if actual_head != record.plan.expected_prior_verified_head
            || authority
                .pending_replay(&batch_key.stage.projection, context)
                .map_err(map_publication_error)?
                .is_some()
        {
            return Err(GraphDbError::Conflict);
        }
        let next_applied = match record.applied_ordinal {
            Some(ordinal) => ordinal.checked_add(1).ok_or_else(|| {
                GraphDbError::unavailable("semantic vector applied batch ordinal exhausted")
            })?,
            None => 0,
        };
        if batch_key.ordinal > next_applied {
            return Err(GraphDbError::unavailable(
                "semantic vector graph batch predecessor is not settled",
            ));
        }
        let receipt = match authority
            .batch_receipt(batch_key, context)
            .map_err(map_staging_error)?
        {
            SemanticVectorStageBatchReceiptLookup::Found(receipt) => receipt,
            SemanticVectorStageBatchReceiptLookup::Missing => {
                return Err(GraphDbError::ResetRequired {
                    message: "semantic vector graph batch has no durable receipt".to_owned(),
                });
            }
        };
        if receipt.key != *batch_key || receipt.receipt_digest != *expected_receipt_digest {
            return Err(GraphDbError::Conflict);
        }
        require_authority_binding(&registration, authority)?;
        require_plan_binding(&registration, &record.plan)?;
        let check = || check_all(&registration, context);
        let commit =
            database.apply_staged_generation_batch(&record.plan, &receipt, batch, &check)?;
        let latest = authority
            .stage(&batch_key.stage, context)
            .map_err(map_staging_error)?
            .ok_or_else(|| GraphDbError::ResetRequired {
                message: "semantic vector stage disappeared after native apply".to_owned(),
            })?;
        if latest.state == SemanticVectorStageState::Cancelled {
            cleanup_cancelled_generation(&database, authority, context, &registration, &latest)?;
            return Err(GraphDbError::Conflict);
        }
        if latest.plan.key != batch_key.stage || latest.state != SemanticVectorStageState::Pending {
            return Err(GraphDbError::Conflict);
        }
        Ok(VerifiedGenerationBatchApply { commit })
    }

    pub fn cancel_generation_stage(
        &self,
        registration: GraphDbRegistration,
        authority: &mut dyn SemanticVectorPublicationAuthority,
        context: &GraphPublicationOperationContextV1<'_>,
        stage: &tracedecay_store::SemanticVectorStageKey,
    ) -> Result<tracedecay_store::SemanticVectorStageCancelOutcome, GraphDbError> {
        check_all(&registration, context)?;
        require_authority_binding(&registration, authority)?;
        require_stage_binding(&registration, stage)?;
        let database = self.resolve(registration.clone())?;
        let Some(record) = authority.stage(stage, context).map_err(map_staging_error)? else {
            return Ok(tracedecay_store::SemanticVectorStageCancelOutcome::MissingStage);
        };
        require_stage_key(&record, stage)?;
        require_plan_binding(&registration, &record.plan)?;
        if !matches!(
            record.state,
            SemanticVectorStageState::Pending | SemanticVectorStageState::Cancelled
        ) {
            return Ok(tracedecay_store::SemanticVectorStageCancelOutcome::ReadyToPublish(record));
        }
        require_unpublished_stage(authority, context, &record)?;
        database.reserve_staged_generation_retirement(&record.plan)?;
        let outcome = match authority.cancel_stage(stage, &record.plan.writer_fence, context) {
            Ok(outcome) => outcome,
            Err(error) => {
                match authority.stage(stage, context) {
                    Ok(Some(observed)) if observed.state == SemanticVectorStageState::Cancelled => {
                        let check = || check_all(&registration, context);
                        database.delete_cancelled_staged_generation(&observed.plan, &check)?;
                    }
                    Ok(Some(_)) => {
                        database.clear_staged_generation_retirement(&record.plan)?;
                    }
                    Ok(None) | Err(_) => {}
                }
                return Err(map_staging_error(error));
            }
        };
        match &outcome {
            tracedecay_store::SemanticVectorStageCancelOutcome::Cancelled(cancelled)
            | tracedecay_store::SemanticVectorStageCancelOutcome::ExactReplay(cancelled) => {
                let check = || check_all(&registration, context);
                database.delete_cancelled_staged_generation(&cancelled.plan, &check)?;
            }
            tracedecay_store::SemanticVectorStageCancelOutcome::StaleFence { .. }
            | tracedecay_store::SemanticVectorStageCancelOutcome::ReadyToPublish(_)
            | tracedecay_store::SemanticVectorStageCancelOutcome::MissingStage => {
                database.clear_staged_generation_retirement(&record.plan)?;
            }
        }
        Ok(outcome)
    }

    pub fn settle_verified_generation_batch(
        &self,
        registration: GraphDbRegistration,
        authority: &mut dyn SemanticVectorPublicationAuthority,
        context: &GraphPublicationOperationContextV1<'_>,
        batch_key: &SemanticVectorStageBatchKey,
        expected_receipt_digest: &SemanticVectorBatchReceiptDigest,
    ) -> Result<SemanticVectorStageGraphBatchEffect, GraphDbError> {
        check_all(&registration, context)?;
        require_authority_binding(&registration, authority)?;
        require_stage_binding(&registration, &batch_key.stage)?;
        let database = self.resolve(registration.clone())?;
        let record = authority
            .stage(&batch_key.stage, context)
            .map_err(map_staging_error)?
            .ok_or_else(|| GraphDbError::ResetRequired {
                message: "semantic vector stage is missing".to_owned(),
            })?;
        require_stage_key(&record, &batch_key.stage)?;
        require_plan_binding(&registration, &record.plan)?;
        if record.state == SemanticVectorStageState::Cancelled {
            cleanup_cancelled_generation(&database, authority, context, &registration, &record)?;
            return Err(GraphDbError::Conflict);
        }
        let receipt = match authority
            .batch_receipt(batch_key, context)
            .map_err(map_staging_error)?
        {
            SemanticVectorStageBatchReceiptLookup::Found(receipt) => receipt,
            SemanticVectorStageBatchReceiptLookup::Missing => {
                return Err(GraphDbError::ResetRequired {
                    message: "semantic vector graph batch has no durable receipt".to_owned(),
                });
            }
        };
        if receipt.key != *batch_key || receipt.receipt_digest != *expected_receipt_digest {
            return Err(GraphDbError::Conflict);
        }
        let graph_batch_digest =
            database.staged_generation_batch_publication_digest(&record.plan, &receipt)?;
        require_authority_binding(&registration, authority)?;
        require_plan_binding(&registration, &record.plan)?;
        let settlement = SemanticVectorStageSettlement {
            batch: batch_key.clone(),
            expected_receipt_digest: expected_receipt_digest.clone(),
            terminal: SemanticVectorStageEffectTerminal::Applied {
                graph_batch_digest: graph_batch_digest.clone(),
            },
        };
        let effect = match authority
            .settle_stage_batch(&settlement, &record.plan.writer_fence, context)
            .map_err(map_staging_error)?
        {
            SemanticVectorStageSettlementOutcome::Settled(effect)
            | SemanticVectorStageSettlementOutcome::ExactReplay(effect)
                if effect.state == SemanticVectorStageEffectState::Applied
                    && effect.terminal_digest.as_deref() == Some(graph_batch_digest.as_str()) =>
            {
                effect
            }
            SemanticVectorStageSettlementOutcome::Settled(_)
            | SemanticVectorStageSettlementOutcome::ExactReplay(_)
            | SemanticVectorStageSettlementOutcome::Conflict(_)
            | SemanticVectorStageSettlementOutcome::StaleOrdinal { .. }
            | SemanticVectorStageSettlementOutcome::StaleFence { .. }
            | SemanticVectorStageSettlementOutcome::Cancelled(_) => {
                return Err(GraphDbError::Conflict);
            }
            SemanticVectorStageSettlementOutcome::MissingBatch => {
                return Err(GraphDbError::ResetRequired {
                    message: "semantic vector graph batch disappeared before settlement".to_owned(),
                });
            }
        };
        Ok(effect)
    }

    pub fn publish_ready_generation(
        &self,
        registration: GraphDbRegistration,
        authority: &mut dyn SemanticVectorPublicationAuthority,
        context: &GraphPublicationOperationContextV1<'_>,
        stage: &tracedecay_store::SemanticVectorStageKey,
    ) -> Result<VerifiedGraphCommit, GraphDbError> {
        check_all(&registration, context)?;
        require_stage_binding(&registration, stage)?;
        require_authority_binding(&registration, authority)?;
        let record = authority
            .stage(stage, context)
            .map_err(map_staging_error)?
            .ok_or_else(|| GraphDbError::ResetRequired {
                message: "ready semantic vector stage is missing".to_owned(),
            })?;
        require_stage_key(&record, stage)?;
        require_plan_binding(&registration, &record.plan)?;
        if !matches!(
            record.state,
            SemanticVectorStageState::ReadyToPublish | SemanticVectorStageState::Published
        ) {
            return Err(GraphDbError::Conflict);
        }
        let intent = record
            .publication_intent
            .as_ref()
            .ok_or_else(|| GraphDbError::Corrupt {
                message: "ready semantic vector stage has no publication intent".to_owned(),
            })?;
        if intent.publication_key != record.plan.publication_key {
            return Err(GraphDbError::Conflict);
        }
        match authority
            .replay(&intent.publication_key, context)
            .map_err(super::publication_support::map_publication_error)?
        {
            GraphPublicationReplayLookupV1::Active(replay)
                if replay.publication.key == intent.publication_key
                    && replay.publication.expected_recovered_digest
                        == intent.expected_recovered_digest => {}
            GraphPublicationReplayLookupV1::Active(_)
            | GraphPublicationReplayLookupV1::Retired(_) => {
                return Err(GraphDbError::Conflict);
            }
            GraphPublicationReplayLookupV1::Missing => {
                return Err(GraphDbError::ResetRequired {
                    message: "ready semantic vector stage lost its graph publication replay"
                        .to_owned(),
                });
            }
        }
        require_authority_binding(&registration, authority)?;
        require_plan_binding(&registration, &record.plan)?;
        self.publish_ready_staged_generation(
            registration,
            authority,
            context,
            &intent.publication_key,
        )
    }

    pub fn prepare_publication_from_staged_native(
        &self,
        registration: GraphDbRegistration,
        authority: &mut dyn SemanticVectorPublicationAuthority,
        context: &GraphPublicationOperationContextV1<'_>,
        stage: &tracedecay_store::SemanticVectorStageKey,
    ) -> Result<tracedecay_store::SemanticVectorStagePublicationPrepareOutcome, GraphDbError> {
        check_all(&registration, context)?;
        require_authority_binding(&registration, authority)?;
        require_stage_binding(&registration, stage)?;
        let database = self.resolve(registration.clone())?;
        let record = authority
            .stage(stage, context)
            .map_err(map_staging_error)?
            .ok_or_else(|| GraphDbError::ResetRequired {
                message: "semantic vector stage is missing before native finalization".to_owned(),
            })?;
        require_stage_key(&record, stage)?;
        require_plan_binding(&registration, &record.plan)?;
        if record.state == SemanticVectorStageState::Cancelled {
            return Ok(
                tracedecay_store::SemanticVectorStagePublicationPrepareOutcome::Cancelled(record),
            );
        }
        if !matches!(
            record.state,
            SemanticVectorStageState::Pending | SemanticVectorStageState::ReadyToPublish
        ) || record.recorded_chunk_count != record.plan.expected_chunk_count
            || record.applied_ordinal.map(|ordinal| ordinal + 1) != Some(record.next_ordinal)
        {
            return Err(GraphDbError::Conflict);
        }
        let checkpoint = record.checkpoint_digest.clone();
        let check = || check_all(&registration, context);
        let replay =
            database.prepare_publication_from_staged_native(&record.plan, &checkpoint, &check)?;
        let request =
            SemanticVectorStagePublicationPrepareRequest::new(stage.clone(), replay, checkpoint)
                .map_err(|error| GraphDbError::invalid(error.to_string()))?;
        require_authority_binding(&registration, authority)?;
        match authority
            .prepare_stage_publication(&request, &record.plan.writer_fence, context)
            .map_err(map_staging_error)?
        {
            outcome @ (tracedecay_store::SemanticVectorStagePublicationPrepareOutcome::ReadyToPublish(
                _,
            )
            | tracedecay_store::SemanticVectorStagePublicationPrepareOutcome::ExactReplay(_)
            | tracedecay_store::SemanticVectorStagePublicationPrepareOutcome::StaleCheckpoint {
                ..
            }) => Ok(outcome),
            tracedecay_store::SemanticVectorStagePublicationPrepareOutcome::Incomplete(_)
            | tracedecay_store::SemanticVectorStagePublicationPrepareOutcome::StaleFence { .. }
            | tracedecay_store::SemanticVectorStagePublicationPrepareOutcome::PublicationConflict
            | tracedecay_store::SemanticVectorStagePublicationPrepareOutcome::SemanticGenerationConflict {
                ..
            }
            | tracedecay_store::SemanticVectorStagePublicationPrepareOutcome::ChunkManifestConflict {
                ..
            }
            | tracedecay_store::SemanticVectorStagePublicationPrepareOutcome::Cancelled(_)
            | tracedecay_store::SemanticVectorStagePublicationPrepareOutcome::MissingStage => {
                Err(GraphDbError::Conflict)
            }
        }
    }
}

fn require_active_stage_replay(
    authority: &mut dyn SemanticVectorPublicationAuthority,
    context: &GraphPublicationOperationContextV1<'_>,
    record: &SemanticVectorStageRecord,
) -> Result<GraphPublicationReplayRecordV1, GraphDbError> {
    let intent = record
        .publication_intent
        .as_ref()
        .ok_or_else(|| GraphDbError::Corrupt {
            message: "published or ready semantic vector stage has no publication intent"
                .to_owned(),
        })?;
    match authority
        .replay(&intent.publication_key, context)
        .map_err(map_publication_error)?
    {
        GraphPublicationReplayLookupV1::Active(replay) => Ok(replay),
        GraphPublicationReplayLookupV1::Retired(_) => Err(GraphDbError::Conflict),
        GraphPublicationReplayLookupV1::Missing => Err(GraphDbError::ResetRequired {
            message: "semantic vector stage lost its publication replay".to_owned(),
        }),
    }
}

fn require_unpublished_stage(
    authority: &mut dyn SemanticVectorPublicationAuthority,
    context: &GraphPublicationOperationContextV1<'_>,
    record: &SemanticVectorStageRecord,
) -> Result<(), GraphDbError> {
    if !matches!(
        authority
            .replay(&record.plan.publication_key, context)
            .map_err(map_publication_error)?,
        GraphPublicationReplayLookupV1::Missing
    ) || authority
        .verified_head(&record.plan.key.projection, context)
        .map_err(map_publication_error)?
        .is_some_and(|head| head.key == record.plan.publication_key)
    {
        return Err(GraphDbError::Conflict);
    }
    Ok(())
}

fn cleanup_cancelled_generation(
    database: &crate::GraphDb,
    authority: &mut dyn SemanticVectorPublicationAuthority,
    context: &GraphPublicationOperationContextV1<'_>,
    registration: &GraphDbRegistration,
    record: &SemanticVectorStageRecord,
) -> Result<(), GraphDbError> {
    if record.state != SemanticVectorStageState::Cancelled {
        return Err(GraphDbError::Conflict);
    }
    require_unpublished_stage(authority, context, record)?;
    database.reserve_staged_generation_retirement(&record.plan)?;
    let check = || check_all(registration, context);
    database.delete_cancelled_staged_generation(&record.plan, &check)
}

fn require_stage_replay_intent(
    record: &SemanticVectorStageRecord,
    replay: &GraphPublicationReplayRecordV1,
) -> Result<(), GraphDbError> {
    let intent = record
        .publication_intent
        .as_ref()
        .ok_or_else(|| GraphDbError::Corrupt {
            message: "published or ready semantic vector stage has no publication intent"
                .to_owned(),
        })?;
    let request = SemanticVectorStagePublicationPrepareRequest::new(
        record.plan.key.clone(),
        replay.publication.clone(),
        record.checkpoint_digest.clone(),
    )
    .map_err(|error| GraphDbError::Corrupt {
        message: error.to_string(),
    })?;
    if intent.publication_key != replay.publication.key
        || intent.expected_recovered_digest != replay.publication.expected_recovered_digest
        || intent.publication_intent_digest != request.publication_intent_digest
        || replay.publication.key != record.plan.publication_key
        || replay.publication.expected_prior_head != record.plan.expected_prior_verified_head
    {
        return Err(GraphDbError::Conflict);
    }
    Ok(())
}

fn require_stage_plan(
    record: &SemanticVectorStageRecord,
    plan: &SemanticVectorStagePlan,
) -> Result<(), GraphDbError> {
    if record.plan != *plan {
        return Err(GraphDbError::Conflict);
    }
    Ok(())
}

fn require_same_semantic_generation(
    existing: &SemanticVectorStagePlan,
    requested: &SemanticVectorStagePlan,
) -> Result<(), GraphDbError> {
    if existing.key.projection != requested.key.projection
        || existing.semantic_generation_id != requested.semantic_generation_id
        || existing.base_generation != requested.base_generation
        || existing.source_scope != requested.source_scope
        || existing.source_generation != requested.source_generation
        || existing.source_dependency != requested.source_dependency
        || existing.recipe != requested.recipe
        || existing.expected_chunk_count != requested.expected_chunk_count
    {
        return Err(GraphDbError::Conflict);
    }
    Ok(())
}

fn require_stage_key(
    record: &SemanticVectorStageRecord,
    stage: &tracedecay_store::SemanticVectorStageKey,
) -> Result<(), GraphDbError> {
    if record.plan.key != *stage || record.plan.publication_key.projection != stage.projection {
        return Err(GraphDbError::Conflict);
    }
    Ok(())
}

fn require_stage_binding(
    registration: &GraphDbRegistration,
    stage: &tracedecay_store::SemanticVectorStageKey,
) -> Result<(), GraphDbError> {
    if registration.binding().shard_id != stage.projection.shard_id {
        return Err(GraphDbError::Conflict);
    }
    Ok(())
}

fn require_plan_binding(
    registration: &GraphDbRegistration,
    plan: &SemanticVectorStagePlan,
) -> Result<(), GraphDbError> {
    if plan.writer_fence.binding != *registration.binding() {
        return Err(GraphDbError::Conflict);
    }
    Ok(())
}

pub(super) fn require_authority_binding(
    registration: &GraphDbRegistration,
    authority: &dyn SemanticVectorPublicationAuthority,
) -> Result<(), GraphDbError> {
    if authority.binding() != registration.binding() {
        return Err(GraphDbError::Conflict);
    }
    Ok(())
}

pub(super) fn map_staging_error(error: SemanticVectorStagingStoreError) -> GraphDbError {
    match error {
        SemanticVectorStagingStoreError::InvalidRequest(error) => {
            GraphDbError::invalid(error.to_string())
        }
        SemanticVectorStagingStoreError::Interrupted(RuntimeInterruptionV1::Cancelled) => {
            GraphDbError::Cancelled
        }
        SemanticVectorStagingStoreError::Interrupted(RuntimeInterruptionV1::DeadlineExceeded) => {
            GraphDbError::DeadlineExceeded
        }
        SemanticVectorStagingStoreError::Infrastructure => {
            GraphDbError::unavailable("semantic vector staging persistence is unavailable")
        }
        SemanticVectorStagingStoreError::AuthorityLost => GraphDbError::Conflict,
        SemanticVectorStagingStoreError::Busy => {
            GraphDbError::unavailable("semantic vector staging authority is busy")
        }
        SemanticVectorStagingStoreError::CensusRevisionChanged { expected, actual } => {
            GraphDbError::ResetRequired {
                message: format!(
                    "semantic vector project census changed from revision {} to {}; restart",
                    expected.get(),
                    actual.get()
                ),
            }
        }
        SemanticVectorStagingStoreError::ReusedOperationContext => {
            GraphDbError::invalid("semantic vector staging operation context was already consumed")
        }
        SemanticVectorStagingStoreError::Corrupt(message) => GraphDbError::Corrupt { message },
    }
}
