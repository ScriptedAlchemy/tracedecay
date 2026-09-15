//! Production-bound exact/lexical/graph candidate-output generator.
//!
//! Builds one published code generation from checked-in sanitized corpus
//! fixtures, then runs the shared `CompositionKernel` over the real exact,
//! lexical, and graph production lanes.
//!
//! Outputs deterministic checked-in `train` / `validation` candidate records
//! plus current/10x resource samples, cancellation, offline, and fallback
//! digests. Labels are ordinary reviewable fixture data, never a production
//! authority.

use std::collections::btree_map::Entry;
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use serde::Serialize;

use tracedecay_code_index::chunks::{ExtractionAdmittedCodeSearchChunkV1, content_digest};
use tracedecay_code_index::graph_projection::CodeGraphEvidenceReader;
use tracedecay_code_index::languages::{LanguageRegistry, StaticLanguageRegistry};
use tracedecay_code_index::production::{
    CodeIndexAtomicPublicationPort, CodeIndexBuildRequestV1, CodeIndexCapturedFileV1,
    CodeIndexGenerationScopeV1, CodeIndexProductionConfigV1, CodeIndexProductionOwnerV1,
    CodeIndexPublicationStoreErrorV1, CodeIndexPublishedGenerationV1,
    CodeIndexRepositoryParseIdentityV1, DAEMON_CODE_INDEX_CHUNKER_REVISION,
    VerifiedSealedLexicalSymbolDisplayV1,
};
use tracedecay_code_index::projection::{
    ChunkProjectionDecisionV1, CodeChunkProjectionSink, ProjectionReceiptBuilderV1,
    ProjectionSinkErrorV1, ProjectionSinkReceiptV1,
};
use tracedecay_contracts::ResolvedScope;
use tracedecay_contracts::historical_query::{
    HistoricalGitQueryAdapter, HistoricalGitReadOutcomeV1, HistoricalGitReadUnavailableReasonV1,
    HistoricalQueryRequestV1, HistoricalRenameModeV1, HistoricalSourceAuthorizationV1,
};
use tracedecay_domain::git::GitOidV1;
use tracedecay_domain::{
    ChunkerRevision, CodeGenerationId, CodeSearchChunkV1, ComponentRevision,
    EphemeralSanitizedQueryViewV1, ExactAdmissionRuleRevision, ExactClass, FileOccurrenceId,
    LanguageId, ManifestDigest, PolicyRevisionId, PrincipalId, PrivacyDomainId, ProjectId,
    ProjectionBatchRequestV1, ProjectionKeyV1, ProjectionKindV1, ProjectionOperationV1,
    ProjectionOutcomeV1, PublicRetrieverStatus, QueryFallbackSubpayload,
    QueryNormalizationRevision, RelationEdgeKindV1, RepositoryDirtyStateV1, RepositoryId,
    RetrievalFailure, RetrievalRequest, RetrievalScope, RetrievalSnapshot, RetrieverKind,
    RetrieverOutcome, SanitizationReceiptId, SanitizedCodeFileV1, SanitizedCodeSnapshotV1,
    SanitizerRevision, SingleRootScopeV1, SnapshotFileDispositionV1, SymbolOccurrenceId,
    TemporalModeV1, UtcMicros, VectorWatermark,
};
use tracedecay_query::native_git::NativeHistoricalBlobReaderV1;
use tracedecay_query::retrieval::exact::{
    CentralExactAdmissionAuthorityV1, ExactAdmissionAuthority, ExactLane, ExactLaneRequest,
    ExactLaneRetriever,
};
use tracedecay_query::retrieval::fusion::{
    CompositionKernel, CompositionLaneInput, CompositionOutputV1, FusionStageInput,
};
use tracedecay_query::retrieval::graph::{
    GraphLane, GraphLaneRequest, GraphLaneRetriever, production_code_index_freshness,
};
use tracedecay_query::retrieval::lexical::{
    CodeLexicalProjectionAdapterV1, CodeLexicalProjectionMetadataV1, LexicalLane,
    LexicalLaneRequest, LexicalLaneRetriever, LexicalRouteOutcomeV1, LexicalRoutePlanV1,
    LexicalRoutingV1, lexical_query_parts, merge_lexical_routes,
};
use tracedecay_query::retrieval::ports::CodeCandidateBindingV1;
use tracedecay_query::search_quality::candidate_output::{
    CandidateOutputError, CandidateWorkloadV1, CorpusDocumentV1, EVALUATION_CACHE_STATE,
    EVALUATION_SEED, GenerateCandidateOutputsResultV1, HistoricalQueryExecutionV1,
    PRODUCTION_BOUNDARY, ProductionCandidateOutputV1, ProfileSpecV1, QueryCandidateRowV1,
    REQUIRED_CANCELLATION, REQUIRED_OFFLINE, RankedCandidateRowV1, ResourceSampleV1,
    WORKLOAD_RELATIVE, WorkloadQueryV1, canonical_json_bytes, canonical_sha256,
    compute_corpus_digest, compute_profile_material_digest, compute_workload_digest,
    evaluated_diversity_policy, fusion_profile, load_candidate_workload, retrieval_budget,
    typed_id as id, validate_workload_for_tuning,
};

mod control;
use control::ActiveControl;

mod peak_rss;
use peak_rss::{completed_resource_sample, peak_rss_bytes};

mod cancellation;
use cancellation::prove_cancellation;

mod environment;

use environment::{hardware_fingerprint, toolchain_fingerprint};

#[derive(Clone, Debug)]
pub struct GenerateCandidateOutputsOptions<'a> {
    pub repo_root: &'a Path,
    pub workload_path: Option<&'a Path>,
    pub profile_ids: Option<&'a [String]>,
    /// Authoritative identity for `repo_root`, injected by the composing
    /// binary. See [`AdmittedCorpusScopeFn`].
    pub admitted_scope: AdmittedCorpusScopeFn,
}

#[derive(Clone, Default)]
struct SharedPublicationStore {
    active: Arc<Mutex<BTreeMap<CodeIndexGenerationScopeV1, Arc<CodeIndexPublishedGenerationV1>>>>,
}

impl CodeIndexAtomicPublicationPort for SharedPublicationStore {
    fn load_active(
        &self,
        scope: &CodeIndexGenerationScopeV1,
    ) -> Result<Option<Arc<CodeIndexPublishedGenerationV1>>, CodeIndexPublicationStoreErrorV1> {
        let active = self.active.lock().map_err(|_| {
            CodeIndexPublicationStoreErrorV1::Unavailable(
                "candidate-output publication lock is poisoned".to_owned(),
            )
        })?;
        Ok(active.get(scope).map(Arc::clone))
    }

    fn publish_atomically(
        &mut self,
        scope: &CodeIndexGenerationScopeV1,
        expected_active_generation: Option<&CodeGenerationId>,
        generation: Arc<CodeIndexPublishedGenerationV1>,
    ) -> Result<(), CodeIndexPublicationStoreErrorV1> {
        let mut active = self.active.lock().map_err(|_| {
            CodeIndexPublicationStoreErrorV1::Unavailable(
                "candidate-output publication lock is poisoned".to_owned(),
            )
        })?;
        if active
            .get(scope)
            .map(|current| current.manifest().generation_id.clone())
            .as_ref()
            != expected_active_generation
        {
            return Err(CodeIndexPublicationStoreErrorV1::CompareAndSwap);
        }
        active.insert(scope.clone(), generation);
        Ok(())
    }
}

#[derive(Default)]
struct ApplyingProjectionSink;

impl CodeChunkProjectionSink for ApplyingProjectionSink {
    fn project_changed_chunks(
        &mut self,
        request: &ProjectionBatchRequestV1,
        receipt_builder: ProjectionReceiptBuilderV1<'_>,
    ) -> Result<ProjectionSinkReceiptV1, ProjectionSinkErrorV1> {
        let mut decisions: Vec<ChunkProjectionDecisionV1> = request
            .changes
            .added_or_changed
            .iter()
            .map(|change| ChunkProjectionDecisionV1 {
                chunk_id: change.chunk_id.clone(),
                prior_chunk_digest: change.prior_digest.clone(),
                current_chunk_digest: change.current_digest.clone(),
                operation: if change.prior_digest.is_some() {
                    ProjectionOperationV1::Updated
                } else {
                    ProjectionOperationV1::Added
                },
                outcome: ProjectionOutcomeV1::Applied,
                output_digest: change.current_digest.clone(),
            })
            .collect();
        decisions.extend(
            request
                .changes
                .deleted
                .iter()
                .map(|change| ChunkProjectionDecisionV1 {
                    chunk_id: change.chunk_id.clone(),
                    prior_chunk_digest: change.prior_digest.clone(),
                    current_chunk_digest: None,
                    operation: ProjectionOperationV1::Deleted,
                    outcome: ProjectionOutcomeV1::Applied,
                    output_digest: None,
                }),
        );
        decisions.extend(
            request
                .changes
                .reused
                .iter()
                .map(|change| ChunkProjectionDecisionV1 {
                    chunk_id: change.chunk_id.clone(),
                    prior_chunk_digest: change.prior_digest.clone(),
                    current_chunk_digest: change.current_digest.clone(),
                    operation: ProjectionOperationV1::Reused,
                    outcome: ProjectionOutcomeV1::Reused,
                    output_digest: None,
                }),
        );
        decisions.sort_by(|left, right| left.chunk_id.cmp(&right.chunk_id));
        receipt_builder
            .build(&decisions)
            .map_err(|error| ProjectionSinkErrorV1::Rejected(error.to_string()))
    }
}

#[derive(Clone)]
struct OccurrenceMapEntry {
    document_id: String,
    scope: String,
    display_anchors: Vec<String>,
}

/// Retrieval adapters keyed by canonical allowed-scope key (sorted, deduped),
/// as produced by [`canonical_scope_key`].
type ScopedLexicalProjections = BTreeMap<Vec<String>, CodeLexicalProjectionAdapterV1>;
type ScopedGraphEvidence = BTreeMap<Vec<String>, CodeGraphEvidenceReader>;

struct PublishedCorpus {
    generation: Arc<CodeIndexPublishedGenerationV1>,
    lexical_projections: ScopedLexicalProjections,
    graph_projections: ScopedGraphEvidence,
    occurrence_map: BTreeMap<String, OccurrenceMapEntry>,
    repo_root: PathBuf,
    source_commit: GitOidV1,
    corpus: Vec<CorpusDocumentV1>,
    corpus_digest: String,
    eligible_chunks: u64,
    admitted_scope: AdmittedCorpusScopeFn,
}

/// Root-injected authoritative identity for the checkout under evaluation.
///
/// Resolving a checkout's project/repository/worktree identity reads the
/// repository identity marker and the provenance admission context, both owned
/// by the composing binary. The evaluator owns everything downstream of the
/// scope: corpus binding, source authorization, and evidence validation.
/// Returning `None` means the checkout carries no authoritative identity, which
/// the historical lane reports as a contract failure rather than guessing one.
pub type AdmittedCorpusScopeFn = fn(&Path) -> Option<ResolvedScope>;

/// Refuses every checkout. Used where historical evidence is out of scope.
pub fn no_admitted_corpus_scope(_repo_root: &Path) -> Option<ResolvedScope> {
    None
}

fn canonical_scope_key(scopes: &[String]) -> Vec<String> {
    let mut key = scopes.to_vec();
    key.sort();
    key.dedup();
    key
}

/// Build every scoped retrieval projection the workload's queries need.
///
/// Preparation is measured on its own span so query evaluation timing can
/// neither absorb nor hide it. Cost is O(chunks + scope memberships): the
/// corpus is classified once through a reverse scope map rather than once per
/// distinct scope set.
#[hotpath::measure(label = "search_eval.corpus.query_projections")]
fn build_query_projections(
    generation: &CodeIndexPublishedGenerationV1,
    file_scopes: &BTreeMap<String, String>,
    symbol_displays: Arc<BTreeMap<SymbolOccurrenceId, VerifiedSealedLexicalSymbolDisplayV1>>,
    queries: &[WorkloadQueryV1],
) -> Result<(ScopedLexicalProjections, ScopedGraphEvidence), CandidateOutputError> {
    let generation_id = generation.manifest().generation_id.clone();
    let freshness = production_code_index_freshness(
        generation.manifest().seal.sealed_at,
        id::<ComponentRevision>("policy.candidate.v1")?,
    )
    .map_err(|error| CandidateOutputError::Contract(error.to_string()))?;
    let metadata = Arc::new(CodeLexicalProjectionMetadataV1 {
        generation: generation_id.clone(),
        repository_id: Some(generation.snapshot().repository.clone()),
        logical_paths: generation
            .snapshot()
            .files
            .iter()
            .map(|file| (file.file_occurrence_id.clone(), file.logical_path.clone()))
            .collect(),
        freshness: freshness.clone(),
        exact_retriever_revision: id(
            tracedecay_query::retrieval::QUERY_EXACT_RETRIEVER_REVISION_V1,
        )?,
        lexical_retriever_revision: id(
            tracedecay_query::retrieval::QUERY_LEXICAL_RETRIEVER_REVISION_V1,
        )?,
        exact_score_domain: id(tracedecay_query::retrieval::QUERY_EXACT_SCORE_DOMAIN_V1)?,
    });
    // Canonical scope keys, deduplicated once; the position is the bucket id.
    let scope_keys = queries
        .iter()
        .map(|query| canonical_scope_key(&query.allowed_scopes))
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    // Reverse map from a file scope to every scope key admitting it, so one
    // corpus pass places each chunk in all of its buckets instead of scanning
    // the corpus once per distinct scope set.
    let mut interested_buckets: BTreeMap<&str, Vec<usize>> = BTreeMap::new();
    for (bucket, scope_key) in scope_keys.iter().enumerate() {
        for scope in scope_key {
            interested_buckets
                .entry(scope.as_str())
                .or_default()
                .push(bucket);
        }
    }
    let buckets_for = |file_occurrence_id: &str| {
        file_scopes
            .get(file_occurrence_id)
            .and_then(|scope| interested_buckets.get(scope.as_str()))
            .into_iter()
            .flatten()
            .copied()
    };
    // Corpus order within each bucket is the order the admitted sweep and the
    // chunk manifest already carry, exactly what a per-scope filter yielded.
    let admitted = generation
        .admitted_chunks()
        .map_err(|error| CandidateOutputError::Contract(error.to_string()))?;
    let mut lexical_chunks: Vec<Vec<ExtractionAdmittedCodeSearchChunkV1>> =
        vec![Vec::new(); scope_keys.len()];
    for chunk in admitted.iter() {
        for bucket in buckets_for(chunk.chunk().anchor.file_occurrence_id.as_str()) {
            lexical_chunks[bucket].push(chunk.clone());
        }
    }
    drop(admitted);
    let mut graph_chunks: Vec<Vec<Arc<CodeSearchChunkV1>>> = vec![Vec::new(); scope_keys.len()];
    for chunk in generation.chunks().chunks() {
        for bucket in buckets_for(chunk.anchor.file_occurrence_id.as_str()) {
            graph_chunks[bucket].push(Arc::clone(chunk));
        }
    }
    let mut lexical = BTreeMap::new();
    let mut graph = BTreeMap::new();
    for ((scope_key, chunks), graph_chunks) in
        scope_keys.into_iter().zip(lexical_chunks).zip(graph_chunks)
    {
        lexical.insert(
            scope_key.clone(),
            CodeLexicalProjectionAdapterV1::new_admitted(
                Arc::clone(&metadata),
                chunks,
                Arc::clone(&symbol_displays),
            )
            .map_err(|error| CandidateOutputError::Contract(error.to_string()))?,
        );
        graph.insert(
            scope_key,
            CodeGraphEvidenceReader::new_for_evaluation(
                generation_id.clone(),
                Some(generation.snapshot().repository.clone()),
                freshness.clone(),
                generation.edges(),
                &graph_chunks,
            )
            .map_err(|error| CandidateOutputError::Contract(error.to_string()))?,
        );
    }
    Ok((lexical, graph))
}

/// The corpora published for one candidate-generation call, memoized by scale.
///
/// `publish_corpus_with_scale` is a pure function of its four arguments: every
/// identity it mints is content-derived (`content_identity` hashes the corpus
/// digest and the copy count), both of its timestamps are constants, and it
/// sorts its files before building. Two calls with equal arguments therefore
/// produce byte-identical generations down to the generation id, so the 1x
/// and 10x corpora are each published once per evaluation.
#[derive(Default)]
struct PublishedCorpusCache {
    by_scale: BTreeMap<usize, PublishedCorpus>,
}

impl PublishedCorpusCache {
    /// Publish the corpus at `copies` scale unless it is already cached.
    fn ensure(
        &mut self,
        repo_root: &Path,
        workload: &CandidateWorkloadV1,
        copies: usize,
        admitted_scope: AdmittedCorpusScopeFn,
    ) -> Result<(), CandidateOutputError> {
        if let Entry::Vacant(entry) = self.by_scale.entry(copies) {
            let published = match copies {
                1 => hotpath::measure_block!(
                    "search_eval.corpus.publish.current",
                    publish_corpus_with_scale(repo_root, workload, copies, admitted_scope)
                ),
                10 => hotpath::measure_block!(
                    "search_eval.corpus.publish.10x",
                    publish_corpus_with_scale(repo_root, workload, copies, admitted_scope)
                ),
                _ => publish_corpus_with_scale(repo_root, workload, copies, admitted_scope),
            }?;
            entry.insert(published);
        }
        Ok(())
    }

    /// Borrow a corpus a prior `ensure` published. Kept separate from `ensure`
    /// so two scales can be borrowed at once.
    fn get(&self, copies: usize) -> Result<&PublishedCorpus, CandidateOutputError> {
        self.by_scale.get(&copies).ok_or_else(|| {
            CandidateOutputError::Contract(format!(
                "{copies}x evaluation corpus was not published before use"
            ))
        })
    }
}

/// Generate deterministic train/validation outputs using the production
/// retrieval kernel.
pub fn generate_candidate_outputs(
    options: &GenerateCandidateOutputsOptions<'_>,
) -> Result<GenerateCandidateOutputsResultV1, CandidateOutputError> {
    hotpath::measure_block!("search_eval.generate_candidates", {
        generate_candidate_outputs_sharing_corpora(options, &mut PublishedCorpusCache::default())
    })
}

fn generate_candidate_outputs_sharing_corpora(
    options: &GenerateCandidateOutputsOptions<'_>,
    corpora: &mut PublishedCorpusCache,
) -> Result<GenerateCandidateOutputsResultV1, CandidateOutputError> {
    let workload_path = options.workload_path.map_or_else(
        || options.repo_root.join(WORKLOAD_RELATIVE),
        Path::to_path_buf,
    );
    let workload = load_candidate_workload(&workload_path)?;
    let workload_digest = compute_workload_digest(&workload)?;
    let profiles: Vec<&ProfileSpecV1> = match options.profile_ids {
        Some(ids) => {
            let known: BTreeSet<_> = workload
                .profile_matrix
                .iter()
                .map(|profile| profile.profile_id.as_str())
                .collect();
            let mut requested = BTreeSet::new();
            for id in ids {
                if !requested.insert(id.as_str()) {
                    return Err(CandidateOutputError::Contract(format!(
                        "duplicate requested profile_id {id}"
                    )));
                }
                if !known.contains(id.as_str()) {
                    return Err(CandidateOutputError::Contract(format!(
                        "unknown requested profile_id {id}"
                    )));
                }
            }
            workload
                .profile_matrix
                .iter()
                .filter(|profile| requested.contains(profile.profile_id.as_str()))
                .collect()
        }
        None => workload.profile_matrix.iter().collect(),
    };
    if profiles.is_empty() {
        return Err(CandidateOutputError::Contract(
            "no profiles selected for candidate generation".to_owned(),
        ));
    }
    corpora.ensure(options.repo_root, &workload, 1, options.admitted_scope)?;

    let mut outputs = Vec::new();
    {
        let published = corpora.get(1)?;
        for &profile in &profiles {
            for partition in ["train", "validation"] {
                let output = generate_partition_output(
                    &workload,
                    &workload_digest,
                    published,
                    profile,
                    partition,
                )?;
                outputs.push(output);
            }
        }
    }
    corpora.ensure(options.repo_root, &workload, 10, options.admitted_scope)?;
    let published = corpora.get(1)?;
    let ten_x_published = corpora.get(10)?;
    let expected_ten_x_chunks = published.eligible_chunks.checked_mul(10).ok_or_else(|| {
        CandidateOutputError::Contract("current eligible chunk count overflows 10x".to_owned())
    })?;
    if ten_x_published.eligible_chunks != expected_ten_x_chunks {
        return Err(CandidateOutputError::Contract(format!(
            "10x corpus produced {} eligible chunks; expected exactly {}",
            ten_x_published.eligible_chunks, expected_ten_x_chunks
        )));
    }
    for output in &mut outputs {
        let profile = profiles
            .iter()
            .copied()
            .find(|profile| profile.profile_id == output.profile_id)
            .ok_or_else(|| {
                CandidateOutputError::Contract(format!(
                    "missing selected profile {}",
                    output.profile_id
                ))
            })?;
        let queries: Vec<_> = workload
            .queries
            .iter()
            .filter(|query| query.partition == output.partition)
            .collect();
        output.resources.insert(
            "10x".to_owned(),
            measure_partition_resources(ten_x_published, profile, &queries)?,
        );
    }

    // Prove cancellation against the production code-index control path once.
    prove_cancellation(options.repo_root, &workload)?;

    Ok(GenerateCandidateOutputsResultV1 {
        workload_digest,
        outputs,
    })
}

/// Direct production call for one query/profile — used by tests to prove the
/// generator emits identical candidate bytes.
pub fn retrieve_partition_query_bytes(
    repo_root: &Path,
    workload: &CandidateWorkloadV1,
    profile_id: &str,
    query_id: &str,
    admitted_scope: AdmittedCorpusScopeFn,
) -> Result<Vec<u8>, CandidateOutputError> {
    validate_workload_for_tuning(workload)?;
    let profile = workload
        .profile_matrix
        .iter()
        .find(|profile| profile.profile_id == profile_id)
        .ok_or_else(|| CandidateOutputError::Contract(format!("unknown profile {profile_id}")))?;
    let query = workload
        .queries
        .iter()
        .find(|query| query.query_id == query_id)
        .ok_or_else(|| CandidateOutputError::Contract(format!("unknown query {query_id}")))?;
    let published = publish_corpus(repo_root, workload, admitted_scope)?;
    let row = retrieve_one_query(&published, profile, query)?;
    canonical_json_bytes(&row)
}

pub fn write_generate_outputs(
    output_root: &Path,
    result: &GenerateCandidateOutputsResultV1,
) -> Result<(), CandidateOutputError> {
    fs::create_dir_all(output_root).map_err(|source| CandidateOutputError::Write {
        path: output_root.to_path_buf(),
        source,
    })?;
    let jsonl_path = output_root.join("train-validation-candidate-outputs.jsonl");
    let mut jsonl = String::new();
    for output in &result.outputs {
        jsonl.push_str(&serde_json::to_string(output).map_err(|error| {
            CandidateOutputError::Contract(format!("serialize candidate output: {error}"))
        })?);
        jsonl.push('\n');
    }
    fs::write(&jsonl_path, jsonl).map_err(|source| CandidateOutputError::Write {
        path: jsonl_path,
        source,
    })?;
    let summary_path = output_root.join("generate-summary.json");
    write_pretty_json(
        &summary_path,
        &serde_json::json!({
            "workload_digest": result.workload_digest,
            "outputs": result.outputs.len(),
            "production_boundary": PRODUCTION_BOUNDARY,
        }),
    )?;
    Ok(())
}

fn generate_partition_output(
    workload: &CandidateWorkloadV1,
    workload_digest: &str,
    published: &PublishedCorpus,
    profile: &ProfileSpecV1,
    partition: &str,
) -> Result<ProductionCandidateOutputV1, CandidateOutputError> {
    let queries: Vec<&WorkloadQueryV1> = workload
        .queries
        .iter()
        .filter(|query| query.partition == partition)
        .collect();
    if queries.is_empty() {
        return Err(CandidateOutputError::Contract(format!(
            "partition {partition} has no queries"
        )));
    }
    let mut rows = Vec::with_capacity(queries.len());
    let mut latencies_us = Vec::with_capacity(queries.len());
    let mut fallback_digests = Vec::with_capacity(queries.len());
    let peak_before = peak_rss_bytes();
    for query in &queries {
        let started = Instant::now();
        // The row and both partition fallback digests share one composition.
        let composed = compose_production_query(published, profile, query)?;
        let fallback = query_fallback_from_composition(&composed)?;
        fallback_digests.push((query.query_id.as_str(), fallback.digest.as_str().to_owned()));
        rows.push(query_row_from_composition(published, query, &composed)?);
        latencies_us.push(started.elapsed().as_micros() as u64);
    }
    let peak_after = peak_rss_bytes().max(peak_before);
    let current = completed_resource_sample(
        published.eligible_chunks,
        peak_after,
        latencies_us,
        rows.len() as u64,
    );

    let fallback_digest = canonical_sha256(&(
        "tracedecay.search-eval.partition-fallbacks.v1",
        &fallback_digests,
    ))?;
    let query_digest = fallback_digest.clone();
    let expected_query_fallback_digest = workload
        .expected_query_fallback_digests
        .get(partition)
        .cloned()
        .ok_or_else(|| {
            CandidateOutputError::Contract(format!(
                "missing expected query fallback digest for {partition}"
            ))
        })?;
    let query_fallback_matches_expected = query_digest == expected_query_fallback_digest;

    let mut resources = BTreeMap::new();
    resources.insert("current".to_owned(), current);

    Ok(ProductionCandidateOutputV1 {
        schema_version: 2,
        workload_digest: workload_digest.to_owned(),
        profile_id: profile.profile_id.clone(),
        partition: partition.to_owned(),
        production_boundary: PRODUCTION_BOUNDARY.to_owned(),
        fixture_source_commit: workload.source_repository_commit.clone(),
        fixture_source_tree: workload.source_repository_tree.clone(),
        corpus_digest: published.corpus_digest.clone(),
        seed: EVALUATION_SEED.to_owned(),
        cache_state: EVALUATION_CACHE_STATE.to_owned(),
        toolchain: toolchain_fingerprint(),
        hardware: hardware_fingerprint(),
        profile_material_digest: compute_profile_material_digest(profile)?,
        fallback_digest,
        query_fallback_digest: query_digest,
        expected_query_fallback_digest,
        query_fallback_matches_expected,
        cancellation: REQUIRED_CANCELLATION.to_owned(),
        offline: REQUIRED_OFFLINE.to_owned(),
        resources,
        queries: rows,
    })
}

fn measure_partition_resources(
    published: &PublishedCorpus,
    profile: &ProfileSpecV1,
    queries: &[&WorkloadQueryV1],
) -> Result<ResourceSampleV1, CandidateOutputError> {
    let peak_before = peak_rss_bytes();
    let mut latencies_us = Vec::with_capacity(queries.len());
    for query in queries {
        let started = Instant::now();
        retrieve_one_query(published, profile, query)?;
        latencies_us.push(started.elapsed().as_micros() as u64);
    }
    Ok(completed_resource_sample(
        published.eligible_chunks,
        peak_rss_bytes().max(peak_before),
        latencies_us,
        queries.len() as u64,
    ))
}

fn retrieve_one_query(
    published: &PublishedCorpus,
    profile: &ProfileSpecV1,
    query: &WorkloadQueryV1,
) -> Result<QueryCandidateRowV1, CandidateOutputError> {
    let composed = compose_production_query(published, profile, query)?;
    query_row_from_composition(published, query, &composed)
}

fn query_row_from_composition(
    published: &PublishedCorpus,
    query: &WorkloadQueryV1,
    composed: &CompositionOutputV1,
) -> Result<QueryCandidateRowV1, CandidateOutputError> {
    let ranked = map_ranked_candidates(published, composed)?;
    let (historical, historical_ranked) = historical_candidates(published, query)?;
    let ranked = merge_candidate_timelines(query, ranked, historical_ranked);
    let abstained = ranked.is_empty();
    Ok(QueryCandidateRowV1 {
        query_id: query.query_id.clone(),
        ranked,
        abstained,
        historical,
    })
}

/// Run one query through the real exact, lexical, and graph lanes and compose
/// them under the profile's fusion material.
fn compose_production_query(
    published: &PublishedCorpus,
    profile: &ProfileSpecV1,
    query: &WorkloadQueryV1,
) -> Result<CompositionOutputV1, CandidateOutputError> {
    let generation_id = published.generation.manifest().generation_id.clone();
    let request = retrieval_request(&profile.profile_id, published)?;
    let query_view = EphemeralSanitizedQueryViewV1::sanitize(
        &query.query,
        id::<SanitizerRevision>(tracedecay_query::retrieval::QUERY_SANITIZER_REVISION_V1)?,
        id::<QueryNormalizationRevision>(
            tracedecay_query::retrieval::QUERY_NORMALIZATION_REVISION_V1,
        )?,
    )
    .map_err(|error| CandidateOutputError::Contract(error.to_string()))?;

    let scope_key = canonical_scope_key(&query.allowed_scopes);
    let lexical_projection = published
        .lexical_projections
        .get(&scope_key)
        .cloned()
        .ok_or_else(|| {
            CandidateOutputError::Contract(format!(
                "missing lexical projection for query {}",
                query.query_id
            ))
        })?;
    let authority = CentralExactAdmissionAuthorityV1::new(id::<ExactAdmissionRuleRevision>(
        tracedecay_query::retrieval::QUERY_EXACT_RULE_REVISION_V1,
    )?);
    let exact_lane = ExactLane::new(
        authority.clone(),
        lexical_projection.exact_adapter(authority.clone()),
    );
    let lexical_lane = LexicalLane::new(lexical_projection);
    let graph_lane = GraphLane::new(
        published
            .graph_projections
            .get(&scope_key)
            .cloned()
            .ok_or_else(|| {
                CandidateOutputError::Contract(format!(
                    "missing graph projection for query {}",
                    query.query_id
                ))
            })?,
    );

    let budget = retrieval_budget();
    let exact_request = ExactLaneRequest {
        base: request.clone(),
        query_view: &query_view,
        generation: generation_id.clone(),
        literals: authority.parse_literals(&query_view, &request),
        budget,
    };
    let exact_outcome = exact_lane
        .retrieve_exact(&exact_request)
        .map_err(|error| CandidateOutputError::Contract(error.to_string()))?;

    let lexical_routing = LexicalRoutingV1::default()
        .with_aliases(query.lexical_aliases.clone())
        .map_err(|error| CandidateOutputError::Contract(error.to_string()))?;
    let route_plan = LexicalRoutePlanV1::plan(query_view.as_str(), &lexical_routing)
        .map_err(|error| CandidateOutputError::Contract(error.to_string()))?;
    let mut route_outcomes = Vec::with_capacity(route_plan.routes().len());
    for route in route_plan.routes() {
        let lexical_outcome = lexical_lane
            .retrieve_lexical(&LexicalLaneRequest {
                base: request.clone(),
                query_view: &query_view,
                generation: generation_id.clone(),
                whole_terms: route.parts.whole_terms.clone(),
                subtokens: route.parts.subtokens.clone(),
                phrases: route.parts.phrases.clone(),
                proximities: route.proximities.clone(),
                field_filters: route.field_filters.clone(),
                fuzzy_budget: 8,
                lexical_profile_revision: id(
                    tracedecay_query::retrieval::QUERY_LEXICAL_PROFILE_REVISION_V1,
                )?,
                score_domain: id(tracedecay_query::retrieval::QUERY_LEXICAL_SCORE_DOMAIN_V1)?,
                budget,
                control: &ActiveControl,
            })
            .map_err(|error| CandidateOutputError::Contract(error.to_string()))?;
        route_outcomes.push(LexicalRouteOutcomeV1 {
            kind: route.kind.clone(),
            outcome: lexical_outcome,
        });
    }
    let (lexical_outcome, _) =
        merge_lexical_routes(&generation_id, &budget, &request.budget, route_outcomes)
            .map_err(|error| CandidateOutputError::Contract(error.to_string()))?;

    let seed_anchors = graph_seeds_from_outcomes(&exact_outcome, &lexical_outcome);
    let graph_outcome = if seed_anchors.is_empty() {
        RetrieverOutcome::Unavailable(RetrievalFailure::AuthorityUnavailable {
            detail: "no graph seeds from exact/lexical".to_owned(),
        })
    } else {
        let graph_request = GraphLaneRequest {
            base: request.clone(),
            generation: generation_id.clone(),
            seed_anchors,
            edge_kinds: vec![
                RelationEdgeKindV1::Calls,
                RelationEdgeKindV1::Uses,
                RelationEdgeKindV1::Contains,
            ],
            max_depth: 2,
            budget,
        };
        graph_lane
            .retrieve_graph(&graph_request, Arc::new(ActiveControl))
            .map_err(|error| CandidateOutputError::Contract(error.to_string()))?
    };

    let kernel = CompositionKernel::new(id::<ComponentRevision>(
        tracedecay_query::retrieval::QUERY_RANKING_REVISION_V1,
    )?);
    let lanes = vec![
        CompositionLaneInput::new(RetrieverKind::ExactLiteral, exact_outcome)
            .map_err(|error| CandidateOutputError::Contract(error.to_string()))?,
        CompositionLaneInput::new(RetrieverKind::Lexical, lexical_outcome)
            .map_err(|error| CandidateOutputError::Contract(error.to_string()))?,
        CompositionLaneInput::new(RetrieverKind::Graph, graph_outcome)
            .map_err(|error| CandidateOutputError::Contract(error.to_string()))?,
    ];
    kernel
        .compose(
            &FusionStageInput {
                profile: fusion_profile(profile)?,
                lanes,
            },
            &evaluated_diversity_policy()?,
        )
        .map_err(|error| CandidateOutputError::Contract(error.to_string()))
}

fn query_fallback_from_composition(
    output: &CompositionOutputV1,
) -> Result<QueryFallbackSubpayload, CandidateOutputError> {
    let mut coverage = BTreeMap::new();
    for lane in RetrieverKind::QUERY_FALLBACK_LANES {
        coverage.insert(
            lane,
            output
                .public_lane_statuses
                .get(&lane)
                .copied()
                .unwrap_or(PublicRetrieverStatus::Unavailable),
        );
    }
    let fallback = QueryFallbackSubpayload::new(
        output.profile_id.clone(),
        output.ranked_candidates.clone(),
        coverage,
        output.freshness.clone(),
        None,
    )
    .map_err(|error| CandidateOutputError::Contract(error.to_string()))?;
    fallback
        .validate()
        .map_err(|error| CandidateOutputError::Contract(error.to_string()))?;
    Ok(fallback)
}

fn map_ranked_candidates(
    published: &PublishedCorpus,
    output: &CompositionOutputV1,
) -> Result<Vec<RankedCandidateRowV1>, CandidateOutputError> {
    map_ranked_candidate_list(published, &output.ranked_candidates)
}

fn map_ranked_candidate_list(
    published: &PublishedCorpus,
    ranked_candidates: &[tracedecay_domain::RankedCandidate],
) -> Result<Vec<RankedCandidateRowV1>, CandidateOutputError> {
    let mut rows = Vec::new();
    for ranked in ranked_candidates {
        let entry = published
            .occurrence_map
            .get(ranked.candidate.anchor_id.as_str())
            .or_else(|| {
                ranked.candidate.occurrences.iter().find_map(|occurrence| {
                    published
                        .occurrence_map
                        .get(occurrence.source_occurrence_id.as_str())
                })
            })
            .cloned()
            .ok_or_else(|| {
                CandidateOutputError::Contract(format!(
                    "ranked candidate {} has no corpus occurrence binding",
                    ranked.candidate.anchor_id
                ))
            })?;
        let tier = if ranked.candidate.exact_class != ExactClass::Approximate {
            "exact"
        } else {
            "approximate"
        };
        let anchor = ranked.candidate.anchor_id.as_str().to_owned();
        let mut anchors = entry.display_anchors;
        if !anchors.contains(&anchor) {
            anchors.insert(0, anchor.clone());
        }
        rows.push(RankedCandidateRowV1 {
            anchor,
            anchors,
            scope: entry.scope,
            document_id: entry.document_id,
            tier: tier.to_owned(),
        });
    }
    Ok(rows)
}

/// Mount the historical code-index join on one already-admitted checkout.
///
/// The scope arrives from the composing binary's identity authority; this
/// function only refuses provider drift and projects adapter errors onto the
/// typed unavailable reasons.
fn read_historical_evidence(
    repo_root: &Path,
    scope: &ResolvedScope,
    authorization: Option<&HistoricalSourceAuthorizationV1>,
    request: &HistoricalQueryRequestV1,
) -> HistoricalGitReadOutcomeV1 {
    let reader = NativeHistoricalBlobReaderV1::new(
        repo_root,
        scope.repository_id.clone(),
        scope.worktree_id.clone(),
    );
    match HistoricalGitQueryAdapter::new(&reader, scope.clone()).query(authorization, request) {
        Ok(result) => HistoricalGitReadOutcomeV1::Complete {
            scope: scope.clone(),
            result,
        },
        Err(error) => HistoricalGitReadOutcomeV1::Unavailable {
            reason: HistoricalGitReadUnavailableReasonV1::from_query_error(&error),
        },
    }
}

fn historical_candidates(
    published: &PublishedCorpus,
    query: &WorkloadQueryV1,
) -> Result<(HistoricalQueryExecutionV1, Vec<RankedCandidateRowV1>), CandidateOutputError> {
    if !query.strata.iter().any(|stratum| {
        matches!(
            stratum.as_str(),
            "incremental_edit"
                | "incremental_delete"
                | "incremental_rename"
                | "renamed_moved_symbol"
        )
    }) {
        return Ok((HistoricalQueryExecutionV1::NotRequested, Vec::new()));
    }

    let scope = (published.admitted_scope)(&published.repo_root).ok_or_else(|| {
        CandidateOutputError::Contract(
            "historical evaluator requires the authoritative repository identity marker".to_owned(),
        )
    })?;
    let source_commit = query
        .historical_commit
        .as_deref()
        .map(GitOidV1::new)
        .transpose()
        .map_err(|error| CandidateOutputError::Contract(error.to_string()))?
        .unwrap_or_else(|| published.source_commit.clone());

    let paths: Vec<String> = published
        .corpus
        .iter()
        .filter(|document| query.allowed_scopes.contains(&document.scope))
        .map(|document| document.source_path.clone())
        .collect();
    let authorization = if paths.is_empty() {
        None
    } else {
        Some(
            HistoricalSourceAuthorizationV1::new(
                scope.clone(),
                [source_commit.clone()],
                paths.clone(),
            )
            .map_err(|error| CandidateOutputError::Contract(error.to_string()))?,
        )
    };
    let terms = lexical_query_parts(&query.query)
        .map_err(|error| CandidateOutputError::Contract(error.to_string()))?
        .whole_terms;
    let request = HistoricalQueryRequestV1 {
        commits: vec![source_commit],
        paths,
        terms,
        rename_mode: HistoricalRenameModeV1::FollowExactObjectRenames,
        max_results: 32,
        max_blob_bytes: 8 * 1024 * 1024,
        max_total_bytes: 32 * 1024 * 1024,
    };
    match read_historical_evidence(
        &published.repo_root,
        &scope,
        authorization.as_ref(),
        &request,
    ) {
        HistoricalGitReadOutcomeV1::Unavailable { reason } => {
            Ok((HistoricalQueryExecutionV1::Unavailable(reason), Vec::new()))
        }
        HistoricalGitReadOutcomeV1::Complete {
            scope: returned_scope,
            result,
        } => {
            if returned_scope != scope || result.scope != scope {
                return Err(CandidateOutputError::Contract(
                    "historical evaluator received cross-scope evidence".to_owned(),
                ));
            }
            let mut rows = Vec::new();
            for evidence in result.evidence {
                let document = published
                    .corpus
                    .iter()
                    .find(|document| document.source_path == evidence.path)
                    .ok_or_else(|| {
                        CandidateOutputError::Contract(format!(
                            "historical evidence path {} is outside the corpus",
                            evidence.path
                        ))
                    })?;
                if !query.allowed_scopes.contains(&document.scope) {
                    return Err(CandidateOutputError::Contract(format!(
                        "historical evidence path {} escaped allowed scopes",
                        evidence.path
                    )));
                }
                let anchors: Vec<_> = evidence
                    .anchors
                    .iter()
                    .flat_map(|anchor| {
                        [
                            format!(
                                "git:{}:{}::{}",
                                evidence.commit.as_str(),
                                evidence.path,
                                anchor.term
                            ),
                            format!("{}::{}", evidence.path, anchor.term),
                        ]
                    })
                    .collect();
                let Some(anchor) = anchors.first().cloned() else {
                    continue;
                };
                rows.push(RankedCandidateRowV1 {
                    anchor,
                    anchors,
                    scope: document.scope.clone(),
                    document_id: document.document_id.clone(),
                    tier: "historical_exact".to_owned(),
                });
            }
            let mut seen = BTreeSet::new();
            rows.retain(|row| seen.insert(row.anchor.clone()));
            Ok((HistoricalQueryExecutionV1::Complete, rows))
        }
    }
}

fn merge_candidate_timelines(
    query: &WorkloadQueryV1,
    mut current: Vec<RankedCandidateRowV1>,
    mut historical: Vec<RankedCandidateRowV1>,
) -> Vec<RankedCandidateRowV1> {
    if query.historical_commit.is_some() {
        historical.append(&mut current);
        historical
    } else {
        current.append(&mut historical);
        current
    }
}

fn publish_corpus(
    repo_root: &Path,
    workload: &CandidateWorkloadV1,
    admitted_scope: AdmittedCorpusScopeFn,
) -> Result<PublishedCorpus, CandidateOutputError> {
    publish_corpus_with_scale(repo_root, workload, 1, admitted_scope)
}

fn publish_corpus_with_scale(
    repo_root: &Path,
    workload: &CandidateWorkloadV1,
    copies: usize,
    admitted_scope: AdmittedCorpusScopeFn,
) -> Result<PublishedCorpus, CandidateOutputError> {
    if copies == 0 {
        return Err(CandidateOutputError::Contract(
            "corpus scale must be positive".to_owned(),
        ));
    }
    let corpus_digest = compute_corpus_digest(repo_root, workload)?;
    let language_registry = StaticLanguageRegistry::new();
    let mut files = Vec::new();
    let mut captured = Vec::new();
    let mut file_to_document = BTreeMap::new();
    let mut file_scopes = BTreeMap::new();
    for copy in 0..copies {
        for document in &workload.corpus {
            let absolute = repo_root.join(&document.path);
            let bytes = fs::read(&absolute).map_err(|source| CandidateOutputError::Read {
                path: absolute.clone(),
                source,
            })?;
            let copy_suffix = if copy == 0 {
                String::new()
            } else {
                format!(".resource-copy-{copy}")
            };
            let file_occurrence_id =
                id::<FileOccurrenceId>(&format!("file.{}{}", document.document_id, copy_suffix))?;
            file_to_document.insert(file_occurrence_id.as_str().to_owned(), document.clone());
            file_scopes.insert(
                file_occurrence_id.as_str().to_owned(),
                document.scope.clone(),
            );
            let language = id::<LanguageId>(&document.language)?;
            let indexable = language_registry.descriptor(&language).is_some();
            files.push(SanitizedCodeFileV1 {
                file_occurrence_id: file_occurrence_id.clone(),
                logical_path: format!("{}{}", document.source_path, copy_suffix),
                language: Some(language),
                content_digest: content_digest(&bytes),
                disposition: if indexable {
                    SnapshotFileDispositionV1::Present
                } else {
                    SnapshotFileDispositionV1::UnsupportedLanguage
                },
            });
            if indexable {
                captured.push(CodeIndexCapturedFileV1 {
                    file_occurrence_id,
                    sanitized_bytes: Arc::from(bytes),
                    sensitivity_level: tracedecay_domain::SensitivityLevelV1::Public,
                });
            }
        }
    }
    files.sort_by(|left, right| {
        (&left.logical_path, &left.file_occurrence_id)
            .cmp(&(&right.logical_path, &right.file_occurrence_id))
    });
    captured.sort_by(|left, right| left.file_occurrence_id.cmp(&right.file_occurrence_id));
    let snapshot = SanitizedCodeSnapshotV1 {
        repository: id::<RepositoryId>("repository.candidate.fixture")?,
        worktree: None,
        reference: None,
        source_revision: None,
        sanitizer_revision: id::<SanitizerRevision>("sanitizer.candidate.v1")?,
        sanitization_receipts: vec![id::<SanitizationReceiptId>("receipt.candidate.v1")?],
        content_identity: id(&canonical_sha256(&(
            "tracedecay.search-eval.scaled-corpus.v1",
            &corpus_digest,
            copies,
        ))?)?,
        captured_at: UtcMicros(1_000_000),
        files,
    };
    let target_projection_key = ProjectionKeyV1 {
        kind: ProjectionKindV1::Lexical,
        schema_revision: "lexical.candidate.v1".to_owned(),
        profile_digest: lexical_projection_profile_digest()?,
    };
    let request = CodeIndexBuildRequestV1 {
        snapshot,
        captured_files: captured,
        changed_files: BTreeSet::new(),
        invalidations: BTreeSet::new(),
        ignored_source_admissions: Vec::new(),
        repository_parse_identity: CodeIndexRepositoryParseIdentityV1 {
            tree: None,
            dirty: RepositoryDirtyStateV1::Dirty,
        },
        sealed_at: UtcMicros(1_100_000),
        target_projection_key,
    };
    let config = CodeIndexProductionConfigV1 {
        project_id: id::<ProjectId>("project.candidate.fixture")?,
        repository: id::<RepositoryId>("repository.candidate.fixture")?,
        sanitizer_revision: id::<SanitizerRevision>("sanitizer.candidate.v1")?,
        policy_revision: id::<PolicyRevisionId>("policy.candidate.v1")?,
        // Must match the daemon production code-index projection identity so
        // the evaluated corpus chunks exactly as the mounted journey does.
        chunker_revision: id::<ChunkerRevision>(DAEMON_CODE_INDEX_CHUNKER_REVISION)?,
        privacy_domain: id::<PrivacyDomainId>("privacy.local-code-index")?,
        privacy_key_epoch: 1,
        max_snapshot_age_micros: None,
    };
    let generation = CodeIndexProductionOwnerV1::new(
        config,
        SharedPublicationStore::default(),
        ApplyingProjectionSink,
    )
    .map_err(|error| CandidateOutputError::Contract(format!("open production owner: {error}")))?
    .build_and_publish(request, &ActiveControl)
    .map_err(|error| CandidateOutputError::Contract(format!("publish generation: {error}")))?;
    let expected_chunks = match copies {
        1 => workload.execution_contract.exact_eligible_chunks_current,
        10 => workload.execution_contract.exact_eligible_chunks_10x,
        _ => {
            return Err(CandidateOutputError::Contract(
                "evaluation corpus scale must be current or exact 10x".to_owned(),
            ));
        }
    };
    let observed_chunks = generation.chunks().chunks().len() as u64;
    if observed_chunks != expected_chunks {
        return Err(CandidateOutputError::Contract(format!(
            "eligible chunk count mismatch for {copies}x corpus: declared {expected_chunks}, observed {observed_chunks}"
        )));
    }
    let symbol_displays: Arc<BTreeMap<_, _>> = Arc::new(
        generation
            .symbols()
            .symbols
            .iter()
            .map(|symbol| {
                (
                    symbol.occurrence.clone(),
                    VerifiedSealedLexicalSymbolDisplayV1::from(symbol.as_ref()),
                )
            })
            .collect(),
    );
    let mut occurrence_map = BTreeMap::new();
    for chunk in generation.chunks().chunks() {
        let Some(document) = file_to_document.get(chunk.anchor.file_occurrence_id.as_str()) else {
            continue;
        };
        let qualified_name = chunk
            .anchor
            .symbol_occurrence_id
            .as_ref()
            .and_then(|symbol| symbol_displays.get(symbol))
            .map(VerifiedSealedLexicalSymbolDisplayV1::qualified_name);
        let display_anchors = display_anchors_for_chunk(chunk, document, qualified_name);
        if let Some(symbol) = &chunk.anchor.symbol_occurrence_id {
            occurrence_map.insert(
                format!("code-symbol:{}", symbol.as_str()),
                OccurrenceMapEntry {
                    document_id: document.document_id.clone(),
                    scope: document.scope.clone(),
                    display_anchors: display_anchors.clone(),
                },
            );
            occurrence_map.insert(
                format!("code-graph:{}", symbol.as_str()),
                OccurrenceMapEntry {
                    document_id: document.document_id.clone(),
                    scope: document.scope.clone(),
                    display_anchors: display_anchors.clone(),
                },
            );
        }
        occurrence_map.insert(
            format!("code-chunk:{}", chunk.id.as_str()),
            OccurrenceMapEntry {
                document_id: document.document_id.clone(),
                scope: document.scope.clone(),
                display_anchors,
            },
        );
    }

    let eligible_chunks = generation
        .admitted_chunks()
        .map_err(|error| CandidateOutputError::Contract(error.to_string()))?
        .len() as u64;
    let (lexical_projections, graph_projections) = build_query_projections(
        &generation,
        &file_scopes,
        symbol_displays,
        &workload.queries,
    )?;
    Ok(PublishedCorpus {
        generation,
        lexical_projections,
        graph_projections,
        occurrence_map,
        repo_root: repo_root.to_path_buf(),
        source_commit: GitOidV1::new(workload.source_repository_commit.clone())
            .map_err(|error| CandidateOutputError::Contract(error.to_string()))?,
        corpus: workload.corpus.clone(),
        corpus_digest,
        eligible_chunks,
        admitted_scope,
    })
}

fn display_anchors_for_chunk(
    chunk: &CodeSearchChunkV1,
    document: &CorpusDocumentV1,
    qualified_name: Option<&str>,
) -> Vec<String> {
    let mut anchors = BTreeSet::from([document.document_id.clone()]);
    if let Some(qualified_name) = qualified_name {
        anchors.insert(qualified_name.to_owned());
        anchors.insert(display_qualified_anchor(document, qualified_name));
    }
    for term in &chunk.exact_terms {
        let term = String::from_utf8_lossy(term.canonical_bytes());
        if !term.is_empty() {
            anchors.insert(format!("{}::{term}", document.document_id));
        }
    }
    let text = chunk.sanitized_text.as_str();
    let first = text.lines().next().unwrap_or(text).trim();
    if !first.is_empty() {
        anchors.insert(format!("{}::{first}", document.document_id));
    }
    let primary = chunk
        .exact_terms
        .iter()
        .find(|term| {
            matches!(
                term.kind(),
                tracedecay_domain::ExactTechnicalTermKindV1::WholeSymbol
            )
        })
        .map(|term| {
            format!(
                "{}::{}",
                document.document_id,
                String::from_utf8_lossy(term.canonical_bytes())
            )
        })
        .or_else(|| {
            qualified_name.map(|qualified_name| display_qualified_anchor(document, qualified_name))
        })
        .unwrap_or_else(|| {
            format!(
                "{}:{}-{}",
                document.document_id,
                chunk.anchor.source_span.start_byte,
                chunk.anchor.source_span.end_byte
            )
        });
    anchors.insert(primary.clone());
    let mut ordered = vec![primary.clone()];
    ordered.extend(anchors.into_iter().filter(|anchor| anchor != &primary));
    ordered
}

fn display_qualified_anchor(document: &CorpusDocumentV1, qualified_name: &str) -> String {
    let segments: Vec<_> = document.source_path.split('/').collect();
    let module_prefix = segments
        .iter()
        .position(|segment| *segment == "src")
        .map(|src| {
            let mut modules = vec!["crate"];
            modules.extend(segments[src + 1..].iter().copied());
            if let Some(file) = modules.last_mut() {
                *file = file.strip_suffix(".rs").unwrap_or(file);
            }
            if modules
                .last()
                .is_some_and(|module| matches!(*module, "lib" | "main" | "mod"))
            {
                modules.pop();
            }
            modules.join("::")
        });
    let local_name = qualified_name
        .strip_prefix(&document.source_path)
        .and_then(|suffix| suffix.strip_prefix("::"))
        .or_else(|| {
            module_prefix
                .as_deref()
                .and_then(|prefix| qualified_name.strip_prefix(prefix))
                .and_then(|suffix| suffix.strip_prefix("::"))
        })
        .unwrap_or(qualified_name);
    if local_name.is_empty() {
        document.document_id.clone()
    } else {
        format!("{}::{local_name}", document.document_id)
    }
}

fn graph_seeds_from_outcomes(
    exact: &RetrieverOutcome<
        tracedecay_domain::RetrieverBatch<tracedecay_query::retrieval::exact::ExactLaneEvidence>,
    >,
    lexical: &RetrieverOutcome<
        tracedecay_domain::RetrieverBatch<
            tracedecay_query::retrieval::lexical::LexicalLaneEvidence,
        >,
    >,
) -> Vec<CodeCandidateBindingV1> {
    let mut seeds = Vec::new();
    let mut seen_occurrences = BTreeSet::new();
    let mut seen_symbols = BTreeSet::new();
    let mut push_seed = |binding: &CodeCandidateBindingV1,
                         seeds: &mut Vec<CodeCandidateBindingV1>| {
        let Some(symbol) = binding.occurrence.symbol.as_ref() else {
            return;
        };
        if !seen_occurrences.insert(binding.source_occurrence.clone()) {
            return;
        }
        if !seen_symbols.insert(symbol.clone()) {
            return;
        }
        seeds.push(binding.clone());
    };
    if let RetrieverOutcome::Complete(batch) | RetrieverOutcome::Partial { value: batch, .. } =
        exact
    {
        for evidence in batch.evidence_by_occurrence.values() {
            push_seed(&evidence.binding, &mut seeds);
            if seeds.len() >= 8 {
                return seeds;
            }
        }
    }
    if let RetrieverOutcome::Complete(batch) | RetrieverOutcome::Partial { value: batch, .. } =
        lexical
    {
        for evidence in batch.evidence_by_occurrence.values() {
            push_seed(&evidence.binding, &mut seeds);
            if seeds.len() >= 8 {
                return seeds;
            }
        }
    }
    seeds
}

fn retrieval_request(
    profile_id: &str,
    published: &PublishedCorpus,
) -> Result<RetrievalRequest, CandidateOutputError> {
    let manifest = published.generation.manifest();
    let freshness_digest = canonical_sha256(&(
        "tracedecay.search-eval.freshness.v1",
        &manifest.generation_id,
        &manifest.seal.expected_digest,
        manifest.seal.sealed_at,
    ))?;
    Ok(RetrievalRequest {
        principal: id::<PrincipalId>("principal.candidate")?,
        scope: RetrievalScope {
            privacy_domain: id("privacy.local-code-index")?,
            root: SingleRootScopeV1 {
                repository: id("repository.candidate.fixture")?,
                worktree: None,
                reference: None,
            },
        },
        temporal_mode: TemporalModeV1::Current,
        snapshot: RetrievalSnapshot {
            watermarks: VectorWatermark::default(),
            freshness_digest: id(&freshness_digest)?,
            authorization_revision: id("authorization.candidate.v1")?,
            captured_at: manifest.seal.sealed_at,
        },
        profile_id: id(&format!("profile.{profile_id}"))?,
        budget: retrieval_budget(),
    })
}

fn lexical_projection_profile_digest() -> Result<ManifestDigest, CandidateOutputError> {
    let digest = canonical_sha256(&(
        "tracedecay.search-eval.lexical-projection-profile.v1",
        "lexical.candidate.v1",
        "sanitizer.candidate.v1",
        "chunker.candidate.v2",
        "policy.candidate.v1",
    ))?;
    id(&digest)
}

fn write_pretty_json(path: &Path, value: &impl Serialize) -> Result<(), CandidateOutputError> {
    let bytes = serde_json::to_vec_pretty(value)
        .map_err(|error| CandidateOutputError::Contract(format!("serialize: {error}")))?;
    fs::write(path, bytes).map_err(|source| CandidateOutputError::Write {
        path: path.to_path_buf(),
        source,
    })
}

#[cfg(test)]
pub(crate) mod tests {
    use super::peak_rss::{
        PeakRssObservation, PeakRssPendingReason, peak_rss_bytes_from_status,
        windows_peak_rss_observation,
    };
    use super::*;
    use crate::packaged_assets::PackagedEvaluatorAssets;
    use tracedecay_query::search_quality::candidate_output::ResourceMeasurementStatusV1;

    /// One materialized copy of the packaged workload, corpus, and Git
    /// authority shared by every test in this binary. The packaged root is
    /// the same fixture the production `compare` command evaluates, so the
    /// tests never depend on the checkout they run from.
    pub(crate) fn packaged_fixture() -> Arc<PackagedEvaluatorAssets> {
        static FIXTURE: std::sync::OnceLock<Mutex<std::sync::Weak<PackagedEvaluatorAssets>>> =
            std::sync::OnceLock::new();
        let mut fixture = FIXTURE
            .get_or_init(|| Mutex::new(std::sync::Weak::new()))
            .lock()
            .expect("packaged fixture lock");
        if let Some(fixture) = fixture.upgrade() {
            return fixture;
        }
        let replacement =
            Arc::new(crate::packaged_assets::materialize().expect("packaged evaluator assets"));
        *fixture = Arc::downgrade(&replacement);
        replacement
    }

    /// Stands in for the composing binary's identity authority. The evaluator
    /// only requires a self-consistent admitted scope; resolving one from the
    /// on-disk repository identity marker is the root binary's contract and is
    /// covered where that authority lives.
    fn fixture_admitted_scope(_repo_root: &Path) -> Option<ResolvedScope> {
        ResolvedScope::new(
            ProjectId::new("project.search-eval-fixture").ok()?,
            RepositoryId::new("repository.search-eval-fixture").ok()?,
            tracedecay_domain::WorktreeId::new("worktree.search-eval-fixture").ok()?,
            None,
        )
        .ok()
    }

    fn workload() -> CandidateWorkloadV1 {
        packaged_fixture().workload().clone()
    }

    #[test]
    fn fusion_profile_carries_the_checked_in_lane_weights() {
        let workload = workload();
        let spec = workload
            .profile_matrix
            .iter()
            .find(|profile| profile.profile_id == "query-fallback")
            .expect("checked-in fallback profile");
        let profile = fusion_profile(spec).expect("fusion profile");

        assert_eq!(
            profile.profile_id.as_str(),
            format!("profile.{}", spec.profile_id)
        );
        assert_eq!(
            profile.weights_micros,
            BTreeMap::from([
                (RetrieverKind::ExactLiteral, 1_000_000),
                (RetrieverKind::Lexical, spec.lexical_weight_ppm),
                (RetrieverKind::Graph, spec.graph_weight_ppm),
            ])
        );
        assert_eq!(
            profile.calibrations.keys().copied().collect::<Vec<_>>(),
            RetrieverKind::QUERY_FALLBACK_LANES
        );
        assert!(profile.minimum_calibrated_feature_micros.is_empty());
        assert_eq!(
            evaluated_diversity_policy().expect("diversity").per_file,
            Some(2)
        );
    }

    #[test]
    fn profile_material_digest_binds_every_checked_in_weight() {
        let workload = workload();
        let profile = workload.profile_matrix.first().expect("profile");
        let digest = compute_profile_material_digest(profile).expect("digest");
        let mut changed = profile.clone();
        changed.graph_weight_ppm = changed.graph_weight_ppm.saturating_add(1);

        assert_ne!(
            digest,
            compute_profile_material_digest(&changed).expect("changed digest")
        );
    }

    #[test]
    fn qualified_display_anchor_strips_exact_source_identity() {
        let document = CorpusDocumentV1 {
            document_id: "watermark".to_owned(),
            source_path: "crates/tracedecay-domain/src/research/watermark.rs".to_owned(),
            path: "tests/fixtures/search_quality/corpus/watermark.rs".to_owned(),
            scope: "research".to_owned(),
            language: "rust".to_owned(),
            eligibility: "eligible".to_owned(),
        };

        assert_eq!(
            display_qualified_anchor(
                &document,
                "crates/tracedecay-domain/src/research/watermark.rs::VectorWatermark::merge_max"
            ),
            "watermark::VectorWatermark::merge_max"
        );
    }

    #[test]
    fn historical_candidates_require_product_repository_identity() {
        let fixture = packaged_fixture();
        let workload = workload();
        let published = publish_corpus(fixture.root(), &workload, no_admitted_corpus_scope)
            .expect("markerless corpus");
        let query = workload
            .queries
            .iter()
            .find(|query| query.query_id == "train-012")
            .expect("historical query");

        let error = historical_candidates(&published, query).expect_err("identity is required");
        assert!(
            error
                .to_string()
                .contains("authoritative repository identity marker")
        );
    }

    #[test]
    fn direct_workload_requires_checked_in_labels() {
        let mut workload = workload();
        workload.queries[0].label = None;
        let error = validate_workload_for_tuning(&workload).expect_err("missing label");
        assert!(error.to_string().contains("missing its checked-in label"));
    }

    #[test]
    fn technical_query_tokens_do_not_leak_common_subtokens() {
        let absent = lexical_query_parts("qzxw_owner_validation_absent_551").expect("absent query");
        assert_eq!(
            absent.whole_terms,
            ["qzxw_owner_validation_absent_551".to_owned()]
        );
        assert!(absent.subtokens.is_empty());
        let private = lexical_query_parts("SessionEvidenceMetadataV1").expect("private query");
        assert_eq!(
            private.whole_terms,
            ["SessionEvidenceMetadataV1".to_owned()]
        );
        assert!(private.subtokens.is_empty());
    }

    #[test]
    fn direct_workload_rejects_ambiguous_corpus_identity() {
        let mut duplicate_id = workload();
        duplicate_id.corpus[1].document_id = duplicate_id.corpus[0].document_id.clone();
        let error = validate_workload_for_tuning(&duplicate_id).expect_err("duplicate document id");
        assert!(error.to_string().contains("duplicate corpus document_id"));

        let mut duplicate_path = workload();
        duplicate_path.corpus[1].path = duplicate_path.corpus[0].path.clone();
        let error =
            validate_workload_for_tuning(&duplicate_path).expect_err("duplicate corpus path");
        assert!(error.to_string().contains("duplicate corpus path"));

        let mut duplicate_source_path = workload();
        duplicate_source_path.corpus[1].source_path =
            duplicate_source_path.corpus[0].source_path.clone();
        let error = validate_workload_for_tuning(&duplicate_source_path)
            .expect_err("duplicate source path");
        assert!(error.to_string().contains("duplicate corpus source_path"));

        let mut unsafe_source_path = workload();
        unsafe_source_path.corpus[0].source_path = "../outside.rs".to_owned();
        let error =
            validate_workload_for_tuning(&unsafe_source_path).expect_err("unsafe source path");
        assert!(error.to_string().contains("safe repository-relative path"));
    }

    #[test]
    fn direct_workload_rejects_empty_and_duplicate_query_ids() {
        let mut empty = workload();
        empty.queries[0].query_id.clear();
        let error = validate_workload_for_tuning(&empty).expect_err("empty query id");
        assert!(error.to_string().contains("query_id must not be empty"));

        let mut duplicate = workload();
        duplicate.queries[1].query_id = duplicate.queries[0].query_id.clone();
        let error = validate_workload_for_tuning(&duplicate).expect_err("duplicate query id");
        assert!(error.to_string().contains("duplicate query_id"));

        let mut invalid_history = workload();
        invalid_history.queries[0].historical_commit = Some("not-a-commit".to_owned());
        let error =
            validate_workload_for_tuning(&invalid_history).expect_err("invalid historical commit");
        assert!(error.to_string().contains("invalid historical commit"));
    }

    #[test]
    fn direct_workload_rejects_duplicate_profile_ids_and_empty_partitions() {
        let mut duplicate = workload();
        let copy = duplicate.profile_matrix[0].clone();
        duplicate.profile_matrix.push(copy);
        let error = validate_workload_for_tuning(&duplicate).expect_err("duplicate profile id");
        assert!(error.to_string().contains("duplicate profile_id"));

        let mut missing = workload();
        missing.queries.retain(|query| query.partition == "train");
        missing.execution_contract.exact_query_count = missing.queries.len() as u64;
        let error = validate_workload_for_tuning(&missing).expect_err("empty validation partition");
        assert!(
            error
                .to_string()
                .contains("partition validation has no queries")
        );
    }

    #[test]
    fn workload_requires_immutable_query_fallback_digests_for_both_partitions() {
        let mut missing = workload();
        missing.expected_query_fallback_digests.remove("validation");
        let error =
            validate_workload_for_tuning(&missing).expect_err("missing validation fallback digest");
        assert!(
            error
                .to_string()
                .contains("expected query fallback digests must bind train and validation")
        );

        let mut malformed = workload();
        malformed.expected_query_fallback_digests.insert(
            "train".to_owned(),
            "sha256:not-a-canonical-digest".to_owned(),
        );
        let error =
            validate_workload_for_tuning(&malformed).expect_err("malformed fallback digest");
        assert!(
            error
                .to_string()
                .contains("expected query fallback digest is not canonical")
        );
    }

    #[test]
    fn candidate_generation_rejects_partially_unknown_profile_selection() {
        let fixture = packaged_fixture();
        let error = generate_candidate_outputs(&GenerateCandidateOutputsOptions {
            repo_root: fixture.root(),
            admitted_scope: fixture_admitted_scope,
            workload_path: None,
            profile_ids: Some(&["query-fallback".to_owned(), "unknown-profile".to_owned()]),
        })
        .expect_err("unknown profile");
        assert!(error.to_string().contains("unknown requested profile_id"));
    }

    #[test]
    fn direct_outputs_cover_train_and_validation() {
        let fixture = packaged_fixture();
        let fixture_root = fixture.root();
        let workload = workload();
        let result = generate_candidate_outputs(&GenerateCandidateOutputsOptions {
            repo_root: fixture_root,
            admitted_scope: fixture_admitted_scope,
            workload_path: None,
            profile_ids: Some(&["query-fallback".to_owned()]),
        })
        .expect("generate");
        assert_eq!(result.outputs.len(), 2);
        let expected_corpus_digest =
            compute_corpus_digest(fixture_root, &workload).expect("corpus digest");
        for output in &result.outputs {
            assert_eq!(output.schema_version, 2);
            assert!(output.partition == "train" || output.partition == "validation");
            assert_eq!(output.production_boundary, PRODUCTION_BOUNDARY);
            assert_eq!(output.cancellation, REQUIRED_CANCELLATION);
            assert_eq!(output.offline, REQUIRED_OFFLINE);
            assert_eq!(output.fallback_digest, output.query_fallback_digest);
            assert_eq!(
                output.expected_query_fallback_digest,
                workload.expected_query_fallback_digests[&output.partition]
            );
            assert_eq!(
                output.query_fallback_matches_expected,
                output.query_fallback_digest == output.expected_query_fallback_digest
            );
            assert_eq!(output.corpus_digest, expected_corpus_digest);
            assert_eq!(output.seed, EVALUATION_SEED);
            assert_eq!(output.cache_state, EVALUATION_CACHE_STATE);
            let current = output.resources.get("current").expect("current samples");
            let expected_status = if peak_rss_bytes().is_measured() {
                ResourceMeasurementStatusV1::Measured
            } else {
                ResourceMeasurementStatusV1::Pending
            };
            assert_eq!(current.status, expected_status);
            assert_eq!(
                current.measured_queries,
                current.latency_samples_us.len() as u64
            );
            assert!(
                serde_json::to_value(current)
                    .expect("resource sample serializes")
                    .get("p99_latency_us")
                    .is_none(),
                "small raw samples must not manufacture p99"
            );
            let ten_x = output.resources.get("10x").expect("10x status");
            assert_eq!(ten_x.status, expected_status);
            assert_eq!(ten_x.measured_queries, output.queries.len() as u64);
            assert_eq!(
                ten_x.measured_queries,
                ten_x.latency_samples_us.len() as u64
            );
            assert_eq!(
                ten_x.eligible_chunks,
                current.eligible_chunks.saturating_mul(10)
            );
            assert_eq!(
                ten_x.peak_rss_bytes.is_some(),
                peak_rss_bytes().is_measured()
            );
            assert!(
                output.queries.iter().all(|query| {
                    serde_json::to_value(query)
                        .expect("query serializes")
                        .get("confidence_ppm")
                        .is_none()
                }),
                "candidate rows must not manufacture confidence"
            );
            for row in &output.queries {
                let query = workload
                    .queries
                    .iter()
                    .find(|query| query.query_id == row.query_id)
                    .expect("checked-in query");
                assert!(
                    row.ranked
                        .iter()
                        .all(|candidate| query.allowed_scopes.contains(&candidate.scope)),
                    "{} leaked a candidate outside its allowed scopes",
                    row.query_id
                );
                assert!(
                    row.ranked.iter().all(|candidate| {
                        !candidate.anchors.is_empty()
                            && candidate.anchors.contains(&candidate.anchor)
                    }),
                    "{} lost authoritative candidate anchors",
                    row.query_id
                );
            }
        }
    }

    #[test]
    fn published_corpus_maps_production_source_occurrences() {
        let fixture = packaged_fixture();
        let workload = workload();
        let published = publish_corpus(fixture.root(), &workload, fixture_admitted_scope)
            .expect("publish corpus");

        for chunk in published.generation.chunks().chunks() {
            assert_eq!(
                chunk.chunker_revision.as_str(),
                DAEMON_CODE_INDEX_CHUNKER_REVISION,
                "evaluation corpus must use the current daemon chunker identity"
            );
            let chunk_occurrence = format!("code-chunk:{}", chunk.id.as_str());
            assert!(
                published.occurrence_map.contains_key(&chunk_occurrence),
                "missing exact chunk occurrence {chunk_occurrence}"
            );
            if let Some(symbol) = &chunk.anchor.symbol_occurrence_id {
                let symbol_occurrence = format!("code-symbol:{}", symbol.as_str());
                assert!(
                    published.occurrence_map.contains_key(&symbol_occurrence),
                    "missing fused symbol occurrence {symbol_occurrence}"
                );
                let graph_occurrence = format!("code-graph:{}", symbol.as_str());
                assert!(
                    published.occurrence_map.contains_key(&graph_occurrence),
                    "missing exact graph occurrence {graph_occurrence}"
                );
            }
        }

        let authoritative_anchors: BTreeSet<_> = published
            .occurrence_map
            .values()
            .flat_map(|entry| entry.display_anchors.iter().map(String::as_str))
            .collect();
        for expected in [
            "watermark::VectorWatermark::merge_max",
            "error::DomainError::InvalidTimeInterval",
            "time::TimeInterval::validate",
            "config_store::ConfigStore::write_config",
            "coverage::RetentionClass::new",
        ] {
            assert!(
                authoritative_anchors.contains(expected),
                "missing extraction-qualified anchor {expected}"
            );
        }

        let path_query = workload
            .queries
            .iter()
            .find(|query| query.query_id == "validation-003")
            .expect("path query");
        let path_output =
            compose_production_query(&published, &workload.profile_matrix[0], path_query)
                .expect("path query composes");
        let path_rows =
            map_ranked_candidates(&published, &path_output).expect("path candidates map");
        assert!(
            path_rows
                .iter()
                .any(|candidate| candidate.document_id == "watermark"),
            "snapshot logical path must retrieve the bound source document"
        );

        let history_query = workload
            .queries
            .iter()
            .find(|query| query.query_id == "train-012")
            .expect("history query");
        let (history_status, history_rows) =
            historical_candidates(&published, history_query).expect("history query executes");
        assert_eq!(history_status, HistoricalQueryExecutionV1::Complete);
        assert!(history_rows.iter().any(|candidate| {
            candidate
                .anchors
                .iter()
                .any(|anchor| anchor.contains("crates/tracedecay-domain/src/session.rs"))
        }));

        for profile in &workload.profile_matrix {
            for query in &workload.queries {
                let output =
                    compose_production_query(&published, profile, query).expect("production query");
                for ranked in output.ranked_candidates {
                    assert!(
                        published
                            .occurrence_map
                            .contains_key(ranked.candidate.anchor_id.as_str())
                            || ranked.candidate.occurrences.iter().any(|occurrence| {
                                published
                                    .occurrence_map
                                    .contains_key(occurrence.source_occurrence_id.as_str())
                            }),
                        "ranked candidate {} has no corpus occurrence binding",
                        ranked.candidate.anchor_id
                    );
                }
            }
        }
    }

    #[test]
    fn distinct_ten_x_corpus_produces_measured_resource_evidence() {
        const CHILD_ENV: &str = "TRACEDECAY_RESOURCE_EVIDENCE_TEST_CHILD";
        if std::env::var_os(CHILD_ENV).is_some() {
            assert_distinct_ten_x_corpus_produces_measured_resource_evidence();
            return;
        }

        let output = std::process::Command::new(
            std::env::current_exe().expect("resource test binary has a current executable"),
        )
        .args([
            "--exact",
            "search_eval::candidate_output::tests::distinct_ten_x_corpus_produces_measured_resource_evidence",
            "--nocapture",
        ])
        .env(CHILD_ENV, "1")
        .output()
        .expect("run resource measurement in a dedicated process");
        assert!(
            output.status.success(),
            "dedicated resource measurement failed:\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn assert_distinct_ten_x_corpus_produces_measured_resource_evidence() {
        let fixture = packaged_fixture();
        let fixture_root = fixture.root();
        let workload = workload();
        let result = generate_candidate_outputs(&GenerateCandidateOutputsOptions {
            repo_root: fixture_root,
            admitted_scope: fixture_admitted_scope,
            workload_path: None,
            profile_ids: Some(&["query-fallback".to_owned()]),
        })
        .expect("generate");
        let report =
            crate::evaluate_generated_outputs(fixture_root, &workload, &result).expect("evaluate");

        let expected_status = crate::DirectEvaluationStatusV1::Fail;
        #[cfg(windows)]
        let expected_resource_status = crate::DirectEvaluationStatusV1::Pass;
        #[cfg(not(windows))]
        let expected_resource_status = if peak_rss_bytes().is_measured() {
            crate::DirectEvaluationStatusV1::Pass
        } else {
            crate::DirectEvaluationStatusV1::Pending
        };
        let resources = result
            .outputs
            .iter()
            .map(|output| (&output.partition, &output.resources))
            .collect::<Vec<_>>();
        assert_eq!(
            report.status, expected_status,
            "unexpected grouped resource evaluation: {resources:#?}"
        );
        assert!(
            report
                .profiles
                .iter()
                .all(|profile| { profile.resource_status == expected_resource_status })
        );

        let mut quality_failure = result.clone();
        for output in &mut quality_failure.outputs {
            for sample in output.resources.values_mut() {
                sample.status = ResourceMeasurementStatusV1::Measured;
                sample.peak_rss_bytes = Some(4096);
                sample.pending_reason = None;
            }
            for query in &mut output.queries {
                query.ranked.clear();
                query.abstained = true;
            }
        }
        let failed_report =
            crate::evaluate_generated_outputs(fixture_root, &workload, &quality_failure)
                .expect("evaluate known quality failure with measured resources");
        assert_eq!(
            failed_report.status,
            crate::DirectEvaluationStatusV1::Fail,
            "measured resource evidence must not promote a quality failure"
        );
        assert!(
            failed_report.profiles.iter().all(|profile| {
                profile.resource_status == crate::DirectEvaluationStatusV1::Pass
            })
        );
        assert!(
            failed_report
                .profiles
                .iter()
                .any(|profile| profile.failed_queries > 0)
        );

        let current = publish_corpus(fixture_root, &workload, fixture_admitted_scope)
            .expect("current corpus");
        let ten_x = publish_corpus_with_scale(fixture_root, &workload, 10, fixture_admitted_scope)
            .expect("10x corpus");
        assert_ne!(
            current.generation.manifest().generation_id,
            ten_x.generation.manifest().generation_id
        );
        assert_eq!(
            ten_x.eligible_chunks,
            current.eligible_chunks.saturating_mul(10)
        );

        let mut missing_resource = result.clone();
        missing_resource.outputs[0].resources.remove("10x");
        let report = crate::evaluate_generated_outputs(fixture_root, &workload, &missing_resource)
            .expect("evaluate");
        assert_eq!(report.status, crate::DirectEvaluationStatusV1::Fail);
        assert_eq!(
            report.profiles[0].resource_status,
            crate::DirectEvaluationStatusV1::Fail
        );

        let mut duplicate_query = result.clone();
        duplicate_query.outputs[0].queries[1] = duplicate_query.outputs[0].queries[0].clone();
        let error = crate::evaluate_generated_outputs(fixture_root, &workload, &duplicate_query)
            .expect_err("duplicate query row");
        assert!(error.to_string().contains("duplicate query row"));

        let mut duplicate_profile_partition = result.clone();
        duplicate_profile_partition.outputs[1] = duplicate_profile_partition.outputs[0].clone();
        let error = crate::evaluate_generated_outputs(
            fixture_root,
            &workload,
            &duplicate_profile_partition,
        )
        .expect_err("duplicate profile partition");
        assert!(error.to_string().contains("duplicate profile/partition"));

        let mut forged = result;
        forged.outputs[0].production_boundary = "lookalike".to_owned();
        let error = crate::evaluate_generated_outputs(fixture_root, &workload, &forged)
            .expect_err("forged production boundary");
        assert!(error.to_string().contains("production boundary"));

        forged.outputs[0].production_boundary = PRODUCTION_BOUNDARY.to_owned();
        forged.outputs[0].fixture_source_commit = "forged".to_owned();
        let error = crate::evaluate_generated_outputs(fixture_root, &workload, &forged)
            .expect_err("forged source commit");
        assert!(error.to_string().contains("source commit"));

        forged.outputs[0].fixture_source_commit = workload.source_repository_commit.clone();
        forged.outputs[0].corpus_digest = canonical_sha256(&"forged corpus").expect("digest");
        let error = crate::evaluate_generated_outputs(fixture_root, &workload, &forged)
            .expect_err("forged corpus digest");
        assert!(error.to_string().contains("byte-exact corpus"));

        forged.outputs[0].corpus_digest =
            compute_corpus_digest(fixture_root, &workload).expect("corpus digest");
        forged.outputs[0].toolchain.clear();
        let error = crate::evaluate_generated_outputs(fixture_root, &workload, &forged)
            .expect_err("missing environment");
        assert!(error.to_string().contains("environment summary"));

        forged.outputs[0].toolchain = "rustc:test".to_owned();
        forged.outputs[0].queries[0].abstained = !forged.outputs[0].queries[0].ranked.is_empty();
        let error = crate::evaluate_generated_outputs(fixture_root, &workload, &forged)
            .expect_err("inconsistent abstention");
        assert!(error.to_string().contains("inconsistent abstention"));
    }

    #[test]
    fn resource_sample_reads_linux_peak_rss() {
        let status = "VmRSS:\t1024 kB\nVmHWM:\t2048 kB\n";
        assert_eq!(peak_rss_bytes_from_status(status), Some(2 * 1024 * 1024));
    }

    #[test]
    fn resource_sample_translates_typed_peak_rss_observation() {
        let measured =
            completed_resource_sample(12, PeakRssObservation::Measured(4096), vec![7], 1);
        assert_eq!(measured.status, ResourceMeasurementStatusV1::Measured);
        assert_eq!(measured.peak_rss_bytes, Some(4096));
        assert_eq!(measured.pending_reason, None);

        for (reason, expected) in [
            (
                PeakRssPendingReason::LinuxStatusReadFailure("denied".to_owned()),
                "Linux peak_rss_bytes is unavailable because /proc/self/status could not be read: denied",
            ),
            (
                PeakRssPendingReason::LinuxMissingNonzeroVmHwm,
                "Linux peak_rss_bytes is unavailable because /proc/self/status has no nonzero VmHWM value",
            ),
            (
                PeakRssPendingReason::MacOsGetrusageFailure("denied".to_owned()),
                "macOS peak_rss_bytes is unavailable because getrusage(RUSAGE_SELF) failed: denied",
            ),
            (
                PeakRssPendingReason::MacOsNonPositiveMaxRss,
                "macOS peak_rss_bytes is unavailable because getrusage(RUSAGE_SELF) returned a non-positive ru_maxrss",
            ),
            (
                PeakRssPendingReason::WindowsK32GetProcessMemoryInfoFailure(
                    "access denied".to_owned(),
                ),
                "Windows peak_rss_bytes is unavailable because K32GetProcessMemoryInfo failed before PeakWorkingSetSize could be read: access denied",
            ),
            (
                PeakRssPendingReason::WindowsZeroPeakWorkingSetSize,
                "Windows peak_rss_bytes is unavailable because K32GetProcessMemoryInfo returned zero PeakWorkingSetSize",
            ),
            (
                PeakRssPendingReason::UnsupportedPlatform("other"),
                "other peak_rss_bytes is unavailable because the platform is unsupported",
            ),
        ] {
            let pending =
                completed_resource_sample(12, PeakRssObservation::Pending(reason), vec![7], 1);
            assert_eq!(pending.status, ResourceMeasurementStatusV1::Pending);
            assert_eq!(pending.peak_rss_bytes, None);
            assert_eq!(pending.pending_reason.as_deref(), Some(expected));
        }
    }

    #[test]
    fn windows_peak_rss_observation_rejects_api_failure_and_zero() {
        assert_eq!(
            windows_peak_rss_observation(4096, Some("access denied".to_owned())),
            PeakRssObservation::Pending(
                PeakRssPendingReason::WindowsK32GetProcessMemoryInfoFailure(
                    "access denied".to_owned(),
                )
            )
        );
        assert_eq!(
            windows_peak_rss_observation(0, None),
            PeakRssObservation::Pending(PeakRssPendingReason::WindowsZeroPeakWorkingSetSize)
        );
        assert_eq!(
            windows_peak_rss_observation(4096, None),
            PeakRssObservation::Measured(4096)
        );
    }

    #[test]
    fn peak_rss_combination_keeps_failed_required_observation_pending() {
        let reason =
            PeakRssPendingReason::WindowsK32GetProcessMemoryInfoFailure("access denied".to_owned());
        assert_eq!(
            PeakRssObservation::Pending(
                PeakRssPendingReason::WindowsK32GetProcessMemoryInfoFailure(
                    "access denied".to_owned(),
                ),
            )
            .max(PeakRssObservation::Measured(4096)),
            PeakRssObservation::Pending(reason)
        );
        assert_eq!(
            PeakRssObservation::Measured(4096).max(PeakRssObservation::Pending(
                PeakRssPendingReason::LinuxMissingNonzeroVmHwm,
            )),
            PeakRssObservation::Pending(PeakRssPendingReason::LinuxMissingNonzeroVmHwm)
        );
        assert_eq!(
            PeakRssObservation::Pending(PeakRssPendingReason::WindowsZeroPeakWorkingSetSize).max(
                PeakRssObservation::Pending(PeakRssPendingReason::LinuxMissingNonzeroVmHwm),
            ),
            PeakRssObservation::Pending(PeakRssPendingReason::WindowsZeroPeakWorkingSetSize)
        );
    }

    #[test]
    fn resource_evidence_enforces_state_and_exact_catalog_without_size_caps() {
        let fixture = packaged_fixture();
        let fixture_root = fixture.root();
        let workload = workload();
        let result = generate_candidate_outputs(&GenerateCandidateOutputsOptions {
            repo_root: fixture_root,
            admitted_scope: fixture_admitted_scope,
            workload_path: None,
            profile_ids: Some(&["query-fallback".to_owned()]),
        })
        .expect("generate");

        let mut invalid_pending = result.clone();
        let current = invalid_pending.outputs[0]
            .resources
            .get_mut("current")
            .expect("current resource");
        current.status = ResourceMeasurementStatusV1::Pending;
        current.pending_reason = None;
        let report = crate::evaluate_generated_outputs(fixture_root, &workload, &invalid_pending)
            .expect("evaluate");
        assert_eq!(
            report.profiles[0].resource_status,
            crate::DirectEvaluationStatusV1::Fail
        );

        let mut wrong_scale = result.clone();
        let current_chunks = wrong_scale.outputs[0]
            .resources
            .get("current")
            .expect("current resource")
            .eligible_chunks;
        wrong_scale.outputs[0]
            .resources
            .get_mut("10x")
            .expect("10x resource")
            .eligible_chunks = current_chunks;
        let report = crate::evaluate_generated_outputs(fixture_root, &workload, &wrong_scale)
            .expect("evaluate");
        assert_eq!(
            report.profiles[0].resource_status,
            crate::DirectEvaluationStatusV1::Fail
        );

        let mut large_measurement = result.clone();
        let current = large_measurement.outputs[0]
            .resources
            .get_mut("current")
            .expect("current resource");
        current.status = ResourceMeasurementStatusV1::Measured;
        current.peak_rss_bytes = Some(u64::MAX);
        current.pending_reason = None;
        current.latency_samples_us.fill(u64::MAX);
        let report = crate::evaluate_generated_outputs(fixture_root, &workload, &large_measurement)
            .expect("evaluate");
        // Only "current" was rewritten as measured; "10x" keeps the host's
        // own sample, which stays pending where peak RSS is unreadable.
        let expected_large = if peak_rss_bytes().is_measured() {
            crate::DirectEvaluationStatusV1::Pass
        } else {
            crate::DirectEvaluationStatusV1::Pending
        };
        assert_eq!(report.profiles[0].resource_status, expected_large);

        let mut extra_resource = result;
        let synthetic = extra_resource.outputs[0]
            .resources
            .get("current")
            .expect("current resource")
            .clone();
        extra_resource.outputs[0]
            .resources
            .insert("synthetic".to_owned(), synthetic);
        let report = crate::evaluate_generated_outputs(fixture_root, &workload, &extra_resource)
            .expect("evaluate");
        assert_eq!(
            report.profiles[0].resource_status,
            crate::DirectEvaluationStatusV1::Fail
        );
    }

    #[test]
    fn candidate_bytes_match_direct_production_calls() {
        let fixture = packaged_fixture();
        let workload = workload();
        let result = generate_candidate_outputs(&GenerateCandidateOutputsOptions {
            repo_root: fixture.root(),
            admitted_scope: fixture_admitted_scope,
            workload_path: None,
            profile_ids: Some(&["query-fallback".to_owned()]),
        })
        .expect("generate");
        let train = result
            .outputs
            .iter()
            .find(|output| output.partition == "train" && output.profile_id == "query-fallback")
            .expect("train output");
        let probe = train.queries.first().expect("at least one train query");
        let direct = retrieve_partition_query_bytes(
            fixture.root(),
            &workload,
            "query-fallback",
            &probe.query_id,
            fixture_admitted_scope,
        )
        .expect("direct retrieve");
        let generated = canonical_json_bytes(probe).expect("generated bytes");
        assert_eq!(
            generated, direct,
            "generator row must match direct production call bytes"
        );
    }

    #[test]
    fn query_phrase_and_historical_queries_reach_their_checked_in_anchors() {
        let fixture = packaged_fixture();
        let workload = workload();
        let retrieve = |query_id: &str| {
            let bytes = retrieve_partition_query_bytes(
                fixture.root(),
                &workload,
                "query-fallback",
                query_id,
                fixture_admitted_scope,
            )
            .expect("direct retrieve");
            serde_json::from_slice::<QueryCandidateRowV1>(&bytes).expect("candidate row")
        };

        let diagnostic = retrieve("train-004");
        let diagnostic_top = diagnostic.ranked.iter().take(10).collect::<Vec<_>>();
        let diagnostic_target = diagnostic.ranked.iter().enumerate().find(|(_, candidate)| {
            candidate.anchor == "error::DomainError::InvalidTimeInterval"
                || candidate
                    .anchors
                    .iter()
                    .any(|anchor| anchor == "error::DomainError::InvalidTimeInterval")
        });
        assert!(
            diagnostic_top.iter().any(|candidate| {
                candidate.anchor == "error::DomainError::InvalidTimeInterval"
                    || candidate
                        .anchors
                        .iter()
                        .any(|anchor| anchor == "error::DomainError::InvalidTimeInterval")
            }),
            "diagnostic target: {diagnostic_target:#?}; top 10: {diagnostic_top:#?}"
        );

        let exact_symbol = retrieve("train-001");
        let unique_exact_symbol_anchors = exact_symbol
            .ranked
            .iter()
            .map(|candidate| candidate.anchor.as_str())
            .collect::<BTreeSet<_>>();
        assert_eq!(
            unique_exact_symbol_anchors.len(),
            exact_symbol.ranked.len(),
            "duplicate visible anchors: {:#?}",
            exact_symbol.ranked
        );

        let historical = retrieve("train-012");
        let historical_top = historical.ranked.iter().take(10).collect::<Vec<_>>();
        assert!(
            historical_top.iter().any(|candidate| {
                candidate.anchor
                == "git:01b0a0afe34c3342d6b5b076383f86ed8a8d0c66:crates/tracedecay-domain/src/session.rs::ClosedUtcIntervalV1"
                || candidate.anchors.iter().any(|anchor| {
                    anchor
                        == "git:01b0a0afe34c3342d6b5b076383f86ed8a8d0c66:crates/tracedecay-domain/src/session.rs::ClosedUtcIntervalV1"
                })
            }),
            "historical top 10: {historical_top:#?}"
        );
    }

    /// A file in one scope must land in every scope set admitting that scope
    /// and in no other; overlapping sets are the path a singleton workload
    /// never exercises.
    #[test]
    fn scoped_projections_partition_chunks_into_every_admitting_scope_set_only() {
        let fixture = packaged_fixture();
        let mut workload = workload();
        let profile = workload.profile_matrix[0].clone();
        let mut probe = |query_id: &str, scopes: &[&str]| {
            let mut query = workload.queries[0].clone();
            query.query_id = query_id.to_owned();
            query.query = "repository watermark".to_owned();
            query.allowed_scopes = scopes.iter().map(|scope| (*scope).to_owned()).collect();
            workload.queries.push(query.clone());
            query
        };
        let research = probe("probe-research", &["research"]);
        let project = probe("probe-project", &["project"]);
        let overlapping = probe("probe-overlap", &["research", "project", "research"]);
        let published = publish_corpus(fixture.root(), &workload, fixture_admitted_scope)
            .expect("published corpus");
        let scopes = |query: &WorkloadQueryV1| {
            let output = compose_production_query(&published, &profile, query)
                .expect("scoped query composes");
            map_ranked_candidates(&published, &output)
                .expect("ranked candidates map")
                .into_iter()
                .map(|candidate| candidate.scope)
                .collect::<BTreeSet<_>>()
        };

        assert_eq!(scopes(&research), BTreeSet::from(["research".to_owned()]));
        assert_eq!(scopes(&project), BTreeSet::from(["project".to_owned()]));
        assert_eq!(
            scopes(&overlapping),
            BTreeSet::from(["project".to_owned(), "research".to_owned()])
        );
    }
}
