use std::collections::BTreeMap;
use std::fs::OpenOptions;
use std::io::Write;
#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt as _;
#[cfg(windows)]
use std::os::windows::ffi::OsStrExt as _;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::adapters::LspAdapterDefinition;
use super::error::{AnalyzerResult, AnalyzerRuntimeError};

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

pub async fn load_settings(dashboard_root: &Path) -> AnalyzerResult<CodeDiagnosticsSettings> {
    let path = settings_path(dashboard_root);
    match tokio::fs::read(&path).await {
        Ok(bytes) => serde_json::from_slice(&bytes).map_err(|error| {
            AnalyzerRuntimeError::new(format!(
                "failed to parse code diagnostics settings '{}': {error}",
                path.display()
            ))
        }),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            Ok(CodeDiagnosticsSettings::default())
        }
        Err(error) => Err(AnalyzerRuntimeError::new(format!(
            "failed to read code diagnostics settings '{}': {error}",
            path.display()
        ))),
    }
}

pub async fn save_settings(
    dashboard_root: &Path,
    settings: &CodeDiagnosticsSettings,
) -> AnalyzerResult<()> {
    let path = settings_path(dashboard_root);
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent).await.map_err(|error| {
            AnalyzerRuntimeError::new(format!(
                "failed to create code diagnostics settings directory '{}': {error}",
                parent.display()
            ))
        })?;
    }
    let bytes = serde_json::to_vec_pretty(settings).map_err(|error| {
        AnalyzerRuntimeError::new(format!(
            "failed to serialize code diagnostics settings: {error}"
        ))
    })?;
    if tokio::fs::try_exists(&path).await.unwrap_or(false) {
        let backup = path.with_extension("json.bak");
        tokio::fs::copy(&path, &backup).await.map_err(|error| {
            AnalyzerRuntimeError::new(format!(
                "failed to back up code diagnostics settings '{}' to '{}': {error}",
                path.display(),
                backup.display()
            ))
        })?;
    }
    let staged = path.with_extension("json.pending");
    let publish_path = path.clone();
    tokio::task::spawn_blocking(move || publish_settings(&staged, &publish_path, &bytes))
        .await
        .map_err(|error| {
            AnalyzerRuntimeError::new(format!(
                "code diagnostics settings write task failed: {error}"
            ))
        })?
}

fn publish_settings(staged: &Path, destination: &Path, bytes: &[u8]) -> AnalyzerResult<()> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        options.mode(0o600);
    }
    let mut created = false;
    let publish = (|| {
        let mut file = options.open(staged).map_err(|error| {
            AnalyzerRuntimeError::new(format!(
                "failed to stage code diagnostics settings '{}': {error}",
                staged.display()
            ))
        })?;
        created = true;
        file.write_all(bytes)
            .and_then(|()| file.sync_all())
            .map_err(|error| {
                AnalyzerRuntimeError::new(format!(
                    "failed to write code diagnostics settings '{}': {error}",
                    staged.display()
                ))
            })?;
        replace_settings_file(staged, destination)?;
        sync_settings_directory(destination)
    })();
    if publish.is_err() && created {
        let _ = std::fs::remove_file(staged);
    }
    publish
}

#[cfg(not(windows))]
fn replace_settings_file(staged: &Path, destination: &Path) -> AnalyzerResult<()> {
    std::fs::rename(staged, destination).map_err(|error| {
        AnalyzerRuntimeError::new(format!(
            "failed to publish code diagnostics settings '{}': {error}",
            destination.display()
        ))
    })
}

#[cfg(windows)]
fn replace_settings_file(staged: &Path, destination: &Path) -> AnalyzerResult<()> {
    const MOVEFILE_REPLACE_EXISTING: u32 = 0x1;
    const MOVEFILE_WRITE_THROUGH: u32 = 0x8;
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn MoveFileExW(existing: *const u16, replacement: *const u16, flags: u32) -> i32;
    }

    let existing = staged
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();
    let replacement = destination
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();
    let replaced = unsafe {
        MoveFileExW(
            existing.as_ptr(),
            replacement.as_ptr(),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
    };
    if replaced == 0 {
        return Err(AnalyzerRuntimeError::new(format!(
            "failed to publish code diagnostics settings '{}': {}",
            destination.display(),
            std::io::Error::last_os_error()
        )));
    }
    Ok(())
}

fn sync_settings_directory(path: &Path) -> AnalyzerResult<()> {
    let parent = path.parent().ok_or_else(|| {
        AnalyzerRuntimeError::new(format!(
            "code diagnostics settings path '{}' has no parent directory",
            path.display()
        ))
    })?;
    #[cfg(unix)]
    {
        std::fs::File::open(parent)
            .and_then(|directory| directory.sync_all())
            .map_err(|error| {
                AnalyzerRuntimeError::new(format!(
                    "failed to sync code diagnostics settings directory '{}': {error}",
                    parent.display()
                ))
            })?;
    }
    Ok(())
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
