use std::collections::BTreeSet;

use tracedecay_application::sdk_executable_binding_registry;
use tracedecay_tool_catalog::{OperationId, SdkTransportBindingV1};

const TYPED_PRIMITIVE_OPERATIONS: [&str; 10] = [
    "callees",
    "context",
    "impact",
    "node",
    "port_order",
    "port_status",
    "redundancy",
    "rename_preview",
    "similar",
    "todos",
];

#[test]
fn established_primitive_tools_are_typed_sdk_operations() {
    let registry = sdk_executable_binding_registry().expect("canonical SDK registry");
    let expected = TYPED_PRIMITIVE_OPERATIONS
        .iter()
        .map(|operation| format!("operation.application.{operation}"))
        .collect::<BTreeSet<_>>();

    for operation_id in expected {
        let binding = registry
            .get(&OperationId::new(operation_id.clone()).expect("operation ID"))
            .and_then(|availability| availability.binding())
            .unwrap_or_else(|| panic!("{operation_id} must be executable"));
        assert!(matches!(
            binding.transport(),
            SdkTransportBindingV1::McpTool { tool_name }
                if tool_name == &format!(
                    "tracedecay_{}",
                    operation_id.trim_start_matches("operation.application.")
                )
        ));
        assert_eq!(binding.request_schema().body()["type"], "object");
        assert_ne!(
            binding.result_schema().body(),
            &serde_json::Value::Bool(true)
        );
    }
}
