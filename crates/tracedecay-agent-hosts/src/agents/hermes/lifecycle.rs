//! High-level Hermes plugin lifecycle orchestration.
//!
//! This module owns the sequencing for user-level install, update, and
//! uninstall. The concrete filesystem/config mutations stay in sibling
//! helpers so the lifecycle path reads as ordered intent and preserves the
//! historical side-effect order.

use std::path::{Path, PathBuf};

use crate::agents::{InstallContext, UpdatePluginOutcome};
use crate::errors::Result;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct InstallOutcome {
    pub plugin_dir: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct UninstallOutcome {
    pub plugin_dir: PathBuf,
}

pub(super) fn install(ctx: &InstallContext) -> Result<InstallOutcome> {
    let plugin_dir = ctx.home.join(".hermes/plugins/tracedecay");

    for profile_plugin_dir in super::profile_plugin_dirs(&ctx.home) {
        install_supported_plugin(&profile_plugin_dir, &ctx.tracedecay_bin, ctx.dashboard)?;
    }

    eprintln!();
    eprintln!("Setup complete. Next steps:");
    eprintln!("  1. cd into your project and run: tracedecay init");
    eprintln!("  2. Start Hermes — tracedecay plugin tools are now available");

    Ok(InstallOutcome { plugin_dir })
}

fn install_supported_plugin(
    plugin_dir: &Path,
    tracedecay_bin: &str,
    deploy_dashboard: bool,
) -> Result<()> {
    let existed = plugin_dir.join("plugin.yaml").is_file();
    if let Err(error) = super::install_plugin(plugin_dir, tracedecay_bin, deploy_dashboard) {
        if !existed && let Err(cleanup_error) = super::remove_generated_plugin_files(plugin_dir) {
            eprintln!(
                "  warning: failed to roll back incomplete Hermes plugin {}: {cleanup_error}",
                plugin_dir.display()
            );
        }
        return Err(error);
    }
    Ok(())
}

pub(super) fn update_plugin(ctx: &InstallContext) -> Result<UpdatePluginOutcome> {
    let refreshed = refresh_installed_plugins(&ctx.home, &ctx.tracedecay_bin)?;
    if refreshed.is_empty() {
        Ok(UpdatePluginOutcome::NotInstalled)
    } else {
        Ok(UpdatePluginOutcome::Refreshed(refreshed))
    }
}

/// Refreshes the generated user-level plugin without rewriting config.yaml.
fn refresh_installed_plugins(home: &Path, tracedecay_bin: &str) -> Result<Vec<PathBuf>> {
    let mut refreshed = Vec::new();
    for plugin_dir in super::detected_plugin_dirs(home) {
        let had_dashboard = super::dashboard_wrapper::is_deployed(&plugin_dir);

        super::write_plugin_files(&plugin_dir, tracedecay_bin)?;
        super::dashboard_wrapper::refresh_if_previously_deployed(
            &plugin_dir,
            tracedecay_bin,
            had_dashboard,
        )?;
        eprintln!(
            "\x1b[32m✔\x1b[0m Refreshed Hermes tracedecay plugin at {}",
            plugin_dir.display()
        );
        refreshed.push(plugin_dir);
    }
    Ok(refreshed)
}

pub(super) fn uninstall(ctx: &InstallContext) -> Result<UninstallOutcome> {
    let plugin_dir = ctx.home.join(".hermes/plugins/tracedecay");

    for profile_plugin_dir in super::profile_plugin_dirs(&ctx.home) {
        super::uninstall_plugin(&profile_plugin_dir)?;
    }

    eprintln!();
    eprintln!("Uninstall complete. Tracedecay has been removed from Hermes.");
    eprintln!("Restart Hermes for changes to take effect.");

    Ok(UninstallOutcome { plugin_dir })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use std::path::Path;
    use std::sync::{Mutex, OnceLock};

    use tempfile::TempDir;

    use crate::agents::{InstallContext, UpdatePluginOutcome};

    use super::*;

    const OLD_BIN: &str = "/old/bin/tracedecay";
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

    fn with_hermes_home<T>(hermes_home: &Path, f: impl FnOnce() -> T) -> T {
        static ENV_LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        let _guard = ENV_LOCK.get_or_init(|| Mutex::new(())).lock().unwrap();
        let previous = std::env::var_os("HERMES_HOME");
        unsafe {
            std::env::set_var("HERMES_HOME", hermes_home);
        }
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(f));
        unsafe {
            if let Some(previous) = previous {
                std::env::set_var("HERMES_HOME", previous);
            } else {
                std::env::remove_var("HERMES_HOME");
            }
        }
        match result {
            Ok(value) => value,
            Err(payload) => std::panic::resume_unwind(payload),
        }
    }

    #[test]
    fn fresh_install_writes_plugin_and_enables_profile_config() {
        let home = TempDir::new().unwrap();

        let outcome = install(&ctx(home.path(), NEW_BIN)).unwrap();

        assert_eq!(
            outcome.plugin_dir,
            home.path().join(".hermes/plugins/tracedecay")
        );
        assert!(outcome.plugin_dir.join("plugin.yaml").is_file());
        assert!(outcome.plugin_dir.join("dashboard/manifest.json").is_file());
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
    fn update_existing_install_rebakes_artifacts_without_rewriting_config() {
        let home = TempDir::new().unwrap();
        let project = TempDir::new().unwrap();
        let mut install_ctx = ctx(home.path(), OLD_BIN);
        install_ctx.project_root = Some(project.path().to_path_buf());
        install(&install_ctx).unwrap();
        let config_path = home.path().join(".hermes/config.yaml");
        let before = std::fs::read(&config_path).unwrap();

        let outcome = with_hermes_home(&home.path().join(".hermes"), || {
            update_plugin(&ctx(home.path(), NEW_BIN)).unwrap()
        });

        let plugin_dir = home.path().join(".hermes/plugins/tracedecay");
        assert!(
            matches!(outcome, UpdatePluginOutcome::Refreshed(paths) if paths == vec![plugin_dir.clone()])
        );
        assert_eq!(std::fs::read(&config_path).unwrap(), before);
        assert!(text(&plugin_dir.join("tools.py")).contains(NEW_BIN));
        assert!(!text(&plugin_dir.join("tools.py")).contains(OLD_BIN));
        assert!(text(&plugin_dir.join("dashboard/plugin_api.py")).contains(NEW_BIN));
    }

    #[test]
    fn install_configures_every_existing_hermes_profile() {
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

        let outcome = install(&ctx(home.path(), NEW_BIN)).unwrap();

        assert!(outcome.plugin_dir.join("plugin.yaml").is_file());
        assert!(named.join("plugin.yaml").exists());
        assert!(redirected_plugin.join("plugin.yaml").exists());
        let named_config = text(&home.path().join(".hermes/profiles/work/config.yaml"));
        assert!(named_config.contains("- tracedecay"));
        assert!(named_config.contains("provider: tracedecay"));
        assert!(named_config.contains("engine: tracedecay"));
        assert!(text(&redirected.path().join("config.yaml")).contains("tracedecay"));
    }

    #[test]
    fn uninstall_removes_generated_current_plugin_state() {
        let home = TempDir::new().unwrap();
        install(&ctx(home.path(), NEW_BIN)).unwrap();

        let outcome = uninstall(&ctx(home.path(), NEW_BIN)).unwrap();

        assert_eq!(
            outcome.plugin_dir,
            home.path().join(".hermes/plugins/tracedecay")
        );
        assert!(!outcome.plugin_dir.join("plugin.yaml").exists());
        let config = text(&home.path().join(".hermes/config.yaml"));
        assert!(
            !config.contains("tracedecay"),
            "uninstall should disable tracedecay:\n{config}"
        );
    }

    #[test]
    fn install_rolls_back_new_artifacts_after_config_validation_failure() {
        let home = TempDir::new().unwrap();
        let config_path = home.path().join(".hermes/config.yaml");
        std::fs::create_dir_all(config_path.parent().unwrap()).unwrap();
        std::fs::write(&config_path, "memory:\n  provider: other\n").unwrap();

        let err = install(&ctx(home.path(), NEW_BIN)).unwrap_err();

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
