use super::*;
use tracedecay_semantic_contracts::SemanticRuntimeScheduleStatusV1;

#[test]
fn semantic_profile_transition_coordinates_only_when_a_profile_changes() {
    assert!(
        !requires_coordinated_semantic_profile_transition(false, false),
        "an inactive profile update must remain an ordinary configuration mutation"
    );
    assert!(
        requires_coordinated_semantic_profile_transition(true, false),
        "disabling an active profile must retain coordinated rollback"
    );
    assert!(
        requires_coordinated_semantic_profile_transition(false, true),
        "selecting a profile from an inactive state must retain activation"
    );
    assert!(
        requires_coordinated_semantic_profile_transition(true, true),
        "replacing an active profile must retain activation"
    );
}

#[tokio::test]
async fn semantic_commit_wake_survives_deferred_reconciler_install() {
    let registered_configuration_wake = Arc::new(Notify::new());
    notify_committed_semantic_activation(&registered_configuration_wake);

    let deferred_reconciler_wake = Arc::clone(&registered_configuration_wake);
    tokio::time::timeout(
        Duration::from_millis(100),
        deferred_reconciler_wake.notified(),
    )
    .await
    .expect("a semantic commit before reconciler installation must retain its wake");
}

#[tokio::test]
async fn semantic_scheduler_is_daemon_private_retained_state_not_a_wire_operation() {
    let service = DaemonInvocationService::default();
    let registrar = DaemonSemanticRuntimeRegistrar::new(&service);
    let project_root = PathBuf::from("/project/semantic-runtime");
    let handle = tracedecay_semantic::DaemonSemanticRuntimeHandleV1::new(1, 8, 1 << 20)
        .expect("semantic scheduler");

    registrar
        .register(project_root.clone(), handle.clone())
        .await
        .expect("mount semantic scheduler");
    assert_eq!(
        service
            .project_runtimes
            .get::<tracedecay_semantic::DaemonSemanticRuntimeHandleV1>(&project_root)
            .await
            .expect("retained semantic scheduler")
            .status(),
        SemanticRuntimeScheduleStatusV1::Unavailable
    );
    assert!(matches!(
        registrar.register(project_root, handle).await,
        Err(DaemonSemanticRuntimeRegistrationError::AlreadyRegistered)
    ));
    assert!(
        serde_json::to_string(&DaemonInvocationOperation::LspOpen)
            .expect("serialize existing operation")
            .find("semantic")
            .is_none(),
        "semantic scheduling must not add a public daemon operation"
    );
}
