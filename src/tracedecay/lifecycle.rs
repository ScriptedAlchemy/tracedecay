//! Lifecycle: init/open/branch-tracking entry points plus the profile-store
//! registration helpers they rely on.

#[cfg(not(any(test, feature = "test-transport")))]
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
#[cfg(not(any(test, feature = "test-transport")))]
use std::sync::{LazyLock, Mutex, Weak};
use std::time::{SystemTime, UNIX_EPOCH};

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
use crate::global_db::{
    GraphScopeUpsert, RegisteredGlobalDb, StoreArtifactUpsert, StoreInstanceUpsert,
};
use crate::storage::{self, StoreLayout};
use tracedecay_code_extraction::LanguageRegistry;
use tracedecay_store::ProjectId;

use super::locking::{
    adopt_dirty_marker_at, dirty_marker_owner_is_live, has_dirty_sentinel_at,
    try_acquire_graph_sync_locks,
};
use super::{TraceDecay, TraceDecayOpenOptions, current_timestamp};

#[cfg(not(any(test, feature = "test-transport")))]
static STANDALONE_MAINTENANCE_SCOPES: LazyLock<
    Mutex<HashMap<PathBuf, Weak<crate::db::OwnedMaintenanceDatabaseScope>>>,
> = LazyLock::new(|| Mutex::new(HashMap::new()));

impl TraceDecay {
    pub(super) fn registered_project_id(store_layout: &StoreLayout) -> Result<ProjectId> {
        let project_id =
            store_layout
                .identity
                .project_id
                .as_ref()
                .ok_or_else(|| TraceDecayError::Config {
                    message: "registered code runtime requires an authoritative project identity"
                        .to_owned(),
                })?;
        ProjectId::new(project_id.clone()).map_err(|error| TraceDecayError::Config {
            message: format!("invalid registered project identity: {error}"),
        })
    }

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

    async fn resolve_store_layout_for_project(
        project_root: &Path,
        open_options: &TraceDecayOpenOptions,
    ) -> Result<StoreLayout> {
        Self::resolve_store_layout_with_identity_migration(
            project_root,
            open_options,
            true,
            None,
            true,
        )
        .await
    }

    pub(crate) async fn resolve_registered_configuration_layout(
        project_root: &Path,
        open_options: &TraceDecayOpenOptions,
        registry_database: &RegisteredGlobalDb,
        allow_repair: bool,
    ) -> Result<StoreLayout> {
        Self::resolve_store_layout_with_identity_migration(
            project_root,
            open_options,
            allow_repair,
            Some(registry_database),
            false,
        )
        .await
    }

    /// Resolves the store layout for a project that has never been enrolled,
    /// minting a fresh path-derived profile-sharded identity so first-touch
    /// `init` can bootstrap it under the daemon's authority.
    ///
    /// This differs from [`Self::resolve_registered_configuration_layout`] only
    /// in that a project with no enrollment marker, registry match, or legacy
    /// shard falls through to a default identity instead of failing closed.
    /// Ambiguous or conflicting *existing* stores still surface their own
    /// identity-cutover errors from [`Self::choose_identity_layout`] and never
    /// reach the default-identity branch, so this never masks a real conflict.
    pub(crate) async fn resolve_first_touch_configuration_layout(
        project_root: &Path,
        open_options: &TraceDecayOpenOptions,
        registry_database: &RegisteredGlobalDb,
        allow_repair: bool,
    ) -> Result<StoreLayout> {
        Self::resolve_store_layout_with_identity_migration(
            project_root,
            open_options,
            allow_repair,
            Some(registry_database),
            true,
        )
        .await
    }

    /// Candidate enrollment roots a registered project claims: its canonical
    /// and display roots plus every registered alias.
    pub(crate) fn registry_context_candidate_roots(
        context: &crate::global_db::ProjectRegistryContext,
    ) -> Vec<PathBuf> {
        let mut candidates = vec![
            PathBuf::from(&context.project.canonical_root),
            PathBuf::from(&context.project.display_root),
        ];
        candidates.extend(
            context
                .aliases
                .iter()
                .map(|alias| PathBuf::from(&alias.alias_path)),
        );
        candidates
    }

    /// Filters candidate roots down to the ones that already carry a
    /// profile-sharded enrollment marker naming exactly `project_id`.
    ///
    /// This never creates or repairs a marker, so a caller that must not mount
    /// a store the profile has not enrolled — a cross-project memory reader,
    /// for one — can tell "not enrolled here" apart from "enrolled".
    pub(crate) fn enrolled_project_roots(
        candidates: impl IntoIterator<Item = PathBuf>,
        project_id: &ProjectId,
    ) -> Result<Vec<PathBuf>> {
        let mut candidates = candidates.into_iter().collect::<Vec<_>>();
        candidates.sort();
        candidates.dedup();

        let mut roots = Vec::new();
        for candidate in candidates {
            let candidate =
                crate::worktree::repository_identity_root(&candidate).unwrap_or(candidate);
            let Ok(canonical) = candidate.canonicalize() else {
                continue;
            };
            if roots.contains(&canonical) {
                continue;
            }
            let Some(marker) = storage::read_enrollment_marker(&canonical)? else {
                continue;
            };
            if marker.storage_mode == storage::StorageMode::ProfileSharded
                && marker.project_id == project_id.as_str()
            {
                roots.push(canonical);
            }
        }
        Ok(roots)
    }

    pub(crate) async fn registered_enrollment_roots(
        project_root: &Path,
        store_layout: &StoreLayout,
        project_id: &ProjectId,
        registry_database: &RegisteredGlobalDb,
    ) -> Result<Vec<PathBuf>> {
        let mut candidates = vec![
            project_root.to_path_buf(),
            store_layout.project_root.clone(),
        ];
        if let Some(context) = registry_database
            .project_registry_context_by_id(project_id.as_str())
            .await?
        {
            candidates.extend(Self::registry_context_candidate_roots(&context));
        }

        let mut roots = Self::enrolled_project_roots(candidates, project_id)?;
        if roots.is_empty() {
            let enrollment_root = crate::worktree::repository_identity_root(project_root)
                .unwrap_or_else(|| project_root.to_path_buf());
            let canonical =
                enrollment_root
                    .canonicalize()
                    .map_err(|error| TraceDecayError::Config {
                        message: format!(
                            "could not canonicalize project enrollment root '{}': {error}",
                            enrollment_root.display()
                        ),
                    })?;
            storage::write_enrollment_marker(
                &canonical,
                &storage::EnrollmentMarker {
                    project_id: project_id.as_str().to_owned(),
                    storage_mode: storage::StorageMode::ProfileSharded,
                },
            )?;
            roots.push(canonical);
        }
        Ok(roots)
    }

    async fn resolve_store_layout_with_identity_migration(
        project_root: &Path,
        open_options: &TraceDecayOpenOptions,
        allow_repair: bool,
        registry_database: Option<&RegisteredGlobalDb>,
        allow_default_identity: bool,
    ) -> Result<StoreLayout> {
        let profile_root = open_options.resolved_profile_root()?;
        if storage::read_enrollment_marker(project_root)?.is_some() {
            return storage::resolve_persisted_layout(project_root, &profile_root)?.ok_or_else(
                || TraceDecayError::Config {
                    message: "enrollment marker did not resolve a profile store".to_string(),
                },
            );
        }

        let mut selected = storage::resolve_persisted_layout(project_root, &profile_root)?;
        // Every linked worktree resolves through its repository, attached or
        // not; suppressing this for detached worktrees dropped them onto the
        // path-hashed identity fallback and minted a duplicate store.
        let git_common_dir = crate::worktree::git_common_dir(project_root);
        if selected.is_none()
            && let Some(registry_database) = registry_database
            && let Some(resolution) = registry_database
                .resolve_project_store_by_identity(project_root, git_common_dir.as_deref())
                .await?
        {
            selected = Some(storage::profile_sharded_layout(
                project_root,
                &profile_root,
                &storage::EnrollmentMarker {
                    project_id: resolution.project.project_id,
                    storage_mode: storage::StorageMode::ProfileSharded,
                },
            )?);
        }

        let selected_id = selected
            .as_ref()
            .and_then(|layout| layout.identity.project_id.as_deref());
        // Store inventory opens graph and session databases, so keep it behind
        // the rare paths that compare actual stores rather than every resolve.
        let (candidates, selected_is_sole_exact_root) =
            storage::matching_legacy_profile_layouts(project_root, &profile_root, selected_id)?;
        let selected = Self::choose_identity_layout(
            project_root,
            selected,
            candidates,
            selected_is_sole_exact_root,
            allow_repair,
        )
        .await?;
        match selected {
            Some(layout) => Ok(layout),
            None if allow_default_identity => {
                storage::default_profile_sharded_layout(project_root, &profile_root)
            }
            None => Err(TraceDecayError::Config {
                message:
                    "registered configuration layout requires an enrolled or registry-resolved project identity"
                        .to_owned(),
            }),
        }
    }

    async fn choose_identity_layout(
        project_root: &Path,
        selected: Option<StoreLayout>,
        candidates: Vec<StoreLayout>,
        selected_is_sole_exact_root: bool,
        allow_repair: bool,
    ) -> Result<Option<StoreLayout>> {
        // With no competing candidate the selected layout wins without an
        // inventory read.
        if selected_is_sole_exact_root
            && !candidates.is_empty()
            && let Some(selected) = selected.as_ref()
        {
            let selected_inventory = store_identity_inventory(selected).await;
            if selected_inventory.is_healthy() && !selected_inventory.is_pristine() {
                return Ok(Some(selected.clone()));
            }
        }
        if candidates.len() > 1 {
            let mut details = Vec::new();
            for candidate in &candidates {
                details.push(store_identity_inventory(candidate).await.to_string());
            }
            return Err(TraceDecayError::Config {
                message: format!(
                    "ambiguous legacy profile stores for '{}': {}; no files changed",
                    project_root.display(),
                    details.join("; ")
                ),
            });
        }
        let Some(candidate) = candidates.into_iter().next() else {
            return Ok(selected);
        };
        let Some(selected) = selected else {
            return Ok(Some(candidate));
        };

        let selected_inventory = store_identity_inventory(&selected).await;
        let candidate_inventory = store_identity_inventory(&candidate).await;
        let manifest_matches_project_root = |layout: &StoreLayout| {
            let manifest_path = layout.manifest_path.as_deref()?;
            let manifest = storage::read_store_manifest(manifest_path).ok()?;
            Some(
                manifest.project_root == project_root
                    || match (
                        manifest.project_root.canonicalize(),
                        project_root.canonicalize(),
                    ) {
                        (Ok(manifest_root), Ok(project_root)) => manifest_root == project_root,
                        _ => false,
                    },
            )
        };
        if manifest_matches_project_root(&candidate) == Some(true)
            && manifest_matches_project_root(&selected) == Some(false)
            && candidate_inventory.is_healthy()
            && !candidate_inventory.is_pristine()
            && selected_inventory.is_healthy()
            && !selected_inventory.is_pristine()
        {
            return Ok(Some(candidate));
        }
        if selected_inventory.is_pristine() && candidate_inventory.is_healthy() {
            if !allow_repair {
                return Err(identity_cutover_conflict(
                    project_root,
                    &selected_inventory,
                    &candidate_inventory,
                    "safe empty-store repair is available during a writable open",
                ));
            }
            let candidate_id = candidate.identity.project_id.as_deref().ok_or_else(|| {
                TraceDecayError::Config {
                    message: "legacy candidate has no project id".to_string(),
                }
            })?;
            storage::write_repository_identity_marker(project_root, candidate_id)?;
            storage::retire_identity_cutover_manifest(&selected)?;
            return Ok(Some(candidate));
        }
        if candidate_inventory.is_pristine() && selected_inventory.is_healthy() {
            if allow_repair {
                let selected_id = selected.identity.project_id.as_deref().ok_or_else(|| {
                    TraceDecayError::Config {
                        message: "selected store has no project id".to_string(),
                    }
                })?;
                storage::write_repository_identity_marker(project_root, selected_id)?;
                storage::retire_identity_cutover_manifest(&candidate)?;
            }
            return Ok(Some(selected));
        }
        let command =
            consolidation_dry_run_command(project_root, &candidate_inventory, &selected_inventory);
        Err(identity_cutover_conflict(
            project_root,
            &selected_inventory,
            &candidate_inventory,
            &format!("run the offline dry-run `{command}` before changing the marker"),
        ))
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
        let marker_is_abandoned =
            |path: &Path| has_dirty_sentinel_at(path) && !dirty_marker_owner_is_live(path);
        let crashed = marker_is_abandoned(&active_graph_layout.dirty_path)
            || marker_is_abandoned(&store_layout.dirty_path);
        let mut crash_preflight_healthy = false;
        if crashed {
            eprintln!(
                "[tracedecay] previous operation was interrupted — checking database integrity…"
            );
        }

        // A dirty marker can also describe a sync that is still active in a
        // peer process. Recovery must own both graph-local and legacy locks so
        // it cannot race that writer or clear its sentinel. Preflight through
        // the read-only connection before Database::open applies writable
        // pragmas or migrations to a potentially damaged recovery set.
        let recovery_lock = if crashed {
            Some(try_acquire_graph_sync_locks(
                &active_graph_layout.sync_lock_path,
                &store_layout.sync_lock_path,
            )?)
        } else {
            None
        };
        // Recovery owns exactly the markers it observed once the lease was
        // held. A marker republished after this point describes a newer
        // writer's work and must survive this recovery, so the clear below is
        // scoped to these adopted epochs rather than to whatever the paths
        // happen to hold at commit time.
        let mut adopted_dirty_markers = Vec::new();
        if crashed {
            adopted_dirty_markers.extend(adopt_dirty_marker_at(&active_graph_layout.dirty_path));
            if active_graph_layout.dirty_path != store_layout.dirty_path {
                adopted_dirty_markers.extend(adopt_dirty_marker_at(&store_layout.dirty_path));
            }
        }
        // SQLite may rewrite a WAL-index sidecar while opening a recovery set,
        // even when the subsequent integrity check rejects the main database.
        // Preserve an obviously invalid derived store before any SQLite handle
        // can alter the forensic copy.
        if repair_corrupt_branch
            && matches!(
                crate::storage::has_sqlite_database_header(&db_path),
                Ok(false)
            )
        {
            drop(recovery_lock);
            return Self::recover_corrupt_branch_or_fail(
                project_root,
                open_options,
                &store_layout,
                &db_path,
                "invalid SQLite database header",
                repair_corrupt_branch,
                configuration_database,
                profile_database,
                runtime_registry,
            )
            .await;
        }
        if crashed {
            // FTS-only damage is repairable from the content table on the
            // writable open below; do not force offline recovery for it. The
            // read-only open runs its own integrity validation, so the damage
            // can surface either as its open error or as a problem row here.
            match Self::mount_worktree_graph(
                runtime_registry.as_ref(),
                project_root,
                &store_layout,
                &db_path,
                mounted_graph_scope.as_deref(),
                "crash verification",
                DatabaseAccessMode::ReadOnly,
            )
            .await
            {
                Ok(verification) => {
                    let integrity = verification.quick_check_report().await;
                    verification.close();
                    match integrity {
                        Ok(None) => crash_preflight_healthy = true,
                        Ok(Some(problem)) if is_fts_only_corruption(&problem) => {}
                        Ok(Some(problem)) => {
                            drop(recovery_lock);
                            return Self::recover_corrupt_branch_or_fail(
                                project_root,
                                open_options,
                                &store_layout,
                                &db_path,
                                format!("read-only SQLite quick_check reported: {problem}"),
                                repair_corrupt_branch,
                                configuration_database,
                                profile_database,
                                runtime_registry,
                            )
                            .await;
                        }
                        Err(error) => {
                            drop(recovery_lock);
                            return Self::recover_corrupt_branch_or_fail(
                                project_root,
                                open_options,
                                &store_layout,
                                &db_path,
                                error,
                                repair_corrupt_branch,
                                configuration_database,
                                profile_database,
                                runtime_registry,
                            )
                            .await;
                        }
                    }
                }
                Err(error) if is_fts_only_corruption(&error.to_string()) => {}
                // A hot rollback journal from an interrupted writer needs
                // write access to recover, so the read-only preflight cannot
                // open it at all. That is normal crash recovery, not damage:
                // defer to the writable open below, which rolls the journal
                // back and still runs the post-open quick_check.
                Err(error) if is_readonly_recovery_block(&error.to_string()) => {}
                Err(error) => {
                    drop(recovery_lock);
                    return Self::recover_corrupt_branch_or_fail(
                        project_root,
                        open_options,
                        &store_layout,
                        &db_path,
                        error,
                        repair_corrupt_branch,
                        configuration_database,
                        profile_database,
                        runtime_registry,
                    )
                    .await;
                }
            }
        }

        // Ordinary opens never replace database files. A daemon or another MCP
        // process may still hold the current DB/WAL/SHM inodes, and deleting
        // them here would split readers and writers across different stores.
        let mut open_result = Self::mount_worktree_graph(
            runtime_registry.as_ref(),
            project_root,
            &store_layout,
            &db_path,
            mounted_graph_scope.as_deref(),
            "open project store",
            DatabaseAccessMode::ReadWrite,
        )
        .await;
        // Open-time validation fails closed on any corruption, including
        // FTS-only damage that is fully derivable from the content table.
        // Rebuild that index under the open's writer authority and retry
        // once; stores corrupted by a live writer carry no dirty sentinel,
        // so this repair cannot be gated on the crash path.
        if let Err(error) = &open_result
            && is_fts_only_corruption(&error.to_string())
        {
            eprintln!("[tracedecay] repairing FTS index after interrupted operation ({error})…");
            match Self::mount_worktree_graph(
                runtime_registry.as_ref(),
                project_root,
                &store_layout,
                &db_path,
                mounted_graph_scope.as_deref(),
                "remount project store for FTS repair",
                DatabaseAccessMode::ReadWrite,
            )
            .await
            {
                Ok(database) => match database.repair_fts_after_open().await {
                    Ok(_) => open_result = Ok(database),
                    Err(repair_error) => {
                        database.close();
                        drop(recovery_lock);
                        return Self::recover_corrupt_branch_or_fail(
                            project_root,
                            open_options,
                            &store_layout,
                            &db_path,
                            repair_error,
                            repair_corrupt_branch,
                            configuration_database,
                            profile_database,
                            runtime_registry,
                        )
                        .await;
                    }
                },
                Err(repair_error) => {
                    drop(recovery_lock);
                    return Self::recover_corrupt_branch_or_fail(
                        project_root,
                        open_options,
                        &store_layout,
                        &db_path,
                        repair_error,
                        repair_corrupt_branch,
                        configuration_database,
                        profile_database,
                        runtime_registry,
                    )
                    .await;
                }
            }
        }
        let db = match open_result {
            Ok(database) => database,
            Err(e) if Database::is_corruption_error(&e) || crashed => {
                drop(recovery_lock);
                return Self::recover_corrupt_branch_or_fail(
                    project_root,
                    open_options,
                    &store_layout,
                    &db_path,
                    e,
                    repair_corrupt_branch,
                    configuration_database,
                    profile_database,
                    runtime_registry,
                )
                .await;
            }
            Err(e) => return Err(e),
        };
        let migrated = crate::db::migrations::migrate(&db).await?;

        // Validation before Database::open cannot observe FTS damage on a
        // retained shared handle because the open reuses that connection.
        // Classify its complete quick-check report after open and schedule the
        // existing rebuild through the canonical writer lane. Non-FTS damage
        // fails closed without entering either repair path.
        // The crash preflight already ran the same complete quick-check while
        // holding both recovery locks. Repeating it after the writable mount
        // doubles peak SQLite scratch memory without adding evidence. Ordinary
        // opens still run this retained-handle check so live-writer FTS damage
        // without a dirty marker remains detectable.
        if !crash_preflight_healthy && !defer_post_open_health {
            match db.repair_fts_after_open().await {
                Ok(Some(problem)) => {
                    eprintln!(
                        "[tracedecay] repaired FTS index after post-open health check ({problem})"
                    );
                }
                Ok(None) => {}
                Err(error) => {
                    db.close();
                    drop(recovery_lock);
                    return Self::recover_corrupt_branch_or_fail(
                        project_root,
                        open_options,
                        &store_layout,
                        &db_path,
                        error,
                        repair_corrupt_branch,
                        configuration_database,
                        profile_database,
                        runtime_registry,
                    )
                    .await;
                }
            }
        }

        if crashed && crash_preflight_healthy {
            for marker in &adopted_dirty_markers {
                marker.clear();
            }
        }

        // If the sentinel was set but the read-only preflight could not prove
        // the database healthy, validate the writable recovery before clearing
        // either marker.
        if crashed && !crash_preflight_healthy {
            let mut integrity = db.quick_check_report().await;
            // An interrupted bulk load can desync the FTS5 inverted index from
            // its content table. That damage is fully derivable: rebuild it in
            // place under the held recovery locks instead of failing closed.
            if let Ok(Some(problem)) = &integrity
                && is_fts_only_corruption(problem)
            {
                eprintln!(
                    "[tracedecay] repairing FTS index after interrupted operation ({problem})…"
                );
                match db.rebuild_fts().await {
                    Ok(()) => integrity = db.quick_check_report().await,
                    Err(error) => {
                        db.close();
                        drop(recovery_lock);
                        return Self::recover_corrupt_branch_or_fail(
                            project_root,
                            open_options,
                            &store_layout,
                            &db_path,
                            error,
                            repair_corrupt_branch,
                            configuration_database,
                            profile_database,
                            runtime_registry,
                        )
                        .await;
                    }
                }
            }
            match integrity {
                Ok(None) => {
                    for marker in &adopted_dirty_markers {
                        marker.clear();
                    }
                }
                Ok(Some(problem)) => {
                    db.close();
                    drop(recovery_lock);
                    return Self::recover_corrupt_branch_or_fail(
                        project_root,
                        open_options,
                        &store_layout,
                        &db_path,
                        format!("SQLite quick_check reported: {problem}"),
                        repair_corrupt_branch,
                        configuration_database,
                        profile_database,
                        runtime_registry,
                    )
                    .await;
                }
                Err(e) => {
                    db.close();
                    drop(recovery_lock);
                    return Self::recover_corrupt_branch_or_fail(
                        project_root,
                        open_options,
                        &store_layout,
                        &db_path,
                        e,
                        repair_corrupt_branch,
                        configuration_database,
                        profile_database,
                        runtime_registry,
                    )
                    .await;
                }
            }
        }

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

    async fn recover_corrupt_branch_or_fail(
        project_root: &Path,
        open_options: TraceDecayOpenOptions,
        store_layout: &StoreLayout,
        db_path: &Path,
        detail: impl std::fmt::Display,
        repair_corrupt_branch: bool,
        configuration_database: Arc<RegisteredGlobalDb>,
        profile_database: Arc<RegisteredGlobalDb>,
        runtime_registry: Arc<DaemonSessionRuntimeRegistryV1>,
    ) -> Result<Self> {
        let detail = detail.to_string();
        if repair_corrupt_branch {
            if let Err(close_error) = runtime_registry
                .close_code_graph_paths([db_path.to_path_buf()])
                .await
            {
                print_corruption_warning(db_path);
                return Err(recovery_required_error(
                    db_path,
                    format!(
                        "{detail}; automatic derived-branch repair could not retire the registered runtime before replacing the database: {close_error}"
                    ),
                ));
            }
            let active_graph_layout = active_graph_layout(db_path);
            let repair_result = (|| {
                let _sync_locks = try_acquire_graph_sync_locks(
                    &active_graph_layout.sync_lock_path,
                    &store_layout.sync_lock_path,
                )?;
                let _authority =
                    DatabaseAuthority::for_runtime(db_path, "preserve corrupt branch store")?;
                preserve_corrupt_branch_store(store_layout, db_path)
            })();

            match repair_result {
                Ok(recovery_dir) => {
                    eprintln!(
                        "[tracedecay] corrupt derived branch index preserved at '{}' — rebuilding from a healthy tracked ancestor",
                        recovery_dir.display()
                    );
                    return Box::pin(Self::open_with_registered_configuration_inner(
                        project_root,
                        open_options,
                        store_layout.clone(),
                        configuration_database,
                        profile_database,
                        runtime_registry,
                        false,
                        false,
                    ))
                    .await;
                }
                Err(repair_error) => {
                    print_corruption_warning(db_path);
                    return Err(recovery_required_error(
                        db_path,
                        format!("{detail}; automatic derived-branch repair failed: {repair_error}"),
                    ));
                }
            }
        }

        print_corruption_warning(db_path);
        Err(recovery_required_error(db_path, detail))
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

    pub(crate) async fn register_project_store_in_global_registry(&self) -> Result<()> {
        static REGISTRY_WRITE_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

        if self.store_layout.storage_mode != storage::StorageMode::ProfileSharded {
            return Ok(());
        }

        let project_id = self
            .store_layout
            .identity
            .project_id
            .as_deref()
            .ok_or_else(|| {
                registry_registration_error("profile-sharded store has no project identity")
            })?;
        let profile_root = profile_root_for_layout(&self.store_layout)
            .ok_or_else(|| registry_registration_error("store is outside a profile root"))?;
        let store_relpath = profile_relative(&profile_root, &self.store_layout.data_root)
            .ok_or_else(|| registry_registration_error("store root is outside its profile"))?;

        let _registry_write = REGISTRY_WRITE_LOCK.lock().await;

        let global_db = self.profile_database.as_ref();

        let meta = branch_meta::load_branch_meta(&self.store_layout.data_root);
        let default_branch = meta.as_ref().map(|meta| meta.default_branch.as_str());
        // Registering without the git common dir leaves the row unreachable
        // by repository identity, so the next first touch from a sibling
        // checkout mints a fresh store. Detached worktrees are no exception:
        // they belong to the same repository as every other checkout.
        let git_common_dir = crate::worktree::git_common_dir(&self.project_root);
        let git_remote_url = git_remote_url(&self.project_root);

        // A shared project id can be reached from any linked worktree (see
        // the git-common-dir alias registered below), so registering
        // straight from `self.project_root` would let whichever worktree
        // happens to touch the project last pin its canonical_root /
        // display_root to a transient worktree path. Redirect registration
        // to the primary checkout when one is detected and still exists.
        let primary_root = crate::project_registry::primary_checkout_root(
            &self.project_root,
            git_common_dir.as_deref(),
        );
        let previous_canonical_root = if primary_root.is_some() {
            global_db
                .get_code_project(project_id)
                .await
                .map(|record| record.canonical_root)
        } else {
            None
        };
        let registration_root = primary_root.as_deref().unwrap_or(&self.project_root);

        let project = global_db
            .upsert_code_project(
                project_id,
                registration_root,
                git_common_dir.as_deref(),
                git_remote_url.as_deref(),
                default_branch,
            )
            .await
            .ok_or_else(|| registry_registration_error("upsert code project failed"))?;

        storage::write_repository_identity_marker(&self.project_root, &project.project_id)?;

        if let Some(primary_root) = primary_root.as_deref() {
            // The registry now points canonical_root/display_root at the
            // primary checkout; keep this worktree itself resolvable for
            // future lookups by registering its own path as an alias.
            global_db
                .upsert_project_alias(&self.project_root, &project.project_id)
                .await
                .ok_or_else(|| registry_registration_error("upsert worktree alias failed"))?;

            let repaired_stale_worktree_root = previous_canonical_root.is_some_and(|previous| {
                previous != RegisteredGlobalDb::canonical_project_key(primary_root)
            });
            if repaired_stale_worktree_root {
                eprintln!(
                    "warning: repaired tracedecay project '{project_id}' canonical_root — \
                     it was pinned to a linked worktree ({}); restored to the primary checkout ({})",
                    self.project_root.display(),
                    primary_root.display()
                );
            }
        }

        let store_id = profile_store_id(&project.project_id);
        let manifest_relpath = self
            .store_layout
            .manifest_path
            .as_ref()
            .and_then(|path| profile_relative(&profile_root, path));
        let now = current_timestamp();
        let store = global_db
            .upsert_store_instance(StoreInstanceUpsert {
                store_id,
                project_id: project.project_id,
                store_kind: "code_project".to_string(),
                storage_mode: "profile_sharded".to_string(),
                store_relpath,
                manifest_relpath,
                last_verified_at: Some(now),
                last_write_at: Some(now),
            })
            .await
            .ok_or_else(|| registry_registration_error("upsert store instance failed"))?;

        if let Some(meta) = meta {
            for (branch_name, entry) in meta.branches {
                let db_path = self.store_layout.data_root.join(&entry.db_file);
                let db_relpath = profile_relative(&profile_root, &db_path).ok_or_else(|| {
                    registry_registration_error("branch database is outside its profile")
                })?;
                global_db
                    .upsert_graph_scope(GraphScopeUpsert {
                        graph_scope_id: profile_graph_scope_id(&store.store_id, &branch_name),
                        project_id: store.project_id.clone(),
                        store_id: store.store_id.clone(),
                        branch_name: branch_name.clone(),
                        db_relpath,
                        parent_scope_id: entry
                            .parent
                            .as_deref()
                            .map(|parent| profile_graph_scope_id(&store.store_id, parent)),
                        last_synced_at: entry.last_synced_at.parse::<i64>().ok(),
                        writable: true,
                    })
                    .await
                    .ok_or_else(|| registry_registration_error("upsert graph scope failed"))?;
            }
        }

        let mut artifacts = Vec::new();
        push_existing_store_artifact(
            &mut artifacts,
            &store.store_id,
            "graph_db",
            &profile_root,
            &self.store_layout.graph_db_path,
            None,
            now,
        );
        push_existing_store_artifact(
            &mut artifacts,
            &store.store_id,
            "sessions_db",
            &profile_root,
            &self.store_layout.sessions_db_path,
            None,
            now,
        );
        push_existing_store_artifact(
            &mut artifacts,
            &store.store_id,
            "branch_meta",
            &profile_root,
            &self.store_layout.branch_meta_path,
            None,
            now,
        );
        if let Some(manifest_path) = &self.store_layout.manifest_path {
            push_existing_store_artifact(
                &mut artifacts,
                &store.store_id,
                "store_manifest",
                &profile_root,
                manifest_path,
                Some(storage::STORE_MANIFEST_SCHEMA_VERSION.to_string()),
                now,
            );
        }
        for artifact in artifacts {
            global_db
                .upsert_store_artifact(artifact)
                .await
                .ok_or_else(|| registry_registration_error("upsert store artifact failed"))?;
        }
        Ok(())
    }

    /// Returns `true` if a `TraceDecay` project has been initialized at the given root.
    pub fn is_initialized(project_root: &Path) -> bool {
        Self::is_initialized_with_options(project_root, &TraceDecayOpenOptions::default())
    }

    pub fn is_initialized_with_options(
        project_root: &Path,
        open_options: &TraceDecayOpenOptions,
    ) -> bool {
        let option_resolved_store_exists = open_options
            .resolved_profile_root()
            .and_then(|profile_root| crate::storage::resolve_layout(project_root, &profile_root))
            .is_ok_and(|layout| {
                layout.storage_mode == crate::storage::StorageMode::ProfileSharded
                    && layout.graph_db_path.exists()
            });
        if open_options.profile_root.is_some() || open_options.global_db_path.is_some() {
            return option_resolved_store_exists;
        }
        option_resolved_store_exists
            || crate::config::has_project_database(project_root)
            || crate::storage::has_enrollment_marker(project_root)
    }

    pub async fn has_initialized_store(project_root: &Path) -> bool {
        Self::has_initialized_store_with_options(project_root, &TraceDecayOpenOptions::default())
            .await
    }

    pub async fn has_initialized_store_with_options(
        project_root: &Path,
        open_options: &TraceDecayOpenOptions,
    ) -> bool {
        Self::initialized_store_layout_with_options(project_root, open_options)
            .await
            .is_some()
    }

    /// Resolves the store layout for a project using the same registry/alias
    /// aware path as [`Self::has_initialized_store`], returning it only when
    /// the resolved store's graph database actually exists.
    pub async fn initialized_store_layout_with_options(
        project_root: &Path,
        open_options: &TraceDecayOpenOptions,
    ) -> Option<StoreLayout> {
        Self::try_initialized_store_layout_with_options(project_root, open_options)
            .await
            .ok()
            .flatten()
    }

    /// Resolves an initialized store without discarding identity conflicts or
    /// other storage errors. User-facing diagnostics must use this variant so
    /// a preserved split store is never mislabeled as uninitialized.
    pub async fn try_initialized_store_layout_with_options(
        project_root: &Path,
        open_options: &TraceDecayOpenOptions,
    ) -> Result<Option<StoreLayout>> {
        let layout =
            Self::resolve_store_layout_for_local_identity(project_root, open_options).await?;
        Ok(layout.graph_db_path.is_file().then_some(layout))
    }

    /// Resolves the profile store layout for a local path using enrollment
    /// markers first, then the global registry aliases for the git identity.
    pub async fn resolve_store_layout_for_identity(project_root: &Path) -> Result<StoreLayout> {
        Self::resolve_store_layout_for_identity_with_options(
            project_root,
            &TraceDecayOpenOptions::default(),
        )
        .await
    }

    pub async fn resolve_store_layout_for_identity_with_options(
        project_root: &Path,
        open_options: &TraceDecayOpenOptions,
    ) -> Result<StoreLayout> {
        Self::resolve_store_layout_for_local_identity(project_root, open_options).await
    }

    async fn resolve_store_layout_for_local_identity(
        project_root: &Path,
        open_options: &TraceDecayOpenOptions,
    ) -> Result<StoreLayout> {
        Self::resolve_store_layout_with_identity_migration(
            project_root,
            open_options,
            false,
            None,
            true,
        )
        .await
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

fn graph_sidecar_path(db_path: &Path, suffix: &str) -> PathBuf {
    let mut file_name = db_path.file_name().unwrap_or_default().to_os_string();
    file_name.push(suffix);
    db_path.with_file_name(file_name)
}

fn preserve_corrupt_branch_store(store_layout: &StoreLayout, db_path: &Path) -> Result<PathBuf> {
    let db_name = db_path.file_name().ok_or_else(|| TraceDecayError::Config {
        message: format!(
            "cannot preserve corrupt branch store with no filename: '{}'",
            db_path.display()
        ),
    })?;
    let recovery_root = store_layout.data_root.join("recovery");
    std::fs::create_dir_all(&recovery_root).map_err(|error| TraceDecayError::Config {
        message: format!(
            "failed to create branch recovery directory '{}': {error}",
            recovery_root.display()
        ),
    })?;
    let recovery_dir = recovery_root.join(format!(
        "{}-{}-{}",
        db_name.to_string_lossy(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos(),
        std::process::id()
    ));
    std::fs::create_dir(&recovery_dir).map_err(|error| TraceDecayError::Config {
        message: format!(
            "failed to create branch recovery set '{}': {error}",
            recovery_dir.display()
        ),
    })?;

    let db_wal = graph_sidecar_path(db_path, "-wal");
    let db_shm = graph_sidecar_path(db_path, "-shm");
    let db_dirty = graph_sidecar_path(db_path, ".dirty");
    let sources = [&db_wal, &db_shm, &db_dirty, db_path];
    let mut preserved_db = false;
    let mut preserved = Vec::new();
    for source in sources {
        let metadata = match std::fs::symlink_metadata(source) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => {
                return Err(TraceDecayError::Config {
                    message: format!(
                        "failed to inspect recovery-set member '{}': {error}",
                        source.display()
                    ),
                });
            }
        };
        if !metadata.file_type().is_file() {
            return Err(TraceDecayError::Config {
                message: format!(
                    "refusing to preserve non-regular recovery-set member '{}'",
                    source.display()
                ),
            });
        }
        let target = recovery_dir.join(source.file_name().unwrap_or_default());
        let copied = std::fs::copy(source, &target).map_err(|error| TraceDecayError::Config {
            message: format!(
                "failed to preserve recovery-set member '{}' at '{}': {error}",
                source.display(),
                target.display()
            ),
        })?;
        if copied != metadata.len() {
            return Err(TraceDecayError::Config {
                message: format!(
                    "incomplete recovery-set copy for '{}': copied {copied} of {} bytes",
                    source.display(),
                    metadata.len()
                ),
            });
        }
        // Windows FlushFileBuffers requires a write handle.
        std::fs::OpenOptions::new()
            .write(true)
            .open(&target)
            .and_then(|file| file.sync_all())
            .map_err(|error| TraceDecayError::Config {
                message: format!(
                    "failed to sync preserved recovery-set member '{}': {error}",
                    target.display()
                ),
            })?;
        preserved_db |= source == db_path;
        preserved.push(source.to_path_buf());
    }
    if !preserved_db {
        return Err(TraceDecayError::Config {
            message: format!(
                "corrupt branch database '{}' disappeared",
                db_path.display()
            ),
        });
    }
    #[cfg(unix)]
    for directory in [&recovery_dir, &recovery_root, &store_layout.data_root] {
        std::fs::File::open(directory)
            .and_then(|directory| directory.sync_all())
            .map_err(|error| TraceDecayError::Config {
                message: format!(
                    "failed to sync preserved recovery-set directory '{}': {error}",
                    directory.display()
                ),
            })?;
    }

    // Retire the source database last. A complete copied recovery set remains
    // available if any source-side cleanup fails.
    for source in preserved {
        std::fs::remove_file(&source).map_err(|error| TraceDecayError::Config {
            message: format!(
                "preserved recovery set at '{}', but failed to retire '{}': {error}",
                recovery_dir.display(),
                source.display()
            ),
        })?;
    }
    Ok(recovery_dir)
}

fn active_graph_layout(db_path: &Path) -> super::ActiveGraphLayout {
    super::ActiveGraphLayout {
        dirty_path: graph_sidecar_path(db_path, ".dirty"),
        sync_lock_path: graph_sidecar_path(db_path, ".sync.lock"),
    }
}

#[derive(Debug)]
struct StoreIdentityInventory {
    project_id: String,
    data_root: PathBuf,
    graph_health: &'static str,
    nodes: u64,
    files: u64,
    facts: u64,
    sessions: u64,
    messages: u64,
    lcm_rows: u64,
    branches: usize,
    automation_files: u64,
    payload_files: u64,
    response_files: u64,
}

impl StoreIdentityInventory {
    fn is_healthy(&self) -> bool {
        self.graph_health == "healthy"
    }

    fn is_pristine(&self) -> bool {
        self.is_healthy()
            && self.nodes == 0
            && self.files == 0
            && self.facts == 0
            && self.sessions == 0
            && self.messages == 0
            && self.lcm_rows == 0
            && self.branches <= 1
            && self.automation_files == 0
            && self.payload_files == 0
            && self.response_files == 0
    }
}

impl std::fmt::Display for StoreIdentityInventory {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "project_id={} path='{}' graph_health={} nodes={} files={} facts={} sessions={} messages={} lcm={} branches={} automation_files={} payload_files={} response_files={}",
            self.project_id,
            self.data_root.display(),
            self.graph_health,
            self.nodes,
            self.files,
            self.facts,
            self.sessions,
            self.messages,
            self.lcm_rows,
            self.branches,
            self.automation_files,
            self.payload_files,
            self.response_files,
        )
    }
}

async fn store_identity_inventory(layout: &StoreLayout) -> StoreIdentityInventory {
    let scratch_root = layout.data_root.join("scratch").join("sqlite-read");
    let open_result = match storage::PrivateStoreIo::create_dir_all(&scratch_root) {
        Ok(()) => crate::sqlite_read_snapshot::open_in(&layout.graph_db_path, &scratch_root).await,
        Err(error) => Err(error),
    };
    let (graph_health, nodes, files, facts) = match open_result {
        Ok(snapshot) => {
            let connection = snapshot.connection();
            let healthy = quick_check_ok(connection).await && snapshot.validate_source().is_ok();
            if healthy {
                (
                    "healthy",
                    count_rows(connection, "nodes").await,
                    count_rows(connection, "files").await,
                    count_rows(connection, "memory_facts").await,
                )
            } else {
                ("corrupt", 0, 0, 0)
            }
        }
        Err(_) if layout.graph_db_path.exists() => ("corrupt", 0, 0, 0),
        Err(_) => ("missing", 0, 0, 0),
    };

    let (sessions, messages, lcm_rows) =
        match storage::PrivateStoreIo::create_dir_all(&scratch_root) {
            Ok(()) => {
                match crate::sqlite_read_snapshot::open_in(&layout.sessions_db_path, &scratch_root)
                    .await
                {
                    Ok(snapshot) => {
                        let connection = snapshot.connection();
                        let counts = (
                            count_rows(connection, "sessions").await,
                            count_rows(connection, "session_messages").await,
                            count_rows(connection, "lcm_raw_messages").await
                                + count_rows(connection, "lcm_summary_nodes").await,
                        );
                        if snapshot.validate_source().is_ok() {
                            counts
                        } else {
                            (0, 0, 0)
                        }
                    }
                    Err(_) => (0, 0, 0),
                }
            }
            Err(_) => (0, 0, 0),
        };

    StoreIdentityInventory {
        project_id: layout
            .identity
            .project_id
            .clone()
            .unwrap_or_else(|| "unknown".to_string()),
        data_root: layout.data_root.clone(),
        graph_health,
        nodes,
        files,
        facts,
        sessions,
        messages,
        lcm_rows,
        branches: branch_meta::load_branch_meta(&layout.data_root)
            .map_or(0, |meta| meta.branches.len()),
        automation_files: count_tree_files(&layout.dashboard_root),
        payload_files: count_tree_files(&layout.lcm_payload_root),
        response_files: count_tree_files(&layout.response_handle_root),
    }
}

async fn quick_check_ok(connection: &(impl crate::db::engine::QueryExecutor + ?Sized)) -> bool {
    let Ok(mut rows) = connection.query("PRAGMA quick_check", ()).await else {
        return false;
    };
    rows.next()
        .await
        .ok()
        .flatten()
        .and_then(|row| row.get::<String>(0).ok())
        .is_some_and(|result| result == "ok")
}

async fn count_rows(
    connection: &(impl crate::db::engine::QueryExecutor + ?Sized),
    table: &str,
) -> u64 {
    let Ok(mut rows) = connection
        .query(&format!("SELECT COUNT(*) FROM {table}"), ())
        .await
    else {
        return 0;
    };
    rows.next()
        .await
        .ok()
        .flatten()
        .and_then(|row| row.get::<i64>(0).ok())
        .and_then(|count| u64::try_from(count).ok())
        .unwrap_or(0)
}

fn count_tree_files(root: &Path) -> u64 {
    let Ok(entries) = std::fs::read_dir(root) else {
        return 0;
    };
    entries
        .flatten()
        .map(|entry| entry.path())
        .map(|path| match std::fs::symlink_metadata(&path) {
            Ok(metadata) if metadata.is_file() => 1,
            Ok(metadata) if metadata.is_dir() => count_tree_files(&path),
            _ => 0,
        })
        .sum()
}

/// Whether a `PRAGMA quick_check` problem row describes damage confined to the
/// graph's FTS5 index (e.g. "malformed inverted index for FTS5 table
/// `main.nodes_fts`"). Such damage is fully derivable from the content table via
/// [`crate::db::Database::rebuild_fts`] and never requires offline recovery.
pub(crate) fn is_fts_only_corruption(problem: &str) -> bool {
    problem.contains("malformed inverted index for FTS5 table main.nodes_fts")
        || problem.contains("malformed inverted index for FTS5 table nodes_fts")
        || (problem.contains("fts5: corruption found") && problem.contains("nodes_fts"))
}

/// Whether a read-only preflight failure means the store needs ordinary
/// writable crash recovery (e.g. a hot rollback journal), which a read-only
/// connection can never perform, rather than actual damage.
fn is_readonly_recovery_block(problem: &str) -> bool {
    problem.contains("attempt to write a readonly database")
}

fn identity_cutover_conflict(
    project_root: &Path,
    selected: &StoreIdentityInventory,
    legacy: &StoreIdentityInventory,
    action: &str,
) -> TraceDecayError {
    TraceDecayError::Config {
        message: format!(
            "identity cutover conflict for '{}': selected [{}]; legacy [{}]; {action}; both shards were preserved and no files changed",
            project_root.display(),
            selected,
            legacy
        ),
    }
}

fn consolidation_dry_run_command(
    project_root: &Path,
    source: &StoreIdentityInventory,
    target: &StoreIdentityInventory,
) -> String {
    format!(
        "tracedecay migrate consolidate --project {} --source-project-id {} --target-project-id {}",
        shell_quote(&project_root.to_string_lossy()),
        source.project_id,
        target.project_id,
    )
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\"'\"'"))
}

fn profile_relative(profile_root: &Path, path: &Path) -> Option<String> {
    path.strip_prefix(profile_root)
        .ok()
        .map(|rel| rel.to_string_lossy().replace('\\', "/"))
}

fn profile_root_for_layout(layout: &StoreLayout) -> Option<PathBuf> {
    layout.data_root.parent()?.parent().map(Path::to_path_buf)
}

fn profile_store_id(project_id: &str) -> String {
    format!("store:{project_id}:profile_sharded")
}

fn registry_registration_error(message: impl Into<String>) -> TraceDecayError {
    TraceDecayError::Database {
        operation: "register project store".to_string(),
        message: message.into(),
    }
}

pub(crate) fn git_remote_url(project_root: &Path) -> Option<String> {
    // gix reads the same config `git config --get` would (repo-local +
    // global) without a subprocess spawn.
    if let Ok(repo) = gix::discover(project_root) {
        let url = repo
            .config_snapshot()
            .string("remote.origin.url")?
            .to_string();
        let url = url.trim();
        return (!url.is_empty()).then(|| url.to_string());
    }
    if !crate::worktree::git_may_resolve_repo(project_root) {
        return None;
    }
    crate::git::git_capture(project_root, &["config", "--get", "remote.origin.url"])
}

fn profile_graph_scope_id(store_id: &str, branch_name: &str) -> String {
    format!("{store_id}:branch:{branch_name}")
}

fn push_existing_store_artifact(
    artifacts: &mut Vec<StoreArtifactUpsert>,
    store_id: &str,
    artifact_kind: &str,
    profile_root: &Path,
    path: &Path,
    schema_version: Option<String>,
    updated_at: i64,
) {
    let Some(relpath) = profile_relative(profile_root, path) else {
        return;
    };
    let Ok(metadata) = std::fs::metadata(path) else {
        return;
    };
    artifacts.push(StoreArtifactUpsert {
        store_id: store_id.to_string(),
        artifact_kind: artifact_kind.to_string(),
        relpath,
        size_bytes: i64::try_from(metadata.len()).ok(),
        schema_version,
        updated_at: Some(updated_at),
    });
}

/// Build an actionable error without replacing any member of the `SQLite`
/// recovery set.
fn recovery_required_error(
    db_path: &std::path::Path,
    detail: impl std::fmt::Display,
) -> TraceDecayError {
    TraceDecayError::Database {
        message: format!(
            "database recovery required at '{}'; DB/WAL/SHM and dirty sentinel were preserved: {detail}",
            db_path.display()
        ),
        operation: "open_recovery_required".to_string(),
    }
}

fn print_corruption_warning(db_path: &std::path::Path) {
    let version = env!("CARGO_PKG_VERSION");
    eprintln!("[tracedecay] \x1b[33m⚠ database recovery required — store preserved\x1b[0m");
    eprintln!("[tracedecay]");
    eprintln!("[tracedecay] Store: {}", db_path.display());
    eprintln!("[tracedecay] Stop TraceDecay daemon/MCP processes before explicit repair.");
    eprintln!("[tracedecay] Preserve the DB, WAL, SHM, and dirty sentinel as one recovery set.");
    eprintln!("[tracedecay] Run `tracedecay doctor` from the project root for exact paths.");
    eprintln!("[tracedecay] Please report this at:");
    eprintln!("[tracedecay]   https://github.com/ScriptedAlchemy/tracedecay/issues");
    eprintln!(
        "[tracedecay]   Include: tracedecay version (v{version}), OS, and what happened before the crash."
    );
    eprintln!("[tracedecay]");
}
