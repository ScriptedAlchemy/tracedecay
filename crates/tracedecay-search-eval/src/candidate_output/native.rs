//! Native semantic measurement for one generated partition.

use std::collections::{BTreeMap, BTreeSet};
use std::time::Instant;

use tracedecay_domain::{QueryFallbackSubpayload, RetrieverOutcome};
use tracedecay_query::search_quality::candidate_output::{
    CandidateOutputError, OptionalStageMeasurementV1, OptionalStageMeasurementsV1,
    ProductionCandidateNativeExecutionAuthorityV1, ProductionCandidateNativeQueryContextV1,
    ProductionCandidateNativeQueryInputsV1, ProductionCandidateNativeResourceContextV1,
    ProductionCandidateOutputV1, ProductionCandidateSemanticProjectionSourcesV1, ProfileSpecV1,
    QueryCandidateRowV1, ResourceMeasurementStatusV1, ResourceSampleV1, WorkloadQueryV1,
    fusion_profile, retrieval_budget,
};
use tracedecay_query::search_quality::semantic_native::{
    SemanticChannelAblationV1, SemanticNativeQueryInputV1, SemanticNativeQueryOutputV1,
    SemanticNativeResourceEvidenceV1, SemanticNativeResourceSampleV1, SemanticNativeStageResultV1,
    evaluate_native_query,
};

use super::{
    PublishedCorpus, canonical_scope_key, historical_candidates, map_ranked_candidate_list,
    measure_late_hydration, merge_candidate_timelines, prepare_production_query,
};

#[hotpath::measure(label = "search_eval.native.partition")]
pub(super) fn measure_native_partition(
    published: &PublishedCorpus,
    profile: &ProfileSpecV1,
    queries: &[&WorkloadQueryV1],
    authority: &dyn ProductionCandidateNativeExecutionAuthorityV1,
    workload_digest: &str,
    corpus_digest: &str,
    scale: &str,
) -> Result<
    (
        Vec<QueryCandidateRowV1>,
        SemanticNativeStageResultV1<SemanticNativeResourceSampleV1>,
    ),
    CandidateOutputError,
> {
    hotpath::gauge!("search_eval_queries_total").set(queries.len());
    hotpath::gauge!("search_eval_queries_completed").set(0_usize);
    let mut rows = None;
    let generation = &published.generation;
    let mut execute_queries = || {
        let mut measured_rows = Vec::with_capacity(queries.len());
        let mut latency_samples_us = Vec::with_capacity(queries.len());
        for (index, query) in queries.iter().enumerate() {
            let started = Instant::now();
            measured_rows.push(retrieve_one_native_query(
                published, profile, query, authority,
            )?);
            latency_samples_us.push(elapsed_micros(started));
            hotpath::gauge!("search_eval_queries_completed").set(index.saturating_add(1));
        }
        rows = Some(measured_rows);
        Ok(latency_samples_us)
    };
    let evidence = authority.measure_resources(
        ProductionCandidateNativeResourceContextV1 {
            profile,
            queries,
            code: generation,
            incremental_code: &published.incremental_generation,
            incremental_before_content_digest: &published.incremental_before_content_digest,
            incremental_after_content_digest: &published.incremental_after_content_digest,
            code_generation: &generation.manifest().generation_id,
            workload_digest,
            corpus_digest,
            scale,
            eligible_chunks: published.eligible_chunks,
            semantic_projection_sources: ProductionCandidateSemanticProjectionSourcesV1 {
                one_symbol: &published.incremental_generation,
                deletion: &published.deletion_generation,
                no_op: &published.no_op_generation,
            },
        },
        &mut execute_queries,
    )?;
    let rows = rows.ok_or_else(|| {
        CandidateOutputError::Contract(
            "native resource authority did not execute the exact query workload".to_owned(),
        )
    })?;
    if let SemanticNativeStageResultV1::Complete(sample) = &evidence {
        let source_manifest_digest = &generation.projection().request().changes.manifest_digest;
        if sample.provenance.workload_digest != workload_digest
            || sample.provenance.corpus_digest != corpus_digest
            || sample.provenance.scale != scale
            || sample.provenance.code_generation_id != generation.manifest().generation_id.as_str()
            || sample.provenance.code_source_manifest_digest != source_manifest_digest.as_str()
            || sample.provenance.incremental_code_generation_id
                != published
                    .incremental_generation
                    .manifest()
                    .generation_id
                    .as_str()
            || sample.provenance.incremental_code_source_manifest_digest
                != published
                    .incremental_generation
                    .projection()
                    .request()
                    .changes
                    .manifest_digest
                    .as_str()
            || sample.provenance.incremental_before_content_digest
                != published.incremental_before_content_digest
            || sample.provenance.incremental_after_content_digest
                != published.incremental_after_content_digest
            || sample.provenance.threads == 0
            || sample.provenance.max_concurrent_sessions == 0
            || sample.provenance.batch_size == 0
            || sample.provenance.sequence_length == 0
            || sample.provenance.load_deadline_ms == 0
            || sample.eligible_chunks != published.eligible_chunks
            || sample.measured_queries != queries.len() as u64
        {
            return Err(CandidateOutputError::Contract(
                "native resource evidence is not bound to the exact evaluator workload".to_owned(),
            ));
        }
    }
    Ok((rows, evidence))
}

pub(super) fn elapsed_micros(started: Instant) -> u64 {
    u64::try_from(started.elapsed().as_micros()).unwrap_or(u64::MAX)
}

pub(super) fn retriever_outcome_candidate_count<E>(
    outcome: &RetrieverOutcome<tracedecay_domain::RetrieverBatch<E>>,
) -> u64 {
    match outcome {
        RetrieverOutcome::Complete(batch) | RetrieverOutcome::Partial { value: batch, .. } => {
            batch.candidates.len() as u64
        }
        RetrieverOutcome::Unavailable(_)
        | RetrieverOutcome::Denied
        | RetrieverOutcome::Stale(_)
        | RetrieverOutcome::BudgetExceeded(_)
        | RetrieverOutcome::TimedOut(_)
        | RetrieverOutcome::Cancelled => 0,
    }
}

#[hotpath::measure(label = "search_eval.native.retrieve")]
fn retrieve_one_native_query(
    published: &PublishedCorpus,
    profile: &ProfileSpecV1,
    query: &WorkloadQueryV1,
    authority: &dyn ProductionCandidateNativeExecutionAuthorityV1,
) -> Result<QueryCandidateRowV1, CandidateOutputError> {
    let prepared = prepare_production_query(published, profile, query)?;
    let mut fusion = fusion_profile(profile, true)?;
    let mut native = None;
    let scope_key = canonical_scope_key(&query.allowed_scopes);
    let semantic_allowed_chunks = published
        .semantic_allowed_chunks
        .get(&scope_key)
        .ok_or_else(|| {
            CandidateOutputError::Contract(format!(
                "query {} has no precomputed semantic scope",
                query.query_id
            ))
        })?;
    let mut evaluate = |inputs: ProductionCandidateNativeQueryInputsV1<'_>| {
        if native.is_some() {
            return Err(CandidateOutputError::Contract(format!(
                "native authority evaluated query {} more than once",
                query.query_id
            )));
        }
        fusion.rerank_policy_id = inputs
            .rerank
            .as_ref()
            .map(|rerank| rerank.policy.policy_id.clone());
        native = Some(
            evaluate_native_query(SemanticNativeQueryInputV1 {
                profile_spec: profile,
                fusion_profile: &fusion,
                diversity_policy: &prepared.diversity,
                kernel: &prepared.kernel,
                fallback_lanes: &prepared.fallback_lanes,
                query_measurements: prepared.query_measurements,
                semantic: inputs.semantic,
                fallback: &prepared.fallback,
                rerank: inputs.rerank,
            })
            .map_err(|error| CandidateOutputError::Contract(error.to_string()))?,
        );
        Ok(())
    };
    authority.with_query_inputs(
        ProductionCandidateNativeQueryContextV1 {
            profile,
            query,
            request: &prepared.request,
            query_view: &prepared.query_view,
            code: &published.generation,
            code_generation: &prepared.code_generation,
            semantic_allowed_chunks,
            rerank_policy: prepared.rerank_policy.as_ref(),
        },
        &mut evaluate,
    )?;
    let mut native = native.ok_or_else(|| {
        CandidateOutputError::Contract(format!(
            "native authority did not evaluate query {}",
            query.query_id
        ))
    })?;
    let ranked = match &native.rerank.on {
        SemanticNativeStageResultV1::Complete(ranked) => ranked.clone(),
        SemanticNativeStageResultV1::NotRequested | SemanticNativeStageResultV1::Pending { .. } => {
            native.rerank.off.clone()
        }
    };
    native.measurements.hydration = Some(measure_late_hydration(
        published,
        &prepared.request,
        &ranked,
        &retrieval_budget(),
    )?);
    validate_native_query_output(profile, &prepared.fallback, &native)?;
    let ranked = map_ranked_candidate_list(published, &ranked)?;
    let (historical, historical_ranked) = historical_candidates(published, query)?;
    let ranked = merge_candidate_timelines(query, ranked, historical_ranked);
    Ok(QueryCandidateRowV1 {
        query_id: query.query_id.clone(),
        abstained: ranked.is_empty(),
        ranked,
        historical,
        native: Some(native),
    })
}

fn validate_native_query_output(
    profile: &ProfileSpecV1,
    fallback: &QueryFallbackSubpayload,
    native: &SemanticNativeQueryOutputV1,
) -> Result<(), CandidateOutputError> {
    if native.profile_id != profile.profile_id
        || native.fallback_digest != fallback.digest.as_str()
        || !native.fallback_bytes_unchanged
    {
        return Err(CandidateOutputError::Contract(format!(
            "native query output does not preserve the exact query fallback for {}",
            profile.profile_id
        )));
    }
    let observed = native
        .ablations
        .iter()
        .map(|result| result.ablation)
        .collect::<BTreeSet<_>>();
    if observed.len() != native.ablations.len()
        || !observed.contains(&SemanticChannelAblationV1::ExactLexical)
        || !observed.contains(&SemanticChannelAblationV1::QueryExactLexicalGraph)
    {
        return Err(CandidateOutputError::Contract(
            "native query output is missing required query baseline ablations".to_owned(),
        ));
    }
    for ablation in &native.ablations {
        if ablation.measurement.output_candidates != ablation.ranked_candidates.len() as u64 {
            return Err(CandidateOutputError::Contract(
                "native fusion measurement does not match its ranked output".to_owned(),
            ));
        }
    }
    let hydration = native.measurements.hydration.ok_or_else(|| {
        CandidateOutputError::Contract(
            "native query output is missing genuine late-hydration measurements".to_owned(),
        )
    })?;
    if hydration.source_fetches != hydration.receipts
        || hydration.receipts > hydration.selected_candidates
        || (hydration.receipts != 0 && hydration.bytes_hydrated == 0)
    {
        return Err(CandidateOutputError::Contract(
            "native late-hydration measurements do not match source receipts".to_owned(),
        ));
    }
    let semantic_ablations = [
        SemanticChannelAblationV1::ExactLexicalSemantic,
        SemanticChannelAblationV1::HybridExactLexicalGraphSemantic,
    ];
    match (&native.exact_flat_oracle, &native.measurements.semantic) {
        (
            SemanticNativeStageResultV1::Complete(oracle),
            SemanticNativeStageResultV1::Complete(measurement),
        ) => {
            if measurement.output_candidates != oracle.hits.len() as u64 {
                return Err(CandidateOutputError::Contract(
                    "native semantic measurement does not match the exact-flat oracle".to_owned(),
                ));
            }
            if semantic_ablations
                .iter()
                .any(|ablation| !observed.contains(ablation))
            {
                return Err(CandidateOutputError::Contract(
                    "complete semantic output is missing required channel ablations".to_owned(),
                ));
            }
        }
        (SemanticNativeStageResultV1::NotRequested, SemanticNativeStageResultV1::NotRequested)
        | (
            SemanticNativeStageResultV1::Pending { .. },
            SemanticNativeStageResultV1::Pending { .. },
        ) => {
            if semantic_ablations
                .iter()
                .any(|ablation| observed.contains(ablation))
            {
                return Err(CandidateOutputError::Contract(
                    "semantic ablations cannot exist without a complete semantic run".to_owned(),
                ));
            }
        }
        _ => {
            return Err(CandidateOutputError::Contract(
                "native semantic result and measurement states disagree".to_owned(),
            ));
        }
    }
    Ok(())
}

pub(super) fn native_optional_stage_measurements(
    profile: &ProfileSpecV1,
    rows: &[QueryCandidateRowV1],
) -> Result<OptionalStageMeasurementsV1, CandidateOutputError> {
    let native = rows
        .iter()
        .map(|row| {
            row.native.as_ref().ok_or_else(|| {
                CandidateOutputError::Contract(format!(
                    "native generation omitted query evidence {}",
                    row.query_id
                ))
            })
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(OptionalStageMeasurementsV1 {
        semantic: aggregate_native_stage(
            profile.semantic_weight_ppm != 0,
            native.iter().map(|native| &native.exact_flat_oracle),
        )?,
        rerank: aggregate_native_rerank_stage(profile.rerank_weight_ppm != 0, &native)?,
    })
}

pub(super) fn aggregate_native_stage<'a, T: 'a>(
    requested: bool,
    results: impl Iterator<Item = &'a SemanticNativeStageResultV1<T>>,
) -> Result<OptionalStageMeasurementV1, CandidateOutputError> {
    let results = results.collect::<Vec<_>>();
    if !requested {
        if results
            .iter()
            .any(|result| !matches!(result, SemanticNativeStageResultV1::NotRequested))
        {
            return Err(CandidateOutputError::Contract(
                "unrequested native stage reported execution".to_owned(),
            ));
        }
        return Ok(OptionalStageMeasurementV1::NotRequested);
    }
    if results
        .iter()
        .any(|result| matches!(result, SemanticNativeStageResultV1::NotRequested))
    {
        return Err(CandidateOutputError::Contract(
            "requested native stage reported not_requested".to_owned(),
        ));
    }
    Ok(
        if results
            .iter()
            .all(|result| matches!(result, SemanticNativeStageResultV1::Complete(_)))
        {
            OptionalStageMeasurementV1::Complete
        } else {
            OptionalStageMeasurementV1::Pending
        },
    )
}

fn aggregate_native_rerank_stage(
    requested: bool,
    native: &[&SemanticNativeQueryOutputV1],
) -> Result<OptionalStageMeasurementV1, CandidateOutputError> {
    for output in native {
        let states_agree = match (&output.rerank.on, &output.rerank.execution) {
            (
                SemanticNativeStageResultV1::NotRequested,
                SemanticNativeStageResultV1::NotRequested,
            )
            | (
                SemanticNativeStageResultV1::Complete(_),
                SemanticNativeStageResultV1::Complete(_),
            ) => true,
            (
                SemanticNativeStageResultV1::Pending { reason: left },
                SemanticNativeStageResultV1::Pending { reason: right },
            ) => left == right,
            _ => false,
        };
        if !states_agree {
            return Err(CandidateOutputError::Contract(
                "rerank output and resource execution states disagree".to_owned(),
            ));
        }
    }
    aggregate_native_stage(requested, native.iter().map(|native| &native.rerank.on))
}

pub(super) fn apply_native_resource_evidence(
    output: &mut ProductionCandidateOutputV1,
    evidence: &SemanticNativeResourceEvidenceV1,
) -> Result<(), CandidateOutputError> {
    let expected_chunks = output
        .resources
        .iter()
        .map(|(scale, sample)| (scale.clone(), sample.eligible_chunks))
        .collect::<BTreeMap<_, _>>();
    let mut projected = BTreeMap::new();
    for (scale, stage) in &evidence.samples {
        let eligible_chunks = expected_chunks.get(scale).copied().ok_or_else(|| {
            CandidateOutputError::Contract(format!("unknown native resource scale {scale}"))
        })?;
        let sample = match stage {
            SemanticNativeStageResultV1::Complete(sample) => {
                let projected = sample.as_existing_evaluator_sample().ok_or_else(|| {
                    CandidateOutputError::Contract(format!(
                        "complete native resource sample {scale} is incomplete"
                    ))
                })?;
                if projected.eligible_chunks != eligible_chunks {
                    return Err(CandidateOutputError::Contract(format!(
                        "native resource sample {scale} has the wrong eligible chunk count"
                    )));
                }
                projected
            }
            SemanticNativeStageResultV1::Pending { reason } => ResourceSampleV1 {
                status: ResourceMeasurementStatusV1::Pending,
                eligible_chunks,
                peak_rss_bytes: None,
                latency_samples_us: Vec::new(),
                measured_queries: 0,
                pending_reason: Some(format!(
                    "native semantic resource measurement pending: {reason:?}"
                )),
            },
            SemanticNativeStageResultV1::NotRequested => {
                return Err(CandidateOutputError::Contract(format!(
                    "native resource sample {scale} cannot be not_requested"
                )));
            }
        };
        projected.insert(scale.clone(), sample);
    }
    output.resources = projected;
    output.native_resources = Some(evidence.clone());
    Ok(())
}
