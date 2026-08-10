use tracedecay_application::multi_root::{
    MultiRootApplicationOperation, multi_root_executable_binding_registry,
};
use tracedecay_tool_catalog::{OperationId, RouteExposureV1};

#[test]
fn multi_root_catalog_binds_every_canonical_http_route() {
    let registry = multi_root_executable_binding_registry().expect("multi-root catalog");

    for (operation, expected_route) in [
        (
            MultiRootApplicationOperation::ScopeSetRead,
            "/application/multi-root/scope-set/read",
        ),
        (
            MultiRootApplicationOperation::ScopeSetCompareAndSwap,
            "/application/multi-root/scope-set/compare-and-swap",
        ),
        (
            MultiRootApplicationOperation::Execute,
            "/application/multi-root/execute",
        ),
    ] {
        let operation_id = OperationId::new(operation.operation_id()).expect("operation id");
        let binding = registry
            .get(&operation_id)
            .and_then(|availability| availability.binding())
            .expect("available multi-root binding");
        let RouteExposureV1::Public { route_path, .. } = binding.exposure() else {
            panic!("multi-root binding must be public");
        };
        assert_eq!(route_path, expected_route);
    }
}
