//! Hermes registration projection for receipt-deployed plugin artifacts.
//!
//! The component catalog exclusively deploys the default profile's core
//! plugin bytes. This module activates that exact deployment in Hermes config
//! and projects it into existing named profiles, whose independent plugin
//! directories are registration state covered by the catalog transaction.

use crate::agents::InstallContext;
use crate::errors::Result;

pub(super) fn activate_deployed_plugin_registration(ctx: &InstallContext) -> Result<()> {
    let deployed_plugin_dir = ctx.home.join(".hermes/plugins/tracedecay");
    let profile_root = crate::automation::skill_targets::profile_root_for_agent_home(&ctx.home);
    for profile_plugin_dir in super::profile_plugin_dirs(&ctx.home) {
        super::activate_deployed_plugin_profile(
            &deployed_plugin_dir,
            &profile_plugin_dir,
            &ctx.tracedecay_bin,
            ctx.dashboard,
            &profile_root,
        )?;
    }
    Ok(())
}

pub(super) fn deactivate_deployed_plugin_registration(ctx: &InstallContext) -> Result<()> {
    let deployed_plugin_dir = ctx.home.join(".hermes/plugins/tracedecay");
    for profile_plugin_dir in super::profile_plugin_dirs(&ctx.home) {
        super::deactivate_deployed_plugin_profile(&deployed_plugin_dir, &profile_plugin_dir)?;
    }
    Ok(())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use std::collections::BTreeMap;
    use std::path::Path;
    use tempfile::TempDir;

    use crate::agents::InstallContext;

    use super::*;

    const NEW_BIN: &str = "/new/bin/tracedecay";

    fn ctx(home: &Path, tracedecay_bin: &str, dashboard: bool) -> InstallContext {
        InstallContext {
            home: home.to_path_buf(),
            tracedecay_bin: tracedecay_bin.to_string(),
            tool_permissions: crate::agents::expected_tool_perms(),
            project_root: None,
            dashboard,
        }
    }

    fn text(path: &Path) -> String {
        std::fs::read_to_string(path)
            .unwrap_or_else(|e| panic!("failed to read {}: {e}", path.display()))
    }

    fn seed_deployed_plugin(plugin_dir: &Path) -> BTreeMap<String, Vec<u8>> {
        super::super::managed_plugin_paths(plugin_dir)
            .into_iter()
            .enumerate()
            .map(|(index, path)| {
                let contents = format!("feedback-target-{index}\n").into_bytes();
                std::fs::create_dir_all(path.parent().unwrap()).unwrap();
                std::fs::write(&path, &contents).unwrap();
                (
                    path.strip_prefix(plugin_dir)
                        .unwrap()
                        .to_string_lossy()
                        .into_owned(),
                    contents,
                )
            })
            .collect()
    }

    fn assert_deployed_plugin_bytes(plugin_dir: &Path, expected: &BTreeMap<String, Vec<u8>>) {
        for (relative, contents) in expected {
            assert_eq!(
                std::fs::read(plugin_dir.join(relative)).unwrap().as_slice(),
                contents.as_slice(),
                "{} did not preserve the deployed target",
                plugin_dir.join(relative).display()
            );
        }
    }

    #[test]
    fn activation_preserves_deployed_plugin_and_enables_profile_config() {
        let home = TempDir::new().unwrap();
        let plugin_dir = home.path().join(".hermes/plugins/tracedecay");
        let deployed = seed_deployed_plugin(&plugin_dir);

        activate_deployed_plugin_registration(&ctx(home.path(), NEW_BIN, true)).unwrap();

        assert_deployed_plugin_bytes(&plugin_dir, &deployed);
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
    fn activation_without_dashboard_leaves_no_wrapper_or_child() {
        let home = TempDir::new().unwrap();
        let plugin_dir = home.path().join(".hermes/plugins/tracedecay");
        let deployed = seed_deployed_plugin(&plugin_dir);

        activate_deployed_plugin_registration(&ctx(home.path(), NEW_BIN, false)).unwrap();

        assert_deployed_plugin_bytes(&plugin_dir, &deployed);
        assert!(!plugin_dir.join("dashboard/manifest.json").exists());
        assert!(!plugin_dir.join("dashboard/plugin_api.py").exists());
        assert!(!plugin_dir.join("dashboard/dist/index.js").exists());
    }

    #[test]
    fn activation_configures_every_existing_hermes_profile() {
        let home = TempDir::new().unwrap();
        let deployed_plugin = home.path().join(".hermes/plugins/tracedecay");
        let deployed = seed_deployed_plugin(&deployed_plugin);
        let named = home.path().join(".hermes/profiles/work/plugins/tracedecay");
        std::fs::create_dir_all(named.parent().unwrap().parent().unwrap()).unwrap();

        activate_deployed_plugin_registration(&ctx(home.path(), NEW_BIN, true)).unwrap();

        assert_deployed_plugin_bytes(&deployed_plugin, &deployed);
        assert_deployed_plugin_bytes(&named, &deployed);
        let named_config = text(&home.path().join(".hermes/profiles/work/config.yaml"));
        assert!(named_config.contains("- tracedecay"));
        assert!(named_config.contains("provider: tracedecay"));
        assert!(named_config.contains("engine: tracedecay"));
    }

    #[test]
    fn deactivation_removes_registration_without_deployed_plugin_bytes() {
        let home = TempDir::new().unwrap();
        let plugin_dir = home.path().join(".hermes/plugins/tracedecay");
        let deployed = seed_deployed_plugin(&plugin_dir);
        activate_deployed_plugin_registration(&ctx(home.path(), NEW_BIN, true)).unwrap();

        deactivate_deployed_plugin_registration(&ctx(home.path(), NEW_BIN, true)).unwrap();

        assert_deployed_plugin_bytes(&plugin_dir, &deployed);
        assert!(!plugin_dir.join("dashboard/manifest.json").exists());
        let config = text(&home.path().join(".hermes/config.yaml"));
        assert!(
            !config.contains("tracedecay"),
            "uninstall should disable tracedecay:\n{config}"
        );
    }

    #[test]
    fn failed_profile_policy_does_not_replace_deployed_plugin_bytes() {
        let home = TempDir::new().unwrap();
        let plugin_dir = home.path().join(".hermes/plugins/tracedecay");
        let deployed = seed_deployed_plugin(&plugin_dir);
        let config_path = home.path().join(".hermes/config.yaml");
        std::fs::write(&config_path, "memory:\n  provider: other\n").unwrap();

        let err =
            activate_deployed_plugin_registration(&ctx(home.path(), NEW_BIN, true)).unwrap_err();

        assert!(
            err.to_string()
                .contains("Hermes memory provider already configured"),
            "unexpected error: {err}"
        );
        assert_deployed_plugin_bytes(&plugin_dir, &deployed);
        assert_eq!(text(&config_path), "memory:\n  provider: other\n");
    }
}
