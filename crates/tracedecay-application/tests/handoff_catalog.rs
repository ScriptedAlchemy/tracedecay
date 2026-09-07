use tracedecay_application::handoff_executable_binding_registry;
use tracedecay_tool_catalog::{CancellationContract, EffectClass, RouteExposureV1};

#[test]
fn registry_exposes_typed_daemon_handoff_issue_list_and_open_operations() {
    let registry = handoff_executable_binding_registry().unwrap();
    let bindings = registry
        .iter()
        .filter_map(|availability| availability.binding())
        .collect::<Vec<_>>();
    assert_eq!(bindings.len(), 4);
    assert_eq!(
        bindings
            .iter()
            .map(|binding| binding.operation_id().as_str())
            .collect::<Vec<_>>(),
        vec![
            "operation.handoff.issue_task_handoff",
            "operation.handoff.list_task_handoffs",
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
            "/application/handoff/issue-task",
            "/application/handoff/list-task",
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
    assert!(
        bindings
            .iter()
            .all(|binding| binding.cancellation() == &CancellationContract::NotCancellable)
    );

    // The enumeration is the one handoff operation that commits nothing, and
    // the catalog has to say so: catalogued as an effect it would advertise a
    // durable receipt and a required idempotency key for a pure read.
    let effect_of = |operation: &str| {
        bindings
            .iter()
            .find(|binding| binding.operation_id().as_str() == operation)
            .map(|binding| binding.effect())
            .expect("operation is registered")
    };
    assert_eq!(
        effect_of("operation.handoff.list_task_handoffs"),
        EffectClass::Read
    );
    for mutating in [
        "operation.handoff.issue_task_handoff",
        "operation.handoff.open_investigation_handoff",
        "operation.handoff.open_task_handoff",
    ] {
        assert_eq!(effect_of(mutating), EffectClass::Administrative);
    }
}
