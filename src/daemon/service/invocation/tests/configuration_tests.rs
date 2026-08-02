//! `configuration` module test coverage (split from the former monolithic
//! `invocation::tests` module).

use super::*;

#[tokio::test]
async fn semantic_scheduler_is_daemon_private_retained_state_not_a_wire_operation() {
    let service = DaemonInvocationService::default();
    let registrar = DaemonSemanticRuntimeRegistrar::new(&service);
    let project_root = PathBuf::from("/project/semantic-runtime");
    let handle = crate::semantic_code::DaemonSemanticRuntimeHandleV1::new(1, 8, 1 << 20)
        .expect("semantic scheduler");

    registrar
        .register(project_root.clone(), handle.clone())
        .await
        .expect("mount semantic scheduler");
    assert_eq!(
        service
            .project_runtimes
            .get::<crate::semantic_code::DaemonSemanticRuntimeHandleV1>(&project_root)
            .await
            .expect("retained semantic scheduler")
            .status(),
        crate::semantic_code::SemanticRuntimeScheduleStatusV1::Unavailable
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
