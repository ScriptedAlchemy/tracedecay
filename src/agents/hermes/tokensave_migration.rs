//! Legacy TokenSave-to-TraceDecay migration helpers.
//!
//! The Hermes integration used to install generated plugin files under
//! `plugins/tokensave` and write `tokensave` aliases into `config.yaml`.  The
//! steady-state lifecycle code should only deal with `tracedecay` installs;
//! this module centralizes the compatibility edge cases that detect legacy
//! layouts, preserve user files, and remove only generated legacy artifacts.

use std::path::{Path, PathBuf};

use crate::errors::Result;

/// Current and legacy plugin locations for one Hermes install scope.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct PluginLocations {
    pub(super) plugin_dir: PathBuf,
    pub(super) legacy_plugin_dir: PathBuf,
}

pub(super) fn profile_locations(home: &Path, profile: Option<&str>) -> PluginLocations {
    let profile_dir = super::hermes_profile_dir(home, profile);
    PluginLocations::from_profile_dir(&profile_dir)
}

pub(super) fn project_local_locations(project_path: &Path) -> PluginLocations {
    PluginLocations::from_profile_dir(&project_path.join(".hermes"))
}

impl PluginLocations {
    fn from_profile_dir(profile_dir: &Path) -> Self {
        let plugins_dir = profile_dir.join("plugins");
        Self {
            plugin_dir: plugins_dir.join("tracedecay"),
            legacy_plugin_dir: plugins_dir.join("tokensave"),
        }
    }
}

/// Returns the current tracedecay plugin path when either the current or the
/// legacy `TokenSave` plugin has a generated manifest below `hermes_root`.
pub(super) fn detected_plugin_dir(hermes_root: &Path) -> Option<PathBuf> {
    let locations = PluginLocations::from_profile_dir(hermes_root);
    let detected = locations.plugin_dir.join("plugin.yaml").is_file()
        || locations.legacy_plugin_dir.join("plugin.yaml").is_file();
    detected.then_some(locations.plugin_dir)
}

/// Removes generated legacy `TokenSave` plugin artifacts before an install.
///
/// This intentionally does not edit `config.yaml`; the subsequent steady-state
/// enable path migrates `tokensave` config aliases to `tracedecay` in one
/// backup-protected write while preserving unrelated user data.
pub(super) fn migrate_before_install(locations: &PluginLocations) -> Result<()> {
    remove_legacy_generated_plugin_if_present(&locations.legacy_plugin_dir)
}

/// Removes generated legacy `TokenSave` plugin artifacts found next to a current
/// plugin dir during `update-plugin`, without rewriting profile config.
pub(super) fn migrate_before_refresh(plugin_dir: &Path) -> Result<()> {
    if let Some(legacy_plugin_dir) = legacy_plugin_dir_for_current(plugin_dir) {
        remove_legacy_generated_plugin_if_present(&legacy_plugin_dir)?;
    }
    Ok(())
}

/// Returns a project pin from the legacy `TokenSave` plugin's owning profile
/// config when no current tracedecay pin exists.
pub(super) fn legacy_pinned_project_root(plugin_dir: &Path) -> Option<String> {
    legacy_plugin_dir_for_current(plugin_dir)
        .and_then(|legacy| super::effective_pinned_project_root(&legacy))
}

/// True when the legacy `TokenSave` plugin had a generated dashboard wrapper.
pub(super) fn legacy_dashboard_deployed(plugin_dir: &Path) -> bool {
    legacy_plugin_dir_for_current(plugin_dir)
        .as_deref()
        .is_some_and(super::dashboard_wrapper::is_deployed)
}

/// Removes legacy generated plugin files as part of uninstall after the current
/// plugin path has already disabled tracedecay/tokensave config aliases.
pub(super) fn remove_legacy_generated_plugin_if_present(legacy_plugin_dir: &Path) -> Result<()> {
    if legacy_plugin_dir.exists() {
        super::remove_generated_plugin_files(legacy_plugin_dir)?;
    }
    Ok(())
}

fn legacy_plugin_dir_for_current(plugin_dir: &Path) -> Option<PathBuf> {
    plugin_dir
        .parent()
        .map(|plugins_dir| plugins_dir.join("tokensave"))
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use tempfile::TempDir;

    use super::*;

    #[test]
    fn profile_locations_point_at_current_and_legacy_plugin_dirs() {
        let home = TempDir::new().unwrap();

        let locations = profile_locations(home.path(), Some("work"));

        assert_eq!(
            locations.plugin_dir,
            home.path().join(".hermes/profiles/work/plugins/tracedecay")
        );
        assert_eq!(
            locations.legacy_plugin_dir,
            home.path().join(".hermes/profiles/work/plugins/tokensave")
        );
    }

    #[test]
    fn detection_maps_legacy_manifest_to_current_plugin_dir() {
        let home = TempDir::new().unwrap();
        let legacy_dir = home.path().join(".hermes/plugins/tokensave");
        std::fs::create_dir_all(&legacy_dir).unwrap();
        std::fs::write(legacy_dir.join("plugin.yaml"), "name: tokensave\n").unwrap();

        assert_eq!(
            detected_plugin_dir(&home.path().join(".hermes")),
            Some(home.path().join(".hermes/plugins/tracedecay"))
        );
    }

    #[test]
    fn migration_removes_generated_legacy_files_but_preserves_user_files() {
        let home = TempDir::new().unwrap();
        let locations = profile_locations(home.path(), None);
        std::fs::create_dir_all(locations.legacy_plugin_dir.join("skills/tokensave")).unwrap();
        std::fs::write(
            locations.legacy_plugin_dir.join("plugin.yaml"),
            "name: tokensave\n",
        )
        .unwrap();
        std::fs::write(
            locations
                .legacy_plugin_dir
                .join("skills/tokensave/SKILL.md"),
            "generated skill\n",
        )
        .unwrap();
        std::fs::write(
            locations.legacy_plugin_dir.join("user-note.txt"),
            "keep me\n",
        )
        .unwrap();

        migrate_before_install(&locations).unwrap();

        assert!(!locations.legacy_plugin_dir.join("plugin.yaml").exists());
        assert!(!locations
            .legacy_plugin_dir
            .join("skills/tokensave/SKILL.md")
            .exists());
        assert_eq!(
            std::fs::read_to_string(locations.legacy_plugin_dir.join("user-note.txt")).unwrap(),
            "keep me\n"
        );
    }
}
