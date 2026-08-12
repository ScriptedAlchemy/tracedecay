//! Root-side shim for the dashboard HTTP surface.
//!
//! The dashboard API — routes, read models, services and their tests — lives
//! in `crates/tracedecay-dashboard-api`. Everything that used to be reachable
//! at `crate::dashboard::…` is re-exported here so existing root modules,
//! command adapters and integration tests keep compiling unchanged.
//!
//! [`assets`] retains only the root build-script bridge: the embedded
//! single-app dist is generated into this crate's `OUT_DIR`. The canonical API
//! crate owns the resulting HTTP router and transport policy.

pub use tracedecay_dashboard_api::*;

pub(crate) mod assets;

/// Canonical observation-capture seeding for dashboard integration fixtures.
#[cfg(any(test, feature = "test-transport"))]
#[doc(hidden)]
pub mod observation_seed;

/// Installs root-owned values consumed by the extracted dashboard crate.
pub(crate) fn register_runtime_ports() {
    tracedecay_dashboard_api::install_build_version(crate::version::build_version);
}

/// Embedded single-page-app routes shared by production and integration
/// servers. The root supplies build-script-owned bytes; `tracedecay-api`
/// owns route matching, cache policy, and the API fallback boundary.
#[doc(hidden)]
pub fn spa_router() -> axum::Router {
    assets::spa_router()
}

#[cfg(test)]
mod spa_router_tests {
    use axum::{
        body::Body,
        http::{Request, StatusCode},
    };
    use tower::ServiceExt;

    #[tokio::test]
    async fn unknown_api_paths_never_receive_the_single_page_app() {
        let response = super::spa_router()
            .oneshot(
                Request::builder()
                    .uri("/api/not-a-real-route")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("SPA router response");

        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }
}

/// Installs the canonical root-owned registered schema port before dashboard
/// integration fixtures open any database authority.
#[cfg(feature = "test-transport")]
#[doc(hidden)]
pub fn register_test_schema_installer() {
    static REGISTER: std::sync::Once = std::sync::Once::new();
    REGISTER.call_once(crate::daemon::store_runtime::register_registered_schema_installer);
}

/// Composes the production dashboard automation authority over one retained
/// integration-test graph. The returned writer is the same serialization
/// authority captured by managed-skill mutation/materialization and must be
/// mounted into dashboard state with the authority. Runs use canonical runner
/// locking directly, so a model turn never holds this broad writer.
#[cfg(feature = "test-transport")]
#[doc(hidden)]
pub fn dashboard_automation_authority_for_test(
    cg: std::sync::Arc<crate::tracedecay::TraceDecay>,
    profile_root: impl AsRef<std::path::Path>,
) -> crate::errors::Result<(DashboardAutomationAuthorityV1, DashboardAutomationWriter)> {
    let profile_root = profile_root.as_ref().canonicalize()?;
    let writer = standalone_dashboard_automation_writer();
    let resident_memory = std::sync::Arc::new(
        tracedecay_runtime_core::resident_memory::ProcessResidentMemoryV1::new(
            tracedecay_runtime_core::resident_memory::DEFAULT_PROCESS_RESIDENT_MEMORY_LIMIT_V1,
        ),
    );
    let authority = crate::daemon::dashboard_automation::compose_dashboard_automation_authority_for_test(
        profile_root,
        cg,
        std::sync::Arc::clone(&writer),
        crate::daemon::DaemonInvocationService::with_code_index_schedulers(
            crate::daemon::code_index_scheduler::CodeIndexSchedulerRegistryV1::with_resident_memory(
                1,
                resident_memory,
            ),
        ),
    )?;
    Ok((authority, writer))
}

/// Mounts the canonical daemon configuration mutation service for dashboard
/// integration tests that exercise HTTP writes and pinned-snapshot refresh.
#[cfg(feature = "test-transport")]
#[doc(hidden)]
pub async fn dashboard_configuration_application_runtime_for_test(
    cg: std::sync::Arc<crate::tracedecay::TraceDecay>,
) -> crate::errors::Result<std::sync::Arc<dyn DashboardApplicationRuntime>> {
    crate::daemon::dashboard_configuration_runtime_for_test(cg).await
}

/// Root-owned graph composition used by dashboard integration tests.
///
/// The dashboard API crate cannot own daemon session registration or graph
/// lifecycle. This opaque adapter keeps those authorities at the root while
/// exposing only graph initialization and reopening to the integration suite.
#[cfg(feature = "test-transport")]
#[doc(hidden)]
pub struct DashboardGraphTestRuntimeV1 {
    profile_root: std::path::PathBuf,
    profile_database: std::sync::Arc<crate::global_db::RegisteredGlobalDb>,
    registry: std::sync::Arc<
        crate::daemon::store_runtime::session_registry::DaemonSessionRuntimeRegistryV1,
    >,
    _database_scope: tracedecay_runtime_core::db::DaemonDatabaseScope,
}

#[cfg(feature = "test-transport")]
impl DashboardGraphTestRuntimeV1 {
    pub async fn open(profile_root: impl AsRef<std::path::Path>) -> crate::errors::Result<Self> {
        use std::sync::atomic::{AtomicU64, Ordering};

        static NEXT_ELECTION_EPOCH: AtomicU64 = AtomicU64::new(1);

        let profile_root = profile_root.as_ref().to_path_buf();
        let identity = crate::daemon::profile_identity::load_or_create(&profile_root)?;
        let epoch = NEXT_ELECTION_EPOCH.fetch_add(1, Ordering::Relaxed);
        let database_scope = tracedecay_runtime_core::db::enter_daemon_database_scope(
            identity.profile_root(),
            epoch,
            "dashboard-graph-test-runtime",
        )?;
        let registry = std::sync::Arc::new(
            crate::daemon::store_runtime::session_registry::DaemonSessionRuntimeRegistryV1::open(
                identity,
            )
            .await?,
        );
        let profile_database = registry.profile_database().await?;
        Ok(Self {
            profile_root,
            profile_database,
            registry,
            _database_scope: database_scope,
        })
    }

    pub fn profile_database(&self) -> std::sync::Arc<crate::global_db::RegisteredGlobalDb> {
        std::sync::Arc::clone(&self.profile_database)
    }

    pub async fn project_sessions(
        &self,
        project_root: &std::path::Path,
        project_id: tracedecay_domain::ProjectId,
    ) -> crate::errors::Result<std::sync::Arc<crate::global_db::RegisteredGlobalDb>> {
        let registered = self
            .registry
            .project_sessions(project_id.clone(), [project_root.to_path_buf()])
            .await?;
        // Production project open binds the retained project graph runtime to
        // the registered project-sessions authority before any ingest runs;
        // git-evidence publication (Loom spans) requires that mount, so the
        // dashboard test composition provides the same binding. The registry
        // caches the mount per project, so repeated opens reuse it.
        if registered.project_graph_runtime().is_none() {
            let project_database = self
                .registry
                .project_memory(project_id.clone(), [project_root.to_path_buf()])
                .await?;
            let graph_runtime = project_database.memory_graph_runtime().ok_or_else(|| {
                crate::errors::TraceDecayError::Database {
                    operation: "bind dashboard project graph".to_owned(),
                    message: "project memory database has no verified graph runtime".to_owned(),
                }
            })?;
            // A lost set race means another caller already bound the same
            // retained runtime; the required postcondition holds either way.
            let _ = registered.bind_project_graph_runtime(graph_runtime);
        }
        Ok(registered)
    }

    pub async fn initialize(
        &self,
        project_root: &std::path::Path,
        project_id: tracedecay_domain::ProjectId,
    ) -> crate::errors::Result<crate::tracedecay::TraceDecay> {
        crate::storage::write_enrollment_marker(
            project_root,
            &crate::storage::EnrollmentMarker {
                project_id: project_id.as_str().to_owned(),
                storage_mode: crate::storage::StorageMode::ProfileSharded,
            },
        )?;
        let options = crate::tracedecay::TraceDecayOpenOptions {
            profile_root: Some(self.profile_root.clone()),
            global_db_path: Some(self.profile_database.db_path().to_path_buf()),
        };
        let layout = crate::tracedecay::TraceDecay::resolve_registered_configuration_layout(
            project_root,
            &options,
            self.profile_database.as_ref(),
        )
        .await?;
        if layout.identity.project_id.as_deref() != Some(project_id.as_str()) {
            return Err(crate::errors::TraceDecayError::Config {
                message: "dashboard graph identity differs from its test authority".to_owned(),
            });
        }
        let project_database = self.project_sessions(project_root, project_id).await?;
        crate::tracedecay::TraceDecay::init_with_registered_configuration(
            project_root,
            options,
            layout,
            project_database,
            std::sync::Arc::clone(&self.profile_database),
            std::sync::Arc::clone(&self.registry),
        )
        .await
    }

    pub async fn reopen(
        &self,
        project_root: &std::path::Path,
    ) -> crate::errors::Result<crate::tracedecay::TraceDecay> {
        let options = crate::tracedecay::TraceDecayOpenOptions {
            profile_root: Some(self.profile_root.clone()),
            global_db_path: Some(self.profile_database.db_path().to_path_buf()),
        };
        let layout = crate::tracedecay::TraceDecay::resolve_registered_configuration_layout(
            project_root,
            &options,
            self.profile_database.as_ref(),
        )
        .await?;
        let project_id = layout
            .identity
            .project_id
            .as_deref()
            .ok_or_else(|| crate::errors::TraceDecayError::Config {
                message: "dashboard graph fixture has no project identity".to_owned(),
            })
            .and_then(|project_id| {
                tracedecay_domain::ProjectId::new(project_id.to_owned()).map_err(|error| {
                    crate::errors::TraceDecayError::Config {
                        message: format!("invalid dashboard graph fixture identity: {error}"),
                    }
                })
            })?;
        let project_database = self.project_sessions(project_root, project_id).await?;
        crate::tracedecay::TraceDecay::open_with_registered_configuration(
            project_root,
            options,
            layout,
            project_database,
            std::sync::Arc::clone(&self.profile_database),
            std::sync::Arc::clone(&self.registry),
        )
        .await
    }
}

/// Composes the daemon-owned LCM read authority over the fixture's
/// registered project-sessions store — the same `DashboardLcmReadAdapter`
/// over the daemon session retrieval service that the MCP dashboard
/// composition mounts in production. Without it every `hermes-lcm` and
/// explorer session read answers `lcm_daemon_authority_unavailable`.
#[cfg(feature = "test-transport")]
#[doc(hidden)]
pub async fn dashboard_lcm_read_authority_for_test(
    cg: &crate::tracedecay::TraceDecay,
    registry: &crate::global_db::RegisteredGlobalDb,
    project_database: std::sync::Arc<crate::global_db::RegisteredGlobalDb>,
) -> Option<std::sync::Arc<dyn DashboardLcmReadPortV1>> {
    let root =
        match crate::daemon::session_retrieval::DaemonSessionRetrievalRoot::project(cg, registry)
            .await
        {
            Some(root) => root,
            None => {
                crate::daemon::session_retrieval::DaemonSessionRetrievalRoot::project_for_test(cg)
            }
        };
    let identity = root.identity().clone();
    let service = crate::daemon::session_retrieval::DaemonSessionRetrievalService::new(
        std::sync::Arc::clone(&project_database),
        root,
        None,
    )?;
    let adapter = crate::mcp::tools::handlers::DashboardLcmReadAdapter::new(
        std::sync::Arc::new(service),
        identity,
    )?;
    Some(std::sync::Arc::new(adapter))
}

/// Composes the daemon-owned git-correlation read authority over the
/// fixture's registered project-sessions store — the same
/// `DashboardGitCorrelationReadAdapter` the MCP dashboard composition mounts
/// in production. Without it Loom's session↔commit and branch/worktree
/// sources answer their typed unavailable states.
#[cfg(feature = "test-transport")]
#[doc(hidden)]
pub fn dashboard_git_correlation_read_authority_for_test(
    project_database: std::sync::Arc<crate::global_db::RegisteredGlobalDb>,
) -> std::sync::Arc<dyn DashboardGitCorrelationReadPortV1> {
    std::sync::Arc::new(
        crate::mcp::tools::handlers::DashboardGitCorrelationReadAdapter::new(project_database),
    )
}

/// Records one git span through a registered ProjectSessions authority.
///
/// Root-owned bridge for the dashboard integration suite: span evidence is
/// published through the same graph-backed correlation store the dashboard
/// APIs read, which is crate-internal.
#[cfg(feature = "test-transport")]
#[doc(hidden)]
pub async fn record_project_span_for_test(
    project_database: &crate::global_db::RegisteredGlobalDb,
    observation: &tracedecay_sessions::runtime::git_correlation::SpanObservation,
    merge_gap_secs: i64,
) -> crate::errors::Result<i64> {
    crate::store::GlobalDbGitCorrelationStore::new(project_database)
        .record_span_observation(observation, merge_gap_secs)
        .await
        .map_err(|error| crate::errors::TraceDecayError::Database {
            operation: "record dashboard test git span".to_owned(),
            message: error.to_string(),
        })
}
