use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use tempfile::TempDir;
use tracedecay_automation_runtime::automation::AutomationRunControl;
use tracedecay_automation_runtime::automation::config::{
    AutomationBackendV1, AutomationConfigPatch, AutomationTaskPatch,
};
use tracedecay_contracts::retrieval::AutomationSchedulerReconcileOutcome;
use tracedecay_daemon_protocol::DaemonHandshake;

use super::super::super::engine::DaemonEngine;
use super::super::super::project_open_admission::ProjectServerKey;
use super::super::{
    apply_project_automation_patch_via_surface, enter_test_daemon_database_scope,
    initialize_test_project, test_client_identity_for, test_daemon_engine_for_profile,
    test_handshake_defaults,
};

const LEGACY_HISTORY: &[u8] = b"retired automation run history\n";
const TICK_SECS: u64 = 1;
const WAIT_LIMIT: Duration = Duration::from_secs(30);

fn tick_attempts(engine: &DaemonEngine) -> usize {
    engine
        .automation_scheduler_tick_attempts
        .load(Ordering::Relaxed)
}

async fn wait_for_ticks(engine: &DaemonEngine, at_least: usize, context: &str) {
    tokio::time::timeout(WAIT_LIMIT, async {
        while tick_attempts(engine) < at_least {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("{context}: {} ticks", tick_attempts(engine)));
}

struct SchedulerProject {
    _dir: TempDir,
    project: std::path::PathBuf,
    dashboard_root: std::path::PathBuf,
    handshake: DaemonHandshake,
    engine: DaemonEngine,
    key: ProjectServerKey,
    server: std::sync::Arc<crate::mcp::McpServer>,
}

/// A real daemon-owned scheduler, with no codex executable bound.
async fn scheduler_project() -> (SchedulerProject, impl Sized) {
    let dir = TempDir::new().expect("temp dir");
    let project = dir.path().canonicalize().expect("canonical temp dir");
    let client_identity = test_client_identity_for(project.join("profile"));
    std::fs::create_dir_all(project.join("src")).expect("src dir");
    std::fs::write(project.join("src/main.rs"), "fn main() {}\n").expect("source file");
    let layout = initialize_test_project(&project, &client_identity).await;
    let dashboard_root = layout.dashboard_root;
    let handshake = DaemonHandshake {
        project_path: Some(project.clone()),
        client_identity,
        ..test_handshake_defaults()
    };
    let engine = test_daemon_engine_for_profile(&handshake.client_identity.profile_root);
    let scope = enter_test_daemon_database_scope(
        &handshake.client_identity.profile_root,
        "reset-blocked-scheduler-test",
    );
    let server = apply_project_automation_patch_via_surface(
        &engine,
        &handshake,
        AutomationConfigPatch {
            enabled: Some(true),
            backend: Some(AutomationBackendV1::CodexAppServer),
            scheduler_tick_secs: Some(TICK_SECS),
            memory_curator: AutomationTaskPatch {
                enabled: Some(true),
                schedule: Some(Some("every:1m".to_string())),
                ..AutomationTaskPatch::default()
            },
            ..AutomationConfigPatch::default()
        },
    )
    .await;
    let cg = server.cg_snapshot().await;
    let key = ProjectServerKey::from_open_project(&cg, &handshake).expect("owner key");
    let reconciled = engine
        .ensure_automation_scheduler(key.clone(), project.clone(), handshake.clone())
        .await;
    assert!(
        matches!(
            reconciled,
            AutomationSchedulerReconcileOutcome::Started
                | AutomationSchedulerReconcileOutcome::RunningNotified
        ),
        "{reconciled:?}"
    );
    (
        SchedulerProject {
            _dir: dir,
            project,
            dashboard_root,
            handshake,
            engine,
            key,
            server,
        },
        scope,
    )
}

#[tokio::test]
async fn scheduler_refuses_legacy_history_and_resumes_after_removal() {
    let (fixture, _scope) = scheduler_project().await;
    let engine = &fixture.engine;
    engine
        .retire_automation_scheduler_locked(&fixture.key)
        .await
        .unwrap()
        .wait()
        .await;
    let cg = fixture.server.cg_snapshot().await;
    let runs = cg.automation_run_store().expect("fresh registered history");
    let retired = fixture.dashboard_root.join("automation_runs.jsonl");
    std::fs::create_dir_all(&fixture.dashboard_root).unwrap();
    std::fs::write(&retired, LEGACY_HISTORY).unwrap();

    let control = AutomationRunControl::from_interrupted(Arc::new(|| false));
    let refused = Box::pin(crate::daemon::scheduler::run_automation_scheduler_tick(
        &fixture.project,
        &cg,
        &fixture.handshake,
        engine,
        &control,
    ))
    .await
    .expect_err("retired history must refuse scheduled work");
    assert_eq!(
        refused
            .project_route_context()
            .map(|(code, retryable, _)| (code, retryable)),
        Some(("automation_run_legacy_jsonl_refused", false))
    );
    assert_eq!(std::fs::read(&retired).unwrap(), LEGACY_HISTORY);
    assert!(runs.load_run_records(10).await.unwrap().is_empty());

    std::fs::remove_file(retired).unwrap();
    Box::pin(crate::daemon::scheduler::run_automation_scheduler_tick(
        &fixture.project,
        &cg,
        &fixture.handshake,
        engine,
        &control,
    ))
    .await
    .expect("ticks may resume once retired history is removed");
    assert!(runs.load_run_records(10).await.unwrap().is_empty());
    drop(runs);
    engine.shutdown_all().await;
}

#[tokio::test]
async fn scheduler_admission_waits_for_mandatory_runtime_publication() {
    let (fixture, _scope) = scheduler_project().await;
    let engine = &fixture.engine;
    let retirement = engine
        .retire_automation_scheduler_locked(&fixture.key)
        .await
        .unwrap();
    retirement.wait().await;
    let registry = &engine.invocation.service.project_runtimes;
    let publication = registry.begin_publication(&fixture.project).unwrap();
    let cg = fixture.server.cg_snapshot().await;
    assert_eq!(
        engine
            .start_automation_scheduler(
                fixture.key.clone(),
                fixture.project.clone(),
                fixture.handshake.clone(),
                cg.clone()
            )
            .await,
        AutomationSchedulerReconcileOutcome::OwnerUnavailable,
    );
    assert!(
        engine
            .store_administration
            .automation_schedulers()
            .lock()
            .await
            .is_empty()
    );
    let before = tick_attempts(engine);
    engine.spawn_project_maintenance_activation(
        fixture.key.clone(),
        fixture.project.clone(),
        fixture.handshake.clone(),
    );
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(
        tick_attempts(engine),
        before,
        "core publication must not tick"
    );
    assert!(registry.mark_publication_ready(&publication));
    wait_for_ticks(engine, before + 1, "full publication wakes activation").await;
    engine.shutdown_all().await;
}

#[tokio::test]
async fn retirement_interrupts_a_reconcile_quiet_period() {
    let (fixture, _scope) = scheduler_project().await;
    let engine = &fixture.engine;
    wait_for_ticks(engine, 1, "first scheduler tick").await;
    let before = tick_attempts(engine);
    engine
        .ensure_automation_scheduler(
            fixture.key.clone(),
            fixture.project.clone(),
            fixture.handshake.clone(),
        )
        .await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    let started = std::time::Instant::now();
    let retirement = engine
        .retire_automation_scheduler_locked(&fixture.key)
        .await
        .unwrap();
    retirement.wait().await;
    assert!(
        started.elapsed() < Duration::from_secs(1),
        "quiet debounce delayed retirement: {:?}",
        started.elapsed()
    );
    assert_eq!(tick_attempts(engine), before);
    engine.shutdown_all().await;
}
