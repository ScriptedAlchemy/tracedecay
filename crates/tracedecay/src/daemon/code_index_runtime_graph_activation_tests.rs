//! Graph-activation scheduler tests that need the root session registry.
//!
//! These journeys used to live beside the scheduler. They open
//! `DaemonSessionRuntimeRegistryV1`, so they compile in the composition root.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::time::Duration;

use tempfile::TempDir;
use tracedecay_application::ResolvedScope;
use tracedecay_code_index_runtime::code_index_scheduler::{
    CodeGraphActivationAuthorityV1, CodeGraphActivationPolicyV1, CodeIndexReconcileOutcomeV1,
    CodeIndexSchedulerRegistryV1, CodeIndexWorktreeSchedulerV1, SharedCodeIndexBytePoolV1,
    scoped_code_index_store_root,
};
use tracedecay_domain::ProjectId;

use tracedecay_store_runtime::DaemonSessionRuntimeRegistryV1;

const ALPHA_LIB_V1: &[(&str, &str)] = &[("src/lib.rs", "pub fn alpha() -> u32 { 1 }\n")];

struct GitFixture {
    root: TempDir,
}

impl GitFixture {
    fn new(files: &[(&str, &str)]) -> Self {
        let root = TempDir::new().expect("fixture root");
        git(root.path(), &["init", "-q", "-b", "main"]);
        git(root.path(), &["config", "user.name", "TraceDecay Test"]);
        git(
            root.path(),
            &["config", "user.email", "tracedecay@example.invalid"],
        );
        for (path, source) in files {
            let dest = root.path().join(path);
            std::fs::create_dir_all(dest.parent().expect("source parent"))
                .expect("create source parent");
            std::fs::write(dest, source).expect("write fixture source");
        }
        git(root.path(), &["add", "."]);
        git(root.path(), &["commit", "-qm", "fixture"]);
        Self { root }
    }

    fn path(&self) -> &Path {
        self.root.path()
    }
}

fn git(root: &Path, args: &[&str]) {
    let status = Command::new(
        tracedecay_runtime_core::git::try_git_program()
            .expect("absolute git executable should resolve"),
    )
    .current_dir(root)
    .args(args)
    .status()
    .expect("run git fixture command");
    assert!(status.success(), "git fixture command failed: {args:?}");
}

fn test_project_id() -> ProjectId {
    ProjectId::new("project.code-index-tests").expect("valid test project identity")
}

fn scheduler(
    fixture: &GitFixture,
    store_root: PathBuf,
    bytes: Arc<SharedCodeIndexBytePoolV1>,
) -> CodeIndexWorktreeSchedulerV1 {
    CodeIndexWorktreeSchedulerV1::open(test_project_id(), fixture.path(), store_root, bytes)
        .expect("open worktree scheduler")
}

fn published(
    outcome: CodeIndexReconcileOutcomeV1,
) -> tracedecay_code_index_runtime::code_index_scheduler::CodeIndexPublishEvidenceV1 {
    match outcome {
        CodeIndexReconcileOutcomeV1::Published(evidence) => evidence,
        CodeIndexReconcileOutcomeV1::Noop(evidence) => {
            panic!("expected a published generation, got noop {evidence:?}")
        }
    }
}

/// A retained text generation reaches exact/lexical readiness when persistent
/// graph replay is permanently refused. The full graph owner stays absent, so
/// text availability never implies graph availability.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn failed_cold_mount_graph_replay_preserves_retained_text_generation() {
    let fixture = GitFixture::new(ALPHA_LIB_V1);
    let store = TempDir::new().expect("store root");
    let bytes = Arc::new(SharedCodeIndexBytePoolV1::default());
    let scoped_store = scoped_code_index_store_root(
        store.path(),
        &fixture.path().canonicalize().expect("canonical fixture"),
    );
    let (scope, seeded_generation_id) = {
        let mut scheduler = scheduler(&fixture, scoped_store, bytes);
        published(scheduler.reconcile_now().expect("seed generation"));
        let latest = scheduler.latest_complete().expect("seeded generation");
        let snapshot = latest.generation().snapshot();
        (
            ResolvedScope::new(
                test_project_id(),
                snapshot.repository.clone(),
                snapshot.worktree.clone().expect("worktree id"),
                snapshot.reference.clone(),
            )
            .expect("resolved scope"),
            latest.generation().manifest().generation_id.clone(),
        )
    };

    let profile = TempDir::new().expect("profile root");
    let profile_root = profile.path().join("profile");
    let project_id = test_project_id();
    tracedecay_runtime_core::storage::pin_fixture_repository_identity(
        fixture.path(),
        project_id.as_str(),
    )
    .expect("project enrollment");
    let identity = tracedecay_daemon_identity::profile_identity::load_or_create(&profile_root)
        .expect("profile identity");
    let _database_scope = tracedecay_runtime_core::db::enter_daemon_database_scope(
        &profile_root,
        93,
        "failed cold-mount graph replay",
    )
    .expect("daemon database scope");
    let graph_runtime = Arc::new(
        DaemonSessionRuntimeRegistryV1::open(identity)
            .await
            .expect("graph runtime registry"),
    );
    let _writable_project_database = graph_runtime
        .project_memory(project_id.clone(), [fixture.path().to_path_buf()])
        .await
        .expect("initialize writable project database");
    let read_only_project_database = Arc::new(
        graph_runtime
            .project_memory_read_only(project_id.clone(), [fixture.path().to_path_buf()])
            .await
            .expect("read-only project database"),
    );
    assert!(
        read_only_project_database
            .graph_publication_storage()
            .is_err(),
        "the fixture must refuse persistent graph publication"
    );

    let registry = CodeIndexSchedulerRegistryV1::with_background_reconcile_permits(1, 1);
    let admission = registry
        .background_reconcile_admission()
        .acquire_owned()
        .await
        .expect("hold background activation");
    registry
        .mount_worktree_with_graph_runtime(
            project_id,
            fixture.path(),
            store.path().to_path_buf(),
            None,
            graph_runtime.code_graph_seat_port(),
            read_only_project_database,
            CodeGraphActivationPolicyV1::Enabled,
        )
        .await
        .expect("mount retained generation");

    let scheduler = registry
        .scheduler_handle(fixture.path())
        .await
        .expect("mounted scheduler");
    drop(admission);

    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        if scheduler
            .try_lock()
            .is_ok_and(|scheduler| scheduler.sealed_decode_count() > 0)
        {
            break;
        }
        assert!(
            std::time::Instant::now() <= deadline,
            "worker did not decode the retained generation for graph replay"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    let text = loop {
        if let Some(text) = registry.latest_text_serving_for_scope(&scope).await
            && text.query_owners_are_warm()
        {
            break text;
        }
        assert!(
            std::time::Instant::now() <= deadline,
            "permanently refused graph replay withheld exact and lexical readiness"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    };
    assert!(text.production_query_owners().is_ok());

    assert_eq!(
        registry.latest_generation_id(fixture.path()).await,
        Some(seeded_generation_id),
        "persistent graph replay failure must not withhold retained text serving"
    );
    assert!(
        registry
            .latest_complete_serving_for_scope(&scope)
            .await
            .is_none(),
        "persistent graph replay failure must not expose a full graph owner"
    );
    registry.shutdown().await;
    graph_runtime
        .shutdown_memory_graph_reconciliation_tasks()
        .await
        .expect("join graph reconciliation tasks");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn persistent_graph_activation_publishes_a_small_generation() {
    let fixture = GitFixture::new(ALPHA_LIB_V1);
    let store = TempDir::new().expect("store root");
    let scoped_store = scoped_code_index_store_root(
        store.path(),
        &fixture.path().canonicalize().expect("canonical fixture"),
    );
    let (latest, replay_binding, repository_id, worktree_id) = {
        let mut scheduler = scheduler(
            &fixture,
            scoped_store,
            Arc::new(SharedCodeIndexBytePoolV1::default()),
        );
        published(scheduler.reconcile_now().expect("seed generation"));
        let latest = scheduler.latest_complete().expect("seeded generation");
        let replay_binding = scheduler
            .code_graph_replay_binding(&latest.generation().manifest().generation_id)
            .expect("sealed replay binding");
        let snapshot = latest.generation().snapshot();
        let repository_id = snapshot.repository.clone();
        let worktree_id = snapshot.worktree.clone().expect("worktree identity");
        (latest, replay_binding, repository_id, worktree_id)
    };
    let profile = TempDir::new().expect("profile root");
    let profile_root = profile.path().join("profile");
    let project_id = test_project_id();
    tracedecay_runtime_core::storage::pin_fixture_repository_identity(
        fixture.path(),
        project_id.as_str(),
    )
    .expect("project enrollment");
    let identity = tracedecay_daemon_identity::profile_identity::load_or_create(&profile_root)
        .expect("profile identity");
    let _database_scope = tracedecay_runtime_core::db::enter_daemon_database_scope(
        &profile_root,
        94,
        "small persistent graph activation",
    )
    .expect("daemon database scope");
    let graph_runtime = Arc::new(
        DaemonSessionRuntimeRegistryV1::open(identity)
            .await
            .expect("graph runtime registry"),
    );
    let project_database = graph_runtime
        .project_memory(project_id.clone(), [fixture.path().to_path_buf()])
        .await
        .expect("writable project database");
    // Activation issues verified graph reads; the project graph runtime binds
    // asynchronously after `project_memory` returns, so an unawaited bind
    // races activation into "not ready for verified reads".
    crate::host_admission::await_bound_graph_runtime(
        &project_database,
        "bind small persistent activation graph runtime",
    )
    .await
    .expect("bound project graph runtime");
    let native_graph_path = project_database.database_path().with_extension("grafeo");
    // Publication is crash-atomic: it commits into the graph store's WAL and
    // reaches the base file only at checkpoint, so durability is the byte
    // footprint of the base file plus its WAL, not the base image alone.
    let native_graph_footprint = |path: &std::path::Path| -> u64 {
        let base = std::fs::metadata(path).map_or(0, |meta| meta.len());
        let wal_dir = path.with_extension("grafeo.wal");
        let wal = walkdir::WalkDir::new(wal_dir)
            .into_iter()
            .flatten()
            .filter(|entry| entry.file_type().is_file())
            .filter_map(|entry| entry.metadata().ok())
            .map(|meta| meta.len())
            .sum::<u64>();
        base + wal
    };
    let native_graph_before = native_graph_footprint(&native_graph_path);

    let graph_activation = CodeGraphActivationAuthorityV1::Persistent {
        runtime: graph_runtime.code_graph_seat_port(),
        project_database,
        policy: Arc::new(std::sync::atomic::AtomicBool::new(true)),
    };

    let activated = latest.clone();
    graph_activation
        .activate(
            &project_id,
            &repository_id,
            &worktree_id,
            latest,
            replay_binding,
            Arc::new(std::sync::atomic::AtomicBool::new(false)),
        )
        .await
        .expect("small persistent generation must activate");
    activated
        .interactive_graph_store()
        .expect("activated generation must publish an interactive graph store");

    graph_runtime
        .shutdown_memory_graph_reconciliation_tasks()
        .await
        .expect("join graph reconciliation tasks");

    // The committed image converges shortly after activation returns rather
    // than atomically with it.
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    loop {
        if native_graph_footprint(&native_graph_path) != native_graph_before {
            break;
        }
        assert!(
            std::time::Instant::now() <= deadline,
            "persistent activation must publish the generation into the native graph"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}
