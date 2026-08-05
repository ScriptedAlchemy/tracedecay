//! High-level Hermes plugin lifecycle orchestration.
//!
//! This module owns the sequencing for catalog-backed native activation,
//! update, and deactivation. The concrete filesystem/config mutations stay in sibling
//! helpers so the lifecycle path reads as ordered intent and preserves the
//! historical side-effect order.

use std::path::Path;

use crate::agents::InstallContext;
use crate::errors::Result;

pub(super) fn activate_catalog_plugin_profiles(ctx: &InstallContext) -> Result<()> {
    for profile_plugin_dir in super::profile_plugin_dirs(&ctx.home) {
        install_supported_plugin(&profile_plugin_dir, &ctx.tracedecay_bin, ctx.dashboard)?;
    }
    Ok(())
}

fn install_supported_plugin(
    plugin_dir: &Path,
    tracedecay_bin: &str,
    deploy_dashboard: bool,
) -> Result<()> {
    let existed = plugin_dir.join("plugin.yaml").is_file();
    if let Err(error) = super::install_plugin(plugin_dir, tracedecay_bin, deploy_dashboard) {
        if !existed && let Err(cleanup_error) = super::remove_generated_plugin_files(plugin_dir) {
            tracing::warn!(
                plugin_dir = %plugin_dir.display(),
                %cleanup_error,
                "failed to roll back incomplete Hermes plugin"
            );
        }
        return Err(error);
    }
    Ok(())
}

pub(super) fn deactivate_catalog_plugin_profiles(ctx: &InstallContext) -> Result<()> {
    for profile_plugin_dir in super::profile_plugin_dirs(&ctx.home) {
        super::uninstall_plugin(&profile_plugin_dir)?;
    }
    Ok(())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use std::path::Path;
    use tempfile::TempDir;

    use crate::agents::InstallContext;

    use super::*;

    const NEW_BIN: &str = "/new/bin/tracedecay";

    fn ctx(home: &Path, tracedecay_bin: &str) -> InstallContext {
        InstallContext {
            home: home.to_path_buf(),
            tracedecay_bin: tracedecay_bin.to_string(),
            tool_permissions: crate::agents::expected_tool_perms(),
            project_root: None,
            dashboard: true,
        }
    }

    fn text(path: &Path) -> String {
        std::fs::read_to_string(path)
            .unwrap_or_else(|e| panic!("failed to read {}: {e}", path.display()))
    }

    #[test]
    fn activation_writes_plugin_and_enables_profile_config() {
        let home = TempDir::new().unwrap();

        activate_catalog_plugin_profiles(&ctx(home.path(), NEW_BIN)).unwrap();
        let plugin_dir = home.path().join(".hermes/plugins/tracedecay");

        assert!(plugin_dir.join("plugin.yaml").is_file());
        assert!(plugin_dir.join("dashboard/manifest.json").is_file());
        let config = text(&home.path().join(".hermes/config.yaml"));
        assert!(
            config.contains("- tracedecay"),
            "config should enable plugin:\n{config}"
        );
        assert!(
            config.contains("provider: tracedecay"),
            "config should select tracedecay memory provider:\n{config}"
        );
        assert!(
            config.contains("engine: tracedecay"),
            "config should select tracedecay context engine:\n{config}"
        );
    }

    #[test]
    fn activation_configures_every_existing_hermes_profile() {
        let home = TempDir::new().unwrap();
        let redirected = TempDir::new().unwrap();
        let named = home.path().join(".hermes/profiles/work/plugins/tracedecay");
        let redirected_plugin = redirected.path().join("plugins/tracedecay");
        for plugin in [&named, &redirected_plugin] {
            std::fs::create_dir_all(plugin).unwrap();
            std::fs::write(plugin.join("plugin.yaml"), "name: tracedecay\n").unwrap();
            std::fs::write(
                plugin
                    .parent()
                    .unwrap()
                    .parent()
                    .unwrap()
                    .join("config.yaml"),
                "plugins:\n  enabled:\n    - tracedecay\n",
            )
            .unwrap();
        }

        activate_catalog_plugin_profiles(&ctx(home.path(), NEW_BIN)).unwrap();

        assert!(
            home.path()
                .join(".hermes/plugins/tracedecay/plugin.yaml")
                .is_file()
        );
        assert!(named.join("plugin.yaml").exists());
        assert!(redirected_plugin.join("plugin.yaml").exists());
        let named_config = text(&home.path().join(".hermes/profiles/work/config.yaml"));
        assert!(named_config.contains("- tracedecay"));
        assert!(named_config.contains("provider: tracedecay"));
        assert!(named_config.contains("engine: tracedecay"));
        assert!(text(&redirected.path().join("config.yaml")).contains("tracedecay"));
    }

    #[test]
    fn deactivation_removes_generated_current_plugin_state() {
        let home = TempDir::new().unwrap();
        activate_catalog_plugin_profiles(&ctx(home.path(), NEW_BIN)).unwrap();

        deactivate_catalog_plugin_profiles(&ctx(home.path(), NEW_BIN)).unwrap();
        let plugin_dir = home.path().join(".hermes/plugins/tracedecay");

        assert!(!plugin_dir.join("plugin.yaml").exists());
        let config = text(&home.path().join(".hermes/config.yaml"));
        assert!(
            !config.contains("tracedecay"),
            "uninstall should disable tracedecay:\n{config}"
        );
    }

    #[test]
    fn activation_rolls_back_new_artifacts_after_config_validation_failure() {
        let home = TempDir::new().unwrap();
        let config_path = home.path().join(".hermes/config.yaml");
        std::fs::create_dir_all(config_path.parent().unwrap()).unwrap();
        std::fs::write(&config_path, "memory:\n  provider: other\n").unwrap();

        let err = activate_catalog_plugin_profiles(&ctx(home.path(), NEW_BIN)).unwrap_err();

        assert!(
            err.to_string()
                .contains("Hermes memory provider already configured"),
            "unexpected error: {err}"
        );
        assert!(
            !home
                .path()
                .join(".hermes/plugins/tracedecay/plugin.yaml")
                .exists()
        );
        assert_eq!(text(&config_path), "memory:\n  provider: other\n");
    }
}
