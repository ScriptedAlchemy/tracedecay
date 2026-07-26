//! Daemon-owned user-settings read and mutation adapter.
//!
//! Dashboard and other transports receive a narrow client and never open or
//! mutate `config.toml` themselves. The client owns strict reads, revision CAS,
//! atomic persistence, and the restart classification returned to callers.

use std::future::Future;
use std::pin::Pin;

use crate::automation::config::AutomationConfig;
use crate::user_config::{ConfigSaveError, UserConfig};

pub type UserSettingsFuture<'a, T> =
    Pin<Box<dyn Future<Output = Result<T, UserSettingsAuthorityError>> + Send + 'a>>;

#[derive(Clone, Debug)]
pub struct UserSettingsSnapshotV1 {
    pub config_path: String,
    pub revision_id: String,
    pub upload_enabled: bool,
    pub watcher_debounce: String,
    pub extraction_timeout_secs: u64,
    pub installed_agents: Vec<String>,
    pub cached_latest_version: String,
    pub automation: AutomationConfig,
}

impl UserSettingsSnapshotV1 {
    fn from_config(config: UserConfig) -> Result<Self, UserSettingsAuthorityError> {
        let revision_id = config
            .revision_id()
            .map_err(UserSettingsAuthorityError::from)?;
        Ok(Self {
            config_path: crate::user_config::config_path()
                .map(|path| path.display().to_string())
                .unwrap_or_default(),
            revision_id,
            upload_enabled: config.upload_enabled,
            watcher_debounce: config.watcher_debounce,
            extraction_timeout_secs: config.extraction_timeout_secs,
            installed_agents: config.installed_agents,
            cached_latest_version: config.cached_latest_version,
            automation: config.automation,
        })
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct UserSettingsMutationV1 {
    pub upload_enabled: Option<bool>,
    pub watcher_debounce: Option<String>,
    pub extraction_timeout_secs: Option<u64>,
}

#[derive(Clone, Debug)]
pub struct UserSettingsMutationReceiptV1 {
    pub snapshot: UserSettingsSnapshotV1,
    pub restart_recommended: bool,
    pub recovered_backup_path: Option<String>,
}

#[derive(Debug)]
pub enum UserSettingsAuthorityError {
    RevisionConflict { expected: String, actual: String },
    Unavailable { message: String },
}

impl std::fmt::Display for UserSettingsAuthorityError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::RevisionConflict { expected, actual } => write!(
                formatter,
                "user settings revision conflict (expected {expected}, actual {actual})"
            ),
            Self::Unavailable { message } => formatter.write_str(message),
        }
    }
}

impl std::error::Error for UserSettingsAuthorityError {}

impl From<ConfigSaveError> for UserSettingsAuthorityError {
    fn from(error: ConfigSaveError) -> Self {
        match error {
            ConfigSaveError::RevisionConflict { expected, actual } => {
                Self::RevisionConflict { expected, actual }
            }
            error => Self::Unavailable {
                message: error.to_string(),
            },
        }
    }
}

pub trait UserSettingsDaemonClient: Send + Sync {
    fn read(&self) -> UserSettingsFuture<'_, UserSettingsSnapshotV1>;

    fn mutate(
        &self,
        expected_revision_id: String,
        mutation: UserSettingsMutationV1,
    ) -> UserSettingsFuture<'_, UserSettingsMutationReceiptV1>;
}

/// Production daemon adapter. File I/O runs on the blocking pool; transports
/// cannot obtain a writable `UserConfig` or bypass the revision CAS.
#[derive(Debug, Default)]
pub struct ProductionUserSettingsDaemonClient;

impl UserSettingsDaemonClient for ProductionUserSettingsDaemonClient {
    fn read(&self) -> UserSettingsFuture<'_, UserSettingsSnapshotV1> {
        Box::pin(async {
            tokio::task::spawn_blocking(|| UserSettingsSnapshotV1::from_config(UserConfig::load()))
                .await
                .map_err(|error| UserSettingsAuthorityError::Unavailable {
                    message: format!("user settings daemon task failed: {error}"),
                })?
        })
    }

    fn mutate(
        &self,
        expected_revision_id: String,
        mutation: UserSettingsMutationV1,
    ) -> UserSettingsFuture<'_, UserSettingsMutationReceiptV1> {
        Box::pin(async move {
            tokio::task::spawn_blocking(move || {
                let committed =
                    UserConfig::mutate_with_recovery_if_revision(&expected_revision_id, |config| {
                        let restart_recommended = mutation
                            .watcher_debounce
                            .as_ref()
                            .is_some_and(|value| *value != config.watcher_debounce)
                            || mutation
                                .extraction_timeout_secs
                                .is_some_and(|value| value != config.extraction_timeout_secs);
                        if let Some(upload_enabled) = mutation.upload_enabled {
                            config.upload_enabled = upload_enabled;
                        }
                        if let Some(watcher_debounce) = mutation.watcher_debounce {
                            config.watcher_debounce = watcher_debounce;
                        }
                        if let Some(extraction_timeout_secs) = mutation.extraction_timeout_secs {
                            config.extraction_timeout_secs = extraction_timeout_secs;
                        }
                        restart_recommended
                    })
                    .map_err(UserSettingsAuthorityError::from)?;
                Ok(UserSettingsMutationReceiptV1 {
                    snapshot: UserSettingsSnapshotV1::from_config(committed.config)?,
                    restart_recommended: committed.output,
                    recovered_backup_path: committed.backup.map(|path| path.display().to_string()),
                })
            })
            .await
            .map_err(|error| UserSettingsAuthorityError::Unavailable {
                message: format!("user settings daemon task failed: {error}"),
            })?
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn production_client_owns_read_revision_and_atomic_mutation() {
        let _pin = crate::config::PinnedUserDataDir::new();
        let client = ProductionUserSettingsDaemonClient;
        let before = client.read().await.expect("read user settings");
        let receipt = client
            .mutate(
                before.revision_id.clone(),
                UserSettingsMutationV1 {
                    watcher_debounce: Some("15s".to_owned()),
                    ..UserSettingsMutationV1::default()
                },
            )
            .await
            .expect("mutate user settings");

        assert_ne!(receipt.snapshot.revision_id, before.revision_id);
        assert_eq!(receipt.snapshot.watcher_debounce, "15s");
        assert!(receipt.restart_recommended);
        let stale = client
            .mutate(before.revision_id, UserSettingsMutationV1::default())
            .await
            .expect_err("stale revision must conflict");
        assert!(matches!(
            stale,
            UserSettingsAuthorityError::RevisionConflict { .. }
        ));
    }
}
