use std::sync::Arc;

use tempfile::TempDir;
use tracedecay_agent_hosts::automation::config::{
    AutomationBackend, AutomationConfigPatch, AutomationTaskPatch,
};
use tracedecay_agent_hosts::automation::run_ledger::load_run_records;
use tracedecay_agent_hosts::automation::scheduler::{
    AutomationSchedulerControl, save_scheduler_control,
};

use super::super::{
    DaemonHandshake, apply_project_automation_patch_via_surface, enter_test_daemon_database_scope,
    initialize_test_project, isolate_codex_app_server_binary, test_client_identity_for,
    test_daemon_engine_for_profile, test_handshake_defaults,
};

#[tokio::test]
async fn automation_scheduler_tick_respects_pause_control_without_backend_call() {
    let dir = TempDir::new().expect("temp dir");
    let _codex_bin = isolate_codex_app_server_binary(dir.path());
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
    let _database_scope = enter_test_daemon_database_scope(
        &handshake.client_identity.profile_root,
        "paused-scheduler-tick-test",
    );
    let server = apply_project_automation_patch_via_surface(
        &engine,
        &handshake,
        AutomationConfigPatch {
            enabled: Some(true),
            backend: Some(AutomationBackend::CodexAppServer),
            memory_curator: AutomationTaskPatch {
                enabled: Some(true),
                schedule: Some(Some("every:1m".to_string())),
                ..AutomationTaskPatch::default()
            },
            ..AutomationConfigPatch::default()
        },
    )
    .await;
    let cg = server.cg().await;
    save_scheduler_control(
        &dashboard_root,
        &AutomationSchedulerControl { paused: true },
    )
    .await
    .expect("save paused scheduler control");

    let run_control = tracedecay_agent_hosts::automation::AutomationRunControl::from_interrupted(
        Arc::new(|| false),
    );
    Box::pin(super::super::super::run_automation_scheduler_tick(
        &project,
        &cg,
        &handshake,
        &engine,
        &run_control,
    ))
    .await
    .expect("paused scheduler tick should exit cleanly");

    let records = load_run_records(&dashboard_root, 10)
        .await
        .expect("load run ledger");
    assert!(
        records.is_empty(),
        "paused scheduler tick must not call backends or append run records"
    );
}
