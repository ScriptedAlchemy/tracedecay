//! Graph-activation scheduler tests that need the root session registry.
//!
//! These journeys used to live beside the scheduler. They open
//! `DaemonSessionRuntimeRegistryV1`, so they compile in the composition root.

use std::collections::BTreeSet;
use std::num::NonZeroU64;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use tempfile::TempDir;
use tracedecay_code_index_runtime::CodeGraphSeatRuntimePortV1;
use tracedecay_code_index_runtime::code_index_executor::code_index_search_executor;
use tracedecay_code_index_runtime::code_index_scheduler::{
    CodeGraphActivationAuthorityV1, CodeGraphActivationPolicyV1, CodeIndexDemandAdmissionV1,
    CodeIndexOwnerSignalsV1, CodeIndexReconcileOutcomeV1, CodeIndexSchedulerRegistryV1,
    CodeIndexWorktreeSchedulerV1, RetainedGraphRecoveryPauseV1, SharedCodeIndexBytePoolV1,
    scoped_code_index_store_root,
};
use tracedecay_code_index_runtime::mcp_admission::{
    CodeIndexMcpAdmissionUnavailableV1, CodeIndexMcpReadAdmissionV1, CodeIndexMcpReadGrantV1,
    CodeIndexScopeResolverV1, CodeIndexScopeUnavailableV1,
};
use tracedecay_contracts::{
    CallableCodeOperationKind, CallableCodeQueryPort, CancellationContext, CapabilityGrantId,
    CapabilityGrantSnapshot, CodeQueryScope, CodeRelationRequest, Deadline, DisclosureClass,
    PageRequest, RequestContext, RequestId, ResolvedScope, ResultProjection, RetrievalOrder,
    RetrievalPortContext, RetrievalPortOutcome, RetrievalRequestMeta, callable_code_operation,
    now_micros,
};
use tracedecay_daemon_protocol::DEFAULT_TOOL_REQUEST_DEADLINE;
use tracedecay_domain::{
    ActorId, AuthorizationRevision, CodeGenerationId, ManifestDigest, PrincipalId, ProjectId,
    UtcMicros,
};
use tracedecay_graph_query::{
    CodeGraphReadFreshnessV1, CodeGraphReadRequest, request_graph_cancellation,
};
use tracedecay_query::code_search::{
    CodeIndexLaneStatusV1, CodeIndexSearchAuthorityV1, CodeIndexSearchCoverageV1,
    CodeIndexSearchOutcomeV1, CodeIndexSearchRequestV1,
};
use tracedecay_query::retrieval::lexical::LexicalRoutingV1;
use tracedecay_runtime_core::resident_memory::{
    ProcessResidentMemoryV1, ProcessResidentSampleV1, RESIDENT_OWNER_IDLE_WINDOW_V1,
    ResidentMemoryPressureV1, ResidentOwnerKindV1, ResidentOwnersReportV1, ResidentOwnersV1,
};
use tracedecay_runtime_core::runtime_telemetry::{
    GenerationCensusServingFreshness, GenerationCensusSnapshot, GenerationCensusUnavailableReason,
};
use tracedecay_session_temporal_store::SessionTemporalAccess;
use tracedecay_store_runtime::DaemonSessionRuntimeRegistryV1;
use tracedecay_tool_catalog::{CapabilityId, UseCaseId};

use tracedecay_code_index_runtime::project_reads::{
    project_code_graph_projection_read_port, project_code_index_generation_census_reader,
};
use tracedecay_runtime_core::path_safety::canonical_existing_identity;

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

fn callers_context(scope: ResolvedScope) -> RequestContext {
    let operation =
        callable_code_operation(CallableCodeOperationKind::Callers).expect("callers operation");
    let grant = CapabilityGrantSnapshot::new(
        CapabilityGrantId::new("grant.persistent-graph-cursor").expect("grant id"),
        1,
        ManifestDigest::new(format!("sha256:{}", "a".repeat(64))).expect("grant digest"),
        ActorId::new("actor.code-index.issuer").expect("issuer"),
        UtcMicros(1),
        UtcMicros(i64::MAX),
        scope.clone(),
        BTreeSet::from([operation.capability_id().clone()]),
        BTreeSet::from([operation.use_case_id().clone()]),
        DisclosureClass::Evidence,
    )
    .expect("grant");
    RequestContext::new(
        ActorId::new("actor.code-index.requester").expect("requester"),
        scope,
        grant,
        RequestId::new("request.persistent-graph-cursor").expect("request id"),
        Deadline::new(UtcMicros(i64::MAX)).expect("deadline"),
        CancellationContext::active("cancel.persistent-graph-cursor").expect("cancellation"),
    )
    .expect("request context")
}

fn callers_meta(
    page_size: u32,
    cursor: Option<tracedecay_contracts::OpaqueCursor>,
) -> RetrievalRequestMeta {
    RetrievalRequestMeta::current(
        PageRequest::new(page_size, cursor).expect("callers page"),
        ResultProjection::Evidence,
        RetrievalOrder::Relevance,
    )
}

/// A retained text generation reaches exact/lexical readiness when persistent
/// graph replay is permanently refused. The retained generation still takes
/// the serving seat, because exact and lexical serving never depended on
/// native graph, but its graph readiness stays typed as not ready and no
/// interactive graph store is exposed, so text availability never implies
/// graph availability.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn failed_cold_mount_graph_replay_preserves_retained_text_generation() {
    let fixture = GitFixture::new(ALPHA_LIB_V1);
    let store = TempDir::new().expect("store root");
    let bytes = Arc::new(SharedCodeIndexBytePoolV1::default());
    let scoped_store = scoped_code_index_store_root(
        store.path(),
        &canonical_existing_identity(fixture.path()).expect("canonical fixture"),
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
            graph_runtime.code_graph_seat_port(),
            read_only_project_database,
            CodeGraphActivationPolicyV1::Enabled,
            tracedecay_code_index_runtime::registry_default_index_path_policy(),
        )
        .await
        .expect("mount retained generation");

    let scheduler = registry
        .scheduler_handle(fixture.path())
        .await
        .expect("mounted scheduler");
    let mut signals = CodeIndexOwnerSignalsV1::subscribe(&registry, fixture.path()).await;
    drop(admission);

    while scheduler
        .lock()
        .expect("scheduler lock")
        .sealed_decode_count()
        == 0
    {
        signals
            .changed()
            .await
            .expect("worker did not decode the retained generation for graph replay");
    }

    let text = loop {
        if let Some(text) = registry.latest_text_serving_for_scope(&scope).await
            && text.query_owners_are_ready()
        {
            break text;
        }
        signals
            .changed()
            .await
            .expect("permanently refused graph replay withheld exact and lexical readiness");
    };
    assert!(text.production_query_owners().is_ok());

    assert_eq!(
        registry.latest_generation_id(fixture.path()).await,
        Some(seeded_generation_id.clone()),
        "persistent graph replay failure must not withhold retained text serving"
    );
    // A retryable activation failure no longer withholds the seat: the sealed
    // generation is installed while native graph keeps retrying, so the
    // contract lives on the seated generation's typed graph state.
    let seated = loop {
        if let Some(seated) = registry.latest_complete_serving_for_scope(&scope).await {
            break seated;
        }
        signals
            .changed()
            .await
            .expect("persistent graph replay failure must still seat the retained generation");
    };
    assert_eq!(
        seated.generation().manifest().generation_id,
        seeded_generation_id,
        "the seat must be the retained generation, not a successor"
    );
    assert_ne!(
        seated.code_graph_serving_readiness(),
        tracedecay_contracts::code_index_freshness::CodeGraphServingReadinessV1::Ready,
        "persistent graph replay failure must not report a ready graph"
    );
    assert!(
        seated.interactive_graph_store().is_err(),
        "persistent graph replay failure must not expose an interactive graph store"
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
        &canonical_existing_identity(fixture.path()).expect("canonical fixture"),
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
    tracedecay_project::test_support::host_admission::await_bound_graph_runtime(
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
        seated: Arc::default(),
    };

    let activated = latest.clone();
    let activated_generation = latest.generation().manifest().generation_id.clone();
    assert_eq!(graph_activation.seated_graph_generation(), None);
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
    assert_eq!(
        graph_activation.seated_graph_generation(),
        Some(activated_generation)
    );
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
async fn persistent_callers_cursor_keeps_generation_a_without_repointing_generation_b() {
    let fixture = GitFixture::new(&[(
        "src/lib.rs",
        "pub fn hub() {}\npub fn caller_a() { hub(); }\npub fn caller_b() { hub(); }\n",
    )]);
    let store = TempDir::new().expect("store root");
    let scoped_store = scoped_code_index_store_root(
        store.path(),
        &canonical_existing_identity(fixture.path()).expect("canonical fixture"),
    );
    let (latest_a, replay_a, scope, hub) = {
        let mut scheduler = scheduler(
            &fixture,
            scoped_store,
            Arc::new(SharedCodeIndexBytePoolV1::default()),
        );
        published(scheduler.reconcile_now().expect("publish generation A"));
        let latest = scheduler.latest_complete().expect("generation A");
        let replay = scheduler
            .code_graph_replay_binding(&latest.generation().manifest().generation_id)
            .expect("generation A replay binding");
        let snapshot = latest.generation().snapshot();
        let scope = ResolvedScope::new(
            test_project_id(),
            snapshot.repository.clone(),
            snapshot.worktree.clone().expect("worktree id"),
            snapshot.reference.clone(),
        )
        .expect("resolved scope");
        let hub = latest
            .generation()
            .symbols()
            .symbols
            .iter()
            .find(|symbol| symbol.qualified_name.ends_with("hub"))
            .expect("hub symbol")
            .occurrence
            .as_str()
            .to_owned();
        (latest, replay, scope, hub)
    };
    let generation_a = latest_a.generation().manifest().generation_id.clone();
    let project_id = test_project_id();
    let profile = TempDir::new().expect("profile root");
    let profile_root = profile.path().join("profile");
    tracedecay_runtime_core::storage::pin_fixture_repository_identity(
        fixture.path(),
        project_id.as_str(),
    )
    .expect("project enrollment");
    let identity = tracedecay_daemon_identity::profile_identity::load_or_create(&profile_root)
        .expect("profile identity");
    let _database_scope = tracedecay_runtime_core::db::enter_daemon_database_scope(
        &profile_root,
        96,
        "persistent historical graph cursor",
    )
    .expect("database scope");
    let graph_runtime = Arc::new(
        DaemonSessionRuntimeRegistryV1::open(identity)
            .await
            .expect("graph runtime registry"),
    );
    let project_database = graph_runtime
        .project_memory(project_id.clone(), [fixture.path().to_path_buf()])
        .await
        .expect("project database");
    tracedecay_project::test_support::host_admission::await_bound_graph_runtime(
        &project_database,
        "bind persistent historical cursor graph runtime",
    )
    .await
    .expect("bound graph runtime");
    let activation = CodeGraphActivationAuthorityV1::Persistent {
        runtime: graph_runtime.code_graph_seat_port(),
        project_database: Arc::clone(&project_database),
        policy: Arc::new(std::sync::atomic::AtomicBool::new(true)),
        seated: Arc::default(),
    };
    activation
        .activate(
            &project_id,
            &scope.repository_id,
            &scope.worktree_id,
            latest_a,
            replay_a,
            Arc::new(std::sync::atomic::AtomicBool::new(false)),
        )
        .await
        .expect("activate generation A");

    let registry = CodeIndexSchedulerRegistryV1::with_background_reconcile_permits(1, 1);
    registry
        .mount_worktree_with_graph_runtime(
            project_id,
            fixture.path(),
            store.path().to_path_buf(),
            graph_runtime.code_graph_seat_port(),
            Arc::clone(&project_database),
            CodeGraphActivationPolicyV1::Enabled,
            tracedecay_code_index_runtime::registry_default_index_path_policy(),
        )
        .await
        .expect("mount persistent generation A");
    let ready_deadline = std::time::Instant::now() + Duration::from_secs(20);
    loop {
        if registry
            .retained_text_owner_freshness_for_scope(&scope)
            .await
            .is_some_and(|(latest, current)| current && latest.interactive_graph_store().is_ok())
        {
            break;
        }
        assert!(
            std::time::Instant::now() <= ready_deadline,
            "generation A graph did not become ready"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    // A recovered head seats no decoded generation, so only the seated graph
    // keeps generation A a retention root for its successor's refresh.
    let mounted_scope = registry
        .serving_code_scope(fixture.path())
        .await
        .expect("mounted scope");
    assert_eq!(mounted_scope.graph_generation, Some(generation_a.clone()));
    let sessions = graph_runtime
        .profile_sessions()
        .await
        .expect("profile session database");
    let cursor_keys = SessionTemporalAccess::new(&*sessions)
        .load_session_cursor_key_provider_result()
        .await
        .expect("cursor keys");
    tracedecay_code_index_runtime::code_index_scheduler::query_runtime::mount_core_query_authority_on_project_open(
        &registry,
        fixture.path(),
        &scope,
        &cursor_keys,
    )
    .await
    .expect("mount query authority");
    let operation =
        callable_code_operation(CallableCodeOperationKind::Callers).expect("callers operation");
    let context = callers_context(scope.clone());
    let query_scope = CodeQueryScope::new(
        CodeGenerationId::new(tracedecay_contracts::UNPINNED_LATEST_GENERATION_SENTINEL)
            .expect("unpinned generation"),
        None,
    )
    .expect("query scope");
    let first = registry
        .callers(
            RetrievalPortContext {
                request: &context,
                operation: &operation,
            },
            &CodeRelationRequest {
                node_id: hub.clone(),
                maximum_depth: 1,
                resolve_trait_dispatch: false,
                scope: query_scope.clone(),
                meta: callers_meta(1, None),
            },
        )
        .await;
    let first = match first {
        RetrievalPortOutcome::Completed(evidence) => evidence.payload.expect("generation A page"),
        other => panic!("generation A callers unavailable: {other:?}"),
    };
    let first_caller = first
        .items
        .first()
        .expect("generation A page 1 caller")
        .symbol
        .node_id
        .clone();
    let cursor_a = first.next_cursor.expect("generation A cursor");
    assert_eq!(first.generation, generation_a);

    fixture.edit(
        "src/lib.rs",
        "pub fn hub() {}\npub fn caller_a() { hub(); }\npub fn caller_b() { hub(); }\npub fn caller_c() { hub(); }\n",
    );
    git(fixture.path(), &["commit", "-qam", "publish generation B"]);
    let (generation_b, hub_b) = loop {
        if let Some((latest, true)) = registry
            .retained_text_owner_freshness_for_scope(&scope)
            .await
            && latest.metadata().manifest().generation_id != generation_a
            && let Ok(store) = latest.interactive_graph_store()
            && let Ok(reader) = store.interactive_reader_with_cancellation(
                &latest.metadata().manifest().generation_id,
                Arc::new(tracedecay_graph_db::NeverCancelled),
            )
            && let Ok(hubs) = reader.resolve_simple_name(
                "hub",
                None,
                2,
                Arc::new(tracedecay_graph_db::NeverCancelled),
            )
            && hubs.len() == 1
        {
            break (
                latest.metadata().manifest().generation_id.clone(),
                hubs[0].occurrence.as_str().to_owned(),
            );
        }
        assert!(
            std::time::Instant::now() <= ready_deadline + Duration::from_secs(20),
            "generation B graph did not become ready"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    };

    let continuation = registry
        .callers(
            RetrievalPortContext {
                request: &context,
                operation: &operation,
            },
            &CodeRelationRequest {
                node_id: hub.clone(),
                maximum_depth: 1,
                resolve_trait_dispatch: false,
                scope: query_scope.clone(),
                meta: callers_meta(1, Some(cursor_a)),
            },
        )
        .await;
    let continuation = match continuation {
        RetrievalPortOutcome::Completed(evidence) => evidence.payload.expect("generation A page 2"),
        other => panic!("generation A cursor did not continue: {other:?}"),
    };
    assert_eq!(continuation.generation, generation_a);
    assert_eq!(continuation.total, Some(2));
    assert_ne!(
        continuation
            .items
            .first()
            .expect("generation A page 2 caller")
            .symbol
            .node_id,
        first_caller,
    );

    let current = registry
        .callers(
            RetrievalPortContext {
                request: &context,
                operation: &operation,
            },
            &CodeRelationRequest {
                node_id: hub_b,
                maximum_depth: 1,
                resolve_trait_dispatch: false,
                scope: query_scope,
                meta: callers_meta(4, None),
            },
        )
        .await;
    let current = match current {
        RetrievalPortOutcome::Completed(evidence) => evidence.payload.expect("generation B page"),
        other => panic!("generation B no longer served after A recovery: {other:?}"),
    };
    assert_eq!(current.generation, generation_b);
    assert_eq!(current.total, Some(3));
    assert!(current.items.iter().any(|item| {
        item.symbol
            .qualified_name
            .rsplit("::")
            .next()
            .is_some_and(|name| name == "caller_c")
    }));

    registry.shutdown().await;
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
async fn corrupt_graph_restart_rebuilds_from_the_sealed_segments_without_decoding() {
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
        &canonical_existing_identity(fixture.path()).expect("canonical fixture"),
    );
    let (
        scope,
        seeded_generation_id,
        seeded_statistics,
        latest,
        replay_binding,
        repository_id,
        worktree_id,
    ) = {
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
        let statistics = latest
            .generation()
            .generation_statistics()
            .expect("seed generation statistics");
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
            statistics,
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
    tracedecay_project::test_support::host_admission::await_bound_graph_runtime(
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
        )
        .await
        .expect("retain seeded graph runtime");
    let seeded_graph = retained
        .publish_verified_snapshot(Arc::new(std::sync::atomic::AtomicBool::new(false)))
        .expect("publish graph head before restart");
    drop(seeded_graph);
    drop(retained);
    drop(latest);
    // The daemon's terminal order: the publication's staging sweep holds the
    // project graph store until it is joined, and a restart opened beside it
    // cannot take the store's write lock.
    graph_runtime
        .shutdown_terminal_tasks()
        .await
        .expect("join graph terminal tasks before restart");
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
    tracedecay_project::test_support::host_admission::await_bound_graph_runtime(
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
                .pause_next_retained_graph_recovery(
                    canonical_existing_identity(fixture.path()).expect("canonical fixture"),
                    RetainedGraphRecoveryPauseV1::BeforeSuccessor,
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
            graph_runtime.code_graph_seat_port(),
            project_database,
            CodeGraphActivationPolicyV1::Enabled,
            tracedecay_code_index_runtime::registry_default_index_path_policy(),
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
            "the retained partitioned graph must recover before any partition decode"
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
                if let Ok(stale_read) = stale_read.as_ref()
                    && matches!(
                        stale_read.freshness(),
                        CodeGraphReadFreshnessV1::LastCompleteStale { .. }
                    )
                {
                    assert_eq!(
                        stale_read.generation(),
                        &seeded_generation_id,
                        "the stale graph read must still belong to the retained generation"
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
    let seated_census = if corrupt_graph || dirty_before_restart {
        // A decoded census is a strictly later state than the text-serving
        // head this case already settled on: `latest_complete_ready_decoded_*`
        // abstains, returning no decoded owner at all, while a reconcile
        // pass is in flight or the scheduler mutex is momentarily held, and
        // that abstention reads back as a statistics-free projection. Sampling
        // once therefore observes a non-terminal state on any host where the
        // rebuilt successor decodes after its text head starts serving.
        let census_deadline = std::time::Instant::now() + Duration::from_secs(10);
        loop {
            let settled_census = census().await;
            // Freshness alone would anchor the later lock-held comparison to
            // whatever generation happened to decode first. The seated head is
            // already pinned above: repairing a corrupt retained graph keeps
            // the seeded identity, while a dirty checkout rebuilds into a
            // successor, so the census must carry that same identity.
            let seats_the_settled_generation = match &settled_census {
                GenerationCensusSnapshot::Observed {
                    generation_id,
                    freshness: GenerationCensusServingFreshness::Current,
                    ..
                } if corrupt_graph => generation_id.as_str() == seeded_generation_id.as_str(),
                GenerationCensusSnapshot::Observed {
                    generation_id,
                    freshness: GenerationCensusServingFreshness::Current,
                    ..
                } => generation_id.as_str() != seeded_generation_id.as_str(),
                _ => false,
            };
            if seats_the_settled_generation {
                break settled_census;
            }
            assert!(
                std::time::Instant::now() <= census_deadline,
                "no current decoded census for the settled generation \
                 (corrupt_graph={corrupt_graph}, dirty_before_restart={dirty_before_restart}, \
                 seeded={seeded_generation_id:?}): {settled_census:?}"
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    } else {
        let settled_census = census().await;
        assert_eq!(
            settled_census,
            GenerationCensusSnapshot::Observed {
                generation_id: seeded_generation_id.as_str().to_owned(),
                freshness: GenerationCensusServingFreshness::Current,
                statistics:
                    tracedecay_runtime_core::runtime_telemetry::GenerationCensusStatistics {
                        source_total_bytes: seeded_statistics.source_total_bytes,
                        symbol_count: seeded_statistics.symbol_count,
                        edge_count: seeded_statistics.edge_count,
                    },
            },
            "clean restart must retain the exact authenticated generation census"
        );
        settled_census
    };

    let scheduler = registry
        .scheduler_handle(fixture.path())
        .await
        .expect("mounted scheduler");
    let sealed_decode_count = scheduler
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .sealed_decode_count();
    if corrupt_graph {
        assert_eq!(
            sealed_decode_count, 0,
            "a corrupt retained graph is rebuilt from its sealed segments and seated from the \
             text owner, without decoding the generation"
        );
    } else if !dirty_before_restart {
        assert_eq!(
            sealed_decode_count, 0,
            "clean restart must seat the verified graph head without replaying partition segments"
        );
        assert_eq!(
            settled_read.generation(),
            &seeded_generation_id,
            "graph requests must preserve the restored publication identity"
        );
        assert!(
            Arc::ptr_eq(
                settled_read.store(),
                &settled.interactive_graph_store().expect("restored graph"),
            ),
            "graph requests must reuse the recovered graph authority"
        );
        let reader = settled_read
            .reader(&settled_context, now_micros())
            .expect("admitted reader for the recovered graph");
        let page = reader
            .symbols_page(None, 16, request_graph_cancellation(&settled_context))
            .expect("bounded symbol query on the recovered graph");
        assert!(!page.has_more, "the single-function fixture fits one page");
        assert!(
            page.symbols.iter().any(|symbol| symbol
                .metadata
                .as_ref()
                .is_some_and(|metadata| metadata.simple_name == "alpha")),
            "the recovered graph must return the fixture function: {:?}",
            page.symbols
        );
        assert_eq!(
            scheduler
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .sealed_decode_count(),
            0,
            "a graph query after clean restart must not replay partition segments"
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
    // After a dirty restart the seated head is the rebuilt successor, not
    // the recovered seed. A lock-held tip move must keep serving that seated
    // generation as LastCompleteStale with the same decoded statistics.
    match (&census_snapshot, &seated_census) {
        (
            GenerationCensusSnapshot::Observed {
                generation_id,
                freshness: GenerationCensusServingFreshness::LastCompleteStale { .. },
                statistics,
            },
            GenerationCensusSnapshot::Observed {
                generation_id: seated_id,
                statistics: seated_statistics,
                ..
            },
        ) if generation_id == seated_id && statistics == seated_statistics => {}
        (other, seated) => panic!(
            "seated-graph census while the scheduler lock is held: {other:?}; seated={seated:?}"
        ),
    }
}

/// A restart must seat the retained verified graph head while its text owner
/// is still projecting, and answer symbol-graph identity from that retained
/// manifest instead of refusing until the lexical artifact finishes.
///
/// The operator journey behind this: a daemon restarted onto a store whose
/// lexical artifact had not finished its finalization index build. The graph
/// seat waited behind that build (377 s measured on a 5,181-file corpus; 15+
/// minutes on the reporter's store) even though verified-head recovery costs
/// seconds, and for the whole window `code_symbol_search` refused with
/// `lsp-code-index-generation-unavailable` while code-generation retention
/// degraded on an incomplete vector census (issue #1244).
///
/// The fixture seeds a generation *without* building its text artifact, so the
/// restart must project one, and holds that projection at its first advance.
/// Everything asserted below happens while it is held.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn restart_seats_the_retained_graph_while_its_text_owner_still_projects() {
    use tracedecay_application::lsp_runtime::LspCodeIndexProjectionIdentityPort;

    let RestartedPublishedGraphHead {
        fixture,
        canonical_fixture,
        store,
        scope,
        seeded_generation_id,
        graph_runtime,
        project_database,
        _database_scope,
        _profile,
    } = restart_with_published_graph_head(TextArtifact::ProjectedOnRestart).await;

    let registry = CodeIndexSchedulerRegistryV1::with_background_reconcile_permits(1, 1);
    let (projecting, release_projection) = registry
        .pause_next_retained_text_projection(canonical_fixture.clone())
        .await;
    let (recovered, release_successor) = registry
        .pause_next_retained_graph_recovery(
            canonical_fixture.clone(),
            RetainedGraphRecoveryPauseV1::BeforeSuccessor,
        )
        .await;
    registry
        .mount_worktree_with_graph_runtime(
            test_project_id(),
            fixture.path(),
            store.path().to_path_buf(),
            graph_runtime.code_graph_seat_port(),
            project_database,
            CodeGraphActivationPolicyV1::Enabled,
            tracedecay_code_index_runtime::registry_default_index_path_policy(),
        )
        .await
        .expect("mount restarted retained generation");

    tokio::time::timeout(Duration::from_secs(20), projecting)
        .await
        .expect("the restart never started projecting its retained text owner")
        .expect("retained text projection gate dropped before observation");
    // The seat must not wait for the projection this gate is holding.
    tokio::time::timeout(Duration::from_secs(20), recovered)
        .await
        .expect("the retained graph head did not recover while its text owner was projecting")
        .expect("retained graph recovery gate dropped before observation");

    assert!(
        registry
            .latest_text_serving_for_root(&canonical_fixture)
            .await
            .is_none(),
        "the gate must still be holding exact and lexical serving: without that, this journey \
         would prove nothing about seating ahead of the text build"
    );
    let identity_owner = registry
        .retained_text_owner_for_root(&canonical_fixture)
        .await
        .expect("a warming text owner still answers identity reads");
    assert_eq!(
        identity_owner.metadata().manifest().generation_id,
        seeded_generation_id,
        "the restart must recover the pre-restart generation, not a successor"
    );
    assert!(
        identity_owner.interactive_graph_store().is_ok(),
        "the recovered verified head must serve graph reads while text still projects"
    );
    let symbol_graph_identity = registry
        .current_identity(canonical_fixture.clone(), None)
        .await
        .expect("symbol-graph identity must resolve from the retained manifest");
    assert_eq!(
        symbol_graph_identity.code_generation_id, seeded_generation_id,
        "symbol-graph identity must name the retained generation"
    );
    let port = project_code_graph_projection_read_port(
        registry.clone(),
        fixture.path().to_path_buf(),
        scope.clone(),
    );
    let warming_context = graph_request_context(scope.clone(), "retained-seat-while-text-warms");
    let warming_read = port
        .open(CodeGraphReadRequest::from_context(
            &warming_context,
            now_micros(),
        ))
        .await
        .expect("the recovered graph must serve while its text owner projects");
    assert_eq!(
        warming_read.generation(),
        &seeded_generation_id,
        "graph reads must name the retained generation"
    );

    release_successor
        .send(())
        .expect("release the successor after the seat observation");
    assert!(
        registry
            .request_complete_generation(&canonical_fixture)
            .await,
        "branch publication demand reaches the mounted retained owner"
    );
    let complete_deadline = std::time::Instant::now() + Duration::from_secs(20);
    loop {
        if let Some(complete) = registry.latest_complete_serving_for_scope(&scope).await {
            assert_eq!(
                complete.generation().manifest().generation_id,
                seeded_generation_id,
                "late complete demand must seat the recovered generation"
            );
            break;
        }
        assert!(
            std::time::Instant::now() <= complete_deadline,
            "complete demand that arrived during retained text projection stayed stranded"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(
        registry
            .latest_text_serving_for_root(&canonical_fixture)
            .await
            .is_none(),
        "complete generation replay must not fabricate finished lexical projection"
    );

    release_projection
        .send(())
        .expect("release the held text projection");
    let ready_deadline = std::time::Instant::now() + Duration::from_secs(20);
    loop {
        if let Some(ready) = registry
            .latest_text_serving_for_root(&canonical_fixture)
            .await
        {
            assert_eq!(
                ready.metadata().manifest().generation_id,
                seeded_generation_id,
                "the released projection must finish the retained generation's own artifact"
            );
            break;
        }
        assert!(
            std::time::Instant::now() <= ready_deadline,
            "the released text projection never reached exact and lexical readiness"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    registry.shutdown().await;
    graph_runtime
        .shutdown_memory_graph_reconciliation_tasks()
        .await
        .expect("join graph reconciliation tasks");
}

/// Whether the seeded generation's durable text artifact exists before the
/// restart.
#[derive(Clone, Copy, PartialEq, Eq)]
enum TextArtifact {
    /// A settled store: the restart only reopens the sealed text owners.
    SealedBeforeRestart,
    /// The restart finds a retained owner whose lexical artifact still has to
    /// be built.
    ProjectedOnRestart,
}

/// A sealed generation whose verified graph head was published before a
/// daemon restart, reopened on the restarted graph runtime the way a daemon
/// reopens it. The code-index registry is left for the caller to mount.
struct RestartedPublishedGraphHead {
    fixture: GitFixture,
    canonical_fixture: PathBuf,
    store: TempDir,
    scope: ResolvedScope,
    seeded_generation_id: CodeGenerationId,
    graph_runtime: Arc<DaemonSessionRuntimeRegistryV1>,
    project_database: Arc<tracedecay_runtime_core::db::Database>,
    _database_scope: tracedecay_runtime_core::db::DaemonDatabaseScope,
    _profile: TempDir,
}

async fn restart_with_published_graph_head(
    text_artifact: TextArtifact,
) -> RestartedPublishedGraphHead {
    let fixture = GitFixture::new(ALPHA_LIB_V1);
    let canonical_fixture = canonical_existing_identity(fixture.path()).expect("canonical fixture");
    let store = TempDir::new().expect("store root");
    let scoped_store = scoped_code_index_store_root(store.path(), &canonical_fixture);
    let mut scheduler = scheduler(
        &fixture,
        scoped_store,
        Arc::new(SharedCodeIndexBytePoolV1::default()),
    );
    published(scheduler.reconcile_now().expect("seed generation"));
    let latest = scheduler.latest_complete().expect("seeded generation");
    if text_artifact == TextArtifact::SealedBeforeRestart {
        latest
            .production_query_owners()
            .expect("seal the durable text artifact before restart");
    }
    let seeded_generation_id = latest.generation().manifest().generation_id.clone();
    let replay_binding = scheduler
        .code_graph_replay_binding(&seeded_generation_id)
        .expect("seed graph replay binding");
    drop(scheduler);
    let snapshot = latest.generation().snapshot();
    let repository_id = snapshot.repository.clone();
    let worktree_id = snapshot.worktree.clone().expect("worktree identity");
    let scope = ResolvedScope::new(
        test_project_id(),
        repository_id.clone(),
        worktree_id.clone(),
        snapshot.reference.clone(),
    )
    .expect("resolved scope");

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
    let database_scope = tracedecay_runtime_core::db::enter_daemon_database_scope(
        &profile_root,
        95,
        "restart with published graph head",
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
    tracedecay_project::test_support::host_admission::await_bound_graph_runtime(
        &project_database,
        "bind graph projection before restart",
    )
    .await
    .expect("bound project graph runtime");
    let retained = graph_runtime
        .retain_code_graph_runtime(
            project_id.clone(),
            repository_id,
            worktree_id,
            snapshot.reference.clone(),
            seeded_generation_id.clone(),
            Arc::clone(&project_database),
            replay_binding,
        )
        .await
        .expect("retain seeded graph runtime");
    drop(
        retained
            .publish_verified_snapshot(Arc::new(std::sync::atomic::AtomicBool::new(false)))
            .expect("publish graph head before restart"),
    );
    drop(retained);
    drop(latest);
    // The daemon's terminal order: the publication's staging sweep holds the
    // project graph store until it is joined.
    graph_runtime
        .shutdown_terminal_tasks()
        .await
        .expect("join graph terminal tasks before restart");
    graph_runtime
        .shutdown_memory_graph_reconciliation_tasks()
        .await
        .expect("stop graph runtime before restart");
    drop(project_database);
    drop(graph_runtime);

    let restarted_identity =
        tracedecay_daemon_identity::profile_identity::load_or_create(&profile_root)
            .expect("restart profile identity");
    let graph_runtime = Arc::new(
        DaemonSessionRuntimeRegistryV1::open(restarted_identity)
            .await
            .expect("restarted graph runtime registry"),
    );
    let project_database = graph_runtime
        .project_memory(project_id, [fixture.path().to_path_buf()])
        .await
        .expect("restarted writable project database");
    tracedecay_project::test_support::host_admission::await_bound_graph_runtime(
        &project_database,
        "bind restarted graph projection",
    )
    .await
    .expect("bound restarted project graph runtime");
    RestartedPublishedGraphHead {
        fixture,
        canonical_fixture,
        store,
        scope,
        seeded_generation_id,
        graph_runtime,
        project_database,
        _database_scope: database_scope,
        _profile: profile,
    }
}

/// The interactive search budget a restarted daemon must meet.
const SEARCH_BUDGET: Duration = Duration::from_secs(5);

#[derive(Clone)]
struct FixtureSearchGrant(CodeIndexSearchAuthorityV1);

impl CodeIndexMcpReadGrantV1 for FixtureSearchGrant {
    fn authorize(
        &self,
        _scope: &ResolvedScope,
        _authority: Option<&CodeIndexSearchAuthorityV1>,
    ) -> Result<CodeIndexSearchAuthorityV1, CodeIndexMcpAdmissionUnavailableV1> {
        Ok(self.0.clone())
    }

    fn search_authority(&self) -> CodeIndexSearchAuthorityV1 {
        self.0.clone()
    }
}

#[derive(Clone)]
struct FixtureSearchAdmission(CodeIndexSearchAuthorityV1);

impl CodeIndexMcpReadAdmissionV1 for FixtureSearchAdmission {
    type Grant = FixtureSearchGrant;

    fn route_is_registered(&self) -> bool {
        true
    }

    fn admit_current(
        &self,
        _scope: &ResolvedScope,
    ) -> Result<Self::Grant, CodeIndexMcpAdmissionUnavailableV1> {
        Ok(FixtureSearchGrant(self.0.clone()))
    }
}

#[derive(Clone)]
struct FixtureScopeResolver(ResolvedScope);

impl CodeIndexScopeResolverV1 for FixtureScopeResolver {
    fn resolved_scope_for_project(
        &self,
        _project_root: &Path,
        _project_id: &ProjectId,
    ) -> Result<ResolvedScope, CodeIndexScopeUnavailableV1> {
        Ok(self.0.clone())
    }
}

fn restart_search_request(project_root: &Path) -> CodeIndexSearchRequestV1 {
    let deadline_micros = i64::try_from(DEFAULT_TOOL_REQUEST_DEADLINE.as_micros())
        .expect("tool deadline fits in microseconds");
    CodeIndexSearchRequestV1 {
        project_root: project_root.to_path_buf(),
        query: "alpha".to_owned(),
        source_revision: None,
        source_tree: None,
        source_reference: None,
        limit: 8,
        cursor: None,
        lexical_routing: LexicalRoutingV1::default(),
        authority: None,
        deadline: Some(
            Deadline::new(UtcMicros(now_micros().0 + deadline_micros)).expect("tool deadline"),
        ),
        cancellation: None,
    }
}

/// The served symbols, as (name, path), and the lane coverage of one search.
fn served(outcome: CodeIndexSearchOutcomeV1) -> (Vec<(String, String)>, CodeIndexSearchCoverageV1) {
    let CodeIndexSearchOutcomeV1::Complete(served) = outcome else {
        panic!("search must serve: {outcome:?}");
    };
    (
        served
            .display_by_anchor
            .into_values()
            .map(|display| (display.name, display.path))
            .collect(),
        served.coverage,
    )
}

/// The first search after a restart of a daemon whose sealed graph is still
/// recovering its verified head answers exact and lexical within the search
/// budget and marks the graph lane warming, then serves the graph once it
/// seats. It used to wait out the whole graph-head recovery (26.9 s on the
/// beta.63 battery corpus), because its wait for the retained generation
/// required graph readiness that exact and lexical never need.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn first_search_after_restart_serves_text_while_the_graph_head_recovers() {
    let RestartedPublishedGraphHead {
        fixture,
        canonical_fixture,
        store,
        scope,
        seeded_generation_id,
        graph_runtime,
        project_database,
        _database_scope,
        _profile,
    } = restart_with_published_graph_head(TextArtifact::SealedBeforeRestart).await;

    // Hold both halves of the restart open: the text owners opening, as on a
    // freshly restarted daemon, and the graph-head recovery, which a large
    // store holds open by itself for tens of seconds.
    let registry = CodeIndexSchedulerRegistryV1::with_background_reconcile_permits(1, 1);
    let (projecting, release_projection) = registry
        .pause_next_retained_text_projection(canonical_fixture.clone())
        .await;
    let (recovering, release_recovery) = registry
        .pause_next_retained_graph_recovery(
            canonical_fixture.clone(),
            RetainedGraphRecoveryPauseV1::BeforeHeadRecovery,
        )
        .await;
    registry
        .mount_worktree_with_graph_runtime(
            test_project_id(),
            fixture.path(),
            store.path().to_path_buf(),
            graph_runtime.code_graph_seat_port(),
            project_database,
            CodeGraphActivationPolicyV1::Enabled,
            tracedecay_code_index_runtime::registry_default_index_path_policy(),
        )
        .await
        .expect("mount restarted retained generation");
    tokio::time::timeout(Duration::from_secs(20), projecting)
        .await
        .expect("the restart never started opening its retained text owners")
        .expect("retained text projection gate dropped before observation");
    tokio::time::timeout(Duration::from_secs(20), recovering)
        .await
        .expect("the restart never started recovering its retained graph head")
        .expect("retained graph head recovery gate dropped before observation");

    // Project open's deferred mount installs the query authority from the
    // retained text owner alone.
    let sessions = graph_runtime
        .profile_sessions()
        .await
        .expect("profile session database");
    let cursor_keys = SessionTemporalAccess::new(&*sessions)
        .load_session_cursor_key_provider_result()
        .await
        .expect("cursor keys");
    tracedecay_code_index_runtime::code_index_scheduler::query_runtime::mount_core_query_authority_on_project_open(
        &registry,
        fixture.path(),
        &scope,
        &cursor_keys,
    )
    .await
    .expect("mount query authority from the retained text owner");

    let authority = CodeIndexSearchAuthorityV1 {
        principal: PrincipalId::new("principal.restart-first-search").expect("principal"),
        authorization_revision: AuthorizationRevision::new("authorization.restart-first-search")
            .expect("authorization revision"),
    };
    let executor = code_index_search_executor(
        registry.clone(),
        test_project_id(),
        FixtureSearchAdmission(authority),
        FixtureScopeResolver(scope.clone()),
    );
    let first = tokio::spawn(executor(restart_search_request(fixture.path())));
    release_projection
        .send(())
        .expect("release the retained text projection");
    let first = tokio::time::timeout(SEARCH_BUDGET, first)
        .await
        .expect("the first search after restart must answer within the search budget")
        .expect("first search task");
    assert_eq!(
        served(first),
        (
            vec![("alpha".to_owned(), "src/lib.rs".to_owned())],
            CodeIndexSearchCoverageV1 {
                graph: CodeIndexLaneStatusV1::Unavailable {
                    reason: "graph_warming",
                },
                // The held pass has not re-proven the retained generation
                // against the checkout yet.
                ..CodeIndexSearchCoverageV1::stale(seeded_generation_id.as_str())
            },
        ),
        "exact and lexical serve while the graph head is still recovering"
    );

    release_recovery
        .send(())
        .expect("release the retained graph head recovery");
    let seat_deadline = std::time::Instant::now() + Duration::from_secs(20);
    while registry
        .retained_text_owner_for_root(&canonical_fixture)
        .await
        .is_none_or(|text| text.interactive_graph_store().is_err())
    {
        assert!(
            std::time::Instant::now() <= seat_deadline,
            "the released graph head never seated"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let seated = tokio::time::timeout(
        SEARCH_BUDGET,
        executor(restart_search_request(fixture.path())),
    )
    .await
    .expect("search after the graph seats answers within the search budget");
    assert_eq!(
        served(seated),
        (
            vec![("alpha".to_owned(), "src/lib.rs".to_owned())],
            CodeIndexSearchCoverageV1::warm(),
        ),
        "every lane serves once the recovered head seats"
    );

    registry.shutdown().await;
    graph_runtime
        .shutdown_memory_graph_reconciliation_tasks()
        .await
        .expect("join graph reconciliation tasks");
}

/// A first index reaches `ready` (fresh, graph serving) with the graph head
/// seated from the sealed manifest and the mapped graph store alone: no
/// reader asked for the whole decoded generation, so none is decoded, and
/// graph reads and the generation census answer from the text owner.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn first_index_serves_graph_reads_without_decoding_the_generation() {
    let fixture = GitFixture::new(ALPHA_LIB_V1);
    let store = TempDir::new().expect("store root");
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
        97,
        "first index graph serving without decode",
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
    tracedecay_project::test_support::host_admission::await_bound_graph_runtime(
        &project_database,
        "bind first index graph runtime",
    )
    .await
    .expect("bound project graph runtime");

    let registry = CodeIndexSchedulerRegistryV1::with_background_reconcile_permits(1, 1);
    registry
        .mount_worktree_with_graph_runtime(
            project_id.clone(),
            fixture.path(),
            store.path().to_path_buf(),
            graph_runtime.code_graph_seat_port(),
            project_database,
            CodeGraphActivationPolicyV1::Enabled,
            tracedecay_code_index_runtime::registry_default_index_path_policy(),
        )
        .await
        .expect("mount first index");
    let reached = registry
        .wait_for_readiness(
            fixture.path(),
            tracedecay_contracts::code_index_freshness::CodeIndexReadinessTargetV1::Ready,
            Duration::from_secs(30),
        )
        .await
        .expect("readiness read");
    assert!(
        matches!(
            reached,
            tracedecay_contracts::code_index_freshness::CodeIndexReadinessWaitReadV1::Reached { .. }
        ),
        "the first index must reach fresh with graph serving: {reached:?}"
    );
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
        "no reader demanded the whole generation, so it is never decoded"
    );

    let text = registry
        .retained_text_owner_for_root(fixture.path())
        .await
        .expect("text owner");
    let snapshot = text.metadata().snapshot();
    let scope = ResolvedScope::new(
        project_id,
        snapshot.repository.clone(),
        snapshot.worktree.clone().expect("worktree identity"),
        snapshot.reference.clone(),
    )
    .expect("resolved scope");
    let generation_id = text.metadata().manifest().generation_id.clone();
    let statistics = text.metadata().generation_statistics().clone();
    let port = project_code_graph_projection_read_port(
        registry.clone(),
        fixture.path().to_path_buf(),
        scope.clone(),
    );
    let context = graph_request_context(scope.clone(), "first-index");
    let read = port
        .open(CodeGraphReadRequest::from_context(&context, now_micros()))
        .await
        .expect("graph read on the first index");
    assert_eq!(read.freshness(), CodeGraphReadFreshnessV1::Current);
    assert_eq!(read.generation(), &generation_id);
    let page = read
        .reader(&context, now_micros())
        .expect("admitted graph reader")
        .symbols_page(None, 16, request_graph_cancellation(&context))
        .expect("bounded symbol query");
    assert_eq!(
        page.symbols
            .iter()
            .filter_map(|symbol| symbol.metadata.as_ref())
            .map(|metadata| metadata.simple_name.as_str())
            .collect::<Vec<_>>(),
        ["alpha"]
    );
    let census =
        project_code_index_generation_census_reader(registry.clone(), fixture.path().into(), scope);
    assert_eq!(
        census().await,
        GenerationCensusSnapshot::Observed {
            generation_id: generation_id.as_str().to_owned(),
            freshness: GenerationCensusServingFreshness::Current,
            statistics: tracedecay_runtime_core::runtime_telemetry::GenerationCensusStatistics {
                source_total_bytes: 28,
                symbol_count: statistics.symbol_count,
                edge_count: statistics.edge_count,
            },
        }
    );
    assert_eq!(
        scheduler
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .sealed_decode_count(),
        0,
        "graph reads and the census do not decode the generation"
    );
    registry.shutdown().await;
    graph_runtime
        .shutdown_memory_graph_reconciliation_tasks()
        .await
        .expect("join graph reconciliation tasks");
}

/// A worktree mounted the way the daemon mounts it, on the persistent graph
/// runtime, and fresh with its graph serving.
struct PersistentGraphMountV1 {
    registry: CodeIndexSchedulerRegistryV1,
    graph_runtime: Arc<DaemonSessionRuntimeRegistryV1>,
    scope: ResolvedScope,
    generation_id: CodeGenerationId,
    _database_scope: tracedecay_runtime_core::db::DaemonDatabaseScope,
    _profile: TempDir,
}

impl PersistentGraphMountV1 {
    async fn mount(
        registry: CodeIndexSchedulerRegistryV1,
        fixture: &GitFixture,
        store: &TempDir,
        election: &str,
    ) -> Self {
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
        let database_scope =
            tracedecay_runtime_core::db::enter_daemon_database_scope(&profile_root, 97, election)
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
        tracedecay_project::test_support::host_admission::await_bound_graph_runtime(
            &project_database,
            "bind persistent graph runtime",
        )
        .await
        .expect("bound project graph runtime");
        registry
            .mount_worktree_with_graph_runtime(
                project_id.clone(),
                fixture.path(),
                store.path().to_path_buf(),
                graph_runtime.code_graph_seat_port(),
                project_database,
                CodeGraphActivationPolicyV1::Enabled,
                tracedecay_code_index_runtime::registry_default_index_path_policy(),
            )
            .await
            .expect("mount worktree");
        let reached = registry
            .wait_for_readiness(
                fixture.path(),
                tracedecay_contracts::code_index_freshness::CodeIndexReadinessTargetV1::Ready,
                Duration::from_secs(30),
            )
            .await
            .expect("readiness read");
        assert!(
            matches!(
                reached,
                tracedecay_contracts::code_index_freshness::CodeIndexReadinessWaitReadV1::Reached { .. }
            ),
            "the first index must reach fresh with graph serving: {reached:?}"
        );
        let text = registry
            .retained_text_owner_for_root(fixture.path())
            .await
            .expect("text owner");
        let snapshot = text.metadata().snapshot();
        let scope = ResolvedScope::new(
            project_id,
            snapshot.repository.clone(),
            snapshot.worktree.clone().expect("worktree identity"),
            snapshot.reference.clone(),
        )
        .expect("resolved scope");
        Self {
            registry,
            graph_runtime,
            scope,
            generation_id: text.metadata().manifest().generation_id.clone(),
            _database_scope: database_scope,
            _profile: profile,
        }
    }

    async fn shutdown(self) {
        self.registry.shutdown().await;
        self.graph_runtime
            .shutdown_memory_graph_reconciliation_tasks()
            .await
            .expect("join graph reconciliation tasks");
    }
}

/// The serving-graph owners the inventory reports for one generation.
fn serving_graph_owners(
    report: &ResidentOwnersReportV1,
    generation: &str,
) -> Vec<ResidentOwnerKindV1> {
    report
        .owners
        .iter()
        .filter(|row| {
            matches!(
                row.kind,
                ResidentOwnerKindV1::GraphCatalog | ResidentOwnerKindV1::GraphEngine
            ) && row
                .holders
                .iter()
                .any(|holder| holder.holding.as_str() == generation)
        })
        .map(|row| row.kind)
        .collect()
}

/// Status reads the generation census and freshness on every poll. Neither
/// needs the graph resident, so neither renews the residency lease that keeps
/// it resident; a graph read does.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn status_polls_let_the_graph_lease_lapse_and_graph_reads_renew_it() {
    const POLL: Duration = Duration::from_millis(250);
    let fixture = GitFixture::new(ALPHA_LIB_V1);
    let store = TempDir::new().expect("store root");
    let owners = Arc::new(ResidentOwnersV1::new(RESIDENT_OWNER_IDLE_WINDOW_V1));
    let mount = PersistentGraphMountV1::mount(
        CodeIndexSchedulerRegistryV1::with_background_reconcile_permits(1, 1)
            .with_resident_owners(Arc::clone(&owners)),
        &fixture,
        &store,
        "status polls leave the graph lease",
    )
    .await;
    let graph_idle_for = || {
        let report = owners.report(std::time::Instant::now());
        assert!(
            serving_graph_owners(&report, mount.generation_id.as_str())
                .contains(&ResidentOwnerKindV1::GraphEngine),
            "the serving graph engine is resident: {report:?}"
        );
        report
            .owners
            .iter()
            .map(|row| row.idle_for)
            .min()
            .expect("resident owners")
    };
    let port = project_code_graph_projection_read_port(
        mount.registry.clone(),
        fixture.path().to_path_buf(),
        mount.scope.clone(),
    );
    let context = graph_request_context(mount.scope.clone(), "status-lease");
    port.open(CodeGraphReadRequest::from_context(&context, now_micros()))
        .await
        .expect("graph read");
    let census = project_code_index_generation_census_reader(
        mount.registry.clone(),
        fixture.path().into(),
        mount.scope.clone(),
    );

    let polled_from = std::time::Instant::now();
    for _ in 0..6 {
        tokio::time::sleep(POLL).await;
        let GenerationCensusSnapshot::Observed {
            generation_id,
            freshness,
            statistics,
        } = census().await
        else {
            panic!("status reports the serving generation");
        };
        assert_eq!(generation_id, mount.generation_id.as_str());
        assert_eq!(freshness, GenerationCensusServingFreshness::Current);
        assert_eq!(statistics.symbol_count, 1, "`alpha` is the one symbol");
        let freshness = mount
            .registry
            .dashboard_freshness_read(fixture.path())
            .await
            .expect("freshness reads")
            .expect("mounted worktree");
        assert_eq!(
            freshness.staleness_state,
            Some(tracedecay_contracts::code_index_freshness::CodeIndexStalenessStateV1::Fresh)
        );
    }
    let polled_for = polled_from.elapsed();
    assert!(
        graph_idle_for() >= polled_for,
        "six status polls over {polled_for:?} renewed the graph lease"
    );

    let read = port
        .open(CodeGraphReadRequest::from_context(&context, now_micros()))
        .await
        .expect("graph read");
    assert_eq!(read.generation(), &mount.generation_id);
    assert!(graph_idle_for() < POLL, "a graph read renews the lease");

    mount.shutdown().await;
}

/// Issue #2619. The modelled process holds, beside the first generation's
/// serving graph, everything else up to one byte under the admission
/// watermark, so nothing the refresh after a commit charges fits until that
/// graph is given back. Status is polled every 250 ms throughout, as a client
/// waiting for the refresh does, and the refresh still reaches current with
/// its own graph serving.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_refresh_refused_beside_the_serving_graph_publishes_under_status_polling() {
    const GIB: u64 = 1024 * 1024 * 1024;
    let fixture = GitFixture::new(ALPHA_LIB_V1);
    let store = TempDir::new().expect("store root");
    let owners = Arc::new(ResidentOwnersV1::new(RESIDENT_OWNER_IDLE_WINDOW_V1));
    let resident = Arc::new(AtomicU64::new(0));
    let limit = NonZeroU64::new(16 * GIB).expect("limit");
    let sampled = Arc::clone(&resident);
    let pressure = Arc::new(ResidentMemoryPressureV1::with_sampler(
        limit,
        Arc::new(move || {
            let bytes = sampled.load(Ordering::Acquire);
            Some(ProcessResidentSampleV1 {
                resident_bytes: bytes,
                unreclaimable_bytes: bytes,
                swapped_bytes: 0,
                cgroup_committed_bytes: None,
            })
        }),
    ));
    let beside_graph = pressure.high_watermark_bytes() - 1;
    let mount = PersistentGraphMountV1::mount(
        CodeIndexSchedulerRegistryV1::with_resident_memory(
            1,
            Arc::new(ProcessResidentMemoryV1::with_pressure(limit, pressure)),
        )
        .with_resident_owners(Arc::clone(&owners)),
        &fixture,
        &store,
        "refresh beside the serving graph",
    )
    .await;
    let first = mount.generation_id.as_str().to_owned();
    let before = serving_graph_owners(&owners.report(std::time::Instant::now()), &first);
    assert!(
        before.contains(&ResidentOwnerKindV1::GraphEngine),
        "the first generation's graph engine is resident: {before:?}"
    );

    let model = tokio::spawn({
        let owners = Arc::clone(&owners);
        let resident = Arc::clone(&resident);
        let first = first.clone();
        async move {
            loop {
                let graph_held =
                    !serving_graph_owners(&owners.report(std::time::Instant::now()), &first)
                        .is_empty();
                resident.store(if graph_held { beside_graph } else { 0 }, Ordering::Release);
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        }
    });
    let census = project_code_index_generation_census_reader(
        mount.registry.clone(),
        fixture.path().into(),
        mount.scope.clone(),
    );
    let poller = tokio::spawn({
        let registry = mount.registry.clone();
        let root = fixture.path().to_path_buf();
        let census = Arc::clone(&census);
        async move {
            loop {
                census().await;
                registry.dashboard_freshness_read(&root).await.ok();
                tokio::time::sleep(Duration::from_millis(250)).await;
            }
        }
    });

    fixture.edit(
        "src/lib.rs",
        "pub fn alpha() -> u32 { 1 }\npub fn beta() -> u32 { 2 }\n",
    );
    git(fixture.path(), &["commit", "-qam", "refresh"]);
    assert!(matches!(
        mount
            .registry
            .notify_path(fixture.path(), fixture.path().join("src/lib.rs"))
            .await,
        CodeIndexDemandAdmissionV1::Queued
    ));

    let deadline = std::time::Instant::now() + Duration::from_mins(2);
    let (second, statistics) = loop {
        let freshness = mount
            .registry
            .dashboard_freshness(fixture.path())
            .await
            .expect("mounted worktree");
        if let GenerationCensusSnapshot::Observed {
            generation_id,
            freshness: GenerationCensusServingFreshness::Current,
            statistics,
        } = census().await
            && generation_id != first
            && freshness.staleness_state
                == Some(
                    tracedecay_contracts::code_index_freshness::CodeIndexStalenessStateV1::Fresh,
                )
            && freshness.code_graph_serving
                == Some(
                    tracedecay_contracts::code_index_freshness::CodeGraphServingReadinessV1::Ready,
                )
        {
            break (generation_id, statistics);
        }
        assert!(
            std::time::Instant::now() <= deadline,
            "the refresh never reached current with its graph serving: {freshness:?}"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    };
    poller.abort();
    model.abort();

    assert_eq!(statistics.symbol_count, 2, "`alpha` and `beta`");
    let report = owners.report(std::time::Instant::now());
    assert_eq!(serving_graph_owners(&report, &first), []);
    assert!(
        serving_graph_owners(&report, &second).contains(&ResidentOwnerKindV1::GraphEngine),
        "the refresh's own graph engine serves: {report:?}"
    );

    mount.shutdown().await;
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
