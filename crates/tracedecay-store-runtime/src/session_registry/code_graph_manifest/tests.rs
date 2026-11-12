use std::sync::Arc;

use tempfile::TempDir;
use tracedecay_code_index_runtime::code_index_scheduler::{
    CodeIndexHintPolicyV1, CodeIndexWorktreeSchedulerV1, SharedCodeIndexBytePoolV1,
};
use tracedecay_runtime_core::db::{
    Database, DatabaseAuthority, TestDatabaseRuntimeMode, TestDatabaseRuntimeScope,
};
use tracedecay_runtime_core::resident_memory::{ProcessResidentMemoryV1, ResidentMemoryPressureV1};

use super::*;

struct Fixture {
    storage: CodeIndexSqliteStorage,
    generation: CodeIndexGenerationKey,
    source: SealedCodeGenerationReplay,
    memory: Arc<ProcessResidentMemoryV1>,
    _database: Arc<Database>,
    _directory: TempDir,
}

fn git(root: &std::path::Path, args: &[&str]) {
    let output = std::process::Command::new("git").args(args).current_dir(root).output().unwrap();
    assert!(output.status.success(), "{args:?}: {:?}", output.stderr);
}

fn fixture() -> Fixture {
    tracedecay_global_db::register_registered_schema_installer();
    let directory = TempDir::new().unwrap();
    let root = directory.path().canonicalize().unwrap();
    let checkout = root.join("checkout");
    std::fs::create_dir_all(checkout.join("src")).unwrap();
    for (file, source) in [
        ("lib.rs", "pub mod alpha;\npub mod beta;\npub fn root() -> u32 { alpha::value() }\n"),
        ("alpha.rs", "pub fn value() -> u32 { crate::beta::other() }\n"),
        ("beta.rs", "pub fn other() -> u32 { 7 }\n"),
    ] {
        std::fs::write(checkout.join("src").join(file), source).unwrap();
    }
    git(&checkout, &["init", "-q", "-b", "main"]);
    git(&checkout, &["config", "user.name", "Test"]);
    git(&checkout, &["config", "user.email", "test@example.invalid"]);
    git(&checkout, &["add", "."]);
    git(&checkout, &["commit", "-qm", "fixture"]);
    let project = ProjectId::new("project-registered-replay").unwrap();
    let path = root.join("project.db");
    let authority = DatabaseAuthority::acquire_test(&path, "registered replay").unwrap();
    let (database, _) = tracedecay_runtime_core::native_runtime::block_on(
        Database::publish_registered_test_runtime(
            &path, &authority, TestDatabaseRuntimeMode::Initialize,
            TestDatabaseRuntimeScope::Project { project_id: project.clone() },
        ),
    ).unwrap().unwrap();
    let database = Arc::new(database);
    let limit = std::num::NonZeroU64::new(1024 * 1024 * 1024).unwrap();
    let memory = Arc::new(ProcessResidentMemoryV1::with_pressure(
        limit, Arc::new(ResidentMemoryPressureV1::with_sampler(limit, Arc::new(|| None))),
    ));
    let mut scheduler = CodeIndexWorktreeSchedulerV1::open_with_policy(
        project, &checkout, root.join("scope"), Arc::new(SharedCodeIndexBytePoolV1::default()),
        CodeIndexHintPolicyV1::default(),
        tracedecay_code_index_runtime::registry_default_index_path_policy(),
        Arc::clone(&database),
    ).unwrap();
    scheduler.bind_resident_memory(Arc::clone(&memory));
    scheduler.reconcile_now().unwrap();
    let latest = scheduler.latest_complete().unwrap();
    let generation = latest.generation().manifest().generation_id.clone();
    let binding = scheduler.code_graph_replay_binding(&generation).unwrap();
    let source = SealedCodeGenerationReplay {
        repository: latest.generation().snapshot().repository.clone(),
        generation: generation.clone(),
        sealed_state_digest: binding.sealed_state_digest,
        projector_revision: GraphProjectorRevision::try_from(
            tracedecay_code_index::graph_projection::CODE_GRAPH_PROJECTOR_REVISION.to_owned(),
        ).unwrap(),
    };
    Fixture {
        storage: binding.storage,
        generation: CodeIndexGenerationKey { worktree_id: binding.worktree_id, generation_id: generation },
        source, memory, _database: database, _directory: directory,
    }
}

fn owner(fixture: &Fixture) -> GraphProjectionIdentityV1 {
    GraphProjectionIdentityV1 {
        shard_id: fixture.storage.binding().shard_id.clone(),
        namespace: tracedecay_store::GraphNamespaceV1::new("code").unwrap(),
        projection: tracedecay_store::GraphProjectionIdV1::new("generation").unwrap(),
    }
}

fn hydrate(
    provider: &DaemonCodeGraphManifestProviderV1,
    fixture: &Fixture,
    source: &SealedCodeGenerationReplay,
    check: &dyn Fn() -> Result<(), GraphDbError>,
) -> Result<SpilledGraphGeneration, GraphDbError> {
    let owner = owner(fixture);
    let scratch = fixture._directory.path().join(format!(
        "rows-{}", fixture.memory.snapshot().used_bytes,
    ));
    provider.hydrate_sealed_code_generation(
        &owner, source,
        GraphGenerationRowSpill::create(scratch, GraphProjectionIdentity::new(
            GraphNamespace::new(owner.namespace.as_str())?,
            GraphProjectionId::new(owner.projection.as_str())?,
        ))?, check,
    )
}

#[test]
fn registered_replay_routes_share_exact_authority_and_retire_with_last_owner() {
    let fixture = fixture();
    let provider = Arc::new(DaemonCodeGraphManifestProviderV1::default());
    let project = match &owner(&fixture).shard_id.scope {
        StoreShardScopeV1::Project { project_id } => project_id.clone(),
        _ => unreachable!(),
    };
    let bind = || provider.bind_sql(
        owner(&fixture).shard_id, project.clone(), fixture.source.repository.clone(),
        fixture.storage.clone(),
    ).unwrap();
    let first = bind();
    let second = bind();
    drop(first);
    let one = hydrate(&provider, &fixture, &fixture.source, &|| Ok(())).unwrap();
    assert!(one.row_counts().0 > 0);
    let digest = one.expected_recovered_digest().clone();
    let counts = one.row_counts();
    drop(one);
    let two = hydrate(&provider, &fixture, &fixture.source, &|| Ok(())).unwrap();
    assert_eq!(two.row_counts(), counts);
    assert_eq!(two.expected_recovered_digest(), &digest);
    drop(two);
    drop(second);
    assert!(provider.sources.read().unwrap().is_empty());
    assert!(matches!(
        hydrate(&provider, &fixture, &fixture.source, &|| Ok(())),
        Err(GraphDbError::Unavailable { .. }),
    ));
}

#[test]
fn registered_replay_refuses_foreign_source_and_preserves_interruptions() {
    let fixture = fixture();
    let provider = Arc::new(DaemonCodeGraphManifestProviderV1::default());
    let project = match &owner(&fixture).shard_id.scope {
        StoreShardScopeV1::Project { project_id } => project_id.clone(),
        _ => unreachable!(),
    };
    let _route = provider.bind_sql(
        owner(&fixture).shard_id, project, fixture.source.repository.clone(),
        fixture.storage.clone(),
    ).unwrap();
    for interruption in [GraphDbError::Cancelled, GraphDbError::DeadlineExceeded] {
        let calls = std::cell::Cell::new(0);
        let check = || {
            let call = calls.get() + 1;
            calls.set(call);
            if call >= 4 { Err(interruption.clone()) } else { Ok(()) }
        };
        assert_eq!(
            hydrate(&provider, &fixture, &fixture.source, &check).unwrap_err(),
            interruption,
        );
    }
    let mut foreign = fixture.source.clone();
    foreign.repository = RepositoryId::new("foreign").unwrap();
    assert!(matches!(
        hydrate(&provider, &fixture, &foreign, &|| Ok(())),
        Err(GraphDbError::Conflict { .. }),
    ));
    let mut missing = fixture.source.clone();
    missing.generation = CodeGenerationId::new("missing").unwrap();
    assert!(matches!(
        hydrate(&provider, &fixture, &missing, &|| Ok(())),
        Err(GraphDbError::Unavailable { .. }),
    ));
}
