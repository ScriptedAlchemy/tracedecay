//! Configuration registrar coverage.

#[tokio::test]
async fn read_only_project_configuration_requires_the_bootstrap_profile_plan() {
    let profile = tempfile::tempdir().expect("profile root");
    let project = tempfile::tempdir().expect("project root");
    let (graph, runtime) =
        tracedecay_project::project::TraceDecay::init_test_fixture_with_registered_runtime(
            profile.path(),
            project.path(),
            "project.configuration.read-only-worker-plan",
        )
        .await
        .expect("registered graph");
    graph.close();
    let read_only = runtime
        .open_project_graph_read_only_for_test(
            project.path(),
            tracedecay_project::project::TraceDecayOpenOptions {
                profile_root: Some(profile.path().to_path_buf()),
                global_db_path: None,
            },
        )
        .await
        .expect("read-only project graph");
    assert!(!read_only.db().is_writable());

    let invocation = crate::daemon::invocation_state::DaemonInvocationState::default();
    let profile_identity =
        tracedecay_daemon_identity::profile_identity::load_or_create(profile.path())
            .expect("profile identity");
    let profile_sessions = runtime
        .session_registry_for_test()
        .profile_sessions()
        .await
        .expect("profile sessions authority");
    invocation
        .install_profile_worker_plan_for_test(profile_identity, profile_sessions)
        .await
        .expect("daemon bootstrap worker plan");
    invocation
        .configuration_runtime_registrar()
        .ensure_worker_plan()
        .expect("read-only project worker-plan boundary");

    assert!(tracedecay_code_index::parallelism::installed_worker_status().is_some());
}
