use tracedecay_application::WorkProductBindingV1;
use tracedecay_domain::ProjectId;
use tracedecay_global_db::tests::harness::RegisteredGlobalDbTestRuntime;
use tracedecay_runtime_core::errors::TraceDecayError;
use tracedecay_tool_catalog::{CapabilityId, UseCaseId};
use tracedecay_usecases::work::{
    RegisteredWorkApplicationServicesV1, RegisteredWorkProductServicesV1,
    RegisteredWorkflowApplicationServicesV1, work_intelligence_service,
};

fn project_id() -> ProjectId {
    ProjectId::new("project.work-service-attach").expect("project id")
}

fn product_binding() -> WorkProductBindingV1 {
    WorkProductBindingV1::new(
        CapabilityId::new("capability.work.graph.read").expect("capability"),
        UseCaseId::new("use-case.work.graph.read").expect("use case"),
    )
}

fn assert_unbound_topology(error: TraceDecayError, operation: &str) {
    match error {
        TraceDecayError::Database {
            operation: got_operation,
            message,
        } => {
            assert_eq!(got_operation, operation);
            assert_eq!(message, "project graph runtime is not bound");
        }
        other => panic!("expected typed Database attach failure, got {other:?}"),
    }
}

#[tokio::test]
async fn work_application_attach_fails_without_project_graph_runtime() {
    tracedecay_global_db::register_test_schema_installer();
    let profile = tempfile::tempdir().expect("profile");
    let project = tempfile::tempdir().expect("project");
    let runtime =
        RegisteredGlobalDbTestRuntime::project(profile.path(), project.path(), project_id())
            .await
            .expect("registered project store opens");
    let Err(error) = RegisteredWorkApplicationServicesV1::attach(
        runtime.project_database().expect("project database"),
    ) else {
        panic!("Work application attach requires a bound project graph");
    };
    assert_unbound_topology(error, "attach registered Work topology");
}

#[tokio::test]
async fn workflow_application_attach_fails_without_project_graph_runtime() {
    tracedecay_global_db::register_test_schema_installer();
    let profile = tempfile::tempdir().expect("profile");
    let project = tempfile::tempdir().expect("project");
    let runtime =
        RegisteredGlobalDbTestRuntime::project(profile.path(), project.path(), project_id())
            .await
            .expect("registered project store opens");
    let Err(error) = RegisteredWorkflowApplicationServicesV1::attach(
        runtime.project_database().expect("project database"),
    ) else {
        panic!("workflow application attach requires a bound project graph");
    };
    assert_unbound_topology(error, "attach registered workflow topology");
}

#[tokio::test]
async fn work_product_and_intelligence_attach_from_storage_without_graph_runtime() {
    tracedecay_global_db::register_test_schema_installer();
    let profile = tempfile::tempdir().expect("profile");
    let project = tempfile::tempdir().expect("project");
    let runtime =
        RegisteredGlobalDbTestRuntime::project(profile.path(), project.path(), project_id())
            .await
            .expect("registered project store opens");
    let db = runtime.project_database().expect("project database");
    RegisteredWorkProductServicesV1::attach(db, product_binding())
        .expect("product services attach from work storage alone");
    work_intelligence_service(db, product_binding())
        .expect("intelligence attaches from work storage alone");
}
