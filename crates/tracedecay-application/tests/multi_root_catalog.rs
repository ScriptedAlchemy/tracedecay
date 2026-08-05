use tracedecay_application::{
    MultiRootApplicationOperation, multi_root_executable_binding_registry,
};
use tracedecay_tool_catalog::{
    ExecutableBindingAvailabilityV1, ExecutableUnavailableDispositionV1, OperationId,
};

#[test]
fn multi_root_catalog_keeps_unmounted_routes_typed_unavailable() {
    let registry = multi_root_executable_binding_registry().expect("multi-root catalog");

    for operation in MultiRootApplicationOperation::ALL {
        let operation_id = OperationId::new(operation.operation_id()).expect("operation id");
        assert!(matches!(
            registry.get(&operation_id),
            Some(ExecutableBindingAvailabilityV1::Unavailable {
                disposition: ExecutableUnavailableDispositionV1::CapabilityDisabled,
                ..
            })
        ));
    }
}
