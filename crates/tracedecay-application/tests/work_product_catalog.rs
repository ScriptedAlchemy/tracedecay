use tracedecay_application::{
    WORK_PRODUCT_OPERATION_IDS_V1, WorkProductOperationV1, work_product_executable_binding_registry,
};
use tracedecay_tool_catalog::{EffectClass, OperationId, RouteExposureV1};

#[test]
fn canonical_catalog_binds_every_product_operation_to_its_typed_route() {
    let registry = work_product_executable_binding_registry().unwrap();
    assert_eq!(
        WORK_PRODUCT_OPERATION_IDS_V1.len(),
        WorkProductOperationV1::ALL.len()
    );
    for operation in WorkProductOperationV1::ALL {
        let operation_id = OperationId::new(format!("operation.work.{}", operation.key())).unwrap();
        let binding = registry.get(&operation_id).unwrap().binding().unwrap();
        assert_eq!(
            binding.effect(),
            if operation.is_read_only() {
                EffectClass::Read
            } else {
                EffectClass::Administrative
            }
        );
        assert!(matches!(
            binding.exposure(),
            RouteExposureV1::Public { route_path, .. }
                if route_path.starts_with("/application/work/product/")
        ));
        assert_ne!(binding.request_schema().body()["title"], "Value");
        assert_ne!(binding.result_schema().body()["title"], "Value");
    }
}
