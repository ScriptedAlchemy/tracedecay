//! Immutable semantic vector-generation storage.
//!
//! The deterministic state machine is retained as a test oracle. Production
//! persistence keeps relational generation manifests and receipts in the
//! already-open project database while the injected Grafeo authority owns
//! every vector payload and vector index.
#![forbid(unsafe_code)]

use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{Arc, Mutex, Weak},
};

use serde::{Deserialize, Serialize};
use thiserror::Error;
use tracedecay_domain::{
    AdmittedEmbeddingProjectionKeyV1, CodeGenerationId, CodeSearchChunkId, ContentDigest,
    ManifestDigest, ProjectionBatchReceiptV1, ProjectionKeyV1, ProjectionKindV1,
    ProjectionOperationV1, ProjectionOutcomeV1, canonical_sha256,
};
use tracedecay_graph_db::{
    GraphDb, GraphDbError, GraphNamespace, GraphProjectionId, GraphProjectionTelemetryRequest,
    GraphPropertyName, GraphVectorIndexRequest, GraphWatermark, NeverCancelled, SourceGeneration,
};

pub use tracedecay_domain::VectorGenerationIdV1;

use tracedecay_code_index::projection::{expected_publication_digest, verify_batch_receipt};
use tracedecay_runtime_core::db::{Database, engine::params};
use tracedecay_semantic::projector::{
    PreparedVectorGenerationV1, ProjectedChunkVectorV1, SemanticProjectionErrorV1,
};

mod grafeo;
use grafeo::{
    SEMANTIC_VECTOR_NAMESPACE, batch_watermark, build_projection_id, graph_chunk_count,
    graph_vector_metric, prepared_delta_publications, retire_projection, verify_projection,
};
pub(crate) use grafeo::{graph_projection_id, graph_vector_entity_id};

const VECTOR_GENERATION_BUILD_DIGEST_DOMAIN: &str = "tracedecay.vector-generation-build.v1";
const VECTOR_GENERATION_MANIFEST_DIGEST_DOMAIN: &str = "tracedecay.vector-generation-manifest.v1";
const VECTOR_GENERATION_PUBLICATION_INTENT_DIGEST_DOMAIN: &str =
    "tracedecay.vector-generation-publication-intent.v1";
const PHYSICAL_VECTOR_REUSE_DIGEST_DOMAIN: &str = "tracedecay.physical-vector-reuse.v1";
const VECTOR_GENERATION_STATE_OPERATION: &str = "persist semantic vector generations";
const VECTOR_GENERATION_STATE_SCHEMA_V1: &str = "
CREATE TABLE IF NOT EXISTS semantic_vector_generation_state (
    singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
    revision INTEGER NOT NULL CHECK (revision >= 0),
    state_json TEXT NOT NULL
) STRICT;
CREATE TABLE IF NOT EXISTS semantic_vector_publication_intent (
    intent_digest TEXT PRIMARY KEY,
    generation_id TEXT NOT NULL,
    build_id TEXT NOT NULL,
    expected_state_revision INTEGER NOT NULL CHECK (expected_state_revision >= 0),
    state_json TEXT NOT NULL,
    publication_json TEXT NOT NULL
) STRICT;
CREATE TABLE IF NOT EXISTS semantic_vector_batch_intent (
    intent_digest TEXT PRIMARY KEY,
    build_id TEXT NOT NULL,
    request_digest TEXT NOT NULL,
    expected_state_revision INTEGER NOT NULL CHECK (expected_state_revision >= 0),
    expected_graph_watermark TEXT,
    state_json TEXT NOT NULL,
    total_graph_chunks INTEGER NOT NULL CHECK (total_graph_chunks > 0),
    published_graph_chunks INTEGER NOT NULL DEFAULT 0
        CHECK (published_graph_chunks >= 0 AND published_graph_chunks <= total_graph_chunks),
    UNIQUE (build_id, request_digest),
    UNIQUE (build_id, expected_state_revision)
) STRICT;
";
/// Slice storage for the state document's corpus-sized metadata.
///
/// Every collection that scales with the corpus — per-vector row metadata,
/// per-chunk projection receipts, the plan's expected chunk set, the pending
/// committed-effect set, the prepared batches, and the physical-byte bindings
/// — is encoded once, addressed by the SHA-256 of those bytes, and written as
/// bounded slices. The state document keeps only the address, so it stays
/// generation-level regardless of corpus size. Content addressing also means
/// a pending collection and the published collection it becomes share one
/// stored copy, so publication writes no new slices.
const VECTOR_STATE_SLICE_SCHEMA_V1: &str = "
CREATE TABLE IF NOT EXISTS semantic_vector_state_slice (
    collection_digest TEXT NOT NULL,
    ordinal INTEGER NOT NULL CHECK (ordinal >= 0),
    payload BLOB NOT NULL
) STRICT;
CREATE UNIQUE INDEX IF NOT EXISTS semantic_vector_state_slice_address
    ON semantic_vector_state_slice (collection_digest, ordinal);
";
/// The evaluation lane keeps a separate slice table so reclaiming
/// unreferenced production metadata can never delete evaluation rows.
const VECTOR_EVALUATION_STATE_SLICE_SCHEMA_V1: &str = "
CREATE TABLE IF NOT EXISTS semantic_vector_evaluation_state_slice (
    collection_digest TEXT NOT NULL,
    ordinal INTEGER NOT NULL CHECK (ordinal >= 0),
    payload BLOB NOT NULL
) STRICT;
CREATE UNIQUE INDEX IF NOT EXISTS semantic_vector_evaluation_state_slice_address
    ON semantic_vector_evaluation_state_slice (collection_digest, ordinal);
";
const VECTOR_STATE_SLICE_TABLE_V1: &str = "semantic_vector_state_slice";
const VECTOR_EVALUATION_STATE_SLICE_TABLE_V1: &str = "semantic_vector_evaluation_state_slice";
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
CREATE TABLE IF NOT EXISTS semantic_vector_evaluation_state (
    evaluation_id TEXT PRIMARY KEY,
    revision INTEGER NOT NULL CHECK (revision >= 0),
    state_json TEXT NOT NULL
) STRICT;
";
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
    pub expected_chunk_ids: ExternalCollection<Vec<CodeSearchChunkId>>,
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

#[derive(Clone, Debug, PartialEq)]
struct SharedVectorBytesV1(Arc<[f32]>);

impl Serialize for SharedVectorBytesV1 {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        self.0.as_ref().serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for SharedVectorBytesV1 {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        Vec::<f32>::deserialize(deserializer).map(|values| Self(Arc::from(values)))
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
struct PhysicalVectorPayloadV1 {
    reuse_key: PhysicalVectorReuseKeyV1,
    values: SharedVectorBytesV1,
}

type PhysicalVectorPoolMapV1 = BTreeMap<PhysicalVectorReuseKeyV1, Weak<[f32]>>;

/// Sweep dead weak handles out of the pool once this many interns have
/// happened since the last sweep. A generation retire drops the strong
/// handles, so a sweep is what turns that retire into released memory.
const PHYSICAL_VECTOR_POOL_SWEEP_INTERVAL: usize = 4_096;

/// Hard ceiling on retained pool keys. Reaching it after a sweep means live
/// interned identities alone exceed the budget, so the pool is dropped
/// wholesale: interning stays correct without it — the next `intern` simply
/// allocates instead of sharing — and RSS is bounded by construction.
const PHYSICAL_VECTOR_POOL_MAX_ENTRIES: usize = 262_144;

#[derive(Default)]
struct PhysicalVectorPoolStateV1 {
    entries: PhysicalVectorPoolMapV1,
    interns_since_sweep: usize,
}

impl PhysicalVectorPoolStateV1 {
    fn sweep(&mut self) {
        self.entries.retain(|_, shared| shared.strong_count() > 0);
        self.interns_since_sweep = 0;
        if self.entries.len() > PHYSICAL_VECTOR_POOL_MAX_ENTRIES {
            self.entries.clear();
        }
    }
}

/// Process-wide physical byte interner. Complete projection and privacy
/// authority is part of the key, so sharing cannot cross either boundary.
///
/// Entries are weak handles, so retiring a generation already releases the
/// float payload; what used to leak was the *key* set, which grew for the
/// lifetime of the process across every project in the daemon. The pool now
/// sweeps dead entries on a fixed intern cadence and caps the live key set, so
/// a retired generation releases both its bytes and its keys.
#[derive(Clone)]
pub struct PhysicalVectorBytePoolV1 {
    entries: Arc<Mutex<PhysicalVectorPoolStateV1>>,
}

impl Default for PhysicalVectorBytePoolV1 {
    fn default() -> Self {
        static ENTRIES: std::sync::OnceLock<Arc<Mutex<PhysicalVectorPoolStateV1>>> =
            std::sync::OnceLock::new();
        Self {
            entries: Arc::clone(
                ENTRIES.get_or_init(|| Arc::new(Mutex::new(PhysicalVectorPoolStateV1::default()))),
            ),
        }
    }
}

impl PhysicalVectorBytePoolV1 {
    fn lock(
        &self,
    ) -> Result<std::sync::MutexGuard<'_, PhysicalVectorPoolStateV1>, VectorGenerationStoreErrorV1>
    {
        self.entries.lock().map_err(|_| {
            VectorGenerationStoreErrorV1::Storage(
                "physical vector byte pool lock is poisoned".to_string(),
            )
        })
    }

    fn intern(
        &self,
        reuse_key: &PhysicalVectorReuseKeyV1,
        values: &[f32],
    ) -> Result<Arc<[f32]>, VectorGenerationStoreErrorV1> {
        let mut pool = self.lock()?;
        if let Some(shared) = pool.entries.get(reuse_key).and_then(Weak::upgrade) {
            if shared.as_ref() != values {
                return Err(VectorGenerationStoreErrorV1::PhysicalVectorConflict);
            }
            return Ok(shared);
        }
        let shared: Arc<[f32]> = Arc::from(values.to_vec());
        pool.entries
            .insert(reuse_key.clone(), Arc::downgrade(&shared));
        pool.interns_since_sweep += 1;
        if pool.interns_since_sweep >= PHYSICAL_VECTOR_POOL_SWEEP_INTERVAL {
            pool.sweep();
        }
        Ok(shared)
    }

    /// Release every entry whose generation has been retired. Interning is
    /// unaffected: a swept key is re-interned on its next use.
    pub fn sweep_retired(&self) -> Result<(), VectorGenerationStoreErrorV1> {
        self.lock()?.sweep();
        Ok(())
    }

    /// Number of retained keys, live or not. Used by the eviction test.
    #[cfg(test)]
    pub(crate) fn retained_entries(&self) -> usize {
        self.entries
            .lock()
            .map(|pool| pool.entries.len())
            .unwrap_or_default()
    }
}

/// Serde adapters that keep projected float payloads out of the canonical
/// state document.
///
/// Only the store's own on-disk encoding changes. Every digest in this module's
/// domain — `output_digest`, `chunk_digest`, the generation manifest digest,
/// batch publication digests — is produced by the projector from domain values,
/// never from this encoding, so an externalized state and an inline state
/// describe byte-identical generation identities.
///
/// A state-document collection whose bytes live in the slice table.
///
/// The plain `Serialize`/`Deserialize` impls are **transparent**: a digest
/// computed over a value containing one of these is byte-identical to the
/// digest over the bare inner collection. That is what lets the store move a
/// corpus-sized field out of the document without moving any identity — the
/// build-identity digest over [`VectorGenerationPlanV1`] still hashes the full
/// expected chunk list. The state document persists these through the
/// [`external_state`] adapters instead, which write only the content address
/// and leave the bytes to the load/seal walk.
///
/// `DerefMut` clears the address, so mutating a collection always forces the
/// next seal to re-encode and re-address it. A stale address is therefore not
/// representable.
#[derive(Clone, Debug, Default)]
pub struct ExternalCollection<T> {
    /// Content address of the sealed bytes; `None` while unsealed.
    address: Option<ContentDigest>,
    value: T,
}

impl<T> ExternalCollection<T> {
    fn new(value: T) -> Self {
        Self {
            address: None,
            value,
        }
    }

    fn into_inner(self) -> T {
        self.value
    }

    /// Mutable access that keeps the address intact.
    ///
    /// Only for edits the externalized encoding cannot observe: filling the
    /// elided float payload back into a hydrated vector row leaves the stored
    /// bytes byte-identical, so re-addressing it would be pure waste. Any edit
    /// that changes the encoded form must go through `DerefMut` instead.
    fn elided_mut(&mut self) -> &mut T {
        &mut self.value
    }
}

impl<T> From<T> for ExternalCollection<T> {
    fn from(value: T) -> Self {
        Self::new(value)
    }
}

impl<Element, T: FromIterator<Element>> FromIterator<Element> for ExternalCollection<T> {
    fn from_iter<I: IntoIterator<Item = Element>>(iterator: I) -> Self {
        Self::new(T::from_iter(iterator))
    }
}

impl<T> std::ops::Deref for ExternalCollection<T> {
    type Target = T;

    fn deref(&self) -> &T {
        &self.value
    }
}

impl<T> std::ops::DerefMut for ExternalCollection<T> {
    fn deref_mut(&mut self) -> &mut T {
        self.address = None;
        &mut self.value
    }
}

impl<T: PartialEq> PartialEq for ExternalCollection<T> {
    /// Identity is the collection, never where its bytes happen to live.
    fn eq(&self, other: &Self) -> bool {
        self.value == other.value
    }
}

impl<T: Eq> Eq for ExternalCollection<T> {}

impl<T: Serialize> Serialize for ExternalCollection<T> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        self.value.serialize(serializer)
    }
}

impl<'de, T: Deserialize<'de>> Deserialize<'de> for ExternalCollection<T> {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        T::deserialize(deserializer).map(Self::new)
    }
}

/// One externalized collection, seen by the load/seal walk without knowing
/// which collection it is.
///
/// One sealed collection: its content address and the bounded slices its bytes
/// were cut into.
type SealedCollection = (ContentDigest, Vec<Vec<u8>>);

trait ExternalSlot {
    /// The address this slot's bytes are stored under, if it is sealed.
    fn address(&self) -> Option<&ContentDigest>;

    /// Seal the slot and hand back the bytes to write.
    ///
    /// Returns `None` when the slot is already sealed and `needed` reports its
    /// address as durable. Re-encoding is otherwise unconditional, so a slot
    /// whose bytes are not known to be durable is always written rather than
    /// assumed present.
    fn seal(
        &mut self,
        needed: &mut dyn FnMut(&ContentDigest) -> bool,
    ) -> Result<Option<SealedCollection>, VectorGenerationStoreErrorV1>;

    /// Fill the slot from the ordered slices stored at its address.
    fn fill(&mut self, slices: &[Vec<u8>]) -> Result<(), VectorGenerationStoreErrorV1>;
}

impl<T> ExternalSlot for ExternalCollection<T>
where
    T: Serialize + serde::de::DeserializeOwned,
{
    fn address(&self) -> Option<&ContentDigest> {
        self.address.as_ref()
    }

    fn seal(
        &mut self,
        needed: &mut dyn FnMut(&ContentDigest) -> bool,
    ) -> Result<Option<SealedCollection>, VectorGenerationStoreErrorV1> {
        if let Some(address) = &self.address
            && !needed(address)
        {
            return Ok(None);
        }
        let bytes = serde_json::to_vec(&self.value).map_err(storage_error)?;
        let address = ContentDigest::of_bytes(&bytes);
        self.address = Some(address.clone());
        if !needed(&address) {
            return Ok(None);
        }
        let slices = bytes
            .chunks(VECTOR_STATE_SLICE_BYTES)
            .map(<[u8]>::to_vec)
            .collect::<Vec<_>>();
        Ok(Some((address, slices)))
    }

    fn fill(&mut self, slices: &[Vec<u8>]) -> Result<(), VectorGenerationStoreErrorV1> {
        let address = self.address.as_ref().ok_or_else(|| {
            VectorGenerationStoreErrorV1::Storage(
                "externalized state slot was filled without an address".to_owned(),
            )
        })?;
        let bytes = slices.concat();
        // Content addressing is the integrity gate: bytes that do not hash to
        // the address the document named are refused rather than parsed.
        if &ContentDigest::of_bytes(&bytes) != address {
            return Err(VectorGenerationStoreErrorV1::Storage(format!(
                "externalized state collection {address} does not match its stored bytes"
            )));
        }
        self.value = serde_json::from_slice(&bytes).map_err(storage_error)?;
        Ok(())
    }
}

/// Per-vector row metadata for one generation, with the float payload elided.
///
/// The floats live only in Grafeo; this carries row identity and content
/// digests, and is itself externalized so the state document never renders one
/// row per chunk.
#[derive(Clone, Debug, Default, PartialEq)]
struct VectorRowMapV1(BTreeMap<CodeSearchChunkId, ProjectedChunkVectorV1>);

impl std::ops::Deref for VectorRowMapV1 {
    type Target = BTreeMap<CodeSearchChunkId, ProjectedChunkVectorV1>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl std::ops::DerefMut for VectorRowMapV1 {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

impl From<BTreeMap<CodeSearchChunkId, ProjectedChunkVectorV1>> for VectorRowMapV1 {
    fn from(value: BTreeMap<CodeSearchChunkId, ProjectedChunkVectorV1>) -> Self {
        Self(value)
    }
}

impl From<BTreeMap<CodeSearchChunkId, ProjectedChunkVectorV1>>
    for ExternalCollection<VectorRowMapV1>
{
    fn from(value: BTreeMap<CodeSearchChunkId, ProjectedChunkVectorV1>) -> Self {
        Self::new(VectorRowMapV1(value))
    }
}

impl Serialize for VectorRowMapV1 {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        externalized_vectors::vector_map::serialize(&self.0, serializer)
    }
}

impl<'de> Deserialize<'de> for VectorRowMapV1 {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        externalized_vectors::vector_map::deserialize(deserializer).map(Self)
    }
}

/// The committed prepared batches of one pending build, floats elided.
#[derive(Clone, Debug, Default, PartialEq)]
struct PreparedBatches(Vec<PreparedVectorGenerationV1>);

impl std::ops::Deref for PreparedBatches {
    type Target = Vec<PreparedVectorGenerationV1>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl std::ops::DerefMut for PreparedBatches {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

impl Serialize for PreparedBatches {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        externalized_vectors::prepared_batches::serialize(&self.0, serializer)
    }
}

impl<'de> Deserialize<'de> for PreparedBatches {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        externalized_vectors::prepared_batches::deserialize(deserializer).map(Self)
    }
}

/// State-document adapters that persist an [`ExternalCollection`] as its content
/// address instead of its contents.
///
/// Deserialization accepts only the final address form written by this store.
mod external_state {
    use super::{ContentDigest, ExternalCollection};
    use serde::de::{self, Visitor};
    use serde::{Deserialize, Deserializer, Serialize, Serializer};
    use std::marker::PhantomData;

    /// Serializes an [`ExternalCollection`] address, refusing an unsealed slot.
    pub(super) struct AddressRefV1<'slot>(pub(super) &'slot Option<ContentDigest>);

    impl Serialize for AddressRefV1<'_> {
        fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
        where
            S: Serializer,
        {
            match self.0 {
                Some(address) => address.serialize(serializer),
                None => Err(serde::ser::Error::custom(
                    "externalized state collection was serialized before it was sealed",
                )),
            }
        }
    }

    struct AddressV1<T>(PhantomData<T>);

    impl<'de, T> Visitor<'de> for AddressV1<T>
    where
        T: Default,
    {
        type Value = ExternalCollection<T>;

        fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
            formatter.write_str("an externalized collection address")
        }

        fn visit_str<E>(self, value: &str) -> Result<Self::Value, E>
        where
            E: de::Error,
        {
            let address = ContentDigest::try_from(value.to_owned()).map_err(de::Error::custom)?;
            Ok(ExternalCollection {
                address: Some(address),
                value: T::default(),
            })
        }
    }

    pub(super) fn serialize<T, S>(
        slot: &ExternalCollection<T>,
        serializer: S,
    ) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        AddressRefV1(&slot.address).serialize(serializer)
    }

    pub(super) fn deserialize<'de, T, D>(deserializer: D) -> Result<ExternalCollection<T>, D::Error>
    where
        T: Default,
        D: Deserializer<'de>,
    {
        deserializer.deserialize_str(AddressV1(PhantomData))
    }

    /// The same adapter for a map of externalized collections, used by the
    /// per-generation physical-byte bindings.
    pub(super) mod address_map {
        use super::{
            AddressRefV1, Deserialize, Deserializer, ExternalCollection, PhantomData, Serializer,
        };
        use crate::store::vector_generations::VectorGenerationIdV1;
        use serde::Serialize;
        use std::collections::BTreeMap;

        struct SlotRefV1<'slot, T>(&'slot ExternalCollection<T>, PhantomData<T>);

        impl<T> Serialize for SlotRefV1<'_, T> {
            fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
            where
                S: Serializer,
            {
                AddressRefV1(&self.0.address).serialize(serializer)
            }
        }

        pub(in super::super) fn serialize<T, S>(
            slots: &BTreeMap<VectorGenerationIdV1, ExternalCollection<T>>,
            serializer: S,
        ) -> Result<S::Ok, S::Error>
        where
            S: Serializer,
        {
            serializer.collect_map(
                slots
                    .iter()
                    .map(|(key, slot)| (key, SlotRefV1(slot, PhantomData))),
            )
        }

        struct SlotV1<T>(ExternalCollection<T>);

        impl<'de, T> Deserialize<'de> for SlotV1<T>
        where
            T: Deserialize<'de> + Default,
        {
            fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
            where
                D: Deserializer<'de>,
            {
                super::deserialize(deserializer).map(Self)
            }
        }

        pub(in super::super) fn deserialize<'de, T, D>(
            deserializer: D,
        ) -> Result<BTreeMap<VectorGenerationIdV1, ExternalCollection<T>>, D::Error>
        where
            T: Deserialize<'de> + Default,
            D: Deserializer<'de>,
        {
            Ok(
                BTreeMap::<VectorGenerationIdV1, SlotV1<T>>::deserialize(deserializer)?
                    .into_iter()
                    .map(|(key, slot)| (key, slot.0))
                    .collect(),
            )
        }
    }

    /// The plan is persisted with its expected chunk list externalized. The
    /// plan's own serde stays transparent so the build-identity digest is
    /// unchanged; only this state-document encoding elides the list.
    pub(super) mod plan {
        use super::{
            AddressRefV1, Deserialize, Deserializer, ExternalCollection, Serialize, Serializer,
        };
        use crate::store::vector_generations::{
            CodeGenerationId, CodeSearchChunkId, ManifestDigest, ProjectionKeyV1,
            VectorGenerationIdV1, VectorGenerationPlanV1,
        };

        #[derive(Serialize)]
        struct PlanRefV1<'plan> {
            target_projection_key: &'plan ProjectionKeyV1,
            source_generation: &'plan CodeGenerationId,
            source_manifest_digest: &'plan ManifestDigest,
            expected_chunk_ids: AddressRefV1<'plan>,
            base_generation: &'plan Option<VectorGenerationIdV1>,
        }

        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct PlanRowV1 {
            target_projection_key: ProjectionKeyV1,
            source_generation: CodeGenerationId,
            source_manifest_digest: ManifestDigest,
            #[serde(deserialize_with = "super::deserialize")]
            expected_chunk_ids: ExternalCollection<Vec<CodeSearchChunkId>>,
            base_generation: Option<VectorGenerationIdV1>,
        }

        pub(in super::super) fn serialize<S>(
            plan: &VectorGenerationPlanV1,
            serializer: S,
        ) -> Result<S::Ok, S::Error>
        where
            S: Serializer,
        {
            PlanRefV1 {
                target_projection_key: &plan.target_projection_key,
                source_generation: &plan.source_generation,
                source_manifest_digest: &plan.source_manifest_digest,
                expected_chunk_ids: AddressRefV1(&plan.expected_chunk_ids.address),
                base_generation: &plan.base_generation,
            }
            .serialize(serializer)
        }

        pub(in super::super) fn deserialize<'de, D>(
            deserializer: D,
        ) -> Result<VectorGenerationPlanV1, D::Error>
        where
            D: Deserializer<'de>,
        {
            let row = PlanRowV1::deserialize(deserializer)?;
            Ok(VectorGenerationPlanV1 {
                target_projection_key: row.target_projection_key,
                source_generation: row.source_generation,
                source_manifest_digest: row.source_manifest_digest,
                expected_chunk_ids: row.expected_chunk_ids,
                base_generation: row.base_generation,
            })
        }
    }
}

mod externalized_vectors {
    use super::{
        BTreeMap, CodeGenerationId, CodeSearchChunkId, ContentDigest, ManifestDigest,
        ProjectedChunkVectorV1, ProjectionKeyV1,
    };
    use serde::{Deserialize, Deserializer, Serialize, Serializer};
    use tracedecay_domain::{
        AdmittedEmbeddingProjectionKeyV1, ProjectionBatchReceiptV1, ProjectionBatchRequestV1,
    };
    use tracedecay_semantic::projector::{PreparedVectorGenerationV1, VectorTombstoneV1};

    #[derive(Serialize)]
    struct VectorRowRefV1<'row> {
        projection_key: &'row ProjectionKeyV1,
        source_generation: &'row CodeGenerationId,
        source_manifest_digest: &'row ManifestDigest,
        chunk_id: &'row CodeSearchChunkId,
        chunk_digest: &'row ContentDigest,
        output_digest: &'row ContentDigest,
    }

    impl<'row> From<&'row ProjectedChunkVectorV1> for VectorRowRefV1<'row> {
        fn from(vector: &'row ProjectedChunkVectorV1) -> Self {
            Self {
                projection_key: &vector.projection_key,
                source_generation: &vector.source_generation,
                source_manifest_digest: &vector.source_manifest_digest,
                chunk_id: &vector.chunk_id,
                chunk_digest: &vector.chunk_digest,
                output_digest: &vector.output_digest,
            }
        }
    }

    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct VectorRowV1 {
        projection_key: ProjectionKeyV1,
        source_generation: CodeGenerationId,
        source_manifest_digest: ManifestDigest,
        chunk_id: CodeSearchChunkId,
        chunk_digest: ContentDigest,
        output_digest: ContentDigest,
    }

    impl From<VectorRowV1> for ProjectedChunkVectorV1 {
        fn from(row: VectorRowV1) -> Self {
            Self {
                projection_key: row.projection_key,
                source_generation: row.source_generation,
                source_manifest_digest: row.source_manifest_digest,
                chunk_id: row.chunk_id,
                chunk_digest: row.chunk_digest,
                values: Vec::new(),
                output_digest: row.output_digest,
            }
        }
    }

    struct VectorSliceRefV1<'row>(&'row [ProjectedChunkVectorV1]);

    impl Serialize for VectorSliceRefV1<'_> {
        fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
        where
            S: Serializer,
        {
            serializer.collect_seq(self.0.iter().map(VectorRowRefV1::from))
        }
    }

    pub(super) mod vector_map {
        use super::{
            BTreeMap, CodeSearchChunkId, Deserialize, Deserializer, ProjectedChunkVectorV1,
            Serializer, VectorRowRefV1, VectorRowV1,
        };

        pub(in super::super) fn serialize<S>(
            vectors: &BTreeMap<CodeSearchChunkId, ProjectedChunkVectorV1>,
            serializer: S,
        ) -> Result<S::Ok, S::Error>
        where
            S: Serializer,
        {
            serializer.collect_map(
                vectors
                    .iter()
                    .map(|(chunk_id, vector)| (chunk_id, VectorRowRefV1::from(vector))),
            )
        }

        pub(in super::super) fn deserialize<'de, D>(
            deserializer: D,
        ) -> Result<BTreeMap<CodeSearchChunkId, ProjectedChunkVectorV1>, D::Error>
        where
            D: Deserializer<'de>,
        {
            Ok(
                BTreeMap::<CodeSearchChunkId, VectorRowV1>::deserialize(deserializer)?
                    .into_iter()
                    .map(|(chunk_id, row)| (chunk_id, row.into()))
                    .collect(),
            )
        }
    }

    pub(super) mod prepared_batches {
        use super::{
            AdmittedEmbeddingProjectionKeyV1, Deserialize, Deserializer,
            PreparedVectorGenerationV1, ProjectionBatchReceiptV1, ProjectionBatchRequestV1,
            Serialize, Serializer, VectorRowV1, VectorSliceRefV1, VectorTombstoneV1,
        };

        #[derive(Serialize)]
        struct PreparedRefV1<'batch> {
            embedding_key: &'batch AdmittedEmbeddingProjectionKeyV1,
            request: &'batch ProjectionBatchRequestV1,
            receipt: &'batch ProjectionBatchReceiptV1,
            vectors: VectorSliceRefV1<'batch>,
            tombstones: &'batch [VectorTombstoneV1],
        }

        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct PreparedRowV1 {
            embedding_key: AdmittedEmbeddingProjectionKeyV1,
            request: ProjectionBatchRequestV1,
            receipt: ProjectionBatchReceiptV1,
            vectors: Vec<VectorRowV1>,
            tombstones: Vec<VectorTombstoneV1>,
        }

        pub(in super::super) fn serialize<S>(
            batches: &[PreparedVectorGenerationV1],
            serializer: S,
        ) -> Result<S::Ok, S::Error>
        where
            S: Serializer,
        {
            serializer.collect_seq(batches.iter().map(|batch| PreparedRefV1 {
                embedding_key: &batch.embedding_key,
                request: &batch.request,
                receipt: &batch.receipt,
                vectors: VectorSliceRefV1(&batch.vectors),
                tombstones: &batch.tombstones,
            }))
        }

        pub(in super::super) fn deserialize<'de, D>(
            deserializer: D,
        ) -> Result<Vec<PreparedVectorGenerationV1>, D::Error>
        where
            D: Deserializer<'de>,
        {
            Ok(Vec::<PreparedRowV1>::deserialize(deserializer)?
                .into_iter()
                .map(|row| PreparedVectorGenerationV1 {
                    embedding_key: row.embedding_key,
                    request: row.request,
                    receipt: row.receipt,
                    vectors: row.vectors.into_iter().map(Into::into).collect(),
                    tombstones: row.tombstones,
                })
                .collect())
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct PublishedVectorGenerationV1 {
    generation_id: VectorGenerationIdV1,
    graph_projection: GraphProjectionId,
    projection_key: ProjectionKeyV1,
    source_generation: CodeGenerationId,
    source_manifest_digest: ManifestDigest,
    base_generation: Option<VectorGenerationIdV1>,
    embedding_key: AdmittedEmbeddingProjectionKeyV1,
    #[serde(with = "external_state")]
    vectors: ExternalCollection<VectorRowMapV1>,
    #[serde(with = "external_state")]
    tombstones: ExternalCollection<Vec<CodeSearchChunkId>>,
    #[serde(with = "external_state")]
    tombstone_digests: ExternalCollection<BTreeMap<CodeSearchChunkId, ContentDigest>>,
    #[serde(with = "external_state")]
    receipts: ExternalCollection<Vec<ProjectionBatchReceiptV1>>,
    checkpoint: VectorProjectionCheckpointV1,
    manifest_digest: ManifestDigest,
}

impl PublishedVectorGenerationV1 {
    pub fn generation_id(&self) -> &VectorGenerationIdV1 {
        &self.generation_id
    }

    pub fn graph_projection(&self) -> &GraphProjectionId {
        &self.graph_projection
    }

    pub fn projection_key(&self) -> &ProjectionKeyV1 {
        &self.projection_key
    }

    pub fn source_generation(&self) -> &CodeGenerationId {
        &self.source_generation
    }

    pub fn source_manifest_digest(&self) -> &ManifestDigest {
        &self.source_manifest_digest
    }

    pub fn base_generation(&self) -> Option<&VectorGenerationIdV1> {
        self.base_generation.as_ref()
    }

    pub fn embedding_key(&self) -> &AdmittedEmbeddingProjectionKeyV1 {
        &self.embedding_key
    }

    pub fn vectors(&self) -> &BTreeMap<CodeSearchChunkId, ProjectedChunkVectorV1> {
        &self.vectors
    }

    pub fn tombstones(&self) -> &[CodeSearchChunkId] {
        &self.tombstones
    }

    pub fn tombstone_digests(&self) -> &BTreeMap<CodeSearchChunkId, ContentDigest> {
        &self.tombstone_digests
    }

    pub fn receipts(&self) -> &[ProjectionBatchReceiptV1] {
        &self.receipts
    }

    pub fn checkpoint(&self) -> &VectorProjectionCheckpointV1 {
        &self.checkpoint
    }

    pub fn manifest_digest(&self) -> &ManifestDigest {
        &self.manifest_digest
    }

    fn same_vector_content(&self, other: &Self) -> bool {
        self.projection_key == other.projection_key
            && self.graph_projection == other.graph_projection
            && self.source_generation == other.source_generation
            && self.source_manifest_digest == other.source_manifest_digest
            && self.embedding_key == other.embedding_key
            && self.vectors == other.vectors
            && self.tombstones == other.tombstones
            && self.tombstone_digests == other.tombstone_digests
            && self.manifest_digest == other.manifest_digest
    }

    fn canonicalize_tombstones(&mut self) {
        self.tombstones = self.tombstone_digests.keys().cloned().collect();
    }

    fn validate_persisted(&self) -> Result<(), VectorGenerationStoreErrorV1> {
        if self.generation_id.as_digest() != &self.manifest_digest
            || generation_identity_digest(
                &VectorGenerationPlanV1 {
                    target_projection_key: self.projection_key.clone(),
                    source_generation: self.source_generation.clone(),
                    source_manifest_digest: self.source_manifest_digest.clone(),
                    expected_chunk_ids: self.vectors.keys().cloned().collect(),
                    base_generation: self.base_generation.clone(),
                },
                &self.vectors,
                &self.tombstone_digests,
            )
            .map_err(|error| VectorGenerationStoreErrorV1::Storage(error.to_string()))?
                != self.manifest_digest
        {
            return Err(VectorGenerationStoreErrorV1::Storage(
                "published generation id does not match manifest digest".to_string(),
            ));
        }
        if self.embedding_key.projection_key() != &self.projection_key {
            return Err(VectorGenerationStoreErrorV1::Storage(
                "published embedding key does not match projection key".to_string(),
            ));
        }
        let canonical_tombstones = self.tombstone_digests.keys().cloned().collect::<Vec<_>>();
        if *self.tombstones != canonical_tombstones {
            return Err(VectorGenerationStoreErrorV1::Storage(
                "published tombstone list is not the canonical digest-map order".to_string(),
            ));
        }
        for vector in self.vectors.values() {
            validate_vector_row_for_published(self, vector)?;
        }
        for chunk_id in self.tombstone_digests.keys() {
            if self.vectors.contains_key(chunk_id) {
                return Err(VectorGenerationStoreErrorV1::Storage(format!(
                    "published generation retains both vector and tombstone for {chunk_id}"
                )));
            }
        }
        validate_published_receipts(self)?;
        Ok(())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct VectorGenerationPublicationV1 {
    pub generation_id: VectorGenerationIdV1,
    pub manifest_digest: ManifestDigest,
    pub checkpoint: VectorProjectionCheckpointV1,
}

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum VectorGenerationStoreErrorV1 {
    #[error("semantic vector operation was cancelled")]
    Cancelled,
    #[error("semantic vector graph authority is unavailable: {0}")]
    Unavailable(String),
    #[error("semantic vector graph authority requires reset: {0}")]
    ResetRequired(String),
    #[error("semantic vector graph authority is corrupt: {0}")]
    Corrupt(String),
    #[error("semantic vector graph durability is uncertain: {0}")]
    DurabilityUncertain(String),
    #[error("semantic vector graph authority is closed")]
    Closed,
    #[error("vector generation plan is invalid: {0}")]
    InvalidPlan(String),
    #[error("unknown vector generation build")]
    UnknownBuild,
    #[error("the supplied checkpoint is stale")]
    StaleCheckpoint,
    #[error("projection batch does not match its vector generation plan")]
    BatchIdentityMismatch,
    #[error("projection batch was replayed with conflicting content")]
    ConflictingBatchReplay,
    #[error("chunk {0} appears in more than one committed batch")]
    DuplicateChunkEffect(CodeSearchChunkId),
    #[error("base vector generation is missing or incompatible")]
    IncompatibleBaseGeneration,
    #[error("reused chunk {0} has no matching immutable base vector")]
    MissingBaseVector(CodeSearchChunkId),
    #[error("applied chunk {0} has no matching vector output")]
    MissingAppliedVector(CodeSearchChunkId),
    #[error("vector generation membership is incomplete")]
    IncompleteGeneration,
    #[error("active vector generation changed before publication")]
    StaleActiveGeneration,
    #[error("immutable vector generation identity already has different content")]
    ImmutableGenerationConflict,
    #[error("physical vector reuse identity already has different bytes")]
    PhysicalVectorConflict,
    #[error("injected failure before atomic publication swap")]
    InjectedPublicationFailure,
    #[error("project vector generation storage failed: {0}")]
    Storage(String),
    #[error("project vector generation state changed repeatedly during compare-and-swap")]
    ConcurrentMutation,
    #[error("semantic projector handoff rejected: {0}")]
    Projection(#[from] SemanticProjectionErrorV1),
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PendingVectorGeneration {
    #[serde(with = "external_state::plan")]
    plan: VectorGenerationPlanV1,
    embedding_key: Option<AdmittedEmbeddingProjectionKeyV1>,
    #[serde(with = "external_state")]
    vectors: ExternalCollection<VectorRowMapV1>,
    #[serde(with = "external_state")]
    tombstones: ExternalCollection<BTreeMap<CodeSearchChunkId, ContentDigest>>,
    #[serde(with = "external_state")]
    batches: ExternalCollection<PreparedBatches>,
    #[serde(with = "external_state")]
    committed_chunk_effects: ExternalCollection<BTreeSet<CodeSearchChunkId>>,
    checkpoint: VectorProjectionCheckpointV1,
}

#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PublishedStateV1 {
    generations: BTreeMap<VectorGenerationIdV1, PublishedVectorGenerationV1>,
    active_generation: Option<VectorGenerationIdV1>,
    #[serde(skip, default)]
    physical_vectors: BTreeMap<ManifestDigest, PhysicalVectorPayloadV1>,
    #[serde(default, with = "external_state::address_map")]
    physical_vector_bindings: BTreeMap<
        VectorGenerationIdV1,
        ExternalCollection<BTreeMap<CodeSearchChunkId, ManifestDigest>>,
    >,
}

/// Deterministic state machine used directly by focused tests and persisted by
/// [`DatabaseVectorGenerationStoreV1`].
#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VectorGenerationState {
    pending: BTreeMap<VectorGenerationBuildIdV1, PendingVectorGeneration>,
    published: PublishedStateV1,
    #[serde(skip, default)]
    physical_vector_pool: PhysicalVectorBytePoolV1,
    #[serde(default, skip)]
    fail_before_publication_swap: bool,
}

impl VectorGenerationState {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn begin_generation(
        &mut self,
        plan: VectorGenerationPlanV1,
    ) -> Result<VectorGenerationBuildIdV1, VectorGenerationStoreErrorV1> {
        validate_plan(&plan)?;
        if let Some(base_id) = &plan.base_generation {
            self.published
                .generations
                .get(base_id)
                .ok_or(VectorGenerationStoreErrorV1::IncompatibleBaseGeneration)?;
        }
        let digest = canonical_sha256(&(VECTOR_GENERATION_BUILD_DIGEST_DOMAIN, &plan))
            .map_err(|error| VectorGenerationStoreErrorV1::InvalidPlan(error.to_string()))?;
        let build_id = VectorGenerationBuildIdV1(digest);
        if let Some(existing) = self.pending.get(&build_id) {
            if existing.plan == plan {
                return Ok(build_id);
            }
            return Err(VectorGenerationStoreErrorV1::InvalidPlan(
                "build identity collision".to_string(),
            ));
        }
        let checkpoint = VectorProjectionCheckpointV1 {
            target_projection_key: plan.target_projection_key.clone(),
            source_generation: plan.source_generation.clone(),
            source_manifest_digest: plan.source_manifest_digest.clone(),
            completed_batches: 0,
            last_request_digest: None,
            last_publication_digest: None,
        };
        self.pending.insert(
            build_id.clone(),
            PendingVectorGeneration {
                plan,
                embedding_key: None,
                vectors: ExternalCollection::default(),
                tombstones: ExternalCollection::default(),
                batches: ExternalCollection::default(),
                committed_chunk_effects: ExternalCollection::default(),
                checkpoint,
            },
        );
        Ok(build_id)
    }

    /// Discard any checkpointed execution for the same deterministic build
    /// identity and restart projection from its authoritative query inputs.
    /// Already-published generations and the active pointer are untouched.
    pub fn rebuild_generation(
        &mut self,
        plan: VectorGenerationPlanV1,
    ) -> Result<VectorGenerationBuildIdV1, VectorGenerationStoreErrorV1> {
        let build_id = self.begin_generation(plan.clone())?;
        let checkpoint = VectorProjectionCheckpointV1 {
            target_projection_key: plan.target_projection_key.clone(),
            source_generation: plan.source_generation.clone(),
            source_manifest_digest: plan.source_manifest_digest.clone(),
            completed_batches: 0,
            last_request_digest: None,
            last_publication_digest: None,
        };
        self.pending.insert(
            build_id.clone(),
            PendingVectorGeneration {
                plan,
                embedding_key: None,
                vectors: ExternalCollection::default(),
                tombstones: ExternalCollection::default(),
                batches: ExternalCollection::default(),
                committed_chunk_effects: ExternalCollection::default(),
                checkpoint,
            },
        );
        Ok(build_id)
    }

    /// Discard one unpublished build without changing any immutable
    /// generation or the active pointer. This is the cancellation boundary
    /// for asynchronous projection work.
    pub fn cancel_generation(&mut self, build_id: &VectorGenerationBuildIdV1) -> bool {
        self.pending.remove(build_id).is_some()
    }

    /// Atomically commit one batch's vector effects, tombstones, Plan 25
    /// receipt, and next checkpoint. Any validation failure leaves the prior
    /// pending state and checkpoint unchanged.
    pub fn commit_batch(
        &mut self,
        build_id: &VectorGenerationBuildIdV1,
        expected_checkpoint: Option<&VectorProjectionCheckpointV1>,
        prepared: PreparedVectorGenerationV1,
    ) -> Result<VectorProjectionCheckpointV1, VectorGenerationStoreErrorV1> {
        self.commit_batch_ref(build_id, expected_checkpoint, &prepared)
    }

    /// Borrowing form of [`Self::commit_batch`]. The persistent adapter drives
    /// this one so a whole-corpus batch is never copied just to satisfy a
    /// retryable mutation closure.
    pub(crate) fn commit_batch_ref(
        &mut self,
        build_id: &VectorGenerationBuildIdV1,
        expected_checkpoint: Option<&VectorProjectionCheckpointV1>,
        prepared: &PreparedVectorGenerationV1,
    ) -> Result<VectorProjectionCheckpointV1, VectorGenerationStoreErrorV1> {
        let current = self
            .pending
            .get(build_id)
            .cloned()
            .ok_or(VectorGenerationStoreErrorV1::UnknownBuild)?;
        if let Some(existing) = current
            .batches
            .iter()
            .find(|batch| batch.request.request_digest == prepared.request.request_digest)
        {
            if existing == prepared {
                return Ok(current.checkpoint);
            }
            return Err(VectorGenerationStoreErrorV1::ConflictingBatchReplay);
        }
        if current.checkpoint.completed_batches == 0 {
            if expected_checkpoint.is_some() {
                return Err(VectorGenerationStoreErrorV1::StaleCheckpoint);
            }
        } else if expected_checkpoint != Some(&current.checkpoint) {
            return Err(VectorGenerationStoreErrorV1::StaleCheckpoint);
        }

        validate_batch_identity(&current.plan, prepared)?;
        validate_base_generation_for_batch(&self.published, &current.plan, prepared)?;
        verify_batch_receipt(&prepared.request, &prepared.receipt)
            .map_err(SemanticProjectionErrorV1::from)?;
        let mut next = current;
        if let Some(key) = &next.embedding_key {
            if key != &prepared.embedding_key {
                return Err(VectorGenerationStoreErrorV1::BatchIdentityMismatch);
            }
        } else {
            next.embedding_key = Some(prepared.embedding_key.clone());
        }

        let vector_by_chunk = prepared
            .vectors
            .iter()
            .map(|vector| (vector.chunk_id.clone(), vector))
            .collect::<BTreeMap<_, _>>();
        let tombstone_by_chunk = prepared
            .tombstones
            .iter()
            .map(|tombstone| (tombstone.chunk_id.clone(), tombstone))
            .collect::<BTreeMap<_, _>>();
        if vector_by_chunk.len() != prepared.vectors.len()
            || tombstone_by_chunk.len() != prepared.tombstones.len()
        {
            return Err(VectorGenerationStoreErrorV1::BatchIdentityMismatch);
        }

        for receipt in &prepared.receipt.receipts {
            if !next
                .committed_chunk_effects
                .insert(receipt.chunk_id.clone())
            {
                return Err(VectorGenerationStoreErrorV1::DuplicateChunkEffect(
                    receipt.chunk_id.clone(),
                ));
            }
            match receipt.operation {
                ProjectionOperationV1::Added | ProjectionOperationV1::Updated => {
                    let vector = vector_by_chunk.get(&receipt.chunk_id).ok_or_else(|| {
                        VectorGenerationStoreErrorV1::MissingAppliedVector(receipt.chunk_id.clone())
                    })?;
                    validate_prepared_vector_row(prepared, vector)?;
                    if receipt.outcome != ProjectionOutcomeV1::Applied
                        || receipt.output_digest.as_ref() != Some(&vector.output_digest)
                    {
                        return Err(VectorGenerationStoreErrorV1::BatchIdentityMismatch);
                    }
                    next.tombstones.remove(&receipt.chunk_id);
                    let mut rebound = (*vector).clone();
                    rebound.source_manifest_digest = next.plan.source_manifest_digest.clone();
                    next.vectors.insert(receipt.chunk_id.clone(), rebound);
                }
                ProjectionOperationV1::Deleted => {
                    let tombstone = tombstone_by_chunk
                        .get(&receipt.chunk_id)
                        .ok_or(VectorGenerationStoreErrorV1::BatchIdentityMismatch)?;
                    if receipt.prior_chunk_digest.as_ref() != Some(&tombstone.prior_chunk_digest) {
                        return Err(VectorGenerationStoreErrorV1::BatchIdentityMismatch);
                    }
                    validate_base_digest(&self.published, &next.plan, receipt)?;
                    next.vectors.remove(&receipt.chunk_id);
                    next.tombstones.insert(
                        receipt.chunk_id.clone(),
                        tombstone.prior_chunk_digest.clone(),
                    );
                }
                ProjectionOperationV1::Reused => {
                    let base = base_vector(&self.published, &next.plan, &receipt.chunk_id)?;
                    if next.plan.target_projection_key != base.projection_key
                        || receipt.prior_chunk_digest.as_ref() != Some(&base.chunk_digest)
                        || receipt.current_chunk_digest.as_ref() != Some(&base.chunk_digest)
                    {
                        return Err(VectorGenerationStoreErrorV1::MissingBaseVector(
                            receipt.chunk_id.clone(),
                        ));
                    }
                    let mut rebound = base.clone();
                    rebound.source_generation = next.plan.source_generation.clone();
                    rebound.source_manifest_digest = next.plan.source_manifest_digest.clone();
                    next.vectors.insert(receipt.chunk_id.clone(), rebound);
                }
            }
        }
        if vector_by_chunk.len()
            != prepared
                .receipt
                .receipts
                .iter()
                .filter(|receipt| {
                    matches!(
                        receipt.operation,
                        ProjectionOperationV1::Added | ProjectionOperationV1::Updated
                    )
                })
                .count()
            || tombstone_by_chunk.len()
                != prepared
                    .receipt
                    .receipts
                    .iter()
                    .filter(|receipt| receipt.operation == ProjectionOperationV1::Deleted)
                    .count()
        {
            return Err(VectorGenerationStoreErrorV1::BatchIdentityMismatch);
        }

        next.checkpoint.completed_batches += 1;
        next.checkpoint.last_request_digest = Some(prepared.request.request_digest.clone());
        next.checkpoint.last_publication_digest = Some(prepared.receipt.publication_digest.clone());
        next.batches.push(prepared.clone());
        let checkpoint = next.checkpoint.clone();
        self.pending.insert(build_id.clone(), next);
        Ok(checkpoint)
    }

    /// Validate a fully pending immutable generation and atomically publish
    /// both its record and active pointer. Partial generations remain in
    /// `pending` and are never returned by active-generation reads.
    pub fn publish_generation(
        &mut self,
        build_id: &VectorGenerationBuildIdV1,
        expected_active_generation: Option<&VectorGenerationIdV1>,
    ) -> Result<VectorGenerationPublicationV1, VectorGenerationStoreErrorV1> {
        if self.published.active_generation.as_ref() != expected_active_generation {
            return Err(VectorGenerationStoreErrorV1::StaleActiveGeneration);
        }
        let pending = self
            .pending
            .get(build_id)
            .cloned()
            .ok_or(VectorGenerationStoreErrorV1::UnknownBuild)?;
        let expected = pending
            .plan
            .expected_chunk_ids
            .iter()
            .cloned()
            .collect::<BTreeSet<_>>();
        let actual = pending.vectors.keys().cloned().collect::<BTreeSet<_>>();
        if expected != actual || pending.batches.is_empty() {
            return Err(VectorGenerationStoreErrorV1::IncompleteGeneration);
        }
        let embedding_key = pending
            .embedding_key
            .clone()
            .ok_or(VectorGenerationStoreErrorV1::IncompleteGeneration)?;
        for vector in pending.vectors.values() {
            validate_vector_row(&pending.plan, &embedding_key, vector)?;
        }

        let manifest_digest =
            generation_identity_digest(&pending.plan, &pending.vectors, &pending.tombstones)?;
        let generation_id = VectorGenerationIdV1::new(manifest_digest.clone());
        let tombstone_digests = pending.tombstones;
        let mut generation = PublishedVectorGenerationV1 {
            generation_id: generation_id.clone(),
            graph_projection: build_projection_id(build_id)?,
            projection_key: pending.plan.target_projection_key,
            source_generation: pending.plan.source_generation,
            source_manifest_digest: pending.plan.source_manifest_digest,
            base_generation: pending.plan.base_generation,
            embedding_key,
            vectors: pending.vectors,
            tombstones: ExternalCollection::default(),
            tombstone_digests,
            receipts: pending
                .batches
                .into_inner()
                .0
                .into_iter()
                .map(|batch| batch.receipt)
                .collect(),
            checkpoint: pending.checkpoint.clone(),
            manifest_digest: manifest_digest.clone(),
        };
        generation.canonicalize_tombstones();
        generation.validate_persisted()?;
        // Decide the whole publication against the current state before
        // touching it, so the swap needs no defensive deep copy of every
        // published generation.
        let replays_existing = match self.published.generations.get(&generation_id) {
            Some(existing) => {
                if !existing.same_vector_content(&generation) {
                    return Err(VectorGenerationStoreErrorV1::ImmutableGenerationConflict);
                }
                true
            }
            None => false,
        };
        if self.fail_before_publication_swap {
            self.fail_before_publication_swap = false;
            return Err(VectorGenerationStoreErrorV1::InjectedPublicationFailure);
        }
        #[cfg(test)]
        intern_generation_vectors(&self.physical_vector_pool, &mut self.published, &generation)?;
        let checkpoint = if replays_existing {
            self.published
                .generations
                .get(&generation_id)
                .ok_or(VectorGenerationStoreErrorV1::ImmutableGenerationConflict)?
                .checkpoint
                .clone()
        } else {
            let checkpoint = generation.checkpoint.clone();
            self.published
                .generations
                .insert(generation_id.clone(), generation);
            checkpoint
        };
        self.published.active_generation = Some(generation_id.clone());
        self.pending.remove(build_id);
        Ok(VectorGenerationPublicationV1 {
            generation_id,
            manifest_digest,
            checkpoint,
        })
    }

    pub fn active_generation_id(&self) -> Option<&VectorGenerationIdV1> {
        self.published.active_generation.as_ref()
    }

    /// Atomically repoint reads to an already-published immutable generation.
    pub fn activate_generation(
        &mut self,
        generation_id: &VectorGenerationIdV1,
        expected_active_generation: Option<&VectorGenerationIdV1>,
    ) -> Result<VectorGenerationPublicationV1, VectorGenerationStoreErrorV1> {
        if self.published.active_generation.as_ref() != expected_active_generation {
            return Err(VectorGenerationStoreErrorV1::StaleActiveGeneration);
        }
        let generation = self
            .published
            .generations
            .get(generation_id)
            .ok_or(VectorGenerationStoreErrorV1::IncompatibleBaseGeneration)?;
        generation.validate_persisted()?;
        let publication = VectorGenerationPublicationV1 {
            generation_id: generation.generation_id().clone(),
            manifest_digest: generation.manifest_digest().clone(),
            checkpoint: generation.checkpoint().clone(),
        };
        if self.fail_before_publication_swap {
            self.fail_before_publication_swap = false;
            return Err(VectorGenerationStoreErrorV1::InjectedPublicationFailure);
        }
        self.published.active_generation = Some(generation_id.clone());
        Ok(publication)
    }

    /// Atomically disable semantic reads while retaining immutable generations
    /// for an exact offline rollback.
    pub fn deactivate_generation(
        &mut self,
        expected_active_generation: Option<&VectorGenerationIdV1>,
    ) -> Result<(), VectorGenerationStoreErrorV1> {
        if self.published.active_generation.as_ref() != expected_active_generation {
            return Err(VectorGenerationStoreErrorV1::StaleActiveGeneration);
        }
        if self.fail_before_publication_swap {
            self.fail_before_publication_swap = false;
            return Err(VectorGenerationStoreErrorV1::InjectedPublicationFailure);
        }
        self.published.active_generation = None;
        Ok(())
    }

    pub fn active_checkpoint(&self) -> Option<&VectorProjectionCheckpointV1> {
        self.active_generation()
            .map(PublishedVectorGenerationV1::checkpoint)
    }

    /// The checkpoint of one pending build, which is how a resumed run learns
    /// how many of its batches are already durable.
    pub fn pending_checkpoint(
        &self,
        build_id: &VectorGenerationBuildIdV1,
    ) -> Option<&VectorProjectionCheckpointV1> {
        self.pending
            .get(build_id)
            .map(|pending| &pending.checkpoint)
    }

    pub fn active_generation(&self) -> Option<&PublishedVectorGenerationV1> {
        self.active_generation_id()
            .and_then(|id| self.published.generations.get(id))
    }

    /// Return the active immutable generation only when every query-facing
    /// projection and source identity matches exactly. A pending replacement
    /// is never considered, so incompatible searches omit semantics rather
    /// than reading stale or partial rows.
    pub fn active_generation_for(
        &self,
        embedding_key: &AdmittedEmbeddingProjectionKeyV1,
        source_generation: &CodeGenerationId,
        source_manifest_digest: &ManifestDigest,
    ) -> Option<&PublishedVectorGenerationV1> {
        self.active_generation().filter(|generation| {
            generation.embedding_key() == embedding_key
                && generation.source_generation() == source_generation
                && generation.source_manifest_digest() == source_manifest_digest
        })
    }

    pub fn generation(
        &self,
        generation_id: &VectorGenerationIdV1,
    ) -> Option<&PublishedVectorGenerationV1> {
        self.published.generations.get(generation_id)
    }

    /// Resolve the shared immutable vector bytes behind one logical generation
    /// occurrence. The returned allocation is reused only inside the exact
    /// projection/privacy authority named by the generation.
    pub fn physical_vector_values(
        &self,
        generation_id: &VectorGenerationIdV1,
        chunk_id: &CodeSearchChunkId,
    ) -> Option<Arc<[f32]>> {
        let physical_id = self
            .published
            .physical_vector_bindings
            .get(generation_id)?
            .get(chunk_id)?;
        self.published
            .physical_vectors
            .get(physical_id)
            .map(|payload| Arc::clone(&payload.values.0))
    }

    pub fn fail_before_publication_swap_once(&mut self) {
        self.fail_before_publication_swap = true;
    }
}

/// Persistent adapter over the already-open project database.
///
/// The complete generation state is one canonical JSON value guarded by a
/// monotonically increasing revision. Every mutation is a single conditional
/// update, so a reader observes either the complete old state or the complete
/// new state. In particular, an immutable generation record cannot become
/// visible separately from its active-generation pointer.
pub struct DatabaseVectorGenerationStoreV1<'database> {
    database: &'database Database,
    graph: Arc<GraphDb>,
    graph_namespace: GraphNamespace,
}

/// Relational, non-authoritative state used by the native semantic evaluator.
///
/// It executes the same generation state machine and writer path as
/// production, but uses an isolated row that is removed after the measured
/// run. It can therefore exercise publication/activation without changing the
/// project's active semantic generation.
pub(crate) struct DatabaseVectorEvaluationStoreV1<'database> {
    database: &'database Database,
    evaluation_id: String,
}

#[derive(Clone, Debug)]
pub(crate) struct ActiveVectorGenerationSnapshotV1 {
    revision: i64,
    generation: PublishedVectorGenerationV1,
}

impl ActiveVectorGenerationSnapshotV1 {
    pub(crate) const fn revision(&self) -> i64 {
        self.revision
    }

    pub(crate) fn generation(&self) -> &PublishedVectorGenerationV1 {
        &self.generation
    }

    pub(crate) fn into_generation(self) -> PublishedVectorGenerationV1 {
        self.generation
    }
}

impl<'database> DatabaseVectorGenerationStoreV1<'database> {
    /// Open relational generation lifecycle state over an injected,
    /// daemon-owned Grafeo vector authority.
    pub async fn open(
        database: &'database Database,
        graph: Arc<GraphDb>,
    ) -> Result<Self, VectorGenerationStoreErrorV1> {
        database
            .execute_write_batch(
                VECTOR_GENERATION_STATE_OPERATION,
                VECTOR_GENERATION_STATE_SCHEMA_V1,
            )
            .await
            .map_err(storage_error)?;
        database
            .execute_write_batch(
                VECTOR_GENERATION_STATE_OPERATION,
                VECTOR_STATE_SLICE_SCHEMA_V1,
            )
            .await
            .map_err(storage_error)?;
        let initial_state =
            serde_json::to_string(&VectorGenerationState::default()).map_err(storage_error)?;
        database
            .execute_write_engine(
                VECTOR_GENERATION_STATE_OPERATION,
                "INSERT OR IGNORE INTO semantic_vector_generation_state (
                    singleton, revision, state_json
                 ) VALUES (1, 0, ?1)",
                params![initial_state],
            )
            .await
            .map_err(storage_error)?;
        Ok(Self {
            database,
            graph,
            graph_namespace: GraphNamespace::new(SEMANTIC_VECTOR_NAMESPACE)
                .map_err(graph_store_error)?,
        })
    }

    /// Read the one active immutable generation needed by a request without
    /// entering the writer lane or deserializing pending/inactive generations.
    pub(crate) async fn read_active_generation_for(
        database: &Database,
        graph: &GraphDb,
        embedding_key: &AdmittedEmbeddingProjectionKeyV1,
        source_generation: &CodeGenerationId,
        source_manifest_digest: &ManifestDigest,
    ) -> Result<Option<PublishedVectorGenerationV1>, VectorGenerationStoreErrorV1> {
        Ok(Self::read_active_generation_snapshot_for(
            database,
            graph,
            embedding_key,
            source_generation,
            source_manifest_digest,
        )
        .await?
        .map(ActiveVectorGenerationSnapshotV1::into_generation))
    }

    pub(crate) async fn read_active_generation_metadata_for(
        database: &Database,
        embedding_key: &AdmittedEmbeddingProjectionKeyV1,
        source_generation: &CodeGenerationId,
        source_manifest_digest: &ManifestDigest,
    ) -> Result<Option<PublishedVectorGenerationV1>, VectorGenerationStoreErrorV1> {
        let mut rows = database
            .engine_conn()
            .query(
                "SELECT entry.value
                 FROM semantic_vector_generation_state AS state
                 JOIN json_each(
                     state.state_json,
                     '$.published.generations'
                 ) AS entry
                   ON entry.key = CAST(json_extract(
                       state.state_json,
                       '$.published.active_generation'
                   ) AS TEXT)
                 WHERE state.singleton = 1
                   AND entry.type = 'object'",
                (),
            )
            .await
            .map_err(storage_error)?;
        let Some(row) = rows.next().await.map_err(storage_error)? else {
            return Ok(None);
        };
        let generation_json = row.get::<String>(0).map_err(storage_error)?;
        drop(rows);
        let mut generation: PublishedVectorGenerationV1 =
            serde_json::from_str(&generation_json).map_err(storage_error)?;
        hydrate_generation_slices(database, VECTOR_STATE_SLICE_TABLE_V1, &mut generation).await?;
        if generation.embedding_key() != embedding_key
            || generation.source_generation() != source_generation
            || generation.source_manifest_digest() != source_manifest_digest
        {
            return Ok(None);
        }
        if generation.generation_id.as_digest() != &generation.manifest_digest
            || generation.embedding_key.projection_key() != &generation.projection_key
        {
            return Err(VectorGenerationStoreErrorV1::Corrupt(
                "active semantic vector generation metadata is inconsistent".to_owned(),
            ));
        }
        validate_published_receipts(&generation)?;
        Ok(Some(generation))
    }

    /// Return source generations that remain reachable from a fully published
    /// Grafeo vector projection. Relational state supplies generation
    /// manifests and candidate bindings; Grafeo must independently prove that
    /// a non-empty vector projection is still readable before retention pins
    /// its source generation.
    pub async fn readable_source_generations(
        database: &Database,
        graph: &GraphDb,
    ) -> Result<BTreeSet<CodeGenerationId>, VectorGenerationStoreErrorV1> {
        let mut rows = database
            .engine_conn()
            .query(
                "SELECT entry.value
                 FROM semantic_vector_generation_state AS state
                 JOIN json_each(
                     state.state_json,
                     '$.published.generations'
                 ) AS entry
                 WHERE state.singleton = 1
                   AND entry.type = 'object'
                 ORDER BY entry.key",
                (),
            )
            .await
            .map_err(storage_error)?;
        let mut encoded_generations = Vec::new();
        while let Some(row) = rows.next().await.map_err(storage_error)? {
            encoded_generations.push(row.get::<String>(0).map_err(storage_error)?);
        }
        drop(rows);

        let namespace =
            GraphNamespace::new(SEMANTIC_VECTOR_NAMESPACE).map_err(graph_store_error)?;
        let mut readable_sources = BTreeSet::new();
        for encoded in encoded_generations {
            let mut generation: PublishedVectorGenerationV1 =
                serde_json::from_str(&encoded).map_err(storage_error)?;
            hydrate_generation_slices(database, VECTOR_STATE_SLICE_TABLE_V1, &mut generation)
                .await?;
            if generation.generation_id.as_digest() != &generation.manifest_digest
                || generation.embedding_key.projection_key() != &generation.projection_key
            {
                return Err(VectorGenerationStoreErrorV1::Corrupt(
                    "published semantic vector generation metadata is inconsistent".to_owned(),
                ));
            }
            validate_published_receipts(&generation)?;
            let telemetry = graph
                .projection_telemetry(GraphProjectionTelemetryRequest {
                    namespace: namespace.clone(),
                    projection: generation.graph_projection().clone(),
                    cancellation: Arc::new(NeverCancelled),
                })
                .map_err(graph_store_error)?
                .ok_or_else(|| {
                    VectorGenerationStoreErrorV1::Corrupt(format!(
                        "published semantic vector generation {} has no Grafeo projection",
                        generation.generation_id().as_digest().as_str()
                    ))
                })?;
            let expected_source = SourceGeneration::new(generation.source_generation().as_str())
                .map_err(graph_store_error)?;
            if telemetry.source_generation != expected_source || telemetry.relation_count != 0 {
                return Err(VectorGenerationStoreErrorV1::Corrupt(format!(
                    "published semantic vector generation {} has inconsistent Grafeo telemetry",
                    generation.generation_id().as_digest().as_str()
                )));
            }
            readable_sources.insert(generation.source_generation().clone());
        }
        Ok(readable_sources)
    }

    /// Read the atomically active immutable generation without entering the
    /// writer lane. Callers must apply their own source/projection admission.
    pub(crate) async fn read_active_generation(
        database: &Database,
        _graph: &GraphDb,
    ) -> Result<Option<PublishedVectorGenerationV1>, VectorGenerationStoreErrorV1> {
        Ok(Self::read_active_generation_snapshot(database, graph)
            .await?
            .map(ActiveVectorGenerationSnapshotV1::into_generation))
    }

    async fn read_active_generation_snapshot(
        database: &Database,
        graph: &GraphDb,
    ) -> Result<Option<ActiveVectorGenerationSnapshotV1>, VectorGenerationStoreErrorV1> {
        let mut rows = database
            .engine_conn()
            .query(
                "SELECT state.revision, entry.value
                 FROM semantic_vector_generation_state AS state
                 JOIN json_each(
                     state.state_json,
                     '$.published.generations'
                 ) AS entry
                   ON entry.key = CAST(json_extract(
                       state.state_json,
                       '$.published.active_generation'
                   ) AS TEXT)
                 WHERE state.singleton = 1
                   AND entry.type = 'object'",
                (),
            )
            .await
            .map_err(storage_error)?;
        let Some(row) = rows.next().await.map_err(storage_error)? else {
            return Ok(None);
        };
        let revision = row.get::<i64>(0).map_err(storage_error)?;
        let generation_json = row.get::<String>(1).map_err(storage_error)?;
        drop(rows);
        let mut generation: PublishedVectorGenerationV1 =
            serde_json::from_str(&generation_json).map_err(storage_error)?;
        drop(generation_json);
        hydrate_generation_slices(database, VECTOR_STATE_SLICE_TABLE_V1, &mut generation).await?;
        generation.validate_persisted()?;
        Ok(Some(ActiveVectorGenerationSnapshotV1 {
            revision,
            generation,
        }))
    }

    pub(crate) async fn read_active_generation_snapshot_for(
        database: &Database,
        _graph: &GraphDb,
        embedding_key: &AdmittedEmbeddingProjectionKeyV1,
        source_generation: &CodeGenerationId,
        source_manifest_digest: &ManifestDigest,
    ) -> Result<Option<ActiveVectorGenerationSnapshotV1>, VectorGenerationStoreErrorV1> {
        let Some(snapshot) = Self::read_active_generation_snapshot(database, graph).await? else {
            return Ok(None);
        };
        if snapshot.generation.embedding_key() != embedding_key
            || snapshot.generation.source_generation() != source_generation
            || snapshot.generation.source_manifest_digest() != source_manifest_digest
        {
            return Ok(None);
        }
        Ok(Some(snapshot))
    }

    pub(crate) async fn active_snapshot_is_current(
        database: &Database,
        revision: i64,
        generation_id: &VectorGenerationIdV1,
    ) -> Result<bool, VectorGenerationStoreErrorV1> {
        let mut rows = database
            .engine_conn()
            .query(
                "SELECT 1
                 FROM semantic_vector_generation_state
                 WHERE singleton = 1
                   AND revision = ?1
                   AND CAST(json_extract(
                       state_json,
                       '$.published.active_generation'
                   ) AS TEXT) = ?2",
                params![revision, generation_id.as_digest().as_str()],
            )
            .await
            .map_err(storage_error)?;
        let is_current = rows.next().await.map_err(storage_error)?.is_some();
        drop(rows);
        Ok(is_current)
    }

    pub(crate) async fn read_generation(
        database: &Database,
        graph: &GraphDb,
        generation_id: &VectorGenerationIdV1,
    ) -> Result<Option<PublishedVectorGenerationV1>, VectorGenerationStoreErrorV1> {
        let mut rows = database
            .engine_conn()
            .query(
                "SELECT entry.value
                 FROM semantic_vector_generation_state AS state
                 JOIN json_each(
                     state.state_json,
                     '$.published.generations'
                 ) AS entry
                   ON entry.key = ?1
                 WHERE state.singleton = 1
                   AND entry.type = 'object'",
                params![generation_id.as_digest().as_str()],
            )
            .await
            .map_err(storage_error)?;
        let Some(row) = rows.next().await.map_err(storage_error)? else {
            return Ok(None);
        };
        let generation_json = row.get::<String>(0).map_err(storage_error)?;
        drop(rows);
        let mut generation: PublishedVectorGenerationV1 =
            serde_json::from_str(&generation_json).map_err(storage_error)?;
        drop(generation_json);
        hydrate_generation_slices(database, VECTOR_STATE_SLICE_TABLE_V1, &mut generation).await?;
        generation.validate_persisted()?;
        (generation.generation_id() == generation_id)
            .then_some(generation)
            .ok_or_else(|| {
                VectorGenerationStoreErrorV1::Storage(
                    "vector generation map key does not match its identity".to_owned(),
                )
            })
            .map(Some)
    }

    pub(crate) async fn read_generation_lineage(
        database: &Database,
        graph: &GraphDb,
        generation_id: &VectorGenerationIdV1,
    ) -> Result<Vec<PublishedVectorGenerationV1>, VectorGenerationStoreErrorV1> {
        let mut lineage = Vec::new();
        let mut next = Some(generation_id.clone());
        let mut seen = BTreeSet::new();
        while let Some(generation_id) = next {
            if !seen.insert(generation_id.clone()) {
                return Err(VectorGenerationStoreErrorV1::Corrupt(
                    "semantic vector generation lineage contains a cycle".to_owned(),
                ));
            }
            let generation = Self::read_generation(database, graph, &generation_id)
                .await?
                .ok_or_else(|| {
                    VectorGenerationStoreErrorV1::Corrupt(format!(
                        "semantic vector base generation {generation_id} is missing"
                    ))
                })?;
            next = generation.base_generation().cloned();
            lineage.push(generation);
        }
        Ok(lineage)
    }

    pub async fn begin_generation(
        &self,
        plan: VectorGenerationPlanV1,
    ) -> Result<VectorGenerationBuildIdV1, VectorGenerationStoreErrorV1> {
        self.mutate_state(|state| state.begin_generation(plan.clone()))
            .await
    }

    pub async fn rebuild_generation(
        &self,
        plan: VectorGenerationPlanV1,
    ) -> Result<VectorGenerationBuildIdV1, VectorGenerationStoreErrorV1> {
        let (revision, _, _) = self.load_state().await?;
        let source =
            SourceGeneration::new(plan.source_generation.as_str()).map_err(graph_store_error)?;
        let build = self
            .mutate_retiring_state(|state| state.rebuild_generation(plan.clone()))
            .await?;
        retire_projection(
            &self.graph,
            &self.graph_namespace,
            build_projection_id(&build)?,
            source,
            revision,
        )?;
        Ok(build)
    }

    pub async fn cancel_generation(
        &self,
        build_id: &VectorGenerationBuildIdV1,
    ) -> Result<bool, VectorGenerationStoreErrorV1> {
        let (revision, state, _) = self.load_state().await?;
        let source = state
            .pending
            .get(build_id)
            .map(|pending| SourceGeneration::new(pending.plan.source_generation.as_str()))
            .transpose()
            .map_err(graph_store_error)?;
        let cancelled = self
            .mutate_retiring_state(|state| Ok(state.cancel_generation(build_id)))
            .await?;
        if cancelled && let Some(source) = source {
            retire_projection(
                &self.graph,
                &self.graph_namespace,
                build_projection_id(build_id)?,
                source,
                revision,
            )?;
        }
        Ok(cancelled)
    }

    pub async fn commit_batch(
        &self,
        build_id: &VectorGenerationBuildIdV1,
        expected_checkpoint: Option<&VectorProjectionCheckpointV1>,
        prepared: PreparedVectorGenerationV1,
    ) -> Result<VectorProjectionCheckpointV1, VectorGenerationStoreErrorV1> {
        // Record an immutable relational intent before the first Grafeo
        // mutation. The singleton state is activated only after every bounded,
        // idempotent graph chunk has committed.
        let intent_transaction = self
            .database
            .begin_write_transaction(VECTOR_GENERATION_STATE_OPERATION)
            .await
            .map_err(storage_error)?;
        let (revision, mut next_state, load) = self
            .load_state_from_transaction(&intent_transaction)
            .await?;
        let prior_checkpoint = next_state
            .pending_checkpoint(build_id)
            .cloned()
            .ok_or(VectorGenerationStoreErrorV1::UnknownBuild)?;
        let checkpoint = next_state.commit_batch_ref(build_id, expected_checkpoint, &prepared)?;
        if checkpoint == prior_checkpoint {
            intent_transaction.rollback().await.map_err(storage_error)?;
            return Ok(checkpoint);
        }
        let pending_slices = seal_external_state(&mut next_state, &load.durable_slices)?;
        let state_json = serde_json::to_string(&next_state).map_err(storage_error)?;
        let total_graph_chunks = graph_chunk_count(&prepared);
        let projection = build_projection_id(build_id)?;
        let mut expected_graph_watermark = self
            .graph
            .projection_telemetry(GraphProjectionTelemetryRequest {
                namespace: self.graph_namespace.clone(),
                projection,
                cancellation: Arc::new(NeverCancelled),
            })
            .map_err(graph_store_error)?
            .map(|telemetry| telemetry.watermark);
        let mut intent_digest = canonical_sha256(&(
            "tracedecay.semantic-vector-batch-intent",
            build_id,
            &prepared.request.request_digest,
            revision,
            expected_graph_watermark
                .as_ref()
                .map(GraphWatermark::as_str),
            total_graph_chunks,
            &state_json,
        ))
        .map_err(storage_error)?;
        write_state_slices(
            &intent_transaction,
            VECTOR_STATE_SLICE_TABLE_V1,
            &pending_slices,
        )
        .await?;
        let inserted = intent_transaction
            .execute_engine(
                "INSERT OR IGNORE INTO semantic_vector_batch_intent (
                    intent_digest,
                    build_id,
                    request_digest,
                    expected_state_revision,
                    expected_graph_watermark,
                    state_json,
                    total_graph_chunks,
                    published_graph_chunks
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 0)",
                params![
                    intent_digest.as_str(),
                    build_id.0.as_str(),
                    prepared.request.request_digest.as_str(),
                    revision,
                    expected_graph_watermark
                        .as_ref()
                        .map(GraphWatermark::as_str),
                    &state_json,
                    i64::try_from(total_graph_chunks).map_err(storage_error)?,
                ],
            )
            .await
            .map_err(storage_error)?;
        if inserted == 0 {
            let mut rows = intent_transaction
                .query_engine(
                    "SELECT intent_digest, expected_graph_watermark
                     FROM semantic_vector_batch_intent
                     WHERE build_id = ?1
                       AND request_digest = ?2
                       AND expected_state_revision = ?3
                       AND state_json = ?4
                       AND total_graph_chunks = ?5",
                    params![
                        build_id.0.as_str(),
                        prepared.request.request_digest.as_str(),
                        revision,
                        &state_json,
                        i64::try_from(total_graph_chunks).map_err(storage_error)?,
                    ],
                )
                .await
                .map_err(storage_error)?;
            let replay = rows.next().await.map_err(storage_error)?.map(|row| {
                let digest = row.get::<String>(0).map_err(storage_error)?;
                let watermark = row.get::<Option<String>>(1).map_err(storage_error)?;
                Ok::<_, VectorGenerationStoreErrorV1>((digest, watermark))
            });
            drop(rows);
            let Some((stored_digest, stored_watermark)) = replay.transpose()? else {
                intent_transaction.rollback().await.map_err(storage_error)?;
                return Err(VectorGenerationStoreErrorV1::ImmutableGenerationConflict);
            };
            intent_digest = ManifestDigest::try_from(stored_digest).map_err(storage_error)?;
            expected_graph_watermark = stored_watermark
                .map(GraphWatermark::new)
                .transpose()
                .map_err(graph_store_error)?;
        }
        intent_transaction.commit().await.map_err(storage_error)?;

        let (_, publications) = prepared_delta_publications(
            &self.graph_namespace,
            build_id,
            expected_graph_watermark,
            &prepared,
        )?;
        for (ordinal, publication) in publications.into_iter().enumerate() {
            self.graph.publish(publication).map_err(graph_store_error)?;
            self.database
                .execute_write_engine(
                    VECTOR_GENERATION_STATE_OPERATION,
                    "UPDATE semantic_vector_batch_intent
                     SET published_graph_chunks = MAX(published_graph_chunks, ?1)
                     WHERE intent_digest = ?2",
                    params![
                        i64::try_from(ordinal.saturating_add(1)).map_err(storage_error)?,
                        intent_digest.as_str(),
                    ],
                )
                .await
                .map_err(storage_error)?;
        }

        let activation = self
            .database
            .begin_write_transaction(VECTOR_GENERATION_STATE_OPERATION)
            .await
            .map_err(storage_error)?;
        let changed = activation
            .execute_engine(
                "UPDATE semantic_vector_generation_state
                 SET revision = revision + 1, state_json = ?1
                 WHERE singleton = 1 AND revision = ?2",
                params![&state_json, revision],
            )
            .await
            .map_err(storage_error)?;
        if changed != 1 {
            activation.rollback().await.map_err(storage_error)?;
            return Err(VectorGenerationStoreErrorV1::ConcurrentMutation);
        }
        activation.commit().await.map_err(storage_error)?;
        Ok(checkpoint)
    }

    pub async fn publish_generation(
        &self,
        build_id: &VectorGenerationBuildIdV1,
        expected_active_generation: Option<&VectorGenerationIdV1>,
    ) -> Result<VectorGenerationPublicationV1, VectorGenerationStoreErrorV1> {
        // Phase 1 durably records the complete immutable publication intent
        // and all referenced relational metadata before Grafeo is touched.
        // The active pointer remains unchanged in this phase.
        let intent_transaction = self
            .database
            .begin_write_transaction(VECTOR_GENERATION_STATE_OPERATION)
            .await
            .map_err(storage_error)?;
        let (revision, mut next_state, load) = self
            .load_state_from_transaction(&intent_transaction)
            .await?;
        let publication = next_state.publish_generation(build_id, expected_active_generation)?;
        let published_generation = next_state
            .generation(&publication.generation_id)
            .ok_or(VectorGenerationStoreErrorV1::ImmutableGenerationConflict)?;
        let published_projection = published_generation.graph_projection().clone();
        let published_source =
            SourceGeneration::new(published_generation.source_generation().as_str())
                .map_err(graph_store_error)?;
        let mut vector_indexes = Vec::new();
        let mut lineage_cursor = Some(publication.generation_id.clone());
        while let Some(generation_id) = lineage_cursor {
            let generation = next_state.generation(&generation_id).ok_or_else(|| {
                VectorGenerationStoreErrorV1::Corrupt(format!(
                    "semantic vector base generation {generation_id} is missing"
                ))
            })?;
            let embedding = generation.embedding_key().embedding_key();
            vector_indexes.push(GraphVectorIndexRequest {
                namespace: self.graph_namespace.clone(),
                projection: generation.graph_projection().clone(),
                property: GraphPropertyName::new("embedding").map_err(graph_store_error)?,
                dimension: usize::try_from(embedding.dimensions).map_err(storage_error)?,
                metric: graph_vector_metric(embedding.metric),
                cancellation: Arc::new(NeverCancelled),
            });
            lineage_cursor = generation.base_generation().cloned();
        }
        let pending_slices = seal_external_state(&mut next_state, &load.durable_slices)?;
        let state_json = serde_json::to_string(&next_state).map_err(storage_error)?;
        let publication_json = serde_json::to_string(&publication).map_err(storage_error)?;
        let intent_digest = canonical_sha256(&(
            VECTOR_GENERATION_PUBLICATION_INTENT_DIGEST_DOMAIN,
            build_id,
            revision,
            &publication,
            &state_json,
        ))
        .map_err(storage_error)?;
        write_state_slices(
            &intent_transaction,
            VECTOR_STATE_SLICE_TABLE_V1,
            &pending_slices,
        )
        .await?;
        let inserted = intent_transaction
            .execute_engine(
                "INSERT OR IGNORE INTO semantic_vector_publication_intent (
                    intent_digest,
                    generation_id,
                    build_id,
                    expected_state_revision,
                    state_json,
                    publication_json
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    intent_digest.as_str(),
                    publication.generation_id.as_digest().as_str(),
                    build_id.0.as_str(),
                    revision,
                    &state_json,
                    &publication_json,
                ],
            )
            .await
            .map_err(storage_error)?;
        if inserted == 0 {
            let mut rows = intent_transaction
                .query_engine(
                    "SELECT 1
                     FROM semantic_vector_publication_intent
                     WHERE intent_digest = ?1
                       AND generation_id = ?2
                       AND build_id = ?3
                       AND expected_state_revision = ?4
                       AND state_json = ?5
                       AND publication_json = ?6",
                    params![
                        intent_digest.as_str(),
                        publication.generation_id.as_digest().as_str(),
                        build_id.0.as_str(),
                        revision,
                        &state_json,
                        &publication_json,
                    ],
                )
                .await
                .map_err(storage_error)?;
            let exact_replay = rows.next().await.map_err(storage_error)?.is_some();
            drop(rows);
            if !exact_replay {
                intent_transaction.rollback().await.map_err(storage_error)?;
                return Err(VectorGenerationStoreErrorV1::ImmutableGenerationConflict);
            }
        }
        intent_transaction.commit().await.map_err(storage_error)?;

        // Phase 2 idempotently materializes the intended generation in
        // Grafeo. A crash or typed error here leaves only an unactivated
        // relational intent; rerunning publication repeats this exact write.
        for request in vector_indexes {
            self.graph
                .ensure_vector_index(request)
                .map_err(graph_store_error)?;
        }
        verify_projection(
            &self.graph,
            &self.graph_namespace,
            &published_projection,
            &published_source,
        )?;

        // Phase 3 activates only the exact state revision that produced the
        // intent. Any concurrent mutation leaves the Grafeo generation
        // unreferenced and therefore invisible to compatible reads.
        let activation_transaction = self
            .database
            .begin_write_transaction(VECTOR_GENERATION_STATE_OPERATION)
            .await
            .map_err(storage_error)?;
        let changed = activation_transaction
            .execute_engine(
                "UPDATE semantic_vector_generation_state
                 SET revision = revision + 1, state_json = ?1
                 WHERE singleton = 1 AND revision = ?2",
                params![&state_json, revision],
            )
            .await
            .map_err(storage_error)?;
        if changed != 1 {
            activation_transaction
                .rollback()
                .await
                .map_err(storage_error)?;
            return Err(VectorGenerationStoreErrorV1::ConcurrentMutation);
        }
        let referenced = referenced_state_addresses(&mut next_state)?;
        prune_unreferenced_state_slices(
            &activation_transaction,
            VECTOR_STATE_SLICE_TABLE_V1,
            &referenced,
        )
        .await?;
        activation_transaction
            .commit()
            .await
            .map_err(storage_error)?;
        Ok(publication)
    }

    pub async fn activate_generation(
        &self,
        generation_id: &VectorGenerationIdV1,
        expected_active_generation: Option<&VectorGenerationIdV1>,
    ) -> Result<VectorGenerationPublicationV1, VectorGenerationStoreErrorV1> {
        self.mutate_retiring_state(|state| {
            state.activate_generation(generation_id, expected_active_generation)
        })
        .await
    }

    pub async fn deactivate_generation(
        &self,
        expected_active_generation: Option<&VectorGenerationIdV1>,
    ) -> Result<(), VectorGenerationStoreErrorV1> {
        self.mutate_retiring_state(|state| state.deactivate_generation(expected_active_generation))
            .await
    }

    pub async fn active_generation_id(
        &self,
    ) -> Result<Option<VectorGenerationIdV1>, VectorGenerationStoreErrorV1> {
        let (_, state, _) = self.load_state().await?;
        Ok(state.active_generation_id().cloned())
    }

    /// The checkpoint of one pending build, or `None` when no build is pending
    /// under that identity yet.
    pub async fn pending_checkpoint(
        &self,
        build_id: &VectorGenerationBuildIdV1,
    ) -> Result<Option<VectorProjectionCheckpointV1>, VectorGenerationStoreErrorV1> {
        let (_, state, _) = self.load_state().await?;
        Ok(state.pending_checkpoint(build_id).cloned())
    }

    pub async fn active_checkpoint(
        &self,
    ) -> Result<Option<VectorProjectionCheckpointV1>, VectorGenerationStoreErrorV1> {
        let (_, state, _) = self.load_state().await?;
        Ok(state.active_checkpoint().cloned())
    }

    pub async fn active_generation(
        &self,
    ) -> Result<Option<PublishedVectorGenerationV1>, VectorGenerationStoreErrorV1> {
        let (_, state, _) = self.load_state().await?;
        Ok(state.active_generation().cloned())
    }

    pub async fn active_generation_for(
        &self,
        embedding_key: &AdmittedEmbeddingProjectionKeyV1,
        source_generation: &CodeGenerationId,
        source_manifest_digest: &ManifestDigest,
    ) -> Result<Option<PublishedVectorGenerationV1>, VectorGenerationStoreErrorV1> {
        let (_, state, _) = self.load_state().await?;
        Ok(state
            .active_generation_for(embedding_key, source_generation, source_manifest_digest)
            .cloned())
    }

    pub async fn generation(
        &self,
        generation_id: &VectorGenerationIdV1,
    ) -> Result<Option<PublishedVectorGenerationV1>, VectorGenerationStoreErrorV1> {
        let (_, state, _) = self.load_state().await?;
        Ok(state.generation(generation_id).cloned())
    }

    pub async fn physical_vector_values(
        &self,
        generation_id: &VectorGenerationIdV1,
        chunk_id: &CodeSearchChunkId,
    ) -> Result<Option<Arc<[f32]>>, VectorGenerationStoreErrorV1> {
        let (_, state, _) = self.load_state().await?;
        Ok(state.physical_vector_values(generation_id, chunk_id))
    }

    async fn mutate_state<ResultValue>(
        &self,
        mutation: impl FnMut(
            &mut VectorGenerationState,
        ) -> Result<ResultValue, VectorGenerationStoreErrorV1>,
    ) -> Result<ResultValue, VectorGenerationStoreErrorV1> {
        self.mutate_state_with_reclamation(false, mutation).await
    }

    /// As [`Self::mutate_state`], but also reclaims metadata slices the committed
    /// state no longer references. Used by the mutations that retire pending or
    /// published generations.
    async fn mutate_retiring_state<ResultValue>(
        &self,
        mutation: impl FnMut(
            &mut VectorGenerationState,
        ) -> Result<ResultValue, VectorGenerationStoreErrorV1>,
    ) -> Result<ResultValue, VectorGenerationStoreErrorV1> {
        self.mutate_state_with_reclamation(true, mutation).await
    }

    async fn mutate_state_with_reclamation<ResultValue>(
        &self,
        reclaim_unreferenced: bool,
        mut mutation: impl FnMut(
            &mut VectorGenerationState,
        ) -> Result<ResultValue, VectorGenerationStoreErrorV1>,
    ) -> Result<ResultValue, VectorGenerationStoreErrorV1> {
        let transaction = self
            .database
            .begin_write_transaction(VECTOR_GENERATION_STATE_OPERATION)
            .await
            .map_err(storage_error)?;
        let (revision, mut state, load) = self.load_state_from_transaction(&transaction).await?;
        let result = mutation(&mut state)?;
        let pending_slices = seal_external_state(&mut state, &load.durable_slices)?;
        let state_json = serde_json::to_string(&state).map_err(storage_error)?;
        write_state_slices(&transaction, VECTOR_STATE_SLICE_TABLE_V1, &pending_slices).await?;
        if reclaim_unreferenced {
            let referenced = referenced_state_addresses(&mut state)?;
            prune_unreferenced_state_slices(&transaction, VECTOR_STATE_SLICE_TABLE_V1, &referenced)
                .await?;
        }
        let changed = transaction
            .execute_engine(
                "UPDATE semantic_vector_generation_state
                 SET revision = revision + 1, state_json = ?1
                 WHERE singleton = 1 AND revision = ?2",
                params![state_json, revision],
            )
            .await
            .map_err(storage_error)?;
        if changed != 1 {
            transaction.rollback().await.map_err(storage_error)?;
            return Err(VectorGenerationStoreErrorV1::ConcurrentMutation);
        }
        transaction.commit().await.map_err(storage_error)?;
        Ok(result)
    }

    async fn load_state(
        &self,
    ) -> Result<(i64, VectorGenerationState, VectorStateLoad), VectorGenerationStoreErrorV1> {
        let mut rows = self
            .database
            .engine_conn()
            .query(
                "SELECT revision, state_json
                 FROM semantic_vector_generation_state
                 WHERE singleton = 1",
                (),
            )
            .await
            .map_err(storage_error)?;
        let row = rows.next().await.map_err(storage_error)?.ok_or_else(|| {
            VectorGenerationStoreErrorV1::Storage(
                "vector generation state row is missing".to_string(),
            )
        })?;
        let revision = row.get::<i64>(0).map_err(storage_error)?;
        let state_json = row.get::<String>(1).map_err(storage_error)?;
        drop(rows);
        let mut state: VectorGenerationState =
            serde_json::from_str(&state_json).map_err(storage_error)?;
        drop(state_json);
        let (durable_slices, _) =
            hydrate_external_state(self.database, VECTOR_STATE_SLICE_TABLE_V1, &mut state).await?;
        let mut load = VectorStateLoad::default();
        load.durable_slices = durable_slices;
        validate_loaded_state(&state)?;
        Ok((revision, state, load))
    }

    async fn load_state_from_transaction(
        &self,
        transaction: &tracedecay_runtime_core::db::DatabaseWriteTransaction<'_>,
    ) -> Result<(i64, VectorGenerationState, VectorStateLoad), VectorGenerationStoreErrorV1> {
        let mut rows = transaction
            .query_engine(
                "SELECT revision, state_json
                 FROM semantic_vector_generation_state
                 WHERE singleton = 1",
                (),
            )
            .await
            .map_err(storage_error)?;
        let row = rows.next().await.map_err(storage_error)?.ok_or_else(|| {
            VectorGenerationStoreErrorV1::Storage(
                "vector generation state row is missing".to_owned(),
            )
        })?;
        let revision = row.get::<i64>(0).map_err(storage_error)?;
        let state_json = row.get::<String>(1).map_err(storage_error)?;
        drop(rows);
        let mut state: VectorGenerationState =
            serde_json::from_str(&state_json).map_err(storage_error)?;
        let (durable_slices, _) =
            hydrate_external_state(self.database, VECTOR_STATE_SLICE_TABLE_V1, &mut state).await?;
        validate_loaded_state(&state)?;
        Ok((
            revision,
            state,
            VectorStateLoad {
                durable_slices,
                ..VectorStateLoad::default()
            },
        ))
    }
}

impl<'database> DatabaseVectorEvaluationStoreV1<'database> {
    pub(crate) async fn open(
        database: &'database Database,
        graph: Arc<GraphDb>,
        evaluation_id: impl Into<String>,
    ) -> Result<Self, VectorGenerationStoreErrorV1> {
        let evaluation_id = evaluation_id.into();
        if evaluation_id.is_empty()
            || evaluation_id.len() > 256
            || evaluation_id.trim() != evaluation_id
            || evaluation_id.chars().any(char::is_control)
        {
            return Err(VectorGenerationStoreErrorV1::Storage(
                "semantic evaluation identity is invalid".to_owned(),
            ));
        }
        database
            .execute_write_batch(
                VECTOR_GENERATION_STATE_OPERATION,
                VECTOR_EVALUATION_STATE_SCHEMA_V1,
            )
            .await
            .map_err(storage_error)?;
        database
            .execute_write_batch(
                VECTOR_GENERATION_STATE_OPERATION,
                VECTOR_EVALUATION_STATE_SLICE_SCHEMA_V1,
            )
            .await
            .map_err(storage_error)?;
        let initial_state =
            serde_json::to_string(&VectorGenerationState::default()).map_err(storage_error)?;
        let inserted = database
            .execute_write_engine(
                VECTOR_GENERATION_STATE_OPERATION,
                "INSERT INTO semantic_vector_evaluation_state (
                    evaluation_id, revision, state_json
                 ) VALUES (?1, 0, ?2)",
                params![evaluation_id.clone(), initial_state],
            )
            .await
            .map_err(storage_error)?;
        if inserted != 1 {
            return Err(VectorGenerationStoreErrorV1::Storage(
                "semantic evaluation state could not be initialized".to_owned(),
            ));
        }
        Ok(Self {
            database,
            evaluation_id,
        })
    }

    pub(crate) async fn rebuild_generation(
        &self,
        plan: VectorGenerationPlanV1,
    ) -> Result<VectorGenerationBuildIdV1, VectorGenerationStoreErrorV1> {
        self.mutate_state(|state| state.rebuild_generation(plan.clone()))
            .await
    }

    pub(crate) async fn commit_batch(
        &self,
        build_id: &VectorGenerationBuildIdV1,
        expected_checkpoint: Option<&VectorProjectionCheckpointV1>,
        prepared: PreparedVectorGenerationV1,
    ) -> Result<VectorProjectionCheckpointV1, VectorGenerationStoreErrorV1> {
        self.mutate_state(|state| {
            state.commit_batch(build_id, expected_checkpoint, prepared.clone())
        })
        .await
    }

    pub(crate) async fn publish_generation(
        &self,
        build_id: &VectorGenerationBuildIdV1,
        expected_active_generation: Option<&VectorGenerationIdV1>,
    ) -> Result<VectorGenerationPublicationV1, VectorGenerationStoreErrorV1> {
        self.mutate_state(|state| state.publish_generation(build_id, expected_active_generation))
            .await
    }

    pub(crate) async fn cancel_generation(
        &self,
        build_id: &VectorGenerationBuildIdV1,
    ) -> Result<bool, VectorGenerationStoreErrorV1> {
        self.mutate_state(|state| Ok(state.cancel_generation(build_id)))
            .await
    }

    pub(crate) async fn active_generation_id(
        &self,
    ) -> Result<Option<VectorGenerationIdV1>, VectorGenerationStoreErrorV1> {
        let (_, state, _) = self.load_state().await?;
        Ok(state.active_generation_id().cloned())
    }

    pub(crate) async fn active_generation_for(
        &self,
        embedding_key: &AdmittedEmbeddingProjectionKeyV1,
        source_generation: &CodeGenerationId,
        source_manifest_digest: &ManifestDigest,
    ) -> Result<Option<PublishedVectorGenerationV1>, VectorGenerationStoreErrorV1> {
        let (_, state, _) = self.load_state().await?;
        Ok(state
            .active_generation_for(embedding_key, source_generation, source_manifest_digest)
            .cloned())
    }

    pub(crate) async fn close(self) -> Result<(), VectorGenerationStoreErrorV1> {
        let transaction = self
            .database
            .begin_write_transaction(VECTOR_GENERATION_STATE_OPERATION)
            .await
            .map_err(storage_error)?;
        let deleted = transaction
            .execute_engine(
                "DELETE FROM semantic_vector_evaluation_state
                 WHERE evaluation_id = ?1",
                params![&self.evaluation_id],
            )
            .await
            .map_err(storage_error)?;
        if deleted != 1 {
            transaction.rollback().await.map_err(storage_error)?;
            return Err(VectorGenerationStoreErrorV1::ConcurrentMutation);
        }
        transaction
            .execute_engine(
                "DELETE FROM semantic_vector_evaluation_state_slice
                 WHERE NOT EXISTS (SELECT 1 FROM semantic_vector_evaluation_state)",
                (),
            )
            .await
            .map_err(storage_error)?;
        transaction.commit().await.map_err(storage_error)?;
        Ok(())
    }

    async fn mutate_state<ResultValue>(
        &self,
        mut mutation: impl FnMut(
            &mut VectorGenerationState,
        ) -> Result<ResultValue, VectorGenerationStoreErrorV1>,
    ) -> Result<ResultValue, VectorGenerationStoreErrorV1> {
        let transaction = self
            .database
            .begin_write_transaction(VECTOR_GENERATION_STATE_OPERATION)
            .await
            .map_err(storage_error)?;
        let (revision, mut state, load) = self.load_state().await?;
        let result = mutation(&mut state)?;
        let pending_slices = seal_external_state(&mut state, &load.durable_slices)?;
        let state_json = serde_json::to_string(&state).map_err(storage_error)?;
        write_state_slices(
            &transaction,
            VECTOR_EVALUATION_STATE_SLICE_TABLE_V1,
            &pending_slices,
        )
        .await?;
        let changed = transaction
            .execute_engine(
                "UPDATE semantic_vector_evaluation_state
                 SET revision = revision + 1, state_json = ?1
                 WHERE evaluation_id = ?2 AND revision = ?3",
                params![state_json, self.evaluation_id.clone(), revision],
            )
            .await
            .map_err(storage_error)?;
        if changed != 1 {
            transaction.rollback().await.map_err(storage_error)?;
            return Err(VectorGenerationStoreErrorV1::ConcurrentMutation);
        }
        transaction.commit().await.map_err(storage_error)?;
        Ok(result)
    }

    async fn load_state(
        &self,
    ) -> Result<(i64, VectorGenerationState, VectorStateLoad), VectorGenerationStoreErrorV1> {
        let mut rows = self
            .database
            .engine_conn()
            .query(
                "SELECT revision, state_json
                 FROM semantic_vector_evaluation_state
                 WHERE evaluation_id = ?1",
                params![self.evaluation_id.clone()],
            )
            .await
            .map_err(storage_error)?;
        let row = rows.next().await.map_err(storage_error)?.ok_or_else(|| {
            VectorGenerationStoreErrorV1::Storage(
                "semantic evaluation state row is missing".to_owned(),
            )
        })?;
        let revision = row.get::<i64>(0).map_err(storage_error)?;
        let state_json = row.get::<String>(1).map_err(storage_error)?;
        drop(rows);
        let mut state: VectorGenerationState =
            serde_json::from_str(&state_json).map_err(storage_error)?;
        drop(state_json);
        let (durable_slices, _) = hydrate_external_state(
            self.database,
            VECTOR_EVALUATION_STATE_SLICE_TABLE_V1,
            &mut state,
        )
        .await?;
        let mut load = VectorStateLoad::default();
        load.durable_slices = durable_slices;
        validate_loaded_state(&state)?;
        Ok((revision, state, load))
    }
}

impl VectorGenerationState {
    /// Rebuild the derived physical-byte index for every published generation.
    ///
    /// The generation map is moved aside rather than cloned: interning only
    /// touches `physical_vectors` and `physical_vector_bindings`, so a deep
    /// copy of every published generation — the whole float corpus, once per
    /// load — bought nothing but the borrow.
    /// In-memory Grafeo stand-in used by state-machine restart tests.
    #[cfg(test)]
    fn hydrate_from(&mut self, reference: &Self) {
        let mut payloads = BTreeMap::new();
        reference.visit_vectors(&mut |vector| {
            payloads.insert(vector.output_digest.clone(), vector.values.clone());
        });
        self.visit_vectors_mut(&mut |vector| {
            if vector.values.is_empty()
                && let Some(values) = payloads.get(&vector.output_digest)
            {
                vector.values.clone_from(values);
            }
        });
    }

    fn ensure_physical_reuse_index(&mut self) -> Result<(), VectorGenerationStoreErrorV1> {
        let generations = std::mem::take(&mut self.published.generations);
        let mut outcome = Ok(());
        for generation in generations.values() {
            outcome = intern_generation_vectors(
                &self.physical_vector_pool,
                &mut self.published,
                generation,
            );
            if outcome.is_err() {
                break;
            }
        }
        self.published.generations = generations;
        outcome
    }
}

fn physical_vector_reuse_key(
    embedding_key: &AdmittedEmbeddingProjectionKeyV1,
    vector: &ProjectedChunkVectorV1,
) -> Result<(ManifestDigest, PhysicalVectorReuseKeyV1), VectorGenerationStoreErrorV1> {
    if embedding_key.projection_key() != &vector.projection_key {
        return Err(VectorGenerationStoreErrorV1::BatchIdentityMismatch);
    }
    let reuse_key = PhysicalVectorReuseKeyV1 {
        canonical_chunk_digest: vector.chunk_digest.clone(),
        projection_key: vector.projection_key.clone(),
        admitted_embedding_key: embedding_key.clone(),
        privacy_domain: embedding_key.privacy_domain().clone(),
        privacy_key_epoch: embedding_key.privacy_key_epoch(),
    };
    let physical_id = canonical_sha256(&(PHYSICAL_VECTOR_REUSE_DIGEST_DOMAIN, &reuse_key))
        .map_err(|error| VectorGenerationStoreErrorV1::Storage(error.to_string()))?;
    Ok((physical_id, reuse_key))
}

fn intern_generation_vectors(
    physical_vector_pool: &PhysicalVectorBytePoolV1,
    published: &mut PublishedStateV1,
    generation: &PublishedVectorGenerationV1,
) -> Result<(), VectorGenerationStoreErrorV1> {
    let mut bindings = BTreeMap::new();
    for (chunk_id, vector) in generation.vectors.iter() {
        let (physical_id, reuse_key) =
            physical_vector_reuse_key(&generation.embedding_key, vector)?;
        match published.physical_vectors.get(&physical_id) {
            Some(existing)
                if existing.reuse_key != reuse_key
                    || existing.values.0.as_ref() != vector.values.as_slice() =>
            {
                return Err(VectorGenerationStoreErrorV1::PhysicalVectorConflict);
            }
            Some(_) => {}
            None => {}
        }
        let shared = physical_vector_pool.intern(&reuse_key, &vector.values)?;
        published.physical_vectors.insert(
            physical_id.clone(),
            PhysicalVectorPayloadV1 {
                reuse_key,
                values: SharedVectorBytesV1(shared),
            },
        );
        bindings.insert(chunk_id.clone(), physical_id);
    }
    match published
        .physical_vector_bindings
        .get(generation.generation_id())
    {
        Some(existing) if **existing != bindings => {
            Err(VectorGenerationStoreErrorV1::ImmutableGenerationConflict)
        }
        Some(_) => Ok(()),
        None => {
            published
                .physical_vector_bindings
                .insert(generation.generation_id().clone(), bindings.into());
            Ok(())
        }
    }
}

fn validate_loaded_state(
    state: &VectorGenerationState,
) -> Result<(), VectorGenerationStoreErrorV1> {
    if let Some(active) = &state.published.active_generation
        && !state.published.generations.contains_key(active)
    {
        return Err(VectorGenerationStoreErrorV1::Storage(
            "active vector generation pointer is dangling".to_string(),
        ));
    }
    for (generation_id, generation) in &state.published.generations {
        if generation.generation_id() != generation_id {
            return Err(VectorGenerationStoreErrorV1::Storage(
                "published generation map key does not match record id".to_string(),
            ));
        }
        generation.validate_persisted()?;
    }
    for pending in state.pending.values() {
        if let Some(embedding_key) = &pending.embedding_key {
            for vector in pending.vectors.values() {
                validate_vector_row(&pending.plan, embedding_key, vector)?;
            }
        }
        let canonical = pending.tombstones.keys().cloned().collect::<BTreeSet<_>>();
        if pending.tombstones.len() != canonical.len() {
            return Err(VectorGenerationStoreErrorV1::Storage(
                "pending tombstones contain duplicate chunk ids".to_string(),
            ));
        }
        for chunk_id in pending.tombstones.keys() {
            if pending.vectors.contains_key(chunk_id) {
                return Err(VectorGenerationStoreErrorV1::Storage(format!(
                    "pending generation retains both vector and tombstone for {chunk_id}"
                )));
            }
        }
    }
    Ok(())
}

fn validate_published_receipts(
    generation: &PublishedVectorGenerationV1,
) -> Result<(), VectorGenerationStoreErrorV1> {
    let checkpoint = generation.checkpoint();
    if checkpoint.target_projection_key != *generation.projection_key()
        || checkpoint.source_generation != *generation.source_generation()
        || checkpoint.source_manifest_digest != *generation.source_manifest_digest()
        || checkpoint.completed_batches == 0
        || checkpoint.completed_batches != generation.receipts().len() as u64
    {
        return Err(VectorGenerationStoreErrorV1::Storage(
            "published generation checkpoint is incomplete or incompatible".to_owned(),
        ));
    }
    let last = generation.receipts().last().ok_or_else(|| {
        VectorGenerationStoreErrorV1::Storage(
            "published generation has no projection receipt".to_owned(),
        )
    })?;
    if checkpoint.last_request_digest.as_ref() != Some(&last.request_digest)
        || checkpoint.last_publication_digest.as_ref() != Some(&last.publication_digest)
    {
        return Err(VectorGenerationStoreErrorV1::Storage(
            "published generation checkpoint does not name its last receipt".to_owned(),
        ));
    }

    let mut effects = BTreeSet::new();
    for batch in generation.receipts() {
        if batch.target_projection_key != *generation.projection_key()
            || batch.source_generation != *generation.source_generation()
            || expected_publication_digest(batch).map_err(storage_error)?
                != batch.publication_digest
            || batch.reused_count
                != batch
                    .receipts
                    .iter()
                    .filter(|receipt| receipt.operation == ProjectionOperationV1::Reused)
                    .count() as u64
        {
            return Err(VectorGenerationStoreErrorV1::Storage(
                "published projection batch receipt is incompatible".to_owned(),
            ));
        }
        for receipt in &batch.receipts {
            if !effects.insert(receipt.chunk_id.clone())
                || receipt.projection_key != *generation.projection_key()
                || receipt.request_digest != batch.request_digest
                || receipt.source_generation != *generation.source_generation()
                || receipt.source_manifest_digest != batch.source_manifest_digest
            {
                return Err(VectorGenerationStoreErrorV1::Storage(
                    "published chunk receipt is duplicated or incompatible".to_owned(),
                ));
            }
            match receipt.operation {
                ProjectionOperationV1::Added | ProjectionOperationV1::Updated => {
                    let vector = generation.vectors().get(&receipt.chunk_id);
                    if receipt.outcome != ProjectionOutcomeV1::Applied
                        || vector.is_none()
                        || receipt.current_chunk_digest.as_ref()
                            != vector.map(|vector| &vector.chunk_digest)
                        || receipt.output_digest.as_ref()
                            != vector.map(|vector| &vector.output_digest)
                        || generation
                            .tombstone_digests()
                            .contains_key(&receipt.chunk_id)
                    {
                        return Err(VectorGenerationStoreErrorV1::Storage(
                            "published applied receipt has no matching vector".to_owned(),
                        ));
                    }
                }
                ProjectionOperationV1::Reused => {
                    let vector = generation.vectors().get(&receipt.chunk_id);
                    if receipt.outcome != ProjectionOutcomeV1::Reused
                        || vector.is_none()
                        || receipt.prior_chunk_digest.as_ref()
                            != vector.map(|vector| &vector.chunk_digest)
                        || receipt.current_chunk_digest.as_ref()
                            != vector.map(|vector| &vector.chunk_digest)
                        || receipt.output_digest.is_some()
                        || generation
                            .tombstone_digests()
                            .contains_key(&receipt.chunk_id)
                    {
                        return Err(VectorGenerationStoreErrorV1::Storage(
                            "published reused receipt has no matching vector".to_owned(),
                        ));
                    }
                }
                ProjectionOperationV1::Deleted => {
                    if receipt.outcome != ProjectionOutcomeV1::Applied
                        || receipt.current_chunk_digest.is_some()
                        || receipt.output_digest.is_some()
                        || receipt.prior_chunk_digest.as_ref()
                            != generation.tombstone_digests().get(&receipt.chunk_id)
                        || generation.vectors().contains_key(&receipt.chunk_id)
                    {
                        return Err(VectorGenerationStoreErrorV1::Storage(
                            "published deletion receipt has no matching tombstone".to_owned(),
                        ));
                    }
                }
            }
        }
    }

    let expected_effects = generation
        .vectors()
        .keys()
        .chain(generation.tombstone_digests().keys())
        .cloned()
        .collect::<BTreeSet<_>>();
    if effects != expected_effects {
        return Err(VectorGenerationStoreErrorV1::Storage(
            "published generation receipt membership is incomplete".to_owned(),
        ));
    }
    Ok(())
}

#[derive(Debug, Default)]
struct VectorStateLoad {
    durable_slices: BTreeSet<ContentDigest>,
}

impl VectorGenerationState {
    #[cfg(test)]
    fn visit_vectors<'state>(&'state self, visit: &mut impl FnMut(&'state ProjectedChunkVectorV1)) {
        for generation in self.published.generations.values() {
            for vector in generation.vectors.values() {
                visit(vector);
            }
        }
        for pending in self.pending.values() {
            for vector in pending.vectors.values() {
                visit(vector);
            }
            for batch in pending.batches.iter() {
                for vector in &batch.vectors {
                    visit(vector);
                }
            }
        }
    }

    /// Refill the elided float payload of every vector row.
    ///
    /// Every write here goes through [`ExternalCollection::elided_mut`]: the
    /// externalized encoding does not carry floats, so restoring them leaves
    /// the stored bytes — and therefore the collection address — unchanged.
    fn visit_vectors_mut(&mut self, visit: &mut impl FnMut(&mut ProjectedChunkVectorV1)) {
        for generation in self.published.generations.values_mut() {
            for vector in generation.vectors.elided_mut().values_mut() {
                visit(vector);
            }
        }
        for pending in self.pending.values_mut() {
            for vector in pending.vectors.elided_mut().values_mut() {
                visit(vector);
            }
            for batch in pending.batches.elided_mut().iter_mut() {
                for vector in &mut batch.vectors {
                    visit(vector);
                }
            }
        }
    }
}

/// Seal a hand-built fixture so its document can be serialized.
///
/// The store's own writers seal inside their mutation path; fixtures that
/// build state directly go through here instead.
#[cfg(test)]
fn seal_test_state(state: &mut VectorGenerationState) -> BTreeMap<ContentDigest, Vec<Vec<u8>>> {
    seal_external_state(state, &BTreeSet::new()).expect("seal externalized state")
}

/// Install collection slices for a hand-built fixture state.
#[cfg(test)]
async fn install_test_state_slices(
    database: &Database,
    slice_table: &str,
    state: &mut VectorGenerationState,
) {
    let pending = seal_test_state(state);
    let transaction = database
        .begin_write_transaction("install test state slices")
        .await
        .expect("slice writer");
    write_state_slices(&transaction, slice_table, &pending)
        .await
        .expect("install test state slices");
    transaction.commit().await.expect("commit test slices");
}

/// Round-trip the state document the way a restart does, standing in for the
/// relational slices and Grafeo projections with the reference state still in
/// memory.
#[cfg(test)]
fn restart_round_trip(state: &mut VectorGenerationState) -> VectorGenerationState {
    let sealed = seal_test_state(state);
    let encoded = serde_json::to_string(&*state).expect("serialize vector state");
    let mut restarted: VectorGenerationState =
        serde_json::from_str(&encoded).expect("deserialize vector state");
    fill_from_sealed(&mut restarted, &sealed);
    restarted.hydrate_from(state);
    restarted
}

#[cfg(test)]
fn fill_from_sealed(
    state: &mut VectorGenerationState,
    sealed: &BTreeMap<ContentDigest, Vec<Vec<u8>>>,
) {
    state
        .visit_external_slots(&mut |slot| {
            let Some(address) = slot.address().cloned() else {
                return Ok(());
            };
            slot.fill(sealed.get(&address).expect("sealed collection"))
        })
        .expect("fill externalized collections");
}

type ExternalSlotVisit<'visit> =
    dyn FnMut(&mut dyn ExternalSlot) -> Result<(), VectorGenerationStoreErrorV1> + 'visit;

impl PublishedVectorGenerationV1 {
    fn visit_external_slots(
        &mut self,
        visit: &mut ExternalSlotVisit<'_>,
    ) -> Result<(), VectorGenerationStoreErrorV1> {
        visit(&mut self.vectors)?;
        visit(&mut self.tombstones)?;
        visit(&mut self.tombstone_digests)?;
        visit(&mut self.receipts)
    }
}

impl VectorGenerationState {
    /// Every externalized collection in the state document, in a stable order.
    fn visit_external_slots(
        &mut self,
        visit: &mut ExternalSlotVisit<'_>,
    ) -> Result<(), VectorGenerationStoreErrorV1> {
        for pending in self.pending.values_mut() {
            visit(&mut pending.plan.expected_chunk_ids)?;
            visit(&mut pending.vectors)?;
            visit(&mut pending.tombstones)?;
            visit(&mut pending.batches)?;
            visit(&mut pending.committed_chunk_effects)?;
        }
        for generation in self.published.generations.values_mut() {
            generation.visit_external_slots(visit)?;
        }
        for bindings in self.published.physical_vector_bindings.values_mut() {
            visit(bindings)?;
        }
        Ok(())
    }
}

/// Seal every externalized collection and collect the slices to write.
///
/// A slot whose address is already durable is left alone, so a mutation
/// re-encodes only what it actually changed: committing one batch writes that
/// batch's slices, not the corpus. Content addressing then makes publication
/// free — the pending collections and the published ones they become hash to
/// the same addresses, which are durable by then.
fn seal_external_state(
    state: &mut VectorGenerationState,
    durable: &BTreeSet<ContentDigest>,
) -> Result<BTreeMap<ContentDigest, Vec<Vec<u8>>>, VectorGenerationStoreErrorV1> {
    let mut pending: BTreeMap<ContentDigest, Vec<Vec<u8>>> = BTreeMap::new();
    state.visit_external_slots(&mut |slot| {
        let sealed =
            slot.seal(&mut |address| !durable.contains(address) && !pending.contains_key(address))?;
        if let Some((address, slices)) = sealed {
            pending.insert(address, slices);
        }
        Ok(())
    })?;
    Ok(pending)
}

/// Address every externalized collection the committed state still references.
fn referenced_state_addresses(
    state: &mut VectorGenerationState,
) -> Result<BTreeSet<ContentDigest>, VectorGenerationStoreErrorV1> {
    let mut referenced = BTreeSet::new();
    state.visit_external_slots(&mut |slot| {
        if let Some(address) = slot.address() {
            referenced.insert(address.clone());
        }
        Ok(())
    })?;
    Ok(referenced)
}

/// Fill every externalized collection in `state` from `slice_table`.
///
/// Collections are resolved one address at a time so a whole-corpus load never
/// holds every encoded collection at once, and each is verified against its
/// content address before it is parsed. A missing address fails closed.
async fn hydrate_external_state(
    database: &Database,
    slice_table: &str,
    state: &mut VectorGenerationState,
) -> Result<(BTreeSet<ContentDigest>, bool), VectorGenerationStoreErrorV1> {
    let mut wanted = BTreeSet::new();
    let mut inline = false;
    state.visit_external_slots(&mut |slot| {
        match slot.address() {
            Some(address) => {
                wanted.insert(address.clone());
            }
            None => inline = true,
        }
        Ok(())
    })?;
    for address in &wanted {
        let slices = read_state_slices(database, slice_table, address).await?;
        state.visit_external_slots(&mut |slot| {
            if slot.address() == Some(address) {
                slot.fill(&slices)?;
            }
            Ok(())
        })?;
    }
    Ok((wanted, inline))
}

/// Fill one standalone published generation read outside the writer lane.
async fn hydrate_generation_slices(
    database: &Database,
    slice_table: &str,
    generation: &mut PublishedVectorGenerationV1,
) -> Result<(), VectorGenerationStoreErrorV1> {
    let mut wanted = BTreeSet::new();
    generation.visit_external_slots(&mut |slot| {
        if let Some(address) = slot.address() {
            wanted.insert(address.clone());
        }
        Ok(())
    })?;
    for address in &wanted {
        let slices = read_state_slices(database, slice_table, address).await?;
        generation.visit_external_slots(&mut |slot| {
            if slot.address() == Some(address) {
                slot.fill(&slices)?;
            }
            Ok(())
        })?;
    }
    Ok(())
}

/// Read one collection's slices in ordinal order.
///
/// Paged by ordinal rather than read as one statement: a whole-corpus
/// collection has more slices than a single query may materialize, and the
/// runtime refuses such a statement outright rather than truncating it. Paging
/// keeps every statement bounded no matter how large the collection grows.
async fn read_state_slices(
    database: &Database,
    slice_table: &str,
    address: &ContentDigest,
) -> Result<Vec<Vec<u8>>, VectorGenerationStoreErrorV1> {
    let connection = database.engine_conn();
    let sql = format!(
        "SELECT ordinal, payload
         FROM {slice_table}
         WHERE collection_digest = ?1 AND ordinal >= ?2 AND ordinal < ?3
         ORDER BY ordinal"
    );
    let mut slices = Vec::new();
    loop {
        let start = i64::try_from(slices.len()).map_err(storage_error)?;
        let end = start
            .checked_add(i64::try_from(VECTOR_STATE_SLICE_READ_ROWS).map_err(storage_error)?)
            .ok_or_else(|| {
                VectorGenerationStoreErrorV1::Storage(
                    "externalized state collection is implausibly large".to_owned(),
                )
            })?;
        let mut rows = connection
            .query(&sql, params![address.as_str(), start, end])
            .await
            .map_err(storage_error)?;
        let mut read = 0_usize;
        while let Some(row) = rows.next().await.map_err(storage_error)? {
            let ordinal = row.get::<i64>(0).map_err(storage_error)?;
            if usize::try_from(ordinal).ok() != Some(slices.len()) {
                return Err(VectorGenerationStoreErrorV1::Storage(format!(
                    "externalized state collection {address} has a gap in its slices"
                )));
            }
            slices.push(row.get::<Vec<u8>>(1).map_err(storage_error)?);
            read += 1;
        }
        drop(rows);
        if read < VECTOR_STATE_SLICE_READ_ROWS {
            break;
        }
    }
    if slices.is_empty() {
        return Err(VectorGenerationStoreErrorV1::Storage(format!(
            "externalized state collection {address} is missing from the store"
        )));
    }
    Ok(slices)
}

/// Persist sealed collection slices inside the caller's transaction.
///
/// Rows are content-addressed and inserted with `OR IGNORE`, so a retried
/// commit is a no-op rather than a conflict, and every statement carries a
/// bounded number of bounded slices.
async fn write_state_slices(
    transaction: &tracedecay_runtime_core::db::DatabaseWriteTransaction<'_>,
    slice_table: &str,
    pending: &BTreeMap<ContentDigest, Vec<Vec<u8>>>,
) -> Result<(), VectorGenerationStoreErrorV1> {
    let rows = pending
        .iter()
        .flat_map(|(address, slices)| {
            slices
                .iter()
                .enumerate()
                .map(move |(ordinal, payload)| (address, ordinal, payload))
        })
        .collect::<Vec<_>>();
    for group in rows.chunks(VECTOR_STATE_SLICE_STATEMENT_ROWS) {
        let tuples = (0..group.len())
            .map(|index| {
                let base = index * 3;
                format!("(?{}, ?{}, ?{})", base + 1, base + 2, base + 3)
            })
            .collect::<Vec<_>>()
            .join(", ");
        let sql = format!(
            "INSERT OR IGNORE INTO {slice_table} (collection_digest, ordinal, payload)
             VALUES {tuples}"
        );
        let mut values = Vec::with_capacity(group.len() * 3);
        for (address, ordinal, payload) in group {
            values.push(tracedecay_runtime_core::db::engine::Value::Text(
                address.as_str().to_owned(),
            ));
            values.push(tracedecay_runtime_core::db::engine::Value::Integer(
                i64::try_from(*ordinal).map_err(storage_error)?,
            ));
            values.push(tracedecay_runtime_core::db::engine::Value::Blob(
                (*payload).clone(),
            ));
        }
        transaction
            .execute_engine(
                &sql,
                tracedecay_runtime_core::db::engine::params_from_iter(values),
            )
            .await
            .map_err(storage_error)?;
    }
    Ok(())
}

/// Delete collection slices the committed state no longer references.
async fn prune_unreferenced_state_slices(
    transaction: &tracedecay_runtime_core::db::DatabaseWriteTransaction<'_>,
    slice_table: &str,
    referenced: &BTreeSet<ContentDigest>,
) -> Result<(), VectorGenerationStoreErrorV1> {
    let scratch_table = format!("temp.{slice_table}_referenced");
    transaction
        .execute_batch_engine(&format!(
            "CREATE TEMP TABLE IF NOT EXISTS {slice_table}_referenced (
                 collection_digest TEXT PRIMARY KEY
             ) STRICT;
             DELETE FROM {scratch_table};"
        ))
        .await
        .map_err(storage_error)?;
    let addresses = referenced.iter().collect::<Vec<_>>();
    for group in addresses.chunks(VECTOR_STATE_ADDRESS_STATEMENT_ROWS) {
        let tuples = (1..=group.len())
            .map(|index| format!("(?{index})"))
            .collect::<Vec<_>>()
            .join(", ");
        let values = group
            .iter()
            .map(|address| {
                tracedecay_runtime_core::db::engine::Value::Text(address.as_str().to_owned())
            })
            .collect::<Vec<_>>();
        transaction
            .execute_engine(
                &format!(
                    "INSERT OR IGNORE INTO {scratch_table} (collection_digest) VALUES {tuples}"
                ),
                tracedecay_runtime_core::db::engine::params_from_iter(values),
            )
            .await
            .map_err(storage_error)?;
    }
    transaction
        .execute_engine(
            &format!(
                "DELETE FROM {slice_table}
                 WHERE NOT EXISTS (
                     SELECT 1 FROM {scratch_table}
                     WHERE {scratch_table}.collection_digest = {slice_table}.collection_digest
                 )"
            ),
            (),
        )
        .await
        .map_err(storage_error)?;
    transaction
        .execute_batch_engine(&format!("DELETE FROM {scratch_table};"))
        .await
        .map_err(storage_error)?;
    Ok(())
}

fn storage_error(error: impl std::fmt::Display) -> VectorGenerationStoreErrorV1 {
    VectorGenerationStoreErrorV1::Storage(error.to_string())
}

/// Derive the immutable vector-generation identity from projected content,
/// not from resumable execution evidence. Receipt batches and checkpoints
/// remain available for audit but must not change the generation they produced.
fn generation_identity_digest(
    plan: &VectorGenerationPlanV1,
    vectors: &BTreeMap<CodeSearchChunkId, ProjectedChunkVectorV1>,
    tombstones: &BTreeMap<CodeSearchChunkId, ContentDigest>,
) -> Result<ManifestDigest, VectorGenerationStoreErrorV1> {
    let vector_digests = vectors
        .iter()
        .map(|(chunk_id, vector)| (chunk_id, &vector.output_digest))
        .collect::<Vec<_>>();
    let tombstone_digests = tombstones.iter().collect::<Vec<_>>();
    canonical_sha256(&(
        VECTOR_GENERATION_MANIFEST_DIGEST_DOMAIN,
        &plan.target_projection_key,
        &plan.source_generation,
        &plan.source_manifest_digest,
        &plan.expected_chunk_ids,
        vector_digests,
        tombstone_digests,
    ))
    .map_err(|error| VectorGenerationStoreErrorV1::InvalidPlan(error.to_string()))
}

fn validate_plan(plan: &VectorGenerationPlanV1) -> Result<(), VectorGenerationStoreErrorV1> {
    if plan.target_projection_key.kind != ProjectionKindV1::Embedding {
        return Err(VectorGenerationStoreErrorV1::InvalidPlan(
            "target projection is not embedding".to_string(),
        ));
    }
    plan.source_generation
        .validate()
        .map_err(|error| VectorGenerationStoreErrorV1::InvalidPlan(error.to_string()))?;
    plan.source_manifest_digest
        .validate()
        .map_err(|error| VectorGenerationStoreErrorV1::InvalidPlan(error.to_string()))?;
    if plan
        .expected_chunk_ids
        .windows(2)
        .any(|pair| pair[0] >= pair[1])
    {
        return Err(VectorGenerationStoreErrorV1::InvalidPlan(
            "expected chunk IDs are not canonical".to_string(),
        ));
    }
    Ok(())
}

fn validate_batch_identity(
    plan: &VectorGenerationPlanV1,
    prepared: &PreparedVectorGenerationV1,
) -> Result<(), VectorGenerationStoreErrorV1> {
    if prepared.request.target_projection_key != plan.target_projection_key
        || prepared.receipt.target_projection_key != plan.target_projection_key
        || prepared.request.changes.to_generation != plan.source_generation
        || prepared.receipt.source_generation != plan.source_generation
    {
        return Err(VectorGenerationStoreErrorV1::BatchIdentityMismatch);
    }
    if prepared.embedding_key.projection_key() != &plan.target_projection_key {
        return Err(VectorGenerationStoreErrorV1::BatchIdentityMismatch);
    }
    Ok(())
}

fn validate_base_generation_for_batch(
    published: &PublishedStateV1,
    plan: &VectorGenerationPlanV1,
    prepared: &PreparedVectorGenerationV1,
) -> Result<(), VectorGenerationStoreErrorV1> {
    let Some(base_id) = plan.base_generation.as_ref() else {
        return Ok(());
    };
    let base = published
        .generations
        .get(base_id)
        .ok_or(VectorGenerationStoreErrorV1::IncompatibleBaseGeneration)?;
    if prepared.request.changes.from_generation.as_ref() != Some(base.source_generation())
        || prepared.request.previous_projection_key.as_ref() != Some(base.projection_key())
        || (prepared.request.target_projection_key == *base.projection_key()
            && prepared.embedding_key != *base.embedding_key())
    {
        return Err(VectorGenerationStoreErrorV1::IncompatibleBaseGeneration);
    }
    Ok(())
}

fn validate_prepared_vector_row(
    prepared: &PreparedVectorGenerationV1,
    vector: &ProjectedChunkVectorV1,
) -> Result<(), VectorGenerationStoreErrorV1> {
    if vector.projection_key != prepared.request.target_projection_key
        || vector.source_generation != prepared.request.changes.to_generation
        || vector.source_manifest_digest != prepared.request.changes.manifest_digest
    {
        return Err(VectorGenerationStoreErrorV1::BatchIdentityMismatch);
    }
    vector.validate(prepared.embedding_key.embedding_key().dimensions)?;
    Ok(())
}

fn validate_vector_row(
    plan: &VectorGenerationPlanV1,
    embedding_key: &AdmittedEmbeddingProjectionKeyV1,
    vector: &ProjectedChunkVectorV1,
) -> Result<(), VectorGenerationStoreErrorV1> {
    if vector.projection_key != plan.target_projection_key
        || vector.source_generation != plan.source_generation
        || vector.source_manifest_digest != plan.source_manifest_digest
    {
        return Err(VectorGenerationStoreErrorV1::BatchIdentityMismatch);
    }
    if !vector.values.is_empty() {
        vector.validate(embedding_key.embedding_key().dimensions)?;
    }
    Ok(())
}

fn validate_vector_row_for_published(
    generation: &PublishedVectorGenerationV1,
    vector: &ProjectedChunkVectorV1,
) -> Result<(), VectorGenerationStoreErrorV1> {
    if vector.projection_key != generation.projection_key
        || vector.source_generation != generation.source_generation
        || vector.source_manifest_digest != generation.source_manifest_digest
    {
        return Err(VectorGenerationStoreErrorV1::Storage(
            "published vector row identity drifted from generation metadata".to_string(),
        ));
    }
    if !vector.values.is_empty() {
        vector
            .validate(generation.embedding_key.embedding_key().dimensions)
            .map_err(|error| VectorGenerationStoreErrorV1::Storage(error.to_string()))?;
    }
    Ok(())
}

fn base_vector<'a>(
    published: &'a PublishedStateV1,
    plan: &VectorGenerationPlanV1,
    chunk_id: &CodeSearchChunkId,
) -> Result<&'a ProjectedChunkVectorV1, VectorGenerationStoreErrorV1> {
    let base_id = plan
        .base_generation
        .as_ref()
        .ok_or_else(|| VectorGenerationStoreErrorV1::MissingBaseVector(chunk_id.clone()))?;
    published
        .generations
        .get(base_id)
        .and_then(|generation| generation.vectors.get(chunk_id))
        .ok_or_else(|| VectorGenerationStoreErrorV1::MissingBaseVector(chunk_id.clone()))
}

fn validate_base_digest(
    published: &PublishedStateV1,
    plan: &VectorGenerationPlanV1,
    receipt: &tracedecay_domain::CodeChunkProjectionReceiptV1,
) -> Result<(), VectorGenerationStoreErrorV1> {
    let base = base_vector(published, plan, &receipt.chunk_id)?;
    if receipt.prior_chunk_digest.as_ref() != Some(&base.chunk_digest) {
        return Err(VectorGenerationStoreErrorV1::MissingBaseVector(
            receipt.chunk_id.clone(),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tracedecay_domain::{
        ChangedCodeChunkSetV1, ChangedCodeChunkV1, ChunkerRevision, EmbeddingDeviceClassV1,
        EmbeddingMetricV1, EmbeddingNormalizationV1, EmbeddingPoolingV1, EmbeddingPrecisionV1,
        EmbeddingProjectionKeyV1, EmbeddingTruncationSideV1, PrivacyDomainId,
        ProjectionBatchRequestV1, ProjectionReplayReasonV1,
    };
    use tracedecay_runtime_core::db::{DatabaseAuthority, TestDatabaseRuntimeMode};

    fn test_graph() -> Arc<GraphDb> {
        Arc::new(
            GraphDb::open(tracedecay_graph_db::GraphDbOpenOptions {
                location: tracedecay_graph_db::GraphDbLocation::Memory,
                expected_format: tracedecay_graph_db::GraphFormatVersion::new(1)
                    .expect("graph format"),
                durability: tracedecay_graph_db::GraphDurability::Memory,
                cancellation: Arc::new(NeverCancelled),
            })
            .expect("in-memory Grafeo vector authority"),
        )
    }

    fn id<T>(value: &str) -> T
    where
        T: TryFrom<String>,
        T::Error: std::fmt::Debug,
    {
        T::try_from(value.to_owned()).expect("canonical test identity")
    }

    fn manifest_digest(byte: char) -> ManifestDigest {
        id(&format!("sha256:{}", byte.to_string().repeat(64)))
    }

    fn content_digest(byte: char) -> ContentDigest {
        id(&format!("sha256:{}", byte.to_string().repeat(64)))
    }

    fn admitted_embedding() -> AdmittedEmbeddingProjectionKeyV1 {
        EmbeddingProjectionKeyV1 {
            model_artifact_digest: manifest_digest('1'),
            tokenizer_digest: manifest_digest('2'),
            config_digest: manifest_digest('3'),
            query_instruction_digest: Some(manifest_digest('4')),
            document_instruction_digest: Some(manifest_digest('5')),
            pooling: EmbeddingPoolingV1::Mean,
            truncation_side: EmbeddingTruncationSideV1::Right,
            truncation_length: 512,
            runtime_backend: "fastembed-ort".to_owned(),
            runtime_build_revision: "ort-test-rev-1".to_owned(),
            device_class: EmbeddingDeviceClassV1::Cpu,
            dimensions: 1,
            metric: EmbeddingMetricV1::Cosine,
            normalization: EmbeddingNormalizationV1::L2,
            precision: EmbeddingPrecisionV1::Fp32,
            chunk_schema_revision: "code-search-chunk.v1".to_owned(),
            chunker_revision: id::<ChunkerRevision>("chunker.v1"),
            privacy_domain: id::<PrivacyDomainId>("privacy.project-a"),
            privacy_key_epoch: 7,
        }
        .admit()
        .expect("admitted embedding fixture")
    }

    fn admitted_embedding_for(
        privacy_domain: &str,
        privacy_key_epoch: u64,
        runtime_build_revision: &str,
    ) -> AdmittedEmbeddingProjectionKeyV1 {
        let mut key = admitted_embedding().embedding_key().clone();
        key.privacy_domain = id(privacy_domain);
        key.privacy_key_epoch = privacy_key_epoch;
        key.runtime_build_revision = runtime_build_revision.to_owned();
        key.admit().expect("admitted embedding fixture variant")
    }

    fn logical_generation(
        generation_digest: char,
        embedding_key: AdmittedEmbeddingProjectionKeyV1,
        source_generation: &str,
        source_manifest_digest: char,
        chunk_id: &str,
        chunk_digest: char,
        values: Vec<f32>,
    ) -> PublishedVectorGenerationV1 {
        let projection_key = embedding_key.projection_key().clone();
        let source_generation: CodeGenerationId = id(source_generation);
        let source_manifest_digest = manifest_digest(source_manifest_digest);
        let chunk_id: CodeSearchChunkId = id(chunk_id);
        let chunk_digest = content_digest(chunk_digest);
        let output_digest = tracedecay_semantic::projector::vector_output_digest(
            &projection_key,
            &chunk_id,
            &chunk_digest,
            &values,
        )
        .expect("canonical vector output digest");
        let vectors = BTreeMap::from([(
            chunk_id.clone(),
            ProjectedChunkVectorV1 {
                projection_key: projection_key.clone(),
                source_generation: source_generation.clone(),
                source_manifest_digest: source_manifest_digest.clone(),
                chunk_id: chunk_id.clone(),
                chunk_digest: chunk_digest.clone(),
                values,
                output_digest: output_digest.clone(),
            },
        )]);
        let plan = VectorGenerationPlanV1 {
            target_projection_key: projection_key.clone(),
            source_generation: source_generation.clone(),
            source_manifest_digest: source_manifest_digest.clone(),
            expected_chunk_ids: vec![chunk_id.clone()].into(),
            base_generation: None,
        };
        let manifest_digest =
            generation_identity_digest(&plan, &vectors, &BTreeMap::new()).expect("manifest digest");
        let generation_id = VectorGenerationIdV1::new(manifest_digest.clone());
        let request_digest = manifest_digest_for_test_request(generation_digest);
        let mut batch = ProjectionBatchReceiptV1 {
            target_projection_key: projection_key.clone(),
            request_digest: request_digest.clone(),
            source_generation: source_generation.clone(),
            source_manifest_digest: source_manifest_digest.clone(),
            receipts: vec![tracedecay_domain::CodeChunkProjectionReceiptV1 {
                projection_key: projection_key.clone(),
                request_digest: request_digest.clone(),
                prior_generation: None,
                source_generation: source_generation.clone(),
                source_manifest_digest: source_manifest_digest.clone(),
                chunk_id,
                prior_chunk_digest: None,
                current_chunk_digest: Some(chunk_digest),
                operation: ProjectionOperationV1::Added,
                outcome: ProjectionOutcomeV1::Applied,
                output_digest: Some(output_digest),
            }],
            reused_count: 0,
            publication_digest: manifest_digest_for_test_request('0'),
        };
        batch.publication_digest = expected_publication_digest(&batch).expect("publication digest");
        let publication_digest = batch.publication_digest.clone();
        PublishedVectorGenerationV1 {
            generation_id: generation_id.clone(),
            graph_projection: graph_projection_id(
                "semantic-vector-generation",
                &manifest_digest_for_test_request(generation_digest),
            )
            .expect("graph projection"),
            projection_key: projection_key.clone(),
            source_generation: source_generation.clone(),
            source_manifest_digest: source_manifest_digest.clone(),
            base_generation: None,
            embedding_key,
            vectors: vectors.into(),
            tombstones: Vec::new().into(),
            tombstone_digests: BTreeMap::new().into(),
            receipts: vec![batch].into(),
            checkpoint: VectorProjectionCheckpointV1 {
                target_projection_key: projection_key,
                source_generation,
                source_manifest_digest,
                completed_batches: 1,
                last_request_digest: Some(request_digest),
                last_publication_digest: Some(publication_digest),
            },
            manifest_digest,
        }
    }

    fn manifest_digest_for_test_request(byte: char) -> ManifestDigest {
        manifest_digest(if byte.is_ascii_hexdigit() { byte } else { 'f' })
    }

    fn reused_prepared(
        embedding_key: &AdmittedEmbeddingProjectionKeyV1,
        from_generation: &CodeGenerationId,
        to_generation: &CodeGenerationId,
        chunk_id: &CodeSearchChunkId,
        chunk_digest: &ContentDigest,
    ) -> PreparedVectorGenerationV1 {
        let mut changes = ChangedCodeChunkSetV1 {
            from_generation: Some(from_generation.clone()),
            to_generation: to_generation.clone(),
            manifest_digest: manifest_digest('0'),
            added_or_changed: vec![],
            deleted: vec![],
            reused: vec![ChangedCodeChunkV1 {
                chunk_id: chunk_id.clone(),
                prior_digest: Some(chunk_digest.clone()),
                current_digest: Some(chunk_digest.clone()),
            }],
        };
        changes.manifest_digest = changes.compute_digest().expect("changed-set digest");
        let mut request = ProjectionBatchRequestV1 {
            request_digest: manifest_digest('0'),
            changes,
            previous_projection_key: Some(embedding_key.projection_key().clone()),
            target_projection_key: embedding_key.projection_key().clone(),
            replay_reason: ProjectionReplayReasonV1::SourceEdit,
        };
        request.request_digest =
            tracedecay_code_index::projection::expected_request_digest(&request)
                .expect("projection request digest");
        let receipt = tracedecay_code_index::projection::build_batch_receipt(
            &request,
            &[
                tracedecay_code_index::projection::ChunkProjectionDecisionV1 {
                    chunk_id: chunk_id.clone(),
                    prior_chunk_digest: Some(chunk_digest.clone()),
                    current_chunk_digest: Some(chunk_digest.clone()),
                    operation: ProjectionOperationV1::Reused,
                    outcome: ProjectionOutcomeV1::Reused,
                    output_digest: None,
                },
            ],
        )
        .expect("reused projection receipt");
        PreparedVectorGenerationV1 {
            embedding_key: embedding_key.clone(),
            request,
            receipt,
            vectors: vec![],
            tombstones: vec![],
        }
    }

    /// One projection batch that adds a single chunk vector.
    fn added_prepared(
        embedding_key: &AdmittedEmbeddingProjectionKeyV1,
        to_generation: &CodeGenerationId,
        chunk_id: &CodeSearchChunkId,
        chunk_digest: &ContentDigest,
        values: Vec<f32>,
    ) -> PreparedVectorGenerationV1 {
        let projection_key = embedding_key.projection_key().clone();
        let output_digest = tracedecay_semantic::projector::vector_output_digest(
            &projection_key,
            chunk_id,
            chunk_digest,
            &values,
        )
        .expect("canonical vector output digest");
        let mut changes = ChangedCodeChunkSetV1 {
            from_generation: None,
            to_generation: to_generation.clone(),
            manifest_digest: manifest_digest('0'),
            added_or_changed: vec![ChangedCodeChunkV1 {
                chunk_id: chunk_id.clone(),
                prior_digest: None,
                current_digest: Some(chunk_digest.clone()),
            }],
            deleted: vec![],
            reused: vec![],
        };
        changes.manifest_digest = changes.compute_digest().expect("changed-set digest");
        let source_manifest_digest = changes.manifest_digest.clone();
        let mut request = ProjectionBatchRequestV1 {
            request_digest: manifest_digest('0'),
            changes,
            previous_projection_key: None,
            target_projection_key: projection_key.clone(),
            replay_reason: ProjectionReplayReasonV1::SourceEdit,
        };
        request.request_digest =
            tracedecay_code_index::projection::expected_request_digest(&request)
                .expect("projection request digest");
        let receipt = tracedecay_code_index::projection::build_batch_receipt(
            &request,
            &[
                tracedecay_code_index::projection::ChunkProjectionDecisionV1 {
                    chunk_id: chunk_id.clone(),
                    prior_chunk_digest: None,
                    current_chunk_digest: Some(chunk_digest.clone()),
                    operation: ProjectionOperationV1::Added,
                    outcome: ProjectionOutcomeV1::Applied,
                    output_digest: Some(output_digest.clone()),
                },
            ],
        )
        .expect("added projection receipt");
        PreparedVectorGenerationV1 {
            embedding_key: embedding_key.clone(),
            request,
            receipt,
            vectors: vec![ProjectedChunkVectorV1 {
                projection_key,
                source_generation: to_generation.clone(),
                source_manifest_digest,
                chunk_id: chunk_id.clone(),
                chunk_digest: chunk_digest.clone(),
                values,
                output_digest,
            }],
            tombstones: vec![],
        }
    }

    async fn open_project_database(
        temporary: &tempfile::TempDir,
        operation: &'static str,
    ) -> (Database, DatabaseAuthority) {
        let path = temporary.path().join("project.db");
        crate::register_test_schema_installer();
        let authority = DatabaseAuthority::acquire_test(&path, operation).expect("authority");
        let (database, _) =
            Database::publish_test_runtime(&path, &authority, TestDatabaseRuntimeMode::Initialize)
                .await
                .expect("database");
        (database, authority)
    }

    async fn state_document(database: &Database) -> String {
        let mut rows = database
            .engine_conn()
            .query(
                "SELECT state_json FROM semantic_vector_generation_state WHERE singleton = 1",
                (),
            )
            .await
            .expect("state document");
        let row = rows.next().await.expect("state row").expect("state row");
        row.get::<String>(0).expect("state json")
    }

    async fn sqlite_table_exists(database: &Database, table: &str) -> bool {
        let mut rows = database
            .engine_conn()
            .query(
                "SELECT EXISTS(
                    SELECT 1
                    FROM sqlite_schema
                    WHERE type = 'table' AND name = ?1
                 )",
                params![table],
            )
            .await
            .expect("inspect SQLite schema");
        let row = rows.next().await.expect("schema row").expect("schema row");
        row.get::<i64>(0).expect("table existence") == 1
    }

    fn insert_generation(
        store: &mut VectorGenerationState,
        generation: PublishedVectorGenerationV1,
    ) -> VectorGenerationIdV1 {
        let generation_id = generation.generation_id().clone();
        intern_generation_vectors(
            &store.physical_vector_pool,
            &mut store.published,
            &generation,
        )
        .expect("intern generation vectors");
        store
            .published
            .generations
            .insert(generation_id.clone(), generation);
        generation_id
    }

    #[test]
    fn batch_watermark_and_base_generation_must_match_the_projection_request() {
        let embedding = admitted_embedding();
        let base = logical_generation(
            'a',
            embedding.clone(),
            "code-generation.base",
            'b',
            "chunk.v1.base",
            'c',
            vec![0.25],
        );
        let chunk_id = base.vectors.keys().next().expect("base chunk").clone();
        let chunk_digest = base
            .vectors
            .get(&chunk_id)
            .expect("base vector")
            .chunk_digest
            .clone();
        let base_id = base.generation_id().clone();
        let mut store = VectorGenerationState::new();
        insert_generation(&mut store, base);
        let foreign_source = id("code-generation.foreign");
        let target_source = id("code-generation.target");
        let prepared = reused_prepared(
            &embedding,
            &foreign_source,
            &target_source,
            &chunk_id,
            &chunk_digest,
        );
        let build = store
            .begin_generation(VectorGenerationPlanV1 {
                target_projection_key: embedding.projection_key().clone(),
                source_generation: target_source.clone(),
                source_manifest_digest: prepared.request.changes.manifest_digest.clone(),
                expected_chunk_ids: vec![chunk_id.clone()].into(),
                base_generation: Some(base_id.clone()),
            })
            .expect("pending build");
        assert_eq!(
            store.commit_batch(&build, None, prepared.clone()),
            Err(VectorGenerationStoreErrorV1::IncompatibleBaseGeneration)
        );

        let mismatched_manifest = manifest_digest('f');
        let mismatched_build = store
            .begin_generation(VectorGenerationPlanV1 {
                target_projection_key: embedding.projection_key().clone(),
                source_generation: target_source,
                source_manifest_digest: mismatched_manifest,
                expected_chunk_ids: vec![chunk_id].into(),
                base_generation: Some(base_id),
            })
            .expect("mismatched-watermark build");
        assert_eq!(
            store.commit_batch(&mismatched_build, None, prepared),
            Err(VectorGenerationStoreErrorV1::BatchIdentityMismatch)
        );
    }

    #[test]
    fn successful_publication_consumes_the_pending_build() {
        let embedding = admitted_embedding();
        let base = logical_generation(
            'a',
            embedding.clone(),
            "code-generation.base",
            'b',
            "chunk.v1.base",
            'c',
            vec![0.25],
        );
        let chunk_id = base.vectors.keys().next().expect("base chunk").clone();
        let chunk_digest = base
            .vectors
            .get(&chunk_id)
            .expect("base vector")
            .chunk_digest
            .clone();
        let base_source = base.source_generation().clone();
        let base_id = base.generation_id().clone();
        let target_source = id("code-generation.target");
        let prepared = reused_prepared(
            &embedding,
            &base_source,
            &target_source,
            &chunk_id,
            &chunk_digest,
        );
        let mut store = VectorGenerationState::new();
        insert_generation(&mut store, base);
        store.published.active_generation = Some(base_id.clone());
        let build = store
            .begin_generation(VectorGenerationPlanV1 {
                target_projection_key: embedding.projection_key().clone(),
                source_generation: target_source,
                source_manifest_digest: prepared.request.changes.manifest_digest.clone(),
                expected_chunk_ids: vec![chunk_id].into(),
                base_generation: Some(base_id.clone()),
            })
            .expect("pending build");
        store
            .commit_batch(&build, None, prepared)
            .expect("complete reused batch");
        let publication = store
            .publish_generation(&build, Some(&base_id))
            .expect("atomic publication");

        assert!(!store.pending.contains_key(&build));
        assert_eq!(
            store.active_generation_id(),
            Some(&publication.generation_id)
        );
        store
            .active_generation()
            .expect("current generation")
            .validate_persisted()
            .expect("current generation is complete");
    }

    #[tokio::test]
    async fn request_read_ignores_corrupt_inactive_and_pending_generations() {
        let temporary = tempfile::tempdir().expect("temporary project database");
        let path = temporary.path().join("project.db");
        crate::register_test_schema_installer();
        let authority = DatabaseAuthority::acquire_test(&path, "active vector request read")
            .expect("authority");
        let (database, _) =
            Database::publish_test_runtime(&path, &authority, TestDatabaseRuntimeMode::Initialize)
                .await
                .expect("database");
        let graph = test_graph();
        let _store = DatabaseVectorGenerationStoreV1::open(&database, Arc::clone(&graph))
            .await
            .expect("vector generation store");
        let embedding = admitted_embedding();
        let source: CodeGenerationId = id("code-generation.request-read");
        let source_manifest = manifest_digest('4');
        let active = logical_generation(
            'c',
            embedding.clone(),
            source.as_str(),
            '4',
            "chunk.v1.request-read",
            'd',
            vec![0.5],
        );
        let active_id = active.generation_id().clone();
        let mut state = VectorGenerationState::new();
        insert_generation(&mut state, active);
        state.published.active_generation = Some(active_id.clone());
        install_test_state_slices(&database, VECTOR_STATE_SLICE_TABLE_V1, &mut state).await;
        let mut state_json = serde_json::to_value(&state).expect("vector state JSON");
        state_json["published"]["generations"][manifest_digest('e').as_str()] =
            serde_json::json!("corrupt-inactive-vector-bytes");
        state_json["pending"] = serde_json::json!({
            "corrupt-build": "corrupt-pending-vector-bytes"
        });
        database
            .execute_write_engine(
                "install inactive corruption fixture",
                "UPDATE semantic_vector_generation_state
                 SET revision = revision + 1, state_json = ?1
                 WHERE singleton = 1",
                params![state_json.to_string()],
            )
            .await
            .expect("corrupt inactive fixture");

        let observed = DatabaseVectorGenerationStoreV1::read_active_generation_for(
            &database,
            &graph,
            &embedding,
            &source,
            &source_manifest,
        )
        .await
        .expect("bounded active read")
        .expect("compatible active generation");
        assert_eq!(observed.generation_id(), &active_id);
        assert!(
            DatabaseVectorGenerationStoreV1::read_active_generation_snapshot_for(
                &database,
                &graph,
                &embedding,
                &source,
                &manifest_digest('5'),
            )
            .await
            .expect("wrong-manifest active read")
            .is_none(),
            "an active generation with the wrong source manifest must be denied"
        );
        let store = DatabaseVectorGenerationStoreV1::open(&database, graph)
            .await
            .expect("open installs schema without decoding the existing document");
        assert!(
            store.active_generation().await.is_err(),
            "full-state decoding would observe unrelated corruption"
        );
    }

    #[tokio::test]
    async fn native_evaluation_state_is_sqlite_backed_and_never_becomes_authoritative() {
        let temporary = tempfile::tempdir().expect("temporary project database");
        let path = temporary.path().join("project.db");
        crate::register_test_schema_installer();
        let authority = DatabaseAuthority::acquire_test(&path, "native semantic evaluation")
            .expect("authority");
        let (database, _) =
            Database::publish_test_runtime(&path, &authority, TestDatabaseRuntimeMode::Initialize)
                .await
                .expect("database");

        let evaluation = DatabaseVectorEvaluationStoreV1::open(
            &database,
            test_graph(),
            "semantic-native-evaluation:test",
        )
        .await
        .expect("isolated evaluation store");
        assert_eq!(
            evaluation
                .active_generation_id()
                .await
                .expect("evaluation active generation"),
            None
        );
        assert_eq!(
            database
                .query_scalar_i64(
                    "inspect native evaluation row",
                    "SELECT COUNT(*) FROM semantic_vector_evaluation_state",
                )
                .await
                .expect("evaluation row count"),
            1
        );
        assert_eq!(
            database
                .query_scalar_i64(
                    "prove native evaluation did not create authoritative state",
                    "SELECT COUNT(*)
                     FROM sqlite_schema
                     WHERE type = 'table'
                       AND name = 'semantic_vector_generation_state'",
                )
                .await
                .expect("authoritative schema count"),
            0
        );

        evaluation.close().await.expect("remove evaluation row");
        assert_eq!(
            database
                .query_scalar_i64(
                    "verify native evaluation cleanup",
                    "SELECT COUNT(*) FROM semantic_vector_evaluation_state",
                )
                .await
                .expect("evaluation row count after cleanup"),
            0
        );
    }

    #[test]
    fn active_pointer_cas_fault_restart_and_semantic_off_are_atomic() {
        let embedding = admitted_embedding();
        let first = logical_generation(
            'a',
            embedding.clone(),
            "code-generation.atomic-a",
            '1',
            "chunk.v1.atomic-a",
            'a',
            vec![0.25],
        );
        let second = logical_generation(
            'b',
            embedding,
            "code-generation.atomic-b",
            '2',
            "chunk.v1.atomic-b",
            'b',
            vec![0.75],
        );
        let mut store = VectorGenerationState::new();
        let first_id = insert_generation(&mut store, first);
        let second_id = insert_generation(&mut store, second);
        store.published.active_generation = Some(first_id.clone());

        assert_eq!(
            store.activate_generation(&second_id, None),
            Err(VectorGenerationStoreErrorV1::StaleActiveGeneration)
        );
        assert_eq!(store.active_generation_id(), Some(&first_id));

        store.fail_before_publication_swap_once();
        assert_eq!(
            store.activate_generation(&second_id, Some(&first_id)),
            Err(VectorGenerationStoreErrorV1::InjectedPublicationFailure)
        );
        assert_eq!(store.active_generation_id(), Some(&first_id));

        store
            .activate_generation(&second_id, Some(&first_id))
            .expect("activate replacement generation");
        assert_eq!(
            store.deactivate_generation(Some(&first_id)),
            Err(VectorGenerationStoreErrorV1::StaleActiveGeneration)
        );
        assert_eq!(store.active_generation_id(), Some(&second_id));
        // The state document carries neither float payloads nor corpus-sized
        // collections; a restart resolves both from their own tables, which
        // this round trip stands in for.
        let mut restarted = restart_round_trip(&mut store);
        restarted
            .ensure_physical_reuse_index()
            .expect("rebuild physical reuse index");
        validate_loaded_state(&restarted).expect("validate restarted vector state");
        assert_eq!(restarted.active_generation_id(), Some(&second_id));

        restarted.fail_before_publication_swap_once();
        assert_eq!(
            restarted.deactivate_generation(Some(&second_id)),
            Err(VectorGenerationStoreErrorV1::InjectedPublicationFailure)
        );
        assert_eq!(restarted.active_generation_id(), Some(&second_id));

        restarted
            .deactivate_generation(Some(&second_id))
            .expect("disable semantic generation");
        assert_eq!(restarted.active_generation_id(), None);
        assert!(
            restarted.generation(&second_id).is_some(),
            "semantic-off retains the immutable generation for rollback"
        );
        restarted
            .activate_generation(&second_id, None)
            .expect("restore exact retained generation");
        assert_eq!(restarted.active_generation_id(), Some(&second_id));
    }

    #[test]
    fn cross_worktree_reuses_physical_bytes_without_reusing_logical_identity() {
        let embedding = admitted_embedding_for("privacy.reuse-regression-a", 7, "ort-test-rev-1");
        let first = logical_generation(
            'a',
            embedding.clone(),
            "code-generation.worktree-a",
            '1',
            "chunk.v1.worktree-a.alpha",
            'c',
            vec![0.25],
        );
        let second = logical_generation(
            'b',
            embedding.clone(),
            "code-generation.worktree-b",
            '2',
            "chunk.v1.worktree-b.alpha",
            'c',
            vec![0.25],
        );
        let first_chunk = first.vectors.keys().next().unwrap().clone();
        let second_chunk = second.vectors.keys().next().unwrap().clone();
        let first_generation = first.generation_id().clone();
        let second_generation = second.generation_id().clone();
        let mut first_store = VectorGenerationState::new();
        let mut second_store = VectorGenerationState::new();

        intern_generation_vectors(
            &first_store.physical_vector_pool,
            &mut first_store.published,
            &first,
        )
        .unwrap();
        first_store
            .published
            .generations
            .insert(first_generation.clone(), first.clone());
        first_store.published.active_generation = Some(first_generation.clone());
        intern_generation_vectors(
            &second_store.physical_vector_pool,
            &mut second_store.published,
            &second,
        )
        .unwrap();
        second_store
            .published
            .generations
            .insert(second_generation.clone(), second.clone());
        second_store.published.active_generation = Some(second_generation.clone());

        let first_values = first_store
            .physical_vector_values(&first_generation, &first_chunk)
            .unwrap();
        let second_values = second_store
            .physical_vector_values(&second_generation, &second_chunk)
            .unwrap();
        assert!(Arc::ptr_eq(&first_values, &second_values));
        assert_eq!(first_store.published.physical_vectors.len(), 1);
        assert_eq!(second_store.published.physical_vectors.len(), 1);
        assert_ne!(first_generation, second_generation);
        assert_ne!(first.source_generation(), second.source_generation());
        assert_ne!(first_chunk, second_chunk);
        assert_ne!(first.receipts(), second.receipts());
        assert_eq!(first_store.active_generation_id(), Some(&first_generation));
        assert_eq!(
            second_store.active_generation_id(),
            Some(&second_generation),
            "each worktree retains its own active pointer"
        );

        for (generation_digest, embedding_key) in [
            (
                'd',
                admitted_embedding_for("privacy.reuse-regression-b", 7, "ort-test-rev-1"),
            ),
            (
                'e',
                admitted_embedding_for("privacy.reuse-regression-a", 8, "ort-test-rev-1"),
            ),
            (
                'f',
                admitted_embedding_for("privacy.reuse-regression-a", 7, "ort-test-rev-2"),
            ),
        ] {
            let isolated = logical_generation(
                generation_digest,
                embedding_key,
                &format!("code-generation.isolated-{generation_digest}"),
                generation_digest,
                &format!("chunk.v1.isolated-{generation_digest}.alpha"),
                'c',
                vec![0.25],
            );
            intern_generation_vectors(
                &second_store.physical_vector_pool,
                &mut second_store.published,
                &isolated,
            )
            .unwrap();
            second_store
                .published
                .generations
                .insert(isolated.generation_id().clone(), isolated);
        }
        assert_eq!(
            second_store.published.physical_vectors.len(),
            4,
            "privacy domain, key epoch, and any projection-key input isolate physical bytes"
        );

        let edited_second = logical_generation(
            '9',
            embedding.clone(),
            "code-generation.worktree-b-edited",
            '9',
            "chunk.v1.worktree-b.alpha-edited",
            '9',
            vec![0.75],
        );
        let edited_generation = edited_second.generation_id().clone();
        let edited_chunk = edited_second.vectors.keys().next().unwrap().clone();
        intern_generation_vectors(
            &second_store.physical_vector_pool,
            &mut second_store.published,
            &edited_second,
        )
        .unwrap();
        second_store
            .published
            .generations
            .insert(edited_generation.clone(), edited_second);
        assert_eq!(second_store.published.physical_vectors.len(), 5);
        assert!(!Arc::ptr_eq(
            &second_values,
            &second_store
                .physical_vector_values(&edited_generation, &edited_chunk)
                .unwrap()
        ));
        assert!(Arc::ptr_eq(
            &first_values,
            &second_store
                .physical_vector_values(&second_generation, &second_chunk)
                .unwrap()
        ));
        assert!(Arc::ptr_eq(
            &first_values,
            &first_store
                .physical_vector_values(&first_generation, &first_chunk)
                .unwrap()
        ));
        assert_eq!(first_store.active_generation_id(), Some(&first_generation));
        assert_eq!(
            second_store.active_generation_id(),
            Some(&second_generation)
        );

        let conflicting = logical_generation(
            '8',
            embedding,
            "code-generation.worktree-c",
            '8',
            "chunk.v1.worktree-c.alpha",
            'c',
            vec![0.5],
        );
        assert_eq!(
            intern_generation_vectors(
                &second_store.physical_vector_pool,
                &mut second_store.published,
                &conflicting,
            ),
            Err(VectorGenerationStoreErrorV1::PhysicalVectorConflict)
        );
    }

    #[test]
    fn generation_identity_ignores_batch_execution_history() {
        let embedding_key = admitted_embedding();
        let projection_key = embedding_key.projection_key().clone();
        let source_generation = id::<CodeGenerationId>("code-generation.1");
        let source_manifest_digest = manifest_digest('b');
        let chunk_id = id::<CodeSearchChunkId>("chunk.v1.alpha");
        let plan = VectorGenerationPlanV1 {
            target_projection_key: projection_key.clone(),
            source_generation: source_generation.clone(),
            source_manifest_digest: source_manifest_digest.clone(),
            expected_chunk_ids: vec![chunk_id.clone()].into(),
            base_generation: None,
        };
        let vectors = BTreeMap::from([(
            chunk_id.clone(),
            ProjectedChunkVectorV1 {
                projection_key: projection_key.clone(),
                source_generation: source_generation.clone(),
                source_manifest_digest: source_manifest_digest.clone(),
                chunk_id,
                chunk_digest: content_digest('c'),
                values: vec![0.25],
                // Identity tests compare digest bytes, not recomputed projector validity.
                output_digest: content_digest('d'),
            },
        )]);
        let tombstones = BTreeMap::new();

        let first = generation_identity_digest(&plan, &vectors, &tombstones)
            .expect("identity from vector content");
        let second = generation_identity_digest(&plan, &vectors, &tombstones)
            .expect("identity remains independent from receipt/checkpoint batching");

        assert_eq!(first, second);

        let checkpoint = VectorProjectionCheckpointV1 {
            target_projection_key: plan.target_projection_key.clone(),
            source_generation: plan.source_generation.clone(),
            source_manifest_digest: plan.source_manifest_digest.clone(),
            completed_batches: 1,
            last_request_digest: Some(manifest_digest('e')),
            last_publication_digest: Some(manifest_digest('f')),
        };
        let published = PublishedVectorGenerationV1 {
            generation_id: VectorGenerationIdV1::new(first.clone()),
            graph_projection: graph_projection_id("semantic-vector-generation", &first)
                .expect("graph projection"),
            projection_key: plan.target_projection_key.clone(),
            source_generation: plan.source_generation.clone(),
            source_manifest_digest: plan.source_manifest_digest.clone(),
            base_generation: None,
            embedding_key,
            vectors: vectors.clone().into(),
            tombstones: vec![].into(),
            tombstone_digests: BTreeMap::new().into(),
            receipts: vec![].into(),
            checkpoint,
            manifest_digest: first,
        };
        let mut replayed = published.clone();
        replayed.checkpoint.completed_batches = 2;
        replayed.checkpoint.last_request_digest = Some(manifest_digest('0'));
        replayed.checkpoint.last_publication_digest = Some(manifest_digest('1'));

        assert_ne!(published.checkpoint, replayed.checkpoint);
        assert!(
            published.same_vector_content(&replayed),
            "execution checkpoint history does not redefine immutable vector content"
        );
        let mut rebuilt_from_another_base = published.clone();
        rebuilt_from_another_base.base_generation =
            Some(VectorGenerationIdV1::new(manifest_digest('9')));
        assert!(
            published.same_vector_content(&rebuilt_from_another_base),
            "execution lineage does not redefine identical immutable vector content"
        );

        let mut sealed_source = VectorGenerationState::new();
        sealed_source
            .published
            .generations
            .insert(published.generation_id().clone(), published.clone());
        let sealed = seal_test_state(&mut sealed_source);
        let published = sealed_source
            .published
            .generations
            .values()
            .next()
            .expect("sealed generation")
            .clone();
        let encoded = serde_json::to_string(&published).expect("serialize published generation");
        assert!(
            !encoded.contains("\"values\""),
            "the state document must not carry inline float payloads"
        );
        assert!(
            !encoded.contains("\"chunk_digest\""),
            "the state document must not carry per-vector row metadata"
        );
        let mut decoded: PublishedVectorGenerationV1 =
            serde_json::from_str(&encoded).expect("deserialize published generation");
        assert!(
            decoded.vectors().is_empty(),
            "decoded rows resolve from externalized collection storage"
        );
        decoded
            .visit_external_slots(&mut |slot| {
                let Some(address) = slot.address().cloned() else {
                    return Ok(());
                };
                slot.fill(sealed.get(&address).expect("sealed collection"))
            })
            .expect("fill externalized collections");
        for (chunk_id, vector) in decoded.vectors.elided_mut().iter_mut() {
            vector
                .values
                .clone_from(&published.vectors[chunk_id].values);
        }
        assert!(published.same_vector_content(&decoded));
        assert_eq!(decoded.tombstones(), published.tombstones());
        assert_eq!(decoded.tombstone_digests(), published.tombstone_digests());
        assert_eq!(decoded.base_generation(), published.base_generation());
        assert_eq!(decoded.embedding_key(), published.embedding_key());
    }

    #[test]
    fn persisted_state_rejects_tombstone_vector_overlap_and_dangling_active() {
        let embedding_key = admitted_embedding();
        let projection_key = embedding_key.projection_key().clone();
        let chunk_id = id::<CodeSearchChunkId>("chunk.v1.alpha");
        let generation_id = VectorGenerationIdV1::new(manifest_digest('a'));
        let mut generation = PublishedVectorGenerationV1 {
            generation_id: generation_id.clone(),
            graph_projection: graph_projection_id(
                "semantic-vector-generation",
                generation_id.as_digest(),
            )
            .expect("graph projection"),
            projection_key: projection_key.clone(),
            source_generation: id("code-generation.1"),
            source_manifest_digest: manifest_digest('b'),
            base_generation: None,
            embedding_key: embedding_key.clone(),
            vectors: BTreeMap::from([(
                chunk_id.clone(),
                ProjectedChunkVectorV1 {
                    projection_key,
                    source_generation: id("code-generation.1"),
                    source_manifest_digest: manifest_digest('b'),
                    chunk_id: chunk_id.clone(),
                    chunk_digest: content_digest('c'),
                    values: vec![1.0],
                    output_digest: content_digest('d'),
                },
            )])
            .into(),
            tombstones: vec![chunk_id.clone()].into(),
            tombstone_digests: BTreeMap::from([(chunk_id, content_digest('c'))]).into(),
            receipts: vec![].into(),
            checkpoint: VectorProjectionCheckpointV1 {
                target_projection_key: embedding_key.projection_key().clone(),
                source_generation: id("code-generation.1"),
                source_manifest_digest: manifest_digest('b'),
                completed_batches: 1,
                last_request_digest: None,
                last_publication_digest: None,
            },
            manifest_digest: generation_id.as_digest().clone(),
        };
        assert!(generation.validate_persisted().is_err());

        generation.vectors.clear();
        generation.canonicalize_tombstones();
        let request_digest = manifest_digest('e');
        let mut deletion_batch = ProjectionBatchReceiptV1 {
            target_projection_key: generation.projection_key.clone(),
            request_digest: request_digest.clone(),
            source_generation: generation.source_generation.clone(),
            source_manifest_digest: generation.source_manifest_digest.clone(),
            receipts: vec![tracedecay_domain::CodeChunkProjectionReceiptV1 {
                projection_key: generation.projection_key.clone(),
                request_digest: request_digest.clone(),
                prior_generation: Some(id("code-generation.0")),
                source_generation: generation.source_generation.clone(),
                source_manifest_digest: generation.source_manifest_digest.clone(),
                chunk_id: generation.tombstones[0].clone(),
                prior_chunk_digest: generation
                    .tombstone_digests
                    .get(&generation.tombstones[0])
                    .cloned(),
                current_chunk_digest: None,
                operation: ProjectionOperationV1::Deleted,
                outcome: ProjectionOutcomeV1::Applied,
                output_digest: None,
            }],
            reused_count: 0,
            publication_digest: manifest_digest('f'),
        };
        deletion_batch.publication_digest =
            expected_publication_digest(&deletion_batch).expect("deletion publication digest");
        generation.checkpoint.last_request_digest = Some(request_digest);
        generation.checkpoint.last_publication_digest =
            Some(deletion_batch.publication_digest.clone());
        *generation.receipts = vec![deletion_batch];
        generation.manifest_digest = generation_identity_digest(
            &VectorGenerationPlanV1 {
                target_projection_key: generation.projection_key.clone(),
                source_generation: generation.source_generation.clone(),
                source_manifest_digest: generation.source_manifest_digest.clone(),
                expected_chunk_ids: vec![].into(),
                base_generation: None,
            },
            &generation.vectors,
            &generation.tombstone_digests,
        )
        .expect("tombstone generation manifest");
        generation.generation_id = VectorGenerationIdV1::new(generation.manifest_digest.clone());
        assert!(generation.validate_persisted().is_ok());

        let mut state = VectorGenerationState::default();
        state.published.active_generation = Some(VectorGenerationIdV1::new(manifest_digest('9')));
        assert!(validate_loaded_state(&state).is_err());
    }

    #[test]
    fn persisted_generation_recomputes_immutable_manifest_content() {
        let mut generation = logical_generation(
            'a',
            admitted_embedding(),
            "code-generation.manifest-integrity",
            'b',
            "chunk.v1.manifest-integrity",
            'c',
            vec![0.25],
        );
        generation
            .validate_persisted()
            .expect("canonical generation");
        let vector = generation
            .vectors
            .values_mut()
            .next()
            .expect("fixture vector");
        vector.values = vec![0.75];
        vector.output_digest = tracedecay_semantic::projector::vector_output_digest(
            &vector.projection_key,
            &vector.chunk_id,
            &vector.chunk_digest,
            &vector.values,
        )
        .expect("tampered vector digest");
        generation.receipts[0].receipts[0].output_digest = Some(vector.output_digest.clone());
        generation.receipts[0].publication_digest =
            expected_publication_digest(&generation.receipts[0])
                .expect("tampered publication digest");
        generation.checkpoint.last_publication_digest =
            Some(generation.receipts[0].publication_digest.clone());

        assert!(
            generation.validate_persisted().is_err(),
            "self-consistent vector/receipt tampering must not retain the immutable generation id"
        );
    }

    /// The externalized store must produce exactly the identity the in-memory
    /// state machine produces for the same inputs, must keep the float payload
    /// out of the state document, and must let a restart resume a pending build.
    #[tokio::test]
    async fn grafeo_storage_preserves_identity_and_resumes_pending_builds() {
        let temporary = tempfile::tempdir().expect("temporary project database");
        let (database, _authority) =
            open_project_database(&temporary, "Grafeo vector storage").await;
        let embedding = admitted_embedding();
        let source: CodeGenerationId = id("code-generation.grafeo-vector");
        let chunk_id: CodeSearchChunkId = id("chunk.v1.grafeo-vector");
        let chunk_digest = content_digest('a');
        let prepared = added_prepared(
            &embedding,
            &source,
            &chunk_id,
            &chunk_digest,
            vec![0.312_5_f32],
        );
        let plan = VectorGenerationPlanV1 {
            target_projection_key: embedding.projection_key().clone(),
            source_generation: source.clone(),
            source_manifest_digest: prepared.request.changes.manifest_digest.clone(),
            expected_chunk_ids: vec![chunk_id.clone()].into(),
            base_generation: None,
        };

        // The oracle: the same plan and batch through the pure state machine.
        let mut oracle = VectorGenerationState::new();
        let oracle_build = oracle
            .begin_generation(plan.clone())
            .expect("oracle build identity");
        oracle
            .commit_batch(&oracle_build, None, prepared.clone())
            .expect("oracle batch");
        let oracle_publication = oracle
            .publish_generation(&oracle_build, None)
            .expect("oracle publication");

        let graph = test_graph();
        let store = DatabaseVectorGenerationStoreV1::open(&database, Arc::clone(&graph))
            .await
            .expect("open vector generation store");
        assert!(
            !sqlite_table_exists(&database, "semantic_vector_payload_v1").await,
            "SQLite must not retain semantic vector payload authority"
        );
        let build = store
            .begin_generation(plan)
            .await
            .expect("durable build identity");
        assert_eq!(build, oracle_build);
        let checkpoint = store
            .commit_batch(&build, None, prepared.clone())
            .await
            .expect("durable batch");
        assert_eq!(checkpoint.completed_batches, 1);

        let document = state_document(&database).await;
        assert!(
            !document.contains("\"values\""),
            "the state document must not carry inline float payloads"
        );
        // Restart: a fresh handle over the same database resumes the pending
        // build and publishes the byte-identical generation identity.
        let restarted = DatabaseVectorGenerationStoreV1::open(&database, Arc::clone(&graph))
            .await
            .expect("reopen vector generation store");
        let publication = restarted
            .publish_generation(&build, None)
            .await
            .expect("publish resumed build");
        assert_eq!(publication.generation_id, oracle_publication.generation_id);
        assert_eq!(
            publication.manifest_digest,
            oracle_publication.manifest_digest
        );
        assert_eq!(publication.checkpoint, oracle_publication.checkpoint);
        let mut intent_rows = database
            .engine_conn()
            .query(
                "SELECT COUNT(*)
                 FROM semantic_vector_publication_intent
                 WHERE generation_id = ?1",
                params![publication.generation_id.as_digest().as_str()],
            )
            .await
            .expect("read publication intent");
        let intent_count = intent_rows
            .next()
            .await
            .expect("read publication intent row")
            .expect("publication intent count")
            .get::<i64>(0)
            .expect("publication intent count value");
        drop(intent_rows);
        assert_eq!(
            intent_count, 1,
            "activation must retain its immutable relational publication intent"
        );
        assert_eq!(
            DatabaseVectorGenerationStoreV1::readable_source_generations(&database, graph.as_ref())
                .await
                .expect("read Grafeo projection telemetry"),
            BTreeSet::from([source]),
        );

        let observed = restarted
            .active_generation()
            .await
            .expect("read active generation")
            .expect("active generation");
        let mut expected = oracle
            .generation(&oracle_publication.generation_id)
            .expect("oracle generation")
            .clone();
        for vector in expected.vectors.elided_mut().values_mut() {
            vector.values.clear();
        }
        assert_eq!(
            observed, expected,
            "round trip restores the exact relational metadata"
        );
        assert_eq!(
            observed.vectors()[&chunk_id].values,
            Vec::<f32>::new(),
            "relational generation reads never hydrate Grafeo float payloads"
        );
        let graph_entity = graph
            .projection_entities(
                &GraphNamespace::new(SEMANTIC_VECTOR_NAMESPACE).expect("namespace"),
                observed.graph_projection(),
                &[
                    graph_vector_entity_id(observed.graph_projection(), &chunk_id)
                        .expect("vector entity"),
                ],
                Arc::new(NeverCancelled),
            )
            .expect("hydrate selected vector")
            .into_iter()
            .next()
            .flatten()
            .expect("selected vector entity");
        assert_eq!(
            graph_entity
                .properties
                .get(&GraphPropertyName::new("embedding").expect("embedding property")),
            Some(&tracedecay_graph_db::GraphProperty::Vector(
                tracedecay_graph_db::GraphVector::new(
                    vec![0.312_5_f32],
                    1,
                    tracedecay_graph_db::VectorMetric::Cosine,
                )
                .expect("expected vector"),
            )),
            "only the selected Grafeo entity hydrates its float payload"
        );
        assert_eq!(
            observed.receipts(),
            expected.receipts(),
            "receipts are unchanged by externalized payload storage"
        );

        let bounded = DatabaseVectorGenerationStoreV1::read_active_generation_for(
            &database,
            &graph,
            &embedding,
            &source,
            observed.source_manifest_digest(),
        )
        .await
        .expect("bounded active read")
        .expect("compatible active generation");
        assert_eq!(bounded, expected);
    }

    /// Retiring a generation must release the interner keys it introduced, or
    /// the process-global pool grows for the lifetime of the daemon.
    #[test]
    fn physical_byte_pool_releases_keys_for_retired_generations() {
        let pool = PhysicalVectorBytePoolV1::default();
        pool.sweep_retired().expect("sweep");
        let baseline = pool.retained_entries();
        {
            let mut retained = Vec::new();
            for index in 0..64_u64 {
                let embedding = admitted_embedding_for("privacy.pool-scope", index, "ort-pool");
                let reuse_key = PhysicalVectorReuseKeyV1 {
                    canonical_chunk_digest: content_digest('a'),
                    projection_key: embedding.projection_key().clone(),
                    admitted_embedding_key: embedding.clone(),
                    privacy_domain: embedding.privacy_domain().clone(),
                    privacy_key_epoch: embedding.privacy_key_epoch(),
                };
                retained.push(pool.intern(&reuse_key, &[0.5_f32]).expect("intern"));
            }
            assert_eq!(
                pool.retained_entries(),
                baseline + 64,
                "live generations retain their interned identities"
            );
            pool.sweep_retired().expect("sweep with live handles");
            assert_eq!(
                pool.retained_entries(),
                baseline + 64,
                "a sweep never drops a live entry"
            );
        }
        pool.sweep_retired().expect("sweep after retire");
        assert_eq!(
            pool.retained_entries(),
            baseline,
            "retiring the generations releases every key they interned"
        );
    }

    /// Peak resident set size of this process, in bytes.
    fn peak_resident_bytes() -> u64 {
        std::fs::read_to_string("/proc/self/status")
            .ok()
            .and_then(|status| {
                status
                    .lines()
                    .find_map(|line| line.strip_prefix("VmHWM:"))
                    .and_then(|value| value.split_whitespace().next())
                    .and_then(|kilobytes| kilobytes.parse::<u64>().ok())
            })
            .map(|kilobytes| kilobytes * 1024)
            .unwrap_or_default()
    }

    /// Build one prepared batch covering `range` of the probe corpus.
    fn probe_prepared_batch(
        embedding: &AdmittedEmbeddingProjectionKeyV1,
        projection_key: &ProjectionKeyV1,
        source: &CodeGenerationId,
        dimensions: u32,
        range: std::ops::Range<usize>,
    ) -> PreparedVectorGenerationV1 {
        let mut vectors = Vec::with_capacity(range.len());
        let mut decisions = Vec::with_capacity(range.len());
        let mut changed = Vec::with_capacity(range.len());
        for index in range {
            let chunk_id: CodeSearchChunkId = id(&format!("chunk.v1.probe-{index:06}"));
            let chunk_digest: ContentDigest = id(&format!("sha256:{index:064x}"));
            let values = (0..dimensions)
                .map(|dimension| (index as f32 + dimension as f32) * 1.0e-4)
                .collect::<Vec<_>>();
            let output_digest = tracedecay_semantic::projector::vector_output_digest(
                projection_key,
                &chunk_id,
                &chunk_digest,
                &values,
            )
            .expect("output digest");
            changed.push(ChangedCodeChunkV1 {
                chunk_id: chunk_id.clone(),
                prior_digest: None,
                current_digest: Some(chunk_digest.clone()),
            });
            decisions.push(
                tracedecay_code_index::projection::ChunkProjectionDecisionV1 {
                    chunk_id: chunk_id.clone(),
                    prior_chunk_digest: None,
                    current_chunk_digest: Some(chunk_digest.clone()),
                    operation: ProjectionOperationV1::Added,
                    outcome: ProjectionOutcomeV1::Applied,
                    output_digest: Some(output_digest.clone()),
                },
            );
            vectors.push(ProjectedChunkVectorV1 {
                projection_key: projection_key.clone(),
                source_generation: source.clone(),
                source_manifest_digest: manifest_digest('0'),
                chunk_id,
                chunk_digest,
                values,
                output_digest,
            });
        }
        let mut changes = ChangedCodeChunkSetV1 {
            from_generation: None,
            to_generation: source.clone(),
            manifest_digest: manifest_digest('0'),
            added_or_changed: changed,
            deleted: vec![],
            reused: vec![],
        };
        changes.manifest_digest = changes.compute_digest().expect("changed-set digest");
        for vector in &mut vectors {
            vector.source_manifest_digest = changes.manifest_digest.clone();
        }
        let mut request = ProjectionBatchRequestV1 {
            request_digest: manifest_digest('0'),
            changes,
            previous_projection_key: None,
            target_projection_key: projection_key.clone(),
            replay_reason: ProjectionReplayReasonV1::SourceEdit,
        };
        request.request_digest =
            tracedecay_code_index::projection::expected_request_digest(&request)
                .expect("request digest");
        let receipt = tracedecay_code_index::projection::build_batch_receipt(&request, &decisions)
            .expect("batch receipt");
        PreparedVectorGenerationV1 {
            embedding_key: embedding.clone(),
            request,
            receipt,
            vectors,
            tombstones: vec![],
        }
    }

    #[test]
    fn graph_publication_batches_scale_with_the_delta_not_the_existing_corpus() {
        let embedding = admitted_embedding();
        let projection_key = embedding.projection_key().clone();
        let source: CodeGenerationId = id("code-generation.delta-bounds");
        let build = VectorGenerationBuildIdV1(manifest_digest('a'));
        let namespace = GraphNamespace::new(SEMANTIC_VECTOR_NAMESPACE).expect("namespace");

        let existing_corpus =
            probe_prepared_batch(&embedding, &projection_key, &source, 1, 0..2_049);
        let (_, initial_publications) =
            prepared_delta_publications(&namespace, &build, None, &existing_corpus)
                .expect("bounded initial publications");
        assert_eq!(initial_publications.len(), 9);
        assert!(
            initial_publications
                .iter()
                .all(|publication| publication.batch.mutations.len() <= 256)
        );

        let one_vector_delta = probe_prepared_batch(&embedding, &projection_key, &source, 1, 0..1);
        let (_, delta_publications) =
            prepared_delta_publications(&namespace, &build, None, &one_vector_delta)
                .expect("bounded delta publication");
        assert_eq!(delta_publications.len(), 1);
        assert_eq!(delta_publications[0].batch.mutations.len(), 1);
    }

    /// Scale probe for a whole-corpus vector generation committed in batches.
    ///
    /// Reports peak RSS and, per commit, the size of the state document the
    /// mutation binds. The document size is the number that used to grow with
    /// the corpus until it hit `MAX_REQUEST_BYTES`; with the metadata
    /// externalized it should stay flat no matter how many batches land.
    ///
    /// Ignored by default: it is a measurement, not an assertion about the
    /// host. Run it with `--ignored --nocapture`, optionally with
    /// `VECTOR_RSS_PROBE_CHUNKS` and `VECTOR_RSS_PROBE_BATCH`.
    #[tokio::test]
    #[ignore = "memory probe; run explicitly"]
    async fn probe_peak_resident_bytes_for_a_whole_corpus_generation() {
        let chunks: usize = std::env::var("VECTOR_RSS_PROBE_CHUNKS")
            .ok()
            .and_then(|value| value.parse().ok())
            .unwrap_or(5_000);
        let batch: usize = std::env::var("VECTOR_RSS_PROBE_BATCH")
            .ok()
            .and_then(|value| value.parse().ok())
            .unwrap_or(chunks)
            .max(1);
        #[expect(non_snake_case, reason = "probe keeps the constant-style names")]
        let CHUNKS = chunks;
        const DIMENSIONS: u32 = 768;
        let temporary = tempfile::tempdir().expect("temporary project database");
        let (database, _authority) = open_project_database(&temporary, "vector rss probe").await;
        let mut key = admitted_embedding().embedding_key().clone();
        key.dimensions = DIMENSIONS;
        let embedding = key.admit().expect("admitted probe embedding");
        let projection_key = embedding.projection_key().clone();
        let source: CodeGenerationId = id("code-generation.rss-probe");

        let mut chunk_ids = (0..CHUNKS)
            .map(|index| id::<CodeSearchChunkId>(&format!("chunk.v1.probe-{index:06}")))
            .collect::<Vec<_>>();
        chunk_ids.sort();
        // The plan's watermark is the corpus's, not any one batch's, so
        // splitting the run never moves the generation identity.
        let whole =
            probe_prepared_batch(&embedding, &projection_key, &source, DIMENSIONS, 0..CHUNKS);
        let source_manifest_digest = whole.request.changes.manifest_digest.clone();
        drop(whole);
        let plan = VectorGenerationPlanV1 {
            target_projection_key: projection_key.clone(),
            source_generation: source.clone(),
            source_manifest_digest,
            expected_chunk_ids: chunk_ids.into(),
            base_generation: None,
        };

        let baseline = peak_resident_bytes();
        let store = DatabaseVectorGenerationStoreV1::open(&database, test_graph())
            .await
            .expect("open store");
        let build = store.begin_generation(plan).await.expect("build identity");
        let mut checkpoint = None;
        let mut widest_document = 0_usize;
        let mut commits = 0_usize;
        let mut start = 0;
        while start < CHUNKS {
            let end = (start + batch).min(CHUNKS);
            let prepared =
                probe_prepared_batch(&embedding, &projection_key, &source, DIMENSIONS, start..end);
            checkpoint = Some(
                store
                    .commit_batch(&build, checkpoint.as_ref(), prepared)
                    .await
                    .expect("commit batch"),
            );
            widest_document = widest_document.max(state_document(&database).await.len());
            commits += 1;
            start = end;
        }
        let publication = store
            .publish_generation(&build, None)
            .await
            .expect("publish corpus");
        widest_document = widest_document.max(state_document(&database).await.len());
        let peak = peak_resident_bytes();
        println!(
            "vector-generation scale probe: chunks={CHUNKS} batch={batch} commits={commits} \
             dimensions={DIMENSIONS} float_payload_bytes={} widest_state_document_bytes={} \
             peak_rss_bytes={peak} peak_rss_gib={:.2} baseline_rss_bytes={baseline} \
             generation={}",
            CHUNKS * DIMENSIONS as usize * size_of::<f32>(),
            widest_document,
            peak as f64 / (1024.0 * 1024.0 * 1024.0),
            publication.generation_id.as_digest(),
        );
    }
}
