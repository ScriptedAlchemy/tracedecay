use std::sync::Arc;

use tempfile::TempDir;
use tracedecay_code_index::production::{
    CodeIndexPublishedGenerationV1, SharedDecodedContentPoolV1,
};
use tracedecay_domain::{ProjectId, WorktreeId};
use tracedecay_runtime_core::db::{
    Database, DatabaseAuthority, TestDatabaseRuntimeMode, TestDatabaseRuntimeScope,
};

use tracedecay_code_index_runtime::code_index_scheduler::{
    CodeIndexHintPolicyV1, CodeIndexReconcileOutcomeV1, CodeIndexWorktreeSchedulerV1,
    SharedCodeIndexBytePoolV1,
};

fn git(root: &std::path::Path, args: &[&str]) {
    let output = std::process::Command::new("git")
        .args(args)
        .current_dir(root)
        .output()
        .unwrap();
    assert!(output.status.success(), "{args:?}: {:?}", output.stderr);
}

#[test]
fn registered_project_publication_restores_exact_bytes_without_loose_segments() {
    tracedecay_global_db::register_registered_schema_installer();
    let directory = TempDir::new().unwrap();
    let root = directory.path().canonicalize().unwrap();
    let checkout = root.join("checkout");
    std::fs::create_dir_all(checkout.join("src")).unwrap();
    std::fs::write(
        checkout.join("src/lib.rs"),
        "pub fn shared(value: u32) -> u32 { value + 1 }\n",
    )
    .unwrap();
    git(&checkout, &["init", "-q", "-b", "main"]);
    git(&checkout, &["config", "user.name", "Test"]);
    git(&checkout, &["config", "user.email", "test@example.invalid"]);
    git(&checkout, &["config", "maintenance.auto", "false"]);
    git(&checkout, &["add", "."]);
    git(&checkout, &["commit", "-qm", "fixture"]);
    let project = ProjectId::new("registered-code-index").unwrap();
    let database_path = root.join("project.db");
    let authority = DatabaseAuthority::acquire_test(&database_path, "registered index").unwrap();
    let (database, _) = tracedecay_runtime_core::native_runtime::block_on(
        Database::publish_registered_test_runtime(
            &database_path,
            &authority,
            TestDatabaseRuntimeMode::Initialize,
            TestDatabaseRuntimeScope::Project {
                project_id: project.clone(),
            },
        ),
    )
    .unwrap()
    .unwrap();
    let database = Arc::new(database);
    let store = root.join("scope");
    let mut scheduler = CodeIndexWorktreeSchedulerV1::open_with_policy(
        project.clone(),
        &checkout,
        store.clone(),
        Arc::new(SharedCodeIndexBytePoolV1::default()),
        CodeIndexHintPolicyV1::default(),
        tracedecay_code_index_runtime::registry_default_index_path_policy(),
        Arc::clone(&database),
    )
    .unwrap();
    let limit = std::num::NonZeroU64::new(1024 * 1024 * 1024).unwrap();
    let memory = Arc::new(
        tracedecay_runtime_core::resident_memory::ProcessResidentMemoryV1::with_pressure(
            limit,
            Arc::new(
                tracedecay_runtime_core::resident_memory::ResidentMemoryPressureV1::with_sampler(
                    limit,
                    Arc::new(|| None),
                ),
            ),
        ),
    );
    scheduler.bind_resident_memory(Arc::clone(&memory));
    assert!(matches!(
        scheduler.reconcile_now().unwrap(),
        CodeIndexReconcileOutcomeV1::Published(_)
    ));
    let latest = scheduler.latest_complete().unwrap();
    let generation = latest.generation().manifest().generation_id.clone();
    let binding = scheduler.code_graph_replay_binding(&generation).unwrap();
    let digest = tracedecay_domain::ManifestDigest::new(binding.sealed_state_digest.as_str()).unwrap();
    let key = tracedecay_runtime_core::db::CodeIndexGenerationKey {
        generation_id: generation.clone(), worktree_id: binding.worktree_id,
    };
    let encoded = binding.storage.read_manifest(&key, &digest).unwrap();
    let restored = CodeIndexPublishedGenerationV1::decode_partitioned_sealed(
        &encoded,
        &SharedDecodedContentPoolV1::default(),
        |request| {
            let (digest, size_bytes, offset, length) = match request {
                tracedecay_code_index::production::SealedGenerationSegmentReadV1::Whole { digest, size_bytes } =>
                    (digest, size_bytes, 0, size_bytes),
                tracedecay_code_index::production::SealedGenerationSegmentReadV1::Range { digest, size_bytes, offset, length } =>
                    (digest, size_bytes, offset, length),
            };
            binding.storage.read_range(&key, &tracedecay_runtime_core::db::CodeIndexPayloadIdentity {
                digest: digest.clone(), size_bytes,
            }, offset, length).map_err(|error| {
                tracedecay_code_index::production::CodeIndexProductionErrorV1::Contract(error.to_string())
            })
        },
    )
    .unwrap();
    assert_eq!(restored.manifest().generation_id, generation);
    assert_eq!(restored.snapshot().files.len(), 1);
    assert_eq!(restored.symbols().symbols.len(), 1);
    assert!(restored.chunks().chunks().iter().any(|chunk| {
        chunk.sanitized_text.as_str().contains("value + 1")
    }));
    let before = memory.snapshot().used_bytes;
    let resealed = restored.encode_partitioned_sealed(&memory, |_| Ok(())).unwrap();
    assert_eq!(resealed.as_bytes(), encoded.as_bytes());
    assert_eq!(memory.snapshot().used_bytes - before, resealed.len() as u64);
    let owner = Arc::new(resealed);
    let shared = Arc::clone(&owner);
    drop(owner);
    assert_eq!(memory.snapshot().used_bytes - before, shared.len() as u64);
    drop(shared);
    assert_eq!(memory.snapshot().used_bytes, before);
    let denied_limit = std::num::NonZeroU64::new(1).unwrap();
    let denied = Arc::new(
        tracedecay_runtime_core::resident_memory::ProcessResidentMemoryV1::with_pressure(
            denied_limit,
            Arc::new(
                tracedecay_runtime_core::resident_memory::ResidentMemoryPressureV1::with_sampler(
                    denied_limit, Arc::new(|| None),
                ),
            ),
        ),
    );
    assert!(matches!(
        restored.encode_partitioned_sealed(&denied, |_| Ok(())),
        Err(tracedecay_code_index::production::CodeIndexProductionErrorV1::Publication(
            tracedecay_code_index::production::CodeIndexPublicationStoreErrorV1::ResidentMemoryRefused(_),
        ))
    ));
    assert_eq!(denied.snapshot().used_bytes, 0);
    assert!(!store.join("active-code-generation-v1.json").exists());
    assert!(!store.join("code-generations-v1").exists());
    assert!(!store.join("code-generation-segments-v1").exists());
    let storage = database.project_code_index_storage(Arc::clone(&memory), &project).unwrap();
    assert!(storage
        .read_head(&WorktreeId::new(restored.snapshot().worktree.as_ref().unwrap().as_str()).unwrap())
        .unwrap()
        .is_some());
    std::fs::write(
        checkout.join("src/lib.rs"),
        "pub fn shared(value: u32) -> u32 { value + 2 }\n",
    ).unwrap();
    assert!(matches!(scheduler.reconcile_now().unwrap(), CodeIndexReconcileOutcomeV1::Published(_)));
    let successor = scheduler.latest_complete().unwrap();
    assert_ne!(successor.generation().manifest().generation_id, generation);
    let protected = std::collections::BTreeSet::from([generation.clone()]);
    tracedecay_code_index_retention::code_index_generations::retain_registered_code_index(
        &storage, &key.worktree_id, &protected, &|| false,
    ).unwrap();
    assert!(storage.read_manifest(&key, &digest).is_ok());
    assert!(tracedecay_code_index_retention::code_index_generations::retain_registered_code_index(
        &storage, &key.worktree_id, &std::collections::BTreeSet::new(), &|| false,
    ).unwrap());
    assert!(matches!(
        storage.read_manifest(&key, &digest),
        Err(tracedecay_runtime_core::db::CodeIndexStorageError::Retired),
    ));
    let receipts = tracedecay_runtime_core::native_runtime::block_on(
        database.query_scalar_i64(
            "canonical code-index receipts",
            "SELECT COUNT(*) FROM code_index_retirement_receipts
             WHERE json_extract(CAST(receipt AS TEXT), '$.schema')
                = 'tracedecay.code-generation-retention-receipt.v1'
                AND json_extract(CAST(receipt AS TEXT), '$.deleted_generations[0].generation_id')
                    = (SELECT generation_id FROM code_index_retirement_receipts LIMIT 1)",
        ),
    ).unwrap().unwrap();
    assert_eq!(receipts, 1);
    let reset = scheduler.reset_corrupt_publication_authority().unwrap();
    assert_eq!(reset.removed_entries, 1);
    assert!(storage.read_head(&key.worktree_id).unwrap().is_none());
    std::fs::write(
        checkout.join("src/lib.rs"),
        "pub fn shared(value: u32) -> u32 { value + 3 }\n",
    ).unwrap();
    assert!(matches!(scheduler.reconcile_now().unwrap(), CodeIndexReconcileOutcomeV1::Published(_)));
    let rebuilt = scheduler.latest_complete().unwrap();
    assert_ne!(
        rebuilt.generation().manifest().generation_id,
        successor.generation().manifest().generation_id,
    );
    assert!(storage.read_head(&key.worktree_id).unwrap().is_some());
    assert_eq!(tracedecay_runtime_core::native_runtime::block_on(
        database.query_scalar_i64(
            "reset preserves canonical retirement receipts",
            "SELECT COUNT(*) FROM code_index_retirement_receipts",
        ),
    ).unwrap().unwrap(), 1);
}
