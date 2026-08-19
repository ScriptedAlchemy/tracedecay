//! Lifecycle: init/open/branch-tracking entry points plus the profile-store
//! registration helpers they rely on.

use std::path::Path;
use std::path::PathBuf;
use std::sync::LazyLock;
use std::sync::{Arc, OnceLock};

use crate::branch;
use crate::branch_meta::{self, BranchMeta};
use crate::config::{
    install_usecase_runtime_configuration_authority, materialize_root_runtime_configuration,
};
use crate::daemon::store_runtime::session_registry::DaemonSessionRuntimeRegistryV1;
use crate::db::{Database, DatabaseAccessMode, DatabaseAuthority};
use crate::errors::{Result, TraceDecayError};
use crate::global_db::RegisteredGlobalDbLeaseV1;
use crate::storage::{self, StoreLayout};
use crate::support::weak_registry::WeakRegistry;
use tokio::sync::Mutex as AsyncMutex;
#[cfg(any(test, feature = "test-transport"))]
use tracedecay_store::ProjectId;
use tracedecay_usecases::config::{
    open_runtime_configuration_for_registered_database,
    open_runtime_configuration_for_registered_database_read_only,
};
use tracedecay_usecases::configuration::ProjectConfigurationRuntime;

use super::{TraceDecay, TraceDecayOpenOptions};

mod adoption;
mod branches;
mod identity;
mod registry;

pub use adoption::MovedStoreAdoption;
pub(crate) use registry::git_remote_url;

#[cfg(not(any(test, feature = "test-transport")))]
static STANDALONE_MAINTENANCE_SCOPES: LazyLock<
    WeakRegistry<PathBuf, crate::db::OwnedMaintenanceDatabaseScope>,
> = LazyLock::new(WeakRegistry::new);

/// One standalone session runtime registry per profile, process-wide.
///
/// Direct init/open still has a single writer for the profile session-relation
/// graph (an exclusive Grafeo file lock). A second independent registry on the
/// same profile cannot open that store. Concurrent opens in one process join
/// the live registry; entries are weak so close-then-reopen constructs a
/// fresh mount after the last holder drops.
static STANDALONE_SESSION_REGISTRIES: LazyLock<
    AsyncMutex<WeakRegistry<PathBuf, DaemonSessionRuntimeRegistryV1>>,
> = LazyLock::new(|| AsyncMutex::new(WeakRegistry::new()));

async fn join_standalone_session_registry(
    identity: crate::daemon::profile_identity::LocalProfileIdentityAuthorityV1,
) -> Result<Arc<DaemonSessionRuntimeRegistryV1>> {
    let profile_key = crate::lifecycle_lease::canonical_or_original(identity.profile_root());
    let registries = STANDALONE_SESSION_REGISTRIES.lock().await;
    if let Some(registry) = registries.get_live(&profile_key) {
        return Ok(registry);
    }
    let registry = Arc::new(DaemonSessionRuntimeRegistryV1::open(identity).await?);
    registries.insert(profile_key, &registry);
    Ok(registry)
}

/// One retained standalone test runtime per (profile root, project root).
///
/// The daemon owns exactly one session runtime registry per profile, and the
/// profile session-relation graph store has a single writer (an exclusive
/// Grafeo file lock). A second independent runtime on the same profile
/// therefore cannot open the store. Standalone test opens share one retained
/// runtime per key, mirroring the production single-registry invariant (and
/// `STANDALONE_MAINTENANCE_SCOPES` above); the underlying daemon session
/// registry is additionally shared per profile inside the runtime
/// constructor, the way one production daemon mounts many projects. Entries
/// are weak: once every graph holding the runtime drops, the next open
/// constructs a fresh runtime, so close-then-reopen journeys still observe
/// fresh mounts.
#[cfg(any(test, feature = "test-transport"))]
static STANDALONE_TEST_RUNTIMES: LazyLock<
    AsyncMutex<WeakRegistry<(PathBuf, PathBuf), crate::host_admission::HostAdmissionTestRuntimeV1>>,
> = LazyLock::new(|| AsyncMutex::new(WeakRegistry::new()));

impl TraceDecay {
    #[cfg(not(any(test, feature = "test-transport")))]
    fn standalone_maintenance_scope(
        open_options: &TraceDecayOpenOptions,
        operation: &'static str,
    ) -> Result<Arc<crate::db::OwnedMaintenanceDatabaseScope>> {
        let profile_root = open_options.resolved_profile_root()?;
        STANDALONE_MAINTENANCE_SCOPES.retain_live();
        let profile_key = crate::lifecycle_lease::canonical_or_original(&profile_root);
        if let Some(scope) = STANDALONE_MAINTENANCE_SCOPES.get_live(&profile_key) {
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
        STANDALONE_MAINTENANCE_SCOPES.insert(profile_key, &scope);
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
    ) -> Result<Arc<crate::host_admission::HostAdmissionTestRuntimeV1>> {
        let profile_root = open_options.resolved_profile_root()?;
        if !crate::db::is_isolated_test_path(project_root)
            || !crate::db::is_isolated_test_path(&profile_root)
        {
            return Err(configuration_runtime_unavailable());
        }
        let project_id = storage::resolve_persisted_layout(project_root, &profile_root)?
            .and_then(|layout| layout.identity.project_id)
            .unwrap_or_else(|| storage::default_profile_project_id(project_root));
        let project_id = ProjectId::new(project_id).map_err(|error| TraceDecayError::Config {
            message: format!("invalid standalone test project identity: {error}"),
        })?;
        let registry_key = (
            crate::lifecycle_lease::canonical_or_original(&profile_root),
            crate::lifecycle_lease::canonical_or_original(project_root),
        );
        // The async lock is held across construction so two concurrent opens
        // of the same key cannot race into two runtimes.
        let runtimes = STANDALONE_TEST_RUNTIMES.lock().await;
        if let Some(runtime) = runtimes.get_live(&registry_key) {
            return Ok(runtime);
        }
        let runtime = Arc::new(
            crate::host_admission::HostAdmissionTestRuntimeV1::project(
                profile_root,
                project_root,
                project_id,
            )
            .await?,
        );
        runtimes.insert(registry_key, &runtime);
        Ok(runtime)
    }

    pub(super) async fn mount_project_graph(
        runtime_registry: &DaemonSessionRuntimeRegistryV1,
        project_root: &Path,
        store_layout: &StoreLayout,
        operation: &'static str,
        access: DatabaseAccessMode,
    ) -> Result<Database> {
        let project_id = Self::registered_project_id(store_layout)?;
        let canonical_database_path = &store_layout.graph_db_path;
        if matches!(access, DatabaseAccessMode::ReadOnly) {
            return runtime_registry
                .project_graph_registered(project_id, canonical_database_path.clone(), access)
                .await;
        }
        let authority = DatabaseAuthority::for_runtime(canonical_database_path, operation)?;
        runtime_registry
            .project_graph(
                project_root,
                project_id,
                canonical_database_path.clone(),
                authority,
                access,
            )
            .await
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
            graph._standalone_maintenance_scope = Some(maintenance);
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
        let runtime_registry = join_standalone_session_registry(identity).await?;
        let profile_database = runtime_registry.profile_database().await?;
        let store_layout = Self::resolve_first_touch_configuration_layout(
            project_root,
            &open_options,
            profile_database.as_ref(),
        )
        .await?;
        let project_id = Self::registered_project_id(&store_layout)?;
        // Persist the minted identity in the sanctioned repo-adjacent anchor:
        // the `.git/` repository identity marker. A non-git root persists
        // nothing here — its identity is deterministic from the canonical
        // path and durably owned by the profile registry. TraceDecay never
        // creates files inside a project's working tree.
        crate::storage::write_repository_identity_marker(project_root, project_id.as_str())?;
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
    ) -> Result<(Self, Arc<crate::host_admission::HostAdmissionTestRuntimeV1>)> {
        let profile_root = crate::storage::default_profile_root()?;
        let project_id = tracedecay_domain::ProjectId::new(project_id).map_err(|error| {
            TraceDecayError::Config {
                message: format!("invalid test fixture project identity: {error}"),
            }
        })?;
        let runtime = Arc::new(
            crate::host_admission::HostAdmissionTestRuntimeV1::project(
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
        configuration_database: RegisteredGlobalDbLeaseV1,
        profile_database: RegisteredGlobalDbLeaseV1,
        runtime_registry: Arc<DaemonSessionRuntimeRegistryV1>,
    ) -> Result<Self> {
        // Computed once and reused below (for `active_branch`) instead of
        // calling `branch::current_branch` twice for the same project root.
        let active_branch = branch::current_branch(project_root);
        let (serving_branch, fallback_warning) =
            Self::resolve_branch_provenance(project_root, &store_layout, &active_branch);
        let db = Self::mount_project_graph(
            runtime_registry.as_ref(),
            project_root,
            &store_layout,
            "init",
            DatabaseAccessMode::ReadWrite,
        )
        .await?;
        install_usecase_runtime_configuration_authority()?;
        let (configuration_runtime, configuration) = ProjectConfigurationRuntime::open(
            open_runtime_configuration_for_registered_database(
                project_root,
                &store_layout,
                configuration_database,
            )
            .await?,
        )?;
        let configuration_runtime = Arc::new(configuration_runtime);
        let config = materialize_root_runtime_configuration(&configuration)?;
        if store_layout.storage_mode == storage::StorageMode::ProfileSharded {
            storage::write_store_manifest(&store_layout)?;
        }

        // Bootstrap branch metadata if we can detect a default branch
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
            open_options,
            active_branch,
            serving_branch,
            fallback_warning,
            read_only: false,
            db_path_cache: OnceLock::new(),
            context_scout_owner: None,
            context_scout_claim_authorities: tokio::sync::RwLock::new(Vec::new()),
            #[cfg(any(test, feature = "test-transport"))]
            test_runtime_guard: None,
            _standalone_maintenance_scope: None,
        };
        // First-touch parity with the registered open path: daemon warm-up
        // refuses to advertise an identity-bearing project whose Context
        // Scout owner is absent, so init must start it too.
        crate::hooks::publish_hook_bindings(&ts.store_layout)?;
        if let Some(project_id) = crate::hooks::hook_project_id_for_layout(&ts.store_layout) {
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

    /// Returns a reference to the underlying database.
    pub fn db(&self) -> &Database {
        &self.db
    }

    async fn schema_version(db: &Database, operation: &str) -> Result<u32> {
        let connection = db.read_connection();
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

    async fn ensure_database_schema_current(db: &Database) -> Result<()> {
        let current = Self::schema_version(db, "ensure_schema_current").await?;
        let supported = crate::db::migrations::SCHEMA_VERSION;
        if current != supported {
            return Err(TraceDecayError::reset_required(
                "graph store",
                format!(
                    "database schema v{current} is not the v{supported} shape this binary \
                     creates; this store was created by an incompatible binary and cannot be \
                     upgraded in place. Remove the store directory and let this binary create a \
                     fresh one."
                ),
            ));
        }
        Ok(())
    }

    /// Refuses a read-only store that is not at the one schema shape this
    /// binary creates. There is no upgrade path to name: the store was written
    /// by an incompatible binary, so the only remedy is a fresh one.
    pub async fn ensure_schema_current(&self) -> Result<()> {
        Self::ensure_database_schema_current(&self.db).await
    }

    /// Opens an existing `TraceDecay` project at the given root.
    ///
    /// If branch metadata exists, resolves the current git branch's published
    /// provenance. Registered open admits only the exact final relational
    /// schema; code-index activation and reconciliation happen after open
    /// through the daemon-owned scheduler.
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
            graph._standalone_maintenance_scope = Some(maintenance);
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
        let runtime_registry = join_standalone_session_registry(identity).await?;
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
        configuration_database: RegisteredGlobalDbLeaseV1,
        profile_database: RegisteredGlobalDbLeaseV1,
        runtime_registry: Arc<DaemonSessionRuntimeRegistryV1>,
    ) -> Result<Self> {
        let active_branch = branch::current_branch(project_root);
        let db_path = store_layout.graph_db_path.clone();
        let (serving_branch, fallback_warning) =
            Self::resolve_branch_provenance(project_root, &store_layout, &active_branch);

        if !db_path.exists() {
            return Err(TraceDecayError::Config {
                message: format!(
                    "no TraceDecay database found at '{}'; run 'tracedecay init' first",
                    db_path.display()
                ),
            });
        }

        // Registered mounts perform the exact final-schema admission. Project
        // open never repairs, rebuilds, or indexes the graph inline; retained
        // code-index activation is owned by the daemon after publication.
        let db = Self::mount_project_graph(
            runtime_registry.as_ref(),
            project_root,
            &store_layout,
            "open project store",
            DatabaseAccessMode::ReadWrite,
        )
        .await?;

        install_usecase_runtime_configuration_authority()?;
        let (configuration_runtime, configuration) = ProjectConfigurationRuntime::open(
            open_runtime_configuration_for_registered_database(
                project_root,
                &store_layout,
                configuration_database,
            )
            .await?,
        )?;
        let configuration_runtime = Arc::new(configuration_runtime);
        let config = materialize_root_runtime_configuration(&configuration)?;
        let mut ts = Self {
            db,
            profile_database,
            store_runtime_registry: runtime_registry,
            config,
            configuration_runtime,
            project_root: project_root.to_path_buf(),
            store_layout,
            open_options,
            active_branch,
            serving_branch,
            fallback_warning,
            read_only: false,
            db_path_cache: OnceLock::new(),
            context_scout_owner: None,
            context_scout_claim_authorities: tokio::sync::RwLock::new(Vec::new()),
            #[cfg(any(test, feature = "test-transport"))]
            test_runtime_guard: None,
            _standalone_maintenance_scope: None,
        };

        crate::hooks::publish_hook_bindings(&ts.store_layout)?;
        if let Some(project_id) = crate::hooks::hook_project_id_for_layout(&ts.store_layout) {
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

    /// Branch provenance for an ordinary open of the single project store.
    ///
    /// One store serves every branch, so this never chooses a database; it
    /// decides only which tracked branch's publication provenance the open is
    /// scoped to (`serving_branch`) and whether that provenance is a fallback
    /// ancestor rather than the live branch itself (`fallback_warning`).
    ///
    /// A detached linked worktree is pinned to its own snapshot scope and has
    /// no branch identity to drift from, so it resolves no provenance at all:
    /// reporting the default branch's provenance there would be untrue, and
    /// the branch-drift guard must stay inert for exactly that shape.
    fn resolve_branch_provenance(
        project_root: &Path,
        store_layout: &StoreLayout,
        active_branch: &Option<String>,
    ) -> (Option<String>, Option<String>) {
        let graph_scope = active_branch
            .clone()
            .or_else(|| crate::worktree::detached_worktree_graph_scope(project_root));
        let (_, serving_branch, fallback_warning) = Self::resolve_db_for_branch(
            project_root,
            &store_layout.data_root,
            graph_scope.as_deref(),
        );
        if active_branch.is_none() && graph_scope.is_some() {
            (None, None)
        } else {
            (serving_branch, fallback_warning)
        }
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
            graph._standalone_maintenance_scope = Some(maintenance);
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
        let runtime_registry = join_standalone_session_registry(identity).await?;
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
        configuration_database: RegisteredGlobalDbLeaseV1,
        profile_database: RegisteredGlobalDbLeaseV1,
        runtime_registry: Arc<DaemonSessionRuntimeRegistryV1>,
    ) -> Result<Self> {
        let active_branch = branch::current_branch(project_root);
        let db_path = store_layout.graph_db_path.clone();
        let (serving_branch, fallback_warning) =
            Self::resolve_branch_provenance(project_root, &store_layout, &active_branch);

        if !db_path.exists() {
            return Err(TraceDecayError::Config {
                message: format!(
                    "no TraceDecay database found at '{}'; run 'tracedecay init' first",
                    db_path.display()
                ),
            });
        }

        let db = Self::mount_project_graph(
            runtime_registry.as_ref(),
            project_root,
            &store_layout,
            "open project store read-only",
            DatabaseAccessMode::ReadOnly,
        )
        .await?;
        // Refuse an incompatible nonempty graph before configuration open,
        // hooks, or any other normal project-open work can observe it.
        Self::ensure_database_schema_current(&db).await?;
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
        Ok(Self {
            db,
            profile_database,
            store_runtime_registry: runtime_registry,
            config,
            configuration_runtime,
            project_root: project_root.to_path_buf(),
            store_layout,
            open_options,
            active_branch,
            serving_branch,
            fallback_warning,
            read_only: true,
            db_path_cache: OnceLock::new(),
            context_scout_owner: None,
            context_scout_claim_authorities: tokio::sync::RwLock::new(Vec::new()),
            #[cfg(any(test, feature = "test-transport"))]
            test_runtime_guard: None,
            _standalone_maintenance_scope: None,
        })
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

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn nonempty_wrong_schema_read_only_open_returns_reset_required() {
        let root = tempfile::TempDir::new().expect("fixture root");
        let project = root.path().join("project");
        let profile = root.path().join("profile");
        std::fs::create_dir_all(&project).expect("create project root");
        let options = TraceDecayOpenOptions {
            profile_root: Some(profile.clone()),
            global_db_path: Some(profile.join("registry.db")),
        };
        let initialized = TraceDecay::init_with_options(&project, options.clone())
            .await
            .expect("initialize project graph");
        let db_path = initialized.store_layout().graph_db_path.clone();
        initialized.close();
        let connection = rusqlite::Connection::open(&db_path).expect("open graph fixture");
        connection
            .pragma_update(
                None,
                "user_version",
                crate::db::migrations::SCHEMA_VERSION - 1,
            )
            .expect("stamp incompatible graph schema");
        drop(connection);

        let error = match TraceDecay::open_read_only_with_options(&project, options).await {
            Ok(_) => panic!("nonempty graph at another schema must require a reset"),
            Err(error) => error,
        };

        match error {
            TraceDecayError::ResetRequired { authority, reason } => {
                assert_eq!(
                    authority, "SQLite store",
                    "wrong-schema read-only open must name the owning store: {reason:?}"
                );
                assert!(
                    reason.contains("schema"),
                    "wrong-schema read-only open must remain a schema ResetRequired: {reason:?}"
                );
            }
            other => panic!("nonempty graph at another schema must require a reset, got {other:?}"),
        }
    }
}
