//! Immutable semantic vector-generation storage.
//!
//! The deterministic state machine is retained as a test oracle. Production
//! persistence stores that same state in the already-open project database,
//! using a revisioned compare-and-swap so generation publication and the
//! active pointer become visible together. No separate vector database or
//! approximate index is introduced.
#![forbid(unsafe_code)]

use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
    sync::{Arc, Mutex, Weak},
};

use serde::{Deserialize, Serialize};
use thiserror::Error;
use tracedecay_domain::{
    AdmittedEmbeddingProjectionKeyV1, CodeGenerationId, CodeSearchChunkId, ContentDigest,
    ManifestDigest, ProjectionBatchReceiptV1, ProjectionKeyV1, ProjectionKindV1,
    ProjectionOperationV1, ProjectionOutcomeV1, canonical_sha256,
};

pub use tracedecay_domain::VectorGenerationIdV1;

use tracedecay_code_index::projection::{expected_publication_digest, verify_batch_receipt};
use tracedecay_runtime_core::db::{Database, engine::params};
use tracedecay_runtime_core::sqlite_read_snapshot::{
    BOUNDED_PROBE_BUSY_TIMEOUT, open_read_only_probe,
};
use tracedecay_semantic::legacy_migration::{
    LegacyVectorInventoryEntryV1, LegacyVectorInventoryPortV1, LegacyVectorInventoryV1,
    LegacyVectorMigrationOutcomeKindV1, LegacyVectorMigrationOwnerTransactionV1,
    LegacyVectorMigrationReceiptV1, canonical_chunk_set_digest,
};
use tracedecay_semantic::projector::{
    PreparedVectorGenerationV1, ProjectedChunkVectorV1, SemanticProjectionErrorV1,
};

const VECTOR_GENERATION_BUILD_DIGEST_DOMAIN: &str = "tracedecay.vector-generation-build.v1";
const VECTOR_GENERATION_MANIFEST_DIGEST_DOMAIN: &str = "tracedecay.vector-generation-manifest.v1";
const PHYSICAL_VECTOR_REUSE_DIGEST_DOMAIN: &str = "tracedecay.physical-vector-reuse.v1";
const VECTOR_GENERATION_STATE_OPERATION: &str = "persist semantic vector generations";
const VECTOR_GENERATION_STATE_SCHEMA_V1: &str = "
CREATE TABLE IF NOT EXISTS semantic_vector_generation_state_v1 (
    singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
    revision INTEGER NOT NULL CHECK (revision >= 0),
    state_json TEXT NOT NULL
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
/// committed-effect set, the prepared batches, and the physical-byte bindings
/// — is encoded once, addressed by the SHA-256 of those bytes, and written as
/// bounded slices. The state document keeps only the address, so it stays
/// generation-level regardless of corpus size. Content addressing also means
/// a staged collection and the published collection it becomes share one
/// stored copy, so publication writes no new slices.
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
/// Addresses resolved per read statement.
const VECTOR_STATE_ADDRESS_STATEMENT_ROWS: usize = 64;
const VECTOR_EVALUATION_STATE_SCHEMA_V1: &str = "
CREATE TABLE IF NOT EXISTS semantic_vector_evaluation_state_v1 (
    evaluation_id TEXT PRIMARY KEY,
    revision INTEGER NOT NULL CHECK (revision >= 0),
    state_json TEXT NOT NULL
) STRICT;
";
const LEGACY_VECTOR_QUARANTINE_SCHEMA_V1: &str = "
CREATE TABLE IF NOT EXISTS semantic_legacy_vector_quarantine_v1 (
    receipt_digest TEXT NOT NULL,
    legacy_generation TEXT NOT NULL,
    reason_digest TEXT NOT NULL,
    generation_json TEXT NOT NULL,
    receipt_json TEXT NOT NULL,
    PRIMARY KEY (receipt_digest, legacy_generation)
) STRICT;
";
const LEGACY_VECTOR_UNREADABLE_REASON_DOMAIN_V1: &str =
    "tracedecay.semantic-code.legacy-vector-unreadable-reason.v1";
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
/// `values` is still *accepted* on read. That is the whole forward migration:
/// a pre-migration blob loads unchanged, and the first write after loading it
/// persists the rows and drops the inline floats.
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
pub struct ExternalV1<T> {
    /// Content address of the sealed bytes; `None` while unsealed.
    address: Option<ContentDigest>,
    value: T,
}

impl<T> ExternalV1<T> {
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

impl<T> From<T> for ExternalV1<T> {
    fn from(value: T) -> Self {
        Self::new(value)
    }
}

impl<Element, T: FromIterator<Element>> FromIterator<Element> for ExternalV1<T> {
    fn from_iter<I: IntoIterator<Item = Element>>(iterator: I) -> Self {
        Self::new(T::from_iter(iterator))
    }
}

impl<T> std::ops::Deref for ExternalV1<T> {
    type Target = T;

    fn deref(&self) -> &T {
        &self.value
    }
}

impl<T> std::ops::DerefMut for ExternalV1<T> {
    fn deref_mut(&mut self) -> &mut T {
        self.address = None;
        &mut self.value
    }
}

impl<T: PartialEq> PartialEq for ExternalV1<T> {
    /// Identity is the collection, never where its bytes happen to live.
    fn eq(&self, other: &Self) -> bool {
        self.value == other.value
    }
}

impl<T: Eq> Eq for ExternalV1<T> {}

impl<T: Serialize> Serialize for ExternalV1<T> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        self.value.serialize(serializer)
    }
}

impl<'de, T: Deserialize<'de>> Deserialize<'de> for ExternalV1<T> {
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
type SealedCollectionV1 = (ContentDigest, Vec<Vec<u8>>);

trait ExternalSlotV1 {
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
    ) -> Result<Option<SealedCollectionV1>, VectorGenerationStoreErrorV1>;

    /// Fill the slot from the ordered slices stored at its address.
    fn fill(&mut self, slices: &[Vec<u8>]) -> Result<(), VectorGenerationStoreErrorV1>;
}

impl<T> ExternalSlotV1 for ExternalV1<T>
where
    T: Serialize + serde::de::DeserializeOwned,
{
    fn address(&self) -> Option<&ContentDigest> {
        self.address.as_ref()
    }

    fn seal(
        &mut self,
        needed: &mut dyn FnMut(&ContentDigest) -> bool,
    ) -> Result<Option<SealedCollectionV1>, VectorGenerationStoreErrorV1> {
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
/// The floats live in the payload table (addressed by `output_digest`); this
/// carries only the row identity, and it is itself externalized so the state
/// document never renders one row per chunk.
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

impl From<BTreeMap<CodeSearchChunkId, ProjectedChunkVectorV1>> for ExternalV1<VectorRowMapV1> {
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

/// The committed prepared batches of one staged build, floats elided.
#[derive(Clone, Debug, Default, PartialEq)]
struct PreparedBatchesV1(Vec<PreparedVectorGenerationV1>);

impl std::ops::Deref for PreparedBatchesV1 {
    type Target = Vec<PreparedVectorGenerationV1>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl std::ops::DerefMut for PreparedBatchesV1 {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

impl Serialize for PreparedBatchesV1 {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        externalized_vectors::prepared_batches::serialize(&self.0, serializer)
    }
}

impl<'de> Deserialize<'de> for PreparedBatchesV1 {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        externalized_vectors::prepared_batches::deserialize(deserializer).map(Self)
    }
}

/// State-document adapters that persist an [`ExternalV1`] as its content
/// address instead of its contents.
///
/// Deserialization accepts either form: an address string for a document this
/// store wrote, or the pre-migration inline collection. An inline value loads
/// with no address, which is exactly the signal the forward migration uses to
/// decide the document must be re-sealed.
mod external_state {
    use super::{ContentDigest, ExternalV1};
    use serde::de::{self, MapAccess, SeqAccess, Visitor};
    use serde::{Deserialize, Deserializer, Serialize, Serializer};
    use std::marker::PhantomData;

    /// Serializes an [`ExternalV1`] address, refusing an unsealed slot.
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

    struct AddressOrInlineV1<T>(PhantomData<T>);

    impl<'de, T> Visitor<'de> for AddressOrInlineV1<T>
    where
        T: Deserialize<'de> + Default,
    {
        type Value = ExternalV1<T>;

        fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
            formatter.write_str("an externalized collection address or its inline value")
        }

        fn visit_str<E>(self, value: &str) -> Result<Self::Value, E>
        where
            E: de::Error,
        {
            let address = ContentDigest::try_from(value.to_owned()).map_err(de::Error::custom)?;
            Ok(ExternalV1 {
                address: Some(address),
                value: T::default(),
            })
        }

        fn visit_seq<A>(self, sequence: A) -> Result<Self::Value, A::Error>
        where
            A: SeqAccess<'de>,
        {
            T::deserialize(de::value::SeqAccessDeserializer::new(sequence)).map(ExternalV1::new)
        }

        fn visit_map<A>(self, map: A) -> Result<Self::Value, A::Error>
        where
            A: MapAccess<'de>,
        {
            T::deserialize(de::value::MapAccessDeserializer::new(map)).map(ExternalV1::new)
        }
    }

    pub(super) fn serialize<T, S>(slot: &ExternalV1<T>, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        AddressRefV1(&slot.address).serialize(serializer)
    }

    pub(super) fn deserialize<'de, T, D>(deserializer: D) -> Result<ExternalV1<T>, D::Error>
    where
        T: Deserialize<'de> + Default,
        D: Deserializer<'de>,
    {
        deserializer.deserialize_any(AddressOrInlineV1(PhantomData))
    }

    /// The same adapter for a map of externalized collections, used by the
    /// per-generation physical-byte bindings.
    pub(super) mod address_map {
        use super::{AddressRefV1, Deserialize, Deserializer, ExternalV1, PhantomData, Serializer};
        use crate::store::vector_generations::VectorGenerationIdV1;
        use serde::Serialize;
        use std::collections::BTreeMap;

        struct SlotRefV1<'slot, T>(&'slot ExternalV1<T>, PhantomData<T>);

        impl<T> Serialize for SlotRefV1<'_, T> {
            fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
            where
                S: Serializer,
            {
                AddressRefV1(&self.0.address).serialize(serializer)
            }
        }

        pub(in super::super) fn serialize<T, S>(
            slots: &BTreeMap<VectorGenerationIdV1, ExternalV1<T>>,
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

        struct SlotV1<T>(ExternalV1<T>);

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
        ) -> Result<BTreeMap<VectorGenerationIdV1, ExternalV1<T>>, D::Error>
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
        use super::{AddressRefV1, Deserialize, Deserializer, ExternalV1, Serialize, Serializer};
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
            expected_chunk_ids: ExternalV1<Vec<CodeSearchChunkId>>,
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
        /// Pre-migration inline payload. Absent in every state this store
        /// writes; the loader hydrates those rows from the payload table.
        #[serde(default)]
        values: Vec<f32>,
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
                values: row.values,
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
    projection_key: ProjectionKeyV1,
    source_generation: CodeGenerationId,
    source_manifest_digest: ManifestDigest,
    base_generation: Option<VectorGenerationIdV1>,
    embedding_key: AdmittedEmbeddingProjectionKeyV1,
    #[serde(with = "external_state")]
    vectors: ExternalV1<VectorRowMapV1>,
    #[serde(with = "external_state")]
    tombstones: ExternalV1<Vec<CodeSearchChunkId>>,
    #[serde(with = "external_state")]
    tombstone_digests: ExternalV1<BTreeMap<CodeSearchChunkId, ContentDigest>>,
    #[serde(with = "external_state")]
    receipts: ExternalV1<Vec<ProjectionBatchReceiptV1>>,
    checkpoint: VectorProjectionCheckpointV1,
    manifest_digest: ManifestDigest,
}

impl PublishedVectorGenerationV1 {
    pub fn generation_id(&self) -> &VectorGenerationIdV1 {
        &self.generation_id
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
    #[error("legacy vector migration failed: {0}")]
    LegacyMigration(String),
    #[error("project vector generation storage failed: {0}")]
    Storage(String),
    #[error("project vector generation state changed repeatedly during compare-and-swap")]
    ConcurrentMutation,
    #[error("semantic projector handoff rejected: {0}")]
    Projection(#[from] SemanticProjectionErrorV1),
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct StagedVectorGenerationV1 {
    #[serde(with = "external_state::plan")]
    plan: VectorGenerationPlanV1,
    embedding_key: Option<AdmittedEmbeddingProjectionKeyV1>,
    #[serde(with = "external_state")]
    vectors: ExternalV1<VectorRowMapV1>,
    #[serde(with = "external_state")]
    tombstones: ExternalV1<BTreeMap<CodeSearchChunkId, ContentDigest>>,
    #[serde(with = "external_state")]
    batches: ExternalV1<PreparedBatchesV1>,
    #[serde(with = "external_state")]
    committed_chunk_effects: ExternalV1<BTreeSet<CodeSearchChunkId>>,
    checkpoint: VectorProjectionCheckpointV1,
}

#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PublishedStateV1 {
    generations: BTreeMap<VectorGenerationIdV1, PublishedVectorGenerationV1>,
    active_generation: Option<VectorGenerationIdV1>,
    #[serde(default)]
    legacy_migration_receipts: BTreeMap<ManifestDigest, LegacyVectorMigrationReceiptV1>,
    #[serde(skip, default)]
    physical_vectors: BTreeMap<ManifestDigest, PhysicalVectorPayloadV1>,
    #[serde(default, with = "external_state::address_map")]
    physical_vector_bindings:
        BTreeMap<VectorGenerationIdV1, ExternalV1<BTreeMap<CodeSearchChunkId, ManifestDigest>>>,
}

/// Deterministic state machine used directly by focused tests and persisted by
/// [`DatabaseVectorGenerationStoreV1`].
#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FakeVectorGenerationStoreV1 {
    staged: BTreeMap<VectorGenerationBuildIdV1, StagedVectorGenerationV1>,
    published: PublishedStateV1,
    #[serde(skip, default)]
    physical_vector_pool: PhysicalVectorBytePoolV1,
    #[serde(default, skip)]
    fail_before_publication_swap: bool,
}

impl FakeVectorGenerationStoreV1 {
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
        if let Some(existing) = self.staged.get(&build_id) {
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
        self.staged.insert(
            build_id.clone(),
            StagedVectorGenerationV1 {
                plan,
                embedding_key: None,
                vectors: ExternalV1::default(),
                tombstones: ExternalV1::default(),
                batches: ExternalV1::default(),
                committed_chunk_effects: ExternalV1::default(),
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
        self.staged.insert(
            build_id.clone(),
            StagedVectorGenerationV1 {
                plan,
                embedding_key: None,
                vectors: ExternalV1::default(),
                tombstones: ExternalV1::default(),
                batches: ExternalV1::default(),
                committed_chunk_effects: ExternalV1::default(),
                checkpoint,
            },
        );
        Ok(build_id)
    }

    /// Discard one unpublished build without changing any immutable
    /// generation or the active pointer. This is the cancellation boundary
    /// for asynchronous projection work.
    pub fn cancel_generation(&mut self, build_id: &VectorGenerationBuildIdV1) -> bool {
        self.staged.remove(build_id).is_some()
    }

    /// Atomically commit one batch's vector effects, tombstones, Plan 25
    /// receipt, and next checkpoint. Any validation failure leaves the prior
    /// staged state and checkpoint unchanged.
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
            .staged
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
        self.staged.insert(build_id.clone(), next);
        Ok(checkpoint)
    }

    /// Validate a fully staged immutable generation and atomically publish
    /// both its record and active pointer. Partial generations remain in
    /// `staged` and are never returned by active-generation reads.
    pub fn publish_generation(
        &mut self,
        build_id: &VectorGenerationBuildIdV1,
        expected_active_generation: Option<&VectorGenerationIdV1>,
    ) -> Result<VectorGenerationPublicationV1, VectorGenerationStoreErrorV1> {
        if self.published.active_generation.as_ref() != expected_active_generation {
            return Err(VectorGenerationStoreErrorV1::StaleActiveGeneration);
        }
        let staged = self
            .staged
            .get(build_id)
            .cloned()
            .ok_or(VectorGenerationStoreErrorV1::UnknownBuild)?;
        let expected = staged
            .plan
            .expected_chunk_ids
            .iter()
            .cloned()
            .collect::<BTreeSet<_>>();
        let actual = staged.vectors.keys().cloned().collect::<BTreeSet<_>>();
        if expected != actual || staged.batches.is_empty() {
            return Err(VectorGenerationStoreErrorV1::IncompleteGeneration);
        }
        let embedding_key = staged
            .embedding_key
            .clone()
            .ok_or(VectorGenerationStoreErrorV1::IncompleteGeneration)?;
        for vector in staged.vectors.values() {
            validate_vector_row(&staged.plan, &embedding_key, vector)?;
        }

        let manifest_digest =
            generation_identity_digest(&staged.plan, &staged.vectors, &staged.tombstones)?;
        let generation_id = VectorGenerationIdV1::new(manifest_digest.clone());
        let tombstone_digests = staged.tombstones;
        let mut generation = PublishedVectorGenerationV1 {
            generation_id: generation_id.clone(),
            projection_key: staged.plan.target_projection_key,
            source_generation: staged.plan.source_generation,
            source_manifest_digest: staged.plan.source_manifest_digest,
            base_generation: staged.plan.base_generation,
            embedding_key,
            vectors: staged.vectors,
            tombstones: ExternalV1::default(),
            tombstone_digests,
            receipts: staged
                .batches
                .into_inner()
                .0
                .into_iter()
                .map(|batch| batch.receipt)
                .collect(),
            checkpoint: staged.checkpoint.clone(),
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
        self.staged.remove(build_id);
        Ok(VectorGenerationPublicationV1 {
            generation_id,
            manifest_digest,
            checkpoint,
        })
    }

    /// Seal a complete generation inside caller-owned scratch state without
    /// making it active. This is the legacy-rebuild staging boundary: the
    /// scratch state is not queryable and can be discarded on any failure.
    pub(crate) fn seal_generation_inactive(
        &mut self,
        build_id: &VectorGenerationBuildIdV1,
    ) -> Result<VectorGenerationPublicationV1, VectorGenerationStoreErrorV1> {
        let prior_active = self.published.active_generation.clone();
        let publication = self.publish_generation(build_id, prior_active.as_ref())?;
        self.published.active_generation = prior_active;
        Ok(publication)
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

    /// Bind scratch-built generations to a validated migration receipt.
    ///
    /// The legacy active pointer belongs to the live state, not this scratch
    /// state, so it is checked by the database replacement transaction.
    fn finish_legacy_replacement(
        &mut self,
        transaction: &LegacyVectorMigrationOwnerTransactionV1,
    ) -> Result<LegacyVectorMigrationReceiptV1, VectorGenerationStoreErrorV1> {
        transaction
            .validate()
            .map_err(|error| VectorGenerationStoreErrorV1::LegacyMigration(error.to_string()))?;
        let mut rebuilt = BTreeMap::new();
        for item in &transaction.receipt.items {
            let Some(generation) = item.rebuilt_generation.as_ref() else {
                continue;
            };
            let identity = (
                item.source_generation.as_ref(),
                item.canonical_chunk_set_digest.as_ref(),
            );
            if rebuilt
                .insert(generation, identity)
                .is_some_and(|existing| existing != identity)
            {
                return Err(VectorGenerationStoreErrorV1::IncompatibleBaseGeneration);
            }
        }
        if rebuilt.len() != self.published.generations.len() {
            return Err(VectorGenerationStoreErrorV1::IncompatibleBaseGeneration);
        }
        for (generation_id, (source_generation, expected_chunk_set_digest)) in rebuilt {
            let generation = self
                .published
                .generations
                .get(generation_id)
                .ok_or(VectorGenerationStoreErrorV1::IncompatibleBaseGeneration)?;
            if Some(generation.source_generation()) != source_generation {
                return Err(VectorGenerationStoreErrorV1::IncompatibleBaseGeneration);
            }
            let chunk_identities = generation
                .vectors
                .iter()
                .map(|(chunk_id, vector)| (chunk_id.clone(), vector.chunk_digest.clone()))
                .collect::<Vec<_>>();
            let actual_chunk_set_digest =
                canonical_chunk_set_digest(generation.source_generation(), &chunk_identities)
                    .map_err(|error| {
                        VectorGenerationStoreErrorV1::LegacyMigration(error.to_string())
                    })?;
            if Some(&actual_chunk_set_digest) != expected_chunk_set_digest {
                return Err(VectorGenerationStoreErrorV1::IncompatibleBaseGeneration);
            }
        }
        if let Some(next_active) = &transaction.next_active_generation
            && !self.published.generations.contains_key(next_active)
        {
            return Err(VectorGenerationStoreErrorV1::IncompatibleBaseGeneration);
        }
        self.staged.clear();
        self.published
            .active_generation
            .clone_from(&transaction.next_active_generation);
        self.published.legacy_migration_receipts.insert(
            transaction.receipt.receipt_digest.clone(),
            transaction.receipt.clone(),
        );
        Ok(transaction.receipt.clone())
    }

    pub fn active_checkpoint(&self) -> Option<&VectorProjectionCheckpointV1> {
        self.active_generation()
            .map(PublishedVectorGenerationV1::checkpoint)
    }

    /// The checkpoint of one staged build, which is how a resumed run learns
    /// how many of its batches are already durable.
    pub fn staged_checkpoint(
        &self,
        build_id: &VectorGenerationBuildIdV1,
    ) -> Option<&VectorProjectionCheckpointV1> {
        self.staged.get(build_id).map(|staged| &staged.checkpoint)
    }

    pub fn active_generation(&self) -> Option<&PublishedVectorGenerationV1> {
        self.active_generation_id()
            .and_then(|id| self.published.generations.get(id))
    }

    /// Return the active immutable generation only when every query-facing
    /// projection and source identity matches exactly. A staged replacement
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
}

/// SQLite-backed, non-authoritative state used by the native semantic evaluator.
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

/// Identity-only snapshot of the legacy state. The SQL adapter never returns
/// legacy vector payloads to Rust.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DatabaseLegacyVectorInventoryV1 {
    revision: i64,
    inventory: LegacyVectorInventoryV1,
}

impl LegacyVectorInventoryPortV1 for DatabaseLegacyVectorInventoryV1 {
    fn read_only_inventory(
        &self,
    ) -> Result<
        LegacyVectorInventoryV1,
        tracedecay_semantic::legacy_migration::LegacyVectorMigrationErrorV1,
    > {
        Ok(self.inventory.clone())
    }
}

/// Read only the code-generation identities named by structurally readable
/// vector generations, without opening a daemon runtime or deserializing vector
/// payloads. This is the offline equivalent of
/// [`DatabaseVectorGenerationStoreV1::read_legacy_inventory`] followed by
/// [`LegacyVectorInventoryV1::retained_readable_sources`].
#[cfg(test)]
pub(crate) fn retained_readable_sources_from_read_only_database(
    database_path: &Path,
) -> Result<BTreeSet<CodeGenerationId>, VectorGenerationStoreErrorV1> {
    retained_readable_sources_from_optional_read_only_database(database_path)?.ok_or_else(|| {
        VectorGenerationStoreErrorV1::Storage(format!(
            "vector generation state table is missing from '{}'",
            database_path.display()
        ))
    })
}

/// Union readable code-generation sources across every graph database in a
/// project store. Code-index files are project-scoped while vector inventories
/// may reside in the root graph database or a branch graph database, so an
/// offline sweep must conservatively mark sources from all inventories.
pub fn retained_readable_sources_from_read_only_project_store(
    data_root: &Path,
) -> Result<BTreeSet<CodeGenerationId>, VectorGenerationStoreErrorV1> {
    let mut database_paths = vec![data_root.join(tracedecay_runtime_core::config::DB_FILENAME)];
    let branches_root = data_root.join("branches");
    if let Ok(entries) = std::fs::read_dir(&branches_root) {
        for entry in entries {
            let entry = entry.map_err(storage_error)?;
            let path = entry.path();
            if path.extension().and_then(|extension| extension.to_str()) == Some("db") {
                database_paths.push(path);
            }
        }
    }
    database_paths.sort();
    let mut readable_sources = BTreeSet::new();
    let mut inventory_count = 0usize;
    for database_path in database_paths {
        if !database_path.is_file() {
            continue;
        }
        if let Some(sources) =
            retained_readable_sources_from_optional_read_only_database(&database_path)?
        {
            inventory_count += 1;
            readable_sources.extend(sources);
        }
    }
    if inventory_count == 0 {
        return Err(VectorGenerationStoreErrorV1::Storage(format!(
            "no vector generation inventory exists under '{}'",
            data_root.display()
        )));
    }
    Ok(readable_sources)
}

fn retained_readable_sources_from_optional_read_only_database(
    database_path: &Path,
) -> Result<Option<BTreeSet<CodeGenerationId>>, VectorGenerationStoreErrorV1> {
    let connection =
        open_read_only_probe(database_path, BOUNDED_PROBE_BUSY_TIMEOUT).map_err(storage_error)?;
    let has_inventory = connection
        .query_row(
            "SELECT EXISTS(
                SELECT 1
                FROM sqlite_schema
                WHERE type = 'table'
                  AND name = 'semantic_vector_generation_state_v1'
             )",
            [],
            |row| row.get::<_, bool>(0),
        )
        .map_err(storage_error)?;
    if !has_inventory {
        return Ok(None);
    }
    let (generations_type, active_type, active_raw) = connection
        .query_row(
            "SELECT json_type(state_json, '$.published.generations'),
                    json_type(state_json, '$.published.active_generation'),
                    CAST(json_extract(
                        state_json,
                        '$.published.active_generation'
                    ) AS TEXT)
             FROM semantic_vector_generation_state_v1
             WHERE singleton = 1",
            [],
            |row| {
                Ok((
                    row.get::<_, Option<String>>(0)?,
                    row.get::<_, Option<String>>(1)?,
                    row.get::<_, Option<String>>(2)?,
                ))
            },
        )
        .map_err(storage_error)?;
    if generations_type.as_deref() != Some("object") {
        return Err(VectorGenerationStoreErrorV1::LegacyMigration(
            "legacy generation inventory is not a JSON object".to_owned(),
        ));
    }
    match (active_type.as_deref(), active_raw.as_deref()) {
        (None | Some("null"), None) => {}
        (Some("text"), Some(raw)) => {
            parse_vector_generation_id(raw)?;
        }
        _ => {
            return Err(VectorGenerationStoreErrorV1::LegacyMigration(
                "legacy active generation identity is unreadable".to_owned(),
            ));
        }
    }
    let mut statement = connection
        .prepare(
            "SELECT entry.key,
                    entry.type,
                    CASE WHEN entry.type = 'object'
                         THEN CAST(json_extract(entry.value, '$.generation_id') AS TEXT)
                    END,
                    CASE WHEN entry.type = 'object'
                         THEN CAST(json_extract(entry.value, '$.source_generation') AS TEXT)
                    END
             FROM semantic_vector_generation_state_v1 AS state
             JOIN json_each(state.state_json, '$.published.generations') AS entry
             WHERE state.singleton = 1
             ORDER BY entry.key",
        )
        .map_err(storage_error)?;
    let mut rows = statement.query([]).map_err(storage_error)?;
    let mut readable_sources = BTreeSet::new();
    while let Some(row) = rows.next().map_err(storage_error)? {
        let map_key = row.get::<_, String>(0).map_err(storage_error)?;
        let value_type = row.get::<_, Option<String>>(1).map_err(storage_error)?;
        let embedded_generation = row.get::<_, Option<String>>(2).map_err(storage_error)?;
        let source_generation = row.get::<_, Option<String>>(3).map_err(storage_error)?;
        let legacy_generation = parse_vector_generation_id(&map_key)?;
        let embedded_matches = embedded_generation
            .as_deref()
            .and_then(|raw| parse_vector_generation_id(raw).ok())
            .as_ref()
            == Some(&legacy_generation);
        let source_generation =
            source_generation.and_then(|raw| CodeGenerationId::try_from(raw).ok());
        if value_type.as_deref() == Some("object")
            && embedded_matches
            && let Some(source_generation) = source_generation
        {
            readable_sources.insert(source_generation);
        }
    }
    Ok(Some(readable_sources))
}

impl<'database> DatabaseVectorGenerationStoreV1<'database> {
    pub async fn open(database: &'database Database) -> Result<Self, VectorGenerationStoreErrorV1> {
        let store = Self::open_legacy_migration(database).await?;
        store.migrate_inline_vector_payloads().await?;
        Ok(store)
    }

    /// Move a pre-migration state document onto externalized storage.
    ///
    /// Covers both externalizations: a document that still carries floats
    /// inline moves onto row-per-vector payloads, and one that still carries
    /// corpus-sized metadata inline moves onto addressed state slices. Both
    /// are forward-only and crash-safe: the new rows and the rewritten
    /// document commit in one transaction guarded by the same revision the
    /// document was read at, so a crash leaves the original blob intact and
    /// the next open retries. Once migrated the check is a load with nothing
    /// inline to find, and this is a no-op.
    async fn migrate_inline_vector_payloads(&self) -> Result<(), VectorGenerationStoreErrorV1> {
        for _ in 0..MAX_STATE_CAS_RETRIES {
            let (revision, mut state, load) = self.load_state().await?;
            if !load.needs_forward_migration() {
                return Ok(());
            }
            let pending_slices = seal_external_state(&mut state, &load.durable_slices)?;
            let state_json = serde_json::to_string(&state).map_err(storage_error)?;
            let transaction = self
                .database
                .begin_write_transaction(VECTOR_GENERATION_STATE_OPERATION)
                .await
                .map_err(storage_error)?;
            write_vector_payloads(&transaction, VECTOR_PAYLOAD_TABLE_V1, &state, &load.durable)
                .await?;
            write_state_slices(&transaction, VECTOR_STATE_SLICE_TABLE_V1, &pending_slices).await?;
            let changed = transaction
                .execute_engine(
                    "UPDATE semantic_vector_generation_state_v1
                     SET revision = revision + 1, state_json = ?1
                     WHERE singleton = 1 AND revision = ?2",
                    params![state_json, revision],
                )
                .await
                .map_err(storage_error)?;
            if changed == 1 {
                transaction.commit().await.map_err(storage_error)?;
                return Ok(());
            }
            transaction.rollback().await.map_err(storage_error)?;
        }
        Err(VectorGenerationStoreErrorV1::ConcurrentMutation)
    }

    /// Open only the identity/atomic-replacement migration boundary.
    ///
    /// Unlike normal runtime open, this does not deserialize legacy state and
    /// therefore remains callable when old vector payloads are unreadable.
    pub async fn open_legacy_migration(
        database: &'database Database,
    ) -> Result<Self, VectorGenerationStoreErrorV1> {
        database
            .execute_write_batch(
                VECTOR_GENERATION_STATE_OPERATION,
                VECTOR_GENERATION_STATE_SCHEMA_V1,
            )
            .await
            .map_err(storage_error)?;
        database
            .execute_write_batch(VECTOR_GENERATION_STATE_OPERATION, VECTOR_PAYLOAD_SCHEMA_V1)
            .await
            .map_err(storage_error)?;
        database
            .execute_write_batch(
                VECTOR_GENERATION_STATE_OPERATION,
                VECTOR_STATE_SLICE_SCHEMA_V1,
            )
            .await
            .map_err(storage_error)?;
        let initial_state = serde_json::to_string(&FakeVectorGenerationStoreV1::default())
            .map_err(storage_error)?;
        database
            .execute_write_engine(
                VECTOR_GENERATION_STATE_OPERATION,
                "INSERT OR IGNORE INTO semantic_vector_generation_state_v1 (
                    singleton, revision, state_json
                 ) VALUES (1, 0, ?1)",
                params![initial_state],
            )
            .await
            .map_err(storage_error)?;
        Ok(Self { database })
    }

    /// Read the one active immutable generation needed by a request without
    /// entering the writer lane or deserializing staged/inactive generations.
    pub(crate) async fn read_active_generation_for(
        database: &Database,
        embedding_key: &AdmittedEmbeddingProjectionKeyV1,
        source_generation: &CodeGenerationId,
        source_manifest_digest: &ManifestDigest,
    ) -> Result<Option<PublishedVectorGenerationV1>, VectorGenerationStoreErrorV1> {
        Ok(Self::read_active_generation_snapshot_for(
            database,
            embedding_key,
            source_generation,
            source_manifest_digest,
        )
        .await?
        .map(ActiveVectorGenerationSnapshotV1::into_generation))
    }

    /// Read the atomically active immutable generation without entering the
    /// writer lane. Callers must apply their own source/projection admission.
    pub(crate) async fn read_active_generation(
        database: &Database,
    ) -> Result<Option<PublishedVectorGenerationV1>, VectorGenerationStoreErrorV1> {
        Ok(Self::read_active_generation_snapshot(database)
            .await?
            .map(ActiveVectorGenerationSnapshotV1::into_generation))
    }

    async fn read_active_generation_snapshot(
        database: &Database,
    ) -> Result<Option<ActiveVectorGenerationSnapshotV1>, VectorGenerationStoreErrorV1> {
        let mut rows = database
            .engine_conn()
            .query(
                "SELECT state.revision, entry.value
                 FROM semantic_vector_generation_state_v1 AS state
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
        hydrate_generation_payloads(database, VECTOR_PAYLOAD_TABLE_V1, &mut generation).await?;
        generation.validate_persisted()?;
        Ok(Some(ActiveVectorGenerationSnapshotV1 {
            revision,
            generation,
        }))
    }

    pub(crate) async fn read_active_generation_snapshot_for(
        database: &Database,
        embedding_key: &AdmittedEmbeddingProjectionKeyV1,
        source_generation: &CodeGenerationId,
        source_manifest_digest: &ManifestDigest,
    ) -> Result<Option<ActiveVectorGenerationSnapshotV1>, VectorGenerationStoreErrorV1> {
        let Some(snapshot) = Self::read_active_generation_snapshot(database).await? else {
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
                 FROM semantic_vector_generation_state_v1
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
        generation_id: &VectorGenerationIdV1,
    ) -> Result<Option<PublishedVectorGenerationV1>, VectorGenerationStoreErrorV1> {
        let mut rows = database
            .engine_conn()
            .query(
                "SELECT entry.value
                 FROM semantic_vector_generation_state_v1 AS state
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
        hydrate_generation_payloads(database, VECTOR_PAYLOAD_TABLE_V1, &mut generation).await?;
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
        self.mutate_retiring_state(|state| state.rebuild_generation(plan.clone()))
            .await
    }

    pub async fn cancel_generation(
        &self,
        build_id: &VectorGenerationBuildIdV1,
    ) -> Result<bool, VectorGenerationStoreErrorV1> {
        self.mutate_retiring_state(|state| Ok(state.cancel_generation(build_id)))
            .await
    }

    pub async fn commit_batch(
        &self,
        build_id: &VectorGenerationBuildIdV1,
        expected_checkpoint: Option<&VectorProjectionCheckpointV1>,
        prepared: PreparedVectorGenerationV1,
    ) -> Result<VectorProjectionCheckpointV1, VectorGenerationStoreErrorV1> {
        self.mutate_state(|state| state.commit_batch_ref(build_id, expected_checkpoint, &prepared))
            .await
    }

    pub async fn publish_generation(
        &self,
        build_id: &VectorGenerationBuildIdV1,
        expected_active_generation: Option<&VectorGenerationIdV1>,
    ) -> Result<VectorGenerationPublicationV1, VectorGenerationStoreErrorV1> {
        self.mutate_retiring_state(|state| {
            state.publish_generation(build_id, expected_active_generation)
        })
        .await
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

    /// Snapshot legacy generation identities without deserializing or
    /// returning any legacy vector payload.
    pub async fn read_legacy_inventory(
        &self,
    ) -> Result<DatabaseLegacyVectorInventoryV1, VectorGenerationStoreErrorV1> {
        let mut rows = self
            .database
            .engine_conn()
            .query(
                "SELECT state.revision,
                        json_type(state.state_json, '$.published.generations'),
                        json_type(state.state_json, '$.published.active_generation'),
                        CAST(json_extract(
                            state.state_json,
                            '$.published.active_generation'
                        ) AS TEXT),
                        entry.key,
                        entry.type,
                        CASE WHEN entry.type = 'object'
                             THEN CAST(json_extract(
                                 entry.value,
                                 '$.generation_id'
                             ) AS TEXT)
                        END,
                        CASE WHEN entry.type = 'object'
                             THEN CAST(json_extract(
                                 entry.value,
                                 '$.source_generation'
                             ) AS TEXT)
                        END
                 FROM semantic_vector_generation_state_v1 AS state
                 LEFT JOIN json_each(
                     state.state_json,
                     '$.published.generations'
                 ) AS entry
                 WHERE state.singleton = 1
                 ORDER BY entry.key",
                (),
            )
            .await
            .map_err(storage_error)?;
        let mut revision = None;
        let mut expected_active_generation = None;
        let mut entries = Vec::new();
        while let Some(row) = rows.next().await.map_err(storage_error)? {
            let row_revision = row.get::<i64>(0).map_err(storage_error)?;
            if revision
                .replace(row_revision)
                .is_some_and(|prior| prior != row_revision)
            {
                return Err(VectorGenerationStoreErrorV1::ConcurrentMutation);
            }
            if row
                .get::<Option<String>>(1)
                .map_err(storage_error)?
                .as_deref()
                != Some("object")
            {
                return Err(VectorGenerationStoreErrorV1::LegacyMigration(
                    "legacy generation inventory is not a JSON object".to_owned(),
                ));
            }
            let active_type = row.get::<Option<String>>(2).map_err(storage_error)?;
            let active_raw = row.get::<Option<String>>(3).map_err(storage_error)?;
            expected_active_generation = match (active_type.as_deref(), active_raw.as_deref()) {
                (None | Some("null"), None) => None,
                (Some("text"), Some(raw)) => Some(parse_vector_generation_id(raw)?),
                _ => {
                    return Err(VectorGenerationStoreErrorV1::LegacyMigration(
                        "legacy active generation identity is unreadable".to_owned(),
                    ));
                }
            };
            let Some(map_key) = row.get::<Option<String>>(4).map_err(storage_error)? else {
                continue;
            };
            let legacy_generation = parse_vector_generation_id(&map_key)?;
            let value_type = row.get::<Option<String>>(5).map_err(storage_error)?;
            let embedded_generation = row.get::<Option<String>>(6).map_err(storage_error)?;
            let source_generation = row.get::<Option<String>>(7).map_err(storage_error)?;
            let readable = value_type.as_deref() == Some("object")
                && embedded_generation
                    .as_deref()
                    .and_then(|raw| parse_vector_generation_id(raw).ok())
                    .as_ref()
                    == Some(&legacy_generation)
                && source_generation
                    .as_deref()
                    .and_then(|raw| CodeGenerationId::try_from(raw.to_owned()).ok())
                    .is_some();
            if readable {
                entries.push(LegacyVectorInventoryEntryV1::Readable {
                    legacy_generation,
                    source_generation: CodeGenerationId::try_from(
                        source_generation.unwrap_or_default(),
                    )
                    .map_err(|error| {
                        VectorGenerationStoreErrorV1::LegacyMigration(error.to_string())
                    })?,
                });
            } else {
                let reason_digest = canonical_sha256(&(
                    LEGACY_VECTOR_UNREADABLE_REASON_DOMAIN_V1,
                    &map_key,
                    &value_type,
                    &embedded_generation,
                    &source_generation,
                ))
                .map_err(storage_error)?;
                entries.push(LegacyVectorInventoryEntryV1::Unreadable {
                    legacy_generation,
                    reason_digest,
                });
            }
        }
        drop(rows);
        Ok(DatabaseLegacyVectorInventoryV1 {
            revision: revision.ok_or_else(|| {
                VectorGenerationStoreErrorV1::Storage(
                    "vector generation state row is missing".to_owned(),
                )
            })?,
            inventory: LegacyVectorInventoryV1 {
                expected_active_generation,
                entries,
            },
        })
    }

    /// Return a durable completed migration receipt, if atomic replacement
    /// already committed. A crash before replacement has no receipt and is
    /// therefore safely retried; a restart after replacement performs no
    /// second rebuild.
    pub(crate) async fn completed_legacy_migration_receipt(
        &self,
    ) -> Result<Option<LegacyVectorMigrationReceiptV1>, VectorGenerationStoreErrorV1> {
        let mut rows = self
            .database
            .engine_conn()
            .query(
                "SELECT entry.key, entry.value
                 FROM semantic_vector_generation_state_v1 AS state
                 JOIN json_each(
                     state.state_json,
                     '$.published.legacy_migration_receipts'
                 ) AS entry
                 WHERE state.singleton = 1
                   AND entry.type = 'object'
                 ORDER BY entry.key",
                (),
            )
            .await
            .map_err(storage_error)?;
        let mut completed = None;
        while let Some(row) = rows.next().await.map_err(storage_error)? {
            let key = row.get::<String>(0).map_err(storage_error)?;
            let receipt_json = row.get::<String>(1).map_err(storage_error)?;
            let receipt: LegacyVectorMigrationReceiptV1 =
                serde_json::from_str(&receipt_json).map_err(storage_error)?;
            receipt.validate().map_err(|error| {
                VectorGenerationStoreErrorV1::LegacyMigration(error.to_string())
            })?;
            if receipt.receipt_digest.as_str() != key {
                return Err(VectorGenerationStoreErrorV1::LegacyMigration(
                    "legacy migration receipt key does not match its digest".to_owned(),
                ));
            }
            completed = Some(receipt);
        }
        Ok(completed)
    }

    /// Replace the complete legacy state with scratch-built canonical
    /// generations in one guarded writer transaction. Unreadable state is
    /// copied into an isolated quarantine table by `SQLite` itself; its bytes
    /// never cross the Rust migration boundary.
    pub(crate) async fn replace_legacy_vectors_atomically(
        &self,
        inventory: &DatabaseLegacyVectorInventoryV1,
        mut replacement: FakeVectorGenerationStoreV1,
        transaction: &LegacyVectorMigrationOwnerTransactionV1,
    ) -> Result<LegacyVectorMigrationReceiptV1, VectorGenerationStoreErrorV1> {
        if inventory.inventory.expected_active_generation
            != transaction.expected_prior_active_generation
        {
            return Err(VectorGenerationStoreErrorV1::StaleActiveGeneration);
        }
        let receipt = replacement.finish_legacy_replacement(transaction)?;
        validate_loaded_state(&replacement)?;
        // The scratch replacement was built entirely in memory, so nothing it
        // references is durable yet: every collection seals and writes here.
        let pending_slices = seal_external_state(&mut replacement, &BTreeSet::new())?;
        let referenced_slices = referenced_state_addresses(&mut replacement)?;
        let state_json = serde_json::to_string(&replacement).map_err(storage_error)?;
        let receipt_json = serde_json::to_string(&receipt).map_err(storage_error)?;
        let quarantined_items = receipt
            .items
            .iter()
            .filter(|item| item.outcome == LegacyVectorMigrationOutcomeKindV1::QuarantineUnreadable)
            .collect::<Vec<_>>();
        let receipt_digest = receipt.receipt_digest.as_str().to_owned();

        let writer = self
            .database
            .begin_write_transaction(VECTOR_GENERATION_STATE_OPERATION)
            .await
            .map_err(storage_error)?;
        let mut current_rows = writer
            .query_engine(
                "SELECT revision,
                        CAST(json_extract(
                            state_json,
                            '$.published.active_generation'
                        ) AS TEXT)
                 FROM semantic_vector_generation_state_v1
                 WHERE singleton = 1",
                (),
            )
            .await
            .map_err(storage_error)?;
        let current = current_rows
            .next()
            .await
            .map_err(storage_error)?
            .ok_or_else(|| {
                VectorGenerationStoreErrorV1::Storage(
                    "vector generation state row is missing".to_owned(),
                )
            })?;
        let current_revision = current.get::<i64>(0).map_err(storage_error)?;
        let current_active = current
            .get::<Option<String>>(1)
            .map_err(storage_error)?
            .as_deref()
            .map(parse_vector_generation_id)
            .transpose()?;
        drop(current_rows);
        if current_revision != inventory.revision
            || current_active != inventory.inventory.expected_active_generation
        {
            writer.rollback().await.map_err(storage_error)?;
            return Err(VectorGenerationStoreErrorV1::ConcurrentMutation);
        }
        if !quarantined_items.is_empty() {
            writer
                .execute_batch_engine(LEGACY_VECTOR_QUARANTINE_SCHEMA_V1)
                .await
                .map_err(storage_error)?;
            for item in quarantined_items {
                let reason = item.quarantine_reason_digest.as_ref().ok_or_else(|| {
                    VectorGenerationStoreErrorV1::LegacyMigration(
                        "quarantine receipt has no reason digest".to_owned(),
                    )
                })?;
                let inserted = writer
                    .execute_engine(
                        "INSERT INTO semantic_legacy_vector_quarantine_v1 (
                        receipt_digest,
                        legacy_generation,
                        reason_digest,
                        generation_json,
                        receipt_json
                     )
                     SELECT ?1,
                            ?2,
                            ?3,
                            CASE entry.type
                                WHEN 'text' THEN json_quote(entry.value)
                                WHEN 'null' THEN 'null'
                                ELSE CAST(entry.value AS TEXT)
                            END,
                            ?4
                     FROM semantic_vector_generation_state_v1 AS state,
                          json_each(
                              state.state_json,
                              '$.published.generations'
                          ) AS entry
                     WHERE state.singleton = 1
                       AND state.revision = ?5
                       AND entry.key = ?2",
                        params![
                            receipt_digest.clone(),
                            item.legacy_generation.as_digest().as_str(),
                            reason.as_str(),
                            receipt_json.clone(),
                            inventory.revision
                        ],
                    )
                    .await
                    .map_err(storage_error)?;
                if inserted != 1 {
                    writer.rollback().await.map_err(storage_error)?;
                    return Err(VectorGenerationStoreErrorV1::ConcurrentMutation);
                }
            }
        }
        // The scratch replacement was built entirely in memory, so none of its
        // payloads are durable yet. They land in the same transaction as the
        // state swap, and the prune releases every payload the replaced legacy
        // state used to reference.
        write_vector_payloads(
            &writer,
            VECTOR_PAYLOAD_TABLE_V1,
            &replacement,
            &BTreeSet::new(),
        )
        .await?;
        prune_unreferenced_vector_payloads(&writer, VECTOR_PAYLOAD_TABLE_V1, &replacement).await?;
        write_state_slices(&writer, VECTOR_STATE_SLICE_TABLE_V1, &pending_slices).await?;
        prune_unreferenced_state_slices(&writer, VECTOR_STATE_SLICE_TABLE_V1, &referenced_slices)
            .await?;
        let changed = writer
            .execute_engine(
                "UPDATE semantic_vector_generation_state_v1
                 SET revision = revision + 1, state_json = ?1
                 WHERE singleton = 1 AND revision = ?2",
                params![state_json, inventory.revision],
            )
            .await
            .map_err(storage_error)?;
        if changed != 1 {
            writer.rollback().await.map_err(storage_error)?;
            return Err(VectorGenerationStoreErrorV1::ConcurrentMutation);
        }
        writer.commit().await.map_err(storage_error)?;
        Ok(receipt)
    }

    pub async fn active_generation_id(
        &self,
    ) -> Result<Option<VectorGenerationIdV1>, VectorGenerationStoreErrorV1> {
        let (_, state, _) = self.load_state().await?;
        Ok(state.active_generation_id().cloned())
    }

    /// The checkpoint of one staged build, or `None` when no build is staged
    /// under that identity yet.
    pub async fn staged_checkpoint(
        &self,
        build_id: &VectorGenerationBuildIdV1,
    ) -> Result<Option<VectorProjectionCheckpointV1>, VectorGenerationStoreErrorV1> {
        let (_, state, _) = self.load_state().await?;
        Ok(state.staged_checkpoint(build_id).cloned())
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
            &mut FakeVectorGenerationStoreV1,
        ) -> Result<ResultValue, VectorGenerationStoreErrorV1>,
    ) -> Result<ResultValue, VectorGenerationStoreErrorV1> {
        self.mutate_state_with_reclamation(false, mutation).await
    }

    /// As [`Self::mutate_state`], but also reclaims payload rows the committed
    /// state no longer references. Used by the mutations that retire staged or
    /// published generations.
    async fn mutate_retiring_state<ResultValue>(
        &self,
        mutation: impl FnMut(
            &mut FakeVectorGenerationStoreV1,
        ) -> Result<ResultValue, VectorGenerationStoreErrorV1>,
    ) -> Result<ResultValue, VectorGenerationStoreErrorV1> {
        self.mutate_state_with_reclamation(true, mutation).await
    }

    async fn mutate_state_with_reclamation<ResultValue>(
        &self,
        reclaim_unreferenced: bool,
        mut mutation: impl FnMut(
            &mut FakeVectorGenerationStoreV1,
        ) -> Result<ResultValue, VectorGenerationStoreErrorV1>,
    ) -> Result<ResultValue, VectorGenerationStoreErrorV1> {
        for _ in 0..MAX_STATE_CAS_RETRIES {
            let (revision, mut state, load) = self.load_state().await?;
            let result = mutation(&mut state)?;
            let pending_slices = seal_external_state(&mut state, &load.durable_slices)?;
            let state_json = serde_json::to_string(&state).map_err(storage_error)?;
            let transaction = self
                .database
                .begin_write_transaction(VECTOR_GENERATION_STATE_OPERATION)
                .await
                .map_err(storage_error)?;
            write_vector_payloads(&transaction, VECTOR_PAYLOAD_TABLE_V1, &state, &load.durable)
                .await?;
            write_state_slices(&transaction, VECTOR_STATE_SLICE_TABLE_V1, &pending_slices).await?;
            if reclaim_unreferenced {
                prune_unreferenced_vector_payloads(&transaction, VECTOR_PAYLOAD_TABLE_V1, &state)
                    .await?;
                let referenced = referenced_state_addresses(&mut state)?;
                prune_unreferenced_state_slices(
                    &transaction,
                    VECTOR_STATE_SLICE_TABLE_V1,
                    &referenced,
                )
                .await?;
            }
            let changed = transaction
                .execute_engine(
                    "UPDATE semantic_vector_generation_state_v1
                     SET revision = revision + 1, state_json = ?1
                     WHERE singleton = 1 AND revision = ?2",
                    params![state_json, revision],
                )
                .await
                .map_err(storage_error)?;
            if changed == 1 {
                transaction.commit().await.map_err(storage_error)?;
                return Ok(result);
            }
            transaction.rollback().await.map_err(storage_error)?;
        }
        Err(VectorGenerationStoreErrorV1::ConcurrentMutation)
    }

    async fn load_state(
        &self,
    ) -> Result<(i64, FakeVectorGenerationStoreV1, VectorPayloadLoadV1), VectorGenerationStoreErrorV1>
    {
        let mut rows = self
            .database
            .engine_conn()
            .query(
                "SELECT revision, state_json
                 FROM semantic_vector_generation_state_v1
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
        let mut state: FakeVectorGenerationStoreV1 =
            serde_json::from_str(&state_json).map_err(storage_error)?;
        drop(state_json);
        let (durable_slices, inline_collections) =
            hydrate_external_state(self.database, VECTOR_STATE_SLICE_TABLE_V1, &mut state).await?;
        let mut load =
            hydrate_vector_payloads(self.database, VECTOR_PAYLOAD_TABLE_V1, &mut state).await?;
        load.durable_slices = durable_slices;
        load.migrated_inline_collections = inline_collections;
        state.ensure_physical_reuse_index()?;
        validate_loaded_state(&state)?;
        Ok((revision, state, load))
    }
}

impl<'database> DatabaseVectorEvaluationStoreV1<'database> {
    pub(crate) async fn open(
        database: &'database Database,
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
                VECTOR_EVALUATION_PAYLOAD_SCHEMA_V1,
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
        let initial_state = serde_json::to_string(&FakeVectorGenerationStoreV1::default())
            .map_err(storage_error)?;
        let inserted = database
            .execute_write_engine(
                VECTOR_GENERATION_STATE_OPERATION,
                "INSERT INTO semantic_vector_evaluation_state_v1 (
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
                "DELETE FROM semantic_vector_evaluation_state_v1
                 WHERE evaluation_id = ?1",
                params![self.evaluation_id],
            )
            .await
            .map_err(storage_error)?;
        if deleted != 1 {
            transaction.rollback().await.map_err(storage_error)?;
            return Err(VectorGenerationStoreErrorV1::ConcurrentMutation);
        }
        // The measured run owned every evaluation payload only while some
        // evaluation state referenced it. Once the last one is gone the whole
        // lane is unreachable, so it is released with the row that named it.
        transaction
            .execute_engine(
                "DELETE FROM semantic_vector_evaluation_payload_v1
                 WHERE NOT EXISTS (SELECT 1 FROM semantic_vector_evaluation_state_v1)",
                (),
            )
            .await
            .map_err(storage_error)?;
        transaction
            .execute_engine(
                "DELETE FROM semantic_vector_evaluation_state_slice_v1
                 WHERE NOT EXISTS (SELECT 1 FROM semantic_vector_evaluation_state_v1)",
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
            &mut FakeVectorGenerationStoreV1,
        ) -> Result<ResultValue, VectorGenerationStoreErrorV1>,
    ) -> Result<ResultValue, VectorGenerationStoreErrorV1> {
        for _ in 0..MAX_STATE_CAS_RETRIES {
            let (revision, mut state, load) = self.load_state().await?;
            let result = mutation(&mut state)?;
            let pending_slices = seal_external_state(&mut state, &load.durable_slices)?;
            let state_json = serde_json::to_string(&state).map_err(storage_error)?;
            let transaction = self
                .database
                .begin_write_transaction(VECTOR_GENERATION_STATE_OPERATION)
                .await
                .map_err(storage_error)?;
            write_vector_payloads(
                &transaction,
                VECTOR_EVALUATION_PAYLOAD_TABLE_V1,
                &state,
                &load.durable,
            )
            .await?;
            write_state_slices(
                &transaction,
                VECTOR_EVALUATION_STATE_SLICE_TABLE_V1,
                &pending_slices,
            )
            .await?;
            let changed = transaction
                .execute_engine(
                    "UPDATE semantic_vector_evaluation_state_v1
                     SET revision = revision + 1, state_json = ?1
                     WHERE evaluation_id = ?2 AND revision = ?3",
                    params![state_json, self.evaluation_id.clone(), revision],
                )
                .await
                .map_err(storage_error)?;
            if changed == 1 {
                transaction.commit().await.map_err(storage_error)?;
                return Ok(result);
            }
            transaction.rollback().await.map_err(storage_error)?;
        }
        Err(VectorGenerationStoreErrorV1::ConcurrentMutation)
    }

    async fn load_state(
        &self,
    ) -> Result<(i64, FakeVectorGenerationStoreV1, VectorPayloadLoadV1), VectorGenerationStoreErrorV1>
    {
        let mut rows = self
            .database
            .engine_conn()
            .query(
                "SELECT revision, state_json
                 FROM semantic_vector_evaluation_state_v1
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
        let mut state: FakeVectorGenerationStoreV1 =
            serde_json::from_str(&state_json).map_err(storage_error)?;
        drop(state_json);
        let (durable_slices, inline_collections) = hydrate_external_state(
            self.database,
            VECTOR_EVALUATION_STATE_SLICE_TABLE_V1,
            &mut state,
        )
        .await?;
        let mut load = hydrate_vector_payloads(
            self.database,
            VECTOR_EVALUATION_PAYLOAD_TABLE_V1,
            &mut state,
        )
        .await?;
        load.durable_slices = durable_slices;
        load.migrated_inline_collections = inline_collections;
        state.ensure_physical_reuse_index()?;
        validate_loaded_state(&state)?;
        Ok((revision, state, load))
    }
}

impl FakeVectorGenerationStoreV1 {
    /// Rebuild the derived physical-byte index for every published generation.
    ///
    /// The generation map is moved aside rather than cloned: interning only
    /// touches `physical_vectors` and `physical_vector_bindings`, so a deep
    /// copy of every published generation — the whole float corpus, once per
    /// load — bought nothing but the borrow.
    /// In-memory stand-in for the payload table, used by restart tests that
    /// round-trip the state document without a database behind it.
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
    state: &FakeVectorGenerationStoreV1,
) -> Result<(), VectorGenerationStoreErrorV1> {
    if let Some(active) = &state.published.active_generation
        && !state.published.generations.contains_key(active)
    {
        return Err(VectorGenerationStoreErrorV1::Storage(
            "active vector generation pointer is dangling".to_string(),
        ));
    }
    for (receipt_digest, receipt) in &state.published.legacy_migration_receipts {
        if &receipt.receipt_digest != receipt_digest || receipt.validate().is_err() {
            return Err(VectorGenerationStoreErrorV1::Storage(
                "legacy vector migration receipt is invalid".to_string(),
            ));
        }
    }
    for (generation_id, generation) in &state.published.generations {
        if generation.generation_id() != generation_id {
            return Err(VectorGenerationStoreErrorV1::Storage(
                "published generation map key does not match record id".to_string(),
            ));
        }
        generation.validate_persisted()?;
        let bindings = state
            .published
            .physical_vector_bindings
            .get(generation_id)
            .ok_or_else(|| {
                VectorGenerationStoreErrorV1::Storage(
                    "published generation has no physical vector bindings".to_string(),
                )
            })?;
        if bindings.len() != generation.vectors.len() {
            return Err(VectorGenerationStoreErrorV1::Storage(
                "published generation physical vector membership is incomplete".to_string(),
            ));
        }
        for (chunk_id, vector) in generation.vectors.iter() {
            let physical_id = bindings.get(chunk_id).ok_or_else(|| {
                VectorGenerationStoreErrorV1::Storage(format!(
                    "published vector {chunk_id} has no physical byte binding"
                ))
            })?;
            let physical = state
                .published
                .physical_vectors
                .get(physical_id)
                .ok_or_else(|| {
                    VectorGenerationStoreErrorV1::Storage(format!(
                        "published vector {chunk_id} has a dangling physical byte binding"
                    ))
                })?;
            let (expected_id, expected_key) =
                physical_vector_reuse_key(generation.embedding_key(), vector)?;
            if physical_id != &expected_id
                || physical.reuse_key != expected_key
                || physical.values.0.as_ref() != vector.values.as_slice()
            {
                return Err(VectorGenerationStoreErrorV1::Storage(format!(
                    "published vector {chunk_id} physical byte binding drifted"
                )));
            }
        }
    }
    for staged in state.staged.values() {
        if let Some(embedding_key) = &staged.embedding_key {
            for vector in staged.vectors.values() {
                validate_vector_row(&staged.plan, embedding_key, vector)?;
            }
        }
        let canonical = staged.tombstones.keys().cloned().collect::<BTreeSet<_>>();
        if staged.tombstones.len() != canonical.len() {
            return Err(VectorGenerationStoreErrorV1::Storage(
                "staged tombstones contain duplicate chunk ids".to_string(),
            ));
        }
        for chunk_id in staged.tombstones.keys() {
            if staged.vectors.contains_key(chunk_id) {
                return Err(VectorGenerationStoreErrorV1::Storage(format!(
                    "staged generation retains both vector and tombstone for {chunk_id}"
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

/// Which payload rows a load resolved, and whether it had to fall back to the
/// pre-migration inline encoding.
#[derive(Debug, Default)]
struct VectorPayloadLoadV1 {
    /// Addresses already durable in the payload table. A later write skips
    /// them, so a commit persists only the rows its own batch introduced.
    durable: BTreeSet<ContentDigest>,
    /// Collection addresses already durable in the slice table, for the same
    /// reason.
    durable_slices: BTreeSet<ContentDigest>,
    /// True when the loaded document still carried inline floats.
    migrated_inline_payloads: bool,
    /// True when the loaded document still carried an inline O(store)
    /// collection that belongs in the slice table.
    migrated_inline_collections: bool,
}

impl VectorPayloadLoadV1 {
    /// Whether the loaded document predates an externalization and must be
    /// rewritten forward before it is served.
    fn needs_forward_migration(&self) -> bool {
        self.migrated_inline_payloads || self.migrated_inline_collections
    }
}

fn encode_vector_payload(values: &[f32]) -> Vec<u8> {
    let mut payload = Vec::with_capacity(std::mem::size_of_val(values));
    for value in values {
        payload.extend_from_slice(&value.to_le_bytes());
    }
    payload
}

fn decode_vector_payload(
    output_digest: &ContentDigest,
    dimensions: i64,
    payload: &[u8],
) -> Result<Vec<f32>, VectorGenerationStoreErrorV1> {
    let width = size_of::<f32>();
    if dimensions <= 0
        || !payload.len().is_multiple_of(width)
        || usize::try_from(dimensions).ok() != Some(payload.len() / width)
    {
        return Err(VectorGenerationStoreErrorV1::Storage(format!(
            "vector payload {output_digest} has an inconsistent width"
        )));
    }
    Ok(payload
        .chunks_exact(width)
        .map(|chunk| f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]))
        .collect())
}

impl FakeVectorGenerationStoreV1 {
    fn visit_vectors<'state>(&'state self, visit: &mut impl FnMut(&'state ProjectedChunkVectorV1)) {
        for generation in self.published.generations.values() {
            for vector in generation.vectors.values() {
                visit(vector);
            }
        }
        for staged in self.staged.values() {
            for vector in staged.vectors.values() {
                visit(vector);
            }
            for batch in staged.batches.iter() {
                for vector in &batch.vectors {
                    visit(vector);
                }
            }
        }
    }

    /// Refill the elided float payload of every vector row.
    ///
    /// Every write here goes through [`ExternalV1::elided_mut`]: the
    /// externalized encoding does not carry floats, so restoring them leaves
    /// the stored bytes — and therefore the collection address — unchanged.
    fn visit_vectors_mut(&mut self, visit: &mut impl FnMut(&mut ProjectedChunkVectorV1)) {
        for generation in self.published.generations.values_mut() {
            for vector in generation.vectors.elided_mut().values_mut() {
                visit(vector);
            }
        }
        for staged in self.staged.values_mut() {
            for vector in staged.vectors.elided_mut().values_mut() {
                visit(vector);
            }
            for batch in staged.batches.elided_mut().iter_mut() {
                for vector in &mut batch.vectors {
                    visit(vector);
                }
            }
        }
    }
}

/// Fill every externalized vector in `state` from `payload_table`.
///
/// Reads are paged: addresses are resolved in bounded `IN (...)` groups rather
/// than materializing the table. A missing address fails closed — a generation
/// whose floats cannot be resolved must not serve.
async fn hydrate_vector_payloads(
    database: &Database,
    payload_table: &str,
    state: &mut FakeVectorGenerationStoreV1,
) -> Result<VectorPayloadLoadV1, VectorGenerationStoreErrorV1> {
    let mut load = VectorPayloadLoadV1::default();
    let mut wanted = BTreeSet::new();
    state.visit_vectors(&mut |vector| {
        if vector.values.is_empty() {
            wanted.insert(vector.output_digest.clone());
        } else {
            load.migrated_inline_payloads = true;
        }
    });
    if wanted.is_empty() {
        return Ok(load);
    }
    let payloads = read_vector_payloads(database, payload_table, &wanted).await?;
    let mut missing = None;
    state.visit_vectors_mut(&mut |vector| {
        if !vector.values.is_empty() {
            return;
        }
        match payloads.get(&vector.output_digest) {
            Some(values) => vector.values.clone_from(values),
            None => {
                missing.get_or_insert_with(|| vector.output_digest.clone());
            }
        }
    });
    if let Some(missing) = missing {
        return Err(VectorGenerationStoreErrorV1::Storage(format!(
            "vector payload {missing} is missing from the store"
        )));
    }
    load.durable = wanted;
    Ok(load)
}

/// Seal a hand-built fixture so its document can be serialized.
///
/// The store's own writers seal inside their mutation path; fixtures that
/// build state directly go through here instead.
#[cfg(test)]
fn seal_test_state(
    state: &mut FakeVectorGenerationStoreV1,
) -> BTreeMap<ContentDigest, Vec<Vec<u8>>> {
    seal_external_state(state, &BTreeSet::new()).expect("seal externalized state")
}

/// Install collection slices for a hand-built fixture state.
#[cfg(test)]
async fn install_test_state_slices(
    database: &Database,
    slice_table: &str,
    state: &mut FakeVectorGenerationStoreV1,
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
/// slice and payload tables with the reference state still in memory.
#[cfg(test)]
fn restart_round_trip(state: &mut FakeVectorGenerationStoreV1) -> FakeVectorGenerationStoreV1 {
    let sealed = seal_test_state(state);
    let encoded = serde_json::to_string(&*state).expect("serialize vector state");
    let mut restarted: FakeVectorGenerationStoreV1 =
        serde_json::from_str(&encoded).expect("deserialize vector state");
    fill_from_sealed(&mut restarted, &sealed);
    restarted.hydrate_from(state);
    restarted
}

#[cfg(test)]
fn fill_from_sealed(
    state: &mut FakeVectorGenerationStoreV1,
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

/// Render a state document in its pre-migration encoding, with every
/// externalized collection written back inline.
#[cfg(test)]
fn legacy_inline_document(state: &mut FakeVectorGenerationStoreV1) -> serde_json::Value {
    let sealed = seal_test_state(state);
    let inline = sealed
        .iter()
        .map(|(address, slices)| {
            (
                address.as_str().to_owned(),
                serde_json::from_slice::<serde_json::Value>(&slices.concat())
                    .expect("inline collection"),
            )
        })
        .collect::<BTreeMap<_, _>>();
    let mut document = serde_json::to_value(&*state).expect("state document");
    inline_addresses(&mut document, &inline);
    document
}

#[cfg(test)]
fn inline_addresses(value: &mut serde_json::Value, inline: &BTreeMap<String, serde_json::Value>) {
    match value {
        serde_json::Value::String(text) => {
            if let Some(replacement) = inline.get(text.as_str()) {
                *value = replacement.clone();
            }
        }
        serde_json::Value::Array(items) => {
            for item in items {
                inline_addresses(item, inline);
            }
        }
        serde_json::Value::Object(fields) => {
            for field in fields.values_mut() {
                inline_addresses(field, inline);
            }
        }
        _ => {}
    }
}

/// Install payload rows for a hand-built fixture state that is written to the
/// state table directly instead of through the store's mutation path.
#[cfg(test)]
async fn install_test_vector_payloads(
    database: &Database,
    payload_table: &str,
    state: &FakeVectorGenerationStoreV1,
) {
    let transaction = database
        .begin_write_transaction("install test vector payloads")
        .await
        .expect("payload writer");
    write_vector_payloads(&transaction, payload_table, state, &BTreeSet::new())
        .await
        .expect("install test vector payloads");
    transaction.commit().await.expect("commit test payloads");
}

/// Fill one standalone published generation read outside the writer lane.
async fn hydrate_generation_payloads(
    database: &Database,
    payload_table: &str,
    generation: &mut PublishedVectorGenerationV1,
) -> Result<(), VectorGenerationStoreErrorV1> {
    let wanted = generation
        .vectors
        .values()
        .filter(|vector| vector.values.is_empty())
        .map(|vector| vector.output_digest.clone())
        .collect::<BTreeSet<_>>();
    if wanted.is_empty() {
        return Ok(());
    }
    let payloads = read_vector_payloads(database, payload_table, &wanted).await?;
    for vector in generation.vectors.values_mut() {
        if !vector.values.is_empty() {
            continue;
        }
        let values = payloads.get(&vector.output_digest).ok_or_else(|| {
            VectorGenerationStoreErrorV1::Storage(format!(
                "vector payload {} is missing from the store",
                vector.output_digest
            ))
        })?;
        vector.values.clone_from(values);
    }
    Ok(())
}

async fn read_vector_payloads(
    database: &Database,
    payload_table: &str,
    wanted: &BTreeSet<ContentDigest>,
) -> Result<BTreeMap<ContentDigest, Vec<f32>>, VectorGenerationStoreErrorV1> {
    let connection = database.engine_conn();
    let addresses = wanted.iter().cloned().collect::<Vec<_>>();
    let mut payloads = BTreeMap::new();
    for group in addresses.chunks(VECTOR_PAYLOAD_STATEMENT_ROWS) {
        let placeholders = (1..=group.len())
            .map(|index| format!("?{index}"))
            .collect::<Vec<_>>()
            .join(", ");
        let sql = format!(
            "SELECT output_digest, dimensions, payload
             FROM {payload_table}
             WHERE output_digest IN ({placeholders})"
        );
        let values = group
            .iter()
            .map(|digest| {
                tracedecay_runtime_core::db::engine::Value::Text(digest.as_str().to_owned())
            })
            .collect::<Vec<_>>();
        let mut rows = connection
            .query(
                &sql,
                tracedecay_runtime_core::db::engine::params_from_iter(values),
            )
            .await
            .map_err(storage_error)?;
        while let Some(row) = rows.next().await.map_err(storage_error)? {
            let output_digest =
                ContentDigest::try_from(row.get::<String>(0).map_err(storage_error)?)
                    .map_err(storage_error)?;
            let dimensions = row.get::<i64>(1).map_err(storage_error)?;
            let payload = row.get::<Vec<u8>>(2).map_err(storage_error)?;
            let decoded = decode_vector_payload(&output_digest, dimensions, &payload)?;
            payloads.insert(output_digest, decoded);
        }
        drop(rows);
    }
    Ok(payloads)
}

/// Persist every payload `state` references that is not already durable.
///
/// Writes happen inside the caller's transaction, so payload rows and the
/// state pointer that names them become visible together. Rows are
/// content-addressed and inserted with `OR IGNORE`, so a retried commit is a
/// no-op rather than a conflict.
async fn write_vector_payloads(
    transaction: &tracedecay_runtime_core::db::DatabaseWriteTransaction<'_>,
    payload_table: &str,
    state: &FakeVectorGenerationStoreV1,
    durable: &BTreeSet<ContentDigest>,
) -> Result<(), VectorGenerationStoreErrorV1> {
    let mut pending: BTreeMap<ContentDigest, &[f32]> = BTreeMap::new();
    state.visit_vectors(&mut |vector| {
        if !durable.contains(&vector.output_digest) {
            pending
                .entry(vector.output_digest.clone())
                .or_insert(&vector.values);
        }
    });
    if pending.is_empty() {
        return Ok(());
    }
    let rows = pending.into_iter().collect::<Vec<_>>();
    for group in rows.chunks(VECTOR_PAYLOAD_STATEMENT_ROWS) {
        let tuples = (0..group.len())
            .map(|index| {
                let base = index * 3;
                format!("(?{}, ?{}, ?{})", base + 1, base + 2, base + 3)
            })
            .collect::<Vec<_>>()
            .join(", ");
        let sql = format!(
            "INSERT OR IGNORE INTO {payload_table} (output_digest, dimensions, payload)
             VALUES {tuples}"
        );
        let mut values = Vec::with_capacity(group.len() * 3);
        for (output_digest, payload) in group {
            values.push(tracedecay_runtime_core::db::engine::Value::Text(
                output_digest.as_str().to_owned(),
            ));
            values.push(tracedecay_runtime_core::db::engine::Value::Integer(
                i64::try_from(payload.len()).map_err(storage_error)?,
            ));
            values.push(tracedecay_runtime_core::db::engine::Value::Blob(
                encode_vector_payload(payload),
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

/// Delete payload rows the committed state no longer references.
///
/// Retiring a generation is what makes its floats unreachable, so reclamation
/// runs with the state-shrinking mutations (publish, activate, deactivate,
/// cancel, rebuild) rather than on every commit.
async fn prune_unreferenced_vector_payloads(
    transaction: &tracedecay_runtime_core::db::DatabaseWriteTransaction<'_>,
    payload_table: &str,
    state: &FakeVectorGenerationStoreV1,
) -> Result<(), VectorGenerationStoreErrorV1> {
    let scratch_table = format!("temp.{payload_table}_referenced");
    transaction
        .execute_batch_engine(&format!(
            "CREATE TEMP TABLE IF NOT EXISTS {payload_table}_referenced (
                 output_digest TEXT PRIMARY KEY
             ) STRICT;
             DELETE FROM {scratch_table};"
        ))
        .await
        .map_err(storage_error)?;
    let mut referenced = BTreeSet::new();
    state.visit_vectors(&mut |vector| {
        referenced.insert(vector.output_digest.clone());
    });
    let referenced = referenced.into_iter().collect::<Vec<_>>();
    for group in referenced.chunks(VECTOR_PAYLOAD_STATEMENT_ROWS) {
        let tuples = (1..=group.len())
            .map(|index| format!("(?{index})"))
            .collect::<Vec<_>>()
            .join(", ");
        let values = group
            .iter()
            .map(|digest| {
                tracedecay_runtime_core::db::engine::Value::Text(digest.as_str().to_owned())
            })
            .collect::<Vec<_>>();
        transaction
            .execute_engine(
                &format!("INSERT OR IGNORE INTO {scratch_table} (output_digest) VALUES {tuples}"),
                tracedecay_runtime_core::db::engine::params_from_iter(values),
            )
            .await
            .map_err(storage_error)?;
    }
    transaction
        .execute_engine(
            &format!(
                "DELETE FROM {payload_table}
                 WHERE output_digest NOT IN (SELECT output_digest FROM {scratch_table})"
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

type ExternalSlotVisitV1<'visit> =
    dyn FnMut(&mut dyn ExternalSlotV1) -> Result<(), VectorGenerationStoreErrorV1> + 'visit;

impl PublishedVectorGenerationV1 {
    fn visit_external_slots(
        &mut self,
        visit: &mut ExternalSlotVisitV1<'_>,
    ) -> Result<(), VectorGenerationStoreErrorV1> {
        visit(&mut self.vectors)?;
        visit(&mut self.tombstones)?;
        visit(&mut self.tombstone_digests)?;
        visit(&mut self.receipts)
    }
}

impl FakeVectorGenerationStoreV1 {
    /// Every externalized collection in the state document, in a stable order.
    fn visit_external_slots(
        &mut self,
        visit: &mut ExternalSlotVisitV1<'_>,
    ) -> Result<(), VectorGenerationStoreErrorV1> {
        for staged in self.staged.values_mut() {
            visit(&mut staged.plan.expected_chunk_ids)?;
            visit(&mut staged.vectors)?;
            visit(&mut staged.tombstones)?;
            visit(&mut staged.batches)?;
            visit(&mut staged.committed_chunk_effects)?;
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
/// free — the staged collections and the published ones they become hash to
/// the same addresses, which are durable by then.
fn seal_external_state(
    state: &mut FakeVectorGenerationStoreV1,
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
    state: &mut FakeVectorGenerationStoreV1,
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
    state: &mut FakeVectorGenerationStoreV1,
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

async fn read_state_slices(
    database: &Database,
    slice_table: &str,
    address: &ContentDigest,
) -> Result<Vec<Vec<u8>>, VectorGenerationStoreErrorV1> {
    let mut rows = database
        .engine_conn()
        .query(
            &format!(
                "SELECT ordinal, payload
                 FROM {slice_table}
                 WHERE collection_digest = ?1
                 ORDER BY ordinal"
            ),
            params![address.as_str()],
        )
        .await
        .map_err(storage_error)?;
    let mut slices = Vec::new();
    while let Some(row) = rows.next().await.map_err(storage_error)? {
        let ordinal = row.get::<i64>(0).map_err(storage_error)?;
        if usize::try_from(ordinal).ok() != Some(slices.len()) {
            return Err(VectorGenerationStoreErrorV1::Storage(format!(
                "externalized state collection {address} has a gap in its slices"
            )));
        }
        slices.push(row.get::<Vec<u8>>(1).map_err(storage_error)?);
    }
    drop(rows);
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
                 WHERE collection_digest NOT IN
                     (SELECT collection_digest FROM {scratch_table})"
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

fn parse_vector_generation_id(
    raw: &str,
) -> Result<VectorGenerationIdV1, VectorGenerationStoreErrorV1> {
    ManifestDigest::try_from(raw.to_owned())
        .map(VectorGenerationIdV1::new)
        .map_err(|error| VectorGenerationStoreErrorV1::LegacyMigration(error.to_string()))
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
    vector.validate(embedding_key.embedding_key().dimensions)?;
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
    vector
        .validate(generation.embedding_key.embedding_key().dimensions)
        .map_err(|error| VectorGenerationStoreErrorV1::Storage(error.to_string()))?;
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
        BoundedSanitizedText, ChangedCodeChunkSetV1, ChangedCodeChunkV1, ChunkerRevision,
        CodeSearchChunkAnchorV1, CodeSearchChunkGrainV1, EmbeddingDeviceClassV1, EmbeddingMetricV1,
        EmbeddingNormalizationV1, EmbeddingPoolingV1, EmbeddingPrecisionV1,
        EmbeddingProjectionKeyV1, EmbeddingTruncationSideV1, FileOccurrenceId,
        LanguageDescriptorRevision, PolicyRevisionId, PrivacyDomainId, ProjectionBatchRequestV1,
        ProjectionReplayReasonV1, SanitizerRevision, SensitivityDecision, SensitivityLevelV1,
        SourceSpan,
    };
    use tracedecay_runtime_core::db::{DatabaseAuthority, TestDatabaseRuntimeMode};
    use tracedecay_semantic::legacy_migration::{
        CanonicalEligibleChunkSetV1, NeverCancelLegacyVectorMigrationV1,
        ProductionLegacyVectorCanonicalRebuilderV1, StagedCanonicalVectorRebuildV1,
        prepare_legacy_vector_migration,
    };

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

    fn canonical_chunk(
        chunk_id: &str,
        source_generation: &CodeGenerationId,
        digest: char,
    ) -> tracedecay_domain::CodeSearchChunkV1 {
        tracedecay_domain::CodeSearchChunkV1 {
            id: id(chunk_id),
            anchor: CodeSearchChunkAnchorV1 {
                generation_id: source_generation.clone(),
                file_occurrence_id: id::<FileOccurrenceId>("file.rs"),
                symbol_occurrence_id: None,
                parent_chunk_id: None,
                source_span: SourceSpan {
                    start_byte: 0,
                    end_byte: 4,
                },
                grain: CodeSearchChunkGrainV1::FileWindow,
                ordinal: 0,
            },
            content_digest: content_digest(digest),
            language_descriptor_revision: id::<LanguageDescriptorRevision>("rust.v1"),
            chunker_revision: id::<ChunkerRevision>("chunker.v1"),
            sanitizer_revision: id::<SanitizerRevision>("sanitizer.v1"),
            sensitivity: SensitivityDecision {
                level: SensitivityLevelV1::Public,
                policy_revision: id::<PolicyRevisionId>("policy.v1"),
            },
            exact_terms: vec![],
            subtokens: vec![],
            sanitized_text: BoundedSanitizedText::new("code").expect("sanitized text"),
        }
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
                "SELECT state_json FROM semantic_vector_generation_state_v1 WHERE singleton = 1",
                (),
            )
            .await
            .expect("state document");
        let row = rows.next().await.expect("state row").expect("state row");
        row.get::<String>(0).expect("state json")
    }

    async fn payload_row_count(database: &Database) -> i64 {
        let mut rows = database
            .engine_conn()
            .query("SELECT COUNT(*) FROM semantic_vector_payload_v1", ())
            .await
            .expect("payload count");
        let row = rows.next().await.expect("payload count row").expect("row");
        row.get::<i64>(0).expect("count")
    }

    fn insert_generation(
        store: &mut FakeVectorGenerationStoreV1,
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
        let mut store = FakeVectorGenerationStoreV1::new();
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
            .expect("staged build");
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
    fn successful_publication_consumes_the_staged_build() {
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
        let mut store = FakeVectorGenerationStoreV1::new();
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
            .expect("staged build");
        store
            .commit_batch(&build, None, prepared)
            .expect("complete reused batch");
        let publication = store
            .publish_generation(&build, Some(&base_id))
            .expect("atomic publication");

        assert!(!store.staged.contains_key(&build));
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
    async fn legacy_inventory_never_deserializes_vectors_and_quarantines_only_unreadable_entries() {
        let temporary = tempfile::tempdir().expect("temporary project database");
        let path = temporary.path().join("project.db");
        crate::register_test_schema_installer();
        let authority =
            DatabaseAuthority::acquire_test(&path, "legacy vector migration").expect("authority");
        let (database, _) =
            Database::publish_test_runtime(&path, &authority, TestDatabaseRuntimeMode::Initialize)
                .await
                .expect("database");
        let store = DatabaseVectorGenerationStoreV1::open_legacy_migration(&database)
            .await
            .expect("migration store");
        let readable = manifest_digest('a');
        let unreadable = manifest_digest('b');
        let source = "code-generation.legacy";
        let secret = "legacy-vector-secret";
        let generations = serde_json::Map::from_iter([
            (
                readable.as_str().to_owned(),
                serde_json::json!({
                    "generation_id": readable.as_str(),
                    "source_generation": source,
                    "vectors": [secret]
                }),
            ),
            (unreadable.as_str().to_owned(), serde_json::json!(secret)),
        ]);
        let state = serde_json::json!({
            "staged": {},
            "published": {
                "generations": generations,
                "active_generation": readable.as_str(),
                "legacy_migration_receipts": {},
                "physical_vector_bindings": {}
            }
        })
        .to_string();
        database
            .execute_write_engine(
                "install unreadable legacy vector fixture",
                "UPDATE semantic_vector_generation_state_v1
                 SET revision = revision + 1, state_json = ?1
                 WHERE singleton = 1",
                params![state],
            )
            .await
            .expect("legacy fixture");

        let inventory = store
            .read_legacy_inventory()
            .await
            .expect("identity-only inventory");
        assert_eq!(inventory.inventory.entries.len(), 2);
        assert!(matches!(
            &inventory.inventory.entries[0],
            LegacyVectorInventoryEntryV1::Readable { .. }
        ));
        assert!(matches!(
            &inventory.inventory.entries[1],
            LegacyVectorInventoryEntryV1::Unreadable { .. }
        ));
        let offline_sources = retained_readable_sources_from_read_only_database(&path)
            .expect("read-only source inventory");
        assert_eq!(
            offline_sources,
            BTreeSet::from([id(source)]),
            "offline retention planning must use exactly the readable source set"
        );
        let mut rebuilder = ProductionLegacyVectorCanonicalRebuilderV1::try_new(
            Vec::new(),
            |_| -> Result<
                StagedCanonicalVectorRebuildV1,
                tracedecay_semantic::legacy_migration::LegacyVectorMigrationErrorV1,
            > { unreachable!("no retained generations") },
        )
        .expect("empty production rebuilder");
        let transaction = prepare_legacy_vector_migration(
            &inventory,
            &mut rebuilder,
            &NeverCancelLegacyVectorMigrationV1,
        )
        .expect("migration transaction");
        store
            .replace_legacy_vectors_atomically(
                &inventory,
                FakeVectorGenerationStoreV1::new(),
                &transaction,
            )
            .await
            .expect("atomic replacement");

        assert_eq!(
            database
                .query_scalar_text(
                    "inspect isolated legacy quarantine",
                    "SELECT generation_json
                     FROM semantic_legacy_vector_quarantine_v1",
                )
                .await
                .expect("quarantine row"),
            serde_json::to_string(secret).expect("secret JSON")
        );
        assert_eq!(
            database
                .query_scalar_i64(
                    "prove readable legacy vectors were dropped",
                    "SELECT COUNT(*)
                     FROM semantic_legacy_vector_quarantine_v1",
                )
                .await
                .expect("quarantine count"),
            1
        );
        assert_eq!(
            database
                .query_scalar_i64(
                    "prove legacy bytes left active state",
                    "SELECT instr(state_json, 'legacy-vector-secret')
                     FROM semantic_vector_generation_state_v1
                     WHERE singleton = 1",
                )
                .await
                .expect("active state inspection"),
            0
        );
        let committed_state = database
            .query_scalar_text(
                "capture committed vector state",
                "SELECT state_json
                 FROM semantic_vector_generation_state_v1
                 WHERE singleton = 1",
            )
            .await
            .expect("committed state");
        assert_eq!(
            store
                .replace_legacy_vectors_atomically(
                    &inventory,
                    FakeVectorGenerationStoreV1::new(),
                    &transaction,
                )
                .await,
            Err(VectorGenerationStoreErrorV1::ConcurrentMutation)
        );
        assert_eq!(
            database
                .query_scalar_text(
                    "verify stale migration rollback",
                    "SELECT state_json
                     FROM semantic_vector_generation_state_v1
                     WHERE singleton = 1",
                )
                .await
                .expect("state after stale migration"),
            committed_state
        );
        DatabaseVectorGenerationStoreV1::open(&database)
            .await
            .expect("replacement state is runtime-readable");
    }

    #[tokio::test]
    async fn retained_canonical_rebuild_and_active_pointer_publish_together() {
        let temporary = tempfile::tempdir().expect("temporary project database");
        let path = temporary.path().join("project.db");
        crate::register_test_schema_installer();
        let authority =
            DatabaseAuthority::acquire_test(&path, "canonical vector rebuild").expect("authority");
        let (database, _) =
            Database::publish_test_runtime(&path, &authority, TestDatabaseRuntimeMode::Initialize)
                .await
                .expect("database");
        let store = DatabaseVectorGenerationStoreV1::open_legacy_migration(&database)
            .await
            .expect("migration store");
        let legacy = manifest_digest('a');
        let source: CodeGenerationId = id("code-generation.retained");
        let legacy_generations = serde_json::Map::from_iter([(
            legacy.as_str().to_owned(),
            serde_json::json!({
                "generation_id": legacy.as_str(),
                "source_generation": source.as_str(),
                "vectors": "legacy-bytes-must-not-be-used"
            }),
        )]);
        let legacy_state = serde_json::json!({
            "staged": {},
            "published": {
                "generations": legacy_generations,
                "active_generation": legacy.as_str(),
                "legacy_migration_receipts": {},
                "physical_vector_bindings": {}
            }
        })
        .to_string();
        database
            .execute_write_engine(
                "install readable legacy vector fixture",
                "UPDATE semantic_vector_generation_state_v1
                 SET revision = revision + 1, state_json = ?1
                 WHERE singleton = 1",
                params![legacy_state],
            )
            .await
            .expect("legacy fixture");
        let inventory = store
            .read_legacy_inventory()
            .await
            .expect("legacy inventory");
        let retained = CanonicalEligibleChunkSetV1::try_from_chunks(
            source.clone(),
            vec![canonical_chunk("chunk.v1.retained", &source, 'd')],
        )
        .expect("retained canonical code");
        let mut replacement = FakeVectorGenerationStoreV1::new();
        let rebuilt = logical_generation(
            'c',
            admitted_embedding(),
            source.as_str(),
            '3',
            "chunk.v1.retained",
            'd',
            vec![0.5],
        );
        let rebuilt_id = insert_generation(&mut replacement, rebuilt);
        let rebuilt_for_callback = rebuilt_id.clone();
        let mut rebuilder = ProductionLegacyVectorCanonicalRebuilderV1::try_new(
            vec![retained],
            move |chunks: &CanonicalEligibleChunkSetV1| {
                Ok(StagedCanonicalVectorRebuildV1 {
                    source_generation: chunks.source_generation().clone(),
                    rebuilt_generation: rebuilt_for_callback.clone(),
                    canonical_chunk_set_digest: chunks.digest().clone(),
                })
            },
        )
        .expect("production rebuilder");
        let transaction = prepare_legacy_vector_migration(
            &inventory,
            &mut rebuilder,
            &NeverCancelLegacyVectorMigrationV1,
        )
        .expect("canonical rebuild transaction");

        let receipt = store
            .replace_legacy_vectors_atomically(&inventory, replacement, &transaction)
            .await
            .expect("atomic canonical rebuild publication");
        assert_eq!(
            store
                .completed_legacy_migration_receipt()
                .await
                .expect("completed migration receipt"),
            Some(receipt)
        );

        let reopened = DatabaseVectorGenerationStoreV1::open(&database)
            .await
            .expect("runtime store");
        assert_eq!(
            reopened
                .active_generation_id()
                .await
                .expect("active generation"),
            Some(rebuilt_id)
        );
        assert_eq!(
            database
                .query_scalar_i64(
                    "prove rebuild did not quarantine readable legacy bytes",
                    "SELECT COUNT(*)
                     FROM sqlite_schema
                     WHERE type = 'table'
                       AND name = 'semantic_legacy_vector_quarantine_v1'",
                )
                .await
                .expect("quarantine schema count"),
            0
        );
    }

    #[tokio::test]
    async fn request_read_ignores_corrupt_inactive_and_staged_generations() {
        let temporary = tempfile::tempdir().expect("temporary project database");
        let path = temporary.path().join("project.db");
        crate::register_test_schema_installer();
        let authority = DatabaseAuthority::acquire_test(&path, "active vector request read")
            .expect("authority");
        let (database, _) =
            Database::publish_test_runtime(&path, &authority, TestDatabaseRuntimeMode::Initialize)
                .await
                .expect("database");
        let _store = DatabaseVectorGenerationStoreV1::open_legacy_migration(&database)
            .await
            .expect("migration store");
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
        let mut state = FakeVectorGenerationStoreV1::new();
        insert_generation(&mut state, active);
        state.published.active_generation = Some(active_id.clone());
        install_test_vector_payloads(&database, VECTOR_PAYLOAD_TABLE_V1, &state).await;
        install_test_state_slices(&database, VECTOR_STATE_SLICE_TABLE_V1, &mut state).await;
        let mut state_json = serde_json::to_value(&state).expect("vector state JSON");
        state_json["published"]["generations"][manifest_digest('e').as_str()] =
            serde_json::json!("corrupt-inactive-vector-bytes");
        state_json["staged"] = serde_json::json!({
            "corrupt-build": "corrupt-staged-vector-bytes"
        });
        database
            .execute_write_engine(
                "install inactive corruption fixture",
                "UPDATE semantic_vector_generation_state_v1
                 SET revision = revision + 1, state_json = ?1
                 WHERE singleton = 1",
                params![state_json.to_string()],
            )
            .await
            .expect("corrupt inactive fixture");

        let observed = DatabaseVectorGenerationStoreV1::read_active_generation_for(
            &database,
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
                &embedding,
                &source,
                &manifest_digest('5'),
            )
            .await
            .expect("wrong-manifest active read")
            .is_none(),
            "an active generation with the wrong source manifest must be denied"
        );
        assert!(
            DatabaseVectorGenerationStoreV1::open(&database)
                .await
                .is_err(),
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

        let evaluation =
            DatabaseVectorEvaluationStoreV1::open(&database, "semantic-native-evaluation:test")
                .await
                .expect("SQLite-backed evaluation store");
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
                    "SELECT COUNT(*) FROM semantic_vector_evaluation_state_v1",
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
                       AND name = 'semantic_vector_generation_state_v1'",
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
                    "SELECT COUNT(*) FROM semantic_vector_evaluation_state_v1",
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
        let mut store = FakeVectorGenerationStoreV1::new();
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
        let mut first_store = FakeVectorGenerationStoreV1::new();
        let mut second_store = FakeVectorGenerationStoreV1::new();

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

        let mut sealed_source = FakeVectorGenerationStoreV1::new();
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

        let mut state = FakeVectorGenerationStoreV1::default();
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
    /// out of the state document, and must let a restart resume a staged build.
    #[tokio::test]
    async fn row_per_vector_storage_preserves_identity_and_resumes_staged_builds() {
        let temporary = tempfile::tempdir().expect("temporary project database");
        let (database, _authority) =
            open_project_database(&temporary, "row per vector storage").await;
        let embedding = admitted_embedding();
        let source: CodeGenerationId = id("code-generation.row-per-vector");
        let chunk_id: CodeSearchChunkId = id("chunk.v1.row-per-vector");
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
        let mut oracle = FakeVectorGenerationStoreV1::new();
        let oracle_build = oracle
            .begin_generation(plan.clone())
            .expect("oracle build identity");
        oracle
            .commit_batch(&oracle_build, None, prepared.clone())
            .expect("oracle batch");
        let oracle_publication = oracle
            .publish_generation(&oracle_build, None)
            .expect("oracle publication");

        let store = DatabaseVectorGenerationStoreV1::open(&database)
            .await
            .expect("open vector generation store");
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
        assert_eq!(
            payload_row_count(&database).await,
            1,
            "the committed batch persists exactly its own vector row"
        );

        // Restart: a fresh handle over the same database resumes the staged
        // build and publishes the byte-identical generation identity.
        let restarted = DatabaseVectorGenerationStoreV1::open(&database)
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

        let observed = restarted
            .active_generation()
            .await
            .expect("read active generation")
            .expect("active generation");
        let expected = oracle
            .generation(&oracle_publication.generation_id)
            .expect("oracle generation");
        assert_eq!(&observed, expected, "round trip restores the exact vectors");
        assert_eq!(
            observed.vectors()[&chunk_id].values,
            vec![0.312_5_f32],
            "float payloads survive the row encoding exactly"
        );
        assert_eq!(
            observed.receipts(),
            expected.receipts(),
            "receipts are unchanged by externalized payload storage"
        );

        let bounded = DatabaseVectorGenerationStoreV1::read_active_generation_for(
            &database,
            &embedding,
            &source,
            observed.source_manifest_digest(),
        )
        .await
        .expect("bounded active read")
        .expect("compatible active generation");
        assert_eq!(&bounded, expected);

        // Publication retires the staged batch copy, so its payload row is the
        // published one and nothing more.
        assert_eq!(payload_row_count(&database).await, 1);
    }

    /// A pre-migration state document carries every float inline. Opening the
    /// store must move those floats to row-per-vector storage, drop them from
    /// the document, and leave every identity untouched.
    #[tokio::test]
    async fn opening_a_legacy_inline_state_migrates_payloads_to_rows() {
        let temporary = tempfile::tempdir().expect("temporary project database");
        let (database, _authority) =
            open_project_database(&temporary, "legacy inline payload migration").await;
        let embedding = admitted_embedding();
        let source: CodeGenerationId = id("code-generation.legacy-inline");
        let generation = logical_generation(
            'c',
            embedding.clone(),
            source.as_str(),
            '4',
            "chunk.v1.legacy-inline",
            'd',
            vec![0.75_f32],
        );
        let generation_id = generation.generation_id().clone();
        let source_manifest_digest = generation.source_manifest_digest().clone();
        let expected = generation.clone();
        let mut state = FakeVectorGenerationStoreV1::new();
        insert_generation(&mut state, generation);
        state.published.active_generation = Some(generation_id.clone());

        // Re-inline both externalizations to reproduce the pre-migration
        // encoding: corpus-sized collections rendered in place, and every
        // float carried inside its vector row.
        let mut document = legacy_inline_document(&mut state);
        let vectors =
            document["published"]["generations"][generation_id.as_digest().as_str()]["vectors"]
                .as_object_mut()
                .expect("vector map");
        for vector in vectors.values_mut() {
            vector["values"] = serde_json::json!([0.75_f32]);
        }
        let legacy_document = document.to_string();
        assert!(legacy_document.contains("\"values\""));
        assert!(legacy_document.contains("\"chunk_digest\""));
        DatabaseVectorGenerationStoreV1::open_legacy_migration(&database)
            .await
            .expect("schema");
        database
            .execute_write_engine(
                "install legacy inline vector fixture",
                "UPDATE semantic_vector_generation_state_v1
                 SET revision = revision + 1, state_json = ?1
                 WHERE singleton = 1",
                params![legacy_document],
            )
            .await
            .expect("install legacy fixture");
        assert_eq!(payload_row_count(&database).await, 0);

        let store = DatabaseVectorGenerationStoreV1::open(&database)
            .await
            .expect("open migrates the legacy document");
        assert_eq!(payload_row_count(&database).await, 1);
        let document = state_document(&database).await;
        assert!(
            !document.contains("\"values\""),
            "migration drops the inline floats from the state document"
        );
        let observed = store
            .active_generation()
            .await
            .expect("read active generation")
            .expect("active generation");
        assert_eq!(observed.generation_id(), &generation_id);
        assert_eq!(&observed, &expected, "migration preserves the generation");

        // Re-opening a migrated store is a no-op rather than a second rewrite.
        let revision_before = state_revision(&database).await;
        DatabaseVectorGenerationStoreV1::open(&database)
            .await
            .expect("reopen migrated store");
        assert_eq!(state_revision(&database).await, revision_before);
        assert_eq!(
            DatabaseVectorGenerationStoreV1::read_active_generation_for(
                &database,
                &embedding,
                &source,
                &source_manifest_digest,
            )
            .await
            .expect("bounded active read")
            .expect("compatible active generation")
            .vectors()
            .values()
            .next()
            .expect("vector")
            .values,
            vec![0.75_f32]
        );
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
        let store = DatabaseVectorGenerationStoreV1::open(&database)
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

    async fn state_revision(database: &Database) -> i64 {
        let mut rows = database
            .engine_conn()
            .query(
                "SELECT revision FROM semantic_vector_generation_state_v1 WHERE singleton = 1",
                (),
            )
            .await
            .expect("revision");
        let row = rows.next().await.expect("revision row").expect("row");
        row.get::<i64>(0).expect("revision")
    }
}
