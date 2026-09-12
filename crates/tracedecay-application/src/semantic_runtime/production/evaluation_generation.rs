//! Prepared evaluation generations and their execution control.

use std::collections::BTreeMap;
use std::sync::Arc;

use tracedecay_code_index::production::CodeIndexPublishedGenerationV1;
use tracedecay_domain::{
    CodeGenerationId, ManifestDigest, RetrievalCursorKeyId, SemanticSearchIndexKeyV1,
    SemanticSearchIndexProfileV1, VectorGenerationIdV1, canonical_sha256,
};
use tracedecay_query::retrieval::fusion::RetrievalCursorKeyringV1;
use tracedecay_query::retrieval::ports::RetrievalExecutionControl;
use tracedecay_query::retrieval::semantic::{SemanticCodeRetriever, SemanticRetrievalRequestV1};
use tracedecay_query::search_quality::{
    CandidateOutputError, ProductionCandidateNativeGenerationResourcesV1,
    ProductionCandidateNativeQueryContextV1, ProductionCandidateNativeQueryInputsV1,
};
use tracedecay_semantic::projector::PreparedVectorGenerationV1;
use tracedecay_semantic::rerank_adapter::{
    GenerationBoundCodeRerankViewsV1, ProductionCodeRerankAuthorityV1,
};
use tracedecay_semantic::{
    PreparedSemanticEvaluationProjectionV1, SemanticEvaluationCancellationV1,
    SemanticEvaluationProjectionBatchCacheV1, SemanticEvaluationQueryFactoryV1,
};
use tracedecay_semantic_contracts::{SemanticResourceCeilings, SemanticRuntimeScheduleFailureV1};

use super::evaluation_support::{InstalledArtifactMemberBytesV1, evaluation_vector_generation_id};
use super::published_vector_read::{
    PublishedSemanticVectorReadPortV1, ScopedSemanticEvaluationVectorReadPortV1,
};

pub struct PreparedSemanticEvaluationGenerationV1 {
    pub(super) code: CodeIndexPublishedGenerationV1,
    pub(super) cancellation: Arc<dyn SemanticEvaluationCancellationV1>,
    pub(super) source_generation: CodeGenerationId,
    pub(super) projection: tracedecay_domain::AdmittedEmbeddingProjectionKeyV1,
    pub(super) search_index_key: SemanticSearchIndexKeyV1,
    pub(super) vector_generation: VectorGenerationIdV1,
    pub(super) prepared_projection: PreparedVectorGenerationV1,
    pub(super) projection_input_bytes: u64,
    pub(super) projection_batch_cache: Arc<SemanticEvaluationProjectionBatchCacheV1>,
    pub(super) capability_manifest_digest: ManifestDigest,
    pub(super) query_factory: SemanticEvaluationQueryFactoryV1,
    pub(super) vectors: PublishedSemanticVectorReadPortV1,
    pub(super) query_keys: RetrievalCursorKeyringV1,
    pub(super) resources: ProductionCandidateNativeGenerationResourcesV1,
}

impl PreparedSemanticEvaluationGenerationV1 {
    #[allow(clippy::too_many_arguments)]
    pub(super) fn new(
        code: CodeIndexPublishedGenerationV1,
        prepared: PreparedSemanticEvaluationProjectionV1,
        artifact_digest: ManifestDigest,
        artifact_bytes: InstalledArtifactMemberBytesV1,
        execution: SemanticResourceCeilings,
        clean_projection_build_micros: u64,
        projection_input_bytes: u64,
        projection_batch_cache: Arc<SemanticEvaluationProjectionBatchCacheV1>,
        cancellation: Arc<dyn SemanticEvaluationCancellationV1>,
    ) -> Result<Self, SemanticRuntimeScheduleFailureV1> {
        let vector_generation = evaluation_vector_generation_id(&code, &prepared.prepared)?;
        let vector_bytes = prepared
            .prepared
            .vectors
            .iter()
            .try_fold(0_u64, |total, vector| {
                let bytes = u64::try_from(vector.values.len())
                    .ok()?
                    .checked_mul(std::mem::size_of::<f32>() as u64)?;
                total.checked_add(bytes)
            })
            .ok_or(SemanticRuntimeScheduleFailureV1::Projection)?;
        let source_manifest_digest = code.projection().request().changes.manifest_digest.clone();
        let source_generation = code.manifest().generation_id.clone();
        let sequence_length = prepared
            .prepared
            .embedding_key
            .embedding_key()
            .truncation_length;
        let resources = ProductionCandidateNativeGenerationResourcesV1 {
            source_generation: source_generation.clone(),
            source_manifest_digest: source_manifest_digest.clone(),
            incremental_source_generation: source_generation.clone(),
            incremental_source_manifest_digest: source_manifest_digest,
            vector_generation: Some(vector_generation.clone()),
            artifact_digest: Some(artifact_digest),
            model_bytes: artifact_bytes.model,
            tokenizer_bytes: artifact_bytes.tokenizer,
            threads: execution.max_threads,
            max_concurrent_sessions: execution.max_concurrent_sessions,
            batch_size: execution.max_batch_size,
            sequence_length,
            load_deadline_ms: execution.load_deadline_ms,
            // Vector preparation or the first genuine query opens the one
            // request-scoped runtime. `generation_resources` observes that
            // shared runtime's cold-load duration before evidence is accepted.
            cold_model_load_micros: 0,
            vector_bytes,
            index_bytes: 0,
            cache_bytes: 0,
            clean_projection_build_micros,
            incremental_rebuild_micros: 0,
            projection_cases: BTreeMap::new(),
        };
        let projection = prepared.prepared.embedding_key.clone();
        let search_index_key = SemanticSearchIndexProfileV1::exact_flat_v1()
            .and_then(|profile| profile.index_key())
            .map_err(SemanticRuntimeScheduleFailureV1::projection)?;
        let vectors = PublishedSemanticVectorReadPortV1::from_prepared(
            &prepared.prepared,
            vector_generation.clone(),
            search_index_key.clone(),
            &code,
        )
        .map_err(SemanticRuntimeScheduleFailureV1::projection)?;
        let secret = canonical_sha256(&(
            "tracedecay.semantic-evaluation-query-key.v1",
            code.manifest().generation_id.clone(),
            vector_generation.clone(),
        ))
        .map_err(|_| SemanticRuntimeScheduleFailureV1::Runtime)?
        .as_str()
        .as_bytes()
        .to_vec();
        let query_keys = RetrievalCursorKeyringV1::new(
            projection.privacy_domain().clone(),
            RetrievalCursorKeyId::new("semantic-evaluation.query-key.v1")
                .map_err(|_| SemanticRuntimeScheduleFailureV1::Runtime)?,
            projection.privacy_key_epoch(),
            secret,
            60_000_000,
        )
        .map_err(|_| SemanticRuntimeScheduleFailureV1::Runtime)?;
        Ok(Self {
            source_generation,
            projection,
            search_index_key,
            vector_generation,
            prepared_projection: prepared.prepared,
            projection_input_bytes,
            projection_batch_cache,
            capability_manifest_digest: code.capability().manifest_digest.clone(),
            code,
            cancellation,
            query_factory: prepared.query_factory,
            vectors,
            query_keys,
            resources,
        })
    }

    pub(crate) fn generation_resources(&self) -> ProductionCandidateNativeGenerationResourcesV1 {
        let mut resources = self.resources.clone();
        resources.cache_bytes = self.query_factory.resident_cache_bytes();
        resources.cold_model_load_micros = self.query_factory.cold_load_micros().unwrap_or(0);
        resources
    }

    pub fn projection(&self) -> &tracedecay_domain::AdmittedEmbeddingProjectionKeyV1 {
        &self.projection
    }

    pub fn query_factory(&self) -> &SemanticEvaluationQueryFactoryV1 {
        &self.query_factory
    }

    pub fn with_query_inputs(
        &self,
        context: ProductionCandidateNativeQueryContextV1<'_>,
        rerank_authority: Option<&ProductionCodeRerankAuthorityV1>,
        evaluate: &mut dyn for<'inputs> FnMut(
            ProductionCandidateNativeQueryInputsV1<'inputs>,
        ) -> Result<(), CandidateOutputError>,
    ) -> Result<(), CandidateOutputError> {
        if context.code_generation != &self.source_generation
            || context.code.manifest().generation_id != self.source_generation
        {
            return Err(CandidateOutputError::Contract(
                "native semantic evaluator generation changed".to_owned(),
            ));
        }
        let control = SemanticEvaluationExecutionControlV1 {
            started: std::time::Instant::now(),
            cancellation: Arc::clone(&self.cancellation),
        };
        let mut rerank_views =
            GenerationBoundCodeRerankViewsV1::new(context.code, context.query_view);
        let rerank = context
            .rerank_policy
            .zip(rerank_authority)
            .map(|(policy, authority)| {
                tracedecay_query::search_quality::semantic_native::SemanticNativeRerankInputV1 {
                    request: context.request,
                    policy,
                    views: &mut rerank_views as &mut _,
                    executor: authority.executor(),
                    control: &control,
                }
            });
        if context.profile.semantic_weight_ppm == 0 {
            return evaluate(ProductionCandidateNativeQueryInputsV1 {
                semantic: None,
                rerank,
            });
        }
        let query_digest = self
            .query_keys
            .digest_active_query(context.request, context.query_view)
            .map_err(|error| CandidateOutputError::Contract(error.to_string()))?;
        let request = SemanticRetrievalRequestV1 {
            base: context.request.clone(),
            query_digest,
            query_view: context.query_view,
            projection: &self.projection,
            search_index_key: &self.search_index_key,
            capability_manifest_digest: self.capability_manifest_digest.clone(),
            vector_generation: self.vector_generation.clone(),
            code_generation: self.source_generation.clone(),
            budget: context.request.budget,
        };
        request
            .validate()
            .map_err(|error| CandidateOutputError::Contract(error.to_string()))?;
        let embedder = self
            .query_factory
            .create(&control, request.budget.deadline_micros);
        let scoped_vectors = ScopedSemanticEvaluationVectorReadPortV1 {
            inner: &self.vectors,
            allowed_chunks: context.semantic_allowed_chunks,
        };
        let lane = SemanticCodeRetriever::new(&embedder, &scoped_vectors, &control);
        evaluate(ProductionCandidateNativeQueryInputsV1 {
            semantic: Some(
                tracedecay_query::search_quality::semantic_native::SemanticNativeSemanticInputV1 {
                    lane: &lane,
                    request: &request,
                },
            ),
            rerank,
        })
    }
}

pub(super) struct SemanticEvaluationExecutionControlV1 {
    pub(super) started: std::time::Instant,
    pub(super) cancellation: Arc<dyn SemanticEvaluationCancellationV1>,
}

impl RetrievalExecutionControl for SemanticEvaluationExecutionControlV1 {
    fn is_cancelled(&self) -> bool {
        self.cancellation.interruption().is_some()
    }

    fn elapsed_micros(&self) -> u64 {
        self.started.elapsed().as_micros().min(u128::from(u64::MAX)) as u64
    }
}
