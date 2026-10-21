use tracedecay_contracts::code_index_freshness::{
    CodeIndexReadinessTargetV1, CodeIndexReadinessV1, CodeIndexReadinessWaitReadV1,
};
use tracedecay_runtime_core::native_runtime::{NativeTask, NativeTaskOutcome};
use tracedecay_session_memory::event_lane::{self, ActivityFamilyV1};

use super::*;

async fn ready_reading(
    fixture: &RmcpRouteFixture,
    project: &std::path::Path,
    phase: &str,
) -> tracedecay_contracts::code_index_freshness::CodeIndexWorktreeFreshnessV1 {
    let reading = fixture
        .engine
        .invocation
        .code_index_schedulers
        .wait_for_readiness(project, CodeIndexReadinessTargetV1::Ready, PHASE_TIMEOUT)
        .await
        .unwrap_or_else(|error| panic!("{phase} readiness read failed: {error:?}"));
    let CodeIndexReadinessWaitReadV1::Reached { reading } = reading else {
        panic!("exact worktree did not become ready: {reading:?}");
    };
    *reading
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn committed_save_remains_noncurrent_until_its_broker_handoff() {
    let fixture = rmcp_route_fixture_with_projects("held-hook-index-handoff", &[], true).await;
    let project =
        canonical_existing_identity(fixture.handshake.project_path.as_deref().unwrap()).unwrap();
    wait_for_host_ingest_publication(&fixture).await;
    let initial_ready = ready_reading(&fixture, &project, "initial").await;
    let graph = fixture.server.cg_snapshot().await;
    let project_id = graph.store_layout().identity.project_id.as_deref().unwrap();
    let scope = tracedecay_code_index_runtime::resolved_scope_for_project(
        &project,
        &project_id.to_owned().try_into().unwrap(),
    )
    .unwrap();
    let database = fixture
        .store_administration
        .registered_project_session_database(graph.project_root(), graph.store_layout())
        .await
        .unwrap();
    let broker = fixture
        .store_administration
        .host_admission_broker(&database)
        .await
        .unwrap();
    let registry = &fixture.engine.invocation.code_index_schedulers;
    assert!(
        registry
            .latest_text_serving_freshness_for_scope(&scope)
            .await
            .unwrap()
            .1
    );
    let _initial_graph_owner = if graph.get_config().native_graph_activation {
        // A ready text owner can outlive its decoded generation. Admit the
        // same exact-scope graph demand a real reader uses, under this phase's
        // existing deadline, before proving the positive decoded census.
        let owner = registry
            .latest_complete_fresh_for_scope_awaiting_seat(
                &scope,
                std::time::Instant::now() + PHASE_TIMEOUT,
            )
            .await;
        assert!(owner.is_some(), "initial ready reading: {initial_ready:?}");
        owner
    } else {
        None
    };
    let replay = broker.begin_replay().await.unwrap();
    assert_eq!(broker.pending_replay_count().await.unwrap(), 0);
    // Every source-proof renewal takes the mounted scheduler mutex. Hold it
    // across the save and durable admission so an independent source observer
    // cannot publish the edit before the broker records its handoff debt.
    let scheduler = registry.scheduler_handle(&project).await.unwrap();
    let (release_scheduler, scheduler_released) = tokio::sync::oneshot::channel::<()>();
    let (scheduler_acquired, scheduler_held) = tokio::sync::oneshot::channel();
    let held_scheduler = tokio::task::spawn_blocking(move || {
        let _scheduler = scheduler
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        scheduler_acquired.send(()).unwrap();
        let _ = scheduler_released.blocking_recv();
    });
    scheduler_held.await.unwrap();
    // Graph demand may have advanced the initial owner. Read the transition's
    // identity only after that demand and the scheduler hold are established.
    let initial = registry
        .dashboard_freshness_read(&project)
        .await
        .unwrap()
        .unwrap();
    std::fs::write(
        project.join("src/main.rs"),
        "fn main() { let saved = 42; }\n",
    )
    .unwrap();
    let request = stateless_request(json!({
        "jsonrpc": "2.0", "id": 51,
        "method": tracedecay_hooks::core_events::HOOK_EVENT_METHOD,
        "params": DaemonHookEvent::post_tool_use_edit(
            HostIntegrationIdV1::Codex, vec!["src/main.rs".to_owned()], project.clone(),
        ),
    }));
    let responses = unix_one_request(&fixture, &request).await;
    assert_single_result(&responses, 51, "durable save while replay held");
    assert_eq!(broker.pending_replay_count().await.unwrap(), 1);
    let owed = registry
        .dashboard_freshness_read(&project)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(owed.latest_generation_id, initial.latest_generation_id);
    assert_eq!(
        owed.hook_hint_count,
        Some(0),
        "broker debt is not a scheduler hint"
    );
    drop(release_scheduler);
    held_scheduler.await.unwrap();
    // Pending broker debt fences currency, not immutable publication. Read
    // again after releasing and joining the holder so these assertions cannot
    // pass merely because a held scheduler makes verification unknown.
    let owed = registry
        .dashboard_freshness_read(&project)
        .await
        .unwrap()
        .unwrap();
    assert_ne!(
        owed.readiness(CodeIndexReadinessTargetV1::Ready),
        CodeIndexReadinessV1::Reached
    );
    assert_ne!(
        owed.readiness(CodeIndexReadinessTargetV1::Fresh),
        CodeIndexReadinessV1::Reached
    );
    assert!(!registry.has_current_ready_decoded_for_root_scope(&project, &scope));
    assert!(
        registry
            .latest_complete_ready_decoded_for_root_scope(&project, &scope)
            .await
            .is_none()
    );
    let (_, current) = registry
        .latest_text_serving_freshness_for_scope(&scope)
        .await
        .unwrap();
    assert!(
        !current,
        "the retained text owner must not claim source currency"
    );
    drop(replay);
    let saved = ready_reading(&fixture, &project, "after broker release").await;
    assert_ne!(saved.latest_generation_id, initial.latest_generation_id);
    assert_ne!(
        saved.snapshot_content_identity,
        initial.snapshot_content_identity
    );
    assert_eq!(saved.source_reference, initial.source_reference);
    assert!(initial.source_revision.is_some());
    assert_eq!(saved.source_revision, None);
    assert_eq!(broker.pending_replay_count().await.unwrap(), 0);
    let activity = event_lane::replay_after(&database, project_id, None)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        activity
            .records
            .iter()
            .filter(|record| record.pulse.family == ActivityFamilyV1::CodeIndex)
            .count(),
        1
    );
    fixture.server.shutdown().await;
    let shutdown = fixture.engine.shutdown_all().await;
    assert!(
        shutdown.project_servers.is_clean(),
        "handoff shutdown: {shutdown:?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn readiness_budget_bounds_canonical_broker_runtime_contention() {
    let fixture = rmcp_route_fixture_with_projects("contended-hook-source-read", &[], true).await;
    let project =
        canonical_existing_identity(fixture.handshake.project_path.as_deref().unwrap()).unwrap();
    wait_for_host_ingest_publication(&fixture).await;
    let initial = ready_reading(&fixture, &project, "before runtime contention").await;
    let graph = fixture.server.cg_snapshot().await;
    let database = fixture
        .store_administration
        .registered_project_session_database(graph.project_root(), graph.store_layout())
        .await
        .unwrap();
    let broker = fixture
        .store_administration
        .host_admission_broker(&database)
        .await
        .unwrap();
    let replay = broker.begin_replay().await.unwrap();
    let payload = tracedecay_mcp::hook_events::encode_durable_hook_event_plan(
        &tracedecay_mcp::hook_events::HookEventPlan::Noop,
    )
    .unwrap();
    broker
        .admit("owned-runtime-contention", &payload)
        .await
        .unwrap();
    let (entered, entered_rx) = tokio::sync::oneshot::channel();
    let (release, released) = std::sync::mpsc::channel();
    let held = std::thread::spawn({
        let broker = Arc::clone(&broker);
        move || {
            let mut entered = Some(entered);
            broker
                .pending_replay_matches(|_| {
                    entered.take().unwrap().send(()).unwrap();
                    let _ = released.recv();
                    Ok(false)
                })
                .unwrap()
        }
    });
    entered_rx.await.unwrap();
    assert_eq!(
        broker
            .pending_replay_matches(|_| Ok(false))
            .unwrap_err()
            .reason_code,
        Some("spool_runtime_unavailable")
    );
    let (waiting, waiting_rx) = tokio::sync::oneshot::channel();
    let registry = fixture.engine.invocation.code_index_schedulers.clone();
    let waiting_root = project.clone();
    let readiness = NativeTask::spawn(async move {
        waiting.send(()).unwrap();
        registry
            .wait_for_readiness(
                &waiting_root,
                CodeIndexReadinessTargetV1::Ready,
                PHASE_TIMEOUT,
            )
            .await
    })
    .unwrap();
    waiting_rx.await.unwrap();
    let progress = NativeTask::spawn(async { 1 }).unwrap();
    assert!(matches!(
        tokio::time::timeout(PHASE_TIMEOUT, progress).await.unwrap(),
        NativeTaskOutcome::Completed(1)
    ));
    let NativeTaskOutcome::Completed(reading) = readiness.await else {
        panic!("the readiness wait must complete while the runtime remains held");
    };
    release.send(()).unwrap();
    assert!(!held.join().unwrap());
    assert!(
        matches!(reading, Ok(CodeIndexReadinessWaitReadV1::TimedOut { .. })),
        "runtime contention must remain within the original readiness budget: {reading:?}"
    );
    let recovered = ready_reading(&fixture, &project, "after runtime contention").await;
    assert_eq!(
        recovered.snapshot_content_identity,
        initial.snapshot_content_identity
    );
    let record = replay.lease_next().await.unwrap().unwrap();
    replay.commit(record.seq).await.unwrap();
    drop(replay);
    fixture.server.shutdown().await;
    let shutdown = fixture.engine.shutdown_all().await;
    assert!(
        shutdown.project_servers.is_clean(),
        "contention shutdown: {shutdown:?}"
    );
}
