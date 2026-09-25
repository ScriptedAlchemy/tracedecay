use serde_json::json;
use tracedecay_contracts::sdk_executable_binding_registry;
use tracedecay_tool_catalog::{OperationId, SdkTransportBindingV1};

#[test]
fn established_primitive_tools_are_typed_sdk_operations() {
    let registry = sdk_executable_binding_registry().expect("canonical SDK registry");

    for (operation, required) in [
        ("context", json!(["task"])),
        ("impact", json!(["node_id"])),
        ("node", json!(["node_id"])),
        ("port_order", json!(["source_dir"])),
        ("port_status", json!(["source_dir", "target_dir"])),
        ("rename_preview", json!(["node_id"])),
        (
            "similar",
            json!([
                "project_id",
                "repository_id",
                "target",
                "match_classes",
                "result_limit",
                "work_limit"
            ]),
        ),
        ("todos", json!([])),
    ] {
        let operation_id = format!("operation.application.{operation}");
        let binding = registry
            .get(&OperationId::new(operation_id.clone()).expect("operation ID"))
            .and_then(|availability| availability.binding())
            .unwrap_or_else(|| panic!("{operation_id} must be executable"));
        assert!(
            matches!(
                binding.transport(),
                SdkTransportBindingV1::McpTool { tool_name }
                    if tool_name == &format!("tracedecay_{operation}")
            ),
            "{operation}"
        );
        let request = binding.request_schema().body();
        let observed_required = request.get("required").cloned().unwrap_or(json!([]));
        assert_eq!(observed_required, required, "{operation}");
    }
}
