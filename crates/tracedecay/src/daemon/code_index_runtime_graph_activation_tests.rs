//! Graph-activation scheduler tests that need the root session registry.
//!
//! These journeys used to live beside the scheduler. They open
//! `DaemonSessionRuntimeRegistryV1`, so they compile in the composition root.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::time::Duration;

use tempfile::TempDir;
use tracedecay_application::{
    CancellationContext, CapabilityGrantId, CapabilityGrantSnapshot, Deadline, DisclosureClass,
    RequestContext, RequestId, ResolvedScope, now_micros,
};
use tracedecay_code_index_runtime::CodeGraphSeatRuntimePortV1;
use tracedecay_code_index_runtime::code_index_scheduler::{
    CodeGraphActivationAuthorityV1, CodeGraphActivationPolicyV1, CodeIndexReconcileOutcomeV1,
    CodeIndexSchedulerRegistryV1, CodeIndexWorktreeSchedulerV1, SharedCodeIndexBytePoolV1,
    scoped_code_index_store_root,
};
use tracedecay_domain::{ActorId, ManifestDigest, ProjectId, UtcMicros};
use tracedecay_graph_query::{CodeGraphReadFreshnessV1, CodeGraphReadRequest};
use tracedecay_session_memory::runtime_telemetry::{
    GenerationCensusServingFreshness, GenerationCensusSnapshot, GenerationCensusUnavailableReason,
};
use tracedecay_store_runtime::DaemonSessionRuntimeRegistryV1;
use tracedecay_tool_catalog::{CapabilityId, UseCaseId};

use crate::daemon::project_open_owners::{
    project_code_graph_projection_read_port, project_code_index_generation_census_reader,
};

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

    fn edit(&self, path: &str, source: &str) {
        std::fs::write(self.path().join(path), source).expect("edit fixture source");
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
    drop(activated);

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
    drop(graph_activation);
    graph_runtime
        .shutdown_memory_graph_reconciliation_tasks()
        .await
        .expect("join graph reconciliation tasks");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn restart_status_tracks_immediate_settled_and_stale_graph_serving_states() {
    restart_status_case(false, false).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn corrupt_graph_restart_repairs_through_canonical_serialized_activation() {
    restart_status_case(true, false).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn dirty_restart_recovers_the_retained_graph_before_rebuilding_its_successor() {
    restart_status_case(false, true).await;
}

async fn restart_status_case(corrupt_graph: bool, dirty_before_restart: bool) {
    let fixture = GitFixture::new(ALPHA_LIB_V1);
    let store = TempDir::new().expect("store root");
    let scoped_store = scoped_code_index_store_root(
        store.path(),
        &fixture.path().canonicalize().expect("canonical fixture"),
    );
    let (scope, seeded_generation_id, latest, replay_binding, repository_id, worktree_id) = {
        let mut scheduler = scheduler(
            &fixture,
            scoped_store.clone(),
            Arc::new(SharedCodeIndexBytePoolV1::default()),
        );
        published(scheduler.reconcile_now().expect("seed generation"));
        let latest = scheduler.latest_complete().expect("seeded generation");
        latest
            .production_query_owners()
            .expect("seed durable text artifact before restart");
        let replay_binding = scheduler
            .code_graph_replay_binding(&latest.generation().manifest().generation_id)
            .expect("seed graph replay binding");
        let snapshot = latest.generation().snapshot();
        let repository_id = snapshot.repository.clone();
        let worktree_id = snapshot.worktree.clone().expect("worktree identity");
        (
            ResolvedScope::new(
                test_project_id(),
                repository_id.clone(),
                worktree_id.clone(),
                snapshot.reference.clone(),
            )
            .expect("resolved scope"),
            latest.generation().manifest().generation_id.clone(),
            latest,
            replay_binding,
            repository_id,
            worktree_id,
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
        95,
        "stale graph status projection",
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
    crate::host_admission::await_bound_graph_runtime(
        &project_database,
        "bind stale graph status projection",
    )
    .await
    .expect("bound project graph runtime");
    let retained = graph_runtime
        .retain_code_graph_runtime(
            project_id.clone(),
            repository_id,
            worktree_id,
            latest.generation().snapshot().reference.clone(),
            latest.generation().manifest().generation_id.clone(),
            Arc::clone(&project_database),
            replay_binding,
            Some(latest.generation_handle()),
        )
        .await
        .expect("retain seeded graph runtime");
    let seeded_graph = retained
        .publish_verified_snapshot(
            latest.generation(),
            Arc::new(std::sync::atomic::AtomicBool::new(false)),
        )
        .expect("publish graph head before restart");
    drop(seeded_graph);
    drop(retained);
    drop(latest);
    graph_runtime
        .shutdown_memory_graph_reconciliation_tasks()
        .await
        .expect("stop graph runtime before restart");
    drop(project_database);
    drop(graph_runtime);

    if corrupt_graph {
        let mut directories = vec![profile_root.clone()];
        let mut corrupted = 0_usize;
        while let Some(directory) = directories.pop() {
            for entry in std::fs::read_dir(directory).expect("walk profile graph files") {
                let entry = entry.expect("profile graph entry");
                let path = entry.path();
                if entry
                    .file_type()
                    .expect("profile graph entry type")
                    .is_dir()
                {
                    directories.push(path);
                } else if entry.file_name() == "generation.grafeo"
                    && path
                        .components()
                        .any(|component| component.as_os_str() == "tracedecay.sealed")
                {
                    std::fs::write(path, b"corrupt sealed graph")
                        .expect("inject sealed graph corruption");
                    corrupted += 1;
                }
            }
        }
        assert!(
            corrupted > 0,
            "fixture must publish a sealed Grafeo generation"
        );
    }

    let restarted_identity =
        tracedecay_daemon_identity::profile_identity::load_or_create(&profile_root)
            .expect("restart profile identity");
    let graph_runtime = Arc::new(
        DaemonSessionRuntimeRegistryV1::open(restarted_identity)
            .await
            .expect("restarted graph runtime registry"),
    );
    let project_database = graph_runtime
        .project_memory(project_id.clone(), [fixture.path().to_path_buf()])
        .await
        .expect("restarted writable project database");
    crate::host_admission::await_bound_graph_runtime(
        &project_database,
        "bind restarted graph status projection",
    )
    .await
    .expect("bound restarted project graph runtime");

    if dirty_before_restart {
        fixture.edit("src/lib.rs", "pub fn alpha() -> u32 { 2 }\n");
    }

    let registry = CodeIndexSchedulerRegistryV1::with_background_reconcile_permits(1, 1);
    let retained_recovery_gate = if !corrupt_graph {
        Some(
            registry
                .pause_next_retained_graph_recovery_before_successor(
                    fixture.path().canonicalize().expect("canonical fixture"),
                )
                .await,
        )
    } else {
        None
    };
    let activation_admission = registry
        .background_reconcile_admission()
        .acquire_owned()
        .await
        .expect("hold restart activation");
    registry
        .mount_worktree_with_graph_runtime(
            project_id,
            fixture.path(),
            store.path().to_path_buf(),
            None,
            graph_runtime.code_graph_seat_port(),
            project_database,
            CodeGraphActivationPolicyV1::Enabled,
        )
        .await
        .expect("mount persistent graph generation");
    let port = project_code_graph_projection_read_port(
        registry.clone(),
        fixture.path().to_path_buf(),
        scope.clone(),
    );
    let census = project_code_index_generation_census_reader(
        registry.clone(),
        fixture.path().to_path_buf(),
        scope.clone(),
    );
    let immediate_context = graph_request_context(scope.clone(), "restart-immediate");
    let immediate_read = port
        .open(CodeGraphReadRequest::from_context(
            &immediate_context,
            now_micros(),
        ))
        .await;
    assert!(
        immediate_read.is_err(),
        "status must not claim ready before the restart-restored graph authority can serve"
    );
    assert!(
        matches!(
            census().await,
            GenerationCensusSnapshot::Unavailable {
                reason: GenerationCensusUnavailableReason::ExactScopeGenerationNotReady,
            }
        ),
        "restart status must remain unavailable while graph admission refuses"
    );
    let restart_started = std::time::Instant::now();
    drop(activation_admission);

    if !corrupt_graph {
        let (recovered, release_successor) =
            retained_recovery_gate.expect("clean retained graph restart must arm recovery gate");
        tokio::time::timeout(Duration::from_secs(5), recovered)
            .await
            .expect("retained graph recovery did not finish")
            .expect("retained graph recovery gate dropped before observation");
        let scheduler = registry
            .scheduler_handle(fixture.path())
            .await
            .expect("mounted scheduler");
        assert_eq!(
            scheduler
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .sealed_decode_count(),
            0,
            "the retained revision-7 graph must recover before any partition decode"
        );

        if dirty_before_restart {
            let stale_context = graph_request_context(scope.clone(), "restart-dirty-retained");
            let stale_deadline = std::time::Instant::now() + Duration::from_secs(5);
            loop {
                let stale_read = port
                    .open(CodeGraphReadRequest::from_context(
                        &stale_context,
                        now_micros(),
                    ))
                    .await;
                if matches!(
                    stale_read
                        .as_ref()
                        .map(tracedecay_graph_query::VerifiedCodeGraphRead::freshness),
                    Ok(CodeGraphReadFreshnessV1::LastCompleteStale { .. })
                ) {
                    let retained = registry
                        .latest_text_serving_for_scope(&scope)
                        .await
                        .expect("recovered retained text owner");
                    assert_eq!(
                        retained.metadata().manifest().generation_id,
                        seeded_generation_id,
                        "the stale graph service must still belong to the retained generation"
                    );
                    break;
                }
                assert!(
                    std::time::Instant::now() <= stale_deadline,
                    "dirty restart never served the recovered retained graph as stale: {stale_read:?}"
                );
                tokio::task::yield_now().await;
            }
        }
        release_successor
            .send(())
            .expect("release reconcile after retained graph observation");
    }

    let settled_deadline = std::time::Instant::now() + Duration::from_secs(10);
    let settled = loop {
        let observed = registry
            .latest_text_serving_freshness_for_scope(&scope)
            .await;
        if let Some((latest, current)) = observed.as_ref()
            && *current
            && latest.interactive_graph_store().is_ok()
        {
            break latest.clone();
        }
        assert!(
            std::time::Instant::now() <= settled_deadline,
            "persistent graph generation did not become query-serving: {:?}",
            observed.as_ref().map(|(latest, current)| (
                current,
                latest.code_graph_serving_readiness(),
                latest.interactive_graph_store().err(),
            ))
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    };
    if corrupt_graph {
        assert_eq!(
            settled.metadata().manifest().generation_id,
            seeded_generation_id,
            "repairing a corrupt retained graph must not fabricate a dirty source successor"
        );
    } else if dirty_before_restart {
        assert_ne!(
            settled.metadata().manifest().generation_id,
            seeded_generation_id,
            "releasing the gate must rebuild the dirty checkout into a successor generation"
        );
    }
    eprintln!(
        "sandbox_restart_to_graph_serving_micros={} corrupt_graph={corrupt_graph}",
        restart_started.elapsed().as_micros()
    );

    let settled_context = graph_request_context(scope.clone(), "restart-settled");
    let settled_read = port
        .open(CodeGraphReadRequest::from_context(
            &settled_context,
            now_micros(),
        ))
        .await
        .expect("settled restart graph read");
    assert_eq!(settled_read.freshness(), CodeGraphReadFreshnessV1::Current);
    let settled_census = census().await;
    if corrupt_graph || dirty_before_restart {
        assert!(matches!(
            settled_census,
            GenerationCensusSnapshot::Observed {
                freshness: GenerationCensusServingFreshness::Current,
                ..
            }
        ));
    } else {
        assert!(matches!(
            settled_census,
            GenerationCensusSnapshot::Unavailable {
                reason: GenerationCensusUnavailableReason::SealedGenerationCensusInvalid,
            }
        ));
    }

    let scheduler = registry
        .scheduler_handle(fixture.path())
        .await
        .expect("mounted scheduler");
    let sealed_decode_count = scheduler
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .sealed_decode_count();
    if corrupt_graph {
        assert!(
            sealed_decode_count > 0,
            "a corrupt retained graph must defer its typed direct-recovery failure to the \
             canonical serialized activation path"
        );
    } else if !dirty_before_restart {
        assert_eq!(
            sealed_decode_count, 0,
            "clean restart must seat the verified graph head without replaying partition segments"
        );
    }
    let (held_tx, held_rx) = std::sync::mpsc::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
    let lock_thread = std::thread::spawn(move || {
        let _guard = scheduler
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        held_tx.send(()).expect("signal scheduler lock held");
        let _ = release_rx.recv();
    });
    held_rx.recv().expect("scheduler lock acquired");
    std::fs::write(
        fixture.path().join("src/lib.rs"),
        "pub fn alpha() -> u32 { 2 }\n",
    )
    .expect("drift fixture source");
    git(fixture.path(), &["add", "."]);
    git(
        fixture.path(),
        &["commit", "-qm", "move tip while scheduler is held"],
    );

    let context = graph_request_context(scope, "restart-stale");
    let graph_read = port
        .open(CodeGraphReadRequest::from_context(&context, now_micros()))
        .await;
    let census_snapshot = census().await;

    release_tx.send(()).expect("release scheduler lock");
    lock_thread.join().expect("scheduler lock thread");
    registry.shutdown().await;
    graph_runtime
        .shutdown_memory_graph_reconciliation_tasks()
        .await
        .expect("join graph reconciliation tasks");

    let graph_read = graph_read.expect("the seated generation must keep serving while stale");
    assert!(matches!(
        graph_read.freshness(),
        CodeGraphReadFreshnessV1::LastCompleteStale { .. }
    ));
    assert!(matches!(
        census_snapshot,
        GenerationCensusSnapshot::Unavailable {
            reason: GenerationCensusUnavailableReason::SealedGenerationCensusInvalid,
        }
    ));
}

fn graph_request_context(scope: ResolvedScope, suffix: &str) -> RequestContext {
    let capability =
        CapabilityId::new("capability.code-graph-status-projection").expect("capability");
    let use_case = UseCaseId::new("use-case.code-graph-status-projection").expect("use case");
    let grant = CapabilityGrantSnapshot::new(
        CapabilityGrantId::new(format!("grant.code-graph-status-{suffix}")).expect("grant"),
        1,
        digest::<ManifestDigest>('a'),
        ActorId::new("actor.code-graph-status-issuer").expect("issuer"),
        UtcMicros(1),
        UtcMicros(i64::MAX),
        scope.clone(),
        BTreeSet::from([capability]),
        BTreeSet::from([use_case]),
        DisclosureClass::Evidence,
    )
    .expect("grant");
    RequestContext::new(
        ActorId::new("actor.code-graph-status-requester").expect("requester"),
        scope,
        grant,
        RequestId::new(format!("request.code-graph-status-{suffix}")).expect("request id"),
        Deadline::new(UtcMicros(i64::MAX)).expect("deadline"),
        CancellationContext::active(format!("cancellation.code-graph-status-{suffix}"))
            .expect("cancellation"),
    )
    .expect("request context")
}

fn digest<T>(byte: char) -> T
where
    T: TryFrom<String>,
    <T as TryFrom<String>>::Error: std::fmt::Debug,
{
    T::try_from(format!("sha256:{}", byte.to_string().repeat(64))).expect("digest")
}
