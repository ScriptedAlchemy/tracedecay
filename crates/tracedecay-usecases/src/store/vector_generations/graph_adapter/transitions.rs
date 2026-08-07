use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Instant;

use tracedecay_domain::{
    ManifestDigest, ProjectionOperationV1, VectorGenerationIdV1, canonical_sha256,
};
use tracedecay_graph_db::{
    GraphCancellation, GraphDbError, GraphGenerationDependency, GraphMutation, GraphWatermark,
    GraphWriteBatch, NeverCancelled, SourceGeneration,
};
use tracedecay_store::{
    GraphGenerationIdV1, GraphNamespaceV1, GraphProjectionIdV1, GraphProjectionIdentityV1,
    GraphPublicationIdempotencyKeyV1, GraphPublicationKeyV1, GraphVerifiedHeadV1,
    SemanticEmbeddingProjectionDigestV1, SemanticModelArtifactDigestV1,
    SemanticPrivacyDomainDigestV1, SemanticProjectionManifestDigestV1,
    SemanticVectorBatchInputDigest, SemanticVectorBatchOutputDigest, SemanticVectorBuildId,
    SemanticVectorCheckpointDigest, SemanticVectorChunkDigest, SemanticVectorChunkId,
    SemanticVectorOutputDigest, SemanticVectorPublishedGenerationKey,
    SemanticVectorPublishedGenerationLookup, SemanticVectorReconstructionRecipe,
    SemanticVectorSourceDependencyV1, SemanticVectorSourceGenerationId,
    SemanticVectorSourceManifestDigest, SemanticVectorStageBatchKey,
    SemanticVectorStageBatchReceipt, SemanticVectorStageCancelOutcome,
    SemanticVectorStageChunkOperation, SemanticVectorStageChunkReceipt, SemanticVectorStagePlan,
    SemanticVectorStagePublicationPrepareOutcome, SemanticVectorStagePublishOutcome,
    SemanticVectorStagePublishSettlement, SemanticVectorStageRecord,
    SemanticVectorStageResumeOutcome, SemanticVectorStageState, SemanticVectorWriterFence,
    StoreRuntimeBindingV1, StoreShardIdV1,
};

use super::super::identity::generation_identity_digest;
use super::super::{
    MAX_STATE_CAS_RETRIES, PhysicalVectorBytePoolV1, PreparedVectorGenerationV1, PublishedStateV1,
    VECTOR_GENERATION_BUILD_DIGEST_DOMAIN, VectorGenerationBuildIdV1, VectorGenerationPlanV1,
    VectorGenerationPublicationV1, VectorGenerationStateMachineV1, VectorGenerationStoreErrorV1,
    VectorProjectionCheckpointV1, validate_plan,
};
use super::native_records::{
    NativeGraphStateV1, ScopedBuildRecordsV1, ScopedGenerationRecordsV1,
    encode_generation_batch_delta, read_build_records, read_cataloged_generation_records,
    read_state_metadata,
};
use super::persistence::{map_graph_error, storage_error};
use super::stage_identity::next_stage_attempt;
use super::{
    GRAPH_OPERATION_DEADLINE, GraphVectorGenerationStoreV1, VectorGenerationBeginOutcomeV1,
};
use crate::semantic_runtime::SemanticGraphExecutionAuthorityV1;

impl GraphVectorGenerationStoreV1 {
    pub(super) fn semantic_stage_plan(
        &self,
        plan: &VectorGenerationPlanV1,
        build_id: &VectorGenerationBuildIdV1,
        descriptor: &super::SemanticVectorStageDescriptorV1,
        authority: &SemanticGraphExecutionAuthorityV1,
    ) -> Result<SemanticVectorStagePlan, VectorGenerationStoreErrorV1> {
        if descriptor.projection.projection_key() != &plan.target_projection_key {
            return Err(VectorGenerationStoreErrorV1::InvalidPlan(
                "semantic vector stage projection does not match the generation plan".to_owned(),
            ));
        }
        let (source_scope, binding) = self.runtime.staging_binding();
        let scope = self.runtime.scope();
        let projection = GraphProjectionIdentityV1 {
            shard_id: binding.shard_id.clone(),
            namespace: GraphNamespaceV1::new(scope.projection().namespace.as_str())
                .map_err(storage_error)?,
            projection: GraphProjectionIdV1::new(scope.projection().projection.as_str())
                .map_err(storage_error)?,
        };
        let publication_key = GraphPublicationKeyV1::new(
            projection.clone(),
            GraphGenerationIdV1::new(build_id.0.as_str()).map_err(storage_error)?,
            GraphPublicationIdempotencyKeyV1::new(format!(
                "semantic-vector:{}",
                build_id.0.as_str()
            ))
            .map_err(storage_error)?,
        );
        let embedding_digest = canonical_sha256(&descriptor.projection).map_err(storage_error)?;
        let privacy_digest =
            canonical_sha256(descriptor.projection.privacy_domain()).map_err(storage_error)?;
        let initial_checkpoint = VectorProjectionCheckpointV1 {
            target_projection_key: plan.target_projection_key.clone(),
            source_generation: plan.source_generation.clone(),
            source_manifest_digest: plan.source_manifest_digest.clone(),
            completed_batches: 0,
            last_request_digest: None,
            last_publication_digest: None,
        };
        let checkpoint_digest = canonical_sha256(&initial_checkpoint).map_err(storage_error)?;
        let expected_chunk_count =
            u64::try_from(descriptor.members.len()).map_err(storage_error)?;
        let embedding_dimension = u16::try_from(descriptor.projection.embedding_key().dimensions)
            .map_err(storage_error)?;
        let recipe = SemanticVectorReconstructionRecipe {
            source_manifest_digest: SemanticVectorSourceManifestDigest::new(
                plan.source_manifest_digest.as_str(),
            )
            .map_err(storage_error)?,
            embedding_projection_digest: SemanticEmbeddingProjectionDigestV1::new(
                embedding_digest.as_str(),
            )
            .map_err(storage_error)?,
            embedding_dimension,
            model_artifact_digest: SemanticModelArtifactDigestV1::new(
                descriptor
                    .projection
                    .embedding_key()
                    .model_artifact_digest
                    .as_str(),
            )
            .map_err(storage_error)?,
            projection_manifest_digest: SemanticProjectionManifestDigestV1::new(
                plan.target_projection_key.profile_digest.as_str(),
            )
            .map_err(storage_error)?,
            privacy_domain_digest: SemanticPrivacyDomainDigestV1::new(privacy_digest.as_str())
                .map_err(storage_error)?,
            privacy_key_epoch: descriptor.projection.privacy_key_epoch(),
            expected_chunk_manifest_digest:
                tracedecay_store::semantic_vector_chunk_manifest_digest(&descriptor.members)
                    .map_err(storage_error)?,
        };
        let expected_prior_verified_head = self
            .runtime
            .verified_head(authority)
            .map_err(map_graph_error)?;
        let (source_scope, source_dependency) =
            semantic_stage_source_identity(&source_scope, &binding, scope.source_dependency())?;
        SemanticVectorStagePlan::new(
            projection,
            SemanticVectorBuildId::new(build_id.0.as_str()).map_err(storage_error)?,
            VectorGenerationIdV1::new(generation_identity_digest(plan)?),
            plan.base_generation.clone(),
            publication_key,
            source_scope,
            scope.code_scope_hash().clone(),
            SemanticVectorSourceGenerationId::new(plan.source_generation.to_string())
                .map_err(storage_error)?,
            source_dependency,
            recipe,
            expected_chunk_count,
            expected_prior_verified_head,
            SemanticVectorCheckpointDigest::new(checkpoint_digest.as_str())
                .map_err(storage_error)?,
            SemanticVectorWriterFence {
                binding: binding.clone(),
            },
        )
        .map_err(storage_error)
    }

    pub(super) fn begin_generation_records(
        &self,
        plan: VectorGenerationPlanV1,
        rebuild: bool,
        cancellation: Arc<dyn GraphCancellation>,
    ) -> Result<VectorGenerationBeginOutcomeV1, VectorGenerationStoreErrorV1> {
        let authority = SemanticGraphExecutionAuthorityV1::new(
            Arc::clone(&cancellation),
            Instant::now() + GRAPH_OPERATION_DEADLINE,
        );
        validate_plan(&plan)?;
        let build_id = VectorGenerationBuildIdV1(
            canonical_sha256(&(VECTOR_GENERATION_BUILD_DIGEST_DOMAIN, &plan))
                .map_err(storage_error)?,
        );
        {
            let pending = self.pending.lock().map_err(|_| {
                VectorGenerationStoreErrorV1::Unavailable(
                    "semantic vector pending build lock is poisoned".to_owned(),
                )
            })?;
            if pending.contains_key(&build_id) && !rebuild {
                return Ok(VectorGenerationBeginOutcomeV1::ReplayFromStart { build_id });
            }
        }
        for _ in 0..MAX_STATE_CAS_RETRIES {
            authority.checkpoint().map_err(map_graph_error)?;
            self.refresh_snapshot(&authority)?;
            let snapshot = self.optional_snapshot()?;
            let metadata = snapshot
                .as_ref()
                .map(|snapshot| read_state_metadata(snapshot, Arc::clone(&cancellation)))
                .transpose()?;
            let existing = snapshot
                .as_ref()
                .map(|snapshot| read_build_records(snapshot, &build_id, Arc::clone(&cancellation)))
                .transpose()?
                .flatten();
            if !rebuild
                && let Some(existing) = &existing
                && existing.staged.plan == plan
            {
                return Ok(VectorGenerationBeginOutcomeV1::ReplayFromStart { build_id });
            }
            let mut generations = Vec::new();
            if let Some(snapshot) = snapshot.as_ref() {
                push_required_generation(
                    &mut generations,
                    snapshot,
                    plan.base_generation.as_ref(),
                    Arc::clone(&cancellation),
                )?;
            } else if plan.base_generation.is_some() {
                return Err(VectorGenerationStoreErrorV1::IncompatibleBaseGeneration);
            }
            let before = transition_state(existing.as_ref(), generations.iter())?;
            let mut after = before.clone();
            let result = if rebuild {
                after.rebuild_generation(plan.clone())
            } else {
                after.begin_generation(plan.clone())
            }?;
            let descriptor = self
                .descriptor
                .lock()
                .map_err(|_| {
                    VectorGenerationStoreErrorV1::Unavailable(
                        "semantic vector stage descriptor lock is poisoned".to_owned(),
                    )
                })?
                .clone()
                .ok_or_else(|| {
                    VectorGenerationStoreErrorV1::InvalidPlan(
                        "semantic vector stage descriptor is not configured".to_owned(),
                    )
                })?;
            let mut attempt = result.clone();
            let (stage, published) = loop {
                let stage_plan =
                    self.semantic_stage_plan(&plan, &attempt, &descriptor, &authority)?;
                let published_key = SemanticVectorPublishedGenerationKey {
                    projection: stage_plan.key.projection.clone(),
                    semantic_generation_id: stage_plan.semantic_generation_id.clone(),
                };
                if let SemanticVectorPublishedGenerationLookup::Published {
                    record,
                    verified_head,
                } = self
                    .runtime
                    .published_semantic_generation(&published_key, &authority)
                    .map_err(map_graph_error)?
                {
                    require_same_semantic_plan(&record, &stage_plan)?;
                    let publication =
                        self.recover_published_generation(&plan, &verified_head, &authority)?;
                    break (*record, Some(publication));
                }
                match self
                    .runtime
                    .resume_stage(&stage_plan.key, &authority)
                    .map_err(map_graph_error)?
                {
                    SemanticVectorStageResumeOutcome::Missing => {
                        let stage = self
                            .runtime
                            .begin_stage(&stage_plan, &authority)
                            .map_err(map_graph_error)?;
                        match stage.state {
                            SemanticVectorStageState::Pending
                            | SemanticVectorStageState::ReadyToPublish => break (stage, None),
                            SemanticVectorStageState::Published => continue,
                            SemanticVectorStageState::Cancelled if rebuild => {
                                let cancelled_attempt = VectorGenerationBuildIdV1(
                                    ManifestDigest::new(stage.plan.key.build_id.as_str())
                                        .map_err(storage_error)?,
                                );
                                attempt = next_stage_attempt(
                                    &result,
                                    &cancelled_attempt,
                                    stage.plan.key.plan_digest.as_str(),
                                )?;
                            }
                            SemanticVectorStageState::Cancelled => {
                                return Err(VectorGenerationStoreErrorV1::UnknownBuild);
                            }
                        }
                    }
                    SemanticVectorStageResumeOutcome::Pending(stage)
                    | SemanticVectorStageResumeOutcome::Ready(stage)
                        if !rebuild =>
                    {
                        require_resumed_plan(&stage, &stage_plan)?;
                        break (stage, None);
                    }
                    SemanticVectorStageResumeOutcome::Pending(stage)
                    | SemanticVectorStageResumeOutcome::Ready(stage) => {
                        require_resumed_plan(&stage, &stage_plan)?;
                        match self
                            .runtime
                            .cancel_stage(&stage.plan.key, &authority)
                            .map_err(map_graph_error)?
                        {
                            SemanticVectorStageCancelOutcome::Cancelled(record)
                            | SemanticVectorStageCancelOutcome::ExactReplay(record) => {
                                require_resumed_plan(&record, &stage_plan)?;
                                let cancelled_attempt = VectorGenerationBuildIdV1(
                                    ManifestDigest::new(record.plan.key.build_id.as_str())
                                        .map_err(storage_error)?,
                                );
                                attempt = next_stage_attempt(
                                    &result,
                                    &cancelled_attempt,
                                    record.plan.key.plan_digest.as_str(),
                                )?;
                            }
                            SemanticVectorStageCancelOutcome::ReadyToPublish(record) => {
                                require_resumed_plan(&record, &stage_plan)?;
                                match self
                                    .runtime
                                    .resume_stage(&stage_plan.key, &authority)
                                    .map_err(map_graph_error)?
                                {
                                    SemanticVectorStageResumeOutcome::Published {
                                        record,
                                        verified_head,
                                    } => {
                                        require_resumed_plan(&record, &stage_plan)?;
                                        let publication = self.recover_published_generation(
                                            &plan,
                                            &verified_head,
                                            &authority,
                                        )?;
                                        break (*record, Some(publication));
                                    }
                                    SemanticVectorStageResumeOutcome::Ready(_) => {
                                        return Err(
                                            VectorGenerationStoreErrorV1::ConcurrentMutation,
                                        );
                                    }
                                    SemanticVectorStageResumeOutcome::Pending(_)
                                    | SemanticVectorStageResumeOutcome::Missing
                                    | SemanticVectorStageResumeOutcome::Cancelled(_) => {
                                        return Err(
                                            VectorGenerationStoreErrorV1::ConcurrentMutation,
                                        );
                                    }
                                }
                            }
                            SemanticVectorStageCancelOutcome::MissingStage
                            | SemanticVectorStageCancelOutcome::StaleFence { .. } => {
                                return Err(VectorGenerationStoreErrorV1::ConcurrentMutation);
                            }
                        }
                    }
                    SemanticVectorStageResumeOutcome::Published {
                        record,
                        verified_head,
                    } => {
                        require_resumed_plan(&record, &stage_plan)?;
                        let publication =
                            self.recover_published_generation(&plan, &verified_head, &authority)?;
                        break (*record, Some(publication));
                    }
                    SemanticVectorStageResumeOutcome::Cancelled(record) => {
                        if !rebuild {
                            return Err(VectorGenerationStoreErrorV1::UnknownBuild);
                        }
                        let cancelled_attempt = VectorGenerationBuildIdV1(
                            ManifestDigest::new(record.plan.key.build_id.as_str())
                                .map_err(storage_error)?,
                        );
                        attempt = next_stage_attempt(
                            &result,
                            &cancelled_attempt,
                            record.plan.key.plan_digest.as_str(),
                        )?;
                    }
                }
            };
            if let Some(publication) = published {
                return Ok(VectorGenerationBeginOutcomeV1::AlreadyPublished {
                    build_id: result,
                    publication,
                });
            }
            let mut pending = self.pending.lock().map_err(|_| {
                VectorGenerationStoreErrorV1::Unavailable(
                    "semantic vector pending build lock is poisoned".to_owned(),
                )
            })?;
            pending.insert(
                result.clone(),
                super::PendingSemanticVectorBuildV1 {
                    state: after,
                    stage,
                    revision: metadata.as_ref().map_or(0, |metadata| metadata.revision),
                    publication: None,
                },
            );
            return Ok(VectorGenerationBeginOutcomeV1::ReplayFromStart { build_id: result });
        }
        Err(VectorGenerationStoreErrorV1::ConcurrentMutation)
    }

    fn recover_published_generation(
        &self,
        plan: &VectorGenerationPlanV1,
        verified_head: &GraphVerifiedHeadV1,
        authority: &SemanticGraphExecutionAuthorityV1,
    ) -> Result<VectorGenerationPublicationV1, VectorGenerationStoreErrorV1> {
        let snapshot = self
            .runtime
            .recover_verified_generation(&verified_head.key, authority)
            .map_err(map_graph_error)?;
        if snapshot.verified_head() != verified_head {
            return Err(VectorGenerationStoreErrorV1::ConcurrentMutation);
        }
        let generation_id = VectorGenerationIdV1::new(generation_identity_digest(plan)?);
        let read = super::snapshot::SemanticVectorVerifiedReadV1::new(snapshot.clone());
        let generation =
            read_cataloged_generation_records(&read, &generation_id, authority.cancellation())?
                .ok_or_else(|| {
                    VectorGenerationStoreErrorV1::ResetRequired(
                        "published semantic vector stage has no exact generation records"
                            .to_owned(),
                    )
                })?;
        let publication = VectorGenerationPublicationV1 {
            generation_id,
            manifest_digest: generation.generation.manifest_digest().clone(),
            checkpoint: generation.generation.checkpoint().clone(),
        };
        self.install_snapshot(snapshot)?;
        Ok(publication)
    }

    pub(super) fn cancel_generation_records(
        &self,
        build_id: &VectorGenerationBuildIdV1,
        cancellation: Arc<dyn GraphCancellation>,
    ) -> Result<bool, VectorGenerationStoreErrorV1> {
        let authority = SemanticGraphExecutionAuthorityV1::new(
            Arc::clone(&cancellation),
            Instant::now() + GRAPH_OPERATION_DEADLINE,
        );
        authority.checkpoint().map_err(map_graph_error)?;
        let stage = {
            let pending = self.pending.lock().map_err(|_| {
                VectorGenerationStoreErrorV1::Unavailable(
                    "semantic vector pending build lock is poisoned".to_owned(),
                )
            })?;
            pending
                .get(build_id)
                .map(|pending| pending.stage.plan.key.clone())
        };
        let Some(stage) = stage else {
            return Ok(false);
        };
        let outcome = self
            .runtime
            .cancel_stage(&stage, &authority)
            .map_err(map_graph_error)?;
        match outcome {
            SemanticVectorStageCancelOutcome::Cancelled(_)
            | SemanticVectorStageCancelOutcome::ExactReplay(_) => {
                self.pending
                    .lock()
                    .map_err(|_| {
                        VectorGenerationStoreErrorV1::Unavailable(
                            "semantic vector pending build lock is poisoned".to_owned(),
                        )
                    })?
                    .remove(build_id);
                Ok(true)
            }
            SemanticVectorStageCancelOutcome::MissingStage => {
                Err(VectorGenerationStoreErrorV1::ResetRequired(
                    "semantic vector stage disappeared before cancellation".to_owned(),
                ))
            }
            SemanticVectorStageCancelOutcome::ReadyToPublish(_)
            | SemanticVectorStageCancelOutcome::StaleFence { .. } => {
                Err(VectorGenerationStoreErrorV1::ConcurrentMutation)
            }
        }
    }

    pub(super) fn commit_batch_records(
        &self,
        build_id: &VectorGenerationBuildIdV1,
        expected_checkpoint: Option<&VectorProjectionCheckpointV1>,
        prepared: PreparedVectorGenerationV1,
        cancellation: Arc<dyn GraphCancellation>,
    ) -> Result<VectorProjectionCheckpointV1, VectorGenerationStoreErrorV1> {
        let authority = SemanticGraphExecutionAuthorityV1::new(
            Arc::clone(&cancellation),
            Instant::now() + GRAPH_OPERATION_DEADLINE,
        );
        authority.checkpoint().map_err(map_graph_error)?;
        let mut pending = self.pending.lock().map_err(|_| {
            VectorGenerationStoreErrorV1::Unavailable(
                "semantic vector pending build lock is poisoned".to_owned(),
            )
        })?;
        let pending = pending
            .get_mut(build_id)
            .ok_or(VectorGenerationStoreErrorV1::UnknownBuild)?;
        let before = pending.state.clone();
        let mut after = before.clone();
        let checkpoint = after.commit_batch_ref(build_id, expected_checkpoint, &prepared)?;
        let next_revision = pending.revision.checked_add(1).ok_or_else(|| {
            VectorGenerationStoreErrorV1::Corrupt(
                "semantic vector graph revision overflowed".to_owned(),
            )
        })?;
        let mutations = full_native_mutations(encode_generation_batch_delta(
            &after,
            build_id,
            &prepared,
            next_revision,
        )?);
        let publication = match after.publish_generation(build_id) {
            Ok(publication) => Some(publication),
            Err(VectorGenerationStoreErrorV1::IncompleteGeneration) => None,
            Err(error) => return Err(error),
        };
        let next_watermark = GraphWatermark::new(format!(
            "semantic-vector-stage:{}:{}",
            pending.stage.next_ordinal,
            canonical_sha256(&checkpoint)
                .map_err(storage_error)?
                .as_str()
        ))
        .map_err(map_graph_error)?;
        let scope = self.runtime.scope();
        let mut batch = GraphWriteBatch::new(
            scope.projection().namespace.clone(),
            scope.projection().projection.clone(),
            SourceGeneration::new(prepared.request.changes.to_generation.to_string())
                .map_err(map_graph_error)?,
            next_watermark.clone(),
            mutations,
            Arc::clone(&cancellation),
        )
        .map_err(map_graph_error)?;
        let native_output = batch
            .semantic_vector_output_digest()
            .map_err(map_graph_error)?;
        let receipt = stage_batch_receipt(
            &pending.stage,
            &after,
            build_id,
            &prepared,
            &checkpoint,
            native_output,
        )?;
        self.runtime
            .append_stage_batch(&receipt, batch, &authority)
            .map_err(map_graph_error)?;
        pending.stage = match self
            .runtime
            .resume_stage(&pending.stage.plan.key, &authority)
            .map_err(map_graph_error)?
        {
            SemanticVectorStageResumeOutcome::Pending(stage)
            | SemanticVectorStageResumeOutcome::Ready(stage) => stage,
            SemanticVectorStageResumeOutcome::Missing => {
                return Err(VectorGenerationStoreErrorV1::ResetRequired(
                    "semantic vector stage disappeared after batch settlement".to_owned(),
                ));
            }
            SemanticVectorStageResumeOutcome::Published { .. }
            | SemanticVectorStageResumeOutcome::Cancelled(_) => {
                return Err(VectorGenerationStoreErrorV1::ConcurrentMutation);
            }
        };
        pending.state = after;
        pending.revision = next_revision;
        pending.publication = publication;
        Ok(checkpoint)
    }

    pub(super) fn publish_generation_records(
        &self,
        build_id: &VectorGenerationBuildIdV1,
        cancellation: Arc<dyn GraphCancellation>,
    ) -> Result<VectorGenerationPublicationV1, VectorGenerationStoreErrorV1> {
        let authority = SemanticGraphExecutionAuthorityV1::new(
            Arc::clone(&cancellation),
            Instant::now() + GRAPH_OPERATION_DEADLINE,
        );
        authority.checkpoint().map_err(map_graph_error)?;
        let (stage, publication) = {
            let pending = self.pending.lock().map_err(|_| {
                VectorGenerationStoreErrorV1::Unavailable(
                    "semantic vector pending build lock is poisoned".to_owned(),
                )
            })?;
            let pending = pending
                .get(build_id)
                .ok_or(VectorGenerationStoreErrorV1::UnknownBuild)?;
            (
                pending.stage.clone(),
                pending
                    .publication
                    .clone()
                    .ok_or(VectorGenerationStoreErrorV1::IncompleteGeneration)?,
            )
        };
        match self
            .runtime
            .prepare_publication_from_staged_native(&stage.plan.key, &authority)
            .map_err(map_graph_error)?
        {
            SemanticVectorStagePublicationPrepareOutcome::ReadyToPublish(_)
            | SemanticVectorStagePublicationPrepareOutcome::ExactReplay(_) => {}
            SemanticVectorStagePublicationPrepareOutcome::Incomplete(_) => {
                return Err(VectorGenerationStoreErrorV1::IncompleteGeneration);
            }
            SemanticVectorStagePublicationPrepareOutcome::Cancelled(_)
            | SemanticVectorStagePublicationPrepareOutcome::MissingStage => {
                return Err(VectorGenerationStoreErrorV1::UnknownBuild);
            }
            SemanticVectorStagePublicationPrepareOutcome::StaleCheckpoint { .. }
            | SemanticVectorStagePublicationPrepareOutcome::StaleFence { .. }
            | SemanticVectorStagePublicationPrepareOutcome::PublicationConflict
            | SemanticVectorStagePublicationPrepareOutcome::SemanticGenerationConflict { .. }
            | SemanticVectorStagePublicationPrepareOutcome::ChunkManifestConflict { .. } => {
                return Err(VectorGenerationStoreErrorV1::ConcurrentMutation);
            }
        }
        let snapshot = self
            .runtime
            .publish_ready_stage(&stage.plan.key, &authority)
            .map_err(map_graph_error)?;
        let verified_head = snapshot.verified_head().clone();
        if verified_head.key != stage.plan.publication_key {
            return Err(VectorGenerationStoreErrorV1::ResetRequired(
                "semantic vector publication installed no matching verified head".to_owned(),
            ));
        }
        // The publication above is durably committed, so settlement is a
        // completion obligation: request cancellation must not abandon it and
        // an interrupted settlement must not report the committed publication
        // as cancelled. The stage record stays replayable, so interruption is
        // durability uncertainty, not failure.
        let settlement_authority = SemanticGraphExecutionAuthorityV1::new(
            Arc::new(NeverCancelled),
            Instant::now() + GRAPH_OPERATION_DEADLINE,
        );
        match self.runtime.settle_published(
            &SemanticVectorStagePublishSettlement {
                stage: stage.plan.key,
                verified_head,
            },
            &settlement_authority,
        ) {
            Ok(
                SemanticVectorStagePublishOutcome::Published(_)
                | SemanticVectorStagePublishOutcome::ExactReplay(_),
            ) => {}
            Ok(
                SemanticVectorStagePublishOutcome::VerifiedHeadConflict
                | SemanticVectorStagePublishOutcome::SemanticGenerationConflict { .. }
                | SemanticVectorStagePublishOutcome::NotReady(_)
                | SemanticVectorStagePublishOutcome::StaleFence { .. },
            ) => {
                return Err(VectorGenerationStoreErrorV1::ConcurrentMutation);
            }
            Ok(SemanticVectorStagePublishOutcome::MissingStage) => {
                return Err(VectorGenerationStoreErrorV1::ResetRequired(
                    "published semantic vector stage disappeared before settlement".to_owned(),
                ));
            }
            Err(error @ (GraphDbError::Cancelled | GraphDbError::DeadlineExceeded)) => {
                return Err(post_commit_publication_settlement_error(error));
            }
            Err(error) => return Err(map_graph_error(error)),
        }
        self.install_snapshot(snapshot)?;
        self.pending
            .lock()
            .map_err(|_| {
                VectorGenerationStoreErrorV1::Unavailable(
                    "semantic vector pending build lock is poisoned".to_owned(),
                )
            })?
            .remove(build_id);
        Ok(publication)
    }
}

fn post_commit_publication_settlement_error(error: GraphDbError) -> VectorGenerationStoreErrorV1 {
    match error {
        GraphDbError::Cancelled | GraphDbError::DeadlineExceeded => {
            VectorGenerationStoreErrorV1::DurabilityUncertain(
                "semantic vector publication committed but stage settlement was interrupted; \
                 settlement replays on the next publish drive"
                    .to_owned(),
            )
        }
        error => map_graph_error(error),
    }
}

fn semantic_stage_source_identity(
    source_scope: &StoreShardIdV1,
    binding: &StoreRuntimeBindingV1,
    dependency: &GraphGenerationDependency,
) -> Result<(StoreShardIdV1, SemanticVectorSourceDependencyV1), VectorGenerationStoreErrorV1> {
    let source_dependency = SemanticVectorSourceDependencyV1 {
        generation: tracedecay_store::GraphDependencyGenerationIdentityV1::new(
            GraphProjectionIdentityV1 {
                shard_id: binding.shard_id.clone(),
                namespace: GraphNamespaceV1::new(dependency.projection.namespace.as_str())
                    .map_err(storage_error)?,
                projection: GraphProjectionIdV1::new(dependency.projection.projection.as_str())
                    .map_err(storage_error)?,
            },
            GraphGenerationIdV1::new(dependency.generation.as_str()).map_err(storage_error)?,
        ),
        idempotency_key: GraphPublicationIdempotencyKeyV1::new(dependency.idempotency_key.as_str())
            .map_err(storage_error)?,
    };
    Ok((source_scope.clone(), source_dependency))
}

pub(super) fn full_native_mutations(state: NativeGraphStateV1) -> Vec<GraphMutation> {
    state
        .entities
        .into_iter()
        .map(GraphMutation::UpsertEntity)
        .chain(
            state
                .relations
                .into_iter()
                .map(GraphMutation::UpsertRelation),
        )
        .collect()
}

fn require_resumed_plan(
    stage: &SemanticVectorStageRecord,
    expected: &SemanticVectorStagePlan,
) -> Result<(), VectorGenerationStoreErrorV1> {
    let mut adopted = stage.plan.clone();
    adopted.writer_fence = expected.writer_fence.clone();
    if adopted == *expected {
        Ok(())
    } else {
        Err(VectorGenerationStoreErrorV1::ConcurrentMutation)
    }
}

fn require_same_semantic_plan(
    stage: &SemanticVectorStageRecord,
    expected: &SemanticVectorStagePlan,
) -> Result<(), VectorGenerationStoreErrorV1> {
    let actual = &stage.plan;
    if actual.key.projection == expected.key.projection
        && actual.semantic_generation_id == expected.semantic_generation_id
        && actual.source_scope == expected.source_scope
        && actual.source_generation == expected.source_generation
        && actual.recipe == expected.recipe
        && actual.expected_chunk_count == expected.expected_chunk_count
        && actual.initial_checkpoint_digest == expected.initial_checkpoint_digest
    {
        Ok(())
    } else {
        Err(VectorGenerationStoreErrorV1::ConcurrentMutation)
    }
}

fn stage_batch_receipt(
    stage: &SemanticVectorStageRecord,
    state: &VectorGenerationStateMachineV1,
    build_id: &VectorGenerationBuildIdV1,
    prepared: &PreparedVectorGenerationV1,
    checkpoint: &VectorProjectionCheckpointV1,
    output_digest: SemanticVectorBatchOutputDigest,
) -> Result<SemanticVectorStageBatchReceipt, VectorGenerationStoreErrorV1> {
    let input = canonical_sha256(&(
        "tracedecay.semantic-vector-stage-batch-input.v1",
        &stage.plan.key,
        &prepared.request,
        &prepared.receipt,
    ))
    .map_err(storage_error)?;
    let checkpoint_digest = canonical_sha256(checkpoint).map_err(storage_error)?;
    let chunks = semantic_stage_chunk_receipts(state, build_id, prepared)?;
    SemanticVectorStageBatchReceipt::new(
        SemanticVectorStageBatchKey {
            stage: stage.plan.key.clone(),
            ordinal: stage.next_ordinal,
        },
        stage.checkpoint_digest.clone(),
        SemanticVectorBatchInputDigest::new(input.as_str()).map_err(storage_error)?,
        output_digest,
        SemanticVectorCheckpointDigest::new(checkpoint_digest.as_str()).map_err(storage_error)?,
        chunks,
    )
    .map_err(storage_error)
}

pub(in crate::store::vector_generations) fn semantic_stage_chunk_receipts(
    state: &VectorGenerationStateMachineV1,
    build_id: &VectorGenerationBuildIdV1,
    prepared: &PreparedVectorGenerationV1,
) -> Result<Vec<SemanticVectorStageChunkReceipt>, VectorGenerationStoreErrorV1> {
    let build = state
        .staged
        .get(build_id)
        .ok_or(VectorGenerationStoreErrorV1::UnknownBuild)?;
    prepared
        .receipt
        .receipts
        .iter()
        .enumerate()
        .map(|(ordinal, receipt)| {
            let (operation, chunk_digest, output_digest) = match receipt.operation {
                ProjectionOperationV1::Added
                | ProjectionOperationV1::Updated
                | ProjectionOperationV1::Reused => {
                    let vector = build.vectors.get(&receipt.chunk_id).ok_or_else(|| {
                        VectorGenerationStoreErrorV1::Corrupt(
                            "semantic vector native receipt has no carried vector effect"
                                .to_owned(),
                        )
                    })?;
                    (
                        SemanticVectorStageChunkOperation::Embed,
                        Some(&vector.chunk_digest),
                        Some(&vector.output_digest),
                    )
                }
                ProjectionOperationV1::Deleted => (
                    SemanticVectorStageChunkOperation::Tombstone,
                    receipt.prior_chunk_digest.as_ref(),
                    None,
                ),
            };
            Ok(SemanticVectorStageChunkReceipt {
                effect_ordinal: u32::try_from(ordinal).map_err(storage_error)?,
                chunk_id: SemanticVectorChunkId::new(receipt.chunk_id.to_string())
                    .map_err(storage_error)?,
                chunk_digest: SemanticVectorChunkDigest::new(
                    chunk_digest
                        .ok_or_else(|| {
                            VectorGenerationStoreErrorV1::Corrupt(
                                "semantic vector receipt has no canonical chunk digest".to_owned(),
                            )
                        })?
                        .as_str(),
                )
                .map_err(storage_error)?,
                operation,
                output_digest: output_digest
                    .map(|digest| SemanticVectorOutputDigest::new(digest.as_str()))
                    .transpose()
                    .map_err(storage_error)?,
            })
        })
        .collect()
}

pub(super) fn transition_state<'a>(
    build: Option<&ScopedBuildRecordsV1>,
    generations: impl Iterator<Item = &'a ScopedGenerationRecordsV1>,
) -> Result<VectorGenerationStateMachineV1, VectorGenerationStoreErrorV1> {
    let staged = build
        .map(|build| -> Result<_, VectorGenerationStoreErrorV1> {
            Ok(BTreeMap::from([(
                VectorGenerationBuildIdV1(
                    canonical_sha256(&(VECTOR_GENERATION_BUILD_DIGEST_DOMAIN, &build.staged.plan))
                        .map_err(storage_error)?,
                ),
                build.staged.clone(),
            )]))
        })
        .transpose()?
        .unwrap_or_default();
    let generations = generations
        .map(|records| {
            (
                records.generation.generation_id().clone(),
                records.generation.clone(),
            )
        })
        .collect();
    let mut state = VectorGenerationStateMachineV1 {
        staged,
        published: PublishedStateV1::immutable_graph_generation(generations),
        physical_vector_pool: PhysicalVectorBytePoolV1::default(),
    };
    state.ensure_physical_reuse_index()?;
    Ok(state)
}

fn push_required_generation(
    generations: &mut Vec<ScopedGenerationRecordsV1>,
    snapshot: &super::snapshot::SemanticVectorVerifiedRead,
    generation_id: Option<&VectorGenerationIdV1>,
    cancellation: Arc<dyn GraphCancellation>,
) -> Result<(), VectorGenerationStoreErrorV1> {
    let Some(generation_id) = generation_id else {
        return Ok(());
    };
    if generations
        .iter()
        .any(|records| records.generation.generation_id() == generation_id)
    {
        return Ok(());
    }
    let records = read_cataloged_generation_records(snapshot, generation_id, cancellation)?
        .ok_or(VectorGenerationStoreErrorV1::IncompatibleBaseGeneration)?;
    generations.push(records);
    Ok(())
}

#[cfg(test)]
#[path = "transitions/tests.rs"]
mod tests;
