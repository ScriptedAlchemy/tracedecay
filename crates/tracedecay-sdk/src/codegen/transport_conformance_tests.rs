use super::{OperationTransport, canonical_application_registry, canonical_operations};

#[test]
fn generated_multi_root_operations_use_the_project_application_mount() {
    let registry = canonical_application_registry().expect("canonical SDK registry");
    let operations = canonical_operations(&registry).expect("canonical generated operations");

    for (operation_id, expected_route) in [
        (
            "operation.multi_root.scope_set_read",
            "/application/multi-root/scope-set/read",
        ),
        (
            "operation.multi_root.scope_set_compare_and_swap",
            "/application/multi-root/scope-set/compare-and-swap",
        ),
        (
            "operation.multi_root.execute",
            "/application/multi-root/execute",
        ),
    ] {
        let generated = operations
            .iter()
            .find(|candidate| candidate.operation_id == operation_id)
            .unwrap_or_else(|| panic!("{operation_id} must be generated"));
        assert!(matches!(
            &generated.transport,
            OperationTransport::Http { route } if route == expected_route
        ));
    }
}

#[test]
fn generated_feedback_and_non_session_primitive_operations_use_live_http_routes() {
    let registry = canonical_application_registry().expect("canonical SDK registry");
    let operations = canonical_operations(&registry).expect("canonical generated operations");

    for (operation, route) in [
        ("feedback_diagnostics", "/application/feedback/diagnostics"),
        ("feedback_get", "/application/feedback/get"),
        ("feedback_expand", "/application/feedback/expand"),
        ("feedback_list", "/application/feedback/list"),
        ("feedback_impact", "/application/feedback/impact"),
        (
            "feedback_advisory_cycle",
            "/application/feedback/advisory_cycle",
        ),
        ("affected_tests", "/application/tests/affected"),
        ("test_results", "/application/tests/results"),
        ("qualified_name", "/application/primitives/qualified_name"),
        ("call_chain", "/application/primitives/call_chain"),
        ("file_dependents", "/application/primitives/file_dependents"),
        ("source_lines", "/application/primitives/source_lines"),
        ("source_body", "/application/primitives/source_body"),
        ("source_outline", "/application/primitives/source_outline"),
        ("module_api", "/application/primitives/module_api"),
        ("file_metadata", "/application/primitives/file_metadata"),
        ("health_read", "/application/primitives/health_read"),
        ("health_delta", "/application/primitives/health_delta"),
        ("storage_status", "/application/primitives/storage_status"),
        (
            "diagnostics_read",
            "/application/primitives/diagnostics_read",
        ),
    ] {
        let operation_id = format!("operation.application.{operation}");
        let generated = operations
            .iter()
            .find(|candidate| candidate.operation_id == operation_id)
            .unwrap_or_else(|| panic!("{operation_id} must be generated"));
        assert!(matches!(
            &generated.transport,
            OperationTransport::Http { route: generated_route } if generated_route == route
        ));
    }

    let session_lookup = operations
        .iter()
        .find(|operation| operation.operation_id == "operation.application.session_lookup")
        .expect("session lookup SDK operation");
    assert!(matches!(
        &session_lookup.transport,
        OperationTransport::McpTool { .. }
    ));
}
