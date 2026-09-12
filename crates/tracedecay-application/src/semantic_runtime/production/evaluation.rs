//! Evaluation-target inspection, verification, and projection measurement.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use tracedecay_code_index::production::CodeIndexPublishedGenerationV1;
use tracedecay_domain::{
    CodeGenerationId, ManifestDigest, ProjectionReplayReasonV1, VectorGenerationIdV1,
};
use tracedecay_graph_db::GraphCancellation;
use tracedecay_query::search_quality::ProductionCandidateNativeGenerationResourcesV1;
use tracedecay_query::search_quality::candidate_output::ProductionCandidateSemanticProjectionSourcesV1;
use tracedecay_query::search_quality::semantic_native::{
    SemanticProjectionCaseOutcomeV1, SemanticProjectionCaseSampleV1, SemanticProjectionCaseV1,
};
use tracedecay_semantic::projector::PreparedVectorGenerationV1;
use tracedecay_semantic::{
    LoadedSemanticArtifactV1, SemanticEvaluationCancellationV1,
    SemanticEvaluationProjectionBatchCachePolicyV1, SemanticEvaluationProjectionBatchCacheV1,
    SemanticEvaluationQueryFactoryV1, measure_semantic_evaluation_projection_cancellation,
    prepare_semantic_evaluation_projection,
};
use tracedecay_semantic_contracts::{
    SemanticGenerationPointerV1, SemanticModelLifecycleStateV1, SemanticRuntimeScheduleFailureV1,
};

use super::super::graph_provider::SemanticGraphExecutionAuthorityV1;
use super::super::ports::{SemanticRuntimeBackendErrorV1, SemanticRuntimeGenerationInspectorV1};
use super::evaluation_generation::PreparedSemanticEvaluationGenerationV1;
use super::evaluation_support::{
    block_on_semantic_evaluation, certify_evaluation_target_compatibility,
    check_evaluation_cancellation, configured_semantic_resource_ceiling, elapsed_micros,
    evaluation_projection_case_store, evaluation_projection_case_store_for_changes,
    evaluation_projection_plan, evaluation_projection_plan_from_canonical_chunks,
    evaluation_projection_plan_from_request, evaluation_projection_resources,
    evaluation_target_resource_requirement, installed_artifact_member_bytes,
    lifecycle_artifact_matches, lifecycle_publication_error, projection_case_sample_from_prepared,
    publish_evaluation_projection_case_isolated, revalidate_lifecycle_verification,
    revalidation_error, semantic_projection_request, semantic_runtime_backend_outcome,
    validate_evaluation_target_search_index,
};
use super::vector_projection_support::{
    commit_evaluation_prepared_generation, projection_input_bytes,
};
use super::{
    ProductionSemanticRuntimeV1, SemanticCompatibleCurrentGenerationSnapshotV1,
    SemanticEvaluationCurrentGenerationSnapshotV1, SemanticEvaluationGraphCancellationV1,
    SemanticEvaluationLifecycleVerificationV1, SemanticEvaluationPublicationLeaseV1,
    SemanticVerifiedEvaluationTargetSnapshotV1, embedding_documents,
};
use crate::store::vector_generations::{
    GraphVectorGenerationStoreV1, IsolatedSemanticEvaluationGraphV1,
    VectorGenerationBeginOutcomeV1, generation_identity_digest, isolated_semantic_evaluation_graph,
};

impl ProductionSemanticRuntimeV1 {
    /// Mint pre-evaluation resource identity from the installed artifact and
    /// the configured execution ceiling. Artifact member lengths are observed
    /// facts; the remaining fields are admission bounds that the genuine
    /// evaluator later replaces with measured report evidence.
    pub fn evaluation_target_resource_requirement(
        &self,
    ) -> Result<
        crate::config::retrieval::SemanticResourceRequirementV1,
        SemanticRuntimeBackendErrorV1,
    > {
        let artifact = installed_artifact_member_bytes(&self.lifecycle)
            .map_err(|_| SemanticRuntimeBackendErrorV1::Unavailable)?;
        evaluation_target_resource_requirement(self.resources, artifact)
    }

    /// Prepare one evaluator generation with authorities retained by the
    /// daemon's enclosing evaluation request. The optional query factory owns
    /// the one shared model runtime; the batch cache remains detached from the
    /// lifecycle and durable vector state.
    pub fn prepare_evaluation_generation_with_cache(
        &self,
        generation: &CodeIndexPublishedGenerationV1,
        query_factory: Option<&SemanticEvaluationQueryFactoryV1>,
        projection_batch_cache: Arc<SemanticEvaluationProjectionBatchCacheV1>,
        cancellation: Arc<dyn SemanticEvaluationCancellationV1>,
    ) -> Result<PreparedSemanticEvaluationGenerationV1, SemanticRuntimeScheduleFailureV1> {
        let artifact_bytes = installed_artifact_member_bytes(&self.lifecycle)?;
        let execution = self.resources;
        let artifact = LoadedSemanticArtifactV1::from_lifecycle(
            &self.lifecycle,
            generation.manifest(),
            self.resources,
            self.document_composition,
        )?;
        let artifact_digest = artifact
            .projection()
            .embedding_key()
            .model_artifact_digest
            .clone();
        let projection = artifact.projection().clone();
        let request = semantic_projection_request(generation, &projection, None)?;
        let projection_input_bytes = projection_input_bytes(generation.chunks().chunks())?;
        let started = std::time::Instant::now();
        let prepared = hotpath::measure_block!("search_eval.projection.case.clean.prepare", {
            prepare_semantic_evaluation_projection(
                artifact,
                query_factory,
                request,
                generation.chunks().chunks(),
                embedding_documents(generation),
                evaluation_projection_resources(execution)?,
                projection_batch_cache.as_ref(),
                SemanticEvaluationProjectionBatchCachePolicyV1::ReuseCompletedBatches,
                Arc::clone(&cancellation),
            )
        })?;
        PreparedSemanticEvaluationGenerationV1::new(
            generation.clone(),
            prepared,
            artifact_digest,
            artifact_bytes,
            execution,
            elapsed_micros(started),
            projection_input_bytes,
            projection_batch_cache,
            cancellation,
        )
    }

    /// Measure a genuine incremental evaluator projection from an already
    /// prepared immutable generation. This never publishes a durable pointer
    /// or relabels a clean rebuild.
    pub fn measure_incremental_evaluation_projection(
        &self,
        current: &PreparedSemanticEvaluationGenerationV1,
        generation: &CodeIndexPublishedGenerationV1,
    ) -> Result<ProductionCandidateNativeGenerationResourcesV1, SemanticRuntimeScheduleFailureV1>
    {
        let request = semantic_projection_request(
            generation,
            &current.projection,
            Some(&SemanticGenerationPointerV1 {
                generation: current.vector_generation.clone(),
                source_generation: current.source_generation.clone(),
                projection_key: current.projection.projection_key().clone(),
            }),
        )?;
        if request.changes.from_generation.as_ref() != Some(&current.source_generation)
            || request.changes.added_or_changed.is_empty()
            || request.changes.added_or_changed.len() >= generation.chunks().chunks().len()
        {
            return Err(SemanticRuntimeScheduleFailureV1::Projection);
        }
        let changed = request
            .changes
            .added_or_changed
            .iter()
            .map(|change| &change.chunk_id)
            .collect::<BTreeSet<_>>();
        let chunks = generation
            .chunks()
            .chunks()
            .iter()
            .filter(|chunk| changed.contains(&chunk.id))
            .cloned()
            .collect::<Vec<_>>();
        let artifact = LoadedSemanticArtifactV1::from_lifecycle(
            &self.lifecycle,
            generation.manifest(),
            self.resources,
            self.document_composition,
        )?;
        let started = std::time::Instant::now();
        let prepared = hotpath::measure_block!("search_eval.projection.incremental.prepare", {
            prepare_semantic_evaluation_projection(
                artifact,
                Some(&current.query_factory),
                request,
                &chunks,
                embedding_documents(generation),
                evaluation_projection_resources(self.resources)?,
                current.projection_batch_cache.as_ref(),
                SemanticEvaluationProjectionBatchCachePolicyV1::ReuseCompletedBatches,
                Arc::clone(&current.cancellation),
            )
        })?;
        if prepared.prepared.request.changes.from_generation.as_ref()
            != Some(&current.source_generation)
            || prepared.prepared.request.changes.to_generation
                != generation.manifest().generation_id
        {
            return Err(SemanticRuntimeScheduleFailureV1::Projection);
        }
        let mut resources = current.generation_resources();
        resources.incremental_source_generation = generation.manifest().generation_id.clone();
        resources.incremental_source_manifest_digest = generation
            .projection()
            .request()
            .changes
            .manifest_digest
            .clone();
        resources.incremental_rebuild_micros = elapsed_micros(started);
        Ok(resources)
    }

    pub fn measure_evaluation_projection_cases(
        &self,
        clean: &PreparedSemanticEvaluationGenerationV1,
        sources: &ProductionCandidateSemanticProjectionSourcesV1<'_>,
    ) -> Result<
        BTreeMap<SemanticProjectionCaseV1, SemanticProjectionCaseSampleV1>,
        SemanticRuntimeScheduleFailureV1,
    > {
        block_on_semantic_evaluation(
            self.measure_evaluation_projection_cases_isolated(clean, sources),
        )
    }

    pub(super) async fn measure_evaluation_projection_cases_isolated(
        &self,
        clean: &PreparedSemanticEvaluationGenerationV1,
        sources: &ProductionCandidateSemanticProjectionSourcesV1<'_>,
    ) -> Result<
        BTreeMap<SemanticProjectionCaseV1, SemanticProjectionCaseSampleV1>,
        SemanticRuntimeScheduleFailureV1,
    > {
        let graph_cancellation: Arc<dyn GraphCancellation> =
            Arc::new(SemanticEvaluationGraphCancellationV1 {
                evaluation: Arc::clone(&clean.cancellation),
            });
        let graph = hotpath::measure_block!("search_eval.projection.case_graph.materialize", {
            isolated_semantic_evaluation_graph(
                &[
                    &clean.code,
                    sources.one_symbol,
                    sources.no_op,
                    sources.deletion,
                ],
                graph_cancellation,
            )
        })
        .map_err(SemanticRuntimeScheduleFailureV1::publication)?;
        self.measure_evaluation_projection_cases_in_store(&graph, clean, sources)
            .await
    }

    pub(super) async fn measure_evaluation_projection_cases_in_store(
        &self,
        graph: &Arc<IsolatedSemanticEvaluationGraphV1>,
        clean: &PreparedSemanticEvaluationGenerationV1,
        sources: &ProductionCandidateSemanticProjectionSourcesV1<'_>,
    ) -> Result<
        BTreeMap<SemanticProjectionCaseV1, SemanticProjectionCaseSampleV1>,
        SemanticRuntimeScheduleFailureV1,
    > {
        let clean_prepared = &clean.prepared_projection;
        if clean_prepared.request.changes.to_generation != clean.source_generation
            || clean_prepared.request.changes.from_generation.is_some()
        {
            return Err(SemanticRuntimeScheduleFailureV1::projection(
                "clean evaluation projection is not a root generation",
            ));
        }
        let clean_plan = evaluation_projection_plan_from_canonical_chunks(
            clean.code.chunks().chunks(),
            &clean_prepared.request,
            None,
        );
        let clean_retained = graph
            .retained(&clean.source_generation)
            .map_err(SemanticRuntimeScheduleFailureV1::publication)?;
        let cancellation = Arc::clone(clean_retained.cancellation());
        let store = evaluation_projection_case_store(&clean_retained, clean_prepared).await?;
        let clean_build = hotpath::future!(
            store.rebuild_generation(clean_plan.clone(), Arc::clone(&cancellation)),
            label = "search_eval.projection.case.clean.graph_begin"
        )
        .await
        .map_err(SemanticRuntimeScheduleFailureV1::projection)?
        .build_id()
        .clone();

        // Replay workload: the first writer begins the durable
        // stage and dies before committing; a fresh store partition recovers
        // that stage and drives the identical prepared batch through commit
        // and publication. This measures the real replay path — durable
        // stage recovery, byte-exact batch convergence, prepare, publish,
        // settle — with zero model calls, instead of a zero-work
        // already-published lookup.
        let replay_store =
            evaluation_projection_case_store(&clean_retained, clean_prepared).await?;
        let replay_build = match replay_store
            .begin_generation(clean_plan.clone(), Arc::clone(&cancellation))
            .await
            .map_err(SemanticRuntimeScheduleFailureV1::projection)?
        {
            VectorGenerationBeginOutcomeV1::ReplayFromStart { build_id }
                if build_id == clean_build =>
            {
                build_id
            }
            VectorGenerationBeginOutcomeV1::ReplayFromStart { .. }
            | VectorGenerationBeginOutcomeV1::AlreadyPublished { .. } => {
                return Err(SemanticRuntimeScheduleFailureV1::projection(
                    "clean evaluation replay did not recover the started build",
                ));
            }
        };
        hotpath::future!(
            commit_evaluation_prepared_generation(
                &replay_store,
                &replay_build,
                clean_prepared,
                clean.code.chunks().chunks(),
                Arc::clone(&cancellation),
            ),
            label = "search_eval.projection.case.clean.graph_commit"
        )
        .await?;
        let clean_publication = hotpath::future!(
            replay_store.publish_generation(&replay_build, Arc::clone(&cancellation)),
            label = "search_eval.projection.case.clean.graph_publish"
        )
        .await
        .map_err(SemanticRuntimeScheduleFailureV1::projection)?;
        if !replay_store
            .published_generation_is_visible(
                &clean_publication.generation_id,
                Arc::clone(&cancellation),
            )
            .await
            .map_err(SemanticRuntimeScheduleFailureV1::projection)?
        {
            return Err(SemanticRuntimeScheduleFailureV1::projection(
                "clean evaluation publication is not visible after publish",
            ));
        }
        // Durable idempotency: a third partition observes the published
        // generation without re-doing any work.
        let idempotent_store =
            evaluation_projection_case_store(&clean_retained, clean_prepared).await?;
        let idempotent_started = std::time::Instant::now();
        let idempotent = hotpath::future!(
            idempotent_store.begin_generation(clean_plan, Arc::clone(&cancellation)),
            label = "search_eval.projection.case.idempotency.graph_observe"
        )
        .await
        .map_err(SemanticRuntimeScheduleFailureV1::projection)?;
        if !matches!(
            idempotent,
            VectorGenerationBeginOutcomeV1::AlreadyPublished {
                publication,
                ..
            } if publication == clean_publication
        ) {
            return Err(SemanticRuntimeScheduleFailureV1::projection(
                "clean evaluation idempotent begin did not observe the published generation",
            ));
        }
        let idempotent_elapsed = elapsed_micros(idempotent_started);

        let mut samples = BTreeMap::new();
        samples.insert(
            SemanticProjectionCaseV1::Clean,
            projection_case_sample_from_prepared(
                clean_prepared,
                clean.resources.clean_projection_build_micros,
                clean.projection_input_bytes,
                SemanticProjectionCaseOutcomeV1::Complete,
            ),
        );
        samples.insert(
            SemanticProjectionCaseV1::IdempotencyReplay,
            SemanticProjectionCaseSampleV1 {
                outcome: SemanticProjectionCaseOutcomeV1::Complete,
                elapsed_micros: idempotent_elapsed,
                input_bytes: 0,
                chunks_added_or_changed: 0,
                chunks_deleted: 0,
                chunks_reused: 0,
                projection_calls: 0,
            },
        );

        let clean_pointer = SemanticGenerationPointerV1 {
            generation: clean_publication.generation_id.clone(),
            source_generation: clean_prepared.request.changes.to_generation.clone(),
            projection_key: clean_prepared.request.target_projection_key.clone(),
        };
        let (one_symbol, one_symbol_elapsed, one_symbol_input) = hotpath::measure_block!(
            "search_eval.projection.case.one_symbol.prepare",
            self.prepare_projection_case(
                sources.one_symbol,
                Some(&clean_pointer),
                &clean.query_factory,
                &clean.projection_batch_cache,
                &clean.cancellation,
            )
        )?;
        let one_symbol_retained = graph
            .retained(&one_symbol.request.changes.to_generation)
            .map_err(SemanticRuntimeScheduleFailureV1::publication)?;
        let one_symbol_store =
            evaluation_projection_case_store(&one_symbol_retained, &one_symbol).await?;
        let one_symbol_publication = hotpath::future!(
            publish_evaluation_projection_case_isolated(
                &one_symbol_store,
                &cancellation,
                sources.one_symbol,
                &one_symbol,
                Some(clean_publication.generation_id.clone()),
            ),
            label = "search_eval.projection.case.one_symbol.graph_publish"
        )
        .await?;
        samples.insert(
            SemanticProjectionCaseV1::OneSymbol,
            projection_case_sample_from_prepared(
                &one_symbol,
                one_symbol_elapsed,
                one_symbol_input,
                SemanticProjectionCaseOutcomeV1::Complete,
            ),
        );

        let one_symbol_pointer = SemanticGenerationPointerV1 {
            generation: one_symbol_publication.generation_id.clone(),
            source_generation: one_symbol.request.changes.to_generation.clone(),
            projection_key: one_symbol.request.target_projection_key.clone(),
        };
        let (no_op, no_op_elapsed, no_op_input) = hotpath::measure_block!(
            "search_eval.projection.case.no_op.prepare",
            self.prepare_projection_case(
                sources.no_op,
                Some(&one_symbol_pointer),
                &clean.query_factory,
                &clean.projection_batch_cache,
                &clean.cancellation,
            )
        )?;
        let no_op_retained = graph
            .retained(&no_op.request.changes.to_generation)
            .map_err(SemanticRuntimeScheduleFailureV1::publication)?;
        let no_op_store = evaluation_projection_case_store(&no_op_retained, &no_op).await?;
        let no_op_publication = hotpath::future!(
            publish_evaluation_projection_case_isolated(
                &no_op_store,
                &cancellation,
                sources.no_op,
                &no_op,
                Some(one_symbol_publication.generation_id.clone()),
            ),
            label = "search_eval.projection.case.no_op.graph_publish"
        )
        .await?;
        samples.insert(
            SemanticProjectionCaseV1::NoOp,
            projection_case_sample_from_prepared(
                &no_op,
                no_op_elapsed,
                no_op_input,
                SemanticProjectionCaseOutcomeV1::Complete,
            ),
        );

        let no_op_pointer = SemanticGenerationPointerV1 {
            generation: no_op_publication.generation_id.clone(),
            source_generation: no_op.request.changes.to_generation.clone(),
            projection_key: no_op.request.target_projection_key.clone(),
        };
        let (deletion, deletion_elapsed, deletion_input) = hotpath::measure_block!(
            "search_eval.projection.case.deletion.prepare",
            self.prepare_projection_case(
                sources.deletion,
                Some(&no_op_pointer),
                &clean.query_factory,
                &clean.projection_batch_cache,
                &clean.cancellation,
            )
        )?;
        let deletion_retained = graph
            .retained(&deletion.request.changes.to_generation)
            .map_err(SemanticRuntimeScheduleFailureV1::publication)?;
        let deletion_store =
            evaluation_projection_case_store(&deletion_retained, &deletion).await?;
        let _deletion_publication = hotpath::future!(
            publish_evaluation_projection_case_isolated(
                &deletion_store,
                &cancellation,
                sources.deletion,
                &deletion,
                Some(no_op_publication.generation_id.clone()),
            ),
            label = "search_eval.projection.case.deletion.graph_publish"
        )
        .await?;
        samples.insert(
            SemanticProjectionCaseV1::Deletion,
            projection_case_sample_from_prepared(
                &deletion,
                deletion_elapsed,
                deletion_input,
                SemanticProjectionCaseOutcomeV1::Complete,
            ),
        );

        let cancellation_artifact = LoadedSemanticArtifactV1::from_lifecycle(
            &self.lifecycle,
            sources.deletion.manifest(),
            self.resources,
            self.document_composition,
        )?;
        let cancellation_projection = cancellation_artifact.projection().clone();
        let cancellation_request =
            semantic_projection_request(sources.deletion, &cancellation_projection, None)?;
        let cancellation_changed = cancellation_request
            .changes
            .added_or_changed
            .iter()
            .map(|change| &change.chunk_id)
            .collect::<BTreeSet<_>>();
        let cancellation_chunks = sources
            .deletion
            .chunks()
            .chunks()
            .iter()
            .filter(|chunk| cancellation_changed.contains(&chunk.id))
            .cloned()
            .collect::<Vec<_>>();
        if cancellation_chunks.len() != cancellation_request.changes.added_or_changed.len() {
            return Err(SemanticRuntimeScheduleFailureV1::Projection);
        }
        let cancellation_input = projection_input_bytes(&cancellation_chunks)?;
        let cancellation_store = evaluation_projection_case_store_for_changes(
            &deletion_retained,
            cancellation_projection,
            &cancellation_request.changes,
        )
        .await?;
        let cancellation_started = std::time::Instant::now();
        let graph_authority = SemanticGraphExecutionAuthorityV1::new(
            Arc::clone(&cancellation),
            std::time::Instant::now() + std::time::Duration::from_secs(30),
        );
        let cancellation_revision_before = cancellation_store
            .verified_revision(Arc::clone(&cancellation))
            .await
            .map_err(SemanticRuntimeScheduleFailureV1::projection)?;
        let cancellation_head_before = deletion_retained
            .runtime()
            .verified_head(&graph_authority)
            .map_err(SemanticRuntimeScheduleFailureV1::projection)?;
        let cancellation_plan =
            evaluation_projection_plan_from_request(sources.deletion, &cancellation_request, None)?;
        let cancellation_generation = VectorGenerationIdV1::new(
            generation_identity_digest(&cancellation_plan)
                .map_err(SemanticRuntimeScheduleFailureV1::projection)?,
        );
        let cancellation_build = hotpath::future!(
            cancellation_store.begin_generation(cancellation_plan, Arc::clone(&cancellation)),
            label = "search_eval.projection.case.cancellation.graph_begin"
        )
        .await
        .map_err(SemanticRuntimeScheduleFailureV1::projection)?;
        let VectorGenerationBeginOutcomeV1::ReplayFromStart {
            build_id: cancellation_build,
        } = cancellation_build
        else {
            return Err(SemanticRuntimeScheduleFailureV1::Projection);
        };
        if cancellation_store
            .published_generation_is_visible(&cancellation_generation, Arc::clone(&cancellation))
            .await
            .map_err(SemanticRuntimeScheduleFailureV1::projection)?
        {
            return Err(SemanticRuntimeScheduleFailureV1::Projection);
        }
        let cancellation_measurement = hotpath::measure_block!(
            "search_eval.projection.case.cancellation.prepare",
            measure_semantic_evaluation_projection_cancellation(
                cancellation_artifact,
                &clean.query_factory,
                cancellation_request.clone(),
                &cancellation_chunks,
                embedding_documents(sources.deletion),
                clean.projection_batch_cache.as_ref(),
                Arc::clone(&clean.cancellation),
            )
        );
        if !hotpath::future!(
            cancellation_store.cancel_generation(&cancellation_build, Arc::clone(&cancellation),),
            label = "search_eval.projection.case.cancellation.graph_cancel"
        )
        .await
        .map_err(SemanticRuntimeScheduleFailureV1::projection)?
        {
            return Err(SemanticRuntimeScheduleFailureV1::Projection);
        }
        let cancellation_after_store = GraphVectorGenerationStoreV1::open(&deletion_retained)
            .await
            .map_err(SemanticRuntimeScheduleFailureV1::publication)?;
        if cancellation_after_store
            .published_generation_is_visible(&cancellation_generation, Arc::clone(&cancellation))
            .await
            .map_err(SemanticRuntimeScheduleFailureV1::projection)?
        {
            return Err(SemanticRuntimeScheduleFailureV1::Projection);
        }
        let cancellation_revision_after = cancellation_after_store
            .verified_revision(Arc::clone(&cancellation))
            .await
            .map_err(SemanticRuntimeScheduleFailureV1::projection)?;
        let cancellation_head_after = deletion_retained
            .runtime()
            .verified_head(&graph_authority)
            .map_err(SemanticRuntimeScheduleFailureV1::projection)?;
        if cancellation_revision_after != cancellation_revision_before
            || cancellation_head_after != cancellation_head_before
        {
            return Err(SemanticRuntimeScheduleFailureV1::Projection);
        }
        let cancellation_measurement = cancellation_measurement?;
        if cancellation_measurement.projection_calls == 0
            || cancellation_measurement.projection_calls
                >= cancellation_measurement.chunks_added_or_changed
            || cancellation_measurement.chunks_added_or_changed
                != cancellation_request.changes.added_or_changed.len() as u64
        {
            return Err(SemanticRuntimeScheduleFailureV1::Projection);
        }
        samples.insert(
            SemanticProjectionCaseV1::Cancellation,
            SemanticProjectionCaseSampleV1 {
                outcome: SemanticProjectionCaseOutcomeV1::CancelledWithoutPublication,
                elapsed_micros: elapsed_micros(cancellation_started),
                input_bytes: cancellation_input,
                chunks_added_or_changed: cancellation_measurement.chunks_added_or_changed,
                chunks_deleted: cancellation_request.changes.deleted.len() as u64,
                chunks_reused: cancellation_request.changes.reused.len() as u64,
                projection_calls: cancellation_measurement.projection_calls,
            },
        );

        let (incompatible, incompatible_elapsed, incompatible_input) = hotpath::measure_block!(
            "search_eval.projection.case.incompatible.prepare",
            self.prepare_projection_case(
                sources.one_symbol,
                None,
                &clean.query_factory,
                &clean.projection_batch_cache,
                &clean.cancellation,
            )
        )?;
        if incompatible.request.changes.from_generation.is_some()
            || incompatible.request.previous_projection_key.is_some()
            || incompatible.request.replay_reason
                != ProjectionReplayReasonV1::FullRebuildIncompatible
        {
            return Err(SemanticRuntimeScheduleFailureV1::Projection);
        }
        let incompatible_plan =
            evaluation_projection_plan(sources.one_symbol, &incompatible, None)?;
        let incompatible_store =
            evaluation_projection_case_store(&one_symbol_retained, &incompatible).await?;
        let incompatible_build = hotpath::future!(
            incompatible_store.rebuild_generation(incompatible_plan, Arc::clone(&cancellation),),
            label = "search_eval.projection.case.incompatible.graph_begin"
        )
        .await
        .map_err(SemanticRuntimeScheduleFailureV1::projection)?
        .build_id()
        .clone();
        hotpath::future!(
            commit_evaluation_prepared_generation(
                &incompatible_store,
                &incompatible_build,
                &incompatible,
                sources.one_symbol.chunks().chunks(),
                Arc::clone(&cancellation),
            ),
            label = "search_eval.projection.case.incompatible.graph_commit"
        )
        .await?;
        if !hotpath::future!(
            incompatible_store.cancel_generation(&incompatible_build, Arc::clone(&cancellation),),
            label = "search_eval.projection.case.incompatible.graph_cancel"
        )
        .await
        .map_err(SemanticRuntimeScheduleFailureV1::projection)?
        {
            return Err(SemanticRuntimeScheduleFailureV1::Projection);
        }
        samples.insert(
            SemanticProjectionCaseV1::IncompatibleState,
            projection_case_sample_from_prepared(
                &incompatible,
                incompatible_elapsed,
                incompatible_input,
                SemanticProjectionCaseOutcomeV1::FullRebuildIncompatible,
            ),
        );
        let required = BTreeSet::from([
            SemanticProjectionCaseV1::Clean,
            SemanticProjectionCaseV1::OneSymbol,
            SemanticProjectionCaseV1::Deletion,
            SemanticProjectionCaseV1::NoOp,
            SemanticProjectionCaseV1::IdempotencyReplay,
            SemanticProjectionCaseV1::Cancellation,
            SemanticProjectionCaseV1::IncompatibleState,
        ]);
        if samples.keys().copied().collect::<BTreeSet<_>>() != required {
            return Err(SemanticRuntimeScheduleFailureV1::Projection);
        }
        Ok(samples)
    }

    pub(super) fn prepare_projection_case(
        &self,
        generation: &CodeIndexPublishedGenerationV1,
        current: Option<&SemanticGenerationPointerV1>,
        query_factory: &SemanticEvaluationQueryFactoryV1,
        projection_batch_cache: &Arc<SemanticEvaluationProjectionBatchCacheV1>,
        cancellation: &Arc<dyn SemanticEvaluationCancellationV1>,
    ) -> Result<(PreparedVectorGenerationV1, u64, u64), SemanticRuntimeScheduleFailureV1> {
        let artifact = LoadedSemanticArtifactV1::from_lifecycle(
            &self.lifecycle,
            generation.manifest(),
            self.resources,
            self.document_composition,
        )?;
        let projection = artifact.projection().clone();
        let request = semantic_projection_request(generation, &projection, current)?;
        let changed = request
            .changes
            .added_or_changed
            .iter()
            .map(|change| &change.chunk_id)
            .collect::<BTreeSet<_>>();
        let chunks = generation
            .chunks()
            .chunks()
            .iter()
            .filter(|chunk| changed.contains(&chunk.id))
            .cloned()
            .collect::<Vec<_>>();
        if chunks.len() != request.changes.added_or_changed.len() {
            return Err(SemanticRuntimeScheduleFailureV1::Projection);
        }
        let input_bytes = projection_input_bytes(&chunks)?;
        let started = std::time::Instant::now();
        let prepared = prepare_semantic_evaluation_projection(
            artifact,
            Some(query_factory),
            request,
            &chunks,
            embedding_documents(generation),
            evaluation_projection_resources(self.resources)?,
            projection_batch_cache.as_ref(),
            SemanticEvaluationProjectionBatchCachePolicyV1::ReuseCompletedBatches,
            Arc::clone(cancellation),
        )?;
        Ok((prepared.prepared, elapsed_micros(started), input_bytes))
    }

    pub async fn inspect_compatible_current_generation_snapshot(
        &self,
        required: &crate::config::retrieval::SemanticCompatibilityPinsV1,
        source_generation: &CodeGenerationId,
        source_manifest_digest: &ManifestDigest,
    ) -> Result<SemanticCompatibleCurrentGenerationSnapshotV1, SemanticRuntimeBackendErrorV1> {
        let retained = hotpath::future!(
            self.graph.graph_for_current(),
            label = "semantic.evaluation.snapshot.current_graph"
        )
        .await
        .map_err(|_| {
            tracing::warn!(
                event = "semantic_evaluation_target_snapshot",
                stage = "current_graph",
                outcome = "unavailable",
            );
            SemanticRuntimeBackendErrorV1::Unavailable
        })?;
        let cancellation = Arc::clone(retained.cancellation());
        let store = hotpath::future!(
            GraphVectorGenerationStoreV1::read_only_generation(
                &retained,
                &required.vector_generation_id,
            ),
            label = "semantic.evaluation.snapshot.vector_store"
        )
        .await
        .map_err(|_| {
            tracing::warn!(
                event = "semantic_evaluation_target_snapshot",
                stage = "vector_store",
                outcome = "unavailable",
            );
            SemanticRuntimeBackendErrorV1::Unavailable
        })?
        .ok_or_else(|| {
            tracing::warn!(
                event = "semantic_evaluation_target_snapshot",
                stage = "vector_store",
                outcome = "missing",
            );
            SemanticRuntimeBackendErrorV1::Rejected
        })?;
        let verified = hotpath::future!(
            store.generation_snapshot_for(
                &required.vector_generation_id,
                &required.projection,
                source_generation,
                source_manifest_digest,
                Arc::clone(&cancellation),
            ),
            label = "semantic.evaluation.snapshot.vector_generation"
        )
        .await
        .map_err(|_| {
            tracing::warn!(
                event = "semantic_evaluation_target_snapshot",
                stage = "vector_snapshot",
                outcome = "unavailable",
            );
            SemanticRuntimeBackendErrorV1::Unavailable
        })?;
        let verified = verified.ok_or_else(|| {
            tracing::warn!(
                event = "semantic_evaluation_target_snapshot",
                stage = "vector_snapshot",
                outcome = "missing",
            );
            SemanticRuntimeBackendErrorV1::Rejected
        })?;
        let executable_lease = hotpath::future!(
            self.inspect_generation(required),
            label = "semantic.evaluation.snapshot.executable_generation"
        )
        .await
        .inspect_err(|error| {
            tracing::warn!(
                event = "semantic_evaluation_target_snapshot",
                stage = "executable_generation",
                outcome = semantic_runtime_backend_outcome(*error),
            );
        })?;
        // Publication identity stays i64 on the wire; the graph adapter's
        // monotonic u64 revision maps 1:1 into it and can only overflow after
        // ~9.2e18 mutations, which we treat as a rejected protocol state.
        let vector_state_revision = i64::try_from(verified.revision())
            .map_err(|_| SemanticRuntimeBackendErrorV1::Rejected)?;
        Ok(SemanticCompatibleCurrentGenerationSnapshotV1 {
            executable: executable_lease.evidence().clone(),
            vector_state_revision,
            vector_generation_id: verified.generation().generation_id().clone(),
        })
    }

    /// Certify a proposed semantic evaluation target without reading accepted
    /// profile or configuration state.
    ///
    /// The candidate must bind the canonical exact-flat index, the exact
    /// current vector/source generation, installed artifact members, runtime
    /// implementation, and configured resource ceiling before a native
    /// evaluator is allowed to run.
    pub async fn inspect_verified_evaluation_target_snapshot(
        &self,
        candidate: &crate::config::retrieval::SemanticCompatibilityPinsV1,
        source_generation: &CodeGenerationId,
        source_manifest_digest: &ManifestDigest,
        capability_manifest_digest: &ManifestDigest,
        cancellation: Arc<dyn SemanticEvaluationCancellationV1>,
    ) -> Result<SemanticVerifiedEvaluationTargetSnapshotV1, SemanticRuntimeBackendErrorV1> {
        check_evaluation_cancellation(cancellation.as_ref())?;
        let measured_maximum_distance_micros = hotpath::future!(
            self.measured_acceptance_distance_micros(candidate),
            label = "semantic.evaluation.snapshot.acceptance_distance"
        )
        .await;
        let certified = hotpath::measure_block!(
            "semantic.evaluation.snapshot.certify_compatibility",
            certify_evaluation_target_compatibility(
                candidate,
                source_generation,
                source_manifest_digest,
                capability_manifest_digest,
                measured_maximum_distance_micros,
            )
        )
        .map_err(|error| {
            tracing::warn!(
                event = "semantic_evaluation_target_snapshot",
                stage = "certify_compatibility",
                outcome = semantic_runtime_backend_outcome(error),
            );
            error
        })?;
        hotpath::measure_block!(
            "semantic.evaluation.snapshot.validate_search_index",
            validate_evaluation_target_search_index(&certified.search_index_key)
        )
        .map_err(|error| {
            tracing::warn!(
                event = "semantic_evaluation_target_snapshot",
                stage = "validate_search_index",
                outcome = semantic_runtime_backend_outcome(error),
            );
            error
        })?;
        let verified = hotpath::future!(
            self.inspect_compatible_current_generation_snapshot(
                &certified,
                source_generation,
                source_manifest_digest,
            ),
            label = "semantic.evaluation.snapshot.verify_generation"
        )
        .await?;
        check_evaluation_cancellation(cancellation.as_ref())?;
        if verified.vector_generation_id != certified.vector_generation_id {
            return Err(SemanticRuntimeBackendErrorV1::Rejected);
        }
        let lifecycle_verification = hotpath::measure_block!(
            "semantic.evaluation.snapshot.verify_lifecycle",
            self.evaluation_lifecycle_verification(
                certified,
                source_generation.clone(),
                source_manifest_digest.clone(),
                capability_manifest_digest.clone(),
                verified.vector_state_revision,
            )
        )
        .map_err(|error| {
            tracing::warn!(
                event = "semantic_evaluation_target_snapshot",
                stage = "verify_lifecycle",
                outcome = semantic_runtime_backend_outcome(error),
            );
            error
        })?;
        Ok(SemanticVerifiedEvaluationTargetSnapshotV1 {
            semantic_compatibility: lifecycle_verification.compatibility.clone(),
            vector_state_revision: verified.vector_state_revision,
            vector_generation_id: verified.vector_generation_id,
            configured_resource_ceiling: configured_semantic_resource_ceiling(self.resources)?,
            lifecycle_verification,
        })
    }

    /// Recheck the opaque pre-acceptance lifecycle observation immediately
    /// before publication. A changed vector/code/lifecycle target is a CAS
    /// conflict, while a malformed or foreign lease remains rejected.
    #[hotpath::measure(label = "usecases.semantic.revalidate_target", future = true)]
    pub async fn revalidate_verified_evaluation_target(
        &self,
        verification: &SemanticEvaluationLifecycleVerificationV1,
        cancellation: Arc<dyn SemanticEvaluationCancellationV1>,
    ) -> Result<(), SemanticRuntimeBackendErrorV1> {
        check_evaluation_cancellation(cancellation.as_ref())?;
        let measured_maximum_distance_micros = self
            .measured_acceptance_distance_micros(&verification.compatibility)
            .await;
        let certified = certify_evaluation_target_compatibility(
            &verification.compatibility,
            &verification.source_generation,
            &verification.source_manifest_digest,
            &verification.capability_manifest_digest,
            measured_maximum_distance_micros,
        )
        .map_err(revalidation_error)?;
        if verification.compatibility != certified {
            return Err(SemanticRuntimeBackendErrorV1::Conflict);
        }
        let verified = self
            .inspect_compatible_current_generation_snapshot(
                &verification.compatibility,
                &verification.source_generation,
                &verification.source_manifest_digest,
            )
            .await
            .map_err(revalidation_error)?;
        check_evaluation_cancellation(cancellation.as_ref())?;
        if verified.vector_state_revision != verification.vector_state_revision
            || verified.vector_generation_id != verification.compatibility.vector_generation_id
        {
            return Err(SemanticRuntimeBackendErrorV1::Conflict);
        }
        let current = self
            .evaluation_lifecycle_verification(
                verification.compatibility.clone(),
                verification.source_generation.clone(),
                verification.source_manifest_digest.clone(),
                verification.capability_manifest_digest.clone(),
                verification.vector_state_revision,
            )
            .map_err(revalidation_error)?;
        revalidate_lifecycle_verification(verification, &current)
    }

    /// Acquire the canonical lifecycle read lease after all target checks
    /// succeed. Daemon publication holds this as its final lease through
    /// commit, so lifecycle writers cannot change the evaluated model between
    /// validation and durable profile publication.
    #[hotpath::measure(label = "usecases.semantic.evaluation_lease", future = true)]
    pub async fn acquire_verified_evaluation_target_publication_lease(
        &self,
        verification: &SemanticEvaluationLifecycleVerificationV1,
        cancellation: Arc<dyn SemanticEvaluationCancellationV1>,
    ) -> Result<SemanticEvaluationPublicationLeaseV1, SemanticRuntimeBackendErrorV1> {
        self.revalidate_verified_evaluation_target(verification, Arc::clone(&cancellation))
            .await?;
        let lifecycle = self
            .lifecycle
            .acquire_verified_evaluation_publication_lease(
                &verification.lifecycle_identity,
                cancellation,
            )
            .await
            .map_err(lifecycle_publication_error)?;
        Ok(SemanticEvaluationPublicationLeaseV1 {
            _lifecycle: lifecycle,
        })
    }

    pub(super) fn evaluation_lifecycle_verification(
        &self,
        compatibility: crate::config::retrieval::SemanticCompatibilityPinsV1,
        source_generation: CodeGenerationId,
        source_manifest_digest: ManifestDigest,
        capability_manifest_digest: ManifestDigest,
        vector_state_revision: i64,
    ) -> Result<SemanticEvaluationLifecycleVerificationV1, SemanticRuntimeBackendErrorV1> {
        let lifecycle_identity = self
            .lifecycle
            .verified_evaluation_publication_identity()
            .map_err(lifecycle_publication_error)?;
        let lifecycle_state = lifecycle_identity.state();
        if !matches!(
            lifecycle_state,
            SemanticModelLifecycleStateV1::Installed { .. }
                | SemanticModelLifecycleStateV1::Loading { .. }
                | SemanticModelLifecycleStateV1::Indexing { .. }
                | SemanticModelLifecycleStateV1::Ready { .. }
        ) || !lifecycle_artifact_matches(
            lifecycle_state,
            &compatibility.artifact_manifest_digest,
        ) {
            return Err(SemanticRuntimeBackendErrorV1::Rejected);
        }
        Ok(SemanticEvaluationLifecycleVerificationV1 {
            compatibility,
            source_generation,
            source_manifest_digest,
            capability_manifest_digest,
            vector_state_revision,
            lifecycle_identity,
        })
    }

    /// Inspect only immutable vector/source identity before native evaluation.
    /// Resource evidence does not exist yet and is therefore not fabricated
    /// from the evaluator's configured ceilings.
    #[hotpath::measure(label = "usecases.semantic.inspect_eval_snapshot", future = true)]
    pub async fn inspect_evaluation_current_generation_snapshot(
        &self,
        required: &crate::config::retrieval::SemanticCompatibilityPinsV1,
        source_generation: &CodeGenerationId,
        source_manifest_digest: &ManifestDigest,
    ) -> Result<SemanticEvaluationCurrentGenerationSnapshotV1, SemanticRuntimeBackendErrorV1> {
        let retained = self
            .graph
            .graph_for_current()
            .await
            .map_err(|_| SemanticRuntimeBackendErrorV1::Unavailable)?;
        let cancellation = Arc::clone(retained.cancellation());
        let store = GraphVectorGenerationStoreV1::read_only_generation(
            &retained,
            &required.vector_generation_id,
        )
        .await
        .map_err(|_| SemanticRuntimeBackendErrorV1::Unavailable)?
        .ok_or(SemanticRuntimeBackendErrorV1::Rejected)?;
        let verified = store
            .generation_snapshot_for(
                &required.vector_generation_id,
                &required.projection,
                source_generation,
                source_manifest_digest,
                cancellation,
            )
            .await
            .map_err(|_| SemanticRuntimeBackendErrorV1::Unavailable)?
            .ok_or(SemanticRuntimeBackendErrorV1::Rejected)?;
        Ok(SemanticEvaluationCurrentGenerationSnapshotV1 {
            vector_state_revision: i64::try_from(verified.revision())
                .map_err(|_| SemanticRuntimeBackendErrorV1::Rejected)?,
            vector_generation_id: verified.generation().generation_id().clone(),
            source_manifest_digest: verified.generation().source_manifest_digest().clone(),
        })
    }
}
