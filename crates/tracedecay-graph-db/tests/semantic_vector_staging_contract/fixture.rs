use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use rusqlite::Savepoint;
use tempfile::TempDir;
use tracedecay_domain::{
    AdmittedEmbeddingProjectionKeyV1, ChunkerRevision, CodeChunkProjectionReceiptV1,
    CodeGenerationId, CodeSearchChunkId, ContentDigest, EmbeddingDeviceClassV1, EmbeddingMetricV1,
    EmbeddingNormalizationV1, EmbeddingPoolingV1, EmbeddingPrecisionV1, EmbeddingProjectionKeyV1,
    EmbeddingTruncationSideV1, ManifestDigest, PrivacyDomainId, ProjectionBatchReceiptV1,
    ProjectionOperationV1, ProjectionOutcomeV1, RepositoryId, UtcMicros, VectorGenerationIdV1,
    WorktreeId, canonical_sha256, projection_batch_publication_digest,
    semantic_vector_output_digest,
};
use tracedecay_graph_db::{
    GraphDbError, GraphDbRegistration, GraphEntity, GraphEntityId, GraphEntityRef,
    GraphGenerationId, GraphGenerationManifest, GraphIdempotencyKey, GraphLabel, GraphMutation,
    GraphNamespace, GraphProjectionId, GraphProjectionIdentity, GraphProperty, GraphPropertyName,
    GraphRelation, GraphRelationId, GraphRelationKind, GraphVector, GraphWatermark,
    GraphWriteBatch, SourceGeneration, VectorMetric, VerifiedGraphCommit, semantic_vector_native,
};
use tracedecay_rusqlite_runtime::{
    ExistingWriterLocator, PersistentWriter, StorageOperationExecutor,
    exact_sql::{ExactSqlError, ExactSqlHandle, ExactSqlWriteAuthority, ExactSqlWriteIntent},
    reader::{ExistingReaderLocator, ReaderPool, ReaderQueryExecutor},
    repository::{
        GRAPH_PUBLICATION_SCHEMA_V1, SEMANTIC_VECTOR_STAGING_SCHEMA,
        SemanticVectorStagingExactSqlStorage,
    },
};
use tracedecay_store::{
    AdmissionConfigV1, CodeShardScopeV1, GraphDependencyGenerationIdentityV1, GraphNamespaceV1,
    GraphProjectionIdV1, GraphProjectionIdentityV1, GraphPublicationIdempotencyKeyV1,
    GraphPublicationInputDigestV1, GraphPublicationKeyV1, GraphPublicationOperationContextV1,
    GraphPublicationStoreV1, RuntimeCancellationIdV1, RuntimeCancellationIdentityV1,
    RuntimeDeadlineIdV1, RuntimeDeadlineV1, RuntimeInterruptionV1, RuntimeRequestControlV1,
    RuntimeRequestProbeV1, SemanticEmbeddingProjectionDigestV1, SemanticModelArtifactDigestV1,
    SemanticPrivacyDomainDigestV1, SemanticProjectionManifestDigestV1,
    SemanticVectorBatchInputDigest, SemanticVectorBuildId, SemanticVectorCheckpointDigest,
    SemanticVectorChunkDigest, SemanticVectorChunkId, SemanticVectorChunkManifestMember,
    SemanticVectorOutputDigest, SemanticVectorReconstructionRecipe,
    SemanticVectorSourceDependencyV1, SemanticVectorSourceGenerationId,
    SemanticVectorSourceManifestDigest, SemanticVectorStageBatchKey,
    SemanticVectorStageBatchReceipt, SemanticVectorStageChunkOperation,
    SemanticVectorStageChunkReceipt, SemanticVectorStagePlan, SemanticVectorStagingStore,
    SemanticVectorWriterFence, StoreRuntimeBindingV1, StoreShardIdV1, VerifiedStoreLocatorV1,
    canonical_store_locator_digest, semantic_vector_chunk_manifest_digest,
};

use super::graph_support::{RegisteredGraph, TestCancellation, registration};

struct NoWrites;

impl StorageOperationExecutor for NoWrites {
    fn execute(
        &mut self,
        _savepoint: &Savepoint<'_>,
        _payload: &tracedecay_store::RepositoryWritePayloadV1,
    ) -> rusqlite::Result<()> {
        Ok(())
    }
}

#[derive(Clone)]
struct NoReads;

impl ReaderQueryExecutor for NoReads {
    fn execute_read(
        &mut self,
        _snapshot: &rusqlite::Transaction<'_>,
        _request: &tracedecay_store::RuntimeReadRequestV1,
    ) -> Result<tracedecay_store::RuntimeReadOutcomeV1, tracedecay_store::StorageRuntimeErrorV1>
    {
        unreachable!("exact SQL bypasses product reads")
    }
}

struct AlwaysAuthorized;

impl ExactSqlWriteAuthority for AlwaysAuthorized {
    fn verify(&self, _intent: ExactSqlWriteIntent) -> Result<(), ExactSqlError> {
        Ok(())
    }
}

struct RequestProbe {
    cancellation: RuntimeCancellationIdentityV1,
    deadline: RuntimeDeadlineV1,
}

impl RuntimeRequestProbeV1 for RequestProbe {
    fn cancellation_identity(&self) -> &RuntimeCancellationIdentityV1 {
        &self.cancellation
    }

    fn deadline_identity(&self) -> &RuntimeDeadlineV1 {
        &self.deadline
    }

    fn interruption(&self) -> Option<RuntimeInterruptionV1> {
        None
    }

    fn try_begin_commit(&self) -> bool {
        true
    }
}

pub struct ContractFixture {
    root: TempDir,
    pub graph: RegisteredGraph,
    _writer: PersistentWriter,
    _readers: ReaderPool<NoReads>,
    handle: ExactSqlHandle,
    source_dependency: SemanticVectorSourceDependencyV1,
    embedding: AdmittedEmbeddingProjectionKeyV1,
}

#[derive(Clone, Copy)]
pub enum NativeMismatch {
    ChunkId,
    ChunkDigest,
    Operation,
    OutputDigest,
    Metric,
    VectorValues,
    ProjectionReceipt,
    PageSourceManifest,
    SameProfileProjection,
    ExtraEffect,
    MissingEffect,
}

pub struct PageBatchSpec<'a> {
    pub name: &'a str,
    pub ordinal: u64,
    pub start: u64,
    pub count: u64,
    pub expected_checkpoint: SemanticVectorCheckpointDigest,
    pub next_checkpoint: SemanticVectorCheckpointDigest,
    pub marker: f32,
}

impl ContractFixture {
    pub fn new() -> Self {
        Self::new_with_embedding_dimensions(3)
    }

    pub fn new_with_embedding_dimensions(dimensions: u32) -> Self {
        let root = TempDir::new().unwrap();
        let graph = RegisteredGraph::new_mounted(root.path()).unwrap();
        let path = root.path().join("semantic-vector-authority.sqlite3");
        drop(rusqlite::Connection::open(&path).unwrap());
        let path = path.canonicalize().unwrap();
        let locator = VerifiedStoreLocatorV1::new(
            graph.binding.shard_id.clone(),
            graph.binding.incarnation,
            canonical_store_locator_digest(&path).unwrap(),
        );
        let writer = PersistentWriter::start(
            ExistingWriterLocator::new(graph.binding.clone(), locator.clone(), path.clone())
                .unwrap(),
            AdmissionConfigV1::default(),
            NoWrites,
        )
        .unwrap();
        let readers = ReaderPool::start(
            ExistingReaderLocator::new(graph.binding.clone(), locator, path).unwrap(),
            AdmissionConfigV1::default().readers,
            NoReads,
        )
        .unwrap();
        let base = ExactSqlHandle::attach(&writer, &readers).unwrap();
        base.execute_batch(GRAPH_PUBLICATION_SCHEMA_V1.to_owned())
            .unwrap();
        base.execute_batch(SEMANTIC_VECTOR_STAGING_SCHEMA.to_owned())
            .unwrap();
        let handle = base
            .with_write_authority(Arc::new(AlwaysAuthorized))
            .unwrap();
        let source_projection = GraphProjectionIdentityV1 {
            shard_id: graph.binding.shard_id.clone(),
            namespace: GraphNamespaceV1::new("semantic-source").unwrap(),
            projection: GraphProjectionIdV1::new("code").unwrap(),
        };
        let source_dependency = SemanticVectorSourceDependencyV1 {
            generation: GraphDependencyGenerationIdentityV1::new(
                source_projection,
                tracedecay_store::GraphGenerationIdV1::new("source-generation").unwrap(),
            ),
            idempotency_key: GraphPublicationIdempotencyKeyV1::new("source-publication").unwrap(),
        };
        let fixture = Self {
            root,
            graph,
            _writer: writer,
            _readers: readers,
            handle,
            source_dependency,
            embedding: admitted_embedding_with_dimensions(dimensions),
        };
        fixture.seed_source_generation();
        fixture
    }

    pub fn authority(&self) -> SemanticVectorStagingExactSqlStorage {
        SemanticVectorStagingExactSqlStorage::from_authorized_handle(self.handle.clone()).unwrap()
    }

    pub fn registration(&self) -> GraphDbRegistration {
        registration(self.graph.binding.clone(), self.root.path())
    }

    pub fn plan(
        &self,
        name: &str,
        semantic_generation: &str,
        prior: Option<tracedecay_store::GraphVerifiedHeadV1>,
    ) -> SemanticVectorStagePlan {
        let projection = GraphProjectionIdentityV1 {
            shard_id: self.graph.binding.shard_id.clone(),
            namespace: GraphNamespaceV1::new("semantic-vector").unwrap(),
            projection: GraphProjectionIdV1::new("chunks").unwrap(),
        };
        let chunk_id = SemanticVectorChunkId::new(format!("chunk.{name}")).unwrap();
        let chunk_digest = digest::<SemanticVectorChunkDigest>(digest_byte(name, 0));
        let manifest =
            semantic_vector_chunk_manifest_digest(&[SemanticVectorChunkManifestMember {
                chunk_id,
                chunk_digest,
                operation: SemanticVectorStageChunkOperation::Embed,
            }])
            .unwrap();
        SemanticVectorStagePlan::new(
            projection.clone(),
            SemanticVectorBuildId::new(format!("build.{name}")).unwrap(),
            VectorGenerationIdV1::new(
                canonical_sha256(&("semantic-vector-staging-contract", semantic_generation))
                    .unwrap(),
            ),
            None,
            GraphPublicationKeyV1::new(
                projection,
                tracedecay_store::GraphGenerationIdV1::new(format!("generation.{name}")).unwrap(),
                GraphPublicationIdempotencyKeyV1::new(format!("publication.{name}")).unwrap(),
            ),
            source_scope(&self.graph.binding),
            tracedecay_store::SemanticVectorCodeScopeHash::new("a".repeat(64)).unwrap(),
            SemanticVectorSourceGenerationId::new("source-code-generation").unwrap(),
            self.source_dependency.clone(),
            SemanticVectorReconstructionRecipe {
                source_manifest_digest: digest::<SemanticVectorSourceManifestDigest>('1'),
                embedding_projection_digest: SemanticEmbeddingProjectionDigestV1::new(
                    canonical_sha256(&self.embedding).unwrap().as_str(),
                )
                .unwrap(),
                embedding_dimension: u16::try_from(self.embedding.embedding_key().dimensions)
                    .expect("fixture embedding dimension fits recipe"),
                model_artifact_digest: SemanticModelArtifactDigestV1::new(
                    self.embedding
                        .embedding_key()
                        .model_artifact_digest
                        .as_str(),
                )
                .unwrap(),
                projection_manifest_digest: SemanticProjectionManifestDigestV1::new(
                    self.embedding.projection_key().profile_digest.as_str(),
                )
                .unwrap(),
                privacy_domain_digest: SemanticPrivacyDomainDigestV1::new(
                    canonical_sha256(self.embedding.privacy_domain())
                        .unwrap()
                        .as_str(),
                )
                .unwrap(),
                privacy_key_epoch: self.embedding.privacy_key_epoch(),
                expected_chunk_manifest_digest: manifest,
            },
            1,
            prior,
            digest::<SemanticVectorCheckpointDigest>('9'),
            SemanticVectorWriterFence {
                binding: self.graph.binding.clone(),
            },
        )
        .unwrap()
    }

    pub fn batch_and_receipt(
        &self,
        plan: &SemanticVectorStagePlan,
        marker: f32,
    ) -> (GraphWriteBatch, SemanticVectorStageBatchReceipt) {
        self.batch_and_receipt_with_mismatch(plan, marker, None)
    }

    pub fn batch_and_receipt_with_mismatch(
        &self,
        plan: &SemanticVectorStagePlan,
        marker: f32,
        mismatch: Option<NativeMismatch>,
    ) -> (GraphWriteBatch, SemanticVectorStageBatchReceipt) {
        let name = plan
            .key
            .build_id
            .as_str()
            .strip_prefix("build.")
            .expect("fixture build id has canonical prefix");
        let chunk_id = SemanticVectorChunkId::new(format!("chunk.{name}")).unwrap();
        let chunk_digest = digest::<SemanticVectorChunkDigest>(digest_byte(name, 0));
        let values = vec![marker, marker + 1.0, marker + 2.0];
        let output_digest = SemanticVectorOutputDigest::new(
            semantic_vector_output_digest(
                self.embedding.projection_key(),
                &CodeSearchChunkId::try_from(chunk_id.as_str().to_owned()).unwrap(),
                &ContentDigest::try_from(chunk_digest.as_str().to_owned()).unwrap(),
                &values,
            )
            .unwrap()
            .as_str(),
        )
        .unwrap();
        let mut mutations = canonical_native_mutations(
            plan,
            &self.embedding,
            &chunk_id,
            &chunk_digest,
            &output_digest,
            values,
        );
        if let Some(mismatch) = mismatch {
            mutate_native_effect(&mut mutations, mismatch);
        }
        let mut batch = GraphWriteBatch::new(
            GraphNamespace::new(plan.key.projection.namespace.as_str()).unwrap(),
            GraphProjectionId::new(plan.key.projection.projection.as_str()).unwrap(),
            SourceGeneration::new(plan.source_generation.as_str()).unwrap(),
            GraphWatermark::new(format!("watermark.{}", plan.key.build_id.as_str())).unwrap(),
            mutations,
            Arc::new(TestCancellation),
        )
        .unwrap();
        let output = batch.semantic_vector_output_digest().unwrap();
        let receipt = SemanticVectorStageBatchReceipt::new(
            SemanticVectorStageBatchKey {
                stage: plan.key.clone(),
                ordinal: 0,
            },
            plan.initial_checkpoint_digest.clone(),
            digest::<SemanticVectorBatchInputDigest>('6'),
            output,
            digest::<SemanticVectorCheckpointDigest>('7'),
            vec![SemanticVectorStageChunkReceipt {
                effect_ordinal: 0,
                chunk_id,
                chunk_digest,
                operation: SemanticVectorStageChunkOperation::Embed,
                output_digest: Some(output_digest),
            }],
        )
        .unwrap();
        (batch, receipt)
    }

    pub fn begin_and_append(
        &self,
        authority: &mut SemanticVectorStagingExactSqlStorage,
        plan: &SemanticVectorStagePlan,
        receipt: &SemanticVectorStageBatchReceipt,
        suffix: &str,
    ) {
        with_context(&format!("{suffix}.begin"), |context| {
            self.graph
                .registry
                .begin_verified_generation(self.registration(), authority, context, plan)
                .unwrap();
        });
        with_context(&format!("{suffix}.append"), |context| {
            authority
                .append_stage_batch(receipt, &plan.writer_fence, context)
                .unwrap();
        });
    }

    pub fn apply(
        &self,
        authority: &mut SemanticVectorStagingExactSqlStorage,
        receipt: &SemanticVectorStageBatchReceipt,
        batch: GraphWriteBatch,
        suffix: &str,
    ) -> Result<(), GraphDbError> {
        with_context(suffix, |context| {
            self.graph
                .registry
                .apply_verified_generation_batch(
                    self.registration(),
                    authority,
                    context,
                    &receipt.key,
                    &receipt.receipt_digest,
                    batch,
                )
                .map(|_| ())
        })
    }

    pub fn settle_batch(
        &self,
        authority: &mut SemanticVectorStagingExactSqlStorage,
        receipt: &SemanticVectorStageBatchReceipt,
        suffix: &str,
    ) {
        with_context(suffix, |context| {
            self.graph
                .registry
                .settle_verified_generation_batch(
                    self.registration(),
                    authority,
                    context,
                    &receipt.key,
                    &receipt.receipt_digest,
                )
                .unwrap();
        });
    }

    pub fn ready(
        &self,
        authority: &mut SemanticVectorStagingExactSqlStorage,
        plan: &SemanticVectorStagePlan,
        suffix: &str,
    ) {
        self.try_ready(authority, plan, suffix).unwrap();
    }

    pub fn try_ready(
        &self,
        authority: &mut SemanticVectorStagingExactSqlStorage,
        plan: &SemanticVectorStagePlan,
        suffix: &str,
    ) -> Result<(), GraphDbError> {
        with_context(suffix, |context| {
            match self.graph.registry.prepare_publication_from_staged_native(
                self.registration(),
                authority,
                context,
                &plan.key,
            )? {
                tracedecay_store::SemanticVectorStagePublicationPrepareOutcome::ReadyToPublish(
                    _,
                )
                | tracedecay_store::SemanticVectorStagePublicationPrepareOutcome::ExactReplay(_) => {
                    Ok(())
                }
                other => Err(GraphDbError::invalid(format!(
                    "semantic vector stage was not ready to publish: {other:?}"
                ))),
            }
        })
    }

    pub fn publish(
        &self,
        authority: &mut SemanticVectorStagingExactSqlStorage,
        plan: &SemanticVectorStagePlan,
        suffix: &str,
    ) -> VerifiedGraphCommit {
        with_context(suffix, |context| {
            self.graph.registry.publish_ready_generation(
                self.registration(),
                authority,
                context,
                &plan.key,
            )
        })
        .unwrap()
    }

    pub fn plan_with_chunk_count(
        &self,
        name: &str,
        semantic_generation: &str,
        chunk_count: u64,
    ) -> SemanticVectorStagePlan {
        let projection = GraphProjectionIdentityV1 {
            shard_id: self.graph.binding.shard_id.clone(),
            namespace: GraphNamespaceV1::new("semantic-vector").unwrap(),
            projection: GraphProjectionIdV1::new("chunks").unwrap(),
        };
        let members = (0..chunk_count)
            .map(|index| SemanticVectorChunkManifestMember {
                chunk_id: page_chunk_id(name, index),
                chunk_digest: unique_digest(index),
                operation: SemanticVectorStageChunkOperation::Embed,
            })
            .collect::<Vec<_>>();
        let manifest = semantic_vector_chunk_manifest_digest(&members).unwrap();
        SemanticVectorStagePlan::new(
            projection.clone(),
            SemanticVectorBuildId::new(format!("build.{name}")).unwrap(),
            VectorGenerationIdV1::new(
                canonical_sha256(&("semantic-vector-staging-contract", semantic_generation))
                    .unwrap(),
            ),
            None,
            GraphPublicationKeyV1::new(
                projection,
                tracedecay_store::GraphGenerationIdV1::new(format!("generation.{name}")).unwrap(),
                GraphPublicationIdempotencyKeyV1::new(format!("publication.{name}")).unwrap(),
            ),
            source_scope(&self.graph.binding),
            tracedecay_store::SemanticVectorCodeScopeHash::new("a".repeat(64)).unwrap(),
            SemanticVectorSourceGenerationId::new("source-code-generation").unwrap(),
            self.source_dependency.clone(),
            SemanticVectorReconstructionRecipe {
                source_manifest_digest: digest::<SemanticVectorSourceManifestDigest>('1'),
                embedding_projection_digest: SemanticEmbeddingProjectionDigestV1::new(
                    canonical_sha256(&self.embedding).unwrap().as_str(),
                )
                .unwrap(),
                embedding_dimension: u16::try_from(self.embedding.embedding_key().dimensions)
                    .expect("fixture embedding dimension fits recipe"),
                model_artifact_digest: SemanticModelArtifactDigestV1::new(
                    self.embedding
                        .embedding_key()
                        .model_artifact_digest
                        .as_str(),
                )
                .unwrap(),
                projection_manifest_digest: SemanticProjectionManifestDigestV1::new(
                    self.embedding.projection_key().profile_digest.as_str(),
                )
                .unwrap(),
                privacy_domain_digest: SemanticPrivacyDomainDigestV1::new(
                    canonical_sha256(self.embedding.privacy_domain())
                        .unwrap()
                        .as_str(),
                )
                .unwrap(),
                privacy_key_epoch: self.embedding.privacy_key_epoch(),
                expected_chunk_manifest_digest: manifest,
            },
            chunk_count,
            None,
            digest::<SemanticVectorCheckpointDigest>('9'),
            SemanticVectorWriterFence {
                binding: self.graph.binding.clone(),
            },
        )
        .unwrap()
    }

    pub fn page_batch_and_receipt(
        &self,
        plan: &SemanticVectorStagePlan,
        page: PageBatchSpec<'_>,
    ) -> (GraphWriteBatch, SemanticVectorStageBatchReceipt) {
        let PageBatchSpec {
            name,
            ordinal,
            start,
            count,
            expected_checkpoint,
            next_checkpoint,
            marker,
        } = page;
        let mut chunks = Vec::with_capacity(usize::try_from(count).unwrap());
        let mut page_chunks = Vec::with_capacity(chunks.capacity());
        for index in start..start.saturating_add(count) {
            let chunk_id = page_chunk_id(name, index);
            let chunk_digest = unique_digest::<SemanticVectorChunkDigest>(index);
            let dimension = usize::try_from(self.embedding.embedding_key().dimensions).unwrap();
            let values = vec![marker; dimension];
            let output_digest = SemanticVectorOutputDigest::new(
                semantic_vector_output_digest(
                    self.embedding.projection_key(),
                    &CodeSearchChunkId::try_from(chunk_id.as_str().to_owned()).unwrap(),
                    &ContentDigest::try_from(chunk_digest.as_str().to_owned()).unwrap(),
                    &values,
                )
                .unwrap()
                .as_str(),
            )
            .unwrap();
            page_chunks.push((
                chunk_id.clone(),
                chunk_digest.clone(),
                output_digest.clone(),
                values,
            ));
            chunks.push(SemanticVectorStageChunkReceipt {
                effect_ordinal: u32::try_from(index.saturating_sub(start)).unwrap(),
                chunk_id,
                chunk_digest,
                operation: SemanticVectorStageChunkOperation::Embed,
                output_digest: Some(output_digest),
            });
        }
        let mutations = canonical_page_mutations(
            plan,
            &self.embedding,
            ordinal,
            start.saturating_add(count),
            &page_chunks,
        );
        let mut batch = GraphWriteBatch::new(
            GraphNamespace::new(plan.key.projection.namespace.as_str()).unwrap(),
            GraphProjectionId::new(plan.key.projection.projection.as_str()).unwrap(),
            SourceGeneration::new(plan.source_generation.as_str()).unwrap(),
            GraphWatermark::new(format!(
                "watermark.{}.{}",
                plan.key.build_id.as_str(),
                ordinal
            ))
            .unwrap(),
            mutations,
            Arc::new(TestCancellation),
        )
        .unwrap();
        let output = batch.semantic_vector_output_digest().unwrap();
        let receipt = SemanticVectorStageBatchReceipt::new(
            SemanticVectorStageBatchKey {
                stage: plan.key.clone(),
                ordinal,
            },
            expected_checkpoint,
            unique_digest::<SemanticVectorBatchInputDigest>(10_000 + ordinal),
            output,
            next_checkpoint,
            chunks,
        )
        .unwrap();
        (batch, receipt)
    }

    pub fn append(
        &self,
        authority: &mut SemanticVectorStagingExactSqlStorage,
        plan: &SemanticVectorStagePlan,
        receipt: &SemanticVectorStageBatchReceipt,
        suffix: &str,
    ) {
        with_context(&format!("{suffix}.append"), |context| {
            authority
                .append_stage_batch(receipt, &plan.writer_fence, context)
                .unwrap();
        });
    }

    pub fn semantic_entity_reference(&self, plan: &SemanticVectorStagePlan) -> GraphEntityRef {
        let name = plan
            .key
            .build_id
            .as_str()
            .strip_prefix("build.")
            .expect("fixture build id has canonical prefix");
        GraphEntityRef {
            projection: GraphProjectionIdentity::new(
                GraphNamespace::new(plan.key.projection.namespace.as_str()).unwrap(),
                GraphProjectionId::new(plan.key.projection.projection.as_str()).unwrap(),
            ),
            identity: GraphEntityId::new(scoped_id(
                "generation-vector",
                plan.semantic_generation_id.as_digest().as_str(),
                &format!("chunk.{name}"),
            ))
            .unwrap(),
        }
    }

    fn seed_source_generation(&self) {
        let projection = GraphProjectionIdentity::new(
            GraphNamespace::new(
                self.source_dependency
                    .generation
                    .projection
                    .namespace
                    .as_str(),
            )
            .unwrap(),
            GraphProjectionId::new(
                self.source_dependency
                    .generation
                    .projection
                    .projection
                    .as_str(),
            )
            .unwrap(),
        );
        let manifest = GraphGenerationManifest::new(
            projection,
            GraphGenerationId::new(self.source_dependency.generation.generation.as_str()).unwrap(),
            SourceGeneration::new("source-code-generation").unwrap(),
            GraphWatermark::new("source-watermark").unwrap(),
            vec![],
            vec![],
            vec![],
        )
        .unwrap();
        let replay = manifest
            .relational_replay(
                self.graph.binding.shard_id.clone(),
                GraphIdempotencyKey::new(self.source_dependency.idempotency_key.as_str()).unwrap(),
                digest::<GraphPublicationInputDigestV1>('0'),
                None,
                &|| Ok(()),
            )
            .unwrap();
        let key = replay.key.clone();
        let mut authority = self.authority();
        with_context("source.append", |context| {
            assert!(matches!(
                authority.append_replay(&replay, context).unwrap(),
                tracedecay_store::GraphReplayAppendOutcomeV1::Appended(_)
            ));
            assert_eq!(
                authority
                    .pending_replay(&key.projection, context)
                    .unwrap()
                    .map(|record| record.publication),
                Some(replay.clone())
            );
            assert_eq!(
                authority.verified_head(&key.projection, context).unwrap(),
                replay.expected_prior_head
            );
        });
        with_context("source.publish", |context| {
            self.graph
                .registry
                .publish_verified(
                    self.registration(),
                    &mut authority,
                    context,
                    &key,
                    Some(manifest),
                )
                .unwrap();
        });
    }
}

fn mutate_native_effect(mutations: &mut Vec<GraphMutation>, mismatch: NativeMismatch) {
    let effect_index = mutations
        .iter()
        .position(|mutation| {
            matches!(
                mutation,
                GraphMutation::UpsertEntity(entity)
                    if entity
                        .labels
                        .iter()
                        .any(|label| label.as_str() == "semantic-vector-generation-vector-v1")
            )
        })
        .unwrap();
    match mismatch {
        NativeMismatch::ChunkId => {
            let GraphMutation::UpsertEntity(entity) = &mut mutations[effect_index] else {
                unreachable!()
            };
            entity.properties.insert(
                GraphPropertyName::new("chunk_id").unwrap(),
                string("chunk.foreign"),
            );
        }
        NativeMismatch::ChunkDigest => {
            let GraphMutation::UpsertEntity(entity) = &mut mutations[effect_index] else {
                unreachable!()
            };
            entity.properties.insert(
                GraphPropertyName::new("chunk_digest").unwrap(),
                string(&format!("sha256:{}", "d".repeat(64))),
            );
        }
        NativeMismatch::Operation => {
            let GraphMutation::UpsertEntity(entity) = &mut mutations[effect_index] else {
                unreachable!()
            };
            entity.labels =
                BTreeSet::from([
                    GraphLabel::new("semantic-vector-generation-tombstone-v1").unwrap()
                ]);
        }
        NativeMismatch::OutputDigest => {
            let GraphMutation::UpsertEntity(entity) = &mut mutations[effect_index] else {
                unreachable!()
            };
            entity.properties.insert(
                GraphPropertyName::new("output_digest").unwrap(),
                string(&format!("sha256:{}", "e".repeat(64))),
            );
        }
        NativeMismatch::Metric | NativeMismatch::VectorValues => {
            let GraphMutation::UpsertEntity(entity) = &mut mutations[effect_index] else {
                unreachable!()
            };
            let vector = entity
                .properties
                .values_mut()
                .find_map(|property| match property {
                    GraphProperty::Vector(vector) => Some(vector),
                    _ => None,
                })
                .unwrap();
            match mismatch {
                NativeMismatch::Metric => vector.metric = VectorMetric::Euclidean,
                NativeMismatch::VectorValues => vector.values[0] += 100.0,
                _ => unreachable!(),
            }
        }
        NativeMismatch::ExtraEffect => {
            let GraphMutation::UpsertEntity(mut entity) = mutations[effect_index].clone() else {
                unreachable!()
            };
            entity.identity =
                GraphEntityId::new("semantic-vector:generation-vector:extra").unwrap();
            mutations.push(GraphMutation::UpsertEntity(entity));
        }
        NativeMismatch::MissingEffect => {
            mutations.remove(effect_index);
        }
        NativeMismatch::ProjectionReceipt => {
            let receipt = mutations
                .iter_mut()
                .find_map(|mutation| match mutation {
                    GraphMutation::UpsertEntity(entity)
                        if entity.labels.iter().any(|label| {
                            label.as_str() == "semantic-vector-generation-receipt-v1"
                        }) =>
                    {
                        Some(entity)
                    }
                    _ => None,
                })
                .unwrap();
            receipt.properties.insert(
                GraphPropertyName::new("receipt").unwrap(),
                GraphProperty::Bytes(vec![0xff]),
            );
        }
        NativeMismatch::PageSourceManifest => {
            let receipt = mutations
                .iter_mut()
                .find_map(|mutation| match mutation {
                    GraphMutation::UpsertEntity(entity)
                        if entity.labels.iter().any(|label| {
                            label.as_str() == "semantic-vector-generation-receipt-v1"
                        }) =>
                    {
                        Some(entity)
                    }
                    _ => None,
                })
                .unwrap();
            let property = receipt
                .properties
                .get_mut(&GraphPropertyName::new("receipt").unwrap())
                .unwrap();
            let GraphProperty::Bytes(bytes) = property else {
                unreachable!()
            };
            let mut decoded = semantic_vector_native::decode_generation_receipt(bytes).unwrap();
            let page_manifest = digest::<ManifestDigest>('9');
            decoded.source_manifest_digest = page_manifest.clone();
            for chunk in &mut decoded.receipts {
                chunk.source_manifest_digest = page_manifest.clone();
            }
            decoded.publication_digest = projection_batch_publication_digest(&decoded).unwrap();
            *bytes = semantic_vector_native::encode_generation_receipt(&decoded).unwrap();
        }
        NativeMismatch::SameProfileProjection => {
            let receipt = mutations
                .iter_mut()
                .find_map(|mutation| match mutation {
                    GraphMutation::UpsertEntity(entity)
                        if entity.labels.iter().any(|label| {
                            label.as_str() == "semantic-vector-generation-receipt-v1"
                        }) =>
                    {
                        Some(entity)
                    }
                    _ => None,
                })
                .unwrap();
            let property = receipt
                .properties
                .get_mut(&GraphPropertyName::new("receipt").unwrap())
                .unwrap();
            let GraphProperty::Bytes(bytes) = property else {
                unreachable!()
            };
            let mut decoded = semantic_vector_native::decode_generation_receipt(bytes).unwrap();
            decoded.target_projection_key.schema_revision = "embedding.fixture.foreign".to_owned();
            for chunk in &mut decoded.receipts {
                chunk.projection_key = decoded.target_projection_key.clone();
            }
            decoded.publication_digest = projection_batch_publication_digest(&decoded).unwrap();
            *bytes = semantic_vector_native::encode_generation_receipt(&decoded).unwrap();
        }
    }
}

fn canonical_native_mutations(
    plan: &SemanticVectorStagePlan,
    embedding: &AdmittedEmbeddingProjectionKeyV1,
    chunk_id: &SemanticVectorChunkId,
    chunk_digest: &SemanticVectorChunkDigest,
    output_digest: &SemanticVectorOutputDigest,
    values: Vec<f32>,
) -> Vec<GraphMutation> {
    let generation = plan.semantic_generation_id.as_digest().as_str();
    let owner_id = format!("semantic-vector:generation:{generation}");
    let effect_id = scoped_id("generation-vector", generation, chunk_id.as_str());
    let receipt_id = scoped_id("generation-receipt", generation, "0");
    let entities = [
        entity(
            "semantic-vector:control",
            &["semantic-vector-control-v1"],
            [("revision", GraphProperty::I64(1))],
        ),
        entity(
            &owner_id,
            &["semantic-vector-generation-v1"],
            [
                ("generation_id", string(generation)),
                ("target_projection", bytes(embedding.projection_key())),
                ("source_generation", string(plan.source_generation.as_str())),
                (
                    "source_manifest",
                    string(plan.recipe.source_manifest_digest.as_str()),
                ),
                ("base_generation", string("")),
                ("embedding_key", bytes(embedding)),
                ("checkpoint", bytes(&plan.initial_checkpoint_digest)),
                ("manifest_digest", string(generation)),
                ("row_count", GraphProperty::I64(1)),
                ("vector_bytes", GraphProperty::I64(12)),
                ("tombstone_count", GraphProperty::I64(0)),
                ("receipt_count", GraphProperty::I64(1)),
            ],
        ),
        entity(
            &effect_id,
            &[
                "semantic-vector-generation-vector-v1",
                &format!("semantic-vector-generation:{generation}"),
            ],
            [
                ("generation_id", string(generation)),
                ("chunk_id", string(chunk_id.as_str())),
                ("chunk_digest", string(chunk_digest.as_str())),
                ("output_digest", string(output_digest.as_str())),
                (
                    &format!("vector:{generation}"),
                    GraphProperty::Vector(
                        GraphVector::new(values, 3, VectorMetric::Cosine).unwrap(),
                    ),
                ),
            ],
        ),
        entity(
            &receipt_id,
            &["semantic-vector-generation-receipt-v1"],
            [
                ("generation_id", string(generation)),
                (
                    "receipt",
                    receipt_bytes(&projection_receipt(
                        plan,
                        embedding,
                        chunk_id,
                        chunk_digest,
                        output_digest,
                    )),
                ),
                ("ordinal", GraphProperty::I64(0)),
            ],
        ),
    ];
    let relations = [
        relation(
            "semantic-vector:control",
            &owner_id,
            "semantic_vector_generation_catalog",
            "generation-catalog",
        ),
        relation(&owner_id, &effect_id, "semantic_vector_contains", "vector"),
        relation(&owner_id, &receipt_id, "semantic_vector_contains", "batch"),
    ];
    entities
        .into_iter()
        .map(GraphMutation::UpsertEntity)
        .chain(relations.into_iter().map(GraphMutation::UpsertRelation))
        .collect()
}

fn page_chunk_id(name: &str, index: u64) -> SemanticVectorChunkId {
    SemanticVectorChunkId::new(format!("chunk.{name}.{index:05}")).unwrap()
}

fn unique_digest<T: TryFrom<String>>(index: u64) -> T
where
    T::Error: std::fmt::Debug,
{
    T::try_from(format!("sha256:{index:064x}")).unwrap()
}

fn canonical_page_mutations(
    plan: &SemanticVectorStagePlan,
    embedding: &AdmittedEmbeddingProjectionKeyV1,
    ordinal: u64,
    applied_rows: u64,
    page_chunks: &[(
        SemanticVectorChunkId,
        SemanticVectorChunkDigest,
        SemanticVectorOutputDigest,
        Vec<f32>,
    )],
) -> Vec<GraphMutation> {
    let generation = plan.semantic_generation_id.as_digest().as_str();
    let owner_id = format!("semantic-vector:generation:{generation}");
    let receipt_id = scoped_id("generation-receipt", generation, &ordinal.to_string());
    let applied = i64::try_from(applied_rows).unwrap();
    let mut entities = vec![
        entity(
            "semantic-vector:control",
            &["semantic-vector-control-v1"],
            [(
                "revision",
                GraphProperty::I64(i64::try_from(ordinal).unwrap() + 1),
            )],
        ),
        entity(
            &owner_id,
            &["semantic-vector-generation-v1"],
            [
                ("generation_id", string(generation)),
                ("target_projection", bytes(embedding.projection_key())),
                ("source_generation", string(plan.source_generation.as_str())),
                (
                    "source_manifest",
                    string(plan.recipe.source_manifest_digest.as_str()),
                ),
                ("base_generation", string("")),
                ("embedding_key", bytes(embedding)),
                ("checkpoint", bytes(&plan.initial_checkpoint_digest)),
                ("manifest_digest", string(generation)),
                ("row_count", GraphProperty::I64(applied)),
                (
                    "vector_bytes",
                    GraphProperty::I64(
                        applied.saturating_mul(
                            i64::try_from(
                                page_chunks
                                    .first()
                                    .map(|(_, _, _, values)| values.len().saturating_mul(4))
                                    .unwrap_or(0),
                            )
                            .unwrap(),
                        ),
                    ),
                ),
                ("tombstone_count", GraphProperty::I64(0)),
                (
                    "receipt_count",
                    GraphProperty::I64(i64::try_from(ordinal).unwrap() + 1),
                ),
            ],
        ),
        entity(
            &receipt_id,
            &["semantic-vector-generation-receipt-v1"],
            [
                ("generation_id", string(generation)),
                (
                    "receipt",
                    receipt_bytes(&page_projection_receipt(plan, embedding, page_chunks)),
                ),
                (
                    "ordinal",
                    GraphProperty::I64(i64::try_from(ordinal).unwrap()),
                ),
            ],
        ),
    ];
    let mut relations = vec![
        relation(
            "semantic-vector:control",
            &owner_id,
            "semantic_vector_generation_catalog",
            "generation-catalog",
        ),
        relation(&owner_id, &receipt_id, "semantic_vector_contains", "batch"),
    ];
    for (chunk_id, chunk_digest, output_digest, values) in page_chunks {
        let effect_id = scoped_id("generation-vector", generation, chunk_id.as_str());
        entities.push(entity(
            &effect_id,
            &[
                "semantic-vector-generation-vector-v1",
                &format!("semantic-vector-generation:{generation}"),
            ],
            [
                ("generation_id", string(generation)),
                ("chunk_id", string(chunk_id.as_str())),
                ("chunk_digest", string(chunk_digest.as_str())),
                ("output_digest", string(output_digest.as_str())),
                (
                    &format!("vector:{generation}"),
                    GraphProperty::Vector(
                        GraphVector::new(values.clone(), values.len(), VectorMetric::Cosine)
                            .unwrap(),
                    ),
                ),
            ],
        ));
        relations.push(relation(
            &owner_id,
            &effect_id,
            "semantic_vector_contains",
            "vector",
        ));
    }
    entities
        .into_iter()
        .map(GraphMutation::UpsertEntity)
        .chain(relations.into_iter().map(GraphMutation::UpsertRelation))
        .collect()
}

fn page_projection_receipt(
    plan: &SemanticVectorStagePlan,
    embedding: &AdmittedEmbeddingProjectionKeyV1,
    page_chunks: &[(
        SemanticVectorChunkId,
        SemanticVectorChunkDigest,
        SemanticVectorOutputDigest,
        Vec<f32>,
    )],
) -> ProjectionBatchReceiptV1 {
    let request_digest = digest::<ManifestDigest>('f');
    let source_generation =
        CodeGenerationId::try_from(plan.source_generation.as_str().to_owned()).unwrap();
    let source_manifest_digest =
        ManifestDigest::try_from(plan.recipe.source_manifest_digest.as_str().to_owned()).unwrap();
    let mut receipts = page_chunks
        .iter()
        .map(
            |(chunk_id, chunk_digest, output_digest, _)| CodeChunkProjectionReceiptV1 {
                projection_key: embedding.projection_key().clone(),
                request_digest: request_digest.clone(),
                prior_generation: None,
                source_generation: source_generation.clone(),
                source_manifest_digest: source_manifest_digest.clone(),
                chunk_id: CodeSearchChunkId::try_from(chunk_id.as_str().to_owned()).unwrap(),
                prior_chunk_digest: None,
                current_chunk_digest: Some(
                    ContentDigest::try_from(chunk_digest.as_str().to_owned()).unwrap(),
                ),
                operation: ProjectionOperationV1::Added,
                outcome: ProjectionOutcomeV1::Applied,
                output_digest: Some(
                    ContentDigest::try_from(output_digest.as_str().to_owned()).unwrap(),
                ),
            },
        )
        .collect::<Vec<_>>();
    receipts.sort_by(|left, right| left.chunk_id.cmp(&right.chunk_id));
    let mut receipt = ProjectionBatchReceiptV1 {
        target_projection_key: embedding.projection_key().clone(),
        request_digest,
        source_generation,
        source_manifest_digest,
        receipts,
        reused_count: 0,
        publication_digest: digest('0'),
    };
    receipt.publication_digest = projection_batch_publication_digest(&receipt).unwrap();
    receipt
}

fn projection_receipt(
    plan: &SemanticVectorStagePlan,
    embedding: &AdmittedEmbeddingProjectionKeyV1,
    chunk_id: &SemanticVectorChunkId,
    chunk_digest: &SemanticVectorChunkDigest,
    output_digest: &SemanticVectorOutputDigest,
) -> ProjectionBatchReceiptV1 {
    let request_digest = digest::<ManifestDigest>('f');
    let source_generation =
        CodeGenerationId::try_from(plan.source_generation.as_str().to_owned()).unwrap();
    let source_manifest_digest =
        ManifestDigest::try_from(plan.recipe.source_manifest_digest.as_str().to_owned()).unwrap();
    let chunk_id = CodeSearchChunkId::try_from(chunk_id.as_str().to_owned()).unwrap();
    let chunk_digest = ContentDigest::try_from(chunk_digest.as_str().to_owned()).unwrap();
    let output_digest = ContentDigest::try_from(output_digest.as_str().to_owned()).unwrap();
    let mut receipt = ProjectionBatchReceiptV1 {
        target_projection_key: embedding.projection_key().clone(),
        request_digest: request_digest.clone(),
        source_generation: source_generation.clone(),
        source_manifest_digest: source_manifest_digest.clone(),
        receipts: vec![CodeChunkProjectionReceiptV1 {
            projection_key: embedding.projection_key().clone(),
            request_digest,
            prior_generation: None,
            source_generation,
            source_manifest_digest,
            chunk_id,
            prior_chunk_digest: None,
            current_chunk_digest: Some(chunk_digest),
            operation: ProjectionOperationV1::Added,
            outcome: ProjectionOutcomeV1::Applied,
            output_digest: Some(output_digest),
        }],
        reused_count: 0,
        publication_digest: digest('0'),
    };
    receipt.publication_digest = projection_batch_publication_digest(&receipt).unwrap();
    receipt
}

fn entity<const N: usize>(
    identity: &str,
    labels: &[&str],
    properties: [(&str, GraphProperty); N],
) -> GraphEntity {
    GraphEntity::new(
        GraphEntityId::new(identity).unwrap(),
        labels
            .iter()
            .map(|label| GraphLabel::new(*label).unwrap())
            .collect(),
        properties
            .into_iter()
            .map(|(name, value)| (GraphPropertyName::new(name).unwrap(), value))
            .collect(),
    )
    .unwrap()
}

fn relation(from: &str, to: &str, kind: &str, discriminator: &str) -> GraphRelation {
    GraphRelation::new(
        GraphRelationId::new(relation_id(from, to, kind, discriminator)).unwrap(),
        GraphEntityId::new(from).unwrap(),
        GraphEntityId::new(to).unwrap(),
        GraphRelationKind::new(kind).unwrap(),
        BTreeMap::new(),
    )
    .unwrap()
}

fn scoped_id(kind: &str, owner: &str, member: &str) -> String {
    semantic_vector_native::scoped_entity_id(kind, owner, member)
        .unwrap()
        .to_string()
}

fn relation_id(from: &str, to: &str, kind: &str, discriminator: &str) -> String {
    semantic_vector_native::relation_id(
        &GraphEntityId::new(from).unwrap(),
        &GraphEntityId::new(to).unwrap(),
        kind,
        discriminator,
    )
    .unwrap()
    .to_string()
}

fn string(value: &str) -> GraphProperty {
    GraphProperty::String(value.to_owned())
}

fn bytes(value: &impl serde::Serialize) -> GraphProperty {
    GraphProperty::Bytes(serde_json::to_vec(value).unwrap())
}

fn receipt_bytes(receipt: &ProjectionBatchReceiptV1) -> GraphProperty {
    GraphProperty::Bytes(semantic_vector_native::encode_generation_receipt(receipt).unwrap())
}

fn admitted_embedding_with_dimensions(dimensions: u32) -> AdmittedEmbeddingProjectionKeyV1 {
    EmbeddingProjectionKeyV1 {
        model_artifact_digest: digest('a'),
        tokenizer_digest: digest('b'),
        config_digest: digest('c'),
        query_instruction_digest: None,
        document_instruction_digest: None,
        pooling: EmbeddingPoolingV1::Mean,
        truncation_side: EmbeddingTruncationSideV1::Right,
        truncation_length: 512,
        inference_batch_size: 8,
        inference_batch_bytes: 16 * 1024,
        runtime_backend: "fixture-runtime".to_owned(),
        runtime_build_revision: "fixture-runtime.v1".to_owned(),
        device_class: EmbeddingDeviceClassV1::Cpu,
        dimensions,
        metric: EmbeddingMetricV1::Cosine,
        normalization: EmbeddingNormalizationV1::L2,
        precision: EmbeddingPrecisionV1::Fp32,
        chunk_schema_revision: "code-search-chunk.v1".to_owned(),
        chunker_revision: ChunkerRevision::new("chunker.fixture.v1").unwrap(),
        privacy_domain: PrivacyDomainId::new("privacy.fixture").unwrap(),
        privacy_key_epoch: 1,
    }
    .admit()
    .unwrap()
}

pub fn with_context<T>(
    suffix: &str,
    operation: impl FnOnce(&GraphPublicationOperationContextV1<'_>) -> T,
) -> T {
    let cancellation = RuntimeCancellationIdentityV1 {
        cancellation_id: RuntimeCancellationIdV1::new(format!("cancel.{suffix}")).unwrap(),
        generation: 1,
    };
    let deadline = RuntimeDeadlineV1 {
        deadline_id: RuntimeDeadlineIdV1::new(format!("deadline.{suffix}")).unwrap(),
    };
    let control = RuntimeRequestControlV1 {
        requested_at: UtcMicros(1),
        deadline: deadline.clone(),
        cancellation: cancellation.clone(),
    };
    let probe = RequestProbe {
        cancellation,
        deadline,
    };
    let context = GraphPublicationOperationContextV1::new(&control, &probe).unwrap();
    operation(&context)
}

fn source_scope(binding: &StoreRuntimeBindingV1) -> StoreShardIdV1 {
    let StoreShardIdV1 {
        brain_id,
        profile_id,
        scope: tracedecay_store::StoreShardScopeV1::Project { project_id },
    } = &binding.shard_id
    else {
        panic!("graph contract fixture must use a project shard");
    };
    StoreShardIdV1::code(
        brain_id.clone(),
        profile_id.clone(),
        project_id.clone(),
        RepositoryId::new("repository.fixture").unwrap(),
        CodeShardScopeV1::Worktree {
            worktree_id: WorktreeId::new("worktree.fixture").unwrap(),
        },
    )
}

fn digest<T: TryFrom<String>>(byte: char) -> T
where
    T::Error: std::fmt::Debug,
{
    T::try_from(format!("sha256:{}", byte.to_string().repeat(64))).unwrap()
}

fn digest_byte(value: &str, offset: usize) -> char {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    char::from(
        DIGITS[(value
            .bytes()
            .fold(offset, |sum, byte| sum + usize::from(byte)))
            % 16],
    )
}

pub fn settle_publication(
    authority: &mut SemanticVectorStagingExactSqlStorage,
    plan: &SemanticVectorStagePlan,
    commit: &VerifiedGraphCommit,
    suffix: &str,
) {
    with_context(suffix, |context| {
        assert!(matches!(
            authority
                .settle_published(
                    &tracedecay_store::SemanticVectorStagePublishSettlement {
                        stage: plan.key.clone(),
                        verified_head: commit.head.clone(),
                    },
                    &plan.writer_fence,
                    context,
                )
                .unwrap(),
            tracedecay_store::SemanticVectorStagePublishOutcome::Published(_)
                | tracedecay_store::SemanticVectorStagePublishOutcome::ExactReplay(_)
        ));
    });
}
