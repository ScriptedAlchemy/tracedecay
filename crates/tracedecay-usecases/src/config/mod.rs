//! Configuration surfaces the use-case layer owns or re-exports.
//!
//! `retrieval` and `scope_control` were moved down out of the root binary's
//! `src/config/`: their whole dependency closure is `tracedecay-domain`,
//! `tracedecay-query`, `tracedecay-search-eval`, `tracedecay-semantic` and this
//! crate's own `configuration` module, so nothing kept them at the root. The
//! remaining rows are re-exports of surfaces `tracedecay-global-db` and
//! `tracedecay-domain` already own, kept under the historical `crate::config::…`
//! spelling so the moved call sites did not have to churn.

pub mod retrieval;
pub mod scope_control;

pub use tracedecay_domain::configuration::SEMANTIC_RUNTIME_SETTING_KEY;
pub use tracedecay_global_db::configuration::semantic::{
    SemanticConfig, SemanticProfileSelection, SemanticResourceCeilings,
};
pub use tracedecay_global_db::configuration::{registry, resolver};
#[cfg(test)]
pub use tracedecay_runtime_core::config::PinnedUserDataDir;

use std::collections::BTreeMap;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::{Arc, OnceLock, RwLock};

use tracedecay_domain::ProjectId;
use tracedecay_domain::configuration::{
    ConfigurationRevisionId, ConfigurationSnapshotV1, ConfigurationValueV1,
    DIAGNOSTICS_PREWARM_SETTING_KEY, INDEX_EXCLUDE_SETTING_KEY,
    INDEX_EXTRACT_DOCSTRINGS_SETTING_KEY, INDEX_GIT_IGNORE_SETTING_KEY, INDEX_INCLUDE_SETTING_KEY,
    INDEX_MAX_FILE_SIZE_SETTING_KEY, INDEX_TRACK_CALL_SITES_SETTING_KEY,
    SYNC_AUTO_TRACK_PR_BRANCHES_SETTING_KEY, SYNC_AUTO_TRACK_PR_POLL_SECS_SETTING_KEY,
    TELEMETRY_TIMINGS_SETTING_KEY,
};
use tracedecay_global_db::RegisteredGlobalDb;
use tracedecay_runtime_core::errors::{Result, TraceDecayError};
use tracedecay_runtime_core::storage::StoreLayout;

#[derive(Debug, Clone, PartialEq)]
pub struct TraceDecayConfig {
    pub include: Vec<String>,
    pub exclude: Vec<String>,
    pub max_file_size: u64,
    pub extract_docstrings: bool,
    pub track_call_sites: bool,
    pub git_ignore: bool,
    pub diagnostics_prewarm: bool,
    pub semantic: SemanticConfig,
    pub sync: SyncConfig,
    pub telemetry: TelemetryConfig,
}

impl Default for TraceDecayConfig {
    fn default() -> Self {
        Self {
            include: Vec::new(),
            exclude: Vec::new(),
            max_file_size: 1_048_576,
            extract_docstrings: true,
            track_call_sites: true,
            git_ignore: true,
            diagnostics_prewarm: false,
            semantic: SemanticConfig::default(),
            sync: SyncConfig::default(),
            telemetry: TelemetryConfig::default(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyncConfig {
    pub auto_track_pr_branches: bool,
    pub auto_track_pr_poll_secs: u64,
}

impl Default for SyncConfig {
    fn default() -> Self {
        Self {
            auto_track_pr_branches: false,
            auto_track_pr_poll_secs: 300,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TelemetryConfig {
    pub timings: bool,
}

impl Default for TelemetryConfig {
    fn default() -> Self {
        Self { timings: true }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RuntimeConfigurationTarget {
    pub project_id: ProjectId,
    pub project_root: PathBuf,
}

#[derive(Clone, Debug)]
pub struct PinnedRuntimeConfiguration {
    pub target: RuntimeConfigurationTarget,
    pub revision_id: ConfigurationRevisionId,
    pub snapshot: ConfigurationSnapshotV1,
    pub config: TraceDecayConfig,
}

impl PinnedRuntimeConfiguration {
    pub fn new(
        target: RuntimeConfigurationTarget,
        revision_id: ConfigurationRevisionId,
        snapshot: ConfigurationSnapshotV1,
    ) -> Result<Self> {
        let config = runtime_config_from_snapshot(&snapshot)?;
        Ok(Self {
            target,
            revision_id,
            snapshot,
            config,
        })
    }
}

pub type RuntimeConfigurationFuture<'a, T> = Pin<Box<dyn Future<Output = Result<T>> + Send + 'a>>;

pub trait ConfigurationDaemonClient: Send + Sync {
    fn mutate_direct(
        &self,
        target: RuntimeConfigurationTarget,
        mutation: crate::configuration::DirectConfigurationMutation,
        expected_revision: ConfigurationRevisionId,
    ) -> RuntimeConfigurationFuture<'_, PinnedRuntimeConfiguration>;
}

pub struct OpenedRuntimeConfiguration {
    pub(crate) configuration: PinnedRuntimeConfiguration,
    pub(crate) registered_database: Arc<RegisteredGlobalDb>,
}

impl OpenedRuntimeConfiguration {
    pub fn new(
        configuration: PinnedRuntimeConfiguration,
        registered_database: Arc<RegisteredGlobalDb>,
    ) -> Self {
        Self {
            configuration,
            registered_database,
        }
    }
}

pub trait RuntimeConfigurationAuthorityPort: Send + Sync {
    fn open<'a>(
        &'a self,
        project_root: &'a Path,
        layout: &'a StoreLayout,
        database: Arc<RegisteredGlobalDb>,
    ) -> RuntimeConfigurationFuture<'a, OpenedRuntimeConfiguration>;

    fn open_read_only<'a>(
        &'a self,
        project_root: &'a Path,
        layout: &'a StoreLayout,
        database: Arc<RegisteredGlobalDb>,
    ) -> RuntimeConfigurationFuture<'a, OpenedRuntimeConfiguration>;

    fn resolve<'a>(
        &'a self,
        project_root: &'a Path,
        layout: &'a StoreLayout,
        database: Arc<RegisteredGlobalDb>,
    ) -> RuntimeConfigurationFuture<'a, PinnedRuntimeConfiguration>;

    fn load_read_only<'a>(
        &'a self,
        project_root: &'a Path,
        layout: &'a StoreLayout,
        database: Arc<RegisteredGlobalDb>,
    ) -> RuntimeConfigurationFuture<'a, PinnedRuntimeConfiguration>;
}

static RUNTIME_CONFIGURATION_AUTHORITY: OnceLock<Arc<dyn RuntimeConfigurationAuthorityPort>> =
    OnceLock::new();

pub fn install_runtime_configuration_authority(
    authority: Arc<dyn RuntimeConfigurationAuthorityPort>,
) -> Result<()> {
    RUNTIME_CONFIGURATION_AUTHORITY
        .set(authority)
        .map_err(|_| config_error("runtime configuration authority is already installed"))
}

pub async fn open_runtime_configuration_for_registered_database(
    project_root: &Path,
    layout: &StoreLayout,
    database: Arc<RegisteredGlobalDb>,
) -> Result<OpenedRuntimeConfiguration> {
    runtime_configuration_authority()?
        .open(project_root, layout, database)
        .await
}

pub async fn open_runtime_configuration_for_registered_database_read_only(
    project_root: &Path,
    layout: &StoreLayout,
    database: Arc<RegisteredGlobalDb>,
) -> Result<OpenedRuntimeConfiguration> {
    runtime_configuration_authority()?
        .open_read_only(project_root, layout, database)
        .await
}

#[cfg(test)]
pub(crate) async fn ensure_runtime_configuration_for_registered_database(
    project_root: &Path,
    layout: &StoreLayout,
    database: Arc<RegisteredGlobalDb>,
) -> Result<PinnedRuntimeConfiguration> {
    Ok(
        open_runtime_configuration_for_registered_database(project_root, layout, database)
            .await?
            .configuration,
    )
}

fn runtime_configuration_authority() -> Result<&'static dyn RuntimeConfigurationAuthorityPort> {
    RUNTIME_CONFIGURATION_AUTHORITY
        .get()
        .map(Arc::as_ref)
        .ok_or_else(|| config_error("runtime configuration authority is not installed"))
}

#[derive(Default)]
struct RuntimeConfigurationState {
    pinned: BTreeMap<String, PinnedRuntimeConfiguration>,
    clients: BTreeMap<String, Arc<dyn ConfigurationDaemonClient>>,
}

fn runtime_configuration_state() -> &'static RwLock<RuntimeConfigurationState> {
    static STATE: OnceLock<RwLock<RuntimeConfigurationState>> = OnceLock::new();
    STATE.get_or_init(|| RwLock::new(RuntimeConfigurationState::default()))
}

pub fn install_pinned_runtime_configuration(
    configuration: PinnedRuntimeConfiguration,
) -> Result<()> {
    runtime_configuration_state()
        .write()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .pinned
        .insert(
            configuration.target.project_id.as_str().to_owned(),
            configuration,
        );
    Ok(())
}

pub fn install_configuration_daemon_client_for_project(
    target: &RuntimeConfigurationTarget,
    client: Arc<dyn ConfigurationDaemonClient>,
) {
    runtime_configuration_state()
        .write()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clients
        .insert(target.project_id.as_str().to_owned(), client);
}

pub fn configuration_daemon_client_for_project(
    project_id: &ProjectId,
) -> Option<Arc<dyn ConfigurationDaemonClient>> {
    runtime_configuration_state()
        .read()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clients
        .get(project_id.as_str())
        .cloned()
}

pub fn uninstall_configuration_daemon_client_for_project(
    target: &RuntimeConfigurationTarget,
    client: &Arc<dyn ConfigurationDaemonClient>,
) {
    let mut state = runtime_configuration_state()
        .write()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if state
        .clients
        .get(target.project_id.as_str())
        .is_some_and(|installed| Arc::ptr_eq(installed, client))
    {
        state.clients.remove(target.project_id.as_str());
    }
}

fn runtime_config_from_snapshot(snapshot: &ConfigurationSnapshotV1) -> Result<TraceDecayConfig> {
    Ok(TraceDecayConfig {
        include: required_string_list(snapshot, INDEX_INCLUDE_SETTING_KEY)?,
        exclude: required_string_list(snapshot, INDEX_EXCLUDE_SETTING_KEY)?,
        max_file_size: required_unsigned(snapshot, INDEX_MAX_FILE_SIZE_SETTING_KEY)?,
        extract_docstrings: required_bool(snapshot, INDEX_EXTRACT_DOCSTRINGS_SETTING_KEY)?,
        track_call_sites: required_bool(snapshot, INDEX_TRACK_CALL_SITES_SETTING_KEY)?,
        git_ignore: required_bool(snapshot, INDEX_GIT_IGNORE_SETTING_KEY)?,
        diagnostics_prewarm: required_bool(snapshot, DIAGNOSTICS_PREWARM_SETTING_KEY)?,
        semantic: SemanticConfig::default(),
        sync: SyncConfig {
            auto_track_pr_branches: required_bool(
                snapshot,
                SYNC_AUTO_TRACK_PR_BRANCHES_SETTING_KEY,
            )?,
            auto_track_pr_poll_secs: required_unsigned(
                snapshot,
                SYNC_AUTO_TRACK_PR_POLL_SECS_SETTING_KEY,
            )?,
        },
        telemetry: TelemetryConfig {
            timings: required_bool(snapshot, TELEMETRY_TIMINGS_SETTING_KEY)?,
        },
    })
}

fn required_setting<'a>(
    snapshot: &'a ConfigurationSnapshotV1,
    key: &str,
) -> Result<&'a ConfigurationValueV1> {
    let key = tracedecay_domain::configuration::SettingKey::new(key)
        .map_err(|error| config_error(format!("invalid runtime setting key: {error}")))?;
    snapshot.effective_values.get(&key).ok_or_else(|| {
        config_error(format!(
            "resolved configuration is missing '{}'",
            key.as_str()
        ))
    })
}

fn required_bool(snapshot: &ConfigurationSnapshotV1, key: &str) -> Result<bool> {
    match required_setting(snapshot, key)? {
        ConfigurationValueV1::Boolean(value) => Ok(*value),
        _ => Err(config_error(format!(
            "resolved configuration setting '{key}' is not boolean"
        ))),
    }
}

fn required_unsigned(snapshot: &ConfigurationSnapshotV1, key: &str) -> Result<u64> {
    match required_setting(snapshot, key)? {
        ConfigurationValueV1::Unsigned(value) => Ok(*value),
        _ => Err(config_error(format!(
            "resolved configuration setting '{key}' is not unsigned"
        ))),
    }
}

fn required_string_list(snapshot: &ConfigurationSnapshotV1, key: &str) -> Result<Vec<String>> {
    match required_setting(snapshot, key)? {
        ConfigurationValueV1::StringList(value) => Ok(value.clone()),
        _ => Err(config_error(format!(
            "resolved configuration setting '{key}' is not a string list"
        ))),
    }
}

fn config_error(message: impl Into<String>) -> TraceDecayError {
    TraceDecayError::Config {
        message: message.into(),
    }
}
