//! Fair scheduling of saved code generations into semantic projection.

use std::sync::Arc;

use tracedecay_code_index::production::CodeIndexPublishedGenerationV1;
use tracedecay_domain::{CodeGenerationId, SemanticSearchIndexProfileV1};
use tracedecay_semantic::{
    FastEmbedSemanticGenerationRequestV1, LoadedSemanticArtifactV1,
    PreparedSemanticRuntimeCommitV1, SemanticProjectionResumeOutcomeV1,
};
use tracedecay_semantic_contracts::{
    SemanticGenerationPointerV1, SemanticRuntimeScheduleFailureV1, SemanticRuntimeScheduleStatusV1,
};

use super::super::SemanticProjectionLeaseV1;
use super::evaluation_support::semantic_projection_request;
use super::publication_failure::SemanticPublicationFailureRecorderV1;
use super::published_vector_read::{PublishedSemanticVectorReadPortV1, semantic_ann_serving_index};
use super::saved_generation_schedule::fair_schedule_failure;
use super::vector_projection_support::BatchCommitStateV1;
use super::{
    CachedPublishedVectorsV1, ProductionSemanticRuntimeV1, SEMANTIC_EMBEDS_PER_COMMIT,
    ScheduledProjectionHandlesV1, embedding_documents, schedule_saved_code_generation,
};
use crate::store::vector_generations::{
    GraphVectorGenerationStoreV1, SemanticVectorStageDescriptorV1, VectorGenerationBeginOutcomeV1,
    VectorGenerationPlanV1,
};

impl ProductionSemanticRuntimeV1 {
    pub(super) fn schedule_saved_generation_fair(
        &self,
        generation: Arc<CodeIndexPublishedGenerationV1>,
        lease: SemanticProjectionLeaseV1,
    ) -> bool {
        self.schedule_saved_generation_inner(generation, Some(lease))
    }

    /// Report one refused projection schedule and answer `false`.
    ///
    /// A silent refusal is indistinguishable from an unbounded "loading":
    /// nothing else names the generation that will never be projected.
    pub(super) fn refused(
        target_generation: &CodeGenerationId,
        outcome: &'static str,
        error: &dyn std::fmt::Debug,
    ) -> bool {
        tracing::warn!(
            event = "semantic_projection_schedule",
            outcome,
            target_generation = ?target_generation,
            error = ?error,
            "semantic projection could not be scheduled for this code generation"
        );
        false
    }

    #[hotpath::measure(label = "usecases.semantic.schedule_inner")]
    pub(super) fn schedule_saved_generation_inner(
        &self,
        generation: Arc<CodeIndexPublishedGenerationV1>,
        fair_lease: Option<SemanticProjectionLeaseV1>,
    ) -> bool {
        let projection = match LoadedSemanticArtifactV1::lifecycle_projection(
            &self.lifecycle,
            generation.manifest(),
            self.resources,
            self.document_composition,
        ) {
            Ok(projection) => {
                crate::hotpath_observe::semantic_candidate_chunks(
                    generation.chunks().chunks().len(),
                );
                projection
            }
            Err(error) => {
                Self::refused(
                    &generation.manifest().generation_id,
                    "artifact_unavailable",
                    &error,
                );
                return schedule_saved_code_generation(
                    &self.handle,
                    &generation,
                    move || Err(error),
                    move || async move {
                        drop(fair_lease);
                        Err(SemanticRuntimeScheduleFailureV1::Publication)
                    },
                );
            }
        };
        // A full projection of this corpus under this projection key may have
        // already been proven to fail terminally at publish time. Rescheduling
        // it re-embeds the whole corpus inside the shared reservation before
        // failing identically, so the memo suppresses it under backoff. This is
        // a scheduling guard only: the memo clears on anything that could
        // change the outcome (key, corpus-size class, witness, or a success).
        let failure_key = super::super::SemanticPublishFailureKeyV1::new(
            projection.projection_key().clone(),
            generation.chunks().chunks().len(),
        );
        let failure_witness =
            super::super::publish_failure_witness(&self.code_index_store_root, &self.resources);
        if let super::super::SemanticPublishAdmissionV1::Suppressed(suppressed) =
            super::super::semantic_publish_failure_memo().admit(&failure_key, &failure_witness)
        {
            tracing::warn!(
                event = "semantic_projection_schedule",
                outcome = "suppressed",
                stored_failure = %suppressed.reason,
                failures = suppressed.failures,
                retry_after_ms = u64::try_from(suppressed.retry_after.as_millis())
                    .unwrap_or(u64::MAX),
                corpus_size_class = failure_key.corpus_size_class,
                projection_kind = ?failure_key.projection_key.kind,
                "semantic publication previously failed for this projection key and \
                 corpus-size class; suppressing the full re-projection until backoff elapses"
            );
            drop(fair_lease);
            return false;
        }
        // Projection publication is independent of the process cache. Without
        // an immutable prior-generation catalog input this is truthfully a
        // full rebuild; `handle.current()` is never a delta/base authority.
        let request = match semantic_projection_request(&generation, &projection, None) {
            Ok(request) => request,
            Err(error) => {
                return Self::refused(
                    &generation.manifest().generation_id,
                    "projection_request_failed",
                    &error,
                );
            }
        };
        let changed_ids = request
            .changes
            .added_or_changed
            .iter()
            .map(|change| &change.chunk_id)
            .collect::<std::collections::BTreeSet<_>>();
        let canonical_chunks = generation
            .chunks()
            .chunks()
            .iter()
            .filter(|chunk| changed_ids.contains(&chunk.id))
            .cloned()
            .collect::<Vec<_>>();
        let target_generation = generation.manifest().generation_id.clone();
        let refusal_target = target_generation.clone();
        let expected_chunk_ids = generation
            .chunks()
            .chunks()
            .iter()
            .map(|chunk| chunk.id.clone())
            .collect::<Vec<_>>();
        let base_generation = None;
        let manifest = generation.manifest().clone();
        let resources = self.resources;
        let document_composition = self.document_composition;
        let documents = embedding_documents(&generation);
        let total_units = request.changes.added_or_changed.len().max(1) as u64;
        // The plan is decided from the whole request before any batch runs, so
        // splitting the run never moves the generation identity: the plan's
        // source watermark and expected membership are the corpus's, not any
        // one batch's.
        let published_source_generation = request.changes.to_generation.clone();
        let published_projection_key = request.target_projection_key.clone();
        let stage_descriptor = match SemanticVectorStageDescriptorV1::from_changes(
            projection.clone(),
            &request.changes,
        ) {
            Ok(descriptor) => descriptor,
            Err(error) => {
                crate::hotpath_observe::semantic_stage_descriptor_failed();
                tracing::warn!(
                    event = "semantic_projection_schedule",
                    outcome = "stage_descriptor_failed",
                    expected_chunks = expected_chunk_ids.len(),
                    error = %error,
                    "semantic projection stage descriptor could not be prepared"
                );
                return false;
            }
        };
        let plan = VectorGenerationPlanV1 {
            target_projection_key: published_projection_key.clone(),
            source_generation: published_source_generation.clone(),
            source_manifest_digest: request.changes.manifest_digest.clone(),
            expected_chunk_ids: expected_chunk_ids.into(),
            base_generation: base_generation.clone(),
        };
        let search_index_key = match SemanticSearchIndexProfileV1::exact_flat_v1()
            .and_then(|profile| profile.index_key())
        {
            Ok(search_index_key) => search_index_key,
            Err(error) => {
                return Self::refused(
                    &generation.manifest().generation_id,
                    "search_index_key_failed",
                    &error,
                );
            }
        };
        // Every stage of one scheduled projection — load, resume, per-batch
        // commit, stage, publish — reaches the same five handles. Bundling them
        // once means each closure clones a single `Arc` instead of restating the
        // same five clones under a stage-specific prefix.
        let handles = Arc::new(ScheduledProjectionHandlesV1 {
            graph: Arc::clone(&self.graph),
            writer: Arc::clone(&self.vector_writer),
            generation,
            commit_state: Arc::new(tokio::sync::Mutex::new(BatchCommitStateV1::default())),
            lifecycle: Arc::clone(&self.lifecycle),
            vector_read_cache: Arc::clone(&self.vector_read_cache),
        });
        let publication_failure = SemanticPublicationFailureRecorderV1::default();
        let resume_failure = publication_failure.clone();
        let commit_failure = publication_failure.clone();
        let publish_failure = publication_failure.clone();
        let fair_lease = fair_lease.map(Arc::new);
        let load_handles = Arc::clone(&handles);
        let resume_handles = Arc::clone(&handles);
        let commit_handles = Arc::clone(&handles);
        let stage_handles = handles;
        let commit_lease = fair_lease.clone();
        let request = match FastEmbedSemanticGenerationRequestV1::new(
            target_generation,
            request,
            canonical_chunks,
            documents,
            SEMANTIC_EMBEDS_PER_COMMIT,
            move || {
                LoadedSemanticArtifactV1::from_lifecycle(
                    &load_handles.lifecycle,
                    &manifest,
                    resources,
                    document_composition,
                )
            },
            move || async move {
                let _writer = resume_handles.writer.lock().await;
                let retained = resume_handles
                    .graph
                    .graph_for_generation(resume_handles.generation.as_ref())
                    .await
                    .map_err(|error| resume_failure.retain_for_resume(&error))?;
                let cancellation = Arc::clone(retained.cancellation());
                let store = Arc::new(
                    GraphVectorGenerationStoreV1::open(&retained)
                        .await
                        .map_err(|error| resume_failure.open_store(&error))?,
                );
                store
                    .configure_stage(stage_descriptor)
                    .map_err(|error| resume_failure.configure_stage(&error))?;
                // The build identity is a digest of the plan, so reopening the
                // same plan re-adopts the same staged build rather than
                // starting a second one.
                let resume = store
                    .begin_generation(plan, Arc::clone(&cancellation))
                    .await
                    .map_err(|error| resume_failure.begin_generation(&error))?;
                let mut state = resume_handles.commit_state.lock().await;
                state.build = Some(resume.build_id().clone());
                state.store = Some(store);
                state.checkpoint = None;
                state.published = match resume {
                    VectorGenerationBeginOutcomeV1::ReplayFromStart { .. } => None,
                    VectorGenerationBeginOutcomeV1::AlreadyPublished { publication, .. } => {
                        Some(publication)
                    }
                };
                // Pending native rows are deliberately unreadable through the
                // verified snapshot. Replay bounded source batches from zero;
                // durable stage receipts and keyed native applies make each
                // replay exact after restart.
                Ok(if state.published.is_some() {
                    SemanticProjectionResumeOutcomeV1::AlreadyPublished
                } else {
                    SemanticProjectionResumeOutcomeV1::ReplayFromStart
                })
            },
            move |prepared| {
                let handles = Arc::clone(&commit_handles);
                let lease = commit_lease.clone();
                let failure = commit_failure.clone();
                async move {
                    if lease
                        .as_deref()
                        .is_some_and(SemanticProjectionLeaseV1::is_cancelled)
                    {
                        return Err(SemanticRuntimeScheduleFailureV1::Cancelled);
                    }
                    let mut state = handles.commit_state.lock().await;
                    let build = state
                        .build
                        .clone()
                        .ok_or_else(|| failure.missing_commit_build())?;
                    let store = state
                        .store
                        .as_ref()
                        .cloned()
                        .ok_or_else(|| failure.missing_commit_store())?;
                    let _writer = handles.writer.lock().await;
                    let retained = handles
                        .graph
                        .graph_for_generation(handles.generation.as_ref())
                        .await
                        .map_err(|error| failure.retain_for_batch(&error))?;
                    let cancellation = Arc::clone(retained.cancellation());
                    let next = store
                        .commit_batch(&build, state.checkpoint.as_ref(), prepared, cancellation)
                        .await
                        .map_err(|error| failure.commit_batch(&error))?;
                    state.checkpoint = Some(next);
                    Ok(())
                }
            },
            move || async move {
                let (build, store, published) = {
                    let state = stage_handles.commit_state.lock().await;
                    (
                        state
                            .build
                            .clone()
                            .ok_or_else(|| publish_failure.missing_publish_build())?,
                        state
                            .store
                            .as_ref()
                            .cloned()
                            .ok_or_else(|| publish_failure.missing_publish_store())?,
                        state.published.clone(),
                    )
                };
                let _ = stage_handles
                    .lifecycle
                    .mark_indexing(total_units, total_units);
                Ok(PreparedSemanticRuntimeCommitV1::new(move || async move {
                    let _publication_lease = fair_lease
                        .as_deref()
                        .map(SemanticProjectionLeaseV1::try_begin_publication)
                        .transpose()
                        .map_err(fair_schedule_failure)?;
                    let _writer = stage_handles.writer.lock().await;
                    let retained = stage_handles
                        .graph
                        .graph_for_generation(stage_handles.generation.as_ref())
                        .await
                        .map_err(|error| publish_failure.retain_for_publish(&error))?;
                    let cancellation = Arc::clone(retained.cancellation());
                    let publication = match published {
                        Some(publication) => publication,
                        None => store
                            .publish_generation(&build, Arc::clone(&cancellation))
                            .await
                            .map_err(|error| publish_failure.publish_generation(&error))?,
                    };
                    let active = store
                        .generation(&publication.generation_id, Arc::clone(&cancellation))
                        .await
                        .map_err(|error| publish_failure.publish_generation(&error))?
                        .ok_or(SemanticRuntimeScheduleFailureV1::Publication)?;
                    let ann = semantic_ann_serving_index(
                        &store,
                        &active,
                        &search_index_key,
                        Arc::clone(&cancellation),
                    )
                    .await
                    .map_err(|error| publish_failure.publish_generation(&error))?;
                    let port = Arc::new(
                        PublishedSemanticVectorReadPortV1::new(
                            active,
                            search_index_key.clone(),
                            stage_handles.generation.as_ref(),
                            ann,
                        )
                        .map_err(SemanticRuntimeScheduleFailureV1::projection)?,
                    );
                    let pointer = SemanticGenerationPointerV1 {
                        generation: port.generation.clone(),
                        source_generation: published_source_generation,
                        projection_key: published_projection_key,
                    };
                    let mut cached = stage_handles
                        .vector_read_cache
                        .lock()
                        .map_err(|_| SemanticRuntimeScheduleFailureV1::Runtime)?;
                    *cached = Some(CachedPublishedVectorsV1 {
                        generation: port.generation.clone(),
                        search_index_key,
                        source_generation: port.source_generation.clone(),
                        port,
                    });
                    Ok(pointer)
                }))
            },
        ) {
            Ok(request) => request,
            Err(error) => {
                return Self::refused(&refusal_target, "generation_request_failed", &error);
            }
        };
        let scheduled = self.handle.schedule_generation(request);
        if !scheduled {
            // The lifecycle is deliberately untouched above: a refused
            // schedule has no worker to drive `Loading`/`Indexing` back to a
            // terminal state, so marking progress here would strand the model
            // in an unbounded "loading" for the life of the daemon.
            return Self::refused(
                &refusal_target,
                "refused",
                &"the semantic runtime declined the work",
            );
        }
        {
            let handle = self.handle.clone();
            let lifecycle = Arc::clone(&self.lifecycle);
            // Accepted work owns the lifecycle: the poller below is the only
            // thing that can leave `Indexing`, so it is armed in the same
            // step that advances into it.
            let _ = lifecycle.mark_loading();
            let _ = lifecycle.mark_indexing(0, total_units);
            tokio::spawn(async move {
                loop {
                    match handle.status() {
                        SemanticRuntimeScheduleStatusV1::Indexing {
                            completed_units,
                            total_units,
                            ..
                        } => {
                            let _ = lifecycle.mark_indexing(completed_units, total_units);
                        }
                        SemanticRuntimeScheduleStatusV1::Current { .. } => {
                            super::super::semantic_publish_failure_memo()
                                .record_success(&failure_key);
                            let _ = lifecycle.mark_ready();
                            break;
                        }
                        SemanticRuntimeScheduleStatusV1::Failed { reason, .. } => {
                            let detail = publication_failure.receipt().map_or_else(
                                || format!("semantic runtime {reason:?}"),
                                |receipt| receipt.detail(),
                            );
                            // Publication failure is the reproducible one: it is
                            // decided by the corpus and the projection key, not
                            // by this attempt. Memoize it so the next published
                            // generation does not pay the full re-embed again.
                            if reason.is_publication() {
                                super::super::semantic_publish_failure_memo().record_failure(
                                    &failure_key,
                                    &failure_witness,
                                    &detail,
                                );
                            }
                            tracing::warn!(
                                event = "semantic_projection_schedule",
                                outcome = "failed",
                                target_generation = ?refusal_target,
                                detail = %detail,
                                "semantic projection failed for this code generation"
                            );
                            let _ = lifecycle.mark_runtime_failed(detail, true);
                            break;
                        }
                        // The pointer this projection was driving was retired
                        // under it. Nothing else will move the lifecycle, so
                        // name the retirement instead of leaving `Indexing`
                        // pinned for the life of the daemon.
                        SemanticRuntimeScheduleStatusV1::Unavailable => {
                            tracing::warn!(
                                event = "semantic_projection_schedule",
                                outcome = "retired",
                                target_generation = ?refusal_target,
                                "semantic projection was retired before it published"
                            );
                            let _ = lifecycle.mark_runtime_failed(
                                "the semantic projection was retired before it published",
                                true,
                            );
                            break;
                        }
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(25)).await;
                }
            });
        }
        scheduled
    }
}
