const VECTOR_GENERATION_BUILD_DIGEST_DOMAIN: &str = "tracedecay.vector-generation-build.v1";
const VECTOR_GENERATION_MANIFEST_DIGEST_DOMAIN: &str = "tracedecay.vector-generation-manifest.v1";
const PHYSICAL_VECTOR_REUSE_DIGEST_DOMAIN: &str = "tracedecay.physical-vector-reuse.v1";
const VECTOR_GENERATION_STATE_OPERATION: &str = "persist semantic vector generations";
pub use tracedecay_runtime_core::store_runtime::schema::SEMANTIC_VECTOR_GRAPH_SCHEMA_V2;
/// Rows bound per statement when writing or reading payloads.
const VECTOR_PAYLOAD_STATEMENT_ROWS: usize = 256;
const VECTOR_PAYLOAD_TABLE_V1: &str = "semantic_vector_payload_v1";
const VECTOR_EVALUATION_PAYLOAD_TABLE_V1: &str = "semantic_vector_evaluation_payload_v1";
const VECTOR_STATE_SLICE_TABLE_V1: &str = "semantic_vector_state_slice_v1";
const VECTOR_EVALUATION_STATE_SLICE_TABLE_V1: &str = "semantic_vector_evaluation_state_slice_v1";
/// Bytes per stored slice.
const VECTOR_STATE_SLICE_BYTES: usize = 32 * 1024;
/// Slices bound per statement.
const VECTOR_STATE_SLICE_STATEMENT_ROWS: usize = 32;
/// Slices read per statement.
const VECTOR_STATE_SLICE_READ_ROWS: usize = 128;
/// Addresses resolved per read statement.
const VECTOR_STATE_ADDRESS_STATEMENT_ROWS: usize = 64;
const MAX_STATE_CAS_RETRIES: usize = 8;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(transparent)]
pub struct VectorGenerationBuildIdV1(ManifestDigest);

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct VectorGenerationPlanV1 {
    pub target_projection_key: ProjectionKeyV1,
    pub source_generation: CodeGenerationId,
    pub source_manifest_digest: ManifestDigest,
    /// The corpus-sized membership set. Externalized in the state document but
    /// serialized inline here, so the build-identity digest over this plan is
    /// unchanged by where the list is stored.
    pub expected_chunk_ids: ExternalV1<Vec<CodeSearchChunkId>>,
    pub base_generation: Option<VectorGenerationIdV1>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct VectorProjectionCheckpointV1 {
    pub target_projection_key: ProjectionKeyV1,
    pub source_generation: CodeGenerationId,
    pub source_manifest_digest: ManifestDigest,
    pub completed_batches: u64,
    pub last_request_digest: Option<ManifestDigest>,
    pub last_publication_digest: Option<ManifestDigest>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(deny_unknown_fields)]
struct PhysicalVectorReuseKeyV1 {
    canonical_chunk_digest: ContentDigest,
    projection_key: ProjectionKeyV1,
    admitted_embedding_key: AdmittedEmbeddingProjectionKeyV1,
    privacy_domain: tracedecay_domain::PrivacyDomainId,
    privacy_key_epoch: u64,
}
