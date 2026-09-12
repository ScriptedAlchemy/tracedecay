//! Coherence of the committed semantic source generation with the serving code generation.

use tracedecay_domain::{CodeGenerationId, CodeGenerationManifestV1, ManifestDigest};

use crate::store::vector_generations::PublishedVectorGenerationV1;

/// How a served vector generation's source binding was admitted.
///
/// Code-generation identifiers are physical publication identities: an
/// unrelated republication (a new commit sealing byte-identical trees, a
/// restart, configuration churn) mints a new identifier over the same source
/// truth. A semantic generation stays valid while the exact source content it
/// was evaluated from stays valid, so serving admits either the exact
/// publication it was projected from or a successor whose sealed chunk corpus
/// is proven byte-identical. Anything less fails closed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SemanticSourceCoherenceV1 {
    /// The vector generation's `source_generation` is the served code
    /// generation.
    ExactGeneration,
    /// The served code generation is a different publication whose full chunk
    /// corpus (chunk identity and content digest, one-to-one) equals the
    /// corpus the vectors were projected from.
    ProvenSourceContent,
}

/// The explicit answer to "may these vectors attach to this code generation":
/// either a coherence proof, or a typed mismatch that names both source
/// identities so the refusal is diagnosable without re-deriving either side.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SemanticSourceCoherenceOutcomeV1 {
    Coherent(SemanticSourceCoherenceV1),
    Mismatch(SemanticSourceMismatchV1),
    Unavailable(SemanticSourceUnavailableV1),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SemanticSourceUnavailableV1 {
    ServingCommitmentsMissing,
    VectorCommitmentInvalid,
}

/// A typed refusal: the identity the vectors were evaluated from and the
/// identity the serving code generation seals, side by side. Nothing attaches
/// silently on this arm; the caller reports both identities and keeps the
/// semantic lane typed-unavailable.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SemanticSourceMismatchV1 {
    /// The code generation the vectors were projected from.
    pub vector_source_generation: CodeGenerationId,
    /// The change-set manifest digest the vector generation recorded at
    /// projection time (its evaluated source identity).
    pub vector_source_manifest_digest: ManifestDigest,
    /// Generation-neutral full replay identity derived from the vector
    /// generation's independently authenticated accepted rows.
    pub vector_source_full_replay_digest: ManifestDigest,
    /// The code generation currently offered for serving.
    pub serving_generation: CodeGenerationId,
    /// The sealed generation transition, including generation watermarks and
    /// the added/deleted/reused partitions.
    pub serving_incremental_manifest_digest: ManifestDigest,
    /// The serving generation's sealed generation-neutral source identity.
    pub serving_source_full_replay_digest: ManifestDigest,
}

/// Decide whether a published vector generation may serve a (possibly newer)
/// sealed code generation, answering with the exact identities either way.
///
/// Both inputs are independently authenticated before they reach this
/// boundary. The vector record projects its accepted rows into the same
/// generation-neutral full-replay digest that the code manifest sealed once;
/// no decoded code generation or candidate-owned digest is copied onto the
/// serving side. Model, profile, and artifact identity are not decided here;
/// callers pin them separately.
pub fn semantic_source_coherence(
    vectors: &PublishedVectorGenerationV1,
    code: &CodeGenerationManifestV1,
) -> SemanticSourceCoherenceOutcomeV1 {
    let Some(serving) = code.source_commitments.as_ref() else {
        return SemanticSourceCoherenceOutcomeV1::Unavailable(
            SemanticSourceUnavailableV1::ServingCommitmentsMissing,
        );
    };
    let Ok(vector_source_full_replay_digest) = vectors.accepted_source_full_replay_digest() else {
        return SemanticSourceCoherenceOutcomeV1::Unavailable(
            SemanticSourceUnavailableV1::VectorCommitmentInvalid,
        );
    };
    if vector_source_full_replay_digest == serving.full_replay_digest {
        return SemanticSourceCoherenceOutcomeV1::Coherent(
            if vectors.source_generation() == &code.generation_id {
                SemanticSourceCoherenceV1::ExactGeneration
            } else {
                SemanticSourceCoherenceV1::ProvenSourceContent
            },
        );
    }
    SemanticSourceCoherenceOutcomeV1::Mismatch(SemanticSourceMismatchV1 {
        vector_source_generation: vectors.source_generation().clone(),
        vector_source_manifest_digest: vectors.source_manifest_digest().clone(),
        vector_source_full_replay_digest,
        serving_generation: code.generation_id.clone(),
        serving_incremental_manifest_digest: serving.incremental_manifest_digest.clone(),
        serving_source_full_replay_digest: serving.full_replay_digest.clone(),
    })
}

/// Content-only convenience over [`semantic_source_coherence`]: true exactly
/// when the vectors' evaluated corpus is byte-identical to the sealed corpus
/// of `code` (the exact-generation arm trivially satisfies this).
pub fn semantic_source_content_coherent(
    vectors: &PublishedVectorGenerationV1,
    code: &CodeGenerationManifestV1,
) -> bool {
    matches!(
        semantic_source_coherence(vectors, code),
        SemanticSourceCoherenceOutcomeV1::Coherent(_)
    )
}
