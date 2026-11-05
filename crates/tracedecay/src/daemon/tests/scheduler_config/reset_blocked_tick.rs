use std::sync::atomic::Ordering;
use std::time::Duration;

use tempfile::TempDir;
use tracedecay_automation_runtime::automation::config::{
    AutomationBackendV1, AutomationConfigPatch, AutomationTaskPatch,
};
use tracedecay_automation_runtime::automation::run_ledger::{load_run_records, run_ledger_path};
use tracedecay_contracts::retrieval::AutomationSchedulerReconcileOutcome;
use tracedecay_daemon_protocol::DaemonHandshake;

use super::super::super::engine::DaemonEngine;
use super::super::super::project_open_admission::ProjectServerKey;
use super::super::{
    apply_project_automation_patch_via_surface, enter_test_daemon_database_scope,
    initialize_test_project, test_client_identity_for, test_daemon_engine_for_profile,
    test_handshake_defaults,
};

/// The persisted-shape refusal the real run ledger raises for a retired row.
const RETIRED_LEDGER_ROW: &str = concat!(
    "{\"schema_version\":1,\"run_id\":\"run-retired\",\"trigger\":\"scheduler\",",
    "\"task\":\"memory_curator\",\"task_key\":\"memory_curator\",",
    "\"backend\":\"codex_app_server\",\"status\":\"succeeded\",",
    "\"accepted_count\":0,\"rejected_count\":0,",
    "\"started_at\":\"1970-01-01T00:00:01Z\",\"completed_at\":\"1\",",
    "\"completed_at_micros\":1000000}\n",
);
const TICK_SECS: u64 = 1;
/// Several cadences: a loop that kept ticking would pass the observed count.
const PARK_OBSERVATION: Duration = Duration::from_millis(3_500);
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

struct BlockedProject {
    _dir: TempDir,
    project: std::path::PathBuf,
    dashboard_root: std::path::PathBuf,
    handshake: DaemonHandshake,
    engine: DaemonEngine,
    key: ProjectServerKey,
    _server: std::sync::Arc<crate::mcp::McpServer>,
}

/// A real daemon-owned scheduler, with no codex executable bound, whose ledger
/// refuses every tick until its reset.
async fn project_with_refusing_ledger() -> (BlockedProject, impl Sized) {
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
    let ledger = run_ledger_path(&dashboard_root);
    std::fs::create_dir_all(ledger.parent().expect("ledger parent")).expect("ledger dir");
    std::fs::write(&ledger, RETIRED_LEDGER_ROW).expect("retired ledger row");
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
        BlockedProject {
            _dir: dir,
            project,
            dashboard_root,
            handshake,
            engine,
            key,
            _server: server,
        },
        scope,
    )
}

#[tokio::test]
async fn reset_refusal_parks_the_loop_until_a_reconcile_finds_the_store_healed() {
    let (fixture, _scope) = project_with_refusing_ledger().await;
    let engine = &fixture.engine;
    wait_for_ticks(engine, 1, "first refused tick").await;

    // Parked, not retrying: several cadences pass without another tick.
    let parked_at = tick_attempts(engine);
    tokio::time::sleep(PARK_OBSERVATION).await;
    assert_eq!(tick_attempts(engine), parked_at, "a refused loop must hold");

    // A reconcile while the store still refuses looks once and holds again.
    assert_eq!(
        engine
            .ensure_automation_scheduler(
                fixture.key.clone(),
                fixture.project.clone(),
                fixture.handshake.clone(),
            )
            .await,
        AutomationSchedulerReconcileOutcome::RunningNotified
    );
    wait_for_ticks(engine, parked_at + 1, "reconcile wakes a parked loop").await;
    let still_parked_at = tick_attempts(engine);
    tokio::time::sleep(PARK_OBSERVATION).await;
    assert_eq!(tick_attempts(engine), still_parked_at);

    // Healed store: the next reconcile restores cadence with no further wake.
    std::fs::remove_file(run_ledger_path(&fixture.dashboard_root)).expect("reset ledger");
    engine
        .ensure_automation_scheduler(
            fixture.key.clone(),
            fixture.project.clone(),
            fixture.handshake.clone(),
        )
        .await;
    wait_for_ticks(
        engine,
        still_parked_at + 3,
        "cadence resumes once the store is healed",
    )
    .await;

    // The memory curator is due on all of those ticks. With no executable
    // bound it reads `backend_unconfigured`: no run, no failure history.
    assert_eq!(
        load_run_records(&fixture.dashboard_root, 10)
            .await
            .expect("load run ledger")
            .len(),
        0,
        "an unconfigured backend must not record a run per tick"
    );
    engine.shutdown_all().await;
}

#[tokio::test]
async fn retirement_reaches_a_loop_parked_on_a_reset_refusal() {
    let (fixture, _scope) = project_with_refusing_ledger().await;
    let engine = &fixture.engine;
    wait_for_ticks(engine, 1, "first refused tick").await;

    let started = std::time::Instant::now();
    let retirement = engine
        .retire_automation_scheduler_locked(&fixture.key)
        .await
        .expect("a live scheduler retires");
    retirement.wait().await;
    // Well inside the reaper's forced-abort deadline: the loop left on its own.
    assert!(
        started.elapsed() < tracedecay_runtime_core::DAEMON_TASK_ABORT_DEADLINE / 2,
        "retiring a parked loop took {:?}",
        started.elapsed()
    );
    engine.shutdown_all().await;
}

#[tokio::test]
async fn scheduler_admission_waits_for_mandatory_runtime_publication() {
    let (fixture, _scope) = project_with_refusing_ledger().await;
    let engine = &fixture.engine;
    let retirement = engine
        .retire_automation_scheduler_locked(&fixture.key)
        .await
        .unwrap();
    retirement.wait().await;
    let registry = &engine.invocation.service.project_runtimes;
    let publication = registry.begin_publication(&fixture.project).unwrap();
    let cg = fixture._server.cg_snapshot().await;
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
async fn retirement_interrupts_a_reset_reconcile_quiet_period() {
    let (fixture, _scope) = project_with_refusing_ledger().await;
    let engine = &fixture.engine;
    wait_for_ticks(engine, 1, "first refused tick").await;
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
    assert_eq!(tick_attempts(engine), 1);
    engine.shutdown_all().await;
}
