//! Test-only dashboard graph fixture. Compiled only under `test-transport`
//! because no production caller needs this runtime.

use std::sync::atomic::{AtomicU64, Ordering};

/// Root-owned graph composition used by dashboard integration tests.
///
/// The dashboard API crate cannot own daemon session registration or graph
/// lifecycle. This adapter keeps those authorities at the test composition
/// layer while exposing only graph initialization and reopening.
#[doc(hidden)]
pub struct DashboardGraphTestRuntimeV1 {
    profile_root: std::path::PathBuf,
    profile_database: tracedecay_global_db::RegisteredGlobalDbLeaseV1,
    profile_sessions_database: tracedecay_global_db::RegisteredGlobalDbLeaseV1,
    registry: std::sync::Arc<tracedecay_store_runtime::DaemonSessionRuntimeRegistryV1>,
    _database_scope: tracedecay_runtime_core::db::DaemonDatabaseScope,
}

impl DashboardGraphTestRuntimeV1 {
    #[hotpath::skip]
    pub async fn open(
        profile_root: impl AsRef<std::path::Path>,
    ) -> tracedecay_domain::errors::Result<Self> {
        // This fixture bypasses CLI and host-admission constructors, so it
        // must install the same root ports before graph init publishes Hook
        // bindings for the admitted project.
        crate::register_runtime_ports()?;

        static NEXT_ELECTION_EPOCH: AtomicU64 = AtomicU64::new(1);

        let profile_root = profile_root.as_ref().to_path_buf();
        let identity = tracedecay_daemon_identity::profile_identity::load_or_create(&profile_root)?;
        let epoch = NEXT_ELECTION_EPOCH.fetch_add(1, Ordering::Relaxed);
        let database_scope = tracedecay_runtime_core::db::enter_daemon_database_scope(
            identity.profile_root(),
            epoch,
            "dashboard-graph-test-runtime",
        )?;
        let registry = std::sync::Arc::new(
            hotpath::future!(
                tracedecay_store_runtime::DaemonSessionRuntimeRegistryV1::open(identity,),
                label = "dashboard.graph.registry"
            )
            .await?,
        );
        let profile_database = hotpath::future!(
            registry.profile_database(),
            label = "dashboard.graph.profile_database"
        )
        .await?;
        let profile_sessions_database = hotpath::future!(
            registry.profile_sessions(),
            label = "dashboard.graph.profile_sessions"
        )
        .await?;
        Ok(Self {
            profile_root,
            profile_database,
            profile_sessions_database,
            registry,
            _database_scope: database_scope,
        })
    }

    pub fn profile_database(&self) -> tracedecay_global_db::RegisteredGlobalDbLeaseV1 {
        self.profile_database.clone()
    }

    pub fn profile_sessions_database(&self) -> tracedecay_global_db::RegisteredGlobalDbLeaseV1 {
        self.profile_sessions_database.clone()
    }

    #[hotpath::skip]
    pub async fn project_sessions(
        &self,
        project_root: &std::path::Path,
        project_id: tracedecay_domain::ProjectId,
    ) -> tracedecay_domain::errors::Result<tracedecay_global_db::RegisteredGlobalDbLeaseV1> {
        let registered = hotpath::future!(
            self.registry
                .project_sessions(project_id.clone(), [project_root.to_path_buf()]),
            label = "dashboard.graph.project_sessions"
        )
        .await?;
        // Production project open binds a weak project graph proxy to the
        // registered project-sessions authority before any ingest runs;
        // git-evidence publication (Loom spans) requires that mount, so the
        // dashboard test composition provides the same binding. The registry
        // caches the mount per project, so repeated opens reuse the proxy.
        if registered.project_graph_runtime().is_none() {
            let project_database = hotpath::future!(
                self.registry
                    .project_memory(project_id.clone(), [project_root.to_path_buf()]),
                label = "dashboard.graph.project_memory"
            )
            .await?;
            let graph_proxy = crate::test_support::host_admission::await_bound_graph_runtime(
                &project_database,
                "bind dashboard project graph",
            )
            .await?;
            // A lost set race means another caller already bound the same
            // weak proxy; the required postcondition holds either way.
            let _ = registered.bind_project_graph_runtime(graph_proxy);
        }
        Ok(registered)
    }

    #[hotpath::skip]
    pub async fn initialize(
        &self,
        project_root: &std::path::Path,
        project_id: tracedecay_domain::ProjectId,
    ) -> tracedecay_domain::errors::Result<crate::tracedecay::TraceDecay> {
        // Fixture identity is pinned in the sanctioned `.git/` repository
        // identity marker; nothing is written into the working tree.
        tracedecay_runtime_core::storage::pin_fixture_repository_identity(
            project_root,
            project_id.as_str(),
        )?;
        let options = crate::tracedecay::TraceDecayOpenOptions {
            profile_root: Some(self.profile_root.clone()),
            global_db_path: Some(self.profile_database.db_path().to_path_buf()),
        };
        let layout = hotpath::future!(
            crate::tracedecay::TraceDecay::resolve_registered_configuration_layout(
                project_root,
                &options,
                self.profile_database.as_ref(),
            ),
            label = "dashboard.graph.layout"
        )
        .await?;
        if layout.identity.project_id.as_deref() != Some(project_id.as_str()) {
            return Err(tracedecay_domain::errors::TraceDecayError::Config {
                message: "dashboard graph identity differs from its test authority".to_owned(),
            });
        }
        let project_database = self.project_sessions(project_root, project_id).await?;
        hotpath::future!(
            crate::tracedecay::TraceDecay::init_with_registered_configuration(
                project_root,
                options,
                layout,
                project_database,
                self.profile_database.clone(),
                std::sync::Arc::clone(&self.registry),
            ),
            label = "dashboard.graph.init"
        )
        .await
    }

    #[hotpath::skip]
    pub async fn reopen(
        &self,
        project_root: &std::path::Path,
    ) -> tracedecay_domain::errors::Result<crate::tracedecay::TraceDecay> {
        let options = crate::tracedecay::TraceDecayOpenOptions {
            profile_root: Some(self.profile_root.clone()),
            global_db_path: Some(self.profile_database.db_path().to_path_buf()),
        };
        let layout = hotpath::future!(
            crate::tracedecay::TraceDecay::resolve_registered_configuration_layout(
                project_root,
                &options,
                self.profile_database.as_ref(),
            ),
            label = "dashboard.graph.reopen.layout"
        )
        .await?;
        let project_id = layout
            .identity
            .project_id
            .as_deref()
            .ok_or_else(|| tracedecay_domain::errors::TraceDecayError::Config {
                message: "dashboard graph fixture has no project identity".to_owned(),
            })
            .and_then(|project_id| {
                tracedecay_domain::ProjectId::new(project_id.to_owned()).map_err(|error| {
                    tracedecay_domain::errors::TraceDecayError::Config {
                        message: format!("invalid dashboard graph fixture identity: {error}"),
                    }
                })
            })?;
        let project_database = self.project_sessions(project_root, project_id).await?;
        hotpath::future!(
            crate::tracedecay::TraceDecay::open_with_registered_configuration(
                project_root,
                options,
                layout,
                project_database,
                self.profile_database.clone(),
                std::sync::Arc::clone(&self.registry),
            ),
            label = "dashboard.graph.reopen.open"
        )
        .await
    }
}
