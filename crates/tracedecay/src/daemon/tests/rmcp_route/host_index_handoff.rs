use tracedecay_contracts::code_index_freshness::{
    CodeIndexReadinessTargetV1, CodeIndexReadinessV1, CodeIndexReadinessWaitReadV1,
};
use tracedecay_session_memory::event_lane::{self, ActivityFamilyV1};

use super::*;

async fn ready_reading(
    fixture: &RmcpRouteFixture,
    project: &std::path::Path,
) -> tracedecay_contracts::code_index_freshness::CodeIndexWorktreeFreshnessV1 {
    let reading = fixture
        .engine
        .invocation
        .code_index_schedulers
        .wait_for_readiness(project, CodeIndexReadinessTargetV1::Ready, PHASE_TIMEOUT)
        .await
        .unwrap();
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
    let initial = ready_reading(&fixture, &project).await;
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
        assert!(owner.is_some(), "initial ready reading: {initial:?}");
        owner
    } else {
        None
    };
    let replay = broker.begin_replay().await.unwrap();
    assert_eq!(broker.pending_replay_count().await.unwrap(), 0);
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
    assert_ne!(
        owed.readiness(CodeIndexReadinessTargetV1::Ready),
        CodeIndexReadinessV1::Reached
    );
    assert_ne!(
        owed.readiness(CodeIndexReadinessTargetV1::Fresh),
        CodeIndexReadinessV1::Reached
    );
    assert_eq!(
        owed.hook_hint_count,
        Some(0),
        "broker debt is not a scheduler hint"
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
    let saved = ready_reading(&fixture, &project).await;
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
