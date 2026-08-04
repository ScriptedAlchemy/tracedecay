use tracedecay_application::git_sdk_executable_binding_registry;
use tracedecay_tool_catalog::{OperationId, SdkTransportBindingV1};

#[test]
fn public_git_operations_are_schema_backed_mcp_sdk_bindings() {
    let registry = git_sdk_executable_binding_registry().expect("Git SDK registry");

    for (operation_id, method, tool_name) in [
        (
            "operation.application.git.status",
            "git_status",
            "tracedecay_git_status",
        ),
        (
            "operation.application.git.diff",
            "git_diff",
            "tracedecay_git_diff",
        ),
        (
            "operation.application.git.history",
            "git_history",
            "tracedecay_git_history",
        ),
        (
            "operation.application.git.blame",
            "git_blame",
            "tracedecay_git_blame",
        ),
        (
            "operation.application.git.hunks",
            "git_hunks",
            "tracedecay_git_hunks",
        ),
        (
            "operation.application.git.preview",
            "git_preview",
            "tracedecay_git_preview",
        ),
        (
            "operation.application.git.apply",
            "git_apply",
            "tracedecay_git_apply",
        ),
    ] {
        let binding = registry
            .get(&OperationId::new(operation_id).expect("operation ID"))
            .and_then(|availability| availability.binding())
            .expect("mounted Git SDK binding");

        assert_eq!(binding.sdk_method().as_str(), method);
        assert!(matches!(
            binding.transport(),
            SdkTransportBindingV1::McpTool { tool_name: actual } if actual == tool_name
        ));
        assert!(binding.request_schema().body().is_object());
        assert!(binding.result_schema().body().is_object());
    }
}
