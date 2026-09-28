//! CLI dispatch resolves bindings without generating executable JSON Schema bodies.
//!
//! On the previous path, resolving any tool call composed the full application
//! catalog, and a graph-tool deadline lookup composed it again. Both walks
//! generate a schema body per request and result type. This test is the
//! behavior that walk must not happen: the generation counter stays put while
//! search, plan context, callers, and file dependents still resolve.

use serde_json::json;
use tracedecay_contracts::{CancellationSignal, Deadline, PageRequest, RequestId};
use tracedecay_daemon_protocol::{RequestedOutputFormat, parse_application_surface_request};
use tracedecay_daemon_service::application_surface::{
    application_operation_deadline_ceiling, application_surface_binding_catalog_ref,
    application_surface_catalog_ref, resolve_application_surface_dispatch_with_controls,
};
use tracedecay_domain::UtcMicros;
use tracedecay_tool_catalog::{
    ApplicationSurfaceOperation, BindingId, BindingSurface, CapabilityId,
    executable_schema_body_generations,
};

#[test]
fn cli_dispatch_does_not_generate_executable_schema_bodies() {
    let operations = [
        (
            ApplicationSurfaceOperation::Search,
            json!({"query": "alpha", "limit": 3}),
        ),
        (
            ApplicationSurfaceOperation::Context,
            json!({"task": "what calls alpha", "mode": "plan", "max_nodes": 4}),
        ),
        (
            ApplicationSurfaceOperation::CodeCallers,
            json!({"node_id": "symbol.v1.example", "maximum_depth": 1}),
        ),
        (
            ApplicationSurfaceOperation::FileDependents,
            json!({"file": "src/lib.rs"}),
        ),
    ];
    let before = executable_schema_body_generations();
    let mut resolved = Vec::new();
    for (index, (operation, arguments)) in operations.into_iter().enumerate() {
        let ceiling =
            application_operation_deadline_ceiling(operation).expect("capability deadline");
        assert!(ceiling.as_millis() > 0);
        let request =
            parse_application_surface_request(operation, arguments).expect("surface request");
        let dispatched = resolve_application_surface_dispatch_with_controls(
            BindingSurface::Cli,
            operation,
            RequestId::new(format!("request.cli-dispatch-schema-bodies.{index}"))
                .expect("request id"),
            request,
            PageRequest::first(10).expect("page"),
            Some(Deadline::new(UtcMicros(60_000_000)).expect("deadline")),
            CancellationSignal::active(format!("cancel.cli-dispatch-schema-bodies.{index}"))
                .expect("cancellation"),
            RequestedOutputFormat::Json,
        )
        .expect("cli dispatch");
        let binding_catalog = application_surface_binding_catalog_ref().expect("binding catalog");
        let binding = binding_catalog
            .binding(&dispatched.invocation.binding_id)
            .expect("resolved binding is in the schema-free catalog");
        let capability = binding_catalog
            .capability(binding.capability_id())
            .expect("capability");
        assert!(
            binding_catalog
                .executable_schema(capability.capability_id())
                .is_none(),
            "{} dispatch catalog carries a schema body",
            operation.as_str()
        );
        assert_eq!(
            dispatched.invocation.request_schema,
            capability.request_schema().clone()
        );
        assert_eq!(
            dispatched.invocation.result_schema,
            capability.result_schema().clone()
        );
        assert_eq!(
            u64::try_from(ceiling.as_millis()).expect("deadline millis"),
            capability.deadline().maximum_millis()
        );
        resolved.push((
            operation,
            dispatched.invocation.binding_id,
            capability.capability_id().clone(),
        ));
    }
    assert_eq!(
        executable_schema_body_generations(),
        before,
        "cli dispatch generated executable schema bodies"
    );

    let binding_catalog = application_surface_binding_catalog_ref().expect("binding catalog");
    assert!(binding_catalog.capabilities().all(|capability| {
        binding_catalog
            .executable_schema(capability.capability_id())
            .is_none()
    }));
    let full_catalog = application_surface_catalog_ref().expect("full catalog");
    assert!(
        executable_schema_body_generations() > before,
        "the full catalog is what generates schema bodies"
    );
    assert!(full_catalog.capabilities().any(|capability| {
        full_catalog
            .executable_schema(capability.capability_id())
            .is_some()
    }));
    for (operation, binding_id, capability_id) in resolved {
        assert_binding_identity(operation, &binding_id, &capability_id);
    }
}

fn assert_binding_identity(
    operation: ApplicationSurfaceOperation,
    binding_id: &BindingId,
    capability_id: &CapabilityId,
) {
    let binding_catalog = application_surface_binding_catalog_ref().expect("binding catalog");
    let full_catalog = application_surface_catalog_ref().expect("full catalog");
    let full_binding = full_catalog.binding(binding_id).unwrap_or_else(|| {
        panic!(
            "{} binding id is missing from the full catalog",
            operation.as_str()
        )
    });
    assert_eq!(full_binding.capability_id(), capability_id);
    let full_capability = full_catalog
        .capability(capability_id)
        .expect("full capability");
    let binding_capability = binding_catalog
        .capability(capability_id)
        .expect("binding capability");
    assert_eq!(
        binding_capability.request_schema(),
        full_capability.request_schema()
    );
    assert_eq!(
        binding_capability.result_schema(),
        full_capability.result_schema()
    );
    assert_eq!(
        binding_capability.deadline().maximum_millis(),
        full_capability.deadline().maximum_millis()
    );
    assert!(
        full_catalog.executable_schema(capability_id).is_some(),
        "{} full catalog is missing its schema body",
        operation.as_str()
    );
}
