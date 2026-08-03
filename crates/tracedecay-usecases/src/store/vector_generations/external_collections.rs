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
