use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use tracedecay_application::WorkProductBindingV1;
use tracedecay_domain::errors::TraceDecayError;
use tracedecay_domain::{BrainId, ProjectId, UserProfileId};
use tracedecay_global_db::tests::harness::RegisteredGlobalDbTestRuntime;
use tracedecay_graph_db::{
    GraphDbError, GraphGenerationManifest, GraphIdempotencyKey, GraphNamespace, GraphProjectionId,
    GraphProjectionIdentity, VerifiedGraphSnapshot,
};
use tracedecay_runtime_core::db::{
    Database, DatabaseAuthority, TestDatabaseRuntimeMode, TestDatabaseRuntimeScope,
    TestRuntimeProfileIdentityV1,
};
use tracedecay_runtime_core::store_runtime::VerifiedGraphRuntimePortV1;
use tracedecay_store::{FactReadControl, StoreRuntimeBindingV1, VerifiedStoreLocatorV1};
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

struct StubProjectGraphRuntime {
    binding: StoreRuntimeBindingV1,
    locator: VerifiedStoreLocatorV1,
}

impl VerifiedGraphRuntimePortV1 for StubProjectGraphRuntime {
    fn relational_binding(&self) -> &StoreRuntimeBindingV1 {
        &self.binding
    }

    fn relational_verified_locator(&self) -> &VerifiedStoreLocatorV1 {
        &self.locator
    }

    fn cancel_reconciliation(&self) {}

    fn publish_verified_manifest(
        &self,
        _manifest: &GraphGenerationManifest,
        _idempotency_key: GraphIdempotencyKey,
        _cancelled: Arc<AtomicBool>,
    ) -> Result<VerifiedGraphSnapshot, GraphDbError> {
        Err(GraphDbError::unavailable(
            "stub project graph runtime has no publication",
        ))
    }

    fn reconcile_verified_manifest(
        &self,
        _manifest: &GraphGenerationManifest,
        _idempotency_key: GraphIdempotencyKey,
    ) -> Result<VerifiedGraphSnapshot, GraphDbError> {
        Err(GraphDbError::unavailable(
            "stub project graph runtime has no reconciliation",
        ))
    }

    fn verified_snapshot(
        &self,
        _projection: &GraphProjectionIdentity,
        _read_control: FactReadControl,
    ) -> Result<Option<VerifiedGraphSnapshot>, GraphDbError> {
        Ok(None)
    }
}

fn probe_projection() -> GraphProjectionIdentity {
    GraphProjectionIdentity::new(
        GraphNamespace::new("project").expect("graph namespace"),
        GraphProjectionId::new("projection.work-service-attach").expect("projection id"),
    )
}

/// The issue #752 shape at the dependent surface: composition binds the
/// deferred graph route while activation is still warming, both application
/// services attach immediately, graph reads stay typed-unavailable until
/// activation, and the same bound route recovers the moment activation
/// completes — no rebind, no 5-second refusal loop for the daemon lifetime.
#[tokio::test]
async fn work_surfaces_attach_and_recover_through_deferred_graph_activation() {
    tracedecay_global_db::register_test_schema_installer();
    let profile = tempfile::tempdir().expect("profile");
    let project = tempfile::tempdir().expect("project");
    let graph_root = tempfile::tempdir().expect("graph store root");
    let project_id = ProjectId::new("project.work-service-deferred").expect("project id");
    let identity = TestRuntimeProfileIdentityV1::new(
        BrainId::new("brain.work-service-deferred").expect("brain id"),
        UserProfileId::new("profile.work-service-deferred").expect("profile id"),
    );
    let runtime = RegisteredGlobalDbTestRuntime::project_for_profile_identity(
        profile.path(),
        project.path(),
        project_id.clone(),
        identity.clone(),
    )
    .await
    .expect("registered project store opens");
    let sessions = runtime.project_database().expect("project database");
    let graph_path = graph_root.path().join("memory.db");
    let graph_authority =
        DatabaseAuthority::acquire_test(&graph_path, "open deferred work-surface project graph")
            .expect("project graph database authority");
    let (graph_database, _) = Database::publish_registered_test_runtime_for_profile_identity(
        &graph_path,
        &graph_authority,
        TestDatabaseRuntimeMode::Initialize,
        identity,
        TestDatabaseRuntimeScope::Project { project_id },
    )
    .await
    .expect("publish project graph database");

    assert!(
        graph_database.memory_graph_runtime().is_none(),
        "graph activation must still be pending at bind time"
    );
    assert!(
        sessions
            .bind_project_graph_runtime(graph_database.deferred_memory_graph_runtime())
            .is_ok(),
        "deferred project graph route binds before activation"
    );

    let work = RegisteredWorkApplicationServicesV1::attach(sessions)
        .expect("Work topology attaches while activation is pending");
    RegisteredWorkflowApplicationServicesV1::attach(sessions)
        .expect("workflow topology attaches while activation is pending");
    let bound = sessions
        .project_graph_runtime()
        .expect("deferred route stays bound")
        .clone();
    assert!(
        matches!(
            bound.verified_snapshot(
                &probe_projection(),
                FactReadControl::new(Arc::new(|| false))
            ),
            Err(GraphDbError::Unavailable { .. })
        ),
        "graph reads must be typed-unavailable until activation completes"
    );

    let activated: Arc<dyn VerifiedGraphRuntimePortV1> = Arc::new(StubProjectGraphRuntime {
        binding: graph_database.registered_binding().clone(),
        locator: graph_database.registered_verified_locator().clone(),
    });
    graph_database
        .bind_memory_graph_runtime(Arc::clone(&activated))
        .expect("deferred graph activation completes");

    assert!(
        matches!(
            bound.verified_snapshot(
                &probe_projection(),
                FactReadControl::new(Arc::new(|| false))
            ),
            Ok(None)
        ),
        "the route bound before activation must resolve once activation completes"
    );
    drop(work);
    RegisteredWorkApplicationServicesV1::attach(sessions)
        .expect("Work topology re-attach stays available after activation");
}
