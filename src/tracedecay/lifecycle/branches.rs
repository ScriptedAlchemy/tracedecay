//! Read-only branch/ref snapshot selection in the project store.

use std::path::Path;
use std::sync::Arc;

use crate::application::configuration::ProjectConfigurationRuntime;
use crate::branch;
use crate::config::{
    install_usecase_runtime_configuration_authority, materialize_root_runtime_configuration,
};
use crate::daemon::store_runtime::session_registry::DaemonSessionRuntimeRegistryV1;
use crate::db::{DatabaseAccessMode, DatabaseAuthority};
use crate::errors::{Result, TraceDecayError};
use crate::global_db::RegisteredGlobalDb;
use crate::storage::StoreLayout;
use tracedecay_code_extraction::LanguageRegistry;
use tracedecay_usecases::config::{
    install_configuration_daemon_client_for_project,
    open_runtime_configuration_for_registered_database_read_only,
};

use super::recovery::active_graph_layout;
use super::{TraceDecay, TraceDecayOpenOptions};

impl TraceDecay {
    /// Opens an exact branch snapshot from the project-wide graph store.
    pub async fn open_branch(project_root: &Path, branch_name: &str) -> Result<Self> {
        Self::open_branch_with_options(project_root, branch_name, TraceDecayOpenOptions::default())
            .await
    }

    pub async fn open_branch_with_options(
        project_root: &Path,
        branch_name: &str,
        open_options: TraceDecayOpenOptions,
    ) -> Result<Self> {
        #[cfg(any(test, feature = "test-transport"))]
        {
            let open_options = Self::standalone_test_open_options(project_root, open_options);
            let runtime = Self::standalone_test_runtime(project_root, &open_options).await?;
            let mut graph = runtime
                .open_project_branch_for_test(project_root, branch_name, open_options)
                .await?;
            graph.test_runtime_guard = Some(runtime);
            Ok(graph)
        }
        #[cfg(not(any(test, feature = "test-transport")))]
        {
            let maintenance =
                Self::standalone_maintenance_scope(&open_options, "direct branch snapshot open")?;
            let mut graph = Self::open_branch_with_exclusive_maintenance(
                project_root,
                branch_name,
                open_options,
                maintenance.lifecycle(),
            )
            .await?;
            graph.standalone_maintenance_scope = Some(maintenance);
            Ok(graph)
        }
    }

    /// Opens an exact branch snapshot while the caller holds the profile's
    /// exclusive maintenance lease.
    pub async fn open_branch_with_exclusive_maintenance(
        project_root: &Path,
        branch_name: &str,
        open_options: TraceDecayOpenOptions,
        lifecycle_lease: &crate::lifecycle_lease::LifecycleLease,
    ) -> Result<Self> {
        let profile_root = open_options.resolved_profile_root()?;
        if !lifecycle_lease.is_exclusive() || !lifecycle_lease.guards_profile(&profile_root) {
            return Err(TraceDecayError::Config {
                message:
                    "branch snapshot open requires the exact profile's exclusive lifecycle lease"
                        .to_owned(),
            });
        }
        let identity = crate::daemon::profile_identity::load_or_create(&profile_root)?;
        let runtime_registry = Arc::new(DaemonSessionRuntimeRegistryV1::open(identity).await?);
        let profile_database = runtime_registry.profile_database().await?;
        let store_layout = Self::resolve_registered_configuration_layout(
            project_root,
            &open_options,
            profile_database.as_ref(),
        )
        .await?;
        let project_id = Self::registered_project_id(&store_layout)?;
        let enrollment_roots = Self::registered_enrollment_roots(
            project_root,
            &store_layout,
            &project_id,
            profile_database.as_ref(),
        )
        .await?;
        let configuration_database = runtime_registry
            .project_sessions(project_id, enrollment_roots)
            .await?;
        Self::open_branch_with_registered_configuration(
            project_root,
            branch_name,
            open_options,
            store_layout,
            configuration_database,
            profile_database,
            runtime_registry,
        )
        .await
    }

    pub(crate) async fn open_branch_with_registered_configuration(
        project_root: &Path,
        branch_name: &str,
        open_options: TraceDecayOpenOptions,
        store_layout: StoreLayout,
        configuration_database: Arc<RegisteredGlobalDb>,
        profile_database: Arc<RegisteredGlobalDb>,
        runtime_registry: Arc<DaemonSessionRuntimeRegistryV1>,
    ) -> Result<Self> {
        if !branch::local_branch_exists(project_root, branch_name)
            && crate::worktree::detached_worktree_graph_scope(project_root).as_deref()
                != Some(branch_name)
        {
            return Err(TraceDecayError::Config {
                message: format!("branch ref '{branch_name}' is unavailable"),
            });
        }
        let db_path = store_layout.graph_db_path.clone();
        if !db_path.exists() {
            return Err(TraceDecayError::Config {
                message: "registered project store is unavailable".to_owned(),
            });
        }
        let db = Self::mount_worktree_graph(
            runtime_registry.as_ref(),
            project_root,
            &store_layout,
            &db_path,
            Some(branch_name),
            "open branch snapshot",
            DatabaseAccessMode::ReadOnly,
        )
        .await?;
        install_usecase_runtime_configuration_authority()?;
        let (configuration_runtime, configuration) = ProjectConfigurationRuntime::open(
            open_runtime_configuration_for_registered_database_read_only(
                project_root,
                &store_layout,
                configuration_database,
            )
            .await?,
        )?;
        let configuration_runtime = Arc::new(configuration_runtime);
        let config = materialize_root_runtime_configuration(&configuration)?;
        install_configuration_daemon_client_for_project(
            &configuration.target,
            configuration_runtime.client(),
        );
        let internal_detached_scope = crate::worktree::detached_worktree_graph_scope(project_root)
            .as_deref()
            == Some(branch_name);
        Ok(Self {
            db,
            profile_database,
            store_runtime_registry: runtime_registry,
            config,
            configuration_runtime,
            project_root: project_root.to_path_buf(),
            store_layout,
            active_graph_layout: active_graph_layout(&db_path),
            open_options,
            registry: LanguageRegistry::new(),
            active_branch: (!internal_detached_scope).then(|| branch_name.to_string()),
            read_only: true,
            context_scout_owner: None,
            context_scout_claim_authorities: tokio::sync::RwLock::default(),
            #[cfg(any(test, feature = "test-transport"))]
            test_runtime_guard: None,
            standalone_maintenance_scope: None,
        })
    }
}
