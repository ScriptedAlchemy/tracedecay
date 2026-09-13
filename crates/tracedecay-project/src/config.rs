use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock, OnceLock, RwLock};

use tracedecay_contracts::clock::now_micros;
use tracedecay_domain::ProjectId;
use tracedecay_domain::configuration::{
    CodeIndexWorkerSelectionV1, ConfigurationLayerIdV1, ConfigurationRevisionId,
    ConfigurationSnapshotV1, ConfigurationValueV1, SOURCE_BINDINGS_SETTING_KEY, SettingKey,
    UserProfileId,
};

use tracedecay_configuration::{
    SyncConfig, TelemetryConfig, TraceDecayConfig, get_config_path, is_in_gitignore,
    load_config_from_path,
};
use tracedecay_domain::errors::{Result, TraceDecayError};
use tracedecay_global_db::configuration::contracts::ports::ConfigurationControlStore;
use tracedecay_global_db::configuration::contracts::types::ConfigurationError;
use tracedecay_global_db::configuration::{
    GlobalDbConfigurationControlStore, ProfileCodeIndexWorkerConfigurationStore,
    ProfileCodeIndexWorkerConfigurationV1,
};
use tracedecay_global_db::{RegisteredGlobalDb, RegisteredGlobalDbLeaseV1};

pub use tracedecay_application::config::retrieval;
pub use tracedecay_global_db::configuration::{registry, resolver};

/// Kernel-owned path primitives. The definitions live in
/// `tracedecay_runtime_core::config` because the storage layout, database,
/// branch-metadata, and store layers depend on them and moved into that crate;
/// re-exporting here keeps every `crate::config::<item>` path intact.
pub use tracedecay_runtime_core::config::{
    DB_FILENAME, TRACEDECAY_DIR, USER_DATA_DIR_ENV, active_data_dir_name, db_filename,
    discover_project_root, get_project_db_path, get_tracedecay_dir, has_project_database,
    is_ambient_project_root, user_data_dir,
};

/// Atomic project-scoped semantic runtime selection.
///
/// The value is canonical JSON for [`SemanticConfig`]. Keeping the active
/// profile, rollback profile, and local resource ceilings under one setting
/// prevents a configuration revision from exposing a partially updated
/// semantic selection.
pub use tracedecay_domain::configuration::SEMANTIC_RUNTIME_SETTING_KEY;

/// The shared generated/vendored segment list and its membership test moved
/// into `tracedecay_runtime_core::config`: the extracted migration inventory
/// scanner consults them and cannot reach back into the root crate.
/// Re-exported so every historical `crate::config::<item>` path keeps
/// resolving.
pub use tracedecay_runtime_core::config::{GENERATED_DIR_SEGMENTS, is_generated_dir_segment};

/// Typed project route for the configuration daemon boundary. The path is
/// display/routing context only; [`ProjectId`] remains the authority key.
pub use tracedecay_configuration::config::RuntimeConfigurationTarget;

/// A complete resolved configuration pinned to one revision before a runtime
/// component starts. No caller may re-read mutable legacy input after holding
/// this value.
///
/// The shared runtime settings live in the embedded
/// [`tracedecay_configuration::config::PinnedRuntimeConfiguration`], which is
/// the one validated snapshot/revision binding; the composition root only
/// layers its daemon-only policy on top. Both are materialized once at
/// construction, so the fields stay private: there is no way to hold this
/// value with settings that disagree with its snapshot.
#[derive(Clone, Debug)]
pub struct DaemonRuntimeConfiguration {
    runtime: tracedecay_configuration::config::PinnedRuntimeConfiguration,
    config: TraceDecayConfig,
}

impl DaemonRuntimeConfiguration {
    /// Materializes the legacy runtime shape from a complete typed snapshot.
    /// The conversion rejects missing or wrongly typed settings rather than
    /// adding adapter-local defaults.
    pub fn new(
        target: RuntimeConfigurationTarget,
        revision_id: ConfigurationRevisionId,
        snapshot: ConfigurationSnapshotV1,
    ) -> Result<Self> {
        Self::from_runtime(
            tracedecay_configuration::config::PinnedRuntimeConfiguration::new(
                target,
                revision_id,
                snapshot,
            )?,
        )
    }

    /// Layers the daemon-only settings over an already validated runtime pin.
    /// Shared settings are taken from the pin, never decoded a second time.
    #[hotpath::measure(label = "daemon.config.materialize")]
    pub fn from_runtime(
        runtime: tracedecay_configuration::config::PinnedRuntimeConfiguration,
    ) -> Result<Self> {
        let config = TraceDecayConfig::from_runtime(&runtime)?;
        Ok(Self { runtime, config })
    }

    pub fn into_runtime(self) -> tracedecay_configuration::config::PinnedRuntimeConfiguration {
        self.runtime
    }

    pub fn target(&self) -> &RuntimeConfigurationTarget {
        self.runtime.target()
    }

    pub fn revision_id(&self) -> &ConfigurationRevisionId {
        self.runtime.revision_id()
    }

    pub fn snapshot(&self) -> &ConfigurationSnapshotV1 {
        self.runtime.snapshot()
    }

    pub fn config(&self) -> &TraceDecayConfig {
        &self.config
    }

    pub fn into_config(self) -> TraceDecayConfig {
        self.config
    }

    /// Splits the daemon runtime shape from the runtime pin the configuration
    /// control plane retains.
    pub fn into_parts(
        self,
    ) -> (
        TraceDecayConfig,
        tracedecay_configuration::config::PinnedRuntimeConfiguration,
    ) {
        (self.config, self.runtime)
    }

    /// The same revision and settings routed under another root of the same
    /// registered project. Only the non-authoritative route and the legacy
    /// `root_dir` metadata change; nothing is decoded again.
    fn with_project_root(mut self, project_root: &Path) -> Self {
        self.runtime = self.runtime.with_project_root(project_root);
        self.config.root_dir = project_root.to_string_lossy().to_string();
        self
    }
}

/// Process-local, immutable-after-publication lookup cache. The daemon owns
/// refreshing it when a configuration revision activates; hook paths only
/// perform an in-memory lookup.
#[derive(Default)]
pub struct RuntimeConfigurationCache {
    by_project: RwLock<BTreeMap<String, DaemonRuntimeConfiguration>>,
    project_by_root: RwLock<BTreeMap<PathBuf, String>>,
}

impl RuntimeConfigurationCache {
    pub fn insert(&self, configuration: DaemonRuntimeConfiguration) {
        let project_id = configuration.target().project_id.as_str().to_owned();
        let project_root = configuration.target().project_root.clone();
        self.by_project
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(project_id.clone(), configuration);
        self.project_by_root
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(project_root, project_id);
    }

    pub fn for_project(&self, project_id: &ProjectId) -> Result<DaemonRuntimeConfiguration> {
        self.by_project
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(project_id.as_str())
            .cloned()
            .ok_or_else(|| {
                config_error(format!(
                    "configuration authority unavailable: no pinned resolved snapshot for project '{}'",
                    project_id.as_str()
                ))
            })
    }

    pub fn for_root(&self, project_root: &Path) -> Result<DaemonRuntimeConfiguration> {
        let project_id = self
            .project_by_root
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(project_root)
            .cloned()
            .ok_or_else(|| {
                config_error(format!(
                    "configuration authority unavailable: no pinned resolved snapshot for '{}'",
                    project_root.display()
                ))
            })?;
        let configuration = self
            .by_project
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&project_id)
            .cloned()
            .ok_or_else(|| {
                config_error(
                    "configuration authority unavailable: runtime snapshot cache is inconsistent",
                )
            })?;
        Ok(configuration.with_project_root(project_root))
    }
}

impl tracedecay_dashboard_api::config::DashboardConfigurationReadPort
    for RuntimeConfigurationCache
{
    fn cached_runtime_configuration(
        &self,
        project_root: &Path,
    ) -> Result<tracedecay_dashboard_api::config::PinnedRuntimeConfiguration> {
        Ok(self.for_root(project_root)?.into_runtime())
    }

    fn is_in_gitignore(&self, project_root: &Path) -> bool {
        is_in_gitignore(project_root)
    }
}

fn runtime_configuration_cache() -> &'static Arc<RuntimeConfigurationCache> {
    static CACHE: OnceLock<Arc<RuntimeConfigurationCache>> = OnceLock::new();
    CACHE.get_or_init(|| Arc::new(RuntimeConfigurationCache::default()))
}

/// Installs the root-owned configuration cache as the dashboard's read port.
pub fn install_dashboard_configuration_read_port() -> Result<()> {
    tracedecay_dashboard_api::config::install_dashboard_configuration_read_port(
        runtime_configuration_cache().clone(),
    )
}

/// Publishes one daemon-resolved snapshot for runtime and hook consumers.
pub fn install_pinned_runtime_configuration(configuration: DaemonRuntimeConfiguration) {
    runtime_configuration_cache().insert(configuration);
}

/// Builds a typed target from a resolved store layout. A missing project ID is
/// never replaced by a path-derived identity.
pub fn runtime_configuration_target_for_layout(
    project_root: &Path,
    layout: &tracedecay_runtime_core::storage::StoreLayout,
) -> Result<RuntimeConfigurationTarget> {
    let project_id = layout.identity.project_id.as_deref().ok_or_else(|| {
        config_error("configuration authority unavailable: store layout has no project id")
    })?;
    runtime_configuration_target_for_project_id(project_root, project_id)
}

/// Builds a typed configuration target from an already-authoritative project
/// ID. The path remains non-authoritative routing context.
pub fn runtime_configuration_target_for_project_id(
    project_root: &Path,
    project_id: &str,
) -> Result<RuntimeConfigurationTarget> {
    Ok(RuntimeConfigurationTarget {
        project_id: ProjectId::new(project_id.to_owned()).map_err(|error| {
            config_error(format!("invalid project id for configuration: {error}"))
        })?,
        project_root: project_root.to_path_buf(),
    })
}

/// Returns the pinned configuration for an exact authoritative layout.
///
/// This is fail-closed: callers that must not invent authority (hooks,
/// destructive branch administration) use it after the daemon has published a
/// snapshot. Daemon project-open paths that need to cold-start a process use
/// [`open_runtime_configuration_for_registered_database`] instead.
pub fn runtime_configuration_for_layout(
    project_root: &Path,
    layout: &tracedecay_runtime_core::storage::StoreLayout,
) -> Result<DaemonRuntimeConfiguration> {
    let target = runtime_configuration_target_for_layout(project_root, layout)?;
    let configuration = runtime_configuration_cache()
        .for_project(&target.project_id)?
        .with_project_root(&target.project_root);
    runtime_configuration_cache().insert(configuration.clone());
    Ok(configuration)
}

/// Resolves the pinned runtime configuration for a daemon-side operation on a
/// live project, pinning it on demand from the durable configuration store when
/// the process-local snapshot cache is cold.
///
/// Unlike [`runtime_configuration_for_layout`], which is fail-closed for hook
/// paths that must never invent authority, this is the daemon authority path:
/// the daemon owns the durable configuration store, so a registered project that
/// simply has not been opened in this process (a first operation, or the first
/// after a daemon restart) is resolved and pinned rather than rejected. It never
/// consults legacy `config.json` input. A cold cache adopts the durable current
/// revision through the same canonical open path as project open, so a fresh
/// store mints the sole canonical initial revision instead of failing; an
/// initialized-but-unreadable store still yields a typed authority error rather
/// than a fabricated default authority.
pub async fn resolve_runtime_configuration_for_registered_database(
    project_root: &Path,
    layout: &tracedecay_runtime_core::storage::StoreLayout,
    database: RegisteredGlobalDbLeaseV1,
) -> Result<DaemonRuntimeConfiguration> {
    let target = runtime_configuration_target_for_layout(project_root, layout)?;
    validate_registered_configuration_database(&target, database.as_ref())?;
    if let Ok(configuration) = runtime_configuration_cache().for_project(&target.project_id) {
        // The cache already holds a daemon-published pin (possibly a migrated
        // durable revision). Retarget it to this operation's non-authoritative
        // route and keep the fast path; do not reopen the store.
        let configuration = configuration.with_project_root(&target.project_root);
        runtime_configuration_cache().insert(configuration.clone());
        return Ok(configuration);
    }
    // Cold cache: adopt the durable current revision through the canonical
    // open path and publish it. A fresh store mints the canonical initial
    // revision — the daemon owns this store, and branch administration must
    // run for a registered project it has not opened yet — while an
    // initialized-but-unreadable store surfaces a typed authority error.
    Ok(
        open_runtime_configuration_for_registered_database(project_root, layout, database)
            .await?
            .configuration,
    )
}

/// Retained store handle paired with the exact revision resolved at project
/// open. Daemon composition consumes this bundle instead of opening a second
/// configuration database or resolving a second snapshot.
pub struct OpenedRuntimeConfiguration {
    pub configuration: DaemonRuntimeConfiguration,
    /// Exact daemon-owned registered session runtime used to resolve this
    /// snapshot. Configuration composition retains this authority directly;
    /// it never reacquires the physical database by path.
    pub registered_database: RegisteredGlobalDbLeaseV1,
}

impl OpenedRuntimeConfiguration {
    /// Splits the daemon runtime shape from the bundle the configuration
    /// control plane retains (runtime pin plus the exact registered store).
    pub fn into_parts(
        self,
    ) -> (
        TraceDecayConfig,
        tracedecay_configuration::config::OpenedRuntimeConfiguration,
    ) {
        let (config, runtime) = self.configuration.into_parts();
        (
            config,
            tracedecay_configuration::config::OpenedRuntimeConfiguration::new(
                runtime,
                self.registered_database,
            ),
        )
    }
}

/// Root-owned pin cache behind the lower crate's
/// [`tracedecay_configuration::config::PinnedRuntimeConfigurationCachePort`].
/// Publication layers the daemon-only settings over the published runtime pin
/// once; a cached read hands the embedded runtime pin back without decoding.
struct RootPinnedRuntimeConfigurationCache;

impl tracedecay_configuration::config::PinnedRuntimeConfigurationCachePort
    for RootPinnedRuntimeConfigurationCache
{
    fn publish(
        &self,
        configuration: tracedecay_configuration::config::PinnedRuntimeConfiguration,
    ) -> Result<()> {
        install_pinned_runtime_configuration(DaemonRuntimeConfiguration::from_runtime(
            configuration,
        )?);
        Ok(())
    }

    fn cached_for_root(
        &self,
        project_root: &Path,
    ) -> Result<tracedecay_configuration::config::PinnedRuntimeConfiguration> {
        Ok(cached_runtime_configuration(project_root)?.into_runtime())
    }
}

/// Installs the root-owned configuration read ports the lower crates reach
/// through their process-global slots: the pin cache and the dashboard
/// configuration reader. Idempotent.
pub fn install_usecase_runtime_configuration_authority() -> Result<()> {
    static INSTALLATION: LazyLock<std::result::Result<(), String>> = LazyLock::new(|| {
        tracedecay_configuration::config::install_pinned_runtime_configuration_cache(Arc::new(
            RootPinnedRuntimeConfigurationCache,
        ))
        .map_err(|error| error.to_string())?;
        install_dashboard_configuration_read_port().map_err(|error| error.to_string())
    });
    INSTALLATION
        .as_ref()
        .map_err(|message| config_error(message.clone()))
        .copied()
}

/// Loads and publishes the durable current configuration for a resolved store
/// layout.
///
/// A fresh project receives one canonical registry-backed revision.
/// Once any revision exists, open always reads that durable current revision;
/// a corrupt or ambiguous history is never replaced with local defaults.
#[hotpath::measure(label = "daemon.config.open", future = true)]
pub async fn open_runtime_configuration_for_registered_database(
    project_root: &Path,
    layout: &tracedecay_runtime_core::storage::StoreLayout,
    database: RegisteredGlobalDbLeaseV1,
) -> Result<OpenedRuntimeConfiguration> {
    let target = runtime_configuration_target_for_layout(project_root, layout)?;
    validate_registered_configuration_database(&target, database.as_ref())?;
    let store = GlobalDbConfigurationControlStore::new_registered(database.as_ref());
    let configuration = open_runtime_configuration_from_store(target, &store).await?;
    Ok(OpenedRuntimeConfiguration {
        configuration,
        registered_database: database,
    })
}

/// Resolve the daemon-wide worker selection from the exact registered
/// `ProfileSessions` authority, initializing only a genuinely fresh profile
/// store from the canonical registry default.
pub async fn read_or_initialize_profile_code_index_worker_selection(
    database: RegisteredGlobalDbLeaseV1,
    profile_id: &UserProfileId,
) -> Result<CodeIndexWorkerSelectionV1> {
    read_or_initialize_profile_code_index_worker_configuration(database, profile_id)
        .await
        .map(|configuration| configuration.selection)
}

#[hotpath::measure(label = "daemon.config.profile_workers.read", future = true)]
pub async fn read_or_initialize_profile_code_index_worker_configuration(
    database: RegisteredGlobalDbLeaseV1,
    profile_id: &UserProfileId,
) -> Result<ProfileCodeIndexWorkerConfigurationV1> {
    let store =
        ProfileCodeIndexWorkerConfigurationStore::new_registered(database.as_ref(), profile_id)
            .map_err(map_configuration_error)?;
    store
        .read_or_initialize(now_micros())
        .await
        .map_err(map_configuration_error)
}

/// Runs only against an uninitialized store; the initial revision publishes
/// exactly the daemon-owned project source binding.
async fn initialize_canonical_project_configuration(
    store: &GlobalDbConfigurationControlStore<'_>,
    target: &RuntimeConfigurationTarget,
) -> Result<()> {
    let registry = registry::ConfigurationRegistry::core()
        .map_err(|error| config_error(format!("configuration registry unavailable: {error}")))?;
    let target_layer = ConfigurationLayerIdV1::Project {
        project_id: target.project_id.clone(),
    };
    let initial_revision_id = ConfigurationRevisionId::new("configuration.initial.canonical.v1")
        .map_err(|error| {
            config_error(format!("invalid initial configuration revision: {error}"))
        })?;
    let daemon_binding =
        tracedecay_configuration::config::scope_control::daemon_owned_project_source_binding(
            &target.project_id,
            &target.project_root,
        )
        .map_err(|error| {
            config_error(format!(
                "daemon project source binding could not be derived: {error}"
            ))
        })?;
    let source_bindings_key = SettingKey::new(SOURCE_BINDINGS_SETTING_KEY)
        .map_err(|error| config_error(format!("invalid source bindings setting key: {error}")))?;
    let resolution = resolver::resolve_configuration(
        &registry,
        &[resolver::ConfigurationLayerV1 {
            layer: target_layer,
            revision_id: initial_revision_id.clone(),
            entries: BTreeMap::from([(
                source_bindings_key,
                ConfigurationValueV1::SourceBindings(vec![daemon_binding]),
            )]),
        }],
    )
    .map_err(|error| {
        config_error(format!(
            "canonical configuration initialization could not resolve: {error}"
        ))
    })?;
    store
        .initialize_canonical(&initial_revision_id, &resolution, now_micros())
        .await
        .map_err(map_configuration_error)
}

#[expect(
    clippy::too_many_lines,
    reason = "Open converges the durable current revision and verifies the daemon-owned source binding before any caller sees the pin."
)]
async fn open_runtime_configuration_from_store(
    target: RuntimeConfigurationTarget,
    store: &GlobalDbConfigurationControlStore<'_>,
) -> Result<DaemonRuntimeConfiguration> {
    if let Err(error) = store.current().await {
        if !store
            .is_uninitialized()
            .await
            .map_err(map_configuration_error)?
        {
            return Err(map_configuration_error(error));
        }
        initialize_canonical_project_configuration(store, &target).await?;
    }
    let daemon_binding =
        tracedecay_configuration::config::scope_control::daemon_owned_project_source_binding(
            &target.project_id,
            &target.project_root,
        )
        .map_err(|error| {
            config_error(format!(
                "daemon project source binding could not be derived: {error}"
            ))
        })?;
    let current = store.current().await.map_err(map_configuration_error)?;
    let mut current = match store
        .converge_registered_additive_defaults(&current.revision_id, now_micros())
        .await
    {
        Ok(state) => state,
        Err(ConfigurationError::RevisionConflict) => {
            store.current().await.map_err(map_configuration_error)?
        }
        Err(error) => return Err(map_configuration_error(error)),
    };
    let source_bindings_key = SettingKey::new(SOURCE_BINDINGS_SETTING_KEY)
        .map_err(|error| config_error(format!("invalid source bindings setting key: {error}")))?;
    enum SourceBindingCheck {
        Verified,
        LocatorDigestDrift,
        Mismatch,
    }
    let mut rebind_attempted = false;
    loop {
        let check = {
            let Some(ConfigurationValueV1::SourceBindings(configured_bindings)) =
                current.snapshot.effective_values.get(&source_bindings_key)
            else {
                return Err(TraceDecayError::reset_required(
                    "configuration",
                    "canonical configuration source bindings are missing",
                ));
            };
            let authority_bindings = configured_bindings
                .iter()
                .filter(|candidate| {
                    candidate.source_kind == daemon_binding.source_kind
                        && candidate.authority == daemon_binding.authority
                })
                .collect::<Vec<_>>();
            match authority_bindings.as_slice() {
                [candidate] if **candidate == daemon_binding => SourceBindingCheck::Verified,
                [candidate]
                    if candidate.binding_id == daemon_binding.binding_id
                        && candidate.source_locator_digest
                            != daemon_binding.source_locator_digest =>
                {
                    SourceBindingCheck::LocatorDigestDrift
                }
                _ => SourceBindingCheck::Mismatch,
            }
        };
        match check {
            SourceBindingCheck::Verified => break,
            // Exactly one daemon-owned binding for this registry-verified
            // project whose only drift is the path-derived locator digest:
            // the checkout moved or was renamed. The registry — not the
            // path — owns identity and has already resolved this exact
            // registered project for the current root, so republish the
            // binding with the new derived digest under compare-and-swap
            // instead of demanding a reset.
            SourceBindingCheck::LocatorDigestDrift if !rebind_attempted => {
                rebind_attempted = true;
                current = match store
                    .rebind_daemon_project_source_binding(
                        &current.revision_id,
                        &daemon_binding,
                        now_micros(),
                    )
                    .await
                {
                    Ok(state) => state,
                    // A concurrent open won the swap; adopt what it
                    // published and re-verify it exactly.
                    Err(ConfigurationError::RevisionConflict) => {
                        store.current().await.map_err(map_configuration_error)?
                    }
                    Err(error) => return Err(map_configuration_error(error)),
                };
            }
            SourceBindingCheck::LocatorDigestDrift | SourceBindingCheck::Mismatch => {
                return Err(TraceDecayError::reset_required(
                    "configuration",
                    "canonical configuration source binding does not match the registered project",
                ));
            }
        }
    }
    let configuration =
        DaemonRuntimeConfiguration::new(target, current.revision_id, current.snapshot)?;
    install_pinned_runtime_configuration(configuration.clone());
    Ok(configuration)
}

/// Test-only convenience wrapper over
/// [`open_runtime_configuration_for_registered_database`] that returns just the
/// pinned snapshot. Production open paths keep the full
/// [`OpenedRuntimeConfiguration`] bundle (snapshot + registered database).
#[cfg(any(test, feature = "test-helpers"))]
pub async fn ensure_runtime_configuration_for_registered_database(
    project_root: &Path,
    layout: &tracedecay_runtime_core::storage::StoreLayout,
    database: RegisteredGlobalDbLeaseV1,
) -> Result<DaemonRuntimeConfiguration> {
    Ok(
        open_runtime_configuration_for_registered_database(project_root, layout, database)
            .await?
            .configuration,
    )
}

/// Loads an already-persisted current configuration without creating a store
/// or publishing a fallback revision.
#[hotpath::measure(label = "daemon.config.open.read_only", future = true)]
pub async fn open_runtime_configuration_for_registered_database_read_only(
    project_root: &Path,
    layout: &tracedecay_runtime_core::storage::StoreLayout,
    database: RegisteredGlobalDbLeaseV1,
) -> Result<OpenedRuntimeConfiguration> {
    let target = runtime_configuration_target_for_layout(project_root, layout)?;
    validate_registered_configuration_database(&target, database.as_ref())?;
    let store = GlobalDbConfigurationControlStore::new_registered(database.as_ref());
    let configuration = open_runtime_configuration_read_only_from_store(target, &store).await?;
    Ok(OpenedRuntimeConfiguration {
        configuration,
        registered_database: database,
    })
}

async fn open_runtime_configuration_read_only_from_store(
    target: RuntimeConfigurationTarget,
    store: &GlobalDbConfigurationControlStore<'_>,
) -> Result<DaemonRuntimeConfiguration> {
    if store
        .is_uninitialized()
        .await
        .map_err(map_configuration_error)?
    {
        return Err(TraceDecayError::reset_required(
            "configuration",
            "configuration store has no canonical revision",
        ));
    }
    let current = store.current().await.map_err(map_configuration_error)?;
    let configuration =
        DaemonRuntimeConfiguration::new(target, current.revision_id, current.snapshot)?;
    install_pinned_runtime_configuration(configuration.clone());
    Ok(configuration)
}

fn validate_registered_configuration_database(
    target: &RuntimeConfigurationTarget,
    database: &RegisteredGlobalDb,
) -> Result<()> {
    match &database.binding().shard_id.scope {
        tracedecay_store::StoreShardScopeV1::ProjectSessions { project_id }
            if project_id == &target.project_id =>
        {
            Ok(())
        }
        _ => Err(config_error(
            "configuration authority unavailable: registered database is not the exact project session shard",
        )),
    }
}

fn map_configuration_error(error: ConfigurationError) -> TraceDecayError {
    match error {
        ConfigurationError::ResetRequired { reason } => {
            TraceDecayError::reset_required("configuration", reason)
        }
        error => config_error(format!("configuration authority unavailable: {error}")),
    }
}

/// Returns a cached configuration without resolving a layout, opening a
/// database, performing IPC, or reading a file. This is the hook-safe lookup.
pub fn cached_runtime_configuration(project_root: &Path) -> Result<DaemonRuntimeConfiguration> {
    runtime_configuration_cache().for_root(project_root)
}

/// Looks up a daemon-published snapshot by an already-authoritative project
/// ID. The supplied root is only used to materialize display metadata;
/// it never participates in authority resolution.
pub fn cached_runtime_configuration_for_project_id(
    project_root: &Path,
    project_id: &str,
) -> Result<DaemonRuntimeConfiguration> {
    let target = runtime_configuration_target_for_project_id(project_root, project_id)?;
    Ok(runtime_configuration_cache()
        .for_project(&target.project_id)?
        .with_project_root(&target.project_root))
}

pub fn cached_sync_config(project_root: &Path) -> Result<SyncConfig> {
    Ok(cached_runtime_configuration(project_root)?
        .into_config()
        .sync)
}

pub fn cached_telemetry_config(project_root: &Path) -> Result<TelemetryConfig> {
    Ok(cached_runtime_configuration(project_root)?
        .into_config()
        .telemetry)
}

fn config_error(message: impl Into<String>) -> TraceDecayError {
    TraceDecayError::Config {
        message: message.into(),
    }
}

pub async fn get_config_path_with_identity(project_root: &Path) -> PathBuf {
    if let Ok(layout) =
        crate::project::TraceDecay::resolve_store_layout_for_identity(project_root).await
    {
        return layout.config_path;
    }
    get_config_path(project_root)
}

pub async fn load_config_with_identity(project_root: &Path) -> Result<TraceDecayConfig> {
    let config_path = get_config_path_with_identity(project_root).await;
    load_config_from_path(project_root, &config_path)
}

#[hotpath::measure(label = "daemon.config.discover", future = true)]
pub async fn discover_project_root_with_identity(start: &Path) -> Option<PathBuf> {
    if let Some(root) = discover_project_root(start) {
        return Some(root);
    }
    let candidate = tracedecay_runtime_core::worktree::git_worktree_root(start)
        .unwrap_or_else(|| start.to_path_buf());
    if crate::project::TraceDecay::has_initialized_store(&candidate).await {
        Some(candidate)
    } else {
        None
    }
}

/// Serializes test and benchmark code that mutates process-wide storage env
/// vars (`TRACEDECAY_DATA_DIR` and related HOME/profile pins).
///
/// Single source of truth: [`tracedecay_runtime_core::config`] owns the lock,
/// [`lock_user_data_dir_test_env`], and `PinnedUserDataDir`; this module only
/// re-exports them so every historical `config::…` call site keeps
/// resolving. The re-export follows the same gate as its only non-test
/// consumer, the root's `session_temporal_benchmark`, so a production build
/// carries neither the harness nor its accessor.
#[cfg(any(test, feature = "test-helpers"))]
pub use tracedecay_runtime_core::config::lock_user_data_dir_test_env;

/// Pins [`USER_DATA_DIR_ENV`] and agent home discovery to an isolated temp
/// profile while holding the shared user-data-dir test lock, so parallel lib
/// tests cannot race profile resolution or scan live host transcripts during
/// `TraceDecay::init` / indexing.
#[cfg(any(test, feature = "test-helpers"))]
pub use tracedecay_runtime_core::config::PinnedUserDataDir;

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests;
