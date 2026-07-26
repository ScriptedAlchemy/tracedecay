use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::diagnostics::lsp::adapters::LspAdapterDefinition;
use crate::errors::{Result, TraceDecayError};

const SETTINGS_FILENAME: &str = "code_diagnostics_settings.json";

/// Daemon-owned idle whole-project diagnostics mode.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IdleBackfillMode {
    Off,
    #[default]
    Idle,
}

/// Per-language Code Diagnostics settings.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LanguageDiagnosticsSettings {
    #[serde(default = "default_enabled")]
    pub enabled: bool,
    #[serde(default)]
    pub command_override: Option<String>,
}

impl Default for LanguageDiagnosticsSettings {
    fn default() -> Self {
        Self {
            enabled: default_enabled(),
            command_override: None,
        }
    }
}

/// Project-scoped Code Diagnostics settings persisted by the daemon authority.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CodeDiagnosticsSettings {
    #[serde(default)]
    pub idle_backfill: IdleBackfillMode,
    #[serde(default)]
    pub languages: BTreeMap<String, LanguageDiagnosticsSettings>,
    #[serde(default)]
    pub custom_adapters: Vec<LspAdapterDefinition>,
}

impl Default for CodeDiagnosticsSettings {
    fn default() -> Self {
        Self {
            idle_backfill: IdleBackfillMode::Idle,
            languages: BTreeMap::new(),
            custom_adapters: Vec::new(),
        }
    }
}

impl CodeDiagnosticsSettings {
    pub fn language_enabled(&self, language: &str) -> bool {
        self.languages
            .get(language)
            .map_or_else(default_enabled, |settings| settings.enabled)
    }

    pub fn set_language_enabled(&mut self, language: &str, enabled: bool) {
        self.languages
            .entry(language.to_string())
            .or_default()
            .enabled = enabled;
    }

    pub fn command_for(&self, language: &str, default_command: &str) -> String {
        self.languages
            .get(language)
            .and_then(|settings| settings.command_override.as_deref())
            .map(str::trim)
            .filter(|command| !command.is_empty())
            .unwrap_or(default_command)
            .to_string()
    }
}

pub fn settings_path(dashboard_root: &Path) -> PathBuf {
    dashboard_root.join(SETTINGS_FILENAME)
}

pub async fn load_settings(dashboard_root: &Path) -> Result<CodeDiagnosticsSettings> {
    let path = settings_path(dashboard_root);
    match tokio::fs::read(&path).await {
        Ok(bytes) => serde_json::from_slice(&bytes).map_err(|e| TraceDecayError::Config {
            message: format!(
                "failed to parse code diagnostics settings '{}': {e}",
                path.display()
            ),
        }),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            Ok(CodeDiagnosticsSettings::default())
        }
        Err(e) => Err(TraceDecayError::Config {
            message: format!(
                "failed to read code diagnostics settings '{}': {e}",
                path.display()
            ),
        }),
    }
}

pub async fn save_settings(
    dashboard_root: &Path,
    settings: &CodeDiagnosticsSettings,
) -> Result<()> {
    let path = settings_path(dashboard_root);
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .map_err(|e| TraceDecayError::Config {
                message: format!(
                    "failed to create code diagnostics settings directory '{}': {e}",
                    parent.display()
                ),
            })?;
    }
    let bytes = serde_json::to_vec_pretty(settings).map_err(|e| TraceDecayError::Config {
        message: format!("failed to serialize code diagnostics settings: {e}"),
    })?;
    if tokio::fs::try_exists(&path).await.unwrap_or(false) {
        let backup = path.with_extension("json.bak");
        tokio::fs::copy(&path, &backup)
            .await
            .map_err(|e| TraceDecayError::Config {
                message: format!(
                    "failed to back up code diagnostics settings '{}' to '{}': {e}",
                    path.display(),
                    backup.display()
                ),
            })?;
    }
    let staged = path.with_extension("json.pending");
    let publish_path = path.clone();
    tokio::task::spawn_blocking(move || {
        crate::db::DatabaseAuthority::publish_record_atomically(
            &staged,
            &publish_path,
            &bytes,
            "code diagnostics settings",
        )
    })
    .await
    .map_err(|error| TraceDecayError::Config {
        message: format!("code diagnostics settings write task failed: {error}"),
    })?
    .map_err(|e| TraceDecayError::Config {
        message: format!(
            "failed to publish code diagnostics settings '{}': {e}",
            path.display()
        ),
    })
}

fn default_enabled() -> bool {
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn save_settings_atomically_replaces_an_existing_file() {
        let temp = tempfile::tempdir().unwrap();
        let mut initial = CodeDiagnosticsSettings::default();
        initial.set_language_enabled("rust", false);
        save_settings(temp.path(), &initial).await.unwrap();

        let mut replacement = CodeDiagnosticsSettings::default();
        replacement.set_language_enabled("rust", true);
        save_settings(temp.path(), &replacement).await.unwrap();

        assert_eq!(load_settings(temp.path()).await.unwrap(), replacement);
        let backup = settings_path(temp.path()).with_extension("json.bak");
        let backup: CodeDiagnosticsSettings =
            serde_json::from_slice(&tokio::fs::read(backup).await.unwrap()).unwrap();
        assert_eq!(backup, initial);
    }
}
