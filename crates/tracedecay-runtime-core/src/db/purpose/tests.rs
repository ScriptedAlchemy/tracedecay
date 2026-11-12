use std::num::NonZeroU64;
use std::sync::Arc;

use sha2::{Digest, Sha256};
use tracedecay_domain::{CodeGenerationId, ManifestDigest, ProjectId, WorktreeId};
use tracedecay_rusqlite_runtime::repository::CodeIndexSqliteStorage;
use tracedecay_store::runtime::{
    CODE_INDEX_REGION_BYTES, CodeIndexGenerationKey, CodeIndexPayloadIdentity,
    CodeIndexPublicationStage, CodeIndexPublishOutcome, CodeIndexStorageError, StoreOperationIdV1,
};
use tracedecay_store::{
    GraphPendingReplayDiscardV1, GraphPublicationKeyV1, GraphPublicationOperationContextV1,
    GraphPublicationReplayV1, GraphPublicationStoreV1, GraphReplayAppendOutcomeV1,
    RuntimeCancellationIdentityV1, RuntimeDeadlineV1, RuntimeRequestControlV1, RuntimeRequestProbeV1,
};

use crate::db::{
    Database, DatabaseAuthority, DatabaseOwnerV1, TestDatabaseRuntimeMode, TestDatabaseRuntimeScope,
};
use crate::resident_memory::{
    ProcessResidentMemoryV1, ResidentMemoryComponentIdV1, ResidentMemoryKeyV1,
    ResidentMemoryPressureV1,
};

fn ledger(bytes: u64) -> Arc<ProcessResidentMemoryV1> {
    let limit = NonZeroU64::new(bytes).unwrap();
    Arc::new(ProcessResidentMemoryV1::with_pressure(
        limit,
        Arc::new(ResidentMemoryPressureV1::with_sampler(
            limit,
            Arc::new(|| None),
        )),
    ))
}

fn key(project: &ProjectId, worktree: &str, generation: &str) -> ResidentMemoryKeyV1 {
    ResidentMemoryKeyV1 {
        project_id: project.clone(),
        worktree_id: WorktreeId::new(worktree).unwrap(),
        generation_id: CodeGenerationId::new(generation).unwrap(),
        component: ResidentMemoryComponentIdV1::new("code-index-storage").unwrap(),
    }
}

async fn project_database(
    path: &std::path::Path,
    project: &ProjectId,
) -> (DatabaseOwnerV1, Database) {
    let authority = DatabaseAuthority::acquire_test(path, "registered code-index SQL test").unwrap();
    let fixture = Database::publish_registered_test_runtime_with_retirement_control(
        path,
        &authority,
        TestDatabaseRuntimeMode::Initialize,
        TestDatabaseRuntimeScope::Project {
            project_id: project.clone(),
        },
    )
    .await
    .unwrap();
    let (owner, runtime, _control) = fixture.into_parts();
    drop(runtime);
    let database = owner.issue_lease().unwrap();
    (owner, database)
}

fn stage(worktree: &str, generation: &str, expected: Option<&[u8]>) -> CodeIndexPublicationStage {
    CodeIndexPublicationStage {
        operation_id: StoreOperationIdV1::new(format!("publish-{worktree}-{generation}")).unwrap(),
        generation: CodeIndexGenerationKey {
            worktree_id: WorktreeId::new(worktree).unwrap(),
            generation_id: CodeGenerationId::new(generation).unwrap(),
        },
        expected_head: expected.map(|expected| {
            Arc::new(super::reserve_encoded_bytes(&ledger(1024 * 1024), expected.len(), |output| {
                output.copy_from_slice(expected);
                Ok(())
            }).unwrap())
        }),
    }
}

fn payload(bytes: &[u8]) -> CodeIndexPayloadIdentity {
    CodeIndexPayloadIdentity {
        digest: ManifestDigest::from_sha256_bytes(&Sha256::digest(bytes)).unwrap(),
        size_bytes: bytes.len() as u64,
    }
}

struct GraphCommitProbe {
    cancellation: RuntimeCancellationIdentityV1,
    deadline: RuntimeDeadlineV1,
    committed: std::sync::atomic::AtomicBool,
}

impl RuntimeRequestProbeV1 for GraphCommitProbe {
    fn cancellation_identity(&self) -> &RuntimeCancellationIdentityV1 { &self.cancellation }
    fn deadline_identity(&self) -> &RuntimeDeadlineV1 { &self.deadline }
    fn interruption(&self) -> Option<tracedecay_store::RuntimeInterruptionV1> { None }
    fn try_begin_commit(&self) -> bool {
        !self.committed.swap(true, std::sync::atomic::Ordering::AcqRel)
    }
}

fn graph_context(name: &str) -> (RuntimeRequestControlV1, GraphCommitProbe) {
    let cancellation = RuntimeCancellationIdentityV1 {
        cancellation_id: tracedecay_store::RuntimeCancellationIdV1::new(name).unwrap(),
        generation: 1,
    };
    let deadline = RuntimeDeadlineV1 {
        deadline_id: tracedecay_store::RuntimeDeadlineIdV1::new(name).unwrap(),
    };
    (
        RuntimeRequestControlV1 {
            requested_at: tracedecay_domain::UtcMicros(1),
            cancellation: cancellation.clone(),
            deadline: deadline.clone(),
        },
        GraphCommitProbe {
            cancellation, deadline, committed: std::sync::atomic::AtomicBool::new(false),
        },
    )
}

fn put(
    storage: &CodeIndexSqliteStorage,
    stage: &CodeIndexPublicationStage,
    bytes: &[u8],
) -> CodeIndexPayloadIdentity {
    let payload = payload(bytes);
    storage.stage_payload(&stage.operation_id, &payload).unwrap();
    for (ordinal, region) in bytes.chunks(CODE_INDEX_REGION_BYTES).enumerate() {
        storage
            .put_region(&stage.operation_id, &payload, ordinal as u64, region)
            .unwrap();
    }
    storage.seal_payload(&stage.operation_id, &payload).unwrap();
    payload
}

#[asupersync::test]
async fn registered_sql_regions_share_exact_bytes_and_cross_region_reads() {
    let directory = tempfile::tempdir().unwrap();
    let project = ProjectId::new("project-shared-sql").unwrap();
    let (_owner, database) = project_database(&directory.path().join("project.db"), &project).await;
    let memory = ledger(16 * 1024 * 1024);
    let storage = database
        .code_index_storage(Arc::clone(&memory), key(&project, "main", "first"))
        .unwrap();
    let read_key = key(&project, "main", "first");
    let sibling = database
        .code_index_storage(Arc::clone(&memory), key(&project, "linked", "first"))
        .unwrap();
    crate::native_runtime::spawn_blocking(move || {
        let bytes = (0..CODE_INDEX_REGION_BYTES * 2 + 31)
            .map(|offset| (offset % 251) as u8)
            .collect::<Vec<_>>();
        let first = stage("main", "first", None);
        storage.begin_stage(&first).unwrap();
        let encoded = put(&storage, &first, &bytes);
        let manifest = put(&storage, &first, b"exact manifest bytes");
        assert_eq!(
            storage.publish(&first, &manifest, b"head-main").unwrap(),
            CodeIndexPublishOutcome::Published
        );
        assert_eq!(
            storage.publish(&first, &manifest, b"head-main").unwrap(),
            CodeIndexPublishOutcome::AlreadyPublished
        );
        let second = stage("linked", "first", None);
        sibling.begin_stage(&second).unwrap();
        assert_eq!(put(&sibling, &second, &bytes), encoded);
        let second_manifest = put(&sibling, &second, b"sibling manifest bytes");
        sibling.publish(&second, &second_manifest, b"head-linked").unwrap();

        let range = storage
            .read_range(&first.generation, &encoded, CODE_INDEX_REGION_BYTES as u64 - 7, 29)
            .unwrap();
        assert_eq!(
            range.as_bytes(),
            &bytes[CODE_INDEX_REGION_BYTES - 7..CODE_INDEX_REGION_BYTES + 22]
        );
        assert!(memory.snapshot().used_bytes > 0);
        assert!(memory.snapshot().charges.iter().any(|charge| charge.key == read_key));
        drop(range);
        assert_eq!(memory.snapshot().used_bytes, 0);
        let whole = storage
            .read_range(&first.generation, &encoded, 0, encoded.size_bytes)
            .unwrap();
        assert_eq!(whole.as_bytes(), bytes);
        assert!(matches!(
            storage.read_range(&first.generation, &encoded, u64::MAX, 1),
            Err(CodeIndexStorageError::InvalidRequest(_))
        ));
        assert!(matches!(
            storage.read_range(&stage("foreign", "first", None).generation, &encoded, 0, 1),
            Err(CodeIndexStorageError::Corrupt(_))
        ));
        encoded.digest
    })
    .await
    .map(|digest| {
        assert!(!digest.as_str().is_empty());
    })
    .unwrap();
    assert_eq!(
        database
            .query_scalar_i64(
                "count shared regions",
                "SELECT COUNT(*) FROM code_index_regions
                 WHERE digest = (SELECT digest FROM code_index_payloads WHERE size_bytes = 2097183)",
            )
            .await
            .unwrap(),
        3
    );
    assert_eq!(
        database
            .query_scalar_i64(
                "count shared payload identity",
                "SELECT COUNT(*) FROM code_index_payloads WHERE size_bytes = 2097183",
            )
            .await
            .unwrap(),
        1
    );
}

#[asupersync::test]
async fn registered_sql_publication_refuses_incomplete_data_and_stale_exact_head() {
    let directory = tempfile::tempdir().unwrap();
    let project = ProjectId::new("project-publication-cas").unwrap();
    let (_owner, database) = project_database(&directory.path().join("project.db"), &project).await;
    let storage = database
        .code_index_storage(ledger(16 * 1024 * 1024), key(&project, "main", "first"))
        .unwrap();
    crate::native_runtime::spawn_blocking(move || {
        let first = stage("main", "first", None);
        storage.begin_stage(&first).unwrap();
        let manifest = put(&storage, &first, b"manifest");
        let incomplete = payload(b"unwritten");
        storage.stage_payload(&first.operation_id, &incomplete).unwrap();
        assert!(matches!(
            storage.publish(&first, &manifest, b"first-head"),
            Err(CodeIndexStorageError::Corrupt(_))
        ));
        assert!(matches!(
            storage.seal_payload(&first.operation_id, &incomplete),
            Err(CodeIndexStorageError::Corrupt(_))
        ));
        storage.abandon_stage(&first.operation_id).unwrap();
        storage.begin_stage(&first).unwrap();
        put(&storage, &first, b"manifest");
        storage.publish(&first, &manifest, b"first-head").unwrap();
        let stale = stage("main", "stale", Some(b"incorrect-head"));
        storage.begin_stage(&stale).unwrap();
        put(&storage, &stale, b"manifest");
        assert_eq!(
            storage.publish(&stale, &manifest, b"stale-head").unwrap(),
            CodeIndexPublishOutcome::HeadConflict
        );
        assert!(matches!(
            storage.read_range(&stale.generation, &manifest, 0, 1),
            Err(CodeIndexStorageError::Corrupt(_))
        ));
        storage.abandon_stage(&stale.operation_id).unwrap();
    })
    .await
    .unwrap();
    assert_eq!(
        database
            .query_scalar_i64("count publication", "SELECT COUNT(*) FROM code_index_generations")
            .await
            .unwrap(),
        1
    );
    assert_eq!(
        database
            .query_scalar_i64("count staging", "SELECT COUNT(*) FROM code_index_staging")
            .await
            .unwrap(),
        0
    );
}

#[asupersync::test]
async fn registered_sql_allocation_denial_precedes_materialization_and_wrong_project_mount() {
    let directory = tempfile::tempdir().unwrap();
    let project = ProjectId::new("project-allocation").unwrap();
    let (_owner, database) = project_database(&directory.path().join("project.db"), &project).await;
    assert!(
        database
            .code_index_storage(
                ledger(16 * 1024 * 1024),
                key(&ProjectId::new("other-project").unwrap(), "main", "first"),
            )
            .is_err()
    );
    let storage = database
        .code_index_storage(ledger(16 * 1024 * 1024), key(&project, "main", "first"))
        .unwrap();
    let small = ledger(8192);
    let denied = database
        .code_index_storage(Arc::clone(&small), key(&project, "main", "first"))
        .unwrap();
    crate::native_runtime::spawn_blocking(move || {
        let first = stage("main", "first", None);
        storage.begin_stage(&first).unwrap();
        let manifest = put(&storage, &first, b"manifest");
        storage.publish(&first, &manifest, b"head").unwrap();
        assert!(matches!(
            denied.read_range(&first.generation, &manifest, 0, 1),
            Err(CodeIndexStorageError::AllocationRefused(_))
        ));
        assert_eq!(small.snapshot().used_bytes, 0);
        let conflicting = stage("main", "second", Some(b"head"));
        storage.begin_stage(&conflicting).unwrap();
        storage.stage_payload(&conflicting.operation_id, &manifest).unwrap();
        assert!(matches!(
            storage.put_region(&conflicting.operation_id, &manifest, 0, b"tampered"),
            Err(CodeIndexStorageError::Corrupt(_))
        ));
        storage.abandon_stage(&conflicting.operation_id).unwrap();
    })
    .await
    .unwrap();
}

#[asupersync::test]
async fn registered_sql_retirement_preserves_sibling_and_graph_replay_references() {
    let directory = tempfile::tempdir().unwrap();
    let project = ProjectId::new("project-retirement").unwrap();
    let (_owner, database) = project_database(&directory.path().join("project.db"), &project).await;
    let storage = database
        .code_index_storage(ledger(16 * 1024 * 1024), key(&project, "main", "first"))
        .unwrap();
    let mut graph = database.graph_publication_storage().unwrap();
    crate::native_runtime::spawn_blocking(move || {
        let first = stage("main", "first", None);
        storage.begin_stage(&first).unwrap();
        let shared = put(&storage, &first, b"shared exact encoded payload");
        let first_manifest = put(&storage, &first, b"first manifest");
        storage.publish(&first, &first_manifest, b"first-head").unwrap();
        let retirement = StoreOperationIdV1::new("retire-first").unwrap();
        assert!(matches!(
            storage.prepare_retirement(&first.generation, &retirement, &first_manifest.digest,
                b"retirement-receipt", b"first-head", b"remaining-head"),
            Err(CodeIndexStorageError::Retained)
        ));
        let sibling = stage("linked", "first", None);
        storage.begin_stage(&sibling).unwrap();
        assert_eq!(put(&storage, &sibling, b"shared exact encoded payload"), shared);
        let sibling_manifest = put(&storage, &sibling, b"sibling manifest");
        storage.publish(&sibling, &sibling_manifest, b"sibling-head").unwrap();

        let source = tracedecay_graph_db::GraphGenerationReplaySource::SealedCodeGeneration(
            tracedecay_graph_db::SealedCodeGenerationReplay {
                repository: tracedecay_domain::RepositoryId::new("repository-retirement").unwrap(),
                generation: first.generation.generation_id.clone(),
                sealed_state_digest: tracedecay_graph_db::SealedGraphStateDigest::try_from(
                    first_manifest.digest.as_str().to_owned(),
                ).unwrap(),
                projector_revision: tracedecay_graph_db::GraphProjectorRevision::try_from(
                    "projector-retirement".to_owned(),
                ).unwrap(),
            },
        );
        let digest = payload(b"graph journal identity").digest;
        let replay = GraphPublicationReplayV1::new(
            GraphPublicationKeyV1::new(
                tracedecay_store::GraphProjectionIdentityV1 {
                    shard_id: storage.binding().shard_id.clone(),
                    namespace: tracedecay_store::GraphNamespaceV1::new("code").unwrap(),
                    projection: tracedecay_store::GraphProjectionIdV1::new("first").unwrap(),
                },
                tracedecay_store::GraphGenerationIdV1::new("first").unwrap(),
                tracedecay_store::GraphPublicationIdempotencyKeyV1::new("graph-first").unwrap(),
            ),
            tracedecay_store::GraphPublicationInputDigestV1::new(digest.as_str()).unwrap(),
            tracedecay_store::GraphDependencyGenerationClosureDigestV1::new(digest.as_str()).unwrap(),
            vec![], None,
            tracedecay_store::GraphRecoveredGenerationDigestV1::new(digest.as_str()).unwrap(),
            serde_json::to_vec(&source).unwrap(),
        ).unwrap();
        let (control, probe) = graph_context("append");
        let context = GraphPublicationOperationContextV1::new(&control, &probe).unwrap();
        let recorded = match graph.append_replay(&replay, &context).unwrap() {
            GraphReplayAppendOutcomeV1::Appended(recorded) => recorded,
            outcome => panic!("unexpected append {outcome:?}"),
        };
        let successor = stage("main", "second", Some(b"first-head"));
        storage.begin_stage(&successor).unwrap();
        let next_manifest = put(&storage, &successor, b"next manifest");
        storage.publish(&successor, &next_manifest, b"next-head").unwrap();
        storage.prepare_retirement(&first.generation, &retirement, &first_manifest.digest,
            b"retirement-receipt", b"next-head", b"remaining-head").unwrap();
        assert!(matches!(
            storage.complete_retirement(&first.generation, &retirement),
            Err(CodeIndexStorageError::Retained)
        ));
        let (control, probe) = graph_context("discard");
        let context = GraphPublicationOperationContextV1::new(&control, &probe).unwrap();
        graph.discard_pending_replay(&GraphPendingReplayDiscardV1 {
            key: replay.key, sequence: recorded.sequence,
        }, &context).unwrap();
        let pinned = storage.read_range(&first.generation, &shared, 0, shared.size_bytes).unwrap();
        storage.complete_retirement(&first.generation, &retirement).unwrap();
        assert_eq!(pinned.as_bytes(), b"shared exact encoded payload");
        storage.complete_retirement(&first.generation, &retirement).unwrap();
        while storage.collect_unreferenced_payload().unwrap() {}
        let bytes = storage
            .read_range(&sibling.generation, &shared, 0, shared.size_bytes)
            .unwrap();
        assert_eq!(bytes.as_bytes(), b"shared exact encoded payload");
        assert!(matches!(
            storage.read_range(&first.generation, &shared, 0, 1),
            Err(CodeIndexStorageError::Retired)
        ));
    })
    .await
    .unwrap();
    assert_eq!(
        database
            .query_scalar_i64("retirement receipts", "SELECT COUNT(*) FROM code_index_retirement_receipts")
            .await
            .unwrap(),
        1
    );
}

#[asupersync::test]
async fn registered_sql_evidence_pages_commit_the_unchanged_aggregate_stream() {
    let directory = tempfile::tempdir().unwrap();
    let project = ProjectId::new("project-evidence-stream").unwrap();
    let (_owner, database) = project_database(&directory.path().join("project.db"), &project).await;
    let storage = database
        .code_index_storage(ledger(16 * 1024 * 1024), key(&project, "main", "first"))
        .unwrap();
    crate::native_runtime::spawn_blocking(move || {
        let first = stage("main", "first", None);
        storage.begin_stage(&first).unwrap();
        let stream = (0..CODE_INDEX_REGION_BYTES * 2 + 43)
            .map(|offset| (offset % 239) as u8)
            .collect::<Vec<_>>();
        let mut pages = 0;
        for (ordinal, page) in stream.chunks(256 * 1024).enumerate() {
            let digest = payload(page).digest;
            storage
                .stage_evidence_page(&first.operation_id, ordinal as u64, &digest, page)
                .unwrap();
            pages += 1;
        }
        let evidence = payload(&stream);
        storage
            .commit_evidence_pages(&first.operation_id, &evidence, pages)
            .unwrap();
        let manifest = put(&storage, &first, b"evidence manifest");
        storage.publish(&first, &manifest, b"evidence-head").unwrap();
        let output = storage
            .read_range(&first.generation, &evidence, 0, evidence.size_bytes)
            .unwrap();
        assert_eq!(output.as_bytes(), stream);
    })
    .await
    .unwrap();
    assert_eq!(
        database
            .query_scalar_i64("staging pages", "SELECT COUNT(*) FROM code_index_staging_pages")
            .await
            .unwrap(),
        0
    );
}

#[asupersync::test]
async fn registered_sql_parallel_outputs_keep_independent_resident_charges() {
    let directory = tempfile::tempdir().unwrap();
    let project = ProjectId::new("project-concurrent-output").unwrap();
    let (_owner, database) = project_database(&directory.path().join("project.db"), &project).await;
    let memory = ledger(32 * 1024 * 1024);
    let storage = database
        .code_index_storage(Arc::clone(&memory), key(&project, "main", "first"))
        .unwrap();
    crate::native_runtime::spawn_blocking(move || {
        let first = stage("main", "first", None);
        storage.begin_stage(&first).unwrap();
        let data = put(&storage, &first, b"concurrent encoded bytes");
        let manifest = put(&storage, &first, b"manifest");
        storage.publish(&first, &manifest, b"head").unwrap();
        let one = storage.read_range(&first.generation, &data, 0, data.size_bytes).unwrap();
        let one_charge = memory.snapshot().used_bytes;
        assert!(one_charge >= data.size_bytes);
        let two = std::thread::scope(|scope| {
            scope.spawn(|| {
                storage.read_range(&first.generation, &data, 0, data.size_bytes).unwrap()
            }).join().unwrap()
        });
        assert_eq!(memory.snapshot().used_bytes, one_charge * 2);
        assert_eq!(one.as_bytes(), two.as_bytes());
        drop(one);
        assert_eq!(memory.snapshot().used_bytes, one_charge);
        let shared = Arc::new(two);
        let retained = Arc::clone(&shared);
        drop(shared);
        assert_eq!(memory.snapshot().used_bytes, one_charge);
        drop(retained);
        assert_eq!(memory.snapshot().used_bytes, 0);
        let pages = storage.generation_page(&first.generation.worktree_id, None).unwrap();
        assert_eq!(pages.records().len(), 1);
        assert_eq!(pages.records()[0].manifest, manifest);
        assert!(memory.snapshot().used_bytes > 0);
        drop(pages);
        assert_eq!(memory.snapshot().used_bytes, 0);
    }).await.unwrap();
}

#[asupersync::test]
async fn registered_sql_verification_is_region_bounded_and_cancellation_releases_charge() {
    let directory = tempfile::tempdir().unwrap();
    let project = ProjectId::new("project-bounded-authentication").unwrap();
    let (_owner, database) = project_database(&directory.path().join("project.db"), &project).await;
    let staging = database
        .code_index_storage(ledger(16 * 1024 * 1024), key(&project, "main", "first"))
        .unwrap();
    let memory = ledger(4 * 1024 * 1024);
    let verification = database
        .code_index_storage(Arc::clone(&memory), key(&project, "main", "first"))
        .unwrap();
    crate::native_runtime::spawn_blocking(move || {
        let first = stage("main", "first", None);
        staging.begin_stage(&first).unwrap();
        let source = vec![31; CODE_INDEX_REGION_BYTES * 9 + 17];
        let data = put(&staging, &first, &source);
        let manifest = put(&staging, &first, b"manifest");
        staging.publish(&first, &manifest, b"head").unwrap();
        assert!(matches!(
            verification.read_range(&first.generation, &data, 0, data.size_bytes),
            Err(CodeIndexStorageError::AllocationRefused(_))
        ));
        verification.verify_payload(&first.generation, &data, &|| Ok(())).unwrap();
        assert_eq!(memory.snapshot().used_bytes, 0);
        let calls = std::cell::Cell::new(0);
        assert!(matches!(
            verification.verify_payload(&first.generation, &data, &|| {
                let count = calls.get() + 1;
                calls.set(count);
                if count == 3 {
                    Err(CodeIndexStorageError::Infrastructure("cancelled".to_owned()))
                } else {
                    Ok(())
                }
            }),
            Err(CodeIndexStorageError::Infrastructure(_))
        ));
        assert_eq!(calls.get(), 3);
        assert_eq!(memory.snapshot().used_bytes, 0);
    }).await.unwrap();
}

#[asupersync::test]
async fn registered_sql_pending_retirement_fences_adoption_and_resumes_exactly() {
    let directory = tempfile::tempdir().unwrap();
    let project = ProjectId::new("project-retirement-intent").unwrap();
    let (_owner, database) = project_database(&directory.path().join("project.db"), &project).await;
    let storage = database
        .code_index_storage(ledger(16 * 1024 * 1024), key(&project, "main", "first"))
        .unwrap();
    let mut graph = database.graph_publication_storage().unwrap();
    crate::native_runtime::spawn_blocking(move || {
        let first = stage("main", "first", None);
        storage.begin_stage(&first).unwrap();
        let manifest = put(&storage, &first, b"manifest");
        storage.publish(&first, &manifest, b"first").unwrap();
        let second = stage("main", "second", Some(b"first"));
        storage.begin_stage(&second).unwrap();
        let next_manifest = put(&storage, &second, b"next manifest");
        storage.publish(&second, &next_manifest, b"second").unwrap();
        let operation = StoreOperationIdV1::new("retirement-intent").unwrap();
        assert!(matches!(
            storage.prepare_retirement(
                &first.generation, &operation, &manifest.digest, b"receipt", b"stale", b"remaining",
            ),
            Err(CodeIndexStorageError::StagingConflict)
        ));
        storage.prepare_retirement(
            &first.generation, &operation, &manifest.digest, b"receipt", b"second", b"remaining",
        ).unwrap();
        assert_eq!(
            storage.pending_retirement(&first.generation).unwrap().unwrap().0,
            operation,
        );
        assert_eq!(storage.read_manifest(&first.generation, &manifest.digest).unwrap().as_bytes(), b"manifest");
        let adoption = br#"{"generation_id":"second","generation_index":[{"generation_id":"first"}]}"#;
        assert!(storage.compare_and_swap_head(&second.generation, b"remaining", adoption).is_err());
        let source = serde_json::json!({"sealed_code_generation": {
            "generation": "first", "sealed_state_digest": manifest.digest.as_str(),
        }});
        let digest = payload(b"graph").digest;
        let replay = GraphPublicationReplayV1::new(
            GraphPublicationKeyV1::new(
                tracedecay_store::GraphProjectionIdentityV1 {
                    shard_id: storage.binding().shard_id.clone(),
                    namespace: tracedecay_store::GraphNamespaceV1::new("code").unwrap(),
                    projection: tracedecay_store::GraphProjectionIdV1::new("intent").unwrap(),
                },
                tracedecay_store::GraphGenerationIdV1::new("first").unwrap(),
                tracedecay_store::GraphPublicationIdempotencyKeyV1::new("new-reference").unwrap(),
            ),
            tracedecay_store::GraphPublicationInputDigestV1::new(digest.as_str()).unwrap(),
            tracedecay_store::GraphDependencyGenerationClosureDigestV1::new(digest.as_str()).unwrap(),
            vec![], None,
            tracedecay_store::GraphRecoveredGenerationDigestV1::new(digest.as_str()).unwrap(),
            serde_json::to_vec(&source).unwrap(),
        ).unwrap();
        let (control, probe) = graph_context("fenced-replay");
        let context = GraphPublicationOperationContextV1::new(&control, &probe).unwrap();
        assert!(graph.append_replay(&replay, &context).is_err());
        // The pending record, not a caller's old head, owns an interrupted retry.
        storage.prepare_retirement(
            &first.generation, &operation, &manifest.digest, b"receipt", b"second", b"remaining",
        ).unwrap();
        storage.complete_retirement(&first.generation, &operation).unwrap();
        storage.complete_retirement(&first.generation, &operation).unwrap();
        assert!(matches!(
            storage.read_manifest(&first.generation, &manifest.digest),
            Err(CodeIndexStorageError::Retired),
        ));
        assert_eq!(storage.read_manifest(&second.generation, &next_manifest.digest).unwrap().as_bytes(), b"next manifest");
    }).await.unwrap();
}

#[asupersync::test]
async fn registered_sql_reset_cas_preserves_shared_payloads_and_project_memory() {
    let directory = tempfile::tempdir().unwrap();
    let project = ProjectId::new("project-scope-reset").unwrap();
    let (_owner, database) = project_database(&directory.path().join("project.db"), &project).await;
    database.execute_write(
        "project memory sentinel", "INSERT INTO metadata(key, value) VALUES (?1, ?2)",
        ("memory-sentinel", "preserved"),
    ).await.unwrap();
    let storage = database
        .code_index_storage(ledger(16 * 1024 * 1024), key(&project, "main", "first"))
        .unwrap();
    crate::native_runtime::spawn_blocking(move || {
        let first = stage("main", "first", None);
        storage.begin_stage(&first).unwrap();
        let payload = put(&storage, &first, b"shared manifest");
        storage.publish(&first, &payload, b"main-head").unwrap();
        let sibling = stage("linked", "first", None);
        storage.begin_stage(&sibling).unwrap();
        put(&storage, &sibling, b"shared manifest");
        storage.publish(&sibling, &payload, b"linked-head").unwrap();
        assert!(!storage.reset_head(&first.generation.worktree_id, b"foreign-head").unwrap());
        assert!(storage.reset_head(&first.generation.worktree_id, b"main-head").unwrap());
        assert!(storage.read_head(&first.generation.worktree_id).unwrap().is_none());
        assert_eq!(storage.read_head(&sibling.generation.worktree_id).unwrap().unwrap().as_bytes(), b"linked-head");
        assert_eq!(storage.read_manifest(&first.generation, &payload.digest).unwrap().as_bytes(), b"shared manifest");
        assert_eq!(storage.read_manifest(&sibling.generation, &payload.digest).unwrap().as_bytes(), b"shared manifest");
        let orphan = stage("main", "abandoned", None);
        storage.begin_stage(&orphan).unwrap();
        put(&storage, &orphan, b"staging debris");
        storage.abandon_stage(&orphan.operation_id).unwrap();
        storage.abandon_stage(&orphan.operation_id).unwrap();
        let successor = stage("main", "rebuilt", None);
        storage.begin_stage(&successor).unwrap();
        let rebuilt = put(&storage, &successor, b"rebuilt manifest");
        storage.publish(&successor, &rebuilt, b"rebuilt-head").unwrap();
        assert_eq!(storage.read_head(&successor.generation.worktree_id).unwrap().unwrap().as_bytes(), b"rebuilt-head");
    }).await.unwrap();
    assert_eq!(
        database.query_scalar_i64(
            "memory sentinel preserved",
            "SELECT COUNT(*) FROM metadata WHERE key = 'memory-sentinel' AND value = 'preserved'",
        ).await.unwrap(),
        1,
    );
    assert_eq!(
        database.query_scalar_i64("orphan staging removed", "SELECT COUNT(*) FROM code_index_staging").await.unwrap(),
        0,
    );
}

#[asupersync::test]
async fn registered_sql_staging_recovery_preserves_live_owner_and_collects_after_retirement() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("project.db");
    let project = ProjectId::new("project-staging-recovery").unwrap();
    let authority = DatabaseAuthority::acquire_test(&path, "staging recovery").unwrap();
    let old_token = authority.token().to_owned();
    let fixture = Database::publish_registered_test_runtime_with_retirement_control(
        &path, &authority, TestDatabaseRuntimeMode::Initialize,
        TestDatabaseRuntimeScope::Project { project_id: project.clone() },
    ).await.unwrap();
    let (owner, runtime, control) = fixture.into_parts();
    drop(runtime);
    let database = owner.issue_lease().unwrap();
    let shared = owner.issue_lease().unwrap();
    let storage = database.code_index_storage(
        ledger(16 * 1024 * 1024), key(&project, "main", "first"),
    ).unwrap();
    let joined = shared.code_index_storage(
        ledger(16 * 1024 * 1024), key(&project, "linked", "first"),
    ).unwrap();
    let operation = crate::native_runtime::spawn_blocking(move || {
        let stage = stage("main", "first", None);
        storage.begin_stage(&stage).unwrap();
        put(&storage, &stage, b"first staged payload");
        put(&storage, &stage, b"second staged payload");
        let page = payload(b"staged evidence");
        storage.stage_evidence_page(
            &stage.operation_id, 0, &page.digest, b"staged evidence",
        ).unwrap();
        assert!(!joined.collect_abandoned_stage().unwrap());
        assert!(!joined.collect_unreferenced_payload().unwrap());
        stage.operation_id
    }).await.unwrap();
    assert_eq!(database.query_scalar_i64(
        "live stage survives", "SELECT COUNT(*) FROM code_index_staging_refs",
    ).await.unwrap(), 2);
    drop(database);
    drop(shared);
    let target = owner.reserve_retirement().unwrap().into_store_retirement_target().unwrap();
    let crate::shard_runtime::registry::StoreRuntimeRetirementResult::Reserved(mut retirement) =
        control.registry().reserve_retirement_batch(vec![target])
    else { panic!("all staging clients have ended"); };
    retirement.commit().unwrap();
    drop(retirement);
    drop(owner);
    drop(control);
    drop(authority);

    let authority = DatabaseAuthority::acquire_test(&path, "replacement staging owner").unwrap();
    assert_ne!(authority.token(), old_token);
    let (database, _) = Database::publish_registered_test_runtime(
        &path, &authority, TestDatabaseRuntimeMode::Existing,
        TestDatabaseRuntimeScope::Project { project_id: project.clone() },
    ).await.unwrap();
    let storage = database.code_index_storage(
        ledger(16 * 1024 * 1024), key(&project, "main", "second"),
    ).unwrap();
    crate::native_runtime::spawn_blocking(move || {
        assert!(matches!(
            storage.begin_stage(&stage("main", "first", None)),
            Err(CodeIndexStorageError::StagingConflict),
        ));
        assert!(storage.collect_abandoned_stage().unwrap());
        assert!(storage.collect_abandoned_stage().unwrap());
        assert!(storage.collect_abandoned_stage().unwrap());
        assert!(storage.collect_abandoned_stage().unwrap());
        assert!(!storage.collect_abandoned_stage().unwrap());
        storage.begin_stage(&stage("main", "first", None)).unwrap();
        storage.abandon_stage(&operation).unwrap();
        while storage.collect_unreferenced_payload().unwrap() {}
    }).await.unwrap();
    assert_eq!(database.query_scalar_i64(
        "recovered stages absent", "SELECT COUNT(*) FROM code_index_staging",
    ).await.unwrap(), 0);
    assert_eq!(database.query_scalar_i64(
        "recovered payloads absent", "SELECT COUNT(*) FROM code_index_payloads",
    ).await.unwrap(), 0);
}
