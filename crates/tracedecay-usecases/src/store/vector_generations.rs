//! Immutable semantic vector-generation storage.
//!
//! The deterministic state machine is the single in-memory authority over
//! generation lifecycle. All persistence lives in the embedded graph database
//! through [`graph_adapter`], whose verified publication makes each immutable
//! generation recoverable by exact identity. Retrieval activation remains the
//! committed configuration authority.
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

pub use tracedecay_domain::VectorGenerationIdV1;

use tracedecay_code_index::projection::{expected_publication_digest, verify_batch_receipt};
use tracedecay_semantic::projector::{
    PreparedVectorGenerationV1, ProjectedChunkVectorV1, SemanticProjectionErrorV1,
};

mod graph_adapter;
mod identity;
pub use graph_adapter::*;
pub(crate) use identity::generation_identity_digest;

const VECTOR_GENERATION_BUILD_DIGEST_DOMAIN: &str = "tracedecay.vector-generation-build.v1";
const VECTOR_COMMITTED_BATCH_DIGEST_DOMAIN: &str = "tracedecay.vector-committed-batch.v1";
const PHYSICAL_VECTOR_REUSE_DIGEST_DOMAIN: &str = "tracedecay.physical-vector-reuse.v1";
/// Bytes per sealed slice of a corpus-sized externalized collection, keeping
/// each sealed unit bounded no matter how large the collection is.
const VECTOR_STATE_SLICE_BYTES: usize = 32 * 1024;
/// Bounded optimistic-retry budget for watermark compare-and-swap loops.

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
    /// A pool with its own private key set, detached from the process-wide
    /// interner. Tests that count retained entries need this: the global pool
    /// is shared across every concurrently running test, so counts over it
    /// observe unrelated interns.
    #[cfg(test)]
    pub(crate) fn isolated() -> Self {
        Self {
            entries: Arc::new(Mutex::new(PhysicalVectorPoolStateV1::default())),
        }
    }

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

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct CommittedVectorBatchV1 {
    request_digest: ManifestDigest,
    prepared_digest: ManifestDigest,
    receipt: ProjectionBatchReceiptV1,
}

#[derive(Clone, Debug, Default, Serialize, PartialEq, Eq)]
struct PreparedBatchesV1(Vec<CommittedVectorBatchV1>);

impl<'de> Deserialize<'de> for PreparedBatchesV1 {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(untagged)]
        #[allow(clippy::large_enum_variant)]
        enum PersistedBatchV1 {
            Committed(CommittedVectorBatchV1),
            LegacyPrepared(PreparedVectorGenerationV1),
        }

        Vec::<PersistedBatchV1>::deserialize(deserializer)?
            .into_iter()
            .map(|batch| match batch {
                PersistedBatchV1::Committed(batch) => Ok(batch),
                PersistedBatchV1::LegacyPrepared(prepared) => {
                    let prepared_digest =
                        canonical_sha256(&(VECTOR_COMMITTED_BATCH_DIGEST_DOMAIN, &prepared))
                            .map_err(serde::de::Error::custom)?;
                    Ok(CommittedVectorBatchV1 {
                        request_digest: prepared.request.request_digest,
                        prepared_digest,
                        receipt: prepared.receipt,
                    })
                }
            })
            .collect::<Result<Vec<_>, _>>()
            .map(Self)
    }
}

impl std::ops::Deref for PreparedBatchesV1 {
    type Target = Vec<CommittedVectorBatchV1>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl std::ops::DerefMut for PreparedBatchesV1 {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
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
            || generation_identity_digest(&VectorGenerationPlanV1 {
                target_projection_key: self.projection_key.clone(),
                source_generation: self.source_generation.clone(),
                source_manifest_digest: self.source_manifest_digest.clone(),
                expected_chunk_ids: self.vectors.keys().cloned().collect::<Vec<_>>().into(),
                base_generation: self.base_generation.clone(),
            })
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

/// Why a planned or hydrated base generation cannot be used.
#[derive(Clone, Copy, Debug, Error, PartialEq, Eq)]
pub enum BaseGenerationIncompatibilityV1 {
    #[error("missing from published generations")]
    MissingPublished,
    #[error(
        "incompatible with the incremental request (source generation, projection key, or embedding key)"
    )]
    IdentityMismatch,
    #[error("missing from the verified snapshot")]
    MissingSnapshot,
}

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum VectorGenerationStoreErrorV1 {
    #[error("semantic vector graph operation was cancelled")]
    Cancelled,
    #[error("semantic vector graph operation exceeded its deadline")]
    DeadlineExceeded,
    #[error("semantic vector graph reset is required: {0}")]
    ResetRequired(String),
    #[error("semantic vector graph is corrupt: {0}")]
    Corrupt(String),
    #[error("semantic vector graph is unavailable: {0}")]
    Unavailable(String),
    #[error("semantic vector graph durability is uncertain: {0}")]
    DurabilityUncertain(String),
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
    #[error("base vector generation is {0}")]
    IncompatibleBaseGeneration(BaseGenerationIncompatibilityV1),
    #[error("reused chunk {0} has no matching immutable base vector")]
    MissingBaseVector(CodeSearchChunkId),
    #[error("applied chunk {0} has no matching vector output")]
    MissingAppliedVector(CodeSearchChunkId),
    #[error("vector generation membership is incomplete")]
    IncompleteGeneration,
    #[error("immutable vector generation identity already has different content")]
    ImmutableGenerationConflict,
    #[error("physical vector reuse identity already has different bytes")]
    PhysicalVectorConflict,
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
    #[serde(skip, default)]
    physical_vectors: BTreeMap<ManifestDigest, PhysicalVectorPayloadV1>,
    #[serde(default, with = "external_state::address_map")]
    physical_vector_bindings:
        BTreeMap<VectorGenerationIdV1, ExternalV1<BTreeMap<CodeSearchChunkId, ManifestDigest>>>,
}

impl PublishedStateV1 {
    fn immutable_graph_generation(
        generations: BTreeMap<VectorGenerationIdV1, PublishedVectorGenerationV1>,
    ) -> Self {
        Self {
            generations,
            physical_vectors: BTreeMap::new(),
            physical_vector_bindings: BTreeMap::new(),
        }
    }
}

/// Deterministic transition authority shared by the SQL store and native graph
/// record adapter.
#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VectorGenerationStateMachineV1 {
    staged: BTreeMap<VectorGenerationBuildIdV1, StagedVectorGenerationV1>,
    published: PublishedStateV1,
    #[serde(skip, default)]
    physical_vector_pool: PhysicalVectorBytePoolV1,
}

impl VectorGenerationStateMachineV1 {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn begin_generation(
        &mut self,
        plan: VectorGenerationPlanV1,
    ) -> Result<VectorGenerationBuildIdV1, VectorGenerationStoreErrorV1> {
        validate_plan(&plan)?;
        if let Some(base_id) = &plan.base_generation {
            self.published.generations.get(base_id).ok_or(
                VectorGenerationStoreErrorV1::IncompatibleBaseGeneration(
                    BaseGenerationIncompatibilityV1::MissingPublished,
                ),
            )?;
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
    /// Already-published generations are untouched.
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
    /// generation. This is the cancellation boundary
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
        let prepared_digest = canonical_sha256(&(VECTOR_COMMITTED_BATCH_DIGEST_DOMAIN, prepared))
            .map_err(storage_error)?;
        if let Some(existing) = current
            .batches
            .iter()
            .find(|batch| batch.request_digest == prepared.request.request_digest)
        {
            if existing.prepared_digest == prepared_digest {
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
        next.batches.push(CommittedVectorBatchV1 {
            request_digest: prepared.request.request_digest.clone(),
            prepared_digest,
            receipt: prepared.receipt.clone(),
        });
        let checkpoint = next.checkpoint.clone();
        self.staged.insert(build_id.clone(), next);
        Ok(checkpoint)
    }

    /// Validate and atomically publish a fully staged immutable generation.
    /// Partial generations remain in `staged` and are never readable.
    pub fn publish_generation(
        &mut self,
        build_id: &VectorGenerationBuildIdV1,
    ) -> Result<VectorGenerationPublicationV1, VectorGenerationStoreErrorV1> {
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

        let manifest_digest = generation_identity_digest(&staged.plan)?;
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
        self.staged.remove(build_id);
        Ok(VectorGenerationPublicationV1 {
            generation_id,
            manifest_digest,
            checkpoint,
        })
    }

    /// The checkpoint of one staged build, which is how a resumed run learns
    /// how many of its batches are already durable.
    pub fn staged_checkpoint(
        &self,
        build_id: &VectorGenerationBuildIdV1,
    ) -> Option<&VectorProjectionCheckpointV1> {
        self.staged.get(build_id).map(|staged| &staged.checkpoint)
    }

    pub fn generation(
        &self,
        generation_id: &VectorGenerationIdV1,
    ) -> Option<&PublishedVectorGenerationV1> {
        self.published.generations.get(generation_id)
    }

    /// Seal every externalized collection, then serialize the state document.
    ///
    /// The document stores only content addresses. Serializing an unsealed
    /// slot fails closed rather than writing a fabricated address. Physical
    /// vector bytes are persisted beside the document because the row map
    /// elides them.
    pub fn persist_sealed(&mut self) -> Result<Vec<u8>, VectorGenerationStoreErrorV1> {
        let collections = seal_external_state(self, &BTreeSet::new())?;
        let document = serde_json::to_vec(self).map_err(storage_error)?;
        serde_json::to_vec(&PersistedSealedStateV1 {
            document,
            collections,
            physical_vectors: self.published.physical_vectors.clone(),
        })
        .map_err(storage_error)
    }

    /// Reload a document persisted by [`Self::persist_sealed`].
    pub fn reopen_sealed(bytes: &[u8]) -> Result<Self, VectorGenerationStoreErrorV1> {
        let persisted: PersistedSealedStateV1 =
            serde_json::from_slice(bytes).map_err(storage_error)?;
        let mut state: Self = serde_json::from_slice(&persisted.document).map_err(storage_error)?;
        state.published.physical_vectors = persisted.physical_vectors;
        state.visit_external_slots(&mut |slot| {
            let Some(address) = slot.address().cloned() else {
                return Ok(());
            };
            let slices = persisted.collections.get(&address).ok_or_else(|| {
                VectorGenerationStoreErrorV1::Storage(format!(
                    "externalized state collection {address} is missing"
                ))
            })?;
            slot.fill(slices)
        })?;
        hydrate_elided_vector_values(&mut state)?;
        Ok(state)
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
}

impl VectorGenerationStateMachineV1 {
    /// Rebuild the derived physical-byte index for every published generation.
    ///
    /// The generation map is moved aside rather than cloned: interning only
    /// touches `physical_vectors` and `physical_vector_bindings`, so a deep
    /// copy of every published generation — the whole float corpus, once per
    /// load — bought nothing but the borrow.
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

/// Seal a hand-built fixture so its document can be serialized.
///
/// Production persist goes through [`VectorGenerationStateMachineV1::persist_sealed`];
/// fixtures that build state directly go through here instead.
#[cfg(test)]
fn seal_test_state(
    state: &mut VectorGenerationStateMachineV1,
) -> BTreeMap<ContentDigest, Vec<Vec<u8>>> {
    seal_external_state(state, &BTreeSet::new()).expect("seal externalized state")
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

impl VectorGenerationStateMachineV1 {
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

/// Address-only state document plus the sealed slices and physical bytes
/// needed to reopen it.
#[derive(Serialize, Deserialize)]
struct PersistedSealedStateV1 {
    document: Vec<u8>,
    collections: BTreeMap<ContentDigest, Vec<Vec<u8>>>,
    physical_vectors: BTreeMap<ManifestDigest, PhysicalVectorPayloadV1>,
}

fn hydrate_elided_vector_values(
    state: &mut VectorGenerationStateMachineV1,
) -> Result<(), VectorGenerationStoreErrorV1> {
    for (generation_id, generation) in state.published.generations.iter_mut() {
        let Some(bindings) = state.published.physical_vector_bindings.get(generation_id) else {
            continue;
        };
        for (chunk_id, vector) in generation.vectors.elided_mut().iter_mut() {
            let Some(physical_id) = bindings.get(chunk_id) else {
                continue;
            };
            let Some(payload) = state.published.physical_vectors.get(physical_id) else {
                return Err(VectorGenerationStoreErrorV1::Storage(format!(
                    "physical vector payload for {chunk_id} is missing after reopen"
                )));
            };
            vector.values = payload.values.0.to_vec();
        }
    }
    Ok(())
}

/// Seal every externalized collection and collect the slices to write.
///
/// A slot whose address is already durable is left alone, so a mutation
/// re-encodes only what it actually changed: committing one batch writes that
/// batch's slices, not the corpus. Content addressing then makes publication
/// free — the staged collections and the published ones they become hash to
/// the same addresses, which are durable by then.
fn seal_external_state(
    state: &mut VectorGenerationStateMachineV1,
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

fn storage_error(error: impl std::fmt::Display) -> VectorGenerationStoreErrorV1 {
    VectorGenerationStoreErrorV1::Storage(error.to_string())
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
    let base = published.generations.get(base_id).ok_or(
        VectorGenerationStoreErrorV1::IncompatibleBaseGeneration(
            BaseGenerationIncompatibilityV1::MissingPublished,
        ),
    )?;
    if prepared.request.changes.from_generation.as_ref() != Some(base.source_generation())
        || prepared.request.previous_projection_key.as_ref() != Some(base.projection_key())
        || (prepared.request.target_projection_key == *base.projection_key()
            && prepared.embedding_key != *base.embedding_key())
    {
        return Err(VectorGenerationStoreErrorV1::IncompatibleBaseGeneration(
            BaseGenerationIncompatibilityV1::IdentityMismatch,
        ));
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
pub type FakeVectorGenerationStoreV1 = VectorGenerationStateMachineV1;

#[cfg(test)]
mod tests {
    use super::*;
    use tracedecay_domain::{
        ChangedCodeChunkSetV1, ChangedCodeChunkV1, ChunkerRevision, EmbeddingDeviceClassV1,
        EmbeddingMetricV1, EmbeddingNormalizationV1, EmbeddingPoolingV1, EmbeddingPrecisionV1,
        EmbeddingProjectionKeyV1, EmbeddingTruncationSideV1, PrivacyDomainId,
        ProjectionBatchRequestV1, ProjectionReplayReasonV1,
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
            inference_batch_size: 8,
            inference_batch_bytes: 16 * 1024,
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
        let manifest_digest = generation_identity_digest(&plan).expect("manifest digest");
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

    /// A batch reusing rows from a generation the plan's base does not own is
    /// rejected, while a per-batch changed-set digest that differs from the
    /// plan's whole-corpus manifest digest is accepted and rebound: batch-by-
    /// batch commits split one request into groups whose changed-set digests
    /// are legitimately their own, and generation identity is enforced at
    /// publication over the rebound rows.
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
            Err(VectorGenerationStoreErrorV1::IncompatibleBaseGeneration(
                BaseGenerationIncompatibilityV1::IdentityMismatch,
            ))
        );

        let base_source = id("code-generation.base");
        let compatible = reused_prepared(
            &embedding,
            &base_source,
            &target_source,
            &chunk_id,
            &chunk_digest,
        );
        let plan_manifest = manifest_digest('f');
        assert_ne!(
            plan_manifest, compatible.request.changes.manifest_digest,
            "the probe must exercise a per-batch digest that differs from the plan's"
        );
        let split_build = store
            .begin_generation(VectorGenerationPlanV1 {
                target_projection_key: embedding.projection_key().clone(),
                source_generation: target_source,
                source_manifest_digest: plan_manifest.clone(),
                expected_chunk_ids: vec![chunk_id.clone()].into(),
                base_generation: Some(base_id),
            })
            .expect("split-batch build");
        store
            .commit_batch(&split_build, None, compatible.clone())
            .expect("a per-batch changed-set digest commits against the plan");
        let durable_chunks = graph_adapter::transitions::semantic_stage_chunk_receipts(
            &store,
            &split_build,
            &compatible,
        )
        .expect("reused batch binds lineage without a local vector effect");
        assert_eq!(durable_chunks.len(), 1);
        assert_eq!(
            durable_chunks[0].operation,
            tracedecay_store::SemanticVectorStageChunkOperation::Reuse
        );
        assert_eq!(durable_chunks[0].output_digest, None);
        assert_eq!(
            durable_chunks[0].chunk_digest.as_str(),
            chunk_digest.as_str()
        );
        let staged = store.staged.get(&split_build).expect("staged build");
        assert_eq!(
            staged
                .vectors
                .get(&chunk_id)
                .expect("rebound reused row")
                .source_manifest_digest,
            plan_manifest,
            "committed rows are rebound to the plan's manifest digest"
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
        let build = store
            .begin_generation(VectorGenerationPlanV1 {
                target_projection_key: embedding.projection_key().clone(),
                source_generation: target_source,
                source_manifest_digest: prepared.request.changes.manifest_digest.clone(),
                expected_chunk_ids: vec![chunk_id.clone()].into(),
                base_generation: Some(base_id.clone()),
            })
            .expect("staged build");
        store
            .commit_batch(&build, None, prepared.clone())
            .expect("complete reused batch");
        let encoded = graph_adapter::encode_generation_batch_delta(&store, &build, &prepared, 1)
            .expect("receipt-only reused encode");
        assert!(
            encoded.entities.iter().all(|entity| {
                !entity.labels.iter().any(|label| {
                    label.as_str()
                        == tracedecay_graph_db::semantic_vector_native::GENERATION_VECTOR_LABEL
                })
            }),
            "ordinary reuse must not copy base vectors as generation entities"
        );
        assert!(
            encoded.entities.iter().any(|entity| {
                entity.labels.iter().any(|label| {
                    label.as_str()
                        == tracedecay_graph_db::semantic_vector_native::GENERATION_RECEIPT_LABEL
                })
            }),
            "ordinary reuse must still persist the generation receipt"
        );
        let publication = store
            .publish_generation(&build)
            .expect("immutable publication");

        assert!(!store.staged.contains_key(&build));
        let published = store
            .generation(&publication.generation_id)
            .expect("published generation");
        published
            .validate_persisted()
            .expect("published generation is complete");
        let reused = published
            .vectors()
            .get(&chunk_id)
            .expect("reused vector remains retrievable");
        assert_eq!(reused.values, vec![0.25]);
        assert_eq!(reused.chunk_digest, chunk_digest);
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
        let first = generation_identity_digest(&plan).expect("identity from admitted plan");
        let second = generation_identity_digest(&plan)
            .expect("identity remains independent from vector execution output");

        assert_eq!(first, second);

        let checkpoint = VectorProjectionCheckpointV1 {
            target_projection_key: plan.target_projection_key.clone(),
            source_generation: plan.source_generation.clone(),
            source_manifest_digest: plan.source_manifest_digest.clone(),
            completed_batches: 1,
            last_request_digest: Some(manifest_digest('a')),
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
    fn persisted_state_rejects_tombstone_vector_overlap() {
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
        generation.manifest_digest = generation_identity_digest(&VectorGenerationPlanV1 {
            target_projection_key: generation.projection_key.clone(),
            source_generation: generation.source_generation.clone(),
            source_manifest_digest: generation.source_manifest_digest.clone(),
            expected_chunk_ids: vec![].into(),
            base_generation: None,
        })
        .expect("tombstone generation manifest");
        generation.generation_id = VectorGenerationIdV1::new(generation.manifest_digest.clone());
        assert!(generation.validate_persisted().is_ok());
        let mut sealed_state = FakeVectorGenerationStoreV1::new();
        sealed_state
            .published
            .generations
            .insert(generation.generation_id().clone(), generation);
        let sealed = seal_test_state(&mut sealed_state);
        let persisted = sealed_state
            .published
            .generations
            .values()
            .next()
            .expect("sealed deletion-to-empty generation");
        let encoded = serde_json::to_string(persisted).expect("serialize deletion generation");
        let mut reloaded: PublishedVectorGenerationV1 =
            serde_json::from_str(&encoded).expect("deserialize deletion generation");
        reloaded
            .visit_external_slots(&mut |slot| {
                let Some(address) = slot.address().cloned() else {
                    return Ok(());
                };
                slot.fill(sealed.get(&address).expect("sealed deletion collection"))
            })
            .expect("hydrate deletion generation");
        reloaded
            .validate_persisted()
            .expect("deletion evidence does not enter eligible membership identity");
    }

    #[test]
    fn initially_empty_generation_publishes_and_reloads_without_inference() {
        let embedding_key = admitted_embedding();
        let source_generation = id::<CodeGenerationId>("code-generation.empty");
        let mut changes = ChangedCodeChunkSetV1 {
            from_generation: None,
            to_generation: source_generation.clone(),
            manifest_digest: manifest_digest('0'),
            added_or_changed: Vec::new(),
            deleted: Vec::new(),
            reused: Vec::new(),
        };
        changes.manifest_digest = changes.compute_digest().expect("empty source digest");
        let mut request = ProjectionBatchRequestV1 {
            request_digest: manifest_digest('0'),
            changes,
            previous_projection_key: None,
            target_projection_key: embedding_key.projection_key().clone(),
            replay_reason: ProjectionReplayReasonV1::FullRebuildIncompatible,
        };
        request.request_digest =
            tracedecay_code_index::projection::expected_request_digest(&request)
                .expect("empty request digest");
        let receipt = tracedecay_code_index::projection::build_batch_receipt(&request, &[])
            .expect("empty batch receipt");
        let plan = VectorGenerationPlanV1 {
            target_projection_key: request.target_projection_key.clone(),
            source_generation,
            source_manifest_digest: request.changes.manifest_digest.clone(),
            expected_chunk_ids: Vec::new().into(),
            base_generation: None,
        };
        let mut store = FakeVectorGenerationStoreV1::new();
        let build = store
            .begin_generation(plan)
            .expect("begin empty generation");
        store
            .commit_batch(
                &build,
                None,
                PreparedVectorGenerationV1 {
                    embedding_key,
                    request,
                    receipt,
                    vectors: Vec::new(),
                    tombstones: Vec::new(),
                },
            )
            .expect("commit canonical empty control batch");
        let publication = store
            .publish_generation(&build)
            .expect("publish empty generation");
        let sealed = seal_test_state(&mut store);
        let persisted = store
            .published
            .generations
            .get(&publication.generation_id)
            .expect("persisted empty generation");
        let encoded = serde_json::to_string(persisted).expect("serialize empty generation");
        let mut reloaded: PublishedVectorGenerationV1 =
            serde_json::from_str(&encoded).expect("deserialize empty generation");
        reloaded
            .visit_external_slots(&mut |slot| {
                let Some(address) = slot.address().cloned() else {
                    return Ok(());
                };
                slot.fill(sealed.get(&address).expect("sealed empty collection"))
            })
            .expect("hydrate empty generation");
        reloaded
            .validate_persisted()
            .expect("empty immutable generation remains valid after restart");
        assert!(reloaded.vectors().is_empty());
        assert!(reloaded.tombstones().is_empty());
    }

    #[test]
    fn persisted_generation_identity_is_plan_bound_and_survives_output_rewrite() {
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
        let original_generation_id = generation.generation_id.clone();
        let original_manifest_digest = generation.manifest_digest.clone();

        // Content evidence still lives in the row and receipt validators: a
        // values rewrite without a recomputed output digest is rejected.
        let mut raw_tamper = generation.clone();
        raw_tamper
            .vectors
            .values_mut()
            .next()
            .expect("fixture vector")
            .values = vec![0.75];
        assert!(
            raw_tamper.validate_persisted().is_err(),
            "vector values that no longer match their output digest must be rejected"
        );

        // A recomputed output digest that is not carried through the batch
        // receipts breaks the receipt evidence chain.
        let mut receipt_tamper = generation.clone();
        let vector = receipt_tamper
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
        .expect("rewritten vector digest");
        assert!(
            receipt_tamper.validate_persisted().is_err(),
            "an output rewrite that skips the batch receipts must be rejected"
        );

        // A fully self-consistent output rewrite (values, output digest,
        // receipts, checkpoint) is fresh execution evidence: the plan-bound
        // generation identity does not move because the projection key,
        // source corpus, and chunk membership are unchanged.
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
        .expect("rewritten vector digest");
        generation.receipts[0].receipts[0].output_digest = Some(vector.output_digest.clone());
        generation.receipts[0].publication_digest =
            expected_publication_digest(&generation.receipts[0])
                .expect("rewritten publication digest");
        generation.checkpoint.last_publication_digest =
            Some(generation.receipts[0].publication_digest.clone());

        generation
            .validate_persisted()
            .expect("self-consistent output rewrite keeps the plan-bound identity valid");
        assert_eq!(generation.generation_id, original_generation_id);
        assert_eq!(generation.manifest_digest, original_manifest_digest);
    }

    /// Retiring a generation must release the interner keys it introduced, or
    /// the process-global pool grows for the lifetime of the daemon. The probe
    /// runs over an isolated pool: the release semantics are identical, but
    /// entry counts over the process-global singleton would race every other
    /// concurrently running test that interns a vector.
    #[test]
    fn physical_byte_pool_releases_keys_for_retired_generations() {
        let pool = PhysicalVectorBytePoolV1::isolated();
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
}
