//! High-level Hermes plugin lifecycle orchestration.
//!
//! This module owns the sequencing for install, project-local install, update,
//! and uninstall. The concrete filesystem/config mutations stay in sibling
//! helpers so the lifecycle path reads as ordered intent and preserves the
//! historical side-effect order.

use std::path::{Path, PathBuf};

use crate::agents::{InstallContext, UpdatePluginOutcome};
use crate::errors::Result;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct InstallOutcome {
    pub plugin_dir: PathBuf,
    pub legacy_plugin_dir: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct UninstallOutcome {
    pub plugin_dir: PathBuf,
    pub legacy_plugin_dir: PathBuf,
}

pub(super) fn install(ctx: &InstallContext) -> Result<InstallOutcome> {
    let profile = super::normalize_profile(ctx.profile.as_deref())?;
    let locations = super::tokensave_migration::profile_locations(&ctx.home, profile.as_deref());
    let plugin_dir = locations.plugin_dir.clone();
    let legacy_plugin_dir = locations.legacy_plugin_dir.clone();

    super::tokensave_migration::migrate_before_install(&locations)?;
    super::install_plugin(
        &plugin_dir,
        &ctx.tracedecay_bin,
        ctx.project_root.as_deref(),
        ctx.dashboard,
    )?;

    eprintln!();
    eprintln!("Setup complete. Next steps:");
    eprintln!("  1. cd into your project and run: tracedecay init");
    eprintln!("  2. Start Hermes — tracedecay plugin tools are now available");

    Ok(InstallOutcome {
        plugin_dir,
        legacy_plugin_dir,
    })
}

pub(super) fn install_local(ctx: &InstallContext, project_path: &Path) -> Result<InstallOutcome> {
    let profile = super::normalize_profile(ctx.profile.as_deref())?;
    let locations = match profile.as_deref() {
        Some(profile) => super::tokensave_migration::profile_locations(&ctx.home, Some(profile)),
        None => super::tokensave_migration::project_local_locations(project_path),
    };
    let plugin_dir = locations.plugin_dir.clone();
    let legacy_plugin_dir = locations.legacy_plugin_dir.clone();

    super::tokensave_migration::migrate_before_install(&locations)?;
    super::install_plugin(
        &plugin_dir,
        &ctx.tracedecay_bin,
        ctx.project_root.as_deref(),
        ctx.dashboard,
    )?;

    if profile.is_none() {
        eprintln!(
            "  Launch Hermes with HERMES_HOME={} so it reads this project-local plugin and memory provider config.",
            project_path.join(".hermes").display()
        );
    }

    Ok(InstallOutcome {
        plugin_dir,
        legacy_plugin_dir,
    })
}

pub(super) fn update_plugin(ctx: &InstallContext) -> Result<UpdatePluginOutcome> {
    let refreshed = refresh_installed_plugins(&ctx.home, &ctx.tracedecay_bin)?;
    if refreshed.is_empty() {
        Ok(UpdatePluginOutcome::NotInstalled)
    } else {
        Ok(UpdatePluginOutcome::Refreshed(refreshed))
    }
}

/// Refreshes generated plugin artifacts for every detected Hermes install.
///
/// Detection covers the default profile (`~/.hermes`), every named profile
/// (`~/.hermes/profiles/*`), a `HERMES_HOME` override, and a project-local
/// `.hermes` in the current directory — a plugin install is "detected" when
/// either its current or legacy generated `plugin.yaml` exists. For each
/// install the existing `plugins.tracedecay.project_root` pin is read from the
/// profile config (with a legacy `plugins.tokensave.project_root` fallback) and
/// re-baked into refreshed artifacts. `update-plugin` does not rewrite
/// `config.yaml`; full config alias migration happens on install/reinstall.
fn refresh_installed_plugins(home: &Path, tracedecay_bin: &str) -> Result<Vec<PathBuf>> {
    let mut refreshed = Vec::new();
    for plugin_dir in super::detected_plugin_dirs(home) {
        let pinned_project_root = super::effective_pinned_project_root(&plugin_dir)
            .or_else(|| super::tokensave_migration::legacy_pinned_project_root(&plugin_dir));
        let had_dashboard = super::dashboard_wrapper::is_deployed(&plugin_dir)
            || super::tokensave_migration::legacy_dashboard_deployed(&plugin_dir);

        super::tokensave_migration::migrate_before_refresh(&plugin_dir)?;
        super::write_plugin_files(&plugin_dir, tracedecay_bin)?;
        super::dashboard_wrapper::refresh_if_previously_deployed(
            &plugin_dir,
            tracedecay_bin,
            pinned_project_root.as_deref(),
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
    let profile = super::normalize_profile(ctx.profile.as_deref())?;
    let locations = super::tokensave_migration::profile_locations(&ctx.home, profile.as_deref());
    let plugin_dir = locations.plugin_dir.clone();
    let legacy_plugin_dir = locations.legacy_plugin_dir.clone();

    super::uninstall_plugin(&plugin_dir)?;
    super::tokensave_migration::remove_legacy_generated_plugin_if_present(&legacy_plugin_dir)?;

    eprintln!();
    eprintln!("Uninstall complete. Tracedecay has been removed from Hermes.");
    eprintln!("Restart Hermes for changes to take effect.");

    Ok(UninstallOutcome {
        plugin_dir,
        legacy_plugin_dir,
    })
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
            profile: None,
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
        std::env::set_var("HERMES_HOME", hermes_home);
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(f));
        if let Some(previous) = previous {
            std::env::set_var("HERMES_HOME", previous);
        } else {
            std::env::remove_var("HERMES_HOME");
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
    fn update_migrates_legacy_install_artifacts_without_rewriting_config() {
        let home = TempDir::new().unwrap();
        let project = TempDir::new().unwrap();
        let legacy_dir = home.path().join(".hermes/plugins/tokensave");
        std::fs::create_dir_all(legacy_dir.join("dashboard")).unwrap();
        std::fs::write(legacy_dir.join("plugin.yaml"), "name: tokensave\n").unwrap();
        std::fs::write(legacy_dir.join("dashboard/manifest.json"), "{}\n").unwrap();
        let config_path = home.path().join(".hermes/config.yaml");
        let pinned_root = serde_json::to_string(&project.path().display().to_string()).unwrap();
        std::fs::write(
            &config_path,
            format!(
                "plugins:\n  enabled:\n    - tokensave\n  tokensave:\n    project_root: {pinned_root}\nmemory:\n  provider: tokensave\ncontext:\n  engine: tokensave\n# user data\nui:\n  theme: dark\n"
            ),
        )
        .unwrap();
        let config_before = std::fs::read(&config_path).unwrap();

        let outcome = with_hermes_home(&home.path().join(".hermes"), || {
            update_plugin(&ctx(home.path(), NEW_BIN)).unwrap()
        });

        let plugin_dir = home.path().join(".hermes/plugins/tracedecay");
        assert!(
            matches!(outcome, UpdatePluginOutcome::Refreshed(paths) if paths == vec![plugin_dir.clone()])
        );
        assert_eq!(std::fs::read(&config_path).unwrap(), config_before);
        assert!(!legacy_dir.join("plugin.yaml").exists());
        assert!(plugin_dir.join("plugin.yaml").is_file());
        let api = text(&plugin_dir.join("dashboard/plugin_api.py"));
        assert!(api.contains(NEW_BIN));
        assert!(api.contains(&pinned_root));
    }

    #[test]
    fn uninstall_removes_generated_current_and_legacy_plugin_state() {
        let home = TempDir::new().unwrap();
        install(&ctx(home.path(), NEW_BIN)).unwrap();
        let legacy_dir = home.path().join(".hermes/plugins/tokensave");
        std::fs::create_dir_all(&legacy_dir).unwrap();
        std::fs::write(legacy_dir.join("plugin.yaml"), "name: tokensave\n").unwrap();

        let outcome = uninstall(&ctx(home.path(), NEW_BIN)).unwrap();

        assert_eq!(
            outcome.plugin_dir,
            home.path().join(".hermes/plugins/tracedecay")
        );
        assert!(!outcome.plugin_dir.join("plugin.yaml").exists());
        assert!(!outcome.legacy_plugin_dir.exists());
        let config = text(&home.path().join(".hermes/config.yaml"));
        assert!(
            !config.contains("tracedecay"),
            "uninstall should disable tracedecay:\n{config}"
        );
    }

    #[test]
    fn install_recovers_from_legacy_partial_state_before_writing_current_plugin() {
        let home = TempDir::new().unwrap();
        let legacy_dir = home.path().join(".hermes/plugins/tokensave");
        std::fs::create_dir_all(&legacy_dir).unwrap();
        std::fs::write(legacy_dir.join("plugin.yaml"), "name: tokensave\n").unwrap();

        let outcome = install(&ctx(home.path(), NEW_BIN)).unwrap();

        assert!(
            !legacy_dir.exists(),
            "legacy generated plugin dir should be removed first"
        );
        assert!(outcome.plugin_dir.join("plugin.yaml").is_file());
    }

    #[test]
    fn install_migrates_legacy_config_once_and_preserves_user_data() {
        let home = TempDir::new().unwrap();
        let project = TempDir::new().unwrap();
        let legacy_dir = home.path().join(".hermes/plugins/tokensave");
        std::fs::create_dir_all(&legacy_dir).unwrap();
        std::fs::write(legacy_dir.join("plugin.yaml"), "name: tokensave\n").unwrap();
        let config_path = home.path().join(".hermes/config.yaml");
        let pinned_root = serde_json::to_string(&project.path().display().to_string()).unwrap();
        let original = format!(
            "plugins:\n  enabled:\n    - tokensave\n  tokensave:\n    project_root: {pinned_root}\nmemory:\n  provider: tokensave\ncontext:\n  engine: tokensave\n# user data\nui:\n  theme: dark\n"
        );
        std::fs::write(&config_path, &original).unwrap();

        install(&ctx(home.path(), NEW_BIN)).unwrap();
        let first = text(&config_path);
        install(&ctx(home.path(), NEW_BIN)).unwrap();
        let second = text(&config_path);

        assert_eq!(first, second, "migration should be idempotent");
        assert!(second.contains("# user data\nui:\n  theme: dark\n"));
        assert!(second.contains("- tracedecay"));
        assert!(second.contains("provider: tracedecay"));
        assert!(second.contains("engine: tracedecay"));
        assert!(!second.contains("tokensave"));
        assert_eq!(text(&home.path().join(".hermes/config.yaml.bak")), original);
    }

    #[test]
    fn install_propagates_config_validation_failure_after_artifact_write() {
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
            home.path()
                .join(".hermes/plugins/tracedecay/plugin.yaml")
                .is_file(),
            "artifact write happens before config validation and must remain in that order"
        );
        assert_eq!(text(&config_path), "memory:\n  provider: other\n");
    }
}
