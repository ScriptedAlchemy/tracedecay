//! Lifecycle: init/open/branch-tracking entry points plus the profile-store
//! registration helpers they rely on.

#[cfg(not(any(test, feature = "test-transport")))]
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
#[cfg(not(any(test, feature = "test-transport")))]
use std::sync::{LazyLock, Mutex, Weak};

use crate::application::configuration::ProjectConfigurationRuntime;
use crate::branch;
use crate::branch_meta::{self, BranchMeta};
use crate::config::{
    db_filename, install_configuration_daemon_client_for_project,
    open_runtime_configuration_for_registered_database,
    open_runtime_configuration_for_registered_database_read_only,
};
use crate::daemon::store_runtime::session_registry::DaemonSessionRuntimeRegistryV1;
use crate::db::{Database, DatabaseAccessMode, DatabaseAuthority};
use crate::errors::{Result, TraceDecayError};
use crate::global_db::RegisteredGlobalDb;
use crate::storage::{self, StoreLayout};
use tracedecay_code_extraction::LanguageRegistry;
use tracedecay_store::ProjectId;

use super::{TraceDecay, TraceDecayOpenOptions};

mod identity;
mod recovery;
mod registry;

use recovery::{OpenHealthOutcome, active_graph_layout};

pub(crate) use recovery::is_fts_only_corruption;
pub(crate) use registry::git_remote_url;

#[cfg(not(any(test, feature = "test-transport")))]
static STANDALONE_MAINTENANCE_SCOPES: LazyLock<
    Mutex<HashMap<PathBuf, Weak<crate::db::OwnedMaintenanceDatabaseScope>>>,
> = LazyLock::new(|| Mutex::new(HashMap::new()));

impl TraceDecay {
    #[cfg(not(any(test, feature = "test-transport")))]
    fn standalone_maintenance_scope(
        open_options: &TraceDecayOpenOptions,
        operation: &'static str,
    ) -> Result<Arc<crate::db::OwnedMaintenanceDatabaseScope>> {
        let profile_root = open_options.resolved_profile_root()?;
        let mut scopes = STANDALONE_MAINTENANCE_SCOPES
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        scopes.retain(|_, scope| scope.strong_count() > 0);
        let profile_key = crate::lifecycle_lease::canonical_or_original(&profile_root);
        if let Some(scope) = scopes.get(&profile_key).and_then(Weak::upgrade) {
            return Ok(scope);
        }
        let lifecycle =
            crate::lifecycle_lease::acquire_exclusive_for_profile(&profile_root, operation)?;
        let scope = Arc::new(crate::db::enter_owned_maintenance_database_scope(
            lifecycle,
            &profile_root,
            operation,
        )?);
        let profile_key = crate::lifecycle_lease::canonical_or_original(&profile_root);
        scopes.insert(profile_key, Arc::downgrade(&scope));
        Ok(scope)
    }

    #[cfg(any(test, feature = "test-transport"))]
    fn standalone_test_open_options(
        project_root: &Path,
        mut open_options: TraceDecayOpenOptions,
    ) -> TraceDecayOpenOptions {
        if open_options.profile_root.is_none() && open_options.global_db_path.is_none() {
            let project_id = storage::default_profile_project_id(project_root);
            let parent = project_root.parent().unwrap_or(project_root);
            open_options.profile_root =
                Some(parent.join(format!(".tracedecay-test-profile-{project_id}")));
        }
        open_options
    }

    #[cfg(any(test, feature = "test-transport"))]
    async fn standalone_test_runtime(
        project_root: &Path,
        open_options: &TraceDecayOpenOptions,
    ) -> Result<Arc<crate::application::host_admission::HostAdmissionTestRuntimeV1>> {
        let profile_root = open_options.resolved_profile_root()?;
        if !crate::db::is_isolated_test_path(project_root)
            || !crate::db::is_isolated_test_path(&profile_root)
        {
            return Err(configuration_runtime_unavailable());
        }
        let project_id = storage::read_enrollment_marker(project_root)?.map_or_else(
            || storage::default_profile_project_id(project_root),
            |marker| marker.project_id,
        );
        let project_id = ProjectId::new(project_id).map_err(|error| TraceDecayError::Config {
            message: format!("invalid standalone test project identity: {error}"),
        })?;
        crate::application::host_admission::HostAdmissionTestRuntimeV1::project(
            profile_root,
            project_root,
            project_id,
        )
        .await
        .map(Arc::new)
    }

    async fn mount_worktree_graph(
        runtime_registry: &DaemonSessionRuntimeRegistryV1,
        project_root: &Path,
        store_layout: &StoreLayout,
        db_path: &Path,
        branch_name: Option<&str>,
        operation: &'static str,
        access: DatabaseAccessMode,
    ) -> Result<Database> {
        let project_id = Self::registered_project_id(store_layout)?;
        if let Some(branch_name) = branch_name {
            if matches!(access, DatabaseAccessMode::ReadOnly) {
                return runtime_registry
                    .code_graph_branch_registered(
                        project_root,
                        project_id,
                        branch_name,
                        db_path.to_path_buf(),
                        access,
                    )
                    .await;
            }
            let authority = DatabaseAuthority::for_runtime(db_path, operation)?;
            runtime_registry
                .code_graph_branch(
                    project_root,
                    project_id,
                    branch_name,
                    db_path.to_path_buf(),
                    authority,
                    access,
                )
                .await
        } else {
            let authority = DatabaseAuthority::for_runtime(db_path, operation)?;
            runtime_registry
                .code_graph_worktree(
                    project_root,
                    project_id,
                    db_path.to_path_buf(),
                    authority,
                    access,
                )
                .await
        }
    }

    /// Initializes a new `TraceDecay` project at the given root.
    ///
    /// Initializes the graph and its durable configuration revision. It never
    /// creates or rewrites legacy `config.json`.
    pub async fn init(project_root: &Path) -> Result<Self> {
        Self::init_with_options(project_root, TraceDecayOpenOptions::default()).await
    }

    pub async fn init_with_options(
        project_root: &Path,
        open_options: TraceDecayOpenOptions,
    ) -> Result<Self> {
        #[cfg(any(test, feature = "test-transport"))]
        {
            let open_options = Self::standalone_test_open_options(project_root, open_options);
            let runtime = Self::standalone_test_runtime(project_root, &open_options).await?;
            let mut graph = runtime
                .initialize_project_graph_for_test(project_root, open_options)
                .await?;
            graph.test_runtime_guard = Some(runtime);
            Ok(graph)
        }
        #[cfg(not(any(test, feature = "test-transport")))]
        {
            let maintenance =
                Self::standalone_maintenance_scope(&open_options, "direct project initialization")?;
            let mut graph = Self::init_with_exclusive_maintenance(
                project_root,
                open_options,
                maintenance.lifecycle(),
            )
            .await?;
            graph.standalone_maintenance_scope = Some(maintenance);
            Ok(graph)
        }
    }

    /// Initializes a first-touch project while the caller holds the exact
    /// profile's exclusive lifecycle lease and maintenance database scope.
    ///
    /// This is the daemonless bootstrap path used by `tracedecay init`. It
    /// still mounts configuration and session storage through the canonical
    /// registered runtime; the lease only replaces daemon ownership during
    /// this bounded maintenance operation.
    pub async fn init_with_exclusive_maintenance(
        project_root: &Path,
        open_options: TraceDecayOpenOptions,
        lifecycle_lease: &crate::lifecycle_lease::LifecycleLease,
    ) -> Result<Self> {
        let profile_root = open_options.resolved_profile_root()?;
        if let Some(message) =
            crate::project_registry::ephemeral_root_rejection(project_root, &profile_root)
        {
            return Err(TraceDecayError::Config { message });
        }
        if !lifecycle_lease.is_exclusive() || !lifecycle_lease.guards_profile(&profile_root) {
            return Err(TraceDecayError::Config {
                message:
                    "project initialization requires the exact profile's exclusive lifecycle lease"
                        .to_owned(),
            });
        }
        let identity = crate::daemon::profile_identity::load_or_create(&profile_root)?;
        let runtime_registry = Arc::new(DaemonSessionRuntimeRegistryV1::open(identity).await?);
        let profile_database = runtime_registry.profile_database().await?;
        let store_layout = Self::resolve_first_touch_configuration_layout(
            project_root,
            &open_options,
            profile_database.as_ref(),
            true,
        )
        .await?;
        let project_id = Self::registered_project_id(&store_layout)?;
        crate::storage::write_enrollment_marker(
            project_root,
            &crate::storage::EnrollmentMarker {
                project_id: project_id.as_str().to_owned(),
                storage_mode: crate::storage::StorageMode::ProfileSharded,
            },
        )?;
        let configuration_database = runtime_registry
            .project_sessions(project_id, [project_root.to_path_buf()])
            .await?;
        Self::init_with_registered_configuration(
            project_root,
            open_options,
            store_layout,
            configuration_database,
            profile_database,
            runtime_registry,
        )
        .await
    }

    #[cfg(test)]
    pub(crate) async fn init_test_fixture_with_registered_runtime(
        project_root: &Path,
        project_id: &str,
    ) -> Result<(
        Self,
        Arc<crate::application::host_admission::HostAdmissionTestRuntimeV1>,
    )> {
        let profile_root = crate::storage::default_profile_root()?;
        let project_id = tracedecay_domain::ProjectId::new(project_id).map_err(|error| {
            TraceDecayError::Config {
                message: format!("invalid test fixture project identity: {error}"),
            }
        })?;
        let runtime = Arc::new(
            crate::application::host_admission::HostAdmissionTestRuntimeV1::project(
                &profile_root,
                project_root,
                project_id,
            )
            .await?,
        );
        let graph = runtime
            .initialize_project_graph_for_test(
                project_root,
                TraceDecayOpenOptions {
                    profile_root: Some(profile_root),
                    global_db_path: None,
                },
            )
            .await?;
        Ok((graph, runtime))
    }

    pub(crate) async fn init_with_registered_configuration(
        project_root: &Path,
        open_options: TraceDecayOpenOptions,
        store_layout: StoreLayout,
        configuration_database: Arc<RegisteredGlobalDb>,
        profile_database: Arc<RegisteredGlobalDb>,
        runtime_registry: Arc<DaemonSessionRuntimeRegistryV1>,
    ) -> Result<Self> {
        let db = Self::mount_worktree_graph(
            runtime_registry.as_ref(),
            project_root,
            &store_layout,
            &store_layout.graph_db_path,
            branch::current_branch(project_root).as_deref(),
            "init",
            DatabaseAccessMode::ReadWrite,
        )
        .await?;
        let (configuration_runtime, configuration) = ProjectConfigurationRuntime::open(
            open_runtime_configuration_for_registered_database(
                project_root,
                &store_layout,
                configuration_database,
            )
            .await?,
        )?;
        let configuration_runtime = Arc::new(configuration_runtime);
        let config = configuration.config.clone();
        install_configuration_daemon_client_for_project(
            &configuration.target,
            configuration_runtime.client(),
        );
        let active_graph_layout = active_graph_layout(&store_layout.graph_db_path);
        if store_layout.storage_mode == storage::StorageMode::ProfileSharded {
            storage::write_store_manifest(&store_layout)?;
        }

        // Bootstrap branch metadata if we can detect a default branch
        let active_branch = branch::current_branch(project_root);
        let default_branch = active_branch.as_ref().and_then(|_| {
            branch::detect_default_branch(project_root).or_else(|| active_branch.clone())
        });
        if let Some(ref default) = default_branch {
            let meta = BranchMeta::new_for_dir(&store_layout.data_root, default);
            let _ = branch_meta::save_branch_meta(&store_layout.data_root, &meta);
        }

        let mut ts = Self {
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
            active_branch,
            serving_branch: None,
            fallback_warning: None,
            read_only: false,
            context_scout_owner: None,
            context_scout_claim_authorities: tokio::sync::RwLock::default(),
            #[cfg(any(test, feature = "test-transport"))]
            test_runtime_guard: None,
            standalone_maintenance_scope: None,
        };
        // First-touch parity with the registered open path: daemon warm-up
        // refuses to advertise an identity-bearing project whose Context
        // Scout owner is absent, so init must start it too.
        crate::hooks::publish_hook_v2_bindings(&ts.store_layout)?;
        if let Some(project_id) = crate::hooks::hook_v2_project_id_for_layout(&ts.store_layout) {
            ts.context_scout_owner =
                crate::agents::context_scout_owner::ProjectContextScoutOwnerV1::startup(
                    ts.db.clone(),
                    project_id,
                    tracedecay_domain::UtcMicros(
                        std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .map_or(1, |duration| {
                                duration.as_micros().min(i64::MAX as u128) as i64
                            }),
                    ),
                    None,
                )
                .await;
        }
        ts.register_project_store_in_global_registry().await?;
        Ok(ts)
    }

    pub async fn init_and_index_with_options(
        project_root: &Path,
        open_options: TraceDecayOpenOptions,
    ) -> Result<Self> {
        let cg = Self::init_with_options(project_root, open_options).await?;
        cg.index_all().await?;
        Ok(cg)
    }

    pub(crate) async fn init_and_index_with_registered_configuration(
        project_root: &Path,
        open_options: TraceDecayOpenOptions,
        store_layout: StoreLayout,
        configuration_database: Arc<RegisteredGlobalDb>,
        profile_database: Arc<RegisteredGlobalDb>,
        runtime_registry: Arc<DaemonSessionRuntimeRegistryV1>,
    ) -> Result<Self> {
        let cg = Self::init_with_registered_configuration(
            project_root,
            open_options,
            store_layout,
            configuration_database,
            profile_database,
            runtime_registry,
        )
        .await?;
        cg.index_all().await?;
        Ok(cg)
    }

    /// Returns a reference to the underlying database.
    pub fn db(&self) -> &Database {
        &self.db
    }

    async fn schema_version(db: &Database, operation: &str) -> Result<u32> {
        let connection = db.engine_conn();
        let mut rows = connection
            .query("PRAGMA user_version", ())
            .await
            .map_err(|e| TraceDecayError::Database {
                message: format!("{operation}: failed to read user_version: {e}"),
                operation: operation.to_string(),
            })?;
        let row = rows.next().await.map_err(|e| TraceDecayError::Database {
            message: format!("{operation}: failed to read user_version row: {e}"),
            operation: operation.to_string(),
        })?;
        match row {
            Some(row) => {
                let version: i64 = row.get(0).map_err(|e| TraceDecayError::Database {
                    message: format!("{operation}: failed to read user_version value: {e}"),
                    operation: operation.to_string(),
                })?;
                Ok(version as u32)
            }
            None => Ok(0),
        }
    }

    async fn latest_schema_version() -> Result<u32> {
        Ok(crate::db::migrations::LATEST_VERSION)
    }

    pub async fn ensure_schema_current(&self) -> Result<()> {
        let current = Self::schema_version(&self.db, "ensure_schema_current").await?;
        let latest = Self::latest_schema_version().await?;
        if current < latest {
            return Err(TraceDecayError::Config {
                message: format!(
                    "read-only TraceDecay database schema is v{current}, but this binary requires \
                     v{latest}; open the project with write access to run migrations before serving \
                     it read-only"
                ),
            });
        }
        if current > latest {
            return Err(TraceDecayError::Config {
                message: format!(
                    "TraceDecay database schema v{current} is newer than this binary supports \
                     (v{latest}); upgrade tracedecay before serving this store"
                ),
            });
        }
        Ok(())
    }

    /// Opens an existing `TraceDecay` project at the given root.
    ///
    /// If branch metadata exists, resolves the current git branch, auto-adds
    /// it to branch tracking when needed, and opens the corresponding DB.
    /// Falls back to the nearest tracked ancestor DB with a warning only when
    /// the live branch cannot be auto-tracked, such as detached HEAD.
    /// If the previous operation was interrupted (dirty sentinel exists),
    /// the database is integrity-checked before any writable open.
    pub async fn open(project_root: &Path) -> Result<Self> {
        Self::open_with_options(project_root, TraceDecayOpenOptions::default()).await
    }

    pub async fn open_with_options(
        project_root: &Path,
        open_options: TraceDecayOpenOptions,
    ) -> Result<Self> {
        #[cfg(any(test, feature = "test-transport"))]
        {
            let open_options = Self::standalone_test_open_options(project_root, open_options);
            let runtime = Self::standalone_test_runtime(project_root, &open_options).await?;
            let mut graph = runtime
                .open_project_graph_for_test(project_root, open_options)
                .await?;
            graph.test_runtime_guard = Some(runtime);
            Ok(graph)
        }
        #[cfg(not(any(test, feature = "test-transport")))]
        {
            let maintenance =
                Self::standalone_maintenance_scope(&open_options, "direct project open")?;
            let mut graph = Self::open_with_exclusive_maintenance(
                project_root,
                open_options,
                maintenance.lifecycle(),
            )
            .await?;
            graph.standalone_maintenance_scope = Some(maintenance);
            Ok(graph)
        }
    }

    /// Opens an initialized project through the canonical registered runtime
    /// while the caller holds the exact profile's exclusive maintenance lease.
    pub async fn open_with_exclusive_maintenance(
        project_root: &Path,
        open_options: TraceDecayOpenOptions,
        lifecycle_lease: &crate::lifecycle_lease::LifecycleLease,
    ) -> Result<Self> {
        let profile_root = open_options.resolved_profile_root()?;
        if !lifecycle_lease.is_exclusive() || !lifecycle_lease.guards_profile(&profile_root) {
            return Err(TraceDecayError::Config {
                message: "project open requires the exact profile's exclusive lifecycle lease"
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
            true,
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
        Self::open_with_registered_configuration(
            project_root,
            open_options,
            store_layout,
            configuration_database,
            profile_database,
            runtime_registry,
        )
        .await
    }

    pub(crate) async fn open_with_registered_configuration(
        project_root: &Path,
        open_options: TraceDecayOpenOptions,
        store_layout: StoreLayout,
        configuration_database: Arc<RegisteredGlobalDb>,
        profile_database: Arc<RegisteredGlobalDb>,
        runtime_registry: Arc<DaemonSessionRuntimeRegistryV1>,
    ) -> Result<Self> {
        Self::open_with_registered_configuration_inner(
            project_root,
            open_options,
            store_layout,
            configuration_database,
            profile_database,
            runtime_registry,
            true,
            false,
        )
        .await
    }

    pub(crate) async fn open_with_registered_configuration_deferred_post_open_health(
        project_root: &Path,
        open_options: TraceDecayOpenOptions,
        store_layout: StoreLayout,
        configuration_database: Arc<RegisteredGlobalDb>,
        profile_database: Arc<RegisteredGlobalDb>,
        runtime_registry: Arc<DaemonSessionRuntimeRegistryV1>,
    ) -> Result<Self> {
        Self::open_with_registered_configuration_inner(
            project_root,
            open_options,
            store_layout,
            configuration_database,
            profile_database,
            runtime_registry,
            true,
            true,
        )
        .await
    }

    async fn open_with_registered_configuration_inner(
        project_root: &Path,
        open_options: TraceDecayOpenOptions,
        store_layout: StoreLayout,
        configuration_database: Arc<RegisteredGlobalDb>,
        profile_database: Arc<RegisteredGlobalDb>,
        runtime_registry: Arc<DaemonSessionRuntimeRegistryV1>,
        allow_corrupt_branch_repair: bool,
        defer_post_open_health: bool,
    ) -> Result<Self> {
        let active_branch = branch::current_branch(project_root);
        let graph_scope = active_branch
            .clone()
            .or_else(|| crate::worktree::detached_worktree_graph_scope(project_root));
        Self::auto_track_active_branch_with_registered_configuration(
            project_root,
            &store_layout.data_root,
            graph_scope.as_deref(),
            open_options.clone(),
            &store_layout,
            &configuration_database,
            &profile_database,
            &runtime_registry,
        )
        .await?;

        let (db_path, mounted_graph_scope, fallback_warning) = Self::resolve_db_for_branch(
            project_root,
            &store_layout.data_root,
            graph_scope.as_deref(),
        );
        let serving_branch = if active_branch.is_none() && graph_scope.is_some() {
            None
        } else {
            mounted_graph_scope.clone()
        };

        // Sync state belongs to the concrete graph DB, not the repository-wide
        // store root. Different tracked branches have independent databases
        // and must never clear or inherit one another's dirty marker or lock.
        let active_graph_layout = active_graph_layout(&db_path);
        let repair_corrupt_branch = allow_corrupt_branch_repair
            && active_branch.is_some()
            && active_branch == serving_branch
            && db_path != store_layout.graph_db_path
            && db_path.parent() == Some(store_layout.data_root.join("branches").as_path());

        if !db_path.exists() {
            return Err(TraceDecayError::Config {
                message: format!(
                    "no TraceDecay database found at '{}'; run 'tracedecay init' first",
                    db_path.display()
                ),
            });
        }

        // A structured marker owned by a live process describes work in
        // flight, not a crash. Only abandoned, legacy, or malformed dirty
        // markers enter recovery and contend for the writer's lock.
        let (db, migrated) = match Self::run_open_health_recovery(
            project_root,
            open_options.clone(),
            &store_layout,
            &db_path,
            mounted_graph_scope.as_deref(),
            &active_graph_layout,
            repair_corrupt_branch,
            defer_post_open_health,
            Arc::clone(&configuration_database),
            Arc::clone(&profile_database),
            Arc::clone(&runtime_registry),
        )
        .await?
        {
            OpenHealthOutcome::Ready { db, migrated } => (db, migrated),
            OpenHealthOutcome::Recovered(result) => return result,
        };

        let (configuration_runtime, configuration) = ProjectConfigurationRuntime::open(
            open_runtime_configuration_for_registered_database(
                project_root,
                &store_layout,
                configuration_database,
            )
            .await?,
        )?;
        let configuration_runtime = Arc::new(configuration_runtime);
        let config = configuration.config.clone();
        install_configuration_daemon_client_for_project(
            &configuration.target,
            configuration_runtime.client(),
        );
        let mut ts = Self {
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
            active_branch,
            serving_branch,
            fallback_warning,
            read_only: false,
            context_scout_owner: None,
            context_scout_claim_authorities: tokio::sync::RwLock::default(),
            #[cfg(any(test, feature = "test-transport"))]
            test_runtime_guard: None,
            standalone_maintenance_scope: None,
        };

        crate::hooks::publish_hook_v2_bindings(&ts.store_layout)?;
        if let Some(project_id) = crate::hooks::hook_v2_project_id_for_layout(&ts.store_layout) {
            ts.context_scout_owner =
                crate::agents::context_scout_owner::ProjectContextScoutOwnerV1::startup(
                    ts.db.clone(),
                    project_id,
                    tracedecay_domain::UtcMicros(
                        std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .map_or(1, |duration| {
                                duration.as_micros().min(i64::MAX as u128) as i64
                            }),
                    ),
                    None,
                )
                .await;
        }

        if migrated.is_some_and(|from| {
            crate::db::migrations::graph_reindex_required(
                from,
                crate::db::migrations::LATEST_VERSION,
            )
        }) {
            ts.mark_migration_reindex_pending().await?;
        }

        ts.register_project_store_in_global_registry().await?;
        ts.schedule_migration_reindex_if_needed().await?;
        Ok(ts)
    }

    /// Opens an existing project for read-only inspection.
    ///
    /// Unlike [`Self::open`], this does not run migrations, repair dirty
    /// sentinels, clear markers, or rewrite corrupted DBs. It is intended for
    /// status/verification commands that must be able to inspect read-only
    /// stores without mutating them.
    pub async fn open_read_only(project_root: &Path) -> Result<Self> {
        Self::open_read_only_with_options(project_root, TraceDecayOpenOptions::default()).await
    }

    pub async fn open_read_only_with_options(
        project_root: &Path,
        open_options: TraceDecayOpenOptions,
    ) -> Result<Self> {
        #[cfg(any(test, feature = "test-transport"))]
        {
            let open_options = Self::standalone_test_open_options(project_root, open_options);
            let runtime = Self::standalone_test_runtime(project_root, &open_options).await?;
            let mut graph = runtime
                .open_project_graph_read_only_for_test(project_root, open_options)
                .await?;
            graph.test_runtime_guard = Some(runtime);
            Ok(graph)
        }
        #[cfg(not(any(test, feature = "test-transport")))]
        {
            let maintenance =
                Self::standalone_maintenance_scope(&open_options, "direct read-only project open")?;
            let mut graph = Self::open_read_only_with_exclusive_maintenance(
                project_root,
                open_options,
                maintenance.lifecycle(),
            )
            .await?;
            graph.standalone_maintenance_scope = Some(maintenance);
            Ok(graph)
        }
    }

    #[cfg(not(any(test, feature = "test-transport")))]
    async fn open_read_only_with_exclusive_maintenance(
        project_root: &Path,
        open_options: TraceDecayOpenOptions,
        lifecycle_lease: &crate::lifecycle_lease::LifecycleLease,
    ) -> Result<Self> {
        let profile_root = open_options.resolved_profile_root()?;
        if !lifecycle_lease.is_exclusive() || !lifecycle_lease.guards_profile(&profile_root) {
            return Err(TraceDecayError::Config {
                message:
                    "read-only project open requires the exact profile's exclusive lifecycle lease"
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
            false,
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
        Self::open_read_only_with_registered_configuration(
            project_root,
            open_options,
            store_layout,
            configuration_database,
            profile_database,
            runtime_registry,
        )
        .await
    }

    pub(crate) async fn open_read_only_with_registered_configuration(
        project_root: &Path,
        open_options: TraceDecayOpenOptions,
        store_layout: StoreLayout,
        configuration_database: Arc<RegisteredGlobalDb>,
        profile_database: Arc<RegisteredGlobalDb>,
        runtime_registry: Arc<DaemonSessionRuntimeRegistryV1>,
    ) -> Result<Self> {
        let active_branch = branch::current_branch(project_root);
        let graph_scope = active_branch
            .clone()
            .or_else(|| crate::worktree::detached_worktree_graph_scope(project_root));

        let (db_path, mounted_graph_scope, fallback_warning) = Self::resolve_db_for_branch(
            project_root,
            &store_layout.data_root,
            graph_scope.as_deref(),
        );
        let serving_branch = if active_branch.is_none() && graph_scope.is_some() {
            None
        } else {
            mounted_graph_scope.clone()
        };
        let active_graph_layout = active_graph_layout(&db_path);

        if !db_path.exists() {
            return Err(TraceDecayError::Config {
                message: format!(
                    "no TraceDecay database found at '{}'; run 'tracedecay init' first",
                    db_path.display()
                ),
            });
        }

        let db = Self::mount_worktree_graph(
            runtime_registry.as_ref(),
            project_root,
            &store_layout,
            &db_path,
            mounted_graph_scope.as_deref(),
            "open project store read-only",
            DatabaseAccessMode::ReadOnly,
        )
        .await?;
        let (configuration_runtime, configuration) = ProjectConfigurationRuntime::open(
            open_runtime_configuration_for_registered_database_read_only(
                project_root,
                &store_layout,
                configuration_database,
            )
            .await?,
        )?;
        let configuration_runtime = Arc::new(configuration_runtime);
        let config = configuration.config.clone();
        install_configuration_daemon_client_for_project(
            &configuration.target,
            configuration_runtime.client(),
        );
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
            active_branch,
            serving_branch,
            fallback_warning,
            read_only: true,
            context_scout_owner: None,
            context_scout_claim_authorities: tokio::sync::RwLock::default(),
            #[cfg(any(test, feature = "test-transport"))]
            test_runtime_guard: None,
            standalone_maintenance_scope: None,
        })
    }

    /// Mirrors automatic branch tracking for a daemon-owned project open.
    ///
    /// The ordinary branch helper reopens through the public standalone API,
    /// which intentionally has no configuration authority. A registered open
    /// must retain its exact registered project session while preparing and
    /// syncing the branch instead.
    async fn auto_track_active_branch_with_registered_configuration(
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
            context_scout_owner: None,
            context_scout_claim_authorities: tokio::sync::RwLock::default(),
            #[cfg(any(test, feature = "test-transport"))]
            test_runtime_guard: self.test_runtime_guard.clone(),
            standalone_maintenance_scope: self.standalone_maintenance_scope.clone(),
        };

        let mut attempts = 0;
        loop {
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
                Err(error) => return Err(error),
            }
        }
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
            false,
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
            match graph.sync().await {
                Ok(_) => return Ok(()),
                Err(TraceDecayError::SyncLock { .. }) if attempts < 20 => {
                    attempts += 1;
                    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                }
                Err(error) => return Err(error),
            }
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
            true,
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
        let (configuration_runtime, configuration) = ProjectConfigurationRuntime::open(
            open_runtime_configuration_for_registered_database_read_only(
                project_root,
                &store_layout,
                configuration_database,
            )
            .await?,
        )?;
        let configuration_runtime = Arc::new(configuration_runtime);
        let config = configuration.config.clone();
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
            context_scout_owner: None,
            context_scout_claim_authorities: tokio::sync::RwLock::default(),
            #[cfg(any(test, feature = "test-transport"))]
            test_runtime_guard: None,
            standalone_maintenance_scope: None,
        })
    }

    /// Lists tracked branches from metadata. Returns `None` if no branch tracking.
    pub fn list_tracked_branches(project_root: &Path) -> Option<Vec<String>> {
        let store_layout = storage::resolve_layout_for_current_profile(project_root).ok()?;
        let meta = branch_meta::load_branch_meta(&store_layout.data_root)?;
        Some(meta.branches.keys().cloned().collect())
    }
}

#[cfg(any(test, feature = "test-transport"))]
fn configuration_runtime_unavailable() -> TraceDecayError {
    TraceDecayError::Config {
        message:
            "configuration authority unavailable: a registered project session runtime is required"
                .to_owned(),
    }
}
