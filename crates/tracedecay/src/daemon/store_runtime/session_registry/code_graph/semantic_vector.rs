use super::*;

impl RetainedCodeGraphRuntimeV1 {
    pub(crate) fn reserve_one_semantic_vector_generation(
        &self,
        after: Option<tracedecay_store::SemanticVectorStageCensusCursor>,
        cancellation: Arc<dyn GraphCancellation>,
        deadline: Instant,
    ) -> std::result::Result<tracedecay_graph_db::SemanticVectorRetentionStep, GraphDbError> {
        let mut authority = self
            .project_database
            .semantic_vector_publication_authority()
            .map_err(|error| GraphDbError::unavailable(error.to_string()))?;
        let (_, binding) = self.semantic_vector_staging_binding();
        let writer_fence = tracedecay_store::SemanticVectorWriterFence {
            binding: binding.clone(),
        };
        self.semantic_operation(
            cancellation,
            deadline,
            "semantic-vector-retention-reserve",
            |registration, context| {
                self.graph_registry.reserve_one_semantic_vector_generation(
                    registration,
                    &mut authority,
                    context,
                    after,
                    &writer_fence,
                )
            },
        )
    }

    pub(crate) fn finalize_reserved_semantic_vector_generation(
        &self,
        reservation: tracedecay_graph_db::SemanticVectorRetirementReservation,
        authorization: &tracedecay_usecases::semantic_runtime::SemanticVectorRetentionAuthorizationV1,
        cancellation: Arc<dyn GraphCancellation>,
        deadline: Instant,
    ) -> std::result::Result<tracedecay_graph_db::SemanticVectorRetentionAction, GraphDbError> {
        if authorization.candidate() != reservation.generation_id()
            || authorization.stage_revision() != reservation.census_revision()
        {
            self.graph_registry
                .release_semantic_vector_retirement(reservation)?;
            return Err(GraphDbError::ResetRequired {
                message: "semantic vector retention authorization does not match its reservation"
                    .to_owned(),
            });
        }
        let mut authority = self
            .project_database
            .semantic_vector_publication_authority()
            .map_err(|error| GraphDbError::unavailable(error.to_string()))?;
        let (_, binding) = self.semantic_vector_staging_binding();
        let writer_fence = tracedecay_store::SemanticVectorWriterFence {
            binding: binding.clone(),
        };
        self.semantic_operation(
            cancellation,
            deadline,
            "semantic-vector-retention-finalize",
            |registration, context| {
                self.graph_registry.finalize_semantic_vector_retirement(
                    registration,
                    &mut authority,
                    context,
                    &writer_fence,
                    reservation,
                )
            },
        )
    }

    pub(crate) fn release_reserved_semantic_vector_generation(
        &self,
        reservation: tracedecay_graph_db::SemanticVectorRetirementReservation,
    ) -> std::result::Result<(), GraphDbError> {
        self.graph_registry
            .release_semantic_vector_retirement(reservation)
    }

    pub(crate) fn semantic_vector_source_generation_is_live(
        &self,
        generation: &tracedecay_store::SemanticVectorSourceGenerationId,
        expected_revision: tracedecay_store::SemanticVectorStageCensusRevision,
        cancellation: Arc<dyn GraphCancellation>,
        deadline: Instant,
    ) -> std::result::Result<bool, GraphDbError> {
        let mut authority = self
            .project_database
            .semantic_vector_publication_authority()
            .map_err(|error| GraphDbError::unavailable(error.to_string()))?;
        self.semantic_operation(
            cancellation,
            deadline,
            "semantic-vector-source-liveness",
            |_registration, context| {
                let (_, binding) = self.semantic_vector_staging_binding();
                authority
                    .source_generation_has_live_reference(
                        &binding.shard_id,
                        generation,
                        expected_revision,
                        context,
                    )
                    .map_err(map_semantic_vector_staging_error)
            },
        )
    }

    pub(crate) fn semantic_vector_source_scope_is_live(
        &self,
        source_scope: &tracedecay_store::StoreShardIdV1,
        expected_revision: tracedecay_store::SemanticVectorStageCensusRevision,
        cancellation: Arc<dyn GraphCancellation>,
        deadline: Instant,
    ) -> std::result::Result<bool, GraphDbError> {
        let mut authority = self
            .project_database
            .semantic_vector_publication_authority()
            .map_err(|error| GraphDbError::unavailable(error.to_string()))?;
        self.semantic_operation(
            cancellation,
            deadline,
            "semantic-vector-source-scope-liveness",
            |_registration, context| {
                let (_, binding) = self.semantic_vector_staging_binding();
                authority
                    .source_scope_has_live_reference(
                        &binding.shard_id,
                        source_scope,
                        expected_revision,
                        context,
                    )
                    .map_err(map_semantic_vector_staging_error)
            },
        )
    }

    pub(crate) fn semantic_vector_published_generation_dependency(
        &self,
        generation: &tracedecay_domain::VectorGenerationIdV1,
        expected_revision: tracedecay_store::SemanticVectorStageCensusRevision,
        cancellation: Arc<dyn GraphCancellation>,
        deadline: Instant,
    ) -> std::result::Result<
        tracedecay_store::SemanticVectorPublishedGenerationDependencyLookup,
        GraphDbError,
    > {
        let mut authority = self
            .project_database
            .semantic_vector_publication_authority()
            .map_err(|error| GraphDbError::unavailable(error.to_string()))?;
        self.semantic_operation(
            cancellation,
            deadline,
            "semantic-vector-published-dependency",
            |_registration, context| {
                let (_, binding) = self.semantic_vector_staging_binding();
                authority
                    .published_generation_dependency(
                        &binding.shard_id,
                        generation,
                        expected_revision,
                        context,
                    )
                    .map_err(map_semantic_vector_staging_error)
            },
        )
    }

    pub(crate) fn validate_semantic_vector_project_census_revision(
        &self,
        expected_revision: tracedecay_store::SemanticVectorStageCensusRevision,
        cancellation: Arc<dyn GraphCancellation>,
        deadline: Instant,
    ) -> std::result::Result<(), GraphDbError> {
        let mut authority = self
            .project_database
            .semantic_vector_publication_authority()
            .map_err(|error| GraphDbError::unavailable(error.to_string()))?;
        self.semantic_operation(
            cancellation,
            deadline,
            "semantic-vector-census-revision",
            |_registration, context| {
                let (_, binding) = self.semantic_vector_staging_binding();
                authority
                    .validate_project_census_revision(&binding.shard_id, expected_revision, context)
                    .map_err(map_semantic_vector_staging_error)
            },
        )
    }

    pub(crate) fn semantic_vector_source_scope_binding(
        &self,
        code_scope_hash: &tracedecay_store::SemanticVectorCodeScopeHash,
        expected_revision: tracedecay_store::SemanticVectorStageCensusRevision,
        cancellation: Arc<dyn GraphCancellation>,
        deadline: Instant,
    ) -> std::result::Result<tracedecay_store::SemanticVectorSourceScopeBindingLookup, GraphDbError>
    {
        let mut authority = self
            .project_database
            .semantic_vector_publication_authority()
            .map_err(|error| GraphDbError::unavailable(error.to_string()))?;
        self.semantic_operation(
            cancellation,
            deadline,
            "semantic-vector-source-scope-binding",
            |_registration, context| {
                let (_, binding) = self.semantic_vector_staging_binding();
                authority
                    .source_scope_binding(
                        &binding.shard_id,
                        code_scope_hash,
                        expected_revision,
                        context,
                    )
                    .map_err(map_semantic_vector_staging_error)
            },
        )
    }

    pub(crate) fn remove_semantic_vector_source_scope_binding(
        &self,
        code_scope_hash: &tracedecay_store::SemanticVectorCodeScopeHash,
        source_scope: &tracedecay_store::StoreShardIdV1,
        expected_revision: tracedecay_store::SemanticVectorStageCensusRevision,
        cancellation: Arc<dyn GraphCancellation>,
        deadline: Instant,
    ) -> std::result::Result<bool, GraphDbError> {
        let mut authority = self
            .project_database
            .semantic_vector_publication_authority()
            .map_err(|error| GraphDbError::unavailable(error.to_string()))?;
        self.semantic_operation(
            cancellation,
            deadline,
            "semantic-vector-remove-source-scope-binding",
            |_registration, context| {
                let (_, binding) = self.semantic_vector_staging_binding();
                authority
                    .remove_source_scope_binding(
                        &binding.shard_id,
                        code_scope_hash,
                        source_scope,
                        expected_revision,
                        context,
                    )
                    .map_err(map_semantic_vector_staging_error)
            },
        )
    }

    pub(crate) fn published_semantic_vector_generation(
        &self,
        key: &tracedecay_store::SemanticVectorPublishedGenerationKey,
        cancellation: Arc<dyn GraphCancellation>,
        deadline: Instant,
    ) -> std::result::Result<tracedecay_store::SemanticVectorPublishedGenerationLookup, GraphDbError>
    {
        let mut authority = self
            .project_database
            .semantic_vector_publication_authority()
            .map_err(|error| GraphDbError::unavailable(error.to_string()))?;
        self.semantic_operation(
            cancellation,
            deadline,
            "published-semantic-generation",
            |registration, context| {
                self.graph_registry.published_semantic_generation(
                    registration,
                    &mut authority,
                    context,
                    key,
                )
            },
        )
    }

    pub(crate) fn semantic_vector_verified_head(
        &self,
        projection: &GraphProjectionIdentity,
        cancellation: Arc<dyn GraphCancellation>,
        deadline: Instant,
    ) -> std::result::Result<Option<tracedecay_store::GraphVerifiedHeadV1>, GraphDbError> {
        let projection = self.relational_projection(projection)?;
        let mut authority = self
            .project_database
            .semantic_vector_publication_authority()
            .map_err(|error| GraphDbError::unavailable(error.to_string()))?;
        self.semantic_operation(cancellation, deadline, "head", |_registration, context| {
            authority
                .verified_head(&projection, context)
                .map_err(map_publication_error)
        })
    }

    pub(crate) fn begin_semantic_vector_stage(
        &self,
        plan: &SemanticVectorStagePlan,
        cancellation: Arc<dyn GraphCancellation>,
        deadline: Instant,
    ) -> std::result::Result<SemanticVectorStageRecord, GraphDbError> {
        let mut authority = self
            .project_database
            .semantic_vector_publication_authority()
            .map_err(|error| GraphDbError::unavailable(error.to_string()))?;
        self.semantic_operation(
            cancellation,
            deadline,
            "stage-begin",
            |registration, context| {
                self.graph_registry.begin_verified_generation(
                    registration,
                    &mut authority,
                    context,
                    plan,
                )
            },
        )
    }

    pub(crate) fn resume_semantic_vector_stage(
        &self,
        stage: &SemanticVectorStageKey,
        cancellation: Arc<dyn GraphCancellation>,
        deadline: Instant,
    ) -> std::result::Result<SemanticVectorStageResumeOutcome, GraphDbError> {
        let mut authority = self
            .project_database
            .semantic_vector_publication_authority()
            .map_err(|error| GraphDbError::unavailable(error.to_string()))?;
        self.semantic_operation(
            cancellation,
            deadline,
            "stage-resume",
            |registration, context| {
                self.graph_registry.resume_generation_stage(
                    registration,
                    &mut authority,
                    context,
                    stage,
                )
            },
        )
    }

    pub(crate) fn append_semantic_vector_stage_batch(
        &self,
        receipt: &SemanticVectorStageBatchReceipt,
        batch: GraphWriteBatch,
        cancellation: Arc<dyn GraphCancellation>,
        deadline: Instant,
    ) -> std::result::Result<VerifiedGenerationBatchCommit, GraphDbError> {
        let mut authority = self
            .project_database
            .semantic_vector_publication_authority()
            .map_err(|error| GraphDbError::unavailable(error.to_string()))?;
        self.semantic_operation(
            Arc::clone(&cancellation),
            deadline,
            "stage-receipt",
            |_registration, context| {
                let record = authority
                    .stage(&receipt.key.stage, context)
                    .map_err(map_semantic_vector_staging_error)?
                    .ok_or_else(|| GraphDbError::ResetRequired {
                        message: "semantic vector stage is missing".to_owned(),
                    })?;
                match authority
                    .append_stage_batch(receipt, &record.plan.writer_fence, context)
                    .map_err(map_semantic_vector_staging_error)?
                {
                    tracedecay_store::SemanticVectorStageAppendOutcome::Appended {
                        effect, ..
                    }
                    | tracedecay_store::SemanticVectorStageAppendOutcome::ExactReplay {
                        effect,
                        ..
                    } => require_retriable_stage_effect(effect.state)?,
                    tracedecay_store::SemanticVectorStageAppendOutcome::InputConflict {
                        ..
                    }
                    | tracedecay_store::SemanticVectorStageAppendOutcome::DuplicateChunk {
                        ..
                    }
                    | tracedecay_store::SemanticVectorStageAppendOutcome::StaleOrdinal { .. }
                    | tracedecay_store::SemanticVectorStageAppendOutcome::StaleCheckpoint {
                        ..
                    }
                    | tracedecay_store::SemanticVectorStageAppendOutcome::StaleFence { .. }
                    | tracedecay_store::SemanticVectorStageAppendOutcome::ReadyToPublish(_)
                    | tracedecay_store::SemanticVectorStageAppendOutcome::Cancelled(_) => {
                        return Err(GraphDbError::Conflict);
                    }
                    tracedecay_store::SemanticVectorStageAppendOutcome::MissingStage => {
                        return Err(GraphDbError::ResetRequired {
                            message: "semantic vector stage disappeared before append".to_owned(),
                        });
                    }
                }
                Ok(())
            },
        )?;
        let applied = self.semantic_operation(
            Arc::clone(&cancellation),
            deadline,
            "stage-apply",
            |registration, context| {
                self.graph_registry.apply_verified_generation_batch(
                    registration,
                    &mut authority,
                    context,
                    &receipt.key,
                    &receipt.receipt_digest,
                    batch,
                )
            },
        )?;
        let effect = self
            .semantic_operation(
                Arc::new(tracedecay_graph_db::NeverCancelled),
                Instant::now() + GRAPH_OPERATION_DEADLINE,
                "stage-settle-batch",
                |registration, context| {
                    self.graph_registry.settle_verified_generation_batch(
                        registration,
                        &mut authority,
                        context,
                        &receipt.key,
                        &receipt.receipt_digest,
                    )
                },
            )
            .map_err(post_commit_batch_settlement_error)?;
        Ok(VerifiedGenerationBatchCommit {
            commit: applied.commit,
            effect,
        })
    }

    pub(crate) fn cancel_semantic_vector_stage(
        &self,
        stage: &SemanticVectorStageKey,
        cancellation: Arc<dyn GraphCancellation>,
        deadline: Instant,
    ) -> std::result::Result<SemanticVectorStageCancelOutcome, GraphDbError> {
        let mut authority = self
            .project_database
            .semantic_vector_publication_authority()
            .map_err(|error| GraphDbError::unavailable(error.to_string()))?;
        self.semantic_operation(
            cancellation,
            deadline,
            "stage-cancel",
            |registration, context| {
                self.graph_registry.cancel_generation_stage(
                    registration,
                    &mut authority,
                    context,
                    stage,
                )
            },
        )
    }

    pub(crate) fn prepare_semantic_vector_publication_from_staged_native(
        &self,
        stage: &SemanticVectorStageKey,
        cancellation: Arc<dyn GraphCancellation>,
        deadline: Instant,
    ) -> std::result::Result<SemanticVectorStagePublicationPrepareOutcome, GraphDbError> {
        let mut authority = self
            .project_database
            .semantic_vector_publication_authority()
            .map_err(|error| GraphDbError::unavailable(error.to_string()))?;
        self.semantic_operation(
            cancellation,
            deadline,
            "stage-ready",
            |registration, context| {
                self.graph_registry.prepare_publication_from_staged_native(
                    registration,
                    &mut authority,
                    context,
                    stage,
                )
            },
        )
    }

    pub(crate) fn publish_ready_semantic_vector_stage(
        &self,
        stage: &SemanticVectorStageKey,
        cancellation: Arc<dyn GraphCancellation>,
        deadline: Instant,
    ) -> std::result::Result<VerifiedGraphSnapshot, GraphDbError> {
        let mut authority = self
            .project_database
            .semantic_vector_publication_authority()
            .map_err(|error| GraphDbError::unavailable(error.to_string()))?;
        self.semantic_operation(
            cancellation,
            deadline,
            "stage-publish",
            |registration, context| {
                self.graph_registry
                    .publish_ready_generation(registration, &mut authority, context, stage)
                    .map(|commit| commit.snapshot)
            },
        )
    }

    pub(crate) fn settle_published_semantic_vector_stage(
        &self,
        settlement: &SemanticVectorStagePublishSettlement,
        cancellation: Arc<dyn GraphCancellation>,
        deadline: Instant,
    ) -> std::result::Result<SemanticVectorStagePublishOutcome, GraphDbError> {
        let mut authority = self
            .project_database
            .semantic_vector_publication_authority()
            .map_err(|error| GraphDbError::unavailable(error.to_string()))?;
        self.semantic_operation(
            cancellation,
            deadline,
            "stage-settle",
            |_registration, context| {
                let record = authority
                    .stage(&settlement.stage, context)
                    .map_err(map_semantic_vector_staging_error)?
                    .ok_or_else(|| GraphDbError::ResetRequired {
                        message: "published semantic vector stage is missing".to_owned(),
                    })?;
                authority
                    .settle_published(settlement, &record.plan.writer_fence, context)
                    .map_err(map_semantic_vector_staging_error)
            },
        )
    }
}

fn post_commit_batch_settlement_error(error: GraphDbError) -> GraphDbError {
    match error {
        GraphDbError::Cancelled | GraphDbError::DeadlineExceeded => {
            GraphDbError::DurabilityUncertain {
                message: "semantic vector batch was durably applied but stage settlement was interrupted; settlement remains replayable"
                    .to_owned(),
            }
        }
        error => error,
    }
}

#[cfg(test)]
mod settlement_tests {
    use super::*;

    #[test]
    fn post_commit_interruptions_are_durability_uncertain_not_cancelled() {
        for interruption in [GraphDbError::Cancelled, GraphDbError::DeadlineExceeded] {
            assert!(matches!(
                post_commit_batch_settlement_error(interruption),
                GraphDbError::DurabilityUncertain { ref message }
                    if message.contains("settlement remains replayable")
            ));
        }
    }
}

fn require_retriable_stage_effect(
    state: tracedecay_store::SemanticVectorStageEffectState,
) -> std::result::Result<(), GraphDbError> {
    match state {
        tracedecay_store::SemanticVectorStageEffectState::Pending
        | tracedecay_store::SemanticVectorStageEffectState::Applied => Ok(()),
        tracedecay_store::SemanticVectorStageEffectState::Failed
        | tracedecay_store::SemanticVectorStageEffectState::Cancelled => {
            Err(GraphDbError::Conflict)
        }
    }
}

fn map_semantic_vector_staging_error(
    error: tracedecay_store::SemanticVectorStagingStoreError,
) -> GraphDbError {
    match error {
        tracedecay_store::SemanticVectorStagingStoreError::InvalidRequest(error) => {
            GraphDbError::invalid(error.to_string())
        }
        tracedecay_store::SemanticVectorStagingStoreError::Interrupted(
            RuntimeInterruptionV1::Cancelled,
        ) => GraphDbError::Cancelled,
        tracedecay_store::SemanticVectorStagingStoreError::Interrupted(
            RuntimeInterruptionV1::DeadlineExceeded,
        ) => GraphDbError::DeadlineExceeded,
        tracedecay_store::SemanticVectorStagingStoreError::Infrastructure
        | tracedecay_store::SemanticVectorStagingStoreError::Busy => {
            GraphDbError::unavailable("semantic vector staging authority is unavailable")
        }
        tracedecay_store::SemanticVectorStagingStoreError::CensusRevisionChanged {
            expected,
            actual,
        } => GraphDbError::ResetRequired {
            message: format!(
                "semantic vector project census changed from revision {} to {}; restart",
                expected.get(),
                actual.get()
            ),
        },
        tracedecay_store::SemanticVectorStagingStoreError::AuthorityLost
        | tracedecay_store::SemanticVectorStagingStoreError::ReusedOperationContext => {
            GraphDbError::Conflict
        }
        tracedecay_store::SemanticVectorStagingStoreError::Corrupt(message) => {
            GraphDbError::Corrupt { message }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{GraphDbError, require_retriable_stage_effect};
    use tracedecay_store::SemanticVectorStageEffectState;

    #[test]
    fn terminal_stage_effects_cannot_be_reapplied() {
        assert!(require_retriable_stage_effect(SemanticVectorStageEffectState::Pending).is_ok());
        assert!(require_retriable_stage_effect(SemanticVectorStageEffectState::Applied).is_ok());
        assert_eq!(
            require_retriable_stage_effect(SemanticVectorStageEffectState::Failed),
            Err(GraphDbError::Conflict)
        );
        assert_eq!(
            require_retriable_stage_effect(SemanticVectorStageEffectState::Cancelled),
            Err(GraphDbError::Conflict)
        );
    }
}
