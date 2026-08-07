use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

use rusqlite::Savepoint;
use tempfile::TempDir;
use tracedecay_domain::{
    BrainId, LocatorDigest, ProjectId, RepositoryId, UserProfileId, UtcMicros,
    VectorGenerationIdV1, WorktreeId, canonical_sha256,
};
use tracedecay_store::{
    AdmissionConfigV1, CodeShardScopeV1, GraphDependencyGenerationClosureDigestV1,
    GraphDependencyGenerationIdentityV1, GraphGenerationIdV1, GraphNamespaceV1,
    GraphProjectionIdV1, GraphProjectionIdentityV1, GraphPublicationIdempotencyKeyV1,
    GraphPublicationInputDigestV1, GraphPublicationKeyV1, GraphPublicationOperationContextV1,
    GraphPublicationReplayRetirementV1, GraphPublicationReplayV1, GraphPublicationStoreErrorV1,
    GraphPublicationStoreV1, GraphRecoveredGenerationDigestV1, GraphVerifiedHeadCasOutcomeV1,
    GraphVerifiedHeadCompareAndSwapV1, MAX_SEMANTIC_VECTOR_STAGE_CHUNKS, RuntimeCancellationIdV1,
    RuntimeCancellationIdentityV1, RuntimeDeadlineIdV1, RuntimeDeadlineV1, RuntimeInterruptionV1,
    RuntimeRequestControlV1, RuntimeRequestProbeV1, SemanticEmbeddingProjectionDigestV1,
    SemanticModelArtifactDigestV1, SemanticPrivacyDomainDigestV1,
    SemanticProjectionManifestDigestV1, SemanticVectorBatchInputDigest,
    SemanticVectorBatchOutputDigest, SemanticVectorBuildId, SemanticVectorCancelledRetirement,
    SemanticVectorCancelledRetirementOutcome, SemanticVectorCheckpointDigest,
    SemanticVectorChunkDigest, SemanticVectorChunkId, SemanticVectorChunkManifestDigest,
    SemanticVectorChunkManifestMember, SemanticVectorGraphBatchDigest, SemanticVectorOutputDigest,
    SemanticVectorPublicationAuthority, SemanticVectorPublishedGenerationKey,
    SemanticVectorPublishedGenerationLookup, SemanticVectorPublishedRetirement,
    SemanticVectorPublishedRetirementOutcome, SemanticVectorReconstructionRecipe,
    SemanticVectorSourceDependencyV1, SemanticVectorSourceGenerationId,
    SemanticVectorSourceManifestDigest, SemanticVectorStageAppendOutcome,
    SemanticVectorStageBatchKey, SemanticVectorStageBatchReceipt, SemanticVectorStageBeginOutcome,
    SemanticVectorStageCensusRequest, SemanticVectorStageChunkOperation,
    SemanticVectorStageChunkReceipt, SemanticVectorStageEffectTerminal, SemanticVectorStageKey,
    SemanticVectorStagePlan, SemanticVectorStagePublicationPrepareOutcome,
    SemanticVectorStagePublicationPrepareRequest, SemanticVectorStagePublishOutcome,
    SemanticVectorStagePublishSettlement, SemanticVectorStageSettlement,
    SemanticVectorStageSettlementOutcome, SemanticVectorStageState,
    SemanticVectorStageWriterAdoption, SemanticVectorStageWriterAdoptionOutcome,
    SemanticVectorStagingStore, SemanticVectorStagingStoreError, SemanticVectorWriterFence,
    StoreAuthorityEpochV1, StoreIncarnationV1, StoreRuntimeBindingV1, StoreShardIdV1,
    VerifiedStoreLocatorV1, semantic_vector_chunk_manifest_digest,
};

use crate::{
    ExistingWriterLocator, PersistentWriter, StorageOperationExecutor,
    exact_sql::{
        ExactSqlError, ExactSqlHandle, ExactSqlStatement, ExactSqlValue, ExactSqlWriteAuthority,
        ExactSqlWriteIntent,
    },
    reader::{ExistingReaderLocator, ReaderPool, ReaderQueryExecutor},
};

use super::{SEMANTIC_VECTOR_STAGING_SCHEMA, SemanticVectorStagingExactSqlStorage};
use crate::repository::GRAPH_PUBLICATION_SCHEMA_V1;

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

struct RevocableAuthority(Arc<AtomicBool>);
impl ExactSqlWriteAuthority for RevocableAuthority {
    fn verify(&self, _intent: ExactSqlWriteIntent) -> Result<(), ExactSqlError> {
        if self.0.load(Ordering::Acquire) {
            Ok(())
        } else {
            Err(ExactSqlError::AuthorityDenied(
                "revoked fixture authority".to_owned(),
            ))
        }
    }
}

struct Fixture {
    _directory: TempDir,
    _writer: PersistentWriter,
    _readers: ReaderPool<NoReads>,
    handle: ExactSqlHandle,
    binding: StoreRuntimeBindingV1,
    allowed: Arc<AtomicBool>,
}

impl Fixture {
    fn new() -> Self {
        let directory = TempDir::new().unwrap();
        let path = directory.path().join("semantic-vector-staging.sqlite3");
        drop(rusqlite::Connection::open(&path).unwrap());
        let path = path.canonicalize().unwrap();
        let binding: StoreRuntimeBindingV1 = serde_json::from_value(serde_json::json!({
            "shard_id": {
                "brain_id": "brain.fixture",
                "profile_id": "profile.fixture",
                "scope": { "kind": "project", "project_id": "project.fixture" }
            },
            "incarnation": 3,
            "authority_epoch": 11
        }))
        .unwrap();
        let locator = VerifiedStoreLocatorV1::new(
            binding.shard_id.clone(),
            StoreIncarnationV1::new(3).unwrap(),
            LocatorDigest::new(format!("sha256:{}", "a".repeat(64))).unwrap(),
        );
        let writer = PersistentWriter::start(
            ExistingWriterLocator::new(binding.clone(), locator.clone(), path.clone()).unwrap(),
            AdmissionConfigV1::default(),
            NoWrites,
        )
        .unwrap();
        let readers = ReaderPool::start(
            ExistingReaderLocator::new(binding.clone(), locator, path).unwrap(),
            AdmissionConfigV1::default().readers,
            NoReads,
        )
        .unwrap();
        let base = ExactSqlHandle::attach(&writer, &readers).unwrap();
        base.execute_batch(GRAPH_PUBLICATION_SCHEMA_V1.to_owned())
            .unwrap();
        base.execute_batch(SEMANTIC_VECTOR_STAGING_SCHEMA.to_owned())
            .unwrap();
        let allowed = Arc::new(AtomicBool::new(true));
        let handle = base
            .with_write_authority(Arc::new(RevocableAuthority(Arc::clone(&allowed))))
            .unwrap();
        Self {
            _directory: directory,
            _writer: writer,
            _readers: readers,
            handle,
            binding,
            allowed,
        }
    }

    fn storage(&self) -> SemanticVectorStagingExactSqlStorage {
        SemanticVectorStagingExactSqlStorage::from_authorized_handle(self.handle.clone()).unwrap()
    }
}

struct Probe {
    cancellation: RuntimeCancellationIdentityV1,
    deadline: RuntimeDeadlineV1,
    interruption: Option<RuntimeInterruptionV1>,
}
impl RuntimeRequestProbeV1 for Probe {
    fn cancellation_identity(&self) -> &RuntimeCancellationIdentityV1 {
        &self.cancellation
    }
    fn deadline_identity(&self) -> &RuntimeDeadlineV1 {
        &self.deadline
    }
    fn interruption(&self) -> Option<RuntimeInterruptionV1> {
        self.interruption
    }
    fn try_begin_commit(&self) -> bool {
        self.interruption.is_none()
    }
}

fn operation(suffix: &str) -> (RuntimeRequestControlV1, Probe) {
    interrupted_operation(suffix, None)
}

fn interrupted_operation(
    suffix: &str,
    interruption: Option<RuntimeInterruptionV1>,
) -> (RuntimeRequestControlV1, Probe) {
    let cancellation = RuntimeCancellationIdentityV1 {
        cancellation_id: RuntimeCancellationIdV1::new(format!("cancel.{suffix}")).unwrap(),
        generation: 1,
    };
    let deadline = RuntimeDeadlineV1 {
        deadline_id: RuntimeDeadlineIdV1::new(format!("deadline.{suffix}")).unwrap(),
    };
    (
        RuntimeRequestControlV1 {
            requested_at: UtcMicros(1),
            cancellation: cancellation.clone(),
            deadline: deadline.clone(),
        },
        Probe {
            cancellation,
            deadline,
            interruption,
        },
    )
}

#[test]
fn begin_exact_replay_conflict_and_interruption_are_typed() {
    let fixture = Fixture::new();
    let plan = plan(&fixture, "begin-cases", chunk_manifest("chunk.fixture"));
    let (control, probe) = operation("begin.cases");
    let context = GraphPublicationOperationContextV1::new(&control, &probe).unwrap();
    assert!(matches!(
        fixture.storage().begin_stage(&plan, &context).unwrap(),
        SemanticVectorStageBeginOutcome::Begun(_)
    ));
    let (control, probe) = operation("begin.replay");
    let context = GraphPublicationOperationContextV1::new(&control, &probe).unwrap();
    assert!(matches!(
        fixture.storage().begin_stage(&plan, &context).unwrap(),
        SemanticVectorStageBeginOutcome::ExactReplay(_)
    ));
    let mut conflict = plan.clone();
    conflict.source_generation = SemanticVectorSourceGenerationId::new("code.conflicting").unwrap();
    let (control, probe) = operation("begin.conflict");
    let context = GraphPublicationOperationContextV1::new(&control, &probe).unwrap();
    assert!(matches!(
        fixture.storage().begin_stage(&conflict, &context),
        Err(SemanticVectorStagingStoreError::InvalidRequest(_))
    ));

    let second = self::plan(&fixture, "cancelled-begin", chunk_manifest("chunk.fixture"));
    let (control, probe) =
        interrupted_operation("cancelled.begin", Some(RuntimeInterruptionV1::Cancelled));
    let context = GraphPublicationOperationContextV1::new(&control, &probe).unwrap();
    assert_eq!(
        fixture.storage().begin_stage(&second, &context),
        Err(SemanticVectorStagingStoreError::Interrupted(
            RuntimeInterruptionV1::Cancelled
        ))
    );
}

#[test]
fn begin_reserves_publication_generation_and_idempotency_independently() {
    let fixture = Fixture::new();
    let original = plan(
        &fixture,
        "publication-identity",
        chunk_manifest("chunk.fixture"),
    );
    let (control, probe) = operation("publication.identity.original");
    let context = GraphPublicationOperationContextV1::new(&control, &probe).unwrap();
    assert!(matches!(
        fixture.storage().begin_stage(&original, &context).unwrap(),
        SemanticVectorStageBeginOutcome::Begun(_)
    ));

    let same_generation = alternative_publication_plan(
        &original,
        "same-generation",
        original.publication_key.generation.as_str(),
        "publication.other",
    );
    let (control, probe) = operation("publication.identity.generation");
    let context = GraphPublicationOperationContextV1::new(&control, &probe).unwrap();
    assert_eq!(
        fixture
            .storage()
            .begin_stage(&same_generation, &context)
            .unwrap(),
        SemanticVectorStageBeginOutcome::PublicationConflict
    );

    let same_idempotency = alternative_publication_plan(
        &original,
        "same-idempotency",
        "generation.other",
        original.publication_key.idempotency_key.as_str(),
    );
    let (control, probe) = operation("publication.identity.idempotency");
    let context = GraphPublicationOperationContextV1::new(&control, &probe).unwrap();
    assert_eq!(
        fixture
            .storage()
            .begin_stage(&same_idempotency, &context)
            .unwrap(),
        SemanticVectorStageBeginOutcome::PublicationConflict
    );

    let mut conflicting_replay = publication_replay(&original);
    conflicting_replay.key = GraphPublicationKeyV1::new(
        original.key.projection.clone(),
        original.publication_key.generation.clone(),
        GraphPublicationIdempotencyKeyV1::new("publication.foreign").unwrap(),
    );
    let (control, probe) = operation("publication.identity.replay");
    let context = GraphPublicationOperationContextV1::new(&control, &probe).unwrap();
    assert_eq!(
        fixture
            .storage()
            .append_replay(&conflicting_replay, &context),
        Err(GraphPublicationStoreErrorV1::Infrastructure)
    );
}

#[test]
fn append_rejects_stale_progress_duplicate_chunks_and_reused_context() {
    let fixture = Fixture::new();
    let plan = plan_with_count(&fixture, "append-cases", chunk_manifest("chunk.fixture"), 2);
    let first = receipt(&plan.key);
    let (control, probe) = operation("begin.append.cases");
    let context = GraphPublicationOperationContextV1::new(&control, &probe).unwrap();
    fixture.storage().begin_stage(&plan, &context).unwrap();

    let stale_ordinal = receipt_at(&plan.key, 1, digest('9'), "chunk.fixture");
    let (control, probe) = operation("stale.ordinal");
    let context = GraphPublicationOperationContextV1::new(&control, &probe).unwrap();
    assert!(matches!(
        fixture
            .storage()
            .append_stage_batch(&stale_ordinal, &plan.writer_fence, &context)
            .unwrap(),
        SemanticVectorStageAppendOutcome::StaleOrdinal { .. }
    ));
    let stale_checkpoint = receipt_at(&plan.key, 0, digest('8'), "chunk.fixture");
    let (control, probe) = operation("stale.checkpoint");
    let context = GraphPublicationOperationContextV1::new(&control, &probe).unwrap();
    assert!(matches!(
        fixture
            .storage()
            .append_stage_batch(&stale_checkpoint, &plan.writer_fence, &context)
            .unwrap(),
        SemanticVectorStageAppendOutcome::StaleCheckpoint { .. }
    ));

    let (control, probe) = operation("append.first");
    let context = GraphPublicationOperationContextV1::new(&control, &probe).unwrap();
    fixture
        .storage()
        .append_stage_batch(&first, &plan.writer_fence, &context)
        .unwrap();
    let duplicate = receipt_at(
        &plan.key,
        1,
        first.checkpoint_digest.clone(),
        "chunk.fixture",
    );
    let (control, probe) = operation("append.duplicate");
    let context = GraphPublicationOperationContextV1::new(&control, &probe).unwrap();
    assert!(matches!(
        fixture
            .storage()
            .append_stage_batch(&duplicate, &plan.writer_fence, &context)
            .unwrap(),
        SemanticVectorStageAppendOutcome::DuplicateChunk { .. }
    ));

    let (control, probe) = operation("reused.context");
    let context = GraphPublicationOperationContextV1::new(&control, &probe).unwrap();
    let unique = receipt_at(
        &plan.key,
        1,
        first.checkpoint_digest.clone(),
        "chunk.second",
    );
    fixture
        .storage()
        .append_stage_batch(&unique, &plan.writer_fence, &context)
        .unwrap();
    let settlement = SemanticVectorStageSettlement {
        batch: first.key,
        expected_receipt_digest: first.receipt_digest,
        terminal: SemanticVectorStageEffectTerminal::Applied {
            graph_batch_digest: digest('a'),
        },
    };
    assert_eq!(
        fixture
            .storage()
            .settle_stage_batch(&settlement, &plan.writer_fence, &context),
        Err(SemanticVectorStagingStoreError::ReusedOperationContext)
    );

    let third = receipt_at(
        &plan.key,
        2,
        unique.checkpoint_digest.clone(),
        "chunk.third",
    );
    let (control, probe) = operation("append.over-cap");
    let context = GraphPublicationOperationContextV1::new(&control, &probe).unwrap();
    assert!(matches!(
        fixture
            .storage()
            .append_stage_batch(&third, &plan.writer_fence, &context),
        Err(SemanticVectorStagingStoreError::InvalidRequest(_))
    ));
}

#[test]
fn cross_binding_reads_and_writes_are_denied_and_busy_is_preserved() {
    let fixture = Fixture::new();
    let plan = plan(&fixture, "binding", chunk_manifest("chunk.fixture"));
    let receipt = receipt(&plan.key);
    let (control, probe) = operation("begin.binding");
    let context = GraphPublicationOperationContextV1::new(&control, &probe).unwrap();
    fixture.storage().begin_stage(&plan, &context).unwrap();

    let mut wrong_fence = plan.writer_fence.clone();
    wrong_fence.binding.incarnation = StoreIncarnationV1::new(4).unwrap();
    let (control, probe) = operation("write.binding");
    let context = GraphPublicationOperationContextV1::new(&control, &probe).unwrap();
    assert!(matches!(
        fixture
            .storage()
            .append_stage_batch(&receipt, &wrong_fence, &context),
        Err(SemanticVectorStagingStoreError::InvalidRequest(_))
    ));

    let mut wrong_key = plan.key.clone();
    wrong_key.projection.shard_id = StoreShardIdV1::project(
        BrainId::new("brain.fixture").unwrap(),
        UserProfileId::new("profile.fixture").unwrap(),
        ProjectId::new("project.other").unwrap(),
    );
    let (control, probe) = operation("read.binding");
    let context = GraphPublicationOperationContextV1::new(&control, &probe).unwrap();
    assert!(matches!(
        fixture.storage().stage(&wrong_key, &context),
        Err(SemanticVectorStagingStoreError::InvalidRequest(_))
    ));
    assert_eq!(
        super::support::map_exact(ExactSqlError::Busy),
        SemanticVectorStagingStoreError::Busy
    );
}

#[test]
fn canonical_digests_reject_changed_fields_and_generation_size_is_bounded() {
    let fixture = Fixture::new();
    let plan = plan(&fixture, "digest-binding", chunk_manifest("chunk.fixture"));
    let exact_plan = self::plan(&fixture, "digest-binding", chunk_manifest("chunk.fixture"));
    assert_eq!(plan.key.plan_digest, exact_plan.key.plan_digest);

    let mut changed_plan = plan.clone();
    changed_plan.source_generation = SemanticVectorSourceGenerationId::new("code.changed").unwrap();
    assert!(changed_plan.validate().is_err());

    let receipt = receipt(&plan.key);
    assert_eq!(receipt, self::receipt(&plan.key));
    let mut changed_receipt = receipt;
    changed_receipt.output_digest = digest::<SemanticVectorBatchOutputDigest>('7');
    assert!(changed_receipt.validate().is_err());

    let mut over_cap = plan;
    over_cap.expected_chunk_count = MAX_SEMANTIC_VECTOR_STAGE_CHUNKS + 1;
    assert!(over_cap.validate().is_err());
    let mut invalid_dimension = exact_plan;
    invalid_dimension.recipe.embedding_dimension = 4_097;
    assert!(invalid_dimension.validate().is_err());
}

#[test]
fn empty_generation_uses_one_control_batch_and_atomically_prepares_replay() {
    let fixture = Fixture::new();
    let empty_manifest = semantic_vector_chunk_manifest_digest(&[]).unwrap();
    let plan = plan_with_count(&fixture, "empty-generation", empty_manifest, 0);
    let control_receipt = control_receipt(&plan.key);
    let (control, probe) = operation("begin.empty");
    let context = GraphPublicationOperationContextV1::new(&control, &probe).unwrap();
    fixture.storage().begin_stage(&plan, &context).unwrap();
    let (control, probe) = operation("append.empty");
    let context = GraphPublicationOperationContextV1::new(&control, &probe).unwrap();
    fixture
        .storage()
        .append_stage_batch(&control_receipt, &plan.writer_fence, &context)
        .unwrap();
    assert!(
        SemanticVectorStageBatchReceipt::new(
            SemanticVectorStageBatchKey {
                stage: plan.key.clone(),
                ordinal: 1,
            },
            control_receipt.checkpoint_digest.clone(),
            digest('a'),
            digest('b'),
            digest('c'),
            vec![],
        )
        .is_err()
    );
    let settlement = SemanticVectorStageSettlement {
        batch: control_receipt.key.clone(),
        expected_receipt_digest: control_receipt.receipt_digest.clone(),
        terminal: SemanticVectorStageEffectTerminal::Applied {
            graph_batch_digest: digest('a'),
        },
    };
    let (control, probe) = operation("settle.empty");
    let context = GraphPublicationOperationContextV1::new(&control, &probe).unwrap();
    fixture
        .storage()
        .settle_stage_batch(&settlement, &plan.writer_fence, &context)
        .unwrap();
    let prepare = SemanticVectorStagePublicationPrepareRequest::new(
        plan.key.clone(),
        publication_replay(&plan),
        control_receipt.checkpoint_digest.clone(),
    )
    .unwrap();
    let (control, probe) = operation("prepare.empty");
    let context = GraphPublicationOperationContextV1::new(&control, &probe).unwrap();
    assert!(matches!(
        fixture
            .storage()
            .prepare_stage_publication(&prepare, &plan.writer_fence, &context)
            .unwrap(),
        SemanticVectorStagePublicationPrepareOutcome::ReadyToPublish(_)
    ));
    let (control, probe) = operation("prepare.empty.replay");
    let context = GraphPublicationOperationContextV1::new(&control, &probe).unwrap();
    assert!(matches!(
        fixture
            .storage()
            .prepare_stage_publication(&prepare, &plan.writer_fence, &context)
            .unwrap(),
        SemanticVectorStagePublicationPrepareOutcome::ExactReplay(_)
    ));

    let replay = prepare.publication_replay.clone();
    let head_request = GraphVerifiedHeadCompareAndSwapV1 {
        publication_key: replay.key.clone(),
        input_digest: replay.input_digest.clone(),
        dependency_generation_closure_digest: replay.dependency_generation_closure_digest.clone(),
        recovered_digest: replay.expected_recovered_digest.clone(),
        expected_prior_head: replay.expected_prior_head.clone(),
    };
    let (control, probe) = operation("publish.empty.head");
    let context = GraphPublicationOperationContextV1::new(&control, &probe).unwrap();
    let verified_head = match fixture
        .storage()
        .compare_and_swap_verified_head(&head_request, &context)
        .unwrap()
    {
        GraphVerifiedHeadCasOutcomeV1::Advanced(head) => head,
        outcome => panic!("unexpected empty publication head outcome: {outcome:?}"),
    };
    let publish = SemanticVectorStagePublishSettlement {
        stage: plan.key.clone(),
        verified_head,
    };
    let (control, probe) = operation("publish.empty.settle");
    let context = GraphPublicationOperationContextV1::new(&control, &probe).unwrap();
    assert!(matches!(
        fixture
            .storage()
            .settle_published(&publish, &plan.writer_fence, &context)
            .unwrap(),
        SemanticVectorStagePublishOutcome::Published(_)
    ));
    let (control, probe) = operation("publish.empty.replay");
    let context = GraphPublicationOperationContextV1::new(&control, &probe).unwrap();
    assert!(matches!(
        fixture
            .storage()
            .settle_published(&publish, &plan.writer_fence, &context)
            .unwrap(),
        SemanticVectorStagePublishOutcome::ExactReplay(_)
    ));
}

#[test]
fn pending_stage_adopts_restarted_writer_by_exact_cas_and_replays_response_loss() {
    let fixture = Fixture::new();
    let plan = plan(&fixture, "writer-adoption", chunk_manifest("chunk.fixture"));
    let (control, probe) = operation("begin.adoption");
    let context = GraphPublicationOperationContextV1::new(&control, &probe).unwrap();
    fixture.storage().begin_stage(&plan, &context).unwrap();

    let mut previous_plan = plan.clone();
    previous_plan.writer_fence.binding.authority_epoch =
        StoreAuthorityEpochV1::new(u64::from(fixture.binding.authority_epoch) - 1).unwrap();
    fixture
        .handle
        .execute(
            ExactSqlStatement::new(
                "UPDATE semantic_vector_stages SET writer_binding=?1,plan_json=?2
                 WHERE plan_digest=?3"
                    .to_owned(),
                vec![
                    ExactSqlValue::Text(
                        serde_json::to_string(&previous_plan.writer_fence.binding).unwrap(),
                    ),
                    ExactSqlValue::Text(serde_json::to_string(&previous_plan).unwrap()),
                    ExactSqlValue::Text(plan.key.plan_digest.as_str().to_owned()),
                ],
            )
            .unwrap(),
        )
        .unwrap();
    let request = SemanticVectorStageWriterAdoption {
        stage: plan.key.clone(),
        expected: previous_plan.writer_fence.clone(),
        replacement: plan.writer_fence.clone(),
        ready_publication_replay: None,
    };
    let (control, probe) = operation("adopt.writer");
    let context = GraphPublicationOperationContextV1::new(&control, &probe).unwrap();
    assert!(matches!(
        fixture
            .storage()
            .adopt_stage_writer(&request, &context)
            .unwrap(),
        SemanticVectorStageWriterAdoptionOutcome::Adopted(_)
    ));
    let (control, probe) = operation("adopt.writer.replay");
    let context = GraphPublicationOperationContextV1::new(&control, &probe).unwrap();
    assert!(matches!(
        fixture
            .storage()
            .adopt_stage_writer(&request, &context)
            .unwrap(),
        SemanticVectorStageWriterAdoptionOutcome::ExactReplay(_)
    ));
    let receipt = receipt(&plan.key);
    let (control, probe) = operation("stale.writer");
    let context = GraphPublicationOperationContextV1::new(&control, &probe).unwrap();
    assert!(matches!(
        fixture
            .storage()
            .append_stage_batch(&receipt, &previous_plan.writer_fence, &context),
        Err(SemanticVectorStagingStoreError::InvalidRequest(_))
    ));
}

#[test]
fn normalized_stage_batch_and_chunk_tampering_is_corruption() {
    let fixture = Fixture::new();
    let plan = plan(
        &fixture,
        "normalized-tamper",
        chunk_manifest("chunk.fixture"),
    );
    let (control, probe) = operation("begin.tamper");
    let context = GraphPublicationOperationContextV1::new(&control, &probe).unwrap();
    fixture.storage().begin_stage(&plan, &context).unwrap();
    let receipt = receipt(&plan.key);
    let (control, probe) = operation("append.tamper");
    let context = GraphPublicationOperationContextV1::new(&control, &probe).unwrap();
    fixture
        .storage()
        .append_stage_batch(&receipt, &plan.writer_fence, &context)
        .unwrap();

    fixture
        .handle
        .execute(
            ExactSqlStatement::new(
                "UPDATE semantic_vector_stage_batches SET output_digest=?1".to_owned(),
                vec![ExactSqlValue::Text(
                    digest::<SemanticVectorBatchOutputDigest>('7')
                        .as_str()
                        .to_owned(),
                )],
            )
            .unwrap(),
        )
        .unwrap();
    let (control, probe) = operation("read.batch.tamper");
    let context = GraphPublicationOperationContextV1::new(&control, &probe).unwrap();
    assert!(matches!(
        fixture.storage().batch_receipt(&receipt.key, &context),
        Err(SemanticVectorStagingStoreError::Corrupt(_))
    ));

    fixture
        .handle
        .execute(
            ExactSqlStatement::new(
                "UPDATE semantic_vector_stage_batches SET output_digest=?1".to_owned(),
                vec![ExactSqlValue::Text(
                    receipt.output_digest.as_str().to_owned(),
                )],
            )
            .unwrap(),
        )
        .unwrap();
    fixture
        .handle
        .execute(
            ExactSqlStatement::new(
                "UPDATE semantic_vector_stage_chunk_receipts SET chunk_digest=?1".to_owned(),
                vec![ExactSqlValue::Text(
                    digest::<SemanticVectorChunkDigest>('7').as_str().to_owned(),
                )],
            )
            .unwrap(),
        )
        .unwrap();
    let (control, probe) = operation("read.chunk.tamper");
    let context = GraphPublicationOperationContextV1::new(&control, &probe).unwrap();
    assert!(matches!(
        fixture.storage().batch_receipt(&receipt.key, &context),
        Err(SemanticVectorStagingStoreError::Corrupt(_))
    ));

    fixture
        .handle
        .execute(
            ExactSqlStatement::new(
                "UPDATE semantic_vector_stages SET source_generation='source.changed'".to_owned(),
                vec![],
            )
            .unwrap(),
        )
        .unwrap();
    let (control, probe) = operation("read.stage.tamper");
    let context = GraphPublicationOperationContextV1::new(&control, &probe).unwrap();
    assert!(matches!(
        fixture.storage().stage(&plan.key, &context),
        Err(SemanticVectorStagingStoreError::Corrupt(_))
    ));
}

fn digest<T: TryFrom<String>>(byte: char) -> T
where
    T::Error: std::fmt::Debug,
{
    T::try_from(format!("sha256:{}", byte.to_string().repeat(64))).unwrap()
}

fn plan(
    fixture: &Fixture,
    name: &str,
    manifest: SemanticVectorChunkManifestDigest,
) -> SemanticVectorStagePlan {
    plan_with_count(fixture, name, manifest, 1)
}

fn plan_with_count(
    fixture: &Fixture,
    name: &str,
    manifest: SemanticVectorChunkManifestDigest,
    expected_chunk_count: u64,
) -> SemanticVectorStagePlan {
    let projection = GraphProjectionIdentityV1 {
        shard_id: fixture.binding.shard_id.clone(),
        namespace: GraphNamespaceV1::new("semantic-code").unwrap(),
        projection: GraphProjectionIdV1::new(name).unwrap(),
    };
    SemanticVectorStagePlan::new(
        projection.clone(),
        SemanticVectorBuildId::new(format!("build.{name}")).unwrap(),
        VectorGenerationIdV1::new(
            canonical_sha256(&("semantic-vector-test-generation", name)).unwrap(),
        ),
        None,
        GraphPublicationKeyV1::new(
            projection.clone(),
            GraphGenerationIdV1::new(format!("generation.{name}")).unwrap(),
            GraphPublicationIdempotencyKeyV1::new(format!("publication.{name}")).unwrap(),
        ),
        StoreShardIdV1::code(
            BrainId::new("brain.fixture").unwrap(),
            UserProfileId::new("profile.fixture").unwrap(),
            ProjectId::new("project.fixture").unwrap(),
            RepositoryId::new("repository.fixture").unwrap(),
            CodeShardScopeV1::Worktree {
                worktree_id: WorktreeId::new("worktree.fixture").unwrap(),
            },
        ),
        tracedecay_store::SemanticVectorCodeScopeHash::new("a".repeat(64)).unwrap(),
        SemanticVectorSourceGenerationId::new("code.generation").unwrap(),
        SemanticVectorSourceDependencyV1 {
            generation: GraphDependencyGenerationIdentityV1::new(
                GraphProjectionIdentityV1 {
                    shard_id: projection.shard_id.clone(),
                    namespace: GraphNamespaceV1::new("code-source").unwrap(),
                    projection: GraphProjectionIdV1::new("code-projection").unwrap(),
                },
                GraphGenerationIdV1::new("code-graph-generation").unwrap(),
            ),
            idempotency_key: GraphPublicationIdempotencyKeyV1::new("code-graph-publication")
                .unwrap(),
        },
        SemanticVectorReconstructionRecipe {
            source_manifest_digest: digest::<SemanticVectorSourceManifestDigest>('2'),
            embedding_projection_digest: digest::<SemanticEmbeddingProjectionDigestV1>('3'),
            embedding_dimension: 384,
            model_artifact_digest: digest::<SemanticModelArtifactDigestV1>('4'),
            projection_manifest_digest: digest::<SemanticProjectionManifestDigestV1>('5'),
            privacy_domain_digest: digest::<SemanticPrivacyDomainDigestV1>('6'),
            privacy_key_epoch: 7,
            expected_chunk_manifest_digest: manifest,
        },
        expected_chunk_count,
        None,
        digest('9'),
        SemanticVectorWriterFence {
            binding: fixture.binding.clone(),
        },
    )
    .unwrap()
}

fn alternative_publication_plan(
    original: &SemanticVectorStagePlan,
    build: &str,
    generation: &str,
    idempotency: &str,
) -> SemanticVectorStagePlan {
    SemanticVectorStagePlan::new(
        original.key.projection.clone(),
        SemanticVectorBuildId::new(format!("build.{build}")).unwrap(),
        original.semantic_generation_id.clone(),
        original.base_generation.clone(),
        GraphPublicationKeyV1::new(
            original.key.projection.clone(),
            GraphGenerationIdV1::new(generation).unwrap(),
            GraphPublicationIdempotencyKeyV1::new(idempotency).unwrap(),
        ),
        original.source_scope.clone(),
        original.code_scope_hash.clone(),
        original.source_generation.clone(),
        original.source_dependency.clone(),
        original.recipe.clone(),
        original.expected_chunk_count,
        original.expected_prior_verified_head.clone(),
        original.initial_checkpoint_digest.clone(),
        original.writer_fence.clone(),
    )
    .unwrap()
}

fn receipt(stage: &SemanticVectorStageKey) -> SemanticVectorStageBatchReceipt {
    receipt_at(stage, 0, digest('9'), "chunk.fixture")
}

fn control_receipt(stage: &SemanticVectorStageKey) -> SemanticVectorStageBatchReceipt {
    SemanticVectorStageBatchReceipt::new(
        SemanticVectorStageBatchKey {
            stage: stage.clone(),
            ordinal: 0,
        },
        digest('9'),
        digest::<SemanticVectorBatchInputDigest>('a'),
        digest::<SemanticVectorBatchOutputDigest>('b'),
        digest('d'),
        vec![],
    )
    .unwrap()
}

fn receipt_at(
    stage: &SemanticVectorStageKey,
    ordinal: u64,
    expected_checkpoint_digest: SemanticVectorCheckpointDigest,
    chunk_id: &str,
) -> SemanticVectorStageBatchReceipt {
    SemanticVectorStageBatchReceipt::new(
        SemanticVectorStageBatchKey {
            stage: stage.clone(),
            ordinal,
        },
        expected_checkpoint_digest,
        digest::<SemanticVectorBatchInputDigest>('a'),
        digest::<SemanticVectorBatchOutputDigest>('b'),
        digest('d'),
        vec![SemanticVectorStageChunkReceipt {
            effect_ordinal: 0,
            chunk_id: SemanticVectorChunkId::new(chunk_id).unwrap(),
            chunk_digest: digest::<SemanticVectorChunkDigest>('e'),
            operation: SemanticVectorStageChunkOperation::Embed,
            output_digest: Some(digest::<SemanticVectorOutputDigest>('f')),
        }],
    )
    .unwrap()
}

fn chunk_manifest(chunk_id: &str) -> SemanticVectorChunkManifestDigest {
    semantic_vector_chunk_manifest_digest(&[SemanticVectorChunkManifestMember {
        chunk_id: SemanticVectorChunkId::new(chunk_id).unwrap(),
        chunk_digest: digest::<SemanticVectorChunkDigest>('e'),
        operation: SemanticVectorStageChunkOperation::Embed,
    }])
    .unwrap()
}

fn publication_replay(plan: &SemanticVectorStagePlan) -> GraphPublicationReplayV1 {
    GraphPublicationReplayV1::new(
        plan.publication_key.clone(),
        digest::<GraphPublicationInputDigestV1>('1'),
        digest::<GraphDependencyGenerationClosureDigestV1>('2'),
        vec![],
        plan.expected_prior_verified_head.clone(),
        digest::<GraphRecoveredGenerationDigestV1>('3'),
        vec![1_u8],
    )
    .unwrap()
}

#[test]
fn production_exact_store_replays_receipts_and_rejects_revoked_writer() {
    let fixture = Fixture::new();
    let plan = plan(&fixture, "authority", chunk_manifest("chunk.fixture"));
    let receipt = receipt(&plan.key);
    let (control, probe) = operation("begin");
    let context = GraphPublicationOperationContextV1::new(&control, &probe).unwrap();
    assert!(matches!(
        fixture.storage().begin_stage(&plan, &context).unwrap(),
        SemanticVectorStageBeginOutcome::Begun(_)
    ));
    let (control, probe) = operation("append");
    let context = GraphPublicationOperationContextV1::new(&control, &probe).unwrap();
    fixture
        .storage()
        .append_stage_batch(&receipt, &plan.writer_fence, &context)
        .unwrap();
    let (control, probe) = operation("append.replay");
    let context = GraphPublicationOperationContextV1::new(&control, &probe).unwrap();
    assert!(matches!(
        fixture
            .storage()
            .append_stage_batch(&receipt, &plan.writer_fence, &context)
            .unwrap(),
        SemanticVectorStageAppendOutcome::ExactReplay { .. }
    ));
    fixture.allowed.store(false, Ordering::Release);
    let (control, probe) = operation("revoked");
    let context = GraphPublicationOperationContextV1::new(&control, &probe).unwrap();
    assert_eq!(
        fixture
            .storage()
            .append_stage_batch(&receipt, &plan.writer_fence, &context),
        Err(SemanticVectorStagingStoreError::AuthorityLost)
    );
}

#[test]
fn production_exact_store_advances_applied_frontier_in_order() {
    let fixture = Fixture::new();
    let plan = plan(&fixture, "settle", chunk_manifest("chunk.fixture"));
    let receipt = receipt(&plan.key);
    let (control, probe) = operation("begin.settle");
    let context = GraphPublicationOperationContextV1::new(&control, &probe).unwrap();
    fixture.storage().begin_stage(&plan, &context).unwrap();
    let (control, probe) = operation("append.settle");
    let context = GraphPublicationOperationContextV1::new(&control, &probe).unwrap();
    assert!(matches!(
        fixture
            .storage()
            .append_stage_batch(&receipt, &plan.writer_fence, &context)
            .unwrap(),
        SemanticVectorStageAppendOutcome::Appended { .. }
    ));
    let (control, probe) = operation("settle");
    let context = GraphPublicationOperationContextV1::new(&control, &probe).unwrap();
    let settlement = SemanticVectorStageSettlement {
        batch: receipt.key,
        expected_receipt_digest: receipt.receipt_digest,
        terminal: SemanticVectorStageEffectTerminal::Applied {
            graph_batch_digest: digest::<SemanticVectorGraphBatchDigest>('a'),
        },
    };
    assert!(matches!(
        fixture
            .storage()
            .settle_stage_batch(&settlement, &plan.writer_fence, &context,)
            .unwrap(),
        SemanticVectorStageSettlementOutcome::Settled(_)
    ));
    let (control, probe) = operation("settle.replay");
    let context = GraphPublicationOperationContextV1::new(&control, &probe).unwrap();
    assert!(matches!(
        fixture
            .storage()
            .settle_stage_batch(&settlement, &plan.writer_fence, &context)
            .unwrap(),
        SemanticVectorStageSettlementOutcome::ExactReplay(_)
    ));
}

#[path = "published_generation_tests.rs"]
mod published_generation_tests;
