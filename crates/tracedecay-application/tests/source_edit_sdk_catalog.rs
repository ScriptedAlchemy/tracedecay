use std::collections::BTreeSet;

use tracedecay_application::{sdk_executable_binding_registry, source_edit_catalog_contribution};
use tracedecay_tool_catalog::{BindingStatus, BindingSurface, OperationId, SdkTransportBindingV1};

#[test]
fn sdk_registry_projects_source_edit_with_its_exact_mcp_schemas() {
    let contribution = source_edit_catalog_contribution().expect("source-edit contribution");
    let registry = sdk_executable_binding_registry().expect("SDK registry");
    let mut projected_capabilities = BTreeSet::new();

    for surface in contribution.bindings().iter().filter(|binding| {
        binding.surface() == BindingSurface::Mcp
            && matches!(binding.status(), BindingStatus::Current)
            && !binding.is_alias()
    }) {
        let operation_id = OperationId::new(format!(
            "operation.application.{}",
            surface.operation().as_str()
        ))
        .expect("source-edit SDK operation ID");
        let schema = contribution
            .executable_schema(surface.capability_id())
            .expect("source-edit executable schema");
        let binding = registry
            .get(&operation_id)
            .and_then(|availability| availability.binding())
            .expect("source-edit operation must be SDK-callable");

        assert_eq!(binding.binding_id(), surface.binding_id());
        assert_eq!(binding.sdk_method(), surface.operation());
        assert_eq!(binding.request_schema(), schema.request_schema());
        assert_eq!(binding.result_schema(), schema.result_schema());
        assert!(matches!(
            binding.transport(),
            SdkTransportBindingV1::McpTool { tool_name }
                if tool_name == &format!("tracedecay_{}", surface.operation().as_str())
        ));
        projected_capabilities.insert(surface.capability_id());
    }

    assert_eq!(
        projected_capabilities.len(),
        contribution.capabilities().len(),
        "every source-edit capability must have one current MCP SDK projection"
    );
}
