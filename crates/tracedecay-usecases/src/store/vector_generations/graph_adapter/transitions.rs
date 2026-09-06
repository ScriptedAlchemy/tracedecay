use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Instant;

use tracedecay_domain::{
    ManifestDigest, ProjectionOperationV1, VectorGenerationIdV1, canonical_sha256,
};
use tracedecay_graph_db::{
    GraphCancellation, GraphDbError, GraphGenerationDependency, GraphMutation, GraphWatermark,
    GraphWriteBatch, NeverCancelled, SourceGeneration, VerifiedGenerationBeginV1,
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
    BaseGenerationIncompatibilityV1, BatchCommitDecisionV1, PreparedBatchCommitV1,
    PreparedVectorGenerationV1, PublishedStateV1, StagedVectorValueRetentionV1,
    VECTOR_GENERATION_BUILD_DIGEST_DOMAIN, VectorGenerationBuildIdV1, VectorGenerationPlanV1,
    VectorGenerationPublicationV1, VectorGenerationStateMachineV1, VectorGenerationStoreErrorV1,
    VectorProjectionCheckpointV1, validate_plan,
};
use super::native_records::{
    NativeGraphStateV1, ScopedBuildRecordsV1, ScopedGenerationRecordsV1,
    encode_generation_batch_delta, read_build_records, read_generation_publication_pointer,
    read_state_metadata,
};
use super::persistence::{map_graph_error, storage_error};
use super::stage_identity::next_stage_attempt;
use super::{
    GRAPH_BACKGROUND_OPERATION_BUDGET, GRAPH_OPERATION_DEADLINE, GraphVectorGenerationStoreV1,
    VectorGenerationBeginOutcomeV1,
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
        let expected_chunk_count = descriptor.expected_chunk_count;
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
            expected_chunk_manifest_digest: descriptor.expected_chunk_manifest_digest.clone(),
        };
        let expected_prior_verified_head = self
            .runtime
            .verified_head(authority)
            .map_err(map_graph_error)?;
        let (source_scope, source_dependency) =
            semantic_stage_source_identity(source_scope, binding, scope.source_dependency())?;
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

    #[hotpath::measure(label = "usecases.store.begin_records")]
    pub(super) fn begin_generation_records(
        &self,
        plan: VectorGenerationPlanV1,
        rebuild: bool,
        cancellation: Arc<dyn GraphCancellation>,
    ) -> Result<VectorGenerationBeginOutcomeV1, VectorGenerationStoreErrorV1> {
        // Opening or re-adopting a generation can hydrate a corpus-sized graph
        // snapshot and reconcile a durable predecessor before the first batch.
        // This is scheduled projection work, so lifecycle cancellation governs
        // it; the interactive graph deadline would strand large restart paths.
        let authority = SemanticGraphExecutionAuthorityV1::new(
            Arc::clone(&cancellation),
            Instant::now() + GRAPH_BACKGROUND_OPERATION_BUDGET,
        );
        validate_plan(&plan)?;
        crate::hotpath_observe::vector_candidates(plan.expected_chunk_ids.len());
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
        {
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
            let has_snapshot = snapshot.is_some();
            drop(snapshot);
            // `open()` starts with an empty process-local pending map.
            // Snapshot-visible builds must still be adopted into pending
            // before commit_batch; otherwise the store reports UnknownBuild.
            let mut generations = Vec::new();
            if !has_snapshot && plan.base_generation.is_some() {
                return Err(VectorGenerationStoreErrorV1::IncompatibleBaseGeneration(
                    BaseGenerationIncompatibilityV1::MissingSnapshot,
                ));
            }
            self.push_required_generation(
                &mut generations,
                plan.base_generation.as_ref(),
                Arc::clone(&cancellation),
            )?;
            let mut after = transition_state(existing.as_ref(), generations.iter())?;
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
            let mut superseded_stage = false;
            let mut completion_authority = None;
            let (stage, published) = loop {
                let operation_authority = completion_authority.as_ref().unwrap_or(&authority);
                let stage_plan =
                    self.semantic_stage_plan(&plan, &attempt, &descriptor, operation_authority)?;
                let published_key = SemanticVectorPublishedGenerationKey {
                    projection: stage_plan.key.projection.clone(),
                    semantic_generation_id: stage_plan.semantic_generation_id.clone(),
                };
                if let SemanticVectorPublishedGenerationLookup::Published {
                    record,
                    verified_head,
                } = self
                    .runtime
                    .published_semantic_generation(&published_key, operation_authority)
                    .map_err(map_graph_error)?
                {
                    require_same_semantic_plan(&record, &stage_plan)?;
                    let publication = self.recover_published_generation(
                        &plan,
                        &verified_head,
                        operation_authority,
                    )?;
                    break (*record, Some(publication));
                }
                match self
                    .runtime
                    .resume_stage(&stage_plan.key, operation_authority)
                    .map_err(map_graph_error)?
                {
                    SemanticVectorStageResumeOutcome::Missing => {
                        let stage = match self
                            .runtime
                            .begin_stage(&stage_plan, operation_authority)
                            .map_err(map_graph_error)?
                        {
                            VerifiedGenerationBeginV1::Begun(stage)
                            | VerifiedGenerationBeginV1::Recovered(stage) => stage,
                            VerifiedGenerationBeginV1::Occupied { existing } => {
                                let is_superseded = existing.state
                                    == SemanticVectorStageState::Pending
                                    && (existing.plan.key.build_id != stage_plan.key.build_id
                                        || existing.plan.key.plan_digest
                                            != stage_plan.key.plan_digest);
                                if superseded_stage || !is_superseded {
                                    return Err(map_graph_error(GraphDbError::conflict_observed(
                                        "usecases.store.begin_generation.occupied_stage",
                                        format!("stage={:?}", stage_plan.key),
                                        format!("stage={:?}", existing.plan.key),
                                    )));
                                }
                                if existing.plan.writer_fence.binding
                                    == stage_plan.writer_fence.binding
                                {
                                    return Err(map_graph_error(GraphDbError::conflict_observed(
                                        "usecases.store.begin_generation.occupied_stage_active_writer",
                                        format!("writer_fence={:?}", stage_plan.writer_fence),
                                        format!("writer_fence={:?}", existing.plan.writer_fence),
                                    )));
                                }
                                let adoption_authority = completion_authority.insert(
                                    SemanticGraphExecutionAuthorityV1::new(
                                        Arc::new(NeverCancelled),
                                        Instant::now() + GRAPH_BACKGROUND_OPERATION_BUDGET,
                                    ),
                                );
                                let adopted = match self
                                    .runtime
                                    .resume_stage(&existing.plan.key, adoption_authority)
                                    .map_err(map_graph_error)?
                                {
                                    SemanticVectorStageResumeOutcome::Pending(record) => record,
                                    SemanticVectorStageResumeOutcome::Ready(record)
                                    | SemanticVectorStageResumeOutcome::Cancelled(record) => {
                                        return Err(map_graph_error(
                                            GraphDbError::conflict_observed(
                                                "usecases.store.begin_generation.adopt_superseded",
                                                "state=Pending",
                                                format!("state={:?}", record.state),
                                            ),
                                        ));
                                    }
                                    SemanticVectorStageResumeOutcome::Published {
                                        record, ..
                                    } => {
                                        return Err(map_graph_error(
                                            GraphDbError::conflict_observed(
                                                "usecases.store.begin_generation.adopt_superseded",
                                                "state=Pending",
                                                format!("state={:?}", record.state),
                                            ),
                                        ));
                                    }
                                    SemanticVectorStageResumeOutcome::Missing => {
                                        return Err(map_graph_error(GraphDbError::conflict(
                                            "usecases.store.begin_generation.adopt_superseded_missing",
                                        )));
                                    }
                                };
                                require_resumed_plan(&adopted, &existing.plan)?;
                                if adopted.plan.writer_fence != stage_plan.writer_fence {
                                    return Err(map_graph_error(GraphDbError::conflict_observed(
                                        "usecases.store.begin_generation.adopt_superseded_fence",
                                        format!("writer_fence={:?}", stage_plan.writer_fence),
                                        format!("writer_fence={:?}", adopted.plan.writer_fence),
                                    )));
                                }
                                match self
                                    .runtime
                                    .cancel_stage(&adopted.plan.key, adoption_authority)
                                    .map_err(map_graph_error)?
                                {
                                    SemanticVectorStageCancelOutcome::Cancelled(record)
                                    | SemanticVectorStageCancelOutcome::ExactReplay(record)
                                        if record.plan == adopted.plan =>
                                    {
                                        superseded_stage = true;
                                        continue;
                                    }
                                    SemanticVectorStageCancelOutcome::Cancelled(record)
                                    | SemanticVectorStageCancelOutcome::ExactReplay(record)
                                    | SemanticVectorStageCancelOutcome::ReadyToPublish(record) => {
                                        return Err(map_graph_error(
                                            GraphDbError::conflict_observed(
                                                "usecases.store.begin_generation.cancel_superseded",
                                                format!("stage={:?}", existing.plan.key),
                                                format!(
                                                    "stage={:?}, state={:?}",
                                                    record.plan.key, record.state
                                                ),
                                            ),
                                        ));
                                    }
                                    SemanticVectorStageCancelOutcome::MissingStage => {
                                        return Err(map_graph_error(GraphDbError::conflict(
                                            "usecases.store.begin_generation.cancel_superseded_missing",
                                        )));
                                    }
                                    SemanticVectorStageCancelOutcome::StaleFence { actual } => {
                                        return Err(map_graph_error(
                                            GraphDbError::conflict_observed(
                                                "usecases.store.begin_generation.cancel_superseded_fence",
                                                format!(
                                                    "writer_fence={:?}",
                                                    existing.plan.writer_fence
                                                ),
                                                format!("writer_fence={actual:?}"),
                                            ),
                                        ));
                                    }
                                }
                            }
                        };
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
                            .cancel_stage(&stage.plan.key, operation_authority)
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
                                    .resume_stage(&stage_plan.key, operation_authority)
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
                                            operation_authority,
                                        )?;
                                        break (*record, Some(publication));
                                    }
                                    SemanticVectorStageResumeOutcome::Ready(_) => {
                                        return Err(map_graph_error(GraphDbError::conflict(
                                            "usecases.store.rebuild.ready_stage",
                                        )));
                                    }
                                    SemanticVectorStageResumeOutcome::Pending(_)
                                    | SemanticVectorStageResumeOutcome::Missing
                                    | SemanticVectorStageResumeOutcome::Cancelled(_) => {
                                        return Err(map_graph_error(GraphDbError::conflict(
                                            "usecases.store.rebuild.cancelled_stage",
                                        )));
                                    }
                                }
                            }
                            SemanticVectorStageCancelOutcome::MissingStage
                            | SemanticVectorStageCancelOutcome::StaleFence { .. } => {
                                return Err(map_graph_error(GraphDbError::conflict(
                                    "usecases.store.rebuild.cancel_stage",
                                )));
                            }
                        }
                    }
                    SemanticVectorStageResumeOutcome::Published {
                        record,
                        verified_head,
                    } => {
                        require_resumed_plan(&record, &stage_plan)?;
                        let publication = self.recover_published_generation(
                            &plan,
                            &verified_head,
                            operation_authority,
                        )?;
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
                crate::hotpath_observe::vector_publication_replayed();
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
            Ok(VectorGenerationBeginOutcomeV1::ReplayFromStart { build_id: result })
        }
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
            return Err(map_graph_error(GraphDbError::conflict_observed(
                "usecases.store.recover_published_generation.verified_head",
                format!("verified_head={verified_head:?}"),
                format!("verified_head={:?}", snapshot.verified_head()),
            )));
        }
        let generation_id = VectorGenerationIdV1::new(generation_identity_digest(plan)?);
        let read = super::snapshot::SemanticVectorVerifiedReadV1::new(snapshot.clone());
        let publication =
            read_generation_publication_pointer(&read, &generation_id, authority.cancellation())?
                .ok_or_else(|| {
                VectorGenerationStoreErrorV1::ResetRequired(
                    "published semantic vector stage has no exact generation records".to_owned(),
                )
            })?;
        self.install_snapshot(snapshot)?;
        Ok(publication)
    }

    #[hotpath::measure(label = "usecases.store.cancel_records")]
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
                crate::hotpath_observe::vector_build_cancelled();
                Ok(true)
            }
            SemanticVectorStageCancelOutcome::MissingStage => {
                Err(VectorGenerationStoreErrorV1::ResetRequired(
                    "semantic vector stage disappeared before cancellation".to_owned(),
                ))
            }
            SemanticVectorStageCancelOutcome::ReadyToPublish(_)
            | SemanticVectorStageCancelOutcome::StaleFence { .. } => Err(map_graph_error(
                GraphDbError::conflict("usecases.store.cancel_generation.stage_state"),
            )),
        }
    }

    #[hotpath::measure(label = "usecases.store.commit_records")]
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
        // Decide the batch against the unmodified machine, write it durably,
        // and only then apply the decided effects in place. A failed durable
        // append drops the decision and leaves the machine byte-identical, so
        // no whole-machine clone is needed for transactionality — cloning the
        // accumulated corpus per batch is what made projection peak RSS a
        // multiple of the corpus.
        let staged_commit =
            match pending
                .state
                .validate_batch(build_id, expected_checkpoint, &prepared)?
            {
                BatchCommitDecisionV1::Replay(checkpoint) => {
                    // An idempotent replay may be the lost-ack final batch, so
                    // the completeness probe still runs before acknowledging.
                    pending.publication = match pending.state.publish_generation(build_id) {
                        Ok(publication) => Some(publication),
                        Err(VectorGenerationStoreErrorV1::IncompleteGeneration) => {
                            pending.publication.take()
                        }
                        Err(error) => return Err(error),
                    };
                    return Ok(checkpoint);
                }
                BatchCommitDecisionV1::Commit(staged_commit) => staged_commit,
            };
        let next_revision = pending.revision.checked_add(1).ok_or_else(|| {
            VectorGenerationStoreErrorV1::Corrupt(
                "semantic vector graph revision overflowed".to_owned(),
            )
        })?;
        let mutations = full_native_mutations(encode_generation_batch_delta(
            &pending.state,
            build_id,
            &prepared,
            &staged_commit,
            next_revision,
        )?);
        let next_watermark = GraphWatermark::new(format!(
            "semantic-vector-stage:{}:{}",
            pending.stage.next_ordinal,
            canonical_sha256(staged_commit.checkpoint())
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
        let receipt =
            stage_batch_receipt(&pending.stage, &prepared, &staged_commit, native_output)?;
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
                return Err(map_graph_error(GraphDbError::conflict(
                    "usecases.store.commit_batch.stage_state",
                )));
            }
        };
        let checkpoint = pending.state.apply_batch(build_id, staged_commit)?;
        // A complete corpus publishes in memory here; the durable publication
        // still happens from staged native records on the publish drive.
        pending.publication = match pending.state.publish_generation(build_id) {
            Ok(publication) => Some(publication),
            Err(VectorGenerationStoreErrorV1::IncompleteGeneration) => None,
            Err(error) => return Err(error),
        };
        pending.revision = next_revision;
        crate::hotpath_observe::vector_batch_committed(
            prepared.receipt.receipts.len(),
            checkpoint.completed_batches,
        );
        Ok(checkpoint)
    }

    #[hotpath::measure(label = "usecases.store.publish_records")]
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
        let prepare_authority = SemanticGraphExecutionAuthorityV1::new(
            Arc::clone(&cancellation),
            Instant::now() + GRAPH_BACKGROUND_OPERATION_BUDGET,
        );
        match self
            .runtime
            .prepare_publication_from_staged_native(&stage.plan.key, &prepare_authority)
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
                return Err(map_graph_error(GraphDbError::conflict(
                    "usecases.store.publish_generation.prepare",
                )));
            }
        }
        let publish_authority = SemanticGraphExecutionAuthorityV1::new(
            Arc::clone(&cancellation),
            Instant::now() + GRAPH_BACKGROUND_OPERATION_BUDGET,
        );
        let snapshot = self
            .runtime
            .publish_ready_stage(&stage.plan.key, &publish_authority)
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
                return Err(map_graph_error(GraphDbError::conflict(
                    "usecases.store.publish_generation.settle",
                )));
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
        Err(map_graph_error(GraphDbError::conflict_observed(
            "usecases.store.require_resumed_plan",
            format!("plan={expected:?}"),
            format!("plan={adopted:?}"),
        )))
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
        Err(map_graph_error(GraphDbError::conflict_observed(
            "usecases.store.require_same_semantic_plan",
            format!("plan={expected:?}"),
            format!("plan={actual:?}"),
        )))
    }
}

fn stage_batch_receipt(
    stage: &SemanticVectorStageRecord,
    prepared: &PreparedVectorGenerationV1,
    staged_commit: &PreparedBatchCommitV1,
    output_digest: SemanticVectorBatchOutputDigest,
) -> Result<SemanticVectorStageBatchReceipt, VectorGenerationStoreErrorV1> {
    let input = canonical_sha256(&(
        "tracedecay.semantic-vector-stage-batch-input.v1",
        &stage.plan.key,
        &prepared.request,
        &prepared.receipt,
    ))
    .map_err(storage_error)?;
    let checkpoint_digest = canonical_sha256(staged_commit.checkpoint()).map_err(storage_error)?;
    let chunks = semantic_stage_chunk_receipts(prepared, staged_commit)?;
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
    prepared: &PreparedVectorGenerationV1,
    staged_commit: &PreparedBatchCommitV1,
) -> Result<Vec<SemanticVectorStageChunkReceipt>, VectorGenerationStoreErrorV1> {
    let prepared_vectors = prepared
        .vectors
        .iter()
        .map(|vector| (&vector.chunk_id, vector))
        .collect::<BTreeMap<_, _>>();
    prepared
        .receipt
        .receipts
        .iter()
        .enumerate()
        .map(|(ordinal, receipt)| {
            let (operation, chunk_digest, output_digest) = match receipt.operation {
                ProjectionOperationV1::Added | ProjectionOperationV1::Updated => {
                    let vector = prepared_vectors.get(&receipt.chunk_id).ok_or_else(|| {
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
                ProjectionOperationV1::Reused => {
                    if !staged_commit.has_vector_effect(&receipt.chunk_id) {
                        return Err(VectorGenerationStoreErrorV1::Corrupt(
                            "semantic vector reused receipt has no staged lineage vector"
                                .to_owned(),
                        ));
                    }
                    (
                        SemanticVectorStageChunkOperation::Reuse,
                        receipt.current_chunk_digest.as_ref(),
                        None,
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
            // Hydrated staged rows were row-validated against the admitted
            // embedding key during decode; this machine's durable bytes live
            // in the graph, so their payloads elide exactly like commit-time
            // inserts do.
            let mut staged = build.staged.clone();
            for vector in staged.vectors.values_mut() {
                vector.values = Vec::new();
            }
            Ok(BTreeMap::from([(
                VectorGenerationBuildIdV1(
                    canonical_sha256(&(VECTOR_GENERATION_BUILD_DIGEST_DOMAIN, &staged.plan))
                        .map_err(storage_error)?,
                ),
                staged,
            )]))
        })
        .transpose()?
        .unwrap_or_default();
    let generations = generations
        .map(|records| {
            (
                records.generation.generation_id().clone(),
                // Base lineage checks consume identity fields only (chunk
                // digest, output digest, projection key); the hydrated float
                // payloads are elided before installation so an incremental
                // build retains O(ids + digests) of base state, not the base
                // float corpus, for its whole multi-batch duration.
                records.generation.cloned_with_elided_payloads(),
            )
        })
        .collect();
    // The graph adapter's machine never serves float payloads and never
    // seals state documents: the durable rows are the graph's native
    // entities, so staged retention is elided, base-generation payloads are
    // elided above, and no physical reuse index is derived. Reused rows are
    // receipt-only in the native encoding and are served by the base
    // generation's own durable rows at read time.
    let mut state = VectorGenerationStateMachineV1::with_staged_value_retention(
        StagedVectorValueRetentionV1::Elided,
    );
    state.staged = staged;
    state.published = PublishedStateV1::immutable_graph_generation(generations);
    Ok(state)
}

impl GraphVectorGenerationStoreV1 {
    fn push_required_generation(
        &self,
        generations: &mut Vec<ScopedGenerationRecordsV1>,
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
        let records = self
            .read_cataloged_hydrating_published_bases(generation_id, cancellation)?
            .ok_or(VectorGenerationStoreErrorV1::IncompatibleBaseGeneration(
                BaseGenerationIncompatibilityV1::MissingSnapshot,
            ))?;
        generations.push(records);
        Ok(())
    }
}

#[cfg(test)]
#[path = "transitions/tests.rs"]
mod tests;
