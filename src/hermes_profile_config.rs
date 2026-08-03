//! Compatibility façade for the Hermes profile configuration kernel.
//!
//! Parsing and deterministic YAML patching live in `tracedecay-agent-hosts`.
//! This façade retains the root crate's filesystem, backup, and error policy
//! used by the Hermes lifecycle integration.

use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use crate::agents::backup_config_file;
use crate::errors::{Result, TraceDecayError};
use tracedecay_agent_hosts::agents::hermes::profile_config::{
    disable_plugin_config, enable_plugin_config,
    read_config_pinned_project_root as parse_config_pinned_project_root,
};

/// Reads the removed `plugins.tracedecay.project_root` setting solely as
/// provenance for one-time data migration and transcript import.
///
/// Keep this path-based façade in the root crate so filesystem and error policy
/// do not leak into the reusable Hermes profile kernel.
pub(crate) fn read_config_pinned_project_root(config_path: &Path) -> Option<String> {
    let config = std::fs::read_to_string(config_path).ok()?;
    parse_config_pinned_project_root(&config)
}

pub(super) fn enable_plugin(config_path: &Path) -> Result<bool> {
    let existing = std::fs::read_to_string(config_path).unwrap_or_default();
    let updated = enable_plugin_config(&existing).map_err(|message| TraceDecayError::Config {
        message: format!(
            "{message} in {}.\nFix the config by hand, then re-run: tracedecay install --agent hermes",
            config_path.display()
        ),
    })?;
    if updated != existing {
        write_config_file(config_path, &updated)?;
    }
    Ok(true)
}

pub(super) fn disable_plugin(config_path: &Path) -> Result<()> {
    let Ok(existing) = std::fs::read_to_string(config_path) else {
        return Ok(());
    };
    let updated = disable_plugin_config(&existing).map_err(|message| TraceDecayError::Config {
        message: format!(
            "{message} in {}; leaving Hermes plugin files in place",
            config_path.display()
        ),
    })?;
    if updated != existing {
        write_config_file(config_path, &updated)?;
    }
    Ok(())
}

fn write_config_file(path: &Path, contents: &str) -> Result<()> {
    let current = match std::fs::read_to_string(path) {
        Ok(current) => Some(current),
        Err(e) if e.kind() == ErrorKind::NotFound => None,
        Err(e) => {
            return Err(TraceDecayError::Config {
                message: format!("failed to read {}: {e}", path.display()),
            });
        }
    };
    if current.as_deref() == Some(contents) {
        return Ok(());
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| TraceDecayError::Config {
            message: format!("failed to create {}: {e}", parent.display()),
        })?;
    }
    let backup = backup_config_file(path)?;
    let new_path = PathBuf::from(format!("{}.new", path.display()));
    if let Err(e) = std::fs::write(&new_path, contents) {
        std::fs::remove_file(&new_path).ok();
        return Err(TraceDecayError::Config {
            message: format!("failed to write {}: {e}", new_path.display()),
        });
    }
    if let Err(e) = std::fs::rename(&new_path, path) {
        std::fs::remove_file(&new_path).ok();
        let backup_hint = backup
            .as_ref()
            .map(|path| format!(" Backup is at {}.", path.display()))
            .unwrap_or_default();
        return Err(TraceDecayError::Config {
            message: format!(
                "failed to replace {} with {}: {e}.{backup_hint}",
                path.display(),
                new_path.display()
            ),
        });
    }
    Ok(())
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    use tempfile::TempDir;

    fn read(path: &Path) -> String {
        std::fs::read_to_string(path)
            .unwrap_or_else(|err| panic!("failed to read {}: {err}", path.display()))
    }

    #[test]
    fn enable_plugin_backs_up_existing_config_before_write() {
        let dir = TempDir::new().unwrap();
        let config = dir.path().join("config.yaml");
        let original = "theme: dark\nplugins:\n  enabled:\n    - other\n";
        std::fs::write(&config, original).unwrap();

        enable_plugin(&config).unwrap();

        let backup = dir.path().join("config.yaml.bak");
        assert!(backup.exists());
        assert_eq!(read(&backup), original);
    }

    #[test]
    fn read_project_pin_decodes_yaml_scalars() {
        let dir = TempDir::new().unwrap();
        let config = dir.path().join("config.yaml");
        std::fs::write(
            &config,
            "plugins:\n  tracedecay:\n    project_root: '/repo/it''s-ok'\n",
        )
        .unwrap();
        assert_eq!(
            read_config_pinned_project_root(&config).as_deref(),
            Some("/repo/it's-ok")
        );
    }
}
