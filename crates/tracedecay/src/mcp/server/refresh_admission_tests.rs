use super::lifecycle::admit_refresh;
use super::writer_test_support::init_indexed_repo;
use super::{
    CodeIndexDemandAdmissionV1, CodeIndexDemandUnavailableV1, CodeIndexDemandV1,
    CodeIndexFreshnessProbeSink, CodeIndexReconcileSink, McpServer, McpServerConstructionContext,
};
use crate::test_support::git::git;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Duration;
use tracedecay_code_index_runtime::code_index_scheduler::{
    CodeIndexActivationV1, CodeIndexAutomaticAdmissionV1, CodeIndexSchedulerRegistryV1,
};
use tracedecay_domain::ProjectId;
use tracedecay_runtime_core::cancellation::CancellationToken;

#[tokio::test]
async fn refresh_preserves_activation_policy_root_and_retirement_verdicts() {
    let (graph, dir, _authority) = init_indexed_repo().await;
    let registered = Arc::new(AtomicBool::new(true));
    let activation = CodeIndexActivationV1::new_with_admission(
        dir.path(),
        Arc::clone(&registered),
        CancellationToken::new(),
        CodeIndexAutomaticAdmissionV1::LinkedWorktreeDisabled,
        Arc::new(|| Box::pin(async { panic!("denied route must not mount") })),
        Arc::new(|_| Box::pin(async { panic!("denied route must not receive demand") })),
    );
    let reconcile: CodeIndexReconcileSink = {
        let activation = activation.clone();
        Arc::new(move |root, demand| {
            let activation = activation.clone();
            Box::pin(async move { activation.admit(&root, demand).await })
        })
    };
    assert_eq!(
        admit_refresh(&graph, |root| Some(reconcile(
            root,
            CodeIndexDemandV1::Reconcile
        )))
        .await
        .unwrap(),
        CodeIndexDemandAdmissionV1::RefusedByPolicy
    );

    let foreign = tempfile::TempDir::new().unwrap();
    assert_eq!(
        admit_refresh(&graph, |_| Some(reconcile(
            foreign.path().to_path_buf(),
            CodeIndexDemandV1::Reconcile,
        )))
        .await
        .unwrap(),
        CodeIndexDemandAdmissionV1::Unavailable(CodeIndexDemandUnavailableV1::ForeignRoot)
    );
    registered.store(false, Ordering::Release);
    assert_eq!(
        admit_refresh(&graph, |root| Some(reconcile(
            root,
            CodeIndexDemandV1::Reconcile
        )))
        .await
        .unwrap(),
        CodeIndexDemandAdmissionV1::Unavailable(CodeIndexDemandUnavailableV1::RouteRetired)
    );
}

#[tokio::test]
async fn refresh_rejects_a_stale_branch_before_admission() {
    let (graph, dir, _authority) = init_indexed_repo().await;
    git(dir.path(), &["switch", "-c", "other"]);
    let error = admit_refresh(&graph, |_| panic!("stale graph must not reach admission"))
        .await
        .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("retained refresh graph is stale")
    );
}

#[tokio::test]
async fn refresh_without_a_scheduler_keeps_the_typed_unavailable_reason() {
    let (graph, _dir, _authority) = init_indexed_repo().await;
    let error = admit_refresh(&graph, |_| None).await.unwrap_err();
    assert_eq!(
        error.project_route_context().map(|(reason, _, _)| reason),
        Some(super::CODE_INDEX_SCHEDULER_UNAVAILABLE)
    );
}

/// Two MCP servers share the real activation and mounted scheduler. Admission
/// settles while its worker is held; only releasing that worker publishes a
/// generation. Neither server owns a second writer or publishes freshness.
#[tokio::test]
async fn startup_refresh_coalesces_at_the_scheduler_and_waits_only_for_admission() {
    let (first_graph, dir, authority) = init_indexed_repo().await;
    let second_graph = authority.reopen_project_graph(dir.path()).await;
    let registry = CodeIndexSchedulerRegistryV1::with_background_reconcile_permits(1, 1);
    let held = registry
        .background_reconcile_admission()
        .acquire_owned()
        .await
        .unwrap();
    let store = tempfile::TempDir::new().unwrap();
    let mounts = Arc::new(AtomicUsize::new(0));
    let mounted = Arc::new(tokio::sync::Notify::new());
    let activation = CodeIndexActivationV1::new(
        dir.path(),
        Arc::new(AtomicBool::new(true)),
        CancellationToken::new(),
        {
            let registry = registry.clone();
            let root = dir.path().to_path_buf();
            let store = store.path().to_path_buf();
            let mounts = Arc::clone(&mounts);
            let mounted = Arc::clone(&mounted);
            Arc::new(move || {
                let registry = registry.clone();
                let root = root.clone();
                let store = store.clone();
                let mounts = Arc::clone(&mounts);
                let mounted = Arc::clone(&mounted);
                Box::pin(async move {
                    mounts.fetch_add(1, Ordering::AcqRel);
                    registry
                        .mount_worktree(ProjectId::new("project.mcp-writer").unwrap(), &root, store)
                        .await
                        .map_err(|error| error.to_string())?;
                    mounted.notify_one();
                    Ok(())
                })
            })
        },
        {
            let registry = registry.clone();
            let root = dir.path().to_path_buf();
            Arc::new(move |batch| {
                let registry = registry.clone();
                let root = root.clone();
                Box::pin(async move {
                    assert!(batch.overflow, "startup submits reconciliation demand");
                    registry.notify_hook_overflow(&root).await
                })
            })
        },
    );
    let reconcile: CodeIndexReconcileSink = Arc::new(move |root, demand| {
        let activation = activation.clone();
        Box::pin(async move { activation.admit(&root, demand).await })
    });
    let freshness: tracedecay_contracts::code_index_freshness::CodeIndexFreshnessReader = {
        let registry = registry.clone();
        Arc::new(move |root| {
            let registry = registry.clone();
            Box::pin(async move { registry.dashboard_freshness_read(&root).await })
        })
    };
    let mut publications = registry.subscribe_generation_publications();
    let (first, second) = tokio::join!(
        McpServer::new_with_context(
            McpServerConstructionContext::direct(first_graph, None)
                .with_code_index_reconcile_sink(Arc::clone(&reconcile))
                .with_dashboard_code_index_freshness_reader(Arc::clone(&freshness)),
        ),
        McpServer::new_with_context(
            McpServerConstructionContext::direct(second_graph, None)
                .with_code_index_reconcile_sink(reconcile)
                .with_dashboard_code_index_freshness_reader(freshness),
        ),
    );
    let (first_done, second_done) = tokio::join!(
        first.wait_for_startup_catch_up(Duration::from_secs(5)),
        second.wait_for_startup_catch_up(Duration::from_secs(5)),
    );
    assert!(
        first_done && second_done,
        "admission never waits for indexing"
    );
    tokio::time::timeout(Duration::from_secs(5), mounted.notified())
        .await
        .unwrap();
    assert_eq!(mounts.load(Ordering::Acquire), 1);
    assert!(registry.latest_generation_id(dir.path()).await.is_none());
    let reader = first
        .tool_call_registry_options(None)
        .code_index_freshness_reader
        .expect("MCP receives the canonical freshness port");
    let before = reader(dir.path().to_path_buf()).await.unwrap().unwrap();
    assert!(
        before.latest_generation_id.is_none(),
        "admission cannot manufacture a generation"
    );
    let mut serving = registry
        .subscribe_serving_generation_changes(dir.path())
        .await
        .unwrap();
    assert!(registry.request_complete_generation(dir.path()).await);
    drop(held);
    let publication = tokio::time::timeout(Duration::from_secs(30), publications.recv())
        .await
        .expect("real scheduler publication")
        .unwrap();
    assert_eq!(publication.project_root, dir.path().canonicalize().unwrap());
    let generation = registry.latest_generation_id(dir.path()).await.unwrap();
    tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            let current = reader(dir.path().to_path_buf()).await.unwrap().unwrap();
            if current.latest_generation_id.as_deref() == Some(generation.as_str()) {
                break;
            }
            serving
                .changed()
                .await
                .expect("serving owner remains mounted");
        }
    })
    .await
    .expect("MCP freshness reads the scheduler's published generation");
    first.shutdown().await;
    second.shutdown().await;
    registry.shutdown().await;
}

async fn wait_for_refresh_admission(server: &McpServer) {
    tokio::time::timeout(Duration::from_secs(5), async {
        while server.refresh_admission_running.load(Ordering::Acquire) {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("refresh admission settles");
}

#[tokio::test]
async fn lazy_refresh_is_detached_single_flighted_and_uses_only_the_probe_port() {
    let (graph, _dir, _authority) = init_indexed_repo().await;
    let entered = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    let calls = Arc::new(AtomicUsize::new(0));
    let probe: CodeIndexFreshnessProbeSink = {
        let entered = Arc::clone(&entered);
        let release = Arc::clone(&release);
        let calls = Arc::clone(&calls);
        Arc::new(move |_root| {
            let entered = Arc::clone(&entered);
            let release = Arc::clone(&release);
            let calls = Arc::clone(&calls);
            Box::pin(async move {
                calls.fetch_add(1, Ordering::AcqRel);
                entered.notify_one();
                release.notified().await;
                CodeIndexDemandAdmissionV1::Unavailable(
                    CodeIndexDemandUnavailableV1::NoProvenChange,
                )
            })
        })
    };
    let mut context = McpServerConstructionContext::direct(graph, None)
        .with_code_index_freshness_probe_sink(probe)
        .with_code_index_reconcile_sink(Arc::new(|_, _| {
            panic!("lazy refresh must not force reconciliation")
        }));
    context.startup_catch_up_enabled = false;
    let server = McpServer::new_with_context(context).await;
    tokio::time::timeout(Duration::from_secs(5), server.maybe_sync_if_stale())
        .await
        .expect("tool returns before its probe settles");
    tokio::time::timeout(Duration::from_secs(5), entered.notified())
        .await
        .unwrap();
    assert!(server.refresh_admission_running.load(Ordering::Acquire));
    server
        .last_edit_refresh_admission_at
        .store(0, Ordering::Release);
    server.maybe_sync_if_stale().await;
    let snapshot = server.cg_snapshot().await;
    server.maybe_spawn_read_refresh(&snapshot, &snapshot.branch_memo());
    assert_eq!(
        calls.load(Ordering::Acquire),
        1,
        "read and edit admissions share one slot"
    );
    release.notify_one();
    wait_for_refresh_admission(&server).await;
    server.refuse_background_work();
    server
        .last_edit_refresh_admission_at
        .store(0, Ordering::Release);
    server
        .last_read_refresh_admission_at
        .store(0, Ordering::Release);
    server.maybe_sync_if_stale().await;
    server.maybe_spawn_read_refresh(&snapshot, &snapshot.branch_memo());
    server.run_startup_catch_up_sync().await;
    assert_eq!(
        calls.load(Ordering::Acquire),
        1,
        "shutdown admits no new refresh"
    );
    server.shutdown().await;
}

#[tokio::test]
async fn parked_freshness_probe_logs_cause_and_remedy_as_fields() {
    let (graph, _dir, _authority) = init_indexed_repo().await;
    let capture = tracedecay_runtime_core::logging::FormattedTracingCapture::start();
    let cause = format!("publication manifest is corrupt: {}", "x".repeat(600));
    let probe: CodeIndexFreshnessProbeSink = {
        let cause = cause.clone();
        Arc::new(move |_root| {
            let cause = cause.clone();
            Box::pin(async move {
                CodeIndexDemandAdmissionV1::Terminal(
                    tracedecay_contracts::code_index_freshness::CodeIndexConvergenceParkedV1 {
                        reason: cause,
                        blocked_reason: None,
                        remediation: "run `tracedecay daemon restart`".to_owned(),
                        parked_at_micros: 1,
                        observed_passes: 1,
                        retries_on_wake: false,
                    },
                )
            })
        })
    };
    let mut context = McpServerConstructionContext::direct(graph, None)
        .with_code_index_freshness_probe_sink(probe);
    context.startup_catch_up_enabled = false;
    let server = McpServer::new_with_context(context).await;
    let snapshot = server.cg_snapshot().await;
    server
        .refresh_admission_running
        .store(true, Ordering::Release);
    server.spawn_read_refresh_task(&snapshot);
    wait_for_refresh_admission(&server).await;
    server.shutdown().await;
    let log = capture.text();
    let line = log
        .lines()
        .find(|line| line.contains("background read reconciliation was not admitted"))
        .unwrap_or_else(|| panic!("probe refusal must be logged: {log}"));
    let (_, fields) = line
        .split_once("was not admitted ")
        .unwrap_or_else(|| panic!("structured fields follow the message: {line}"));
    assert_eq!(
        fields,
        format!(
            "reason_code=\"code_index_publication_authority_corrupt\" cause=\"{cause}\" \
         remedy=\"run `tracedecay daemon restart`\" retries_on_wake=false"
        )
    );
}

/// Missing admission settles startup on the native runtime, with no fabricated
/// scheduler success and no Tokio runtime requirement.
#[test]
fn startup_catch_up_settles_without_a_tokio_runtime() {
    native_server_catch_up(None, async |server| {
        assert!(
            server
                .wait_for_startup_catch_up(Duration::from_secs(30))
                .await
        );
    });
}

fn native_server_catch_up(
    reconcile: Option<CodeIndexReconcileSink>,
    observe: impl AsyncFnOnce(&McpServer),
) {
    let hazards = crate::test_support::native_runtime_hazards();
    tracedecay_runtime_core::native_runtime::block_on(async {
        assert!(tokio::runtime::Handle::try_current().is_err());
        let (graph, _dir, _authority) = init_indexed_repo().await;
        let mut context = McpServerConstructionContext::direct(graph, None);
        context.code_index_reconcile_sink = reconcile;
        let server = McpServer::new_with_context(context).await;
        observe(&server).await;
        server.shutdown_background_tasks().await;
    })
    .expect("native runtime");
    assert_eq!(crate::test_support::native_runtime_hazards(), hazards);
}

#[test]
fn shutdown_aborts_a_parked_startup_admission_without_a_tokio_runtime() {
    struct Dropped(Arc<AtomicBool>);
    impl Drop for Dropped {
        fn drop(&mut self) {
            self.0.store(true, Ordering::Release);
        }
    }
    let entered = Arc::new(tokio::sync::Notify::new());
    let dropped = Arc::new(AtomicBool::new(false));
    let reconcile: CodeIndexReconcileSink = {
        let entered = Arc::clone(&entered);
        let dropped = Arc::clone(&dropped);
        Arc::new(move |_, _| {
            let entered = Arc::clone(&entered);
            let guard = Dropped(Arc::clone(&dropped));
            Box::pin(async move {
                let _guard = guard;
                entered.notify_one();
                std::future::pending().await
            })
        })
    };
    native_server_catch_up(Some(reconcile), async |_server| entered.notified().await);
    assert!(
        dropped.load(Ordering::Acquire),
        "shutdown aborts and joins pending admission"
    );
}
