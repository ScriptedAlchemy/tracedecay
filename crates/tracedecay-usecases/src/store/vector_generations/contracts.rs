const VECTOR_GENERATION_BUILD_DIGEST_DOMAIN: &str = "tracedecay.vector-generation-build.v1";
const VECTOR_GENERATION_MANIFEST_DIGEST_DOMAIN: &str = "tracedecay.vector-generation-manifest.v1";
const PHYSICAL_VECTOR_REUSE_DIGEST_DOMAIN: &str = "tracedecay.physical-vector-reuse.v1";
const VECTOR_GENERATION_STATE_OPERATION: &str = "persist semantic vector generations";
#[cfg(test)]
const VECTOR_GENERATION_STATE_SCHEMA_V1: &str = "
CREATE TABLE IF NOT EXISTS semantic_vector_generation_state_v1 (
    singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
    revision INTEGER NOT NULL CHECK (revision >= 0),
    state_json TEXT NOT NULL
) STRICT;
";
const VECTOR_GENERATION_RECORD_SCHEMA_V1: &str = "
CREATE TABLE IF NOT EXISTS semantic_vector_generation_v1 (
    build_id TEXT PRIMARY KEY,
    revision INTEGER NOT NULL CHECK (revision >= 0),
    lifecycle TEXT NOT NULL CHECK (lifecycle IN ('staged', 'published')),
    generation_id TEXT UNIQUE,
    record_json TEXT NOT NULL,
    CHECK (
        (lifecycle = 'staged' AND generation_id IS NULL)
        OR (lifecycle = 'published' AND generation_id IS NOT NULL)
    )
) STRICT;
CREATE TABLE IF NOT EXISTS semantic_vector_active_generation_v1 (
    singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
    revision INTEGER NOT NULL CHECK (revision >= 0),
    shard_id_json TEXT NOT NULL,
    generation_id TEXT
) STRICT;
CREATE TABLE IF NOT EXISTS semantic_vector_payload_owner_v1 (
    build_id TEXT NOT NULL,
    output_digest TEXT NOT NULL,
    PRIMARY KEY (build_id, output_digest)
) STRICT;
CREATE INDEX IF NOT EXISTS semantic_vector_payload_owner_v1_address
    ON semantic_vector_payload_owner_v1 (output_digest);
CREATE TABLE IF NOT EXISTS semantic_vector_state_slice_owner_v1 (
    build_id TEXT NOT NULL,
    collection_digest TEXT NOT NULL,
    PRIMARY KEY (build_id, collection_digest)
) STRICT;
CREATE INDEX IF NOT EXISTS semantic_vector_state_slice_owner_v1_address
    ON semantic_vector_state_slice_owner_v1 (collection_digest);
CREATE TABLE IF NOT EXISTS semantic_vector_generation_retired_v1 (
    build_id TEXT PRIMARY KEY
) STRICT;
CREATE TABLE IF NOT EXISTS semantic_vector_orphan_resource_v1 (
    kind TEXT NOT NULL CHECK (kind IN ('payload', 'state_slice')),
    address TEXT NOT NULL,
    PRIMARY KEY (kind, address)
) STRICT;
";
/// Row-per-vector float storage for the production generation state.
///
/// The payload is content-addressed by `output_digest`, which the projector
/// derives from `(projection_key, chunk_id, chunk_digest, values)`. Two rows
/// with the same address therefore hold the same floats, and
/// [`ProjectedChunkVectorV1::validate`] re-derives that address on every load,
/// so a mis-bound payload fails closed instead of being served.
const VECTOR_PAYLOAD_SCHEMA_V1: &str = "
CREATE TABLE IF NOT EXISTS semantic_vector_payload_v1 (
    output_digest TEXT PRIMARY KEY,
    dimensions INTEGER NOT NULL CHECK (dimensions > 0),
    payload BLOB NOT NULL
) STRICT;
";
/// The evaluation lane keeps a separate payload table so that reclaiming
/// unreferenced production payloads can never delete evaluation rows.
const VECTOR_EVALUATION_PAYLOAD_SCHEMA_V1: &str = "
CREATE TABLE IF NOT EXISTS semantic_vector_evaluation_payload_v1 (
    output_digest TEXT PRIMARY KEY,
    dimensions INTEGER NOT NULL CHECK (dimensions > 0),
    payload BLOB NOT NULL
) STRICT;
";
/// Rows bound per statement when writing or reading payloads. Keeps every
/// statement inside the runtime's bound-parameter and materialization limits
/// so a whole-corpus generation moves as a sequence of bounded pages.
const VECTOR_PAYLOAD_STATEMENT_ROWS: usize = 256;
const VECTOR_PAYLOAD_TABLE_V1: &str = "semantic_vector_payload_v1";
const VECTOR_EVALUATION_PAYLOAD_TABLE_V1: &str = "semantic_vector_evaluation_payload_v1";
/// Slice storage for the state document's corpus-sized metadata.
///
/// Every collection that scales with the corpus — per-vector row metadata,
/// per-chunk projection receipts, the plan's expected chunk set, the staged
/// committed-effect set, and the prepared batches — is encoded once, addressed
/// by the SHA-256 of those bytes, and written as bounded slices. The state
/// document keeps only the address, so it stays generation-level regardless of
/// corpus size. Content addressing also means a staged collection and the
/// published collection it becomes share one stored copy, so publication
/// writes no new slices.
const VECTOR_STATE_SLICE_SCHEMA_V1: &str = "
CREATE TABLE IF NOT EXISTS semantic_vector_state_slice_v1 (
    collection_digest TEXT NOT NULL,
    ordinal INTEGER NOT NULL CHECK (ordinal >= 0),
    payload BLOB NOT NULL
) STRICT;
CREATE UNIQUE INDEX IF NOT EXISTS semantic_vector_state_slice_v1_address
    ON semantic_vector_state_slice_v1 (collection_digest, ordinal);
";
/// The evaluation lane keeps a separate slice table for the same reason it
/// keeps a separate payload table: reclaiming unreferenced production slices
/// can never delete evaluation rows.
const VECTOR_EVALUATION_STATE_SLICE_SCHEMA_V1: &str = "
CREATE TABLE IF NOT EXISTS semantic_vector_evaluation_state_slice_v1 (
    collection_digest TEXT NOT NULL,
    ordinal INTEGER NOT NULL CHECK (ordinal >= 0),
    payload BLOB NOT NULL
) STRICT;
CREATE UNIQUE INDEX IF NOT EXISTS semantic_vector_evaluation_state_slice_v1_address
    ON semantic_vector_evaluation_state_slice_v1 (collection_digest, ordinal);
";
const VECTOR_STATE_SLICE_TABLE_V1: &str = "semantic_vector_state_slice_v1";
const VECTOR_EVALUATION_STATE_SLICE_TABLE_V1: &str = "semantic_vector_evaluation_state_slice_v1";
/// Bytes per stored slice. One statement carries
/// `VECTOR_STATE_SLICE_STATEMENT_ROWS` of these, so the widest statement this
/// store issues stays near a megabyte no matter how large the collection is.
const VECTOR_STATE_SLICE_BYTES: usize = 32 * 1024;
/// Slices bound per statement.
const VECTOR_STATE_SLICE_STATEMENT_ROWS: usize = 32;
/// Slices read per statement. A single query may materialize neither more rows
/// nor more bytes than the runtime allows, and a whole-corpus collection
/// exceeds both, so reads page through the ordinals in bounded groups.
const VECTOR_STATE_SLICE_READ_ROWS: usize = 128;
/// Addresses resolved per read statement.
const VECTOR_STATE_ADDRESS_STATEMENT_ROWS: usize = 64;
const VECTOR_EVALUATION_STATE_SCHEMA_V1: &str = "
CREATE TABLE IF NOT EXISTS semantic_vector_evaluation_state_v1 (
    evaluation_id TEXT PRIMARY KEY,
    revision INTEGER NOT NULL CHECK (revision >= 0),
    state_json TEXT NOT NULL
) STRICT;
";
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
