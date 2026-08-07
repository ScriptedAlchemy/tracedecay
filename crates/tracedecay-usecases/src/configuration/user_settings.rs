//! User-profile projection over the canonical configuration control plane.
//!
//! Editable values come only from the daemon-owned resolved snapshot. The
//! legacy `config.toml` remains a read-only metadata source for fields that are
//! not settings (installed agents, cached version state, and automation
//! discovery); transports cannot obtain a write capability for it.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use tracedecay_automation::config::AutomationConfig;
use tracedecay_domain::configuration::{
    ConfigurationLayerIdV1, ConfigurationValueV1, SettingKey,
    USER_EXTRACTION_TIMEOUT_SECS_SETTING_KEY, USER_UPLOAD_ENABLED_SETTING_KEY,
    USER_WATCHER_DEBOUNCE_MS_SETTING_KEY, UserProfileId,
};

use super::{DirectConfigurationMutation, ProductionConfigurationDaemonClient};
use crate::user_config::UserConfig;

pub type UserSettingsFuture<'a, T> =
    Pin<Box<dyn Future<Output = Result<T, UserSettingsAuthorityError>> + Send + 'a>>;

#[derive(Clone, Debug)]
pub struct UserSettingsSnapshotV1 {
    pub legacy_config_path: String,
    pub configuration_snapshot_id: String,
    pub configuration_revision_id: String,
    pub upload_enabled: bool,
    pub watcher_debounce: String,
    pub watcher_debounce_ms: u64,
    pub extraction_timeout_secs: u64,
    pub installed_agents: Vec<String>,
    pub cached_latest_version: String,
    pub automation: AutomationConfig,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct UserSettingsMutationV1 {
    pub upload_enabled: Option<bool>,
    pub watcher_debounce: Option<String>,
    pub extraction_timeout_secs: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UserSettingsMutationPlanV1 {
    pub mutations: Vec<DirectConfigurationMutation>,
    pub restart_recommended: bool,
}

#[derive(Debug)]
pub enum UserSettingsAuthorityError {
    Unavailable { message: String },
}

impl std::fmt::Display for UserSettingsAuthorityError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unavailable { message } => formatter.write_str(message),
        }
    }
}

impl std::error::Error for UserSettingsAuthorityError {}

pub trait UserSettingsDaemonClient: Send + Sync {
    fn read(&self) -> UserSettingsFuture<'_, UserSettingsSnapshotV1>;
}

/// Daemon-owned read projection. A default-constructed value is deliberately
/// unavailable and is used only by isolated dashboard fixtures that did not
/// mount the configuration runtime.
#[derive(Default)]
pub struct ProductionUserSettingsDaemonClient {
    configuration: Option<Arc<ProductionConfigurationDaemonClient>>,
    profile_id: Option<UserProfileId>,
}

impl ProductionUserSettingsDaemonClient {
    pub fn new(
        configuration: Arc<ProductionConfigurationDaemonClient>,
        profile_id: UserProfileId,
    ) -> Self {
        Self {
            configuration: Some(configuration),
            profile_id: Some(profile_id),
        }
    }
}

impl UserSettingsDaemonClient for ProductionUserSettingsDaemonClient {
    fn read(&self) -> UserSettingsFuture<'_, UserSettingsSnapshotV1> {
        let configuration = self.configuration.clone();
        let profile_id = self.profile_id.clone();
        Box::pin(async move {
            let configuration =
                configuration.ok_or_else(|| unavailable("configuration runtime"))?;
            let profile_id = profile_id.ok_or_else(|| unavailable("user profile identity"))?;
            let current = configuration
                .current()
                .await
                .map_err(|error| unavailable(format!("resolved configuration: {error}")))?;
            let metadata = tokio::task::spawn_blocking(read_legacy_user_metadata)
                .await
                .map_err(|error| unavailable(format!("user settings metadata task: {error}")))??;
            user_settings_snapshot(&current, &profile_id, metadata)
        })
    }
}

struct LegacyUserMetadata {
    path: String,
    installed_agents: Vec<String>,
    cached_latest_version: String,
    automation: AutomationConfig,
}

fn read_legacy_user_metadata() -> Result<LegacyUserMetadata, UserSettingsAuthorityError> {
    let path = crate::user_config::config_path()
        .ok_or_else(|| unavailable("legacy user configuration path"))?;
    let config = UserConfig::load_strict()
        .map_err(|error| unavailable(format!("legacy user configuration metadata: {error}")))?;
    Ok(LegacyUserMetadata {
        path: path.display().to_string(),
        installed_agents: config.installed_agents,
        cached_latest_version: config.cached_latest_version,
        automation: config.automation,
    })
}

fn user_settings_snapshot(
    current: &crate::config::PinnedRuntimeConfiguration,
    profile_id: &UserProfileId,
    metadata: LegacyUserMetadata,
) -> Result<UserSettingsSnapshotV1, UserSettingsAuthorityError> {
    validate_profile_provenance(current, profile_id)?;
    let upload_enabled = required_bool(current, USER_UPLOAD_ENABLED_SETTING_KEY)?;
    let watcher_debounce_ms = required_unsigned(current, USER_WATCHER_DEBOUNCE_MS_SETTING_KEY)?;
    let extraction_timeout_secs =
        required_unsigned(current, USER_EXTRACTION_TIMEOUT_SECS_SETTING_KEY)?;
    Ok(UserSettingsSnapshotV1 {
        legacy_config_path: metadata.path,
        configuration_snapshot_id: current.snapshot.snapshot_id.as_str().to_owned(),
        configuration_revision_id: current.revision_id.as_str().to_owned(),
        upload_enabled,
        watcher_debounce: format_duration_millis(watcher_debounce_ms),
        watcher_debounce_ms,
        extraction_timeout_secs,
        installed_agents: metadata.installed_agents,
        cached_latest_version: metadata.cached_latest_version,
        automation: metadata.automation,
    })
}

fn validate_profile_provenance(
    current: &crate::config::PinnedRuntimeConfiguration,
    profile_id: &UserProfileId,
) -> Result<(), UserSettingsAuthorityError> {
    for raw_key in [
        USER_UPLOAD_ENABLED_SETTING_KEY,
        USER_WATCHER_DEBOUNCE_MS_SETTING_KEY,
        USER_EXTRACTION_TIMEOUT_SECS_SETTING_KEY,
    ] {
        let key = setting_key(raw_key)?;
        let candidates = current
            .snapshot
            .provenance
            .get(&key)
            .ok_or_else(|| unavailable(format!("provenance for {raw_key}")))?;
        if candidates.iter().any(|candidate| match &candidate.layer {
            ConfigurationLayerIdV1::Default => false,
            ConfigurationLayerIdV1::UserProfile {
                profile_id: candidate_profile,
            } => candidate_profile != profile_id,
            ConfigurationLayerIdV1::Project { .. } | ConfigurationLayerIdV1::Collection { .. } => {
                true
            }
        }) {
            return Err(unavailable(format!(
                "exact user profile provenance for {raw_key}"
            )));
        }
    }
    Ok(())
}

pub fn plan_user_settings_mutation(
    _current: &UserSettingsSnapshotV1,
    profile_id: UserProfileId,
    mutation: UserSettingsMutationV1,
) -> Result<UserSettingsMutationPlanV1, UserSettingsAuthorityError> {
    let layer = ConfigurationLayerIdV1::UserProfile { profile_id };
    let mut mutations = Vec::new();
    let mut restart_recommended = false;

    if let Some(upload_enabled) = mutation.upload_enabled {
        mutations.push(set(
            layer.clone(),
            USER_UPLOAD_ENABLED_SETTING_KEY,
            ConfigurationValueV1::Boolean(upload_enabled),
        )?);
    }
    if let Some(watcher_debounce) = mutation.watcher_debounce {
        let watcher_debounce_ms = parse_duration_millis(&watcher_debounce)
            .ok_or_else(|| unavailable("canonical watcher debounce duration"))?;
        mutations.push(set(
            layer.clone(),
            USER_WATCHER_DEBOUNCE_MS_SETTING_KEY,
            ConfigurationValueV1::Unsigned(watcher_debounce_ms),
        )?);
        restart_recommended = true;
    }
    if let Some(extraction_timeout_secs) = mutation.extraction_timeout_secs {
        mutations.push(set(
            layer,
            USER_EXTRACTION_TIMEOUT_SECS_SETTING_KEY,
            ConfigurationValueV1::Unsigned(extraction_timeout_secs),
        )?);
        restart_recommended = true;
    }

    Ok(UserSettingsMutationPlanV1 {
        mutations,
        restart_recommended,
    })
}

pub fn parse_duration_millis(value: &str) -> Option<u64> {
    let millis = crate::user_config::parse_duration(value)?.as_millis();
    u64::try_from(millis).ok().filter(|millis| *millis > 0)
}

fn format_duration_millis(millis: u64) -> String {
    if millis.is_multiple_of(60_000) {
        format!("{}m", millis / 60_000)
    } else if millis.is_multiple_of(1_000) {
        format!("{}s", millis / 1_000)
    } else {
        format!("{millis}ms")
    }
}

fn set(
    layer: ConfigurationLayerIdV1,
    raw_key: &str,
    value: ConfigurationValueV1,
) -> Result<DirectConfigurationMutation, UserSettingsAuthorityError> {
    Ok(DirectConfigurationMutation::Set {
        layer,
        key: setting_key(raw_key)?,
        value,
    })
}

fn setting_key(raw_key: &str) -> Result<SettingKey, UserSettingsAuthorityError> {
    SettingKey::new(raw_key)
        .map_err(|error| unavailable(format!("registered user setting key: {error}")))
}

fn required_setting<'a>(
    current: &'a crate::config::PinnedRuntimeConfiguration,
    raw_key: &str,
) -> Result<&'a ConfigurationValueV1, UserSettingsAuthorityError> {
    let key = setting_key(raw_key)?;
    current
        .snapshot
        .effective_values
        .get(&key)
        .ok_or_else(|| unavailable(format!("resolved user setting {raw_key}")))
}

fn required_bool(
    current: &crate::config::PinnedRuntimeConfiguration,
    raw_key: &str,
) -> Result<bool, UserSettingsAuthorityError> {
    match required_setting(current, raw_key)? {
        ConfigurationValueV1::Boolean(value) => Ok(*value),
        _ => Err(unavailable(format!("boolean user setting {raw_key}"))),
    }
}

fn required_unsigned(
    current: &crate::config::PinnedRuntimeConfiguration,
    raw_key: &str,
) -> Result<u64, UserSettingsAuthorityError> {
    match required_setting(current, raw_key)? {
        ConfigurationValueV1::Unsigned(value) if *value > 0 => Ok(*value),
        _ => Err(unavailable(format!(
            "positive unsigned user setting {raw_key}"
        ))),
    }
}

fn unavailable(authority: impl Into<String>) -> UserSettingsAuthorityError {
    UserSettingsAuthorityError::Unavailable {
        message: format!("user settings authority unavailable: {}", authority.into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snapshot() -> UserSettingsSnapshotV1 {
        UserSettingsSnapshotV1 {
            legacy_config_path: "/profile/config.toml".to_owned(),
            configuration_snapshot_id: "configuration.snapshot.fixture".to_owned(),
            configuration_revision_id: "configuration.revision.fixture".to_owned(),
            upload_enabled: false,
            watcher_debounce: "2s".to_owned(),
            watcher_debounce_ms: 2_000,
            extraction_timeout_secs: 60,
            installed_agents: Vec::new(),
            cached_latest_version: String::new(),
            automation: AutomationConfig::default(),
        }
    }

    #[test]
    fn profile_plan_targets_only_the_exact_profile_and_classifies_restart() {
        let profile_id = UserProfileId::new("profile.fixture").unwrap();
        let plan = plan_user_settings_mutation(
            &snapshot(),
            profile_id.clone(),
            UserSettingsMutationV1 {
                upload_enabled: Some(true),
                watcher_debounce: Some("15s".to_owned()),
                extraction_timeout_secs: Some(60),
            },
        )
        .unwrap();

        assert_eq!(plan.mutations.len(), 3);
        assert!(plan.restart_recommended);
        assert!(plan.mutations.iter().all(|mutation| {
            matches!(
                mutation.target_layer(),
                Ok(ConfigurationLayerIdV1::UserProfile {
                    profile_id: target_profile_id,
                }) if target_profile_id == &profile_id
            )
        }));
    }

    #[test]
    fn duration_round_trip_is_canonical_and_positive() {
        assert_eq!(parse_duration_millis("15s"), Some(15_000));
        assert_eq!(format_duration_millis(15_000), "15s");
        assert_eq!(parse_duration_millis("1m"), Some(60_000));
        assert_eq!(format_duration_millis(60_000), "1m");
        assert_eq!(parse_duration_millis("0s"), None);
    }
}
