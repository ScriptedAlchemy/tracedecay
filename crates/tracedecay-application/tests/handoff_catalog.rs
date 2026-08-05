use tracedecay_application::handoff_executable_binding_registry;
use tracedecay_tool_catalog::RouteExposureV1;

#[test]
fn registry_exposes_both_typed_daemon_handoff_open_operations() {
    let registry = handoff_executable_binding_registry().unwrap();
    let bindings = registry
        .iter()
        .filter_map(|availability| availability.binding())
        .collect::<Vec<_>>();
    assert_eq!(bindings.len(), 2);
    assert_eq!(
        bindings
            .iter()
            .map(|binding| binding.operation_id().as_str())
            .collect::<Vec<_>>(),
        vec![
            "operation.handoff.open_investigation_handoff",
            "operation.handoff.open_task_handoff",
        ]
    );
    assert_eq!(
        bindings
            .iter()
            .map(|binding| match binding.exposure() {
                RouteExposureV1::Public { route_path, .. } => route_path.as_str(),
                _ => panic!("handoff opens must use daemon-owned public routes"),
            })
            .collect::<Vec<_>>(),
        vec![
            "/application/handoff/open-investigation",
            "/application/handoff/open-task",
        ]
    );
    assert!(bindings.iter().all(|binding| {
        binding
            .request_schema()
            .body()
            .to_string()
            .contains("session_id")
    }));
}
