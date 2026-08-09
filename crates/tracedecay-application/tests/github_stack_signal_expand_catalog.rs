use schemars::schema_for;
use tracedecay_application::git::{
    GitHubStackSignalExpandSurfaceRequest, GitHubStackSignalExpandSurfaceResultV1,
    git_surface_executable_binding_registry,
};
use tracedecay_application::git_surface_catalog_contribution;
use tracedecay_tool_catalog::{BindingSurface, CapabilityId, RouteExposureV1};

const CAPABILITY: &str = "capability.application.github-stack.signal-expand";
const OPERATION: &str = "github_stack_signal_expand";

#[test]
fn github_stack_signal_expand_is_schema_backed_and_publicly_mounted() {
    let contribution = git_surface_catalog_contribution().expect("Git surface contribution");
    let capability_id = CapabilityId::new(CAPABILITY).expect("capability ID");
    let capability = contribution
        .capabilities()
        .iter()
        .find(|candidate| candidate.capability_id() == &capability_id)
        .expect("GitHub stack signal expansion capability");
    let operation = tracedecay_application::git::git_surface_operation(OPERATION)
        .expect("Git surface operation")
        .expect("GitHub stack signal expansion operation");
    assert_eq!(operation.capability_id(), &capability_id);
    assert_eq!(
        operation.use_case_id().as_str(),
        "use-case.application.github-stack.signal-expand"
    );
    let schema = contribution
        .executable_schema(&capability_id)
        .expect("GitHub stack signal expansion schema");

    let expected_request = serde_json::to_value(schema_for!(GitHubStackSignalExpandSurfaceRequest))
        .expect("request schema JSON");
    assert_eq!(
        schema.request_schema().body()["properties"],
        expected_request["properties"]
    );
    let result_schema = schema.result_schema().body().to_string();
    assert!(result_schema.contains("expanded"));
    assert!(result_schema.contains("unavailable"));
    assert!(capability.binding_ids().iter().any(|binding_id| {
        contribution
            .bindings()
            .iter()
            .find(|binding| binding.binding_id() == binding_id)
            .is_some_and(|binding| {
                binding.surface() == BindingSurface::Mcp
                    && binding.operation().as_str() == OPERATION
            })
    }));

    let registry = git_surface_executable_binding_registry().expect("Git HTTP registry");
    let binding = registry
        .iter()
        .filter_map(|availability| availability.binding())
        .find(|binding| {
            binding.operation_id().as_str() == "operation.application.github_stack_signal_expand"
        })
        .expect("GitHub stack signal expansion executable binding");
    assert!(matches!(
        binding.exposure(),
        RouteExposureV1::Public { route_path, .. }
            if route_path == "/application/github-stack/signal-expand"
    ));
}

#[test]
fn github_stack_signal_expand_result_schema_stays_bounded() {
    let schema = serde_json::to_value(schema_for!(GitHubStackSignalExpandSurfaceResultV1))
        .expect("result schema JSON");
    let rendered = schema.to_string();
    for field in [
        "signal_id",
        "watermark_id",
        "stack_revision_digest",
        "state_digest",
        "observed_at",
    ] {
        assert!(
            rendered.contains(&format!("\"{field}\"")),
            "missing {field}"
        );
    }
    assert!(!rendered.contains("repository_path"));
    assert!(!rendered.contains("pull_request_body"));
    assert!(!rendered.contains("commit_message"));
}
