//! Admitted local `FastEmbed` reranking over generation-bound code views.
//!
//! Artifact bytes are opened only through the digest-addressed artifact-store
//! capability. Query and chunk bytes remain request-local and are dropped
//! after [`BoundedRerankRuntimeV1`] returns.

use std::sync::{Arc, Mutex, TryLockError};

#[cfg(feature = "semantic-fastembed")]
use fastembed::{
    RerankInitOptionsUserDefined, RerankerModel, TextRerank, TokenizerFiles,
    UserDefinedRerankingModel,
};
use tracedecay_code_index::production::CodeIndexPublishedGenerationV1;
use tracedecay_domain::{
    AuthorizedRerankView, CodeSearchChunkId, CodeSearchChunkV1, EphemeralSanitizedQueryViewV1,
    FreshnessCompatibilityV1, ManifestDigest, RankedCandidate, RerankPolicy, RetrievalAnchorId,
    RetrievalRequest, SanitizedStageFailure, SymbolOccurrenceId, canonical_sha256,
};
use tracedecay_query::retrieval::rerank::{
    BoundedRerankOutcomeV1, BoundedRerankRuntimeV1, DeterministicLocalRerankExecutorV1,
    EphemeralRerankViewSourceV1, LocalRerankFailureV1, LocalRerankInputV1, LocalRerankPermitV1,
    RerankExecutionControlV1, RerankViewOutcomeV1, RerankViewPermitV1,
};

use super::artifact_store::AdmittedArtifactV1;
use super::manifest::{ArtifactMemberRoleV1, ArtifactProfileKindV1};
use crate::RerankCompatibilityPinsV1;
use tracedecay_query::retrieval::rerank::AdmittedNativeRerankExecutorV1;

pub const RERANK_IMPLEMENTATION_REVISION_V1: &str = "rerank.fastembed.production.v1";
pub const RERANK_RUNTIME_DIGEST_DOMAIN_V1: &str = "tracedecay.rerank-runtime-compatibility.v1";
const CODE_CHUNK_ANCHOR_PREFIX: &str = "code-chunk:";
const CODE_SYMBOL_ANCHOR_PREFIX: &str = "code-symbol:";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RerankArtifactAdmissionErrorV1 {
    IncompatiblePins,
    IncompatibleArtifact,
}

#[derive(Clone)]
struct AdmittedRerankArtifactV1 {
    #[cfg(feature = "semantic-fastembed")]
    artifact: AdmittedArtifactV1,
    pins: RerankCompatibilityPinsV1,
    max_batch_size: u32,
    #[cfg(feature = "semantic-fastembed")]
    max_sequence_length: u32,
    #[cfg(feature = "semantic-fastembed")]
    max_threads: u32,
    resident_byte_ceiling: u64,
}

impl AdmittedRerankArtifactV1 {
    fn admit(
        artifact: AdmittedArtifactV1,
        pins: RerankCompatibilityPinsV1,
    ) -> Result<Self, RerankArtifactAdmissionErrorV1> {
        let resources = {
            let manifest = artifact.manifest();
            let resources = validate_reranker_manifest_pins(manifest, &pins)?;
            if artifact.artifact_digest() != &manifest.artifact_identity_digest()
                || artifact.manifest_digest() != &manifest.canonical_digest()
            {
                return Err(RerankArtifactAdmissionErrorV1::IncompatiblePins);
            }
            resources
        };
        Ok(Self {
            #[cfg(feature = "semantic-fastembed")]
            artifact,
            pins,
            max_batch_size: resources.max_batch_size,
            #[cfg(feature = "semantic-fastembed")]
            max_sequence_length: resources.max_sequence_length,
            #[cfg(feature = "semantic-fastembed")]
            max_threads: resources.max_threads,
            resident_byte_ceiling: resources.max_resident_bytes,
        })
    }
}

pub fn validate_reranker_manifest_pins(
    manifest: &super::manifest::ModelArtifactManifestV1,
    pins: &RerankCompatibilityPinsV1,
) -> Result<super::manifest::ResourceCeilingV1, RerankArtifactAdmissionErrorV1> {
    let payload = &manifest.payload;
    manifest
        .validate()
        .map_err(|_| RerankArtifactAdmissionErrorV1::IncompatibleArtifact)?;
    let artifact_digest = ManifestDigest::new(format!(
        "sha256:{}",
        manifest.artifact_identity_digest().as_str()
    ))
    .map_err(|_| RerankArtifactAdmissionErrorV1::IncompatibleArtifact)?;
    let runtime_digest = canonical_sha256(&(
        RERANK_RUNTIME_DIGEST_DOMAIN_V1,
        &payload.runtime.runtime,
        &payload.runtime.build_revision,
        payload.device,
        payload.precision,
    ))
    .map_err(|_| RerankArtifactAdmissionErrorV1::IncompatibleArtifact)?;
    if payload.profile_kind != ArtifactProfileKindV1::Reranker
        || pins.implementation_revision.as_str() != RERANK_IMPLEMENTATION_REVISION_V1
        || pins.artifact_manifest_digest != artifact_digest
        || pins.runtime_compatibility_digest != runtime_digest
        || [
            ArtifactMemberRoleV1::Model,
            ArtifactMemberRoleV1::Tokenizer,
            ArtifactMemberRoleV1::Config,
            ArtifactMemberRoleV1::SpecialTokensMap,
            ArtifactMemberRoleV1::TokenizerConfig,
        ]
        .iter()
        .any(|role| manifest.package_member(*role).is_none())
    {
        return Err(RerankArtifactAdmissionErrorV1::IncompatiblePins);
    }
    #[cfg(feature = "semantic-fastembed")]
    supported_reranker_model(&payload.upstream.name, &payload.artifact_id)
        .ok_or(RerankArtifactAdmissionErrorV1::IncompatibleArtifact)?;
    Ok(payload.resource_ceiling)
}

/// Real, locally loaded rerank executor. One warmed session is retained under
/// a fail-fast mutex: concurrent demand returns typed unavailability instead
/// of waiting or opening an unbounded second model.
pub struct FastEmbedRerankExecutorV1 {
    authority: Arc<AdmittedRerankArtifactV1>,
    session: Mutex<Option<FastEmbedRerankSessionV1>>,
}

impl FastEmbedRerankExecutorV1 {
    fn new(authority: Arc<AdmittedRerankArtifactV1>) -> Self {
        Self {
            authority,
            session: Mutex::new(None),
        }
    }

    pub fn compatibility(&self) -> &RerankCompatibilityPinsV1 {
        &self.authority.pins
    }
}

#[cfg(feature = "semantic-fastembed")]
struct FastEmbedRerankSessionV1 {
    model: TextRerank,
}

#[cfg(not(feature = "semantic-fastembed"))]
struct FastEmbedRerankSessionV1;

impl DeterministicLocalRerankExecutorV1 for FastEmbedRerankExecutorV1 {
    fn planned_model_invocations(&self, candidate_count: u32) -> Result<u32, LocalRerankFailureV1> {
        if candidate_count == 0 {
            return Ok(0);
        }
        Ok(candidate_count.div_ceil(self.authority.max_batch_size))
    }

    fn rerank(
        &self,
        _policy: &RerankPolicy,
        inputs: &[LocalRerankInputV1<'_>],
        permit: LocalRerankPermitV1,
    ) -> Result<Vec<RetrievalAnchorId>, LocalRerankFailureV1> {
        let (query, documents) = decode_views(inputs)?;
        let expected_invocations = u32::try_from(inputs.len())
            .unwrap_or(u32::MAX)
            .div_ceil(self.authority.max_batch_size);
        if permit.model_invocations != expected_invocations
            || permit.input_bytes > self.authority.resident_byte_ceiling
        {
            return Err(LocalRerankFailureV1::Rejected(
                SanitizedStageFailure::Incompatible,
            ));
        }
        let mut session = match self.session.try_lock() {
            Ok(session) => session,
            Err(TryLockError::WouldBlock) => {
                return Err(LocalRerankFailureV1::Unavailable(
                    SanitizedStageFailure::AuthorityUnavailable,
                ));
            }
            Err(TryLockError::Poisoned(error)) => error.into_inner(),
        };
        if session.is_none() {
            *session = Some(open_session(&self.authority)?);
        }
        run_session(
            session
                .as_mut()
                .unwrap_or_else(|| panic!("rerank session initialized above")),
            query,
            &documents,
            inputs,
            self.authority.max_batch_size,
        )
    }
}

impl AdmittedNativeRerankExecutorV1 for FastEmbedRerankExecutorV1 {
    fn artifact_manifest_digest(&self) -> &ManifestDigest {
        &self.authority.pins.artifact_manifest_digest
    }
}

#[cfg(feature = "semantic-fastembed")]
fn open_session(
    authority: &AdmittedRerankArtifactV1,
) -> Result<FastEmbedRerankSessionV1, LocalRerankFailureV1> {
    let payload = &authority.artifact.manifest().payload;
    let _model_pin = supported_reranker_model(&payload.upstream.name, &payload.artifact_id).ok_or(
        LocalRerankFailureV1::Rejected(SanitizedStageFailure::Incompatible),
    )?;
    let tokenizer_files = TokenizerFiles {
        tokenizer_file: member_bytes(authority, ArtifactMemberRoleV1::Tokenizer)?,
        config_file: member_bytes(authority, ArtifactMemberRoleV1::Config)?,
        special_tokens_map_file: member_bytes(authority, ArtifactMemberRoleV1::SpecialTokensMap)?,
        tokenizer_config_file: member_bytes(authority, ArtifactMemberRoleV1::TokenizerConfig)?,
    };
    let model = UserDefinedRerankingModel::new(
        member_bytes(authority, ArtifactMemberRoleV1::Model)?,
        tokenizer_files,
    );
    let options = RerankInitOptionsUserDefined::new()
        .with_max_length(authority.max_sequence_length as usize)
        .with_intra_threads(authority.max_threads as usize);
    TextRerank::try_new_from_user_defined(model, options)
        .map(|model| FastEmbedRerankSessionV1 { model })
        .map_err(|_| LocalRerankFailureV1::Unavailable(SanitizedStageFailure::AuthorityUnavailable))
}

#[cfg(not(feature = "semantic-fastembed"))]
fn open_session(
    _authority: &AdmittedRerankArtifactV1,
) -> Result<FastEmbedRerankSessionV1, LocalRerankFailureV1> {
    Err(LocalRerankFailureV1::Unavailable(
        SanitizedStageFailure::AuthorityUnavailable,
    ))
}

#[cfg(feature = "semantic-fastembed")]
fn member_bytes(
    authority: &AdmittedRerankArtifactV1,
    role: ArtifactMemberRoleV1,
) -> Result<Vec<u8>, LocalRerankFailureV1> {
    authority
        .artifact
        .read_member_bytes(role)
        .map_err(|_| LocalRerankFailureV1::Unavailable(SanitizedStageFailure::AuthorityUnavailable))
}

#[cfg(feature = "semantic-fastembed")]
fn run_session(
    session: &mut FastEmbedRerankSessionV1,
    query: &str,
    documents: &[&str],
    inputs: &[LocalRerankInputV1<'_>],
    batch_size: u32,
) -> Result<Vec<RetrievalAnchorId>, LocalRerankFailureV1> {
    let results = session
        .model
        .rerank(query, documents, false, Some(batch_size as usize))
        .map_err(|_| LocalRerankFailureV1::Unavailable(SanitizedStageFailure::Internal))?;
    if results.len() != inputs.len()
        || results
            .iter()
            .any(|result| result.index >= inputs.len() || !result.score.is_finite())
    {
        return Err(LocalRerankFailureV1::Rejected(
            SanitizedStageFailure::Invalid,
        ));
    }
    let mut scores = vec![None; inputs.len()];
    for result in results {
        if scores[result.index].replace(result.score).is_some() {
            return Err(LocalRerankFailureV1::Rejected(
                SanitizedStageFailure::Invalid,
            ));
        }
    }
    let mut order = (0..inputs.len()).collect::<Vec<_>>();
    order.sort_by(|left, right| {
        scores[*right]
            .unwrap_or(f32::NEG_INFINITY)
            .total_cmp(&scores[*left].unwrap_or(f32::NEG_INFINITY))
            .then_with(|| {
                inputs[*left]
                    .candidate
                    .candidate
                    .anchor_id
                    .cmp(&inputs[*right].candidate.candidate.anchor_id)
            })
    });
    Ok(order
        .into_iter()
        .map(|index| inputs[index].candidate.candidate.anchor_id.clone())
        .collect())
}

#[cfg(not(feature = "semantic-fastembed"))]
fn run_session(
    _session: &mut FastEmbedRerankSessionV1,
    _query: &str,
    _documents: &[&str],
    _inputs: &[LocalRerankInputV1<'_>],
    _batch_size: u32,
) -> Result<Vec<RetrievalAnchorId>, LocalRerankFailureV1> {
    Err(LocalRerankFailureV1::Unavailable(
        SanitizedStageFailure::AuthorityUnavailable,
    ))
}

#[cfg(feature = "semantic-fastembed")]
fn supported_reranker_model(upstream: &str, artifact_id: &str) -> Option<RerankerModel> {
    match [upstream, artifact_id] {
        values if values.contains(&"BAAI/bge-reranker-base") => {
            Some(RerankerModel::BGERerankerBase)
        }
        values if values.contains(&"rozgo/bge-reranker-v2-m3") => {
            Some(RerankerModel::BGERerankerV2M3)
        }
        values if values.contains(&"jinaai/jina-reranker-v1-turbo-en") => {
            Some(RerankerModel::JINARerankerV1TurboEn)
        }
        values if values.contains(&"jinaai/jina-reranker-v2-base-multilingual") => {
            Some(RerankerModel::JINARerankerV2BaseMultiligual)
        }
        _ => None,
    }
}

/// Request-local source authority over one immutable code generation.
pub struct GenerationBoundCodeRerankViewsV1<'a> {
    generation: &'a CodeIndexPublishedGenerationV1,
    query: &'a EphemeralSanitizedQueryViewV1,
}

impl<'a> GenerationBoundCodeRerankViewsV1<'a> {
    pub fn new(
        generation: &'a CodeIndexPublishedGenerationV1,
        query: &'a EphemeralSanitizedQueryViewV1,
    ) -> Self {
        Self { generation, query }
    }
}

impl EphemeralRerankViewSourceV1 for GenerationBoundCodeRerankViewsV1<'_> {
    fn authorize_ephemeral_view(
        &mut self,
        request: &RetrievalRequest,
        candidate: &RankedCandidate,
        permit: &RerankViewPermitV1,
    ) -> RerankViewOutcomeV1 {
        let snapshot = self.generation.snapshot();
        if request.scope.privacy_domain != self.generation.manifest().privacy_domain
            || request.scope.root.repository != snapshot.repository
            || request.scope.root.worktree != snapshot.worktree
            || request.scope.root.reference != snapshot.reference
            || permit.expected_privacy_domain != request.scope.privacy_domain
        {
            return RerankViewOutcomeV1::Denied;
        }
        let Some(chunk) =
            resolve_generation_chunk(self.generation, candidate.candidate.anchor_id.as_str())
        else {
            return RerankViewOutcomeV1::Missing;
        };
        if chunk.anchor.generation_id != self.generation.manifest().generation_id {
            return RerankViewOutcomeV1::Unavailable(SanitizedStageFailure::Stale);
        }
        let approved_features = encode_view(self.query.as_bytes(), chunk.sanitized_text.as_str());
        let input_bytes = approved_features.len() as u64;
        if input_bytes > permit.remaining_input_bytes {
            return RerankViewOutcomeV1::Unavailable(SanitizedStageFailure::AuthorityUnavailable);
        }
        RerankViewOutcomeV1::Authorized {
            view: AuthorizedRerankView {
                anchor_id: candidate.candidate.anchor_id.clone(),
                snapshot_digest: permit.expected_snapshot_digest.clone(),
                privacy_domain: request.scope.privacy_domain.clone(),
                compatibility: FreshnessCompatibilityV1::Current,
                approved_features,
            },
            // UTF-8 bytes are a conservative upper bound on tokenizer output.
            input_tokens: input_bytes,
            work_units: 1,
        }
    }
}

pub fn resolve_generation_chunk<'a>(
    generation: &'a CodeIndexPublishedGenerationV1,
    anchor: &str,
) -> Option<&'a CodeSearchChunkV1> {
    if let Some(raw_chunk_id) = anchor.strip_prefix(CODE_CHUNK_ANCHOR_PREFIX) {
        let chunk_id = CodeSearchChunkId::new(raw_chunk_id).ok()?;
        return generation.chunks().chunk(&chunk_id);
    }
    let raw_symbol = anchor.strip_prefix(CODE_SYMBOL_ANCHOR_PREFIX)?;
    let symbol = SymbolOccurrenceId::new(raw_symbol).ok()?;
    if generation.symbols().generation_id != generation.manifest().generation_id
        || generation
            .symbols()
            .symbols
            .binary_search_by(|record| record.occurrence.cmp(&symbol))
            .is_err()
    {
        return None;
    }
    // The generation chunk manifest is canonically ordered by typed chunk
    // identity. Selecting its first exact symbol binding matches the graph
    // projection's canonical representative without reparsing mutable files.
    generation.chunks().chunks().iter().find(|chunk| {
        chunk.anchor.generation_id == generation.manifest().generation_id
            && chunk.anchor.symbol_occurrence_id.as_ref() == Some(&symbol)
    })
}

fn encode_view(query: &[u8], document: &str) -> Vec<u8> {
    let query_len = u32::try_from(query.len()).unwrap_or(u32::MAX);
    let mut bytes = Vec::with_capacity(4 + query.len() + document.len());
    bytes.extend_from_slice(&query_len.to_be_bytes());
    bytes.extend_from_slice(query);
    bytes.extend_from_slice(document.as_bytes());
    bytes
}

fn decode_views<'a>(
    inputs: &'a [LocalRerankInputV1<'a>],
) -> Result<(&'a str, Vec<&'a str>), LocalRerankFailureV1> {
    let mut query = None;
    let mut documents = Vec::with_capacity(inputs.len());
    for input in inputs {
        let bytes = &input.view.approved_features;
        let prefix: [u8; 4] = bytes
            .get(..4)
            .and_then(|prefix| prefix.try_into().ok())
            .ok_or(LocalRerankFailureV1::Rejected(
                SanitizedStageFailure::Invalid,
            ))?;
        let query_end = 4_usize.saturating_add(u32::from_be_bytes(prefix) as usize);
        let current_query = std::str::from_utf8(bytes.get(4..query_end).ok_or(
            LocalRerankFailureV1::Rejected(SanitizedStageFailure::Invalid),
        )?)
        .map_err(|_| LocalRerankFailureV1::Rejected(SanitizedStageFailure::Invalid))?;
        let document = std::str::from_utf8(bytes.get(query_end..).ok_or(
            LocalRerankFailureV1::Rejected(SanitizedStageFailure::Invalid),
        )?)
        .map_err(|_| LocalRerankFailureV1::Rejected(SanitizedStageFailure::Invalid))?;
        if query.is_some_and(|query| query != current_query) {
            return Err(LocalRerankFailureV1::Rejected(
                SanitizedStageFailure::Incompatible,
            ));
        }
        query = Some(current_query);
        documents.push(document);
    }
    query
        .map(|query| (query, documents))
        .ok_or(LocalRerankFailureV1::Rejected(
            SanitizedStageFailure::Invalid,
        ))
}

/// Mounted production authority: exact compatibility pins plus the admitted
/// executor that [`BoundedRerankRuntimeV1`] invokes after fusion.
pub trait MountedRerankExecutorV1: AdmittedNativeRerankExecutorV1 + Send + Sync {}

impl<T> MountedRerankExecutorV1 for T where T: AdmittedNativeRerankExecutorV1 + Send + Sync {}

#[derive(Clone)]
pub struct ProductionCodeRerankAuthorityV1 {
    pins: RerankCompatibilityPinsV1,
    executor: Arc<dyn MountedRerankExecutorV1>,
}

impl ProductionCodeRerankAuthorityV1 {
    pub fn from_admitted(
        artifact: AdmittedArtifactV1,
        pins: RerankCompatibilityPinsV1,
    ) -> Result<Self, RerankArtifactAdmissionErrorV1> {
        let authority = Arc::new(AdmittedRerankArtifactV1::admit(artifact, pins.clone())?);
        Ok(Self {
            pins,
            executor: Arc::new(FastEmbedRerankExecutorV1::new(authority)),
        })
    }

    #[cfg(test)]
    pub fn from_executor_for_tests(
        pins: RerankCompatibilityPinsV1,
        executor: Arc<dyn MountedRerankExecutorV1>,
    ) -> Self {
        Self { pins, executor }
    }

    pub fn compatibility(&self) -> &RerankCompatibilityPinsV1 {
        &self.pins
    }

    pub fn executor(&self) -> &dyn AdmittedNativeRerankExecutorV1 {
        self.executor.as_ref()
    }

    pub fn execute(
        &self,
        generation: &CodeIndexPublishedGenerationV1,
        query: &EphemeralSanitizedQueryViewV1,
        request: &RetrievalRequest,
        policy: &RerankPolicy,
        pre_rerank: &[RankedCandidate],
        control: &dyn RerankExecutionControlV1,
    ) -> BoundedRerankOutcomeV1 {
        let mut views = GenerationBoundCodeRerankViewsV1::new(generation, query);
        BoundedRerankRuntimeV1::new(&mut views, self.executor.as_ref())
            .rerank(request, policy, pre_rerank, control)
    }
}
