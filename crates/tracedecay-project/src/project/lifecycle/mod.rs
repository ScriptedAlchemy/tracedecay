//! Lifecycle: init/open/branch-tracking entry points plus the profile-store
//! registration helpers they rely on.

use std::path::Path;
use std::path::PathBuf;
use std::sync::LazyLock;
use std::sync::{Arc, OnceLock};

use crate::config::{
    install_usecase_runtime_configuration_authority,
    open_runtime_configuration_for_registered_database,
    open_runtime_configuration_for_registered_database_read_only,
};
use crate::project_store_runtime::join_standalone_session_registry;
#[cfg(any(test, feature = "test-helpers"))]
use tokio::sync::Mutex as AsyncMutex;
use tracedecay_configuration::ProjectConfigurationRuntime;
use tracedecay_domain::errors::{Result, TraceDecayError};
use tracedecay_global_db::{RegisteredGlobalDbLeaseV1, registered_enrollment_roots};
use tracedecay_runtime_core::branch;
use tracedecay_runtime_core::branch_meta::{self, BranchMeta};
use tracedecay_runtime_core::db::{Database, DatabaseAccessMode, DatabaseAuthority};
use tracedecay_runtime_core::storage::{self, StoreLayout};
use tracedecay_runtime_core::weak_registry::WeakRegistry;
#[cfg(any(test, feature = "test-helpers"))]
use tracedecay_store::ProjectId;
use tracedecay_store_runtime::DaemonSessionRuntimeRegistryV1;

use super::{TraceDecay, TraceDecayOpenOptions};

mod branches;
mod identity;
mod registry;

pub use tracedecay_daemon_protocol::MovedStoreAdoption;

#[cfg(not(any(test, feature = "test-transport")))]
static STANDALONE_MAINTENANCE_SCOPES: LazyLock<
    WeakRegistry<PathBuf, tracedecay_runtime_core::db::OwnedMaintenanceDatabaseScope>,
> = LazyLock::new(WeakRegistry::new);

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
#[cfg(any(test, feature = "test-helpers"))]
static STANDALONE_TEST_RUNTIMES: LazyLock<
    AsyncMutex<
        WeakRegistry<
            (PathBuf, PathBuf),
            crate::test_support::host_admission::HostAdmissionTestRuntimeV1,
        >,
    >,
> = LazyLock::new(|| AsyncMutex::new(WeakRegistry::new()));

impl TraceDecay {
    #[cfg(not(any(test, feature = "test-transport")))]
    fn standalone_maintenance_scope(
        open_options: &TraceDecayOpenOptions,
        operation: &'static str,
    ) -> Result<Arc<tracedecay_runtime_core::db::OwnedMaintenanceDatabaseScope>> {
        let profile_root = open_options.resolved_profile_root()?;
        STANDALONE_MAINTENANCE_SCOPES.retain_live();
        let profile_key =
            tracedecay_runtime_core::lifecycle_lease::canonical_or_original(&profile_root);
        if let Some(scope) = STANDALONE_MAINTENANCE_SCOPES.get_live(&profile_key) {
            return Ok(scope);
        }
        let lifecycle = tracedecay_runtime_core::lifecycle_lease::acquire_exclusive_for_profile(
            &profile_root,
            operation,
        )?;
        let scope = Arc::new(
            tracedecay_runtime_core::db::enter_owned_maintenance_database_scope(
                lifecycle,
                &profile_root,
                operation,
            )?,
        );
        let profile_key =
            tracedecay_runtime_core::lifecycle_lease::canonical_or_original(&profile_root);
        STANDALONE_MAINTENANCE_SCOPES.insert(profile_key, &scope);
        Ok(scope)
    }

    #[cfg(any(test, feature = "test-helpers"))]
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

    #[cfg(any(test, feature = "test-helpers"))]
    #[hotpath::skip]
    async fn standalone_test_runtime(
        project_root: &Path,
        open_options: &TraceDecayOpenOptions,
    ) -> Result<Arc<crate::test_support::host_admission::HostAdmissionTestRuntimeV1>> {
        let profile_root = open_options.resolved_profile_root()?;
        if !tracedecay_runtime_core::db::is_isolated_test_path(project_root)
            || !tracedecay_runtime_core::db::is_isolated_test_path(&profile_root)
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
            tracedecay_runtime_core::lifecycle_lease::canonical_or_original(&profile_root),
            tracedecay_runtime_core::lifecycle_lease::canonical_or_original(project_root),
        );
        // The async lock is held across construction so two concurrent opens
        // of the same key cannot race into two runtimes.
        let runtimes = STANDALONE_TEST_RUNTIMES.lock().await;
        if let Some(runtime) = runtimes.get_live(&registry_key) {
            return Ok(runtime);
        }
        let runtime = Arc::new(
            crate::test_support::host_admission::HostAdmissionTestRuntimeV1::project(
                profile_root,
                project_root,
                project_id,
            )
            .await?,
        );
        runtimes.insert(registry_key, &runtime);
        Ok(runtime)
    }

    /// Initializes a project through the shared registered test runtime for
    /// its (profile, project) key instead of the exclusive maintenance lease
    /// a production standalone init takes, so a test process that also holds
    /// daemon-scoped fixtures on the same profile can open it. The graph keeps
    /// the runtime alive for its lifetime ([`Self::test_runtime_for_test`]).
    ///
    /// `test-transport` builds route [`Self::init_with_options`] here; every
    /// other test build names this constructor explicitly.
    #[cfg(any(test, feature = "test-helpers"))]
    #[hotpath::skip]
    pub async fn init_with_options_for_test(
        project_root: &Path,
        open_options: TraceDecayOpenOptions,
    ) -> Result<Self> {
        let open_options = Self::standalone_test_open_options(project_root, open_options);
        let runtime = Self::standalone_test_runtime(project_root, &open_options).await?;
        let mut graph = runtime
            .initialize_project_graph_for_test(project_root, open_options)
            .await?;
        graph.test_runtime_guard = Some(runtime);
        Ok(graph)
    }

    /// [`Self::open_with_options`] through the shared registered test runtime;
    /// see [`Self::init_with_options_for_test`].
    #[cfg(any(test, feature = "test-helpers"))]
    #[hotpath::skip]
    pub async fn open_with_options_for_test(
        project_root: &Path,
        open_options: TraceDecayOpenOptions,
    ) -> Result<Self> {
        let open_options = Self::standalone_test_open_options(project_root, open_options);
        let runtime = Self::standalone_test_runtime(project_root, &open_options).await?;
        let mut graph = runtime
            .open_project_graph_for_test(project_root, open_options)
            .await?;
        graph.test_runtime_guard = Some(runtime);
        Ok(graph)
    }

    /// [`Self::open_read_only_with_options`] through the shared registered
    /// test runtime; see [`Self::init_with_options_for_test`].
    #[cfg(any(test, feature = "test-helpers"))]
    #[hotpath::skip]
    pub async fn open_read_only_with_options_for_test(
        project_root: &Path,
        open_options: TraceDecayOpenOptions,
    ) -> Result<Self> {
        let open_options = Self::standalone_test_open_options(project_root, open_options);
        let runtime = Self::standalone_test_runtime(project_root, &open_options).await?;
        let mut graph = runtime
            .open_project_graph_read_only_for_test(project_root, open_options)
            .await?;
        graph.test_runtime_guard = Some(runtime);
        Ok(graph)
    }

    #[hotpath::measure(label = "lifecycle.mount_project_graph", future = true)]
    pub(super) async fn mount_project_graph(
        runtime: &DaemonSessionRuntimeRegistryV1,
        project_root: &Path,
        store_layout: &StoreLayout,
        operation: &'static str,
        access: DatabaseAccessMode,
    ) -> Result<Database> {
        let project_id = storage::registered_project_id(store_layout)?;
        let canonical_database_path = &store_layout.graph_db_path;
        if matches!(access, DatabaseAccessMode::ReadOnly) {
            return runtime
                .project_graph_registered(project_id, canonical_database_path.clone(), access)
                .await;
        }
        let authority = DatabaseAuthority::for_runtime(canonical_database_path, operation)?;
        runtime
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
    #[hotpath::skip]
    pub async fn init(project_root: &Path) -> Result<Self> {
        Self::init_with_options(project_root, TraceDecayOpenOptions::default()).await
    }

    #[hotpath::skip]
    pub async fn init_with_options(
        project_root: &Path,
        open_options: TraceDecayOpenOptions,
    ) -> Result<Self> {
        #[cfg(any(test, feature = "test-transport"))]
        {
            Self::init_with_options_for_test(project_root, open_options).await
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
    #[hotpath::measure(label = "lifecycle.init.exclusive", future = true)]
    pub async fn init_with_exclusive_maintenance(
        project_root: &Path,
        open_options: TraceDecayOpenOptions,
        lifecycle_lease: &tracedecay_runtime_core::lifecycle_lease::LifecycleLease,
    ) -> Result<Self> {
        let profile_root = open_options.resolved_profile_root()?;
        if let Some(message) =
            tracedecay_global_db::ephemeral_root_rejection(project_root, &profile_root)
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
        let identity = tracedecay_daemon_identity::profile_identity::load_or_create(&profile_root)?;
        let runtime_registry = join_standalone_session_registry(identity).await?;
        let profile_database = runtime_registry.profile_database().await?;
        let store_layout = Self::resolve_first_touch_configuration_layout(
            project_root,
            &open_options,
            profile_database.as_ref(),
        )
        .await?;
        let project_id = storage::registered_project_id(&store_layout)?;
        // Persist the minted identity in the sanctioned repo-adjacent anchor:
        // the `.git/` repository identity marker. A non-git root persists
        // nothing here — its identity is deterministic from the canonical
        // path and durably owned by the profile registry. TraceDecay never
        // creates files inside a project's working tree.
        tracedecay_runtime_core::storage::write_repository_identity_marker(
            project_root,
            project_id.as_str(),
        )?;
        let configuration_database = runtime_registry
            .project_sessions(project_id, vec![project_root.to_path_buf()])
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

    #[cfg(any(test, feature = "test-helpers"))]
    #[hotpath::skip]
    pub async fn init_test_fixture_with_registered_runtime(
        project_root: &Path,
        project_id: &str,
    ) -> Result<(
        Self,
        Arc<crate::test_support::host_admission::HostAdmissionTestRuntimeV1>,
    )> {
        let profile_root = tracedecay_runtime_core::storage::default_profile_root()?;
        let project_id = tracedecay_domain::ProjectId::new(project_id).map_err(|error| {
            TraceDecayError::Config {
                message: format!("invalid test fixture project identity: {error}"),
            }
        })?;
        let runtime = Arc::new(
            crate::test_support::host_admission::HostAdmissionTestRuntimeV1::project(
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

    #[hotpath::measure(label = "lifecycle.init.registered", future = true)]
    pub async fn init_with_registered_configuration(
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
        let (config, opened) = open_runtime_configuration_for_registered_database(
            project_root,
            &store_layout,
            configuration_database,
        )
        .await?
        .into_parts();
        let (configuration_runtime, _) = ProjectConfigurationRuntime::open(opened)?;
        let configuration_runtime = Arc::new(configuration_runtime);
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

        let ts = Self {
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
            #[cfg(any(test, feature = "test-helpers"))]
            test_runtime_guard: None,
            _standalone_maintenance_scope: None,
        };
        // First-touch parity with the registered open path: daemon warm-up
        // refuses to advertise an identity-bearing project whose Context
        // Scout owner is absent, so init must start it too.
        tracedecay_agent_hosts::hooks::publish_hook_bindings(
            &crate::runtime_ports::hook_runtime()?,
            &ts.store_layout,
        )?;
        if let Some(project_id) =
            tracedecay_agent_hosts::hooks::hook_project_id_for_layout(&ts.store_layout)
        {
            let _ = tracedecay_agent_hosts::agents::context_scout::owner::ProjectContextScoutOwnerV1::startup(
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

    #[hotpath::skip]
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

    #[hotpath::skip]
    async fn ensure_database_schema_current(db: &Database) -> Result<()> {
        let current = Self::schema_version(db, "ensure_schema_current").await?;
        let supported = tracedecay_runtime_core::db::migrations::SCHEMA_VERSION;
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
    #[hotpath::measure(label = "lifecycle.ensure_schema", future = true)]
    pub async fn ensure_schema_current(&self) -> Result<()> {
        Self::ensure_database_schema_current(&self.db).await
    }

    /// Opens an existing `TraceDecay` project at the given root.
    ///
    /// If branch metadata exists, resolves the current git branch's published
    /// provenance. Registered open admits only the exact final relational
    /// schema; code-index activation and reconciliation happen after open
    /// through the daemon-owned scheduler.
    #[hotpath::skip]
    pub async fn open(project_root: &Path) -> Result<Self> {
        Self::open_with_options(project_root, TraceDecayOpenOptions::default()).await
    }

    #[hotpath::skip]
    pub async fn open_with_options(
        project_root: &Path,
        open_options: TraceDecayOpenOptions,
    ) -> Result<Self> {
        #[cfg(any(test, feature = "test-transport"))]
        {
            Self::open_with_options_for_test(project_root, open_options).await
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
    #[hotpath::measure(label = "lifecycle.open.exclusive", future = true)]
    pub async fn open_with_exclusive_maintenance(
        project_root: &Path,
        open_options: TraceDecayOpenOptions,
        lifecycle_lease: &tracedecay_runtime_core::lifecycle_lease::LifecycleLease,
    ) -> Result<Self> {
        let profile_root = open_options.resolved_profile_root()?;
        if !lifecycle_lease.is_exclusive() || !lifecycle_lease.guards_profile(&profile_root) {
            return Err(TraceDecayError::Config {
                message: "project open requires the exact profile's exclusive lifecycle lease"
                    .to_owned(),
            });
        }
        let identity = tracedecay_daemon_identity::profile_identity::load_or_create(&profile_root)?;
        let runtime_registry = join_standalone_session_registry(identity).await?;
        let profile_database = runtime_registry.profile_database().await?;
        let store_layout = Self::resolve_registered_configuration_layout(
            project_root,
            &open_options,
            profile_database.as_ref(),
        )
        .await?;
        let project_id = storage::registered_project_id(&store_layout)?;
        let enrollment_roots = registered_enrollment_roots(
            profile_database.as_ref(),
            project_root,
            &store_layout,
            &project_id,
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

    #[hotpath::measure(label = "lifecycle.open.registered", future = true)]
    pub async fn open_with_registered_configuration(
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
        let (config, opened) = open_runtime_configuration_for_registered_database(
            project_root,
            &store_layout,
            configuration_database,
        )
        .await?
        .into_parts();
        let (configuration_runtime, _) = ProjectConfigurationRuntime::open(opened)?;
        let configuration_runtime = Arc::new(configuration_runtime);
        let ts = Self {
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
            #[cfg(any(test, feature = "test-helpers"))]
            test_runtime_guard: None,
            _standalone_maintenance_scope: None,
        };

        tracedecay_agent_hosts::hooks::publish_hook_bindings(
            &crate::runtime_ports::hook_runtime()?,
            &ts.store_layout,
        )?;
        if let Some(project_id) =
            tracedecay_agent_hosts::hooks::hook_project_id_for_layout(&ts.store_layout)
        {
            let _ = tracedecay_agent_hosts::agents::context_scout::owner::ProjectContextScoutOwnerV1::startup(
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
        let graph_scope = active_branch.clone().or_else(|| {
            tracedecay_runtime_core::worktree::detached_worktree_graph_scope(project_root)
        });
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
    #[hotpath::skip]
    pub async fn open_read_only(project_root: &Path) -> Result<Self> {
        Self::open_read_only_with_options(project_root, TraceDecayOpenOptions::default()).await
    }

    #[hotpath::skip]
    pub async fn open_read_only_with_options(
        project_root: &Path,
        open_options: TraceDecayOpenOptions,
    ) -> Result<Self> {
        #[cfg(any(test, feature = "test-transport"))]
        {
            Self::open_read_only_with_options_for_test(project_root, open_options).await
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
    #[hotpath::measure(label = "lifecycle.open_read_only.exclusive", future = true)]
    async fn open_read_only_with_exclusive_maintenance(
        project_root: &Path,
        open_options: TraceDecayOpenOptions,
        lifecycle_lease: &tracedecay_runtime_core::lifecycle_lease::LifecycleLease,
    ) -> Result<Self> {
        let profile_root = open_options.resolved_profile_root()?;
        if !lifecycle_lease.is_exclusive() || !lifecycle_lease.guards_profile(&profile_root) {
            return Err(TraceDecayError::Config {
                message:
                    "read-only project open requires the exact profile's exclusive lifecycle lease"
                        .to_owned(),
            });
        }
        let identity = tracedecay_daemon_identity::profile_identity::load_or_create(&profile_root)?;
        let runtime_registry = join_standalone_session_registry(identity).await?;
        let profile_database = runtime_registry.profile_database().await?;
        let store_layout = Self::resolve_registered_configuration_layout(
            project_root,
            &open_options,
            profile_database.as_ref(),
        )
        .await?;
        let project_id = storage::registered_project_id(&store_layout)?;
        let enrollment_roots = registered_enrollment_roots(
            profile_database.as_ref(),
            project_root,
            &store_layout,
            &project_id,
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

    #[hotpath::measure(label = "lifecycle.open_read_only.registered", future = true)]
    pub async fn open_read_only_with_registered_configuration(
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
        let (config, opened) = open_runtime_configuration_for_registered_database_read_only(
            project_root,
            &store_layout,
            configuration_database,
        )
        .await?
        .into_parts();
        let (configuration_runtime, _) = ProjectConfigurationRuntime::open(opened)?;
        let configuration_runtime = Arc::new(configuration_runtime);
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
            #[cfg(any(test, feature = "test-helpers"))]
            test_runtime_guard: None,
            _standalone_maintenance_scope: None,
        })
    }
}

#[cfg(any(test, feature = "test-helpers"))]
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
    use std::collections::{BTreeMap, BTreeSet};
    use tracedecay_agent_hosts::agents::context_scout::ports::{
        AdmittedContextScoutHookV1, ContextScoutAddressBindOutcomeV1, ContextScoutAuthorityPinV1,
        ContextScoutConfigurationPinV1, ContextScoutLifecycleAddressV1,
        ProjectContextScoutAddressRegistryV1,
    };
    use tracedecay_application::configuration::ConfigurationCurrentStateV1;
    use tracedecay_contracts::{
        CancellationContext, CapabilityGrantId, CapabilityGrantSnapshot, Deadline, DisclosureClass,
        RequestId, ResolvedScope,
    };
    use tracedecay_domain::canonical_sha256;
    use tracedecay_domain::configuration::{
        CONTEXT_SCOUT_SETTINGS_SETTING_KEY, CandidateDispositionV1, ConfigurationCandidateV1,
        ConfigurationLayerIdV1, ConfigurationRevisionId, ConfigurationSnapshotV1,
        ConfigurationValueV1, ContextScoutSettingsV1, SettingKey,
    };
    use tracedecay_domain::feedback::FeedbackScopeV1;
    use tracedecay_domain::{ActorId, RepositoryId, UtcMicros, WorktreeId};
    use tracedecay_hooks::{
        HookCapabilityV1, HookEventFamily, HookHostV1, HookScopeBindingV1,
        NativeEnvelopeMaterialV1, decode_bound_native_hook_event, stock_event_support,
    };
    use tracedecay_tool_catalog::{CapabilityId, UseCaseId};

    async fn mount_verified_reopen_claim(
        owner: &tracedecay_agent_hosts::agents::context_scout::owner::ProjectContextScoutOwnerV1,
    ) -> (
        tracedecay_agent_hosts::agents::context_scout::ports::ContextScoutLifecycleAddressV1,
        tracedecay_contracts::context_scout::ContextScoutAddressV1,
    ) {
        fn id<T: TryFrom<String>>(value: &str) -> T
        where
            T::Error: std::fmt::Debug,
        {
            T::try_from(value.to_owned()).unwrap()
        }

        let observed_at = UtcMicros(10);
        let project_id = id::<tracedecay_domain::ProjectId>("project.scout.fixture");
        let repository_id = id::<RepositoryId>("repository.scout.fixture");
        let worktree_id = id::<WorktreeId>("worktree.scout.fixture");
        let scope = ResolvedScope::new(
            project_id.clone(),
            repository_id.clone(),
            worktree_id.clone(),
            Some(id("refs/heads/main")),
        )
        .expect("scope");
        let grant = CapabilityGrantSnapshot::new(
            CapabilityGrantId::new("grant.scout.reopen").expect("grant"),
            1,
            canonical_sha256(&"scout.reopen").expect("digest"),
            ActorId::new("actor.scout.issuer").expect("issuer"),
            UtcMicros(1),
            UtcMicros(10_000),
            scope.clone(),
            BTreeSet::from([CapabilityId::new("capability.scout.reopen").expect("capability")]),
            BTreeSet::from([UseCaseId::new("use-case.scout.reopen").expect("use case")]),
            DisclosureClass::Evidence,
        )
        .expect("grant");
        let context = tracedecay_contracts::RequestContext::new(
            ActorId::new("actor.scout.requester").expect("actor"),
            scope,
            grant,
            RequestId::new("request.scout.reopen").expect("request"),
            Deadline::new(UtcMicros(10_000)).expect("deadline"),
            CancellationContext::active("cancel.scout.reopen").expect("cancel"),
        )
        .expect("request context");
        let setting_key = SettingKey::new(CONTEXT_SCOUT_SETTINGS_SETTING_KEY).expect("setting");
        let revision_id = ConfigurationRevisionId::new("revision.scout.reopen").expect("revision");
        let snapshot = ConfigurationSnapshotV1::new(
            BTreeMap::from([(
                setting_key.clone(),
                ConfigurationValueV1::ContextScoutSettings(ContextScoutSettingsV1::disabled()),
            )]),
            BTreeMap::from([(
                setting_key,
                vec![ConfigurationCandidateV1 {
                    layer: ConfigurationLayerIdV1::Project {
                        project_id: project_id.clone(),
                    },
                    revision_id: revision_id.clone(),
                    disposition: CandidateDispositionV1::Winning,
                    safe_reason: None,
                }],
            )]),
        )
        .expect("snapshot");
        let configuration =
            ContextScoutConfigurationPinV1::from_current(&ConfigurationCurrentStateV1 {
                revision_id,
                snapshot,
            })
            .expect("configuration pin");
        let pin = ContextScoutAuthorityPinV1::new(
            &context,
            FeedbackScopeV1 {
                project_id,
                repository_id,
                worktree_id,
                branch_ref: "refs/heads/main".to_owned(),
                head_commit_id: id("commit.scout.fixture"),
            },
            configuration,
            observed_at,
        )
        .expect("authority pin");
        let binding = HookScopeBindingV1 {
            host: HookHostV1::ClaudeCode,
            project_id: [1; 16],
            repository_id: [2; 16],
            worktree_id: [3; 16],
            worktree_epoch: 1,
            binding_token: [4; 32],
            capabilities: [
                HookEventFamily::SessionBoundary,
                HookEventFamily::PromptBoundary,
                HookEventFamily::ToolLifecycle,
                HookEventFamily::SavedEdit,
                HookEventFamily::TestLifecycle,
            ]
            .into_iter()
            .map(|family| HookCapabilityV1 {
                family,
                support: stock_event_support(HookHostV1::ClaudeCode, family),
            })
            .collect(),
        };
        let envelope = decode_bound_native_hook_event(
            HookHostV1::ClaudeCode,
            include_bytes!(
                "../../../../../tests/fixtures/packaged_host_events/claude/post_tool_use_write.json"
            ),
            &binding,
            NativeEnvelopeMaterialV1 {
                event_id: [5; 16],
                protected_session_id: [6; 32],
                observed_at,
                tool_id: Some([7; 16]),
                effect_receipt_id: Some([8; 16]),
                file_id: Some([9; 16]),
                changed_range_count: 1,
            },
        )
        .expect("hook envelope");
        let hook = AdmittedContextScoutHookV1::new(envelope, &binding).expect("admitted hook");
        let registry = ProjectContextScoutAddressRegistryV1::new(
            owner.store().database().clone(),
            id("project.scout.fixture"),
        )
        .expect("address registry");
        let lifecycle = ContextScoutLifecycleAddressV1 {
            profile_id: id("profile.scout.fixture"),
            provider_id: id("provider.claude"),
            project_id: id("project.scout.fixture"),
            worktree_id: id("worktree.scout.fixture"),
            session_id: id("session.scout.fixture"),
            thread_id: id("thread.scout.fixture"),
            turn_id: id("turn.scout.fixture"),
            agent_id: id("agent.scout.fixture"),
            logical_message_id: id("message.scout.reopen"),
        };
        let address = match registry.bind(&hook, &pin, lifecycle.clone()).await {
            ContextScoutAddressBindOutcomeV1::Bound(address)
            | ContextScoutAddressBindOutcomeV1::Existing(address) => address,
            other => panic!("expected bound reopen address, got {other:?}"),
        };
        assert_eq!(
            owner
                .mount_current_claim_authority(
                    registry,
                    &hook,
                    pin,
                    context,
                    lifecycle.clone(),
                    address,
                    [9; 32],
                    observed_at,
                    true,
                )
                .await,
            tracedecay_agent_hosts::agents::context_scout::owner::ContextScoutClaimAdmissionV1::Mounted,
            "one claim authority must be admissible before reopen"
        );
        (lifecycle, address)
    }

    #[tokio::test]
    async fn context_scout_owner_survives_branch_reopen() {
        let root = tempfile::TempDir::new().expect("fixture root");
        let project = root.path().join("project");
        let profile = root.path().join("profile");
        std::fs::create_dir_all(&project).expect("create project root");
        let options = TraceDecayOpenOptions {
            profile_root: Some(profile.clone()),
            global_db_path: Some(profile.join("registry.db")),
        };
        let opened = TraceDecay::init_with_options(&project, options)
            .await
            .expect("initialize project graph");
        let project_id =
            tracedecay_agent_hosts::hooks::hook_project_id_for_layout(opened.hook_store_layout())
                .expect("initialized project has hook identity");
        let owner = opened
            .context_scout_owner()
            .expect("Context Scout owner starts with the project");
        let registered =
            tracedecay_agent_hosts::agents::context_scout::owner::lookup_registered_context_scout_owners(
                project_id,
            );
        assert!(
            registered
                .iter()
                .any(|candidate| Arc::ptr_eq(candidate, &owner)),
            "startup must publish the owner into the process-global registry"
        );

        let (lifecycle, address) = mount_verified_reopen_claim(&owner).await;

        let reopened = opened
            .reopen_for_current_branch()
            .await
            .expect("reopen onto the live branch");
        let after = reopened
            .context_scout_owner()
            .expect("Context Scout owner must remain resolvable after branch reopen");
        assert!(
            Arc::ptr_eq(&owner, &after),
            "Context Scout owner is keyed by project identity, not by the TraceDecay instance"
        );
        assert_eq!(
            after.resolve_admitted_claim(&lifecycle).await,
            Some((address, [9; 32])),
            "mounted claim authority must remain resolvable after branch reopen"
        );
        let registered =
            tracedecay_agent_hosts::agents::context_scout::owner::lookup_registered_context_scout_owners(
                project_id,
            );
        assert_eq!(
            registered.len(),
            1,
            "one project identity has one Context Scout owner"
        );
        assert!(Arc::ptr_eq(&owner, &registered[0]));
    }

    #[tokio::test]
    async fn read_only_open_does_not_resolve_writable_context_scout_owner() {
        let root = tempfile::TempDir::new().expect("fixture root");
        let project = root.path().join("project");
        let profile = root.path().join("profile");
        std::fs::create_dir_all(&project).expect("create project root");
        let options = TraceDecayOpenOptions {
            profile_root: Some(profile.clone()),
            global_db_path: Some(profile.join("registry.db")),
        };
        let writable = TraceDecay::init_with_options(&project, options.clone())
            .await
            .expect("initialize writable project graph");
        assert!(
            writable.context_scout_owner().is_some(),
            "a writable open starts and registers the Context Scout owner"
        );
        let read_only = TraceDecay::open_read_only_with_options(&project, options)
            .await
            .expect("open the same project read-only");
        assert!(
            matches!(
                read_only.context_scout_owner_lookup(),
                crate::project::ContextScoutOwnerLookupV1::ReadOnly
            ),
            "a read-only open must not resolve the writable instance's owner"
        );
        assert!(
            read_only.context_scout_owner().is_none(),
            "read-only TraceDecay has no Context Scout owner"
        );
    }

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
        // Not `SCHEMA_VERSION - 1`: that stamp is
        // `PAYLOAD_DIGEST_STEP_SOURCE_VERSION`, the one sanctioned step this
        // binary carries forward in place, so an open of it upgrades instead
        // of refusing. Age the store one step past the sanctioned source.
        connection
            .pragma_update(
                None,
                "user_version",
                tracedecay_runtime_core::db::migrations::PAYLOAD_DIGEST_STEP_SOURCE_VERSION - 1,
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
