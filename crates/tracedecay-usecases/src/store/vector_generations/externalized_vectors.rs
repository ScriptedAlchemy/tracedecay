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
