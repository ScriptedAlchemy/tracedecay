//! Runtime-specific choices for production project composition.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use tracedecay_global_db::RegisteredGlobalDb;
use tracedecay_runtime_core::cancellation::CancellationToken;

#[cfg(unix)]
use super::DaemonEngine;
#[cfg(any(not(unix), test, feature = "test-transport"))]
use super::portable_database_owner_reconciler;
use super::{DaemonHandshake, ProjectServerKey, StoreAdministration};

/// Binds the project database's graph runtime route to the registered
/// project sessions authority.
///
/// Composition regularly reaches this bind while deferred graph activation is
/// still warming, so the bound proxy is the deferred-activation route: it
/// resolves the runtime at use time, answering typed-unavailable until
/// activation publishes the runtime and recovering every dependent surface
/// (Work topology, workflow recovery, semantic evaluation) within that
/// activation. A missing runtime is therefore never reported as success, and
/// no rebind retry is required.
#[hotpath::measure(label = "daemon.project.compose.bind_graph", future = true)]
pub(super) async fn bind_verified_project_graph_runtime(
    database: &tracedecay_runtime_core::db::Database,
    sessions: &RegisteredGlobalDb,
) -> tracedecay_domain::errors::Result<()> {
    sessions
        .bind_project_graph_runtime(database.deferred_memory_graph_runtime())
        .map_err(|_| tracedecay_domain::errors::TraceDecayError::Config {
            message: "project graph runtime was already mounted for project sessions".to_owned(),
        })
}

#[derive(Clone)]
pub(in crate::daemon) enum ProductionProjectCompositionRuntime {
    #[cfg(unix)]
    Unix(Box<DaemonEngine>),
    #[cfg(any(not(unix), test, feature = "test-transport"))]
    Portable {
        semantic_auto_download: bool,
        startup_catch_up: bool,
    },
}

impl ProductionProjectCompositionRuntime {
    pub(super) fn database_owner_reconciler(
        &self,
        _store_administration: &StoreAdministration,
        current_key: Arc<tokio::sync::Mutex<ProjectServerKey>>,
        current_project_path: Arc<tokio::sync::Mutex<PathBuf>>,
        route_registered: Arc<AtomicBool>,
        route_cancellation: CancellationToken,
        handshake: DaemonHandshake,
    ) -> crate::mcp::DatabaseOwnerReconciler {
        match self {
            #[cfg(unix)]
            Self::Unix(engine) => engine.database_owner_reconciler(
                current_key,
                current_project_path,
                route_registered,
                route_cancellation,
                handshake,
            ),
            #[cfg(any(not(unix), test, feature = "test-transport"))]
            Self::Portable { .. } => portable_database_owner_reconciler(
                _store_administration.clone(),
                current_key,
                route_registered,
                route_cancellation,
                handshake,
            ),
        }
    }

    pub(super) fn automation_scheduler_reconciler(
        &self,
        current_key: Arc<tokio::sync::Mutex<ProjectServerKey>>,
        current_project_path: Arc<tokio::sync::Mutex<PathBuf>>,
        handshake: DaemonHandshake,
    ) -> Option<tracedecay_dashboard_api::AutomationSchedulerReconciler> {
        match self {
            #[cfg(unix)]
            Self::Unix(engine) => Some(engine.automation_scheduler_reconciler(
                current_key,
                current_project_path,
                handshake,
            )),
            #[cfg(any(not(unix), test, feature = "test-transport"))]
            Self::Portable { .. } => None,
        }
    }

    #[hotpath::skip]
    pub(super) const fn semantic_auto_download(&self) -> bool {
        match self {
            #[cfg(unix)]
            Self::Unix(_) => true,
            #[cfg(any(not(unix), test, feature = "test-transport"))]
            Self::Portable {
                semantic_auto_download,
                ..
            } => *semantic_auto_download,
        }
    }

    #[hotpath::skip]
    pub(super) const fn startup_catch_up(&self) -> bool {
        match self {
            #[cfg(unix)]
            Self::Unix(_) => true,
            #[cfg(any(not(unix), test, feature = "test-transport"))]
            Self::Portable {
                startup_catch_up, ..
            } => *startup_catch_up,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::AtomicBool;

    use tracedecay_domain::{BrainId, ProjectId, UserProfileId};
    use tracedecay_global_db::tests::harness::RegisteredGlobalDbTestRuntime;
    use tracedecay_graph_db::{
        GraphDbError, GraphGenerationManifest, GraphIdempotencyKey, GraphNamespace,
        GraphProjectionId, GraphProjectionIdentity, VerifiedGraphSnapshot,
    };
    use tracedecay_runtime_core::db::{
        Database, DatabaseAuthority, TestDatabaseRuntimeMode, TestDatabaseRuntimeScope,
        TestRuntimeProfileIdentityV1,
    };
    use tracedecay_runtime_core::store_runtime::VerifiedGraphRuntimePortV1;
    use tracedecay_store::{FactReadControl, StoreRuntimeBindingV1, VerifiedStoreLocatorV1};
    use tracedecay_usecases::work::{
        RegisteredWorkApplicationServicesV1, RegisteredWorkflowApplicationServicesV1,
    };

    use super::bind_verified_project_graph_runtime;

    struct DeferredActivationGraphRuntime {
        binding: StoreRuntimeBindingV1,
        locator: VerifiedStoreLocatorV1,
    }

    impl VerifiedGraphRuntimePortV1 for DeferredActivationGraphRuntime {
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
                "activation test runtime has no publication",
            ))
        }

        fn reconcile_verified_manifest(
            &self,
            _manifest: &GraphGenerationManifest,
            _idempotency_key: GraphIdempotencyKey,
        ) -> Result<VerifiedGraphSnapshot, GraphDbError> {
            Err(GraphDbError::unavailable(
                "activation test runtime has no reconciliation",
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

    struct DeferredBindFixture {
        sessions_runtime: RegisteredGlobalDbTestRuntime,
        graph_database: Database,
        _profile_root: tempfile::TempDir,
        _project_root: tempfile::TempDir,
        _graph_root: tempfile::TempDir,
    }

    /// The issue #752 composition shape: the project sessions store is open
    /// while the project graph store's deferred activation has not yet bound
    /// a memory graph runtime to the database.
    async fn deferred_bind_fixture(label: &str) -> DeferredBindFixture {
        let profile_root = tempfile::tempdir().expect("profile root");
        let project_root = tempfile::tempdir().expect("project root");
        let graph_root = tempfile::tempdir().expect("graph store root");
        let project_id =
            ProjectId::new(format!("project.deferred-graph-bind.{label}")).expect("project id");
        let identity = TestRuntimeProfileIdentityV1::new(
            BrainId::new(format!("brain.deferred-graph-bind.{label}")).expect("brain id"),
            UserProfileId::new(format!("profile.deferred-graph-bind.{label}")).expect("profile id"),
        );
        let sessions_runtime = RegisteredGlobalDbTestRuntime::project_for_profile_identity(
            profile_root.path(),
            project_root.path(),
            project_id.clone(),
            identity.clone(),
        )
        .await
        .expect("registered project sessions runtime");
        let graph_path = graph_root.path().join("memory.db");
        let graph_authority =
            DatabaseAuthority::acquire_test(&graph_path, "open deferred-bind project graph")
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
        DeferredBindFixture {
            sessions_runtime,
            graph_database,
            _profile_root: profile_root,
            _project_root: project_root,
            _graph_root: graph_root,
        }
    }

    fn activation_runtime(graph_database: &Database) -> Arc<dyn VerifiedGraphRuntimePortV1> {
        Arc::new(DeferredActivationGraphRuntime {
            binding: graph_database.registered_binding().clone(),
            locator: graph_database.registered_verified_locator().clone(),
        })
    }

    fn probe_projection(label: &str) -> GraphProjectionIdentity {
        GraphProjectionIdentity::new(
            GraphNamespace::new("project").expect("graph namespace"),
            GraphProjectionId::new(format!("projection.deferred-bind.{label}"))
                .expect("projection id"),
        )
    }

    /// Issue #752: composition reaches the project-graph bind while deferred
    /// graph activation is still warming, activation completes afterwards,
    /// and the dependent Work and workflow surfaces must recover within that
    /// activation instead of refusing "project graph runtime is not bound"
    /// for the daemon's lifetime.
    #[tokio::test]
    async fn work_surfaces_recover_when_graph_activation_completes_after_composition_bind() {
        let fixture = deferred_bind_fixture("recovers").await;
        let sessions = fixture
            .sessions_runtime
            .project_database()
            .expect("project sessions database");

        assert!(
            fixture.graph_database.memory_graph_runtime().is_none(),
            "fixture must reach composition before graph activation"
        );
        bind_verified_project_graph_runtime(&fixture.graph_database, sessions)
            .await
            .expect("composition binds the deferred project graph runtime");

        let runtime = activation_runtime(&fixture.graph_database);
        fixture
            .graph_database
            .bind_memory_graph_runtime(Arc::clone(&runtime))
            .expect("deferred graph activation completes");

        RegisteredWorkApplicationServicesV1::attach(sessions)
            .expect("Work topology attaches once deferred activation completes");
        RegisteredWorkflowApplicationServicesV1::attach(sessions)
            .expect("workflow topology attaches once deferred activation completes");
        let bound = sessions
            .project_graph_runtime()
            .expect("composition bound the project graph proxy");
        assert!(
            matches!(
                bound.verified_snapshot(
                    &probe_projection("recovers"),
                    FactReadControl::new(Arc::new(|| false)),
                ),
                Ok(None)
            ),
            "bound proxy must resolve the activated graph runtime"
        );
    }

    /// When graph activation never completes in-process, the composition
    /// bind must stay a typed refusal at every dependent graph read — never
    /// a panic, a silent success, or an untyped empty result.
    #[tokio::test]
    async fn never_activated_graph_runtime_stays_typed_on_dependent_surfaces() {
        let fixture = deferred_bind_fixture("never-activates").await;
        let sessions = fixture
            .sessions_runtime
            .project_database()
            .expect("project sessions database");

        bind_verified_project_graph_runtime(&fixture.graph_database, sessions)
            .await
            .expect("composition binds the deferred project graph runtime");

        let services = RegisteredWorkApplicationServicesV1::attach(sessions)
            .expect("Work topology attaches while activation is pending");
        drop(services);
        let bound = sessions
            .project_graph_runtime()
            .expect("composition bound the project graph proxy");
        assert!(
            matches!(
                bound.verified_snapshot(
                    &probe_projection("never-activates"),
                    FactReadControl::new(Arc::new(|| false)),
                ),
                Err(GraphDbError::Unavailable { .. })
            ),
            "graph reads must stay typed while activation never completes"
        );
    }
}
