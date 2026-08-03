//! Branch tracking: auto-tracking the active branch on a registered open,
//! adding/syncing new tracked branches, resolving which DB file serves a
//! branch, and opening a specific tracked branch's snapshot.

use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};

use crate::application::configuration::ProjectConfigurationRuntime;
use crate::branch;
use crate::branch_meta;
use crate::config::{
    db_filename, install_usecase_runtime_configuration_authority,
    materialize_root_runtime_configuration,
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
    /// Mirrors automatic branch tracking for a daemon-owned project open.
    ///
    /// The ordinary branch helper reopens through the public standalone API,
    /// which intentionally has no configuration authority. A registered open
    /// must retain its exact registered project session while preparing and
    /// syncing the branch instead.
    pub(super) async fn auto_track_active_branch_with_registered_configuration(
        project_root: &Path,
        tracedecay_dir: &Path,
        active_branch: Option<&str>,
        open_options: TraceDecayOpenOptions,
        store_layout: &StoreLayout,
        configuration_database: &Arc<RegisteredGlobalDb>,
        profile_database: &Arc<RegisteredGlobalDb>,
        runtime_registry: &Arc<DaemonSessionRuntimeRegistryV1>,
    ) -> Result<()> {
        let Some(branch_name) = active_branch else {
            return Ok(());
        };
        let prepared =
            branch::prepare_branch_tracking_in_layout(project_root, branch_name, tracedecay_dir)
                .await?;
        let branch::BranchTrackingPreparation::Added(prepared) = prepared else {
            return Ok(());
        };
        let sync_result = Self::sync_new_branch_with_registered_configuration(
            project_root,
            branch_name,
            open_options,
            store_layout.clone(),
            Arc::clone(configuration_database),
            Arc::clone(profile_database),
            Arc::clone(runtime_registry),
        )
        .await;
        if let Err(TraceDecayError::SyncLock { .. }) = sync_result {
            return Ok(());
        } else if let Err(error) = sync_result {
            return match branch::rollback_prepared_branch_tracking(tracedecay_dir, &prepared) {
                Ok(()) => Err(error),
                Err(rollback_error) => Err(TraceDecayError::Config {
                    message: format!(
                        "branch sync failed: {error}; published branch rollback also failed: {rollback_error}"
                    ),
                }),
            };
        }
        branch::finalize_prepared_branch_tracking(tracedecay_dir, &prepared);
        Ok(())
    }

    pub(crate) async fn track_worktree_branch(
        &self,
        worktree_root: &Path,
        branch_name: &str,
    ) -> Result<branch::BranchAddOutcome> {
        let prepared = branch::prepare_branch_tracking_from_database(
            worktree_root,
            branch_name,
            &self.store_layout.data_root,
            &self.db,
        )
        .await?;
        let branch::BranchTrackingPreparation::Added(prepared) = prepared else {
            return Ok(match prepared {
                branch::BranchTrackingPreparation::AlreadyTracked => {
                    branch::BranchAddOutcome::AlreadyTracked
                }
                branch::BranchTrackingPreparation::Deferred => branch::BranchAddOutcome::Deferred,
                branch::BranchTrackingPreparation::Added(_) => unreachable!(),
            });
        };

        let sync_result = self
            .sync_retained_worktree_branch(worktree_root, branch_name, prepared.database_path())
            .await;
        if let Err(TraceDecayError::SyncLock { .. }) = sync_result {
            return Ok(branch::BranchAddOutcome::Deferred);
        } else if let Err(error) = sync_result {
            return match branch::rollback_prepared_branch_tracking(
                &self.store_layout.data_root,
                &prepared,
            ) {
                Ok(()) => Err(error),
                Err(rollback_error) => Err(TraceDecayError::Config {
                    message: format!(
                        "branch sync failed: {error}; published branch rollback also failed: \
                         {rollback_error}"
                    ),
                }),
            };
        }

        branch::finalize_prepared_branch_tracking(&self.store_layout.data_root, &prepared);
        Ok(branch::BranchAddOutcome::Added)
    }

    pub(crate) async fn sync_retained_worktree_branch(
        &self,
        worktree_root: &Path,
        branch_name: &str,
        database_path: &Path,
    ) -> Result<Self> {
        let db = self
            .store_runtime_registry
            .code_graph_branch_registered(
                worktree_root,
                Self::registered_project_id(&self.store_layout)?,
                branch_name,
                database_path.to_path_buf(),
                DatabaseAccessMode::ReadWrite,
            )
            .await?;
        let branch_graph = Self {
            db,
            profile_database: Arc::clone(&self.profile_database),
            store_runtime_registry: Arc::clone(&self.store_runtime_registry),
            config: self.config.clone(),
            configuration_runtime: Arc::clone(&self.configuration_runtime),
            project_root: worktree_root
                .canonicalize()
                .unwrap_or_else(|_| worktree_root.to_path_buf()),
            store_layout: self.store_layout.clone(),
            active_graph_layout: active_graph_layout(database_path),
            open_options: self.open_options.clone(),
            registry: LanguageRegistry::new(),
            active_branch: Some(branch_name.to_owned()),
            serving_branch: Some(branch_name.to_owned()),
            fallback_warning: None,
            read_only: false,
            db_path_cache: OnceLock::new(),
            context_scout_owner: None,
            context_scout_claim_authorities: tokio::sync::RwLock::default(),
            #[cfg(any(test, feature = "test-transport"))]
            test_runtime_guard: self.test_runtime_guard.clone(),
            standalone_maintenance_scope: self.standalone_maintenance_scope.clone(),
        };

        let mut attempts = 0;
        let sync_error = loop {
            match branch_graph.sync_checkpointed().await {
                Ok(_) => {
                    branch_graph
                        .register_project_store_in_global_registry()
                        .await?;
                    return Ok(branch_graph);
                }
                Err(TraceDecayError::SyncLock { .. }) if attempts < 20 => {
                    attempts += 1;
                    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                }
                Err(error) => break error,
            }
        };
        drop(branch_graph);
        Err(Self::retire_branch_runtime_after_failed_sync(
            &self.store_runtime_registry,
            database_path,
            sync_error,
        )
        .await)
    }

    /// Silently bootstraps/maintains tracedecay branch tracking for `branch_name`.
    ///
    /// This is the library-level core shared with the `tracedecay branch add`
    /// CLI command and hook integrations. It loads or bootstraps branch
    /// metadata, no-ops when the branch is already tracked, otherwise copies
    /// the nearest tracked ancestor's DB and runs an incremental sync against
    /// the new branch DB.
    pub async fn add_branch_tracking(
        project_root: &Path,
        branch_name: &str,
    ) -> Result<branch::BranchAddOutcome> {
        Self::add_branch_tracking_with_options(
            project_root,
            branch_name,
            TraceDecayOpenOptions::default(),
        )
        .await
    }

    pub async fn add_branch_tracking_with_options(
        project_root: &Path,
        branch_name: &str,
        open_options: TraceDecayOpenOptions,
    ) -> Result<branch::BranchAddOutcome> {
        let store_layout = match Self::resolve_store_layout_for_project(project_root, &open_options)
            .await
        {
            Ok(layout) => layout,
            Err(TraceDecayError::Config { .. }) => return Ok(branch::BranchAddOutcome::NotIndexed),
            Err(err) => return Err(err),
        };

        if !store_layout.graph_db_path.is_file() {
            return Ok(branch::BranchAddOutcome::NotIndexed);
        }

        // Branch preparation copies a live SQLite store and rewrites metadata;
        // reject non-daemon callers before either filesystem mutation occurs.
        let _authority =
            DatabaseAuthority::for_runtime(&store_layout.graph_db_path, "add branch tracking")?;
        Self::add_branch_tracking_in_layout(
            project_root,
            branch_name,
            &store_layout.data_root,
            open_options,
        )
        .await
    }

    async fn add_branch_tracking_in_layout(
        project_root: &Path,
        branch_name: &str,
        tracedecay_dir: &Path,
        open_options: TraceDecayOpenOptions,
    ) -> Result<branch::BranchAddOutcome> {
        let prepared =
            branch::prepare_branch_tracking_in_layout(project_root, branch_name, tracedecay_dir)
                .await?;
        let branch::BranchTrackingPreparation::Added(prepared) = prepared else {
            return Ok(match prepared {
                branch::BranchTrackingPreparation::AlreadyTracked => {
                    branch::BranchAddOutcome::AlreadyTracked
                }
                branch::BranchTrackingPreparation::Deferred => branch::BranchAddOutcome::Deferred,
                branch::BranchTrackingPreparation::Added(_) => unreachable!(),
            });
        };

        let sync_result = Self::sync_new_branch_with_retries(
            project_root,
            branch_name,
            tracedecay_dir,
            open_options,
        )
        .await;
        if let Err(TraceDecayError::SyncLock { .. }) = sync_result {
            return Ok(branch::BranchAddOutcome::Deferred);
        } else if let Err(e) = sync_result {
            return match branch::rollback_prepared_branch_tracking(tracedecay_dir, &prepared) {
                Ok(()) => Err(e),
                Err(rollback_error) => Err(TraceDecayError::Config {
                    message: format!(
                        "branch sync failed: {e}; published branch rollback also failed: {rollback_error}"
                    ),
                }),
            };
        }

        branch::finalize_prepared_branch_tracking(tracedecay_dir, &prepared);
        Ok(branch::BranchAddOutcome::Added)
    }

    async fn sync_new_branch_with_retries(
        project_root: &Path,
        branch_name: &str,
        expected_data_root: &Path,
        open_options: TraceDecayOpenOptions,
    ) -> Result<()> {
        #[cfg(any(test, feature = "test-transport"))]
        let _test_runtime = Self::standalone_test_runtime(project_root, &open_options).await?;

        let profile_root = open_options.resolved_profile_root()?;
        let identity = crate::daemon::profile_identity::load_or_create(&profile_root)?;
        let runtime_registry = Arc::new(DaemonSessionRuntimeRegistryV1::open(identity).await?);
        let profile_database = runtime_registry.profile_database().await?;
        let store_layout = Self::resolve_registered_configuration_layout(
            project_root,
            &open_options,
            profile_database.as_ref(),
        )
        .await?;
        if store_layout.data_root != expected_data_root {
            return Err(TraceDecayError::Config {
                message: "branch sync resolved a different registered project store".to_owned(),
            });
        }
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
        Self::sync_new_branch_with_registered_configuration(
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

    async fn sync_new_branch_with_registered_configuration(
        project_root: &Path,
        branch_name: &str,
        open_options: TraceDecayOpenOptions,
        store_layout: StoreLayout,
        configuration_database: Arc<RegisteredGlobalDb>,
        profile_database: Arc<RegisteredGlobalDb>,
        runtime_registry: Arc<DaemonSessionRuntimeRegistryV1>,
    ) -> Result<()> {
        let mut attempts = 0;
        loop {
            let graph = Self::open_branch_with_registered_configuration_access(
                project_root,
                branch_name,
                open_options.clone(),
                store_layout.clone(),
                Arc::clone(&configuration_database),
                Arc::clone(&profile_database),
                Arc::clone(&runtime_registry),
                DatabaseAccessMode::ReadWrite,
                "sync newly tracked branch",
                false,
            )
            .await?;
            let branch_database_path = graph.db_path();
            let sync_result = graph.sync().await;
            drop(graph);
            match sync_result {
                Ok(_) => return Ok(()),
                Err(TraceDecayError::SyncLock { .. }) if attempts < 20 => {
                    attempts += 1;
                    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                }
                Err(error) => {
                    return Err(Self::retire_branch_runtime_after_failed_sync(
                        &runtime_registry,
                        &branch_database_path,
                        error,
                    )
                    .await);
                }
            }
        }
    }

    /// Retires the registered runtime for a branch store whose sync failed, so
    /// the caller can roll the published branch back in this same process.
    ///
    /// A branch sync mounts the new branch database through the process-wide
    /// store runtime registry, and the registry keeps that mount — and the
    /// database authority lease behind it — alive after the failed
    /// [`TraceDecay`] handle is dropped. Rollback quarantines the same
    /// `SQLite` family behind a deletion fence, and a deletion fence refuses
    /// any database this process still holds an authority for. Without this
    /// retirement the failure handler reported "this process already holds an
    /// incompatible database authority or deletion fence" and left the failed
    /// branch published until some other process cleaned it up.
    ///
    /// Retirement failures are folded into the returned error: the caller must
    /// not attempt a rollback that is still fenced.
    async fn retire_branch_runtime_after_failed_sync(
        runtime_registry: &DaemonSessionRuntimeRegistryV1,
        branch_database_path: &Path,
        sync_error: TraceDecayError,
    ) -> TraceDecayError {
        match runtime_registry
            .close_code_graph_paths([branch_database_path.to_path_buf()])
            .await
        {
            Ok(()) => sync_error,
            Err(close_error) => TraceDecayError::Config {
                message: format!(
                    "branch sync failed: {sync_error}; the published branch runtime could not be \
                     retired before rollback: {close_error}"
                ),
            },
        }
    }

    /// Resolves which DB file to open for a given branch.
    ///
    /// Returns `(db_path, serving_branch, fallback_warning)`.
    /// `serving_branch` is the branch whose DB is actually opened.
    /// The warning is `Some` when falling back to an ancestor branch's DB.
    pub(crate) fn resolve_db_for_branch(
        project_root: &Path,
        tracedecay_dir: &Path,
        branch: Option<&str>,
    ) -> (PathBuf, Option<String>, Option<String>) {
        let default_db = tracedecay_dir.join(db_filename(tracedecay_dir));

        let Some(meta) = branch_meta::load_branch_meta(tracedecay_dir) else {
            // No branch metadata — single-DB mode (backward compat)
            return (default_db, None, None);
        };

        let Some(branch) = branch else {
            // Detached HEAD — use default branch DB
            return (
                default_db,
                Some(meta.default_branch.clone()),
                Some("detached HEAD — using default branch index".to_string()),
            );
        };

        // Exact match: branch is tracked
        if let Some(path) = branch::resolve_branch_db_path(tracedecay_dir, branch, &meta)
            && path.exists()
        {
            return (path, Some(branch.to_string()), None);
        }

        // Fallback: find nearest tracked ancestor
        if let Some(ancestor) = branch::find_nearest_tracked_ancestor(project_root, branch, &meta)
            && let Some(path) = branch::resolve_branch_db_path(tracedecay_dir, &ancestor, &meta)
            && path.exists()
        {
            return (
                path,
                Some(ancestor.clone()),
                Some(format!(
                    "branch '{branch}' is not tracked — serving from '{ancestor}'. \
                             Run `tracedecay branch add {branch}` to track it."
                )),
            );
        }

        // Last resort: default branch DB
        let serving = meta.default_branch.clone();
        (
            default_db,
            Some(serving),
            Some(format!(
                "branch '{branch}' is not tracked — serving from '{}'. \
                 Run `tracedecay branch add {branch}` to track it.",
                meta.default_branch
            )),
        )
    }

    /// Opens a specific branch's DB.
    ///
    /// Returns an error if the branch is not tracked or the DB doesn't exist.
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
                Self::standalone_maintenance_scope(&open_options, "direct branch open")?;
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

    /// Opens a tracked branch through the canonical registered runtime while
    /// the caller holds the exact profile's exclusive maintenance lease.
    pub async fn open_branch_with_exclusive_maintenance(
        project_root: &Path,
        branch_name: &str,
        open_options: TraceDecayOpenOptions,
        lifecycle_lease: &crate::lifecycle_lease::LifecycleLease,
    ) -> Result<Self> {
        let profile_root = open_options.resolved_profile_root()?;
        if !lifecycle_lease.is_exclusive() || !lifecycle_lease.guards_profile(&profile_root) {
            return Err(TraceDecayError::Config {
                message: "branch open requires the exact profile's exclusive lifecycle lease"
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
        Self::open_branch_with_registered_configuration_access(
            project_root,
            branch_name,
            open_options,
            store_layout,
            configuration_database,
            profile_database,
            runtime_registry,
            DatabaseAccessMode::ReadOnly,
            "open branch snapshot",
            true,
        )
        .await
    }

    async fn open_branch_with_registered_configuration_access(
        project_root: &Path,
        branch_name: &str,
        open_options: TraceDecayOpenOptions,
        store_layout: StoreLayout,
        configuration_database: Arc<RegisteredGlobalDb>,
        profile_database: Arc<RegisteredGlobalDb>,
        runtime_registry: Arc<DaemonSessionRuntimeRegistryV1>,
        access_mode: DatabaseAccessMode,
        operation: &'static str,
        read_only: bool,
    ) -> Result<Self> {
        let meta = branch_meta::load_branch_meta(&store_layout.data_root).ok_or_else(|| {
            TraceDecayError::Config {
                message: "no branch tracking configured — run `tracedecay branch add` first"
                    .to_string(),
            }
        })?;

        let db_path = branch::resolve_branch_db_path(&store_layout.data_root, branch_name, &meta)
            .ok_or_else(|| TraceDecayError::Config {
            message: format!("branch '{branch_name}' is not tracked"),
        })?;
        let active_graph_layout = active_graph_layout(&db_path);

        if !db_path.exists() {
            return Err(TraceDecayError::Config {
                message: format!(
                    "DB for branch '{branch_name}' not found at '{}'",
                    db_path.display()
                ),
            });
        }

        let db = Self::mount_worktree_graph(
            runtime_registry.as_ref(),
            project_root,
            &store_layout,
            &db_path,
            Some(branch_name),
            operation,
            access_mode,
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
            active_graph_layout,
            open_options,
            registry: LanguageRegistry::new(),
            active_branch: (!internal_detached_scope).then(|| branch_name.to_string()),
            serving_branch: (!internal_detached_scope).then(|| branch_name.to_string()),
            fallback_warning: None,
            read_only,
            db_path_cache: OnceLock::new(),
            context_scout_owner: None,
            context_scout_claim_authorities: tokio::sync::RwLock::default(),
            #[cfg(any(test, feature = "test-transport"))]
            test_runtime_guard: None,
            standalone_maintenance_scope: None,
        })
    }
}
